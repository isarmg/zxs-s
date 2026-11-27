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
            "id": arguments[2], "version": 1, "cache": arguments[3]
        ]
        let reply = try await Bridge.$progress.withValue({ progress.add($0) }) {
            try await Bridge.call("open", request)
        }
        guard let path = reply["path"] as? String else { preconditionFailure("Missing local preview") }
        let bytes = try Data(contentsOf: URL(fileURLWithPath: path))
        precondition(bytes == Data("published content".utf8), "Swift bridge saved different bytes")
        let samples = progress.read()
        precondition(samples.first?.transferred == 0 && samples.first?.direction == 1)
        precondition(samples.last?.transferred == UInt64(bytes.count) && samples.last?.total == UInt64(bytes.count))
        precondition((samples.last?.rate ?? 0) > 0)
        var cancelled = request
        let cache = URL(fileURLWithPath: arguments[3]).appendingPathComponent("cancelled")
        cancelled["cache"] = cache.path
        try await cancelDownload(cancelled)
        let remaining = try FileManager.default.contentsOfDirectory(atPath: cache.path)
        precondition(remaining == [".lock"], "Cancelled preview left a partial file")
        print("Swift bridge download progress, cancellation and persisted bytes passed")
    }

    private static func cancelDownload(_ request: [String: Any]) async throws {
        let started = DispatchSemaphore(value: 0)
        let proceed = DispatchSemaphore(value: 0)
        let task = Task {
            _ = try await Bridge.$progress.withValue({ sample in
                if sample.transferred == 0 {
                    started.signal()
                    proceed.wait()
                }
            }) {
                try await Bridge.call("open", request)
            }
        }
        let reached = await Task.detached { started.wait(timeout: .now() + 5) == .success }.value
        task.cancel()
        proceed.signal()
        precondition(reached, "Missing download progress")
        do {
            try await task.value
            preconditionFailure("Cancelled download completed")
        } catch is CancellationError {}
    }
}
