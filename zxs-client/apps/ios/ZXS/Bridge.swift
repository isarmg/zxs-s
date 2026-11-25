import CryptoKit
import Foundation
import Security

struct ZXSFailure: LocalizedError {
    let message: String
    var status: Int? = nil
    var errorDescription: String? { message }
}

struct SavedConnection: Codable, Equatable {
    let server: String
    let password: String

    var fields: [String: Any] { ["server": server, "password": password] }
    var domainID: String {
        "server-" + SHA256.hash(data: Data(server.utf8)).map { String(format: "%02x", $0) }.joined()
    }

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

    // The first access group in both signed targets is the existing app's Keychain group.
    private static let key: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: "zxs.mobile",
        kSecAttrAccount as String: "credentials"
    ]
}

enum Bridge {
    static func call(_ action: String, _ fields: [String: Any]) async throws -> [String: Any] {
        var arguments = fields
        arguments["action"] = action
        let request = try JSONSerialization.data(withJSONObject: arguments)
        let data = try await Task.detached {
            let pointer = request.withUnsafeBytes { bytes in
                zxs_call(bytes.bindMemory(to: UInt8.self).baseAddress, request.count)
            }
            guard let pointer else { throw ZXSFailure(message: "ZXS 无响应") }
            defer { zxs_free(pointer) }
            return Data(bytes: pointer, count: strlen(pointer))
        }.value
        guard let reply = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              reply["ok"] as? Bool == true,
              let body = reply["data"] as? [String: Any] else {
            let response = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            throw ZXSFailure(message: response?["error"] as? String ?? "ZXS 响应格式错误", status: response?["status"] as? Int)
        }
        return body
    }
}
