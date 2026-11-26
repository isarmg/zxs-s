import FileProvider
import Foundation

final class FileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    private let catalog: ProviderCatalog

    required init(domain: NSFileProviderDomain) { catalog = ProviderCatalog(domain: domain) }

    func invalidate() {}

    func item(for identifier: NSFileProviderItemIdentifier, request: NSFileProviderRequest,
              completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void) -> Progress {
        run({ try await self.catalog.item(ItemKey(identifier)) }) { result in
            switch result {
            case .success(let item): completionHandler(item, nil)
            case .failure(let error): completionHandler(nil, error)
            }
        }
    }

    func fetchContents(for itemIdentifier: NSFileProviderItemIdentifier, version requestedVersion: NSFileProviderItemVersion?,
                       request: NSFileProviderRequest,
                       completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void) -> Progress {
        run({
            var revision: UInt32?
            if let requestedVersion {
                guard let requested = UInt32(String(decoding: requestedVersion.contentVersion, as: UTF8.self)) else {
                    throw NSFileProviderError(.noSuchItem)
                }
                revision = requested
            }
            let key = try ItemKey(itemIdentifier)
            guard !key.isDirectory, let manager = NSFileProviderManager(for: self.catalog.domain) else {
                throw NSFileProviderError(.noSuchItem)
            }
            let temporary = try manager.temporaryDirectoryURL().appendingPathComponent(UUID().uuidString)
            do {
                let item = try await self.catalog.materialize(key, revision: revision, at: temporary)
                return (temporary, item)
            } catch {
                try? FileManager.default.removeItem(at: temporary)
                throw error
            }
        }) { result in
            switch result {
            case .success(let (url, item)): completionHandler(url, item, nil)
            case .failure(let error): completionHandler(nil, nil, error)
            }
        }
    }

    func enumerator(for containerItemIdentifier: NSFileProviderItemIdentifier,
                    request: NSFileProviderRequest) throws -> NSFileProviderEnumerator {
        let contents: ProviderEnumerator.Contents
        switch containerItemIdentifier {
        case .workingSet: contents = .workingSet
        case .trashContainer: contents = .empty
        default: contents = .directory(try ItemKey(containerItemIdentifier))
        }
        return ProviderEnumerator(catalog: catalog, contents: contents)
    }

    func createItem(basedOn itemTemplate: NSFileProviderItem, fields: NSFileProviderItemFields,
                    contents url: URL?, options: NSFileProviderCreateItemOptions, request: NSFileProviderRequest,
                    completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void) -> Progress {
        completionHandler(nil, fields, false, CocoaError(.fileWriteNoPermission))
        return Progress(totalUnitCount: 1)
    }

    func modifyItem(_ item: NSFileProviderItem, baseVersion version: NSFileProviderItemVersion,
                    changedFields: NSFileProviderItemFields, contents newContents: URL?, options: NSFileProviderModifyItemOptions,
                    request: NSFileProviderRequest,
                    completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void) -> Progress {
        completionHandler(nil, changedFields, false, CocoaError(.fileWriteNoPermission))
        return Progress(totalUnitCount: 1)
    }

    func deleteItem(identifier: NSFileProviderItemIdentifier, baseVersion version: NSFileProviderItemVersion,
                    options: NSFileProviderDeleteItemOptions, request: NSFileProviderRequest,
                    completionHandler: @escaping (Error?) -> Void) -> Progress {
        completionHandler(CocoaError(.fileWriteNoPermission))
        return Progress(totalUnitCount: 1)
    }

    private func run<T>(_ operation: @escaping () async throws -> T,
                        completion: @escaping (Result<T, Error>) -> Void) -> Progress {
        let progress = Progress(totalUnitCount: 1)
        let task = Task {
            defer { progress.completedUnitCount = 1 }
            do {
                try Task.checkCancellation()
                completion(.success(try await operation()))
            } catch { completion(.failure(providerError(error))) }
        }
        progress.cancellationHandler = { task.cancel() }
        return progress
    }
}
