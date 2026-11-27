import Foundation
import Security

struct ZXSFailure: LocalizedError {
    let message: String
    var status: Int? = nil
    var passwordRejected = false
    var errorDescription: String? { message }
}

struct SavedConnection: Codable {
    let server: String
    let password: String

    var fields: [String: Any] { ["server": server, "password": password] }
    static func load() -> SavedConnection? {
        var query = key
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return nil }
        return try? JSONDecoder().decode(Self.self, from: data)
    }

    func save() throws {
        let data = try JSONEncoder().encode(self)
        let update = [kSecValueData as String: data]
        var status = SecItemUpdate(Self.key as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var query = Self.key
            query[kSecValueData as String] = data
            query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            status = SecItemAdd(query as CFDictionary, nil)
        }
        if status != errSecSuccess { throw ZXSFailure(message: "无法保存登录凭证（\(status)）") }
    }

    static func clear() throws {
        let status = SecItemDelete(key as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw ZXSFailure(message: "无法清除登录凭证（\(status)）")
        }
    }

    private static let key: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: "zxs.mobile",
        kSecAttrAccount as String: "credentials"
    ]
}

struct TransferSample: Sendable {
    let direction: UInt8
    let transferred: UInt64
    let total: UInt64
    let rate: UInt64

    var fraction: Double { total == 0 ? 1 : min(1, Double(transferred) / Double(total)) }
}

typealias TransferObserver = @Sendable (TransferSample) -> Void

private final class CallbackContext: Sendable {
    let report: TransferObserver
    init(_ report: @escaping TransferObserver) { self.report = report }
}

private let transferCallback: zxs_progress_callback = { context, direction, transferred, total, rate in
    guard let context else { return }
    let receiver = Unmanaged<CallbackContext>.fromOpaque(context).takeUnretainedValue()
    receiver.report(TransferSample(direction: direction, transferred: transferred, total: total, rate: rate))
}

private let downloadCancelled: zxs_cancel_callback = { _ in Task.isCancelled }

enum Bridge {
    @TaskLocal static var progress: TransferObserver?

    static var documentCache: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("ZXS-Documents", isDirectory: true)
    }

    static func call(_ action: String, _ fields: [String: Any]) async throws -> [String: Any] {
        var arguments = fields
        arguments["action"] = action
        let request = try JSONSerialization.data(withJSONObject: arguments)
        let observer = progress
        let task = Task.detached {
            try Task.checkCancellation()
            let pointer = request.withUnsafeBytes { bytes in
                dispatch(bytes.bindMemory(to: UInt8.self), observer: observer)
            }
            guard let pointer else { throw ZXSFailure(message: "ZXS 无响应") }
            defer { zxs_free(pointer) }
            return Data(bytes: pointer, count: strlen(pointer))
        }
        let data = try await withTaskCancellationHandler {
            try await task.value
        } onCancel: {
            task.cancel()
        }
        try Task.checkCancellation()
        let reply = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        guard reply?["ok"] as? Bool == true, let body = reply?["data"] as? [String: Any] else {
            throw ZXSFailure(message: reply?["error"] as? String ?? "ZXS 响应格式错误",
                             status: reply?["status"] as? Int,
                             passwordRejected: reply?["password_rejected"] as? Bool ?? false)
        }
        return body
    }

    private static func dispatch(_ bytes: UnsafeBufferPointer<UInt8>, observer: TransferObserver?) -> UnsafeMutablePointer<CChar>? {
        guard let observer else {
            return zxs_call_with_transfer(bytes.baseAddress, bytes.count, nil, downloadCancelled, nil)
        }
        let context = CallbackContext(observer)
        return withExtendedLifetime(context) {
            zxs_call_with_transfer(bytes.baseAddress, bytes.count, transferCallback, downloadCancelled,
                                   Unmanaged.passUnretained(context).toOpaque())
        }
    }
}
