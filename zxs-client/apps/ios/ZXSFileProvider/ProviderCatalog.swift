import CryptoKit
import FileProvider
import Foundation
import UniformTypeIdentifiers

enum ItemKey: Equatable {
    case directory(String)
    case file(String)

    init(_ identifier: NSFileProviderItemIdentifier) throws {
        if identifier == .rootContainer { self = .directory("/"); return }
        let parts = identifier.rawValue.split(separator: ":", maxSplits: 1).map(String.init)
        guard parts.count == 2 else { throw NSFileProviderError(.noSuchItem) }
        switch parts[0] {
        case "folder": self = .directory(parts[1])
        case "file": self = .file(parts[1])
        default: throw NSFileProviderError(.noSuchItem)
        }
    }

    var identifier: NSFileProviderItemIdentifier {
        switch self {
        case .directory("/"): .rootContainer
        case .directory(let path): .init(rawValue: "folder:" + path)
        case .file(let id): .init(rawValue: "file:" + id)
        }
    }

    var isDirectory: Bool {
        if case .directory = self { return true }
        return false
    }

    var containerID: String? {
        if case .file(let id) = self { return id }
        return nil
    }
}

final class ProviderItem: NSObject, NSFileProviderItem {
    let key: ItemKey
    let parent: ItemKey
    let filename: String
    let revision: UInt32?
    let documentSize: NSNumber?
    let contentModificationDate: Date?

    init(_ key: ItemKey, parent: ItemKey, name: String, revision: UInt32? = nil,
         size: NSNumber? = nil, updated: Date? = nil) {
        self.key = key
        self.parent = parent
        filename = name
        self.revision = revision
        documentSize = size
        contentModificationDate = updated
    }

    var itemIdentifier: NSFileProviderItemIdentifier { key.identifier }
    var parentItemIdentifier: NSFileProviderItemIdentifier { parent.identifier }
    var contentType: UTType {
        if key.isDirectory { return .folder }
        let ext = (filename as NSString).pathExtension
        return UTType(filenameExtension: ext) ?? .data
    }
    var typeIdentifier: String { contentType.identifier }
    var capabilities: NSFileProviderItemCapabilities {
        key.isDirectory ? [.allowsReading, .allowsContentEnumerating] : [.allowsReading, .allowsEvicting]
    }
    var itemVersion: NSFileProviderItemVersion {
        let content = Data(String(revision ?? 0).utf8)
        let stamp = contentModificationDate?.timeIntervalSince1970 ?? 0
        // A move or rename keeps the same content version.
        let metadata = "\(parent.identifier.rawValue)\n\(filename)\n\(stamp)"
        return NSFileProviderItemVersion(contentVersion: content,
                                         metadataVersion: Data(SHA256.hash(data: Data(metadata.utf8))))
    }
}

struct CatalogCursor: Codable {
    let token: String
}

struct ProviderCatalog {
    let domain: NSFileProviderDomain

    func item(_ key: ItemKey) async throws -> ProviderItem {
        switch key {
        case .directory("/"):
            _ = try connection()
            return ProviderItem(key, parent: key, name: domain.displayName)
        case .directory(let path):
            let parent = parentPath(path)
            let data = try await call("folders", ["folder": parent])
            let folders = data["folders"] as? [[String: Any]] ?? []
            guard let map = folders.first(where: { ($0["folder"] as? String) == path }),
                  let item = directory(map, parent: .directory(parent)) else {
                throw NSFileProviderError(.noSuchItem)
            }
            return item
        case .file(let id):
            guard let item = try await containerItems([id])[id] else { throw NSFileProviderError(.noSuchItem) }
            return item
        }
    }

    func containerItems(_ ids: [String]) async throws -> [String: ProviderItem] {
        guard !ids.isEmpty else { return [:] }
        let data = try await call("lookup", ["ids": Array(Set(ids))])
        return Dictionary(uniqueKeysWithValues: containers(data).compactMap { item in
            item.key.containerID.map { ($0, item) }
        })
    }

    func children(_ key: ItemKey, cursor: CatalogCursor?) async throws -> ([ProviderItem], CatalogCursor?) {
        guard case .directory(let folder) = key else { return ([], nil) }
        var fields: [String: Any] = ["folder": folder]
        if let cursor { fields["cursor"] = cursor.token }
        let data = try await call("browse", fields)
        let folders = (data["folders"] as? [[String: Any]] ?? []).compactMap { directory($0, parent: key) }
        let next = (data["next_cursor"] as? String).map { CatalogCursor(token: $0) }
        return (folders + containers(data), next)
    }

    func materialize(_ key: ItemKey, revision: UInt32?, at url: URL) async throws -> ProviderItem {
        guard case .file(let id) = key else { throw NSFileProviderError(.noSuchItem) }
        var fields: [String: Any] = ["id": id, "output": url.path]
        if let revision { fields["version"] = revision }
        let data = try await call("download", fields)
        guard let map = data["container"] as? [String: Any],
              let version = (data["versions"] as? [[String: Any]])?.first,
              let item = file(map, version: version), item.key == key else {
            throw NSFileProviderError(.noSuchItem)
        }
        return item
    }

    private func connection() throws -> SavedConnection {
        guard let saved = SavedConnection.load(), saved.domainID == domain.identifier.rawValue else {
            throw NSFileProviderError(.notAuthenticated)
        }
        return saved
    }

    private func call(_ action: String, _ fields: [String: Any]) async throws -> [String: Any] {
        let saved = try connection()
        var fields = fields
        fields["connection"] = saved.fields
        let result = try await Bridge.call(action, fields)
        guard saved == SavedConnection.load() else { throw NSFileProviderError(.notAuthenticated) }
        try Task.checkCancellation()
        return result
    }

    private func directory(_ map: [String: Any], parent: ItemKey) -> ProviderItem? {
        guard let path = map["folder"] as? String, let name = path.split(separator: "/").last else { return nil }
        return ProviderItem(.directory(path), parent: parent, name: String(name))
    }

    private func containers(_ response: [String: Any]) -> [ProviderItem] {
        (response["containers"] as? [[String: Any]] ?? []).compactMap { file($0) }
    }

    private func file(_ map: [String: Any], version: [String: Any]? = nil) -> ProviderItem? {
        guard let id = map["container_id"] as? String, let name = map["name"] as? String,
              let folder = map["folder"] as? String,
              let latest = (map["latest_version"] as? NSNumber)?.uint32Value else { return nil }
        let revision = (version?["version"] as? NSNumber)?.uint32Value ?? latest
        let updated = (map["updated_at"] as? NSNumber).map { Date(timeIntervalSince1970: $0.doubleValue) }
        return ProviderItem(.file(id), parent: .directory(folder), name: name,
                            revision: revision, size: version?["size"] as? NSNumber, updated: updated)
    }
}

private func parentPath(_ path: String) -> String {
    guard let slash = path.lastIndex(of: "/"), slash != path.startIndex else { return "/" }
    return String(path[..<slash])
}

func providerError(_ error: Error) -> Error {
    if error is CancellationError { return CocoaError(.userCancelled) }
    switch (error as? ZXSFailure)?.status {
    case 401: return NSFileProviderError(.notAuthenticated)
    case 404, 410: return NSFileProviderError(.noSuchItem)
    case 502, 503, 504: return NSFileProviderError(.serverUnreachable)
    default: return error
    }
}
