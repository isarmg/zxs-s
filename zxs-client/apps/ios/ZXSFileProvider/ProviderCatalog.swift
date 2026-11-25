import CryptoKit
import FileProvider
import Foundation
import UniformTypeIdentifiers

enum ItemKey: Equatable {
    case directory(String)
    case file(String)
    case bundle(String)
    case member(String, String, directory: Bool)

    init(_ identifier: NSFileProviderItemIdentifier) throws {
        if identifier == .rootContainer { self = .directory("/"); return }
        let parts = identifier.rawValue.split(separator: ":", maxSplits: 2).map(String.init)
        guard parts.count >= 2 else { throw NSFileProviderError(.noSuchItem) }
        switch (parts[0], parts.count) {
        case ("folder", 2): self = .directory(parts[1])
        case ("file", 2): self = .file(parts[1])
        case ("bundle", 2): self = .bundle(parts[1])
        case ("member-folder", 3): self = .member(parts[1], parts[2], directory: true)
        case ("member-file", 3): self = .member(parts[1], parts[2], directory: false)
        default: throw NSFileProviderError(.noSuchItem)
        }
    }

    var identifier: NSFileProviderItemIdentifier {
        switch self {
        case .directory("/"): .rootContainer
        case .directory(let path): .init(rawValue: "folder:" + path)
        case .file(let id): .init(rawValue: "file:" + id)
        case .bundle(let id): .init(rawValue: "bundle:" + id)
        case .member(let id, let path, let directory):
            .init(rawValue: (directory ? "member-folder:" : "member-file:") + id + ":" + path)
        }
    }

    var isDirectory: Bool {
        switch self {
        case .directory, .bundle, .member(_, _, true): true
        default: false
        }
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
    let revision: UInt32?
}

struct ProviderCatalog {
    let domain: NSFileProviderDomain

    func item(_ key: ItemKey, revision: UInt32? = nil) async throws -> ProviderItem {
        switch key {
        case .directory("/"):
            _ = try connection()
            return ProviderItem(key, parent: key, name: domain.displayName)
        case .directory(let path):
            let parent = ItemKey.directory(parentPath(path))
            let (items, _) = try await children(parent, cursor: nil)
            guard let item = items.first(where: { $0.key == key }) else { throw NSFileProviderError(.noSuchItem) }
            return item
        case .file(let id), .bundle(let id):
            let data = try await call("detail", ["id": id])
            guard let map = data["container"] as? [String: Any],
                  let item = container(map, response: data, revision: revision), item.key == key else {
                if revision != nil { throw NSFileProviderError(.versionNoLongerAvailable) }
                throw NSFileProviderError(.noSuchItem)
            }
            return item
        case .member(let id, let path, _):
            let parent = memberParent(id: id, path: path)
            var cursor: CatalogCursor?
            repeat {
                let (items, next) = try await children(parent, cursor: cursor, revision: revision)
                if let item = items.first(where: { $0.key == key }) { return item }
                cursor = next
            } while cursor != nil
            if revision != nil { throw NSFileProviderError(.versionNoLongerAvailable) }
            throw NSFileProviderError(.noSuchItem)
        }
    }

    func children(_ key: ItemKey, cursor: CatalogCursor?, revision: UInt32? = nil) async throws
        -> ([ProviderItem], CatalogCursor?) {
        switch key {
        case .directory(let folder):
            var fields: [String: Any] = ["folder": folder]
            if let cursor { fields["cursor"] = cursor.token }
            let data = try await call("browse", fields)
            let folders = cursor == nil ? data["folders"] as? [[String: Any]] ?? [] : []
            let directories = folders.compactMap { map -> ProviderItem? in
                guard let path = map["folder"] as? String, let name = path.split(separator: "/").last else { return nil }
                return ProviderItem(.directory(path), parent: key, name: String(name))
            }
            let files = (data["containers"] as? [[String: Any]] ?? []).compactMap { container($0, response: data) }
            return (directories + files, next(data))
        case .bundle(let id):
            return try await bundleChildren(key, id: id, path: ".", cursor: cursor, revision: revision)
        case .member(let id, let path, true):
            return try await bundleChildren(key, id: id, path: path, cursor: cursor, revision: revision)
        default:
            return ([], nil)
        }
    }

    func materialize(_ item: ProviderItem, at url: URL) async throws {
        guard let version = item.revision else { throw NSFileProviderError(.noSuchItem) }
        switch item.key {
        case .file(let id):
            _ = try await call("download", ["id": id, "version": version, "output": url.path])
        case .member(let id, let path, false):
            _ = try await call("open_bundle_member", ["id": id, "version": version, "path": path, "output": url.path])
        default:
            throw NSFileProviderError(.noSuchItem)
        }
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

    private func bundleChildren(_ key: ItemKey, id: String, path: String, cursor: CatalogCursor?,
                                revision: UInt32?) async throws
        -> ([ProviderItem], CatalogCursor?) {
        var fields: [String: Any] = ["id": id, "directory": path]
        if let cursor { fields["cursor"] = cursor.token }
        if let pinned = cursor?.revision ?? revision { fields["version"] = pinned }
        let data = try await call("browse_bundle", fields)
        guard let revision = (data["version"] as? NSNumber)?.uint32Value else {
            throw ZXSFailure(message: "目录工作区缺少版本信息")
        }
        let items = (data["entries"] as? [[String: Any]] ?? []).compactMap { map -> ProviderItem? in
            guard let relative = map["path"] as? String, let name = map["name"] as? String,
                  let kind = map["kind"] as? String, kind == "file" || kind == "directory" else { return nil }
            return ProviderItem(.member(id, relative, directory: kind == "directory"), parent: key,
                                name: name, revision: revision, size: map["size"] as? NSNumber)
        }
        return (items, next(data, revision: revision))
    }

    private func next(_ response: [String: Any], revision: UInt32? = nil) -> CatalogCursor? {
        (response["next_cursor"] as? String).map { CatalogCursor(token: $0, revision: revision) }
    }

    private func container(_ map: [String: Any], response: [String: Any], revision: UInt32? = nil) -> ProviderItem? {
        guard let id = map["container_id"] as? String, let name = map["name"] as? String,
              let folder = map["folder"] as? String,
              let latest = (map["latest_version"] as? NSNumber)?.uint32Value else { return nil }
        let version = revision ?? latest
        let descriptor = (response["versions"] as? [[String: Any]])?.first {
            ($0["version"] as? NSNumber)?.uint32Value == version
        }
        guard version == latest || descriptor != nil else { return nil }
        let bundle = (response["bundles"] as? [String])?.contains(id) == true
        let conflict = (response["conflicts"] as? [String])?.contains(id) == true
        let displayName = bundle && !conflict && name.count > 4 && name.lowercased().hasSuffix(".zip")
            ? String(name.dropLast(4)) : name
        let updated = (map["updated_at"] as? NSNumber).map { Date(timeIntervalSince1970: $0.doubleValue) }
        return ProviderItem(bundle ? .bundle(id) : .file(id), parent: .directory(folder), name: displayName,
                            revision: version, size: bundle ? nil : descriptor?["size"] as? NSNumber, updated: updated)
    }
}

private func parentPath(_ path: String) -> String {
    guard let slash = path.lastIndex(of: "/"), slash != path.startIndex else { return "/" }
    return String(path[..<slash])
}

private func memberParent(id: String, path: String) -> ItemKey {
    guard let slash = path.lastIndex(of: "/") else { return .bundle(id) }
    return .member(id, String(path[..<slash]), directory: true)
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
