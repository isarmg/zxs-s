import AVKit
import Combine
import Foundation
import QuickLook
import SwiftUI

struct Opened: Identifiable {
    let url: URL
    let media: Bool
    var headers: [String: String] = [:]
    var refresh: (() async throws -> [String: String])? = nil
    var id: URL { url }
}

struct FilePreview: View {
    let file: Opened
    let close: () -> Void

    var body: some View {
        NavigationStack {
            Group {
                if file.media { PlayerView(file: file) }
                else { QuickPreview(url: file.url) }
            }
            .toolbar { Button("关闭", action: close) }
        }
    }
}

private struct PlayerView: View {
    let file: Opened
    @State private var player: AVPlayer
    @State private var item: AVPlayerItem
    @State private var headers: [String: String]
    @State private var failures = 0
    @State private var message: String?

    init(file: Opened) {
        self.file = file
        let item = Self.makeItem(file.url, headers: file.headers)
        _item = State(initialValue: item)
        _player = State(initialValue: AVPlayer(playerItem: item))
        _headers = State(initialValue: file.headers)
    }

    var body: some View {
        VideoPlayer(player: player)
            .onAppear { player.play() }
            .onDisappear { player.pause() }
            .onReceive(item.publisher(for: \.status).receive(on: RunLoop.main)) { status in
                if status == .failed { failures += 1 }
            }
            .onReceive(NotificationCenter.default.publisher(for: AVPlayerItem.failedToPlayToEndTimeNotification,
                                                             object: item).receive(on: RunLoop.main)) { _ in failures += 1 }
            .task(id: failures) {
                if failures > 0 { await reconnect() }
            }
            .overlay {
                if let message { Text(message).padding().background(.regularMaterial) }
            }
    }

    private func reconnect() async {
        let position = player.currentTime()
        do {
            let renewed = try await file.refresh?()
            try Task.checkCancellation()
            guard let renewed, !renewed.isEmpty, renewed != headers else {
                message = item.error?.localizedDescription ?? "系统播放器无法播放此文件。"
                return
            }
            headers = renewed
            item = Self.makeItem(file.url, headers: renewed)
            player.replaceCurrentItem(with: item)
            if position.isNumeric { player.seek(to: position, completionHandler: { _ in }) }
            player.play()
        } catch is CancellationError {
        } catch {
            message = error.localizedDescription
        }
    }

    private static func makeItem(_ url: URL, headers: [String: String]) -> AVPlayerItem {
        if headers.isEmpty { return AVPlayerItem(url: url) }
        return AVPlayerItem(asset: AVURLAsset(url: url, options: ["AVURLAssetHTTPHeaderFieldsKey": headers]))
    }
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
