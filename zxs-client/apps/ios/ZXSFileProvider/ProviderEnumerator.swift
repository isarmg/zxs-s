import FileProvider
import Foundation

final class ProviderEnumerator: NSObject, NSFileProviderEnumerator {
    enum Contents {
        case directory(ItemKey)
        case workingSet
        case empty
    }

    let catalog: ProviderCatalog
    let contents: Contents
    private let localItems: NSFileProviderEnumerator?

    init(catalog: ProviderCatalog, contents: Contents) {
        self.catalog = catalog
        self.contents = contents
        if case .workingSet = contents {
            localItems = NSFileProviderManager(for: catalog.domain)?.enumeratorForMaterializedItems()
        } else { localItems = nil }
    }

    func invalidate() { localItems?.invalidate() }

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        Task {
            do {
                let (items, next) = try await loadPage(page)
                observer.didEnumerate(items)
                observer.finishEnumerating(upTo: next)
            } catch {
                observer.finishEnumeratingWithError(providerError(error))
            }
        }
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(NSFileProviderSyncAnchor(rawValue: Data([1])))
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        // The catalog has no incremental change feed. When signaled, the system must reload metadata.
        observer.finishEnumeratingWithError(NSFileProviderError(.syncAnchorExpired))
    }

    private func loadPage(_ page: NSFileProviderPage) async throws -> ([NSFileProviderItem], NSFileProviderPage?) {
        switch contents {
        case .directory(let key):
            var cursor: CatalogCursor?
            if page.rawValue != NSFileProviderPage.initialPageSortedByName as Data &&
                page.rawValue != NSFileProviderPage.initialPageSortedByDate as Data {
                guard let decoded = try? JSONDecoder().decode(CatalogCursor.self, from: page.rawValue) else {
                    throw NSFileProviderError(.pageExpired)
                }
                cursor = decoded
            }
            let (items, next) = try await catalog.children(key, cursor: cursor)
            let nextPage = try next.map { NSFileProviderPage(rawValue: try JSONEncoder().encode($0)) }
            return (items, nextPage)
        case .workingSet:
            return try await materialized(page)
        case .empty:
            return ([], nil)
        }
    }

    private func materialized(_ page: NSFileProviderPage) async throws -> ([NSFileProviderItem], NSFileProviderPage?) {
        guard let localItems else { throw NSFileProviderError(.notAuthenticated) }
        // Ask the system which items are locally available. This scopes refreshes to files and
        // directories that have been used, without walking every disk and ZIP workspace.
        let (identifiers, next): ([NSFileProviderItemIdentifier], NSFileProviderPage?) = try await withCheckedThrowingContinuation { result in
            localItems.enumerateItems(for: MaterializedObserver(result), startingAt: page)
        }
        let keys: [ItemKey] = identifiers.filter { $0 != .rootContainer }.compactMap { try? ItemKey($0) }
        let containers = try await catalog.containerItems(keys.compactMap(\.containerID))
        var items: [NSFileProviderItem] = []
        for key in keys {
            if let id = key.containerID {
                if let item = containers[id] { items.append(item) }
                continue
            }
            do {
                let item = try await catalog.item(key)
                items.append(item)
            } catch {
                guard let failure = providerError(error) as? NSFileProviderError, failure.code == .noSuchItem else {
                    throw error
                }
            }
        }
        return (items, next)
    }
}

private final class MaterializedObserver: NSObject, NSFileProviderEnumerationObserver {
    private let result: CheckedContinuation<([NSFileProviderItemIdentifier], NSFileProviderPage?), Error>
    private var identifiers: [NSFileProviderItemIdentifier] = []

    init(_ result: CheckedContinuation<([NSFileProviderItemIdentifier], NSFileProviderPage?), Error>) {
        self.result = result
    }

    func didEnumerate(_ updatedItems: [NSFileProviderItem]) {
        identifiers.append(contentsOf: updatedItems.map(\.itemIdentifier))
    }

    func finishEnumerating(upTo nextPage: NSFileProviderPage?) { result.resume(returning: (identifiers, nextPage)) }
    func finishEnumeratingWithError(_ error: Error) { result.resume(throwing: error) }
}
