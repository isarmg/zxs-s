import Foundation

private final class ProgressLog: @unchecked Sendable {
    let lock = NSLock()
    var samples: [TransferSample] = []

    func add(_ sample: TransferSample) {
        lock.lock()
        defer { lock.unlock() }
        samples.append(sample)
    }

    func read() -> [TransferSample] {
        lock.lock()
        defer { lock.unlock() }
        return samples
    }
}

@main struct AppleBridgeTest {
    static func main() async throws {
        let arguments = CommandLine.arguments
        let progress = ProgressLog()
        let request: [String: Any] = [
            "connection": ["server": arguments[1], "password": "fixture"],
            "id": arguments[2], "version": 1, "output": arguments[3]
        ]
        _ = try await Bridge.$progress.withValue({ progress.add($0) }) {
            try await Bridge.call("download", request)
        }
        let bytes = try Data(contentsOf: URL(fileURLWithPath: arguments[3]))
        precondition(bytes == Data("published content".utf8), "Swift bridge saved different bytes")
        let samples = progress.read()
        precondition(samples.first?.transferred == 0 && samples.first?.direction == 1)
        precondition(samples.last?.transferred == UInt64(bytes.count) && samples.last?.total == UInt64(bytes.count))
        precondition((samples.last?.rate ?? 0) > 0)
        print("Swift bridge download progress and persisted bytes passed")
    }
}
