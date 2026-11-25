import AVKit
import CoreTransferable
import PhotosUI
import QuickLook
import SwiftUI
import UniformTypeIdentifiers

@main
struct ZXSApp: App {
    var body: some Scene { WindowGroup { BrowserView() } }
}

struct Opened: Identifiable {
    let url: URL
    let media: Bool
    var headers: [String: String] = [:]
    var export = false
    var id: URL { url }
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

struct BrowserView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var connection = SavedConnection.load()
    @State private var server = "http://"
    @State private var password = ""
    @State private var folder = "/"
    @State private var cursor: String?
    @State private var entries: [FileEntry] = []
    @State private var directories: [FolderEntry] = []
    @State private var disks: [FolderEntry] = []
    @State private var albumDisk = ""
    @State private var picks: [PhotosPickerItem] = []
    @State private var opened: Opened?
    @State private var exportOffer: FileEntry?
    @State private var notice: BrowserNotice?
    @State private var working = false

    var body: some View {
        NavigationStack {
            Group {
                if connection == nil { loginForm } else { browser }
            }
            .navigationTitle("ZXS")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                if connection != nil {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("退出") { perform { try await logout() } }
                    }
                }
            }
            .overlay { if working { ProgressView().padding().background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12)) } }
            .disabled(working)
            .overlay(alignment: .bottom) {
                if let notice { NoticeView(notice: notice) { self.notice = nil } }
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: notice?.id)
            .task(id: notice?.id) {
                guard let current = notice, !current.isError else { return }
                try? await Task.sleep(for: .milliseconds(3500))
                if !Task.isCancelled && notice?.id == current.id { notice = nil }
            }
            .sheet(item: $opened) { item in
                NavigationStack {
                    Group {
                        if item.export { ShareLink(item: item.url) { Label("导出整包", systemImage: "square.and.arrow.up") } }
                        else if item.media { PlayerView(url: item.url, headers: item.headers) }
                        else { QuickPreview(url: item.url) }
                    }
                    .toolbar { Button("关闭") { opened = nil } }
                }
            }
            .confirmationDialog("目录工作区", isPresented: Binding(
                get: { exportOffer != nil }, set: { if !$0 { exportOffer = nil } }
            ), presenting: exportOffer) { entry in
                Button("导出整包") { perform { try await open(entry, export: true) } }
            } message: { _ in
                Text("目录工作区请在桌面端检出编辑，或选择导出整包。")
            }
            .task {
                if connection != nil && entries.isEmpty { perform { try await browse() } }
            }
            .onChange(of: picks) { _, selected in
                if !selected.isEmpty { perform { try await backup(selected) } }
            }
        }
        .tint(MobileTheme.accent)
    }

    private var loginForm: some View {
        Form {
            TextField("局域网服务器地址", text: $server)
                .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
            SecureField("口令", text: $password)
            Button("连接") { perform { try await login() } }
                .disabled(server == "http://")
        }
        .scrollContentBackground(.hidden)
        .background(MobileTheme.canvas)
    }

    private var browser: some View {
        List {
            Section {
                if disks.count > 1 {
                    Picker("相册备份数据盘", selection: Binding(
                        get: { albumDisk },
                        set: { disk in
                            albumDisk = disk
                            if let server = connection?.server {
                                UserDefaults.standard.set(disk, forKey: "album-disk:" + server)
                            }
                        }
                    )) {
                        ForEach(disks) { disk in Text(disk.name).tag(disk.id) }
                    }
                }
                PhotosPicker(selection: $picks, matching: .any(of: [.images, .videos]), preferredItemEncoding: .current) {
                    Label("选择相片和视频备份", systemImage: "photo.on.rectangle")
                }
                .disabled(disks.isEmpty)
            }
            .listRowBackground(MobileTheme.surface)
            Section(folder) {
                if folder != "/" {
                    Button {
                        if let end = folder.lastIndex(of: "/") {
                            let parent = end == folder.startIndex ? "/" : String(folder[..<end])
                            perform { try await browse(target: parent) }
                        }
                    } label: { Label("上一级", systemImage: "arrow.up") }
                }
                ForEach(directories) { directory in
                    Button {
                        perform { try await browse(target: directory.id) }
                    } label: {
                        FileRow(title: directory.name, subtitle: "", glyph: folder == "/" ? .disk : .folder)
                    }
                }
                ForEach(entries) { entry in
                    Button {
                        if entry.glyph == .bundle { exportOffer = entry }
                        else { perform { try await open(entry) } }
                    } label: {
                        FileRow(title: entry.name, subtitle: entry.subtitle, glyph: entry.glyph)
                    }
                }
                if cursor != nil { Button("加载下一页") { perform { try await browse(append: true) } } }
                if directories.isEmpty && entries.isEmpty && !working {
                    Text("此目录为空").foregroundStyle(MobileTheme.secondary)
                }
            }
            .listRowBackground(MobileTheme.surface)
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(MobileTheme.canvas)
        .refreshable {
            guard !working else { return }
            working = true
            defer { working = false }
            do { try await browse() } catch { showError(error) }
        }
    }

    private func perform(_ work: @escaping @MainActor () async throws -> Void) {
        guard !working else { return }
        working = true
        Task {
            defer { working = false }
            do { try await work() } catch { showError(error) }
        }
    }

    private func showError(_ error: Error) {
        if (error as? ZXSFailure)?.status == 401 {
            SavedConnection.clear()
            connection = nil
        }
        notice = BrowserNotice(text: error.localizedDescription, isError: true)
    }

    private func login() async throws {
        let result = try await Bridge.call("login", ["server": server, "password": password])
        guard let server = result["server"] as? String else {
            throw ZXSFailure(message: "服务器未返回登录凭证")
        }
        let saved = SavedConnection(server: server, password: password)
        try saved.save()
        password = ""
        connection = saved
        try await browse(target: "/")
    }

    private func logout() async throws {
        if let connection { _ = try await Bridge.call("logout", ["connection": connection.fields]) }
        SavedConnection.clear()
        connection = nil
        folder = "/"
        entries = []
        directories = []
        disks = []
        albumDisk = ""
        cursor = nil
        notice = nil
    }

    private func browse(target: String? = nil, append: Bool = false) async throws {
        guard let connection else { return }
        let requested = target ?? folder
        var fields: [String: Any] = ["connection": connection.fields, "folder": requested]
        if append, let cursor { fields["cursor"] = cursor }
        let data = try await Bridge.call("browse", fields)
        let bundles = Set(data["bundles"] as? [String] ?? [])
        let conflicts = Set(data["conflicts"] as? [String] ?? [])
        let files = (data["containers"] as? [[String: Any]] ?? []).compactMap {
            FileEntry($0, bundles: bundles, conflicts: conflicts)
        }
        if append { entries += files } else {
            entries = files
            directories = (data["folders"] as? [[String: Any]] ?? []).compactMap(FolderEntry.init)
            if requested == "/" {
                disks = directories
                let saved = UserDefaults.standard.string(forKey: "album-disk:" + connection.server)
                albumDisk = disks.first(where: { $0.id == saved })?.id ?? disks.first?.id ?? ""
            }
        }
        folder = requested
        cursor = data["next_cursor"] as? String
    }

    private func backup(_ selected: [PhotosPickerItem]) async throws {
        defer { picks = [] }
        guard let connection else { return }
        let disk = albumDisk
        guard !disk.isEmpty else { throw ZXSFailure(message: "请先选择相册备份数据盘") }
        let album = disk + "/相册"
        for item in selected {
            guard let media = try await item.loadTransferable(type: PickedMedia.self) else {
                throw ZXSFailure(message: "无法读取所选相片或视频")
            }
            defer { try? FileManager.default.removeItem(at: media.url.deletingLastPathComponent()) }
            _ = try await Bridge.call("backup", [
                "connection": connection.fields, "file": media.url.path,
                "name": media.url.lastPathComponent, "folder": album
            ])
        }
        try await browse(target: album)
        notice = BrowserNotice(text: "已检查并备份 \(selected.count) 个项目", isError: false)
    }

    private func open(_ entry: FileEntry, export: Bool = false) async throws {
        guard let connection else { return }
        let cache = FileManager.default.temporaryDirectory.appendingPathComponent("ZXS-Documents")
        let result = try await Bridge.call("open", [
            "connection": connection.fields, "id": entry.id, "cache": cache.path, "export": export
        ])
        if result["bundle"] as? Bool == true && !export { exportOffer = entry; return }
        if let text = result["url"] as? String, let url = URL(string: text) {
            opened = Opened(url: url, media: true, headers: result["headers"] as? [String: String] ?? [:])
            return
        }
        guard let path = result["path"] as? String else { throw ZXSFailure(message: "下载没有返回本地路径") }
        opened = Opened(url: URL(fileURLWithPath: path), media: false, export: export)
    }
}

struct PlayerView: View {
    @State private var player: AVPlayer
    init(url: URL, headers: [String: String]) {
        let asset = AVURLAsset(url: url, options: ["AVURLAssetHTTPHeaderFieldsKey": headers])
        _player = State(initialValue: AVPlayer(playerItem: AVPlayerItem(asset: asset)))
    }
    var body: some View { VideoPlayer(player: player).onAppear { player.play() }.onDisappear { player.pause() } }
}

struct QuickPreview: UIViewControllerRepresentable {
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
