import FileProvider
import Foundation

actor FileAccess {
    static let shared = FileAccess()
    private var pending: Task<Void, Error>?

    func update(folder: String = "/") async throws {
        let previous = pending
        let task = Task {
            _ = await previous?.result
            try await Self.synchronize(folder: folder)
        }
        pending = task
        try await task.value
    }

    private static func synchronize(folder: String) async throws {
        let id = SavedConnection.load().map { NSFileProviderDomainIdentifier(rawValue: $0.domainID) }
        let domains = try await NSFileProviderManager.domains()
        for domain in domains where domain.identifier != id {
            try await NSFileProviderManager.remove(domain)
        }
        guard let id else { return }
        guard let domain = domains.first(where: { $0.identifier == id }) else {
            try await NSFileProviderManager.add(NSFileProviderDomain(identifier: id, displayName: "ZXS"))
            return
        }
        guard domain.userEnabled, let manager = NSFileProviderManager(for: domain) else { return }
        try? await manager.signalErrorResolved(NSFileProviderError(.notAuthenticated))
        try? await manager.signalErrorResolved(NSFileProviderError(.serverUnreachable))
        if folder != "/" {
            try? await manager.signalEnumerator(for: .init(rawValue: "folder:" + folder))
        }
        try await manager.signalEnumerator(for: .rootContainer)
        try await manager.signalEnumerator(for: .workingSet)
    }
}
