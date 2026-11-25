import AVKit
import CoreTransferable
import Foundation
import PhotosUI
import QuickLook
import SwiftUI
import UniformTypeIdentifiers

struct Opened: Identifiable {
    let url: URL
    let media: Bool
    var headers: [String: String] = [:]
    var id: URL { url }
}

struct FilePreview: View {
    let file: Opened
    let close: () -> Void

    var body: some View {
        NavigationStack {
            Group {
                if file.media { PlayerView(url: file.url, headers: file.headers) }
                else { QuickPreview(url: file.url) }
            }
            .toolbar { Button("关闭", action: close) }
        }
    }
}

private struct PlayerView: View {
    @State private var player: AVPlayer

    init(url: URL, headers: [String: String]) {
        if headers.isEmpty {
            _player = State(initialValue: AVPlayer(url: url))
        } else {
            let asset = AVURLAsset(url: url, options: ["AVURLAssetHTTPHeaderFieldsKey": headers])
            _player = State(initialValue: AVPlayer(playerItem: AVPlayerItem(asset: asset)))
        }
    }

    var body: some View { VideoPlayer(player: player).onAppear { player.play() }.onDisappear { player.pause() } }
}

private struct QuickPreview: UIViewControllerRepresentable {
    let url: URL

    func makeCoordinator() -> Source { Source(url: url) }
    func makeUIViewController(context: Context) -> QLPreviewController {
        let controller = QLPreviewController()
        controller.dataSource = context.coordinator
        return controller
    }
    func updateUIViewController(_ controller: QLPreviewController, context: Context) {}

    final class Source: NSObject, QLPreviewControllerDataSource {
        let url: URL
        init(url: URL) { self.url = url }
        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }
        func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem { url as NSURL }
    }
}

@MainActor
func backupPhotos(_ selected: [PhotosPickerItem], connection: SavedConnection, folder: String) async throws {
    for item in selected {
        guard let media = try await item.loadTransferable(type: PickedMedia.self) else {
            throw ZXSFailure(message: "无法读取所选相片或视频")
        }
        defer { try? FileManager.default.removeItem(at: media.url.deletingLastPathComponent()) }
        _ = try await Bridge.call("backup", [
            "connection": connection.fields, "file": media.url.path,
            "name": media.url.lastPathComponent, "folder": folder
        ])
    }
}

private struct PickedMedia: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .item) { received in
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let copy = directory.appendingPathComponent(received.file.lastPathComponent)
            do { try FileManager.default.copyItem(at: received.file, to: copy) }
            catch {
                try? FileManager.default.removeItem(at: directory)
                throw error
            }
            return Self(url: copy)
        }
    }
}
