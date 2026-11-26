import AVKit
import Combine
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

struct BackupOutcome: Identifiable {
    enum Status: String { case saved = "已备份", skipped = "已存在，跳过", failed = "失败" }

    let id: Int
    let source: PhotosPickerItem
    let name: String
    let status: Status
    let detail: String
}

struct BackupReport: Identifiable {
    let id = UUID()
    let folder: String
    let outcomes: [BackupOutcome]

    var failed: [PhotosPickerItem] { outcomes.filter { $0.status == .failed }.map(\.source) }
    var hasSavedItems: Bool { outcomes.contains { $0.status != .failed } }
    var summary: String {
        let saved = outcomes.lazy.filter { $0.status == .saved }.count
        let skipped = outcomes.lazy.filter { $0.status == .skipped }.count
        return "已备份 \(saved) · 跳过 \(skipped) · 失败 \(outcomes.count - saved - skipped)"
    }
}

struct BackupResultView: View {
    let report: BackupReport
    let done: () -> Void
    let retry: () -> Void

    var body: some View {
        NavigationStack {
            List {
                Section(report.summary) {
                    ForEach(report.outcomes) { item in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(item.name)
                            Text(item.detail.isEmpty ? item.status.rawValue : "\(item.status.rawValue) · \(item.detail)")
                                .font(.caption).foregroundStyle(item.status == .failed ? MobileTheme.danger : MobileTheme.secondary)
                        }
                    }
                }
                if !report.failed.isEmpty { Button("重试失败项", action: retry) }
            }
            .navigationTitle("相册备份结果")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { Button("完成", action: done) }
        }
    }
}

@MainActor
func backupPhotos(_ selected: [PhotosPickerItem], connection: SavedConnection, folder: String,
                  progress: (Int) -> Void) async -> BackupReport {
    var outcomes: [BackupOutcome] = []
    let batch = selected.count > 1 ? UUID().uuidString : nil
    for (index, item) in selected.enumerated() {
        var name = "第 \(index + 1) 个项目"
        do {
            guard let media = try await item.loadTransferable(type: PickedMedia.self) else {
                throw ZXSFailure(message: "无法读取所选相片或视频")
            }
            defer { try? FileManager.default.removeItem(at: media.url.deletingLastPathComponent()) }
            name = media.url.lastPathComponent
            var request: [String: Any] = [
                "connection": connection.fields, "file": media.url.path, "name": name, "folder": folder
            ]
            if let batch { request["batch"] = batch }
            let result = try await Bridge.call("backup", request)
            let stored = (result["container"] as? [String: Any])?["name"] as? String ?? name
            let status: BackupOutcome.Status = result["already_present"] as? Bool == true ? .skipped : .saved
            outcomes.append(BackupOutcome(id: index, source: item, name: name, status: status,
                                          detail: stored == name ? "" : stored))
        } catch {
            outcomes.append(BackupOutcome(id: index, source: item, name: name, status: .failed,
                                          detail: error.localizedDescription))
        }
        progress(outcomes.count)
    }
    return BackupReport(folder: folder, outcomes: outcomes)
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
