import CoreTransferable
import Foundation
import PhotosUI
import UniformTypeIdentifiers

enum UploadSource {
    case document(URL)
    case photo(PhotosPickerItem)

    var name: String? {
        if case let .document(url) = self { return url.lastPathComponent }
        return nil
    }

    func stage() async throws -> StagedUpload {
        switch self {
        case let .document(url):
            return try await Task.detached { try StagedUpload.document(url) }.value
        case let .photo(item):
            guard let file = try await item.loadTransferable(type: StagedUpload.self) else {
                throw ZXSFailure(message: "无法读取所选相片或视频")
            }
            return file
        }
    }
}

struct StagedUpload: Transferable {
    let url: URL
    let name: String

    private init(copying source: URL) throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        name = source.lastPathComponent
        url = directory.appendingPathComponent("payload")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        do { try FileManager.default.copyItem(at: source, to: url) }
        catch {
            try? FileManager.default.removeItem(at: directory)
            throw error
        }
    }

    func remove() { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }

    static func document(_ source: URL) throws -> Self {
        let scoped = source.startAccessingSecurityScopedResource()
        defer { if scoped { source.stopAccessingSecurityScopedResource() } }
        var coordinationError: NSError?
        var staged: Result<Self, Error>?
        NSFileCoordinator().coordinate(readingItemAt: source, options: [], error: &coordinationError) { url in
            staged = Result {
                guard try url.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile == true else {
                    throw ZXSFailure(message: "请选择普通文件或 ZIP")
                }
                return try Self(copying: url)
            }
        }
        if let coordinationError {
            (try? staged?.get())?.remove()
            throw coordinationError
        }
        guard let staged else { throw ZXSFailure(message: "无法读取所选文件") }
        return try staged.get()
    }

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .item) { received in
            try Self(copying: received.file)
        }
    }
}
