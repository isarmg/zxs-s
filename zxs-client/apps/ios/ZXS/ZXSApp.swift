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

struct FileEntry: Identifiable {
    let id: String
    let name: String

    init?(_ map: [String: Any]) {
        guard let id = map["container_id"] as? String, let name = map["name"] as? String else { return nil }
        self.id = id
        self.name = name
    }
}

struct Opened: Identifiable {
    let url: URL
    let media: Bool
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
    @State private var connection = SavedConnection.load()
    @State private var server = "http://"
    @State private var password = ""
    @State private var folder = "/"
    @State private var cursor: String?
    @State private var entries: [FileEntry] = []
    @State private var directories: [String] = []
    @State private var picks: [PhotosPickerItem] = []
    @State private var opened: Opened?
    @State private var message: String?
    @State private var working = false

    var body: some View {
        NavigationStack {
            Group {
                if connection == nil { loginForm } else { browser }
            }
            .navigationTitle("ZXS")
            .toolbar {
                if connection != nil {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("退出") { perform { try await logout() } }
                    }
                }
            }
            .overlay { if working { ProgressView().padding().background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12)) } }
            .disabled(working)
            .alert("ZXS", isPresented: Binding(get: { message != nil }, set: { if !$0 { message = nil } })) {
                Button("确定", role: .cancel) {}
            } message: { Text(message ?? "") }
            .sheet(item: $opened) { item in
                NavigationStack {
                    Group {
                        if item.media { PlayerView(url: item.url) } else { QuickPreview(url: item.url) }
                    }
                    .toolbar { Button("关闭") { opened = nil } }
                }
            }
            .task {
                if connection != nil && entries.isEmpty { perform { try await browse() } }
            }
            .onChange(of: picks) { _, selected in
                if !selected.isEmpty { perform { try await backup(selected) } }
            }
        }
    }

    private var loginForm: some View {
        Form {
            TextField("局域网服务器地址", text: $server)
                .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
            SecureField("口令", text: $password)
            Button("连接") { perform { try await login() } }
                .disabled(password.isEmpty || server == "http://")
        }
    }

    private var browser: some View {
        List {
            Section {
                PhotosPicker(selection: $picks, maxSelectionCount: 64, matching: .any(of: [.images, .videos]), preferredItemEncoding: .current) {
                    Label("选择相片和视频备份", systemImage: "photo.on.rectangle")
                }
            }
            Section(folder) {
                if folder != "/" {
                    Button {
                        folder = String(folder.prefix(upTo: folder.lastIndex(of: "/") ?? folder.startIndex))
                        if folder.isEmpty { folder = "/" }
                        perform { try await browse() }
                    } label: { Label("上一级", systemImage: "arrow.up") }
                }
                ForEach(directories, id: \.self) { path in
                    Button {
                        folder = path
                        perform { try await browse() }
                    } label: { Label(path.split(separator: "/").last.map(String.init) ?? path, systemImage: "folder") }
                }
                ForEach(entries) { entry in
                    Button { perform { try await open(entry) } } label: { Label(entry.name, systemImage: "doc") }
                }
                if cursor != nil { Button("加载下一页") { perform { try await browse(append: true) } } }
                if directories.isEmpty && entries.isEmpty { Text("此目录为空").foregroundStyle(.secondary) }
            }
        }
        .refreshable {
            do { try await browse() } catch { message = error.localizedDescription }
        }
    }

    private func perform(_ work: @escaping @MainActor () async throws -> Void) {
        guard !working else { return }
        working = true
        Task {
            defer { working = false }
            do { try await work() } catch { message = error.localizedDescription }
        }
    }

    private func login() async throws {
        let result = try await Bridge.call("login", ["server": server, "password": password])
        guard let server = result["server"] as? String,
              let session = result["session"] as? [String: Any], let token = session["token"] as? String else {
            throw ZXSFailure(message: "服务器未返回登录凭证")
        }
        let saved = SavedConnection(server: server, token: token)
        try saved.save()
        password = ""
        connection = saved
        try await browse()
    }

    private func logout() async throws {
        if let connection { _ = try await Bridge.call("logout", ["connection": connection.fields]) }
        SavedConnection.clear()
        connection = nil
        folder = "/"
        entries = []
        directories = []
        cursor = nil
    }

    private func browse(append: Bool = false) async throws {
        guard let connection else { return }
        var fields: [String: Any] = ["connection": connection.fields, "folder": folder]
        if append, let cursor { fields["cursor"] = cursor }
        let data = try await Bridge.call("browse", fields)
        let files = (data["containers"] as? [[String: Any]] ?? []).compactMap(FileEntry.init)
        if append { entries += files } else {
            entries = files
            directories = (data["folders"] as? [[String: Any]] ?? []).compactMap { $0["folder"] as? String }
        }
        cursor = data["next_cursor"] as? String
    }

    private func backup(_ selected: [PhotosPickerItem]) async throws {
        defer { picks = [] }
        guard let connection else { return }
        for item in selected {
            guard let media = try await item.loadTransferable(type: PickedMedia.self) else {
                throw ZXSFailure(message: "无法读取所选相片或视频")
            }
            defer { try? FileManager.default.removeItem(at: media.url.deletingLastPathComponent()) }
            _ = try await Bridge.call("backup", [
                "connection": connection.fields, "file": media.url.path,
                "name": media.url.lastPathComponent, "folder": "/相册"
            ])
        }
        folder = "/相册"
        try await browse()
        message = "已检查并备份 \(selected.count) 个项目"
    }

    private func open(_ entry: FileEntry) async throws {
        guard let connection else { return }
        var args: [String: Any] = ["connection": connection.fields, "id": entry.id]
        let result = try await Bridge.call("media", args)
        let type = result["type"] as? String ?? ""
        if (type.hasPrefix("video/") || type.hasPrefix("audio/")),
           let text = result["url"] as? String, let url = URL(string: text) {
            opened = Opened(url: url, media: true)
            return
        }
        args["cache"] = FileManager.default.temporaryDirectory.appendingPathComponent("ZXS-Documents").path
        let doc = try await Bridge.call("document", args)
        guard let path = doc["path"] as? String else { throw ZXSFailure(message: "下载没有返回本地路径") }
        opened = Opened(url: URL(fileURLWithPath: path), media: false)
    }
}

struct PlayerView: View {
    @State private var player: AVPlayer
    init(url: URL) { _player = State(initialValue: AVPlayer(url: url)) }
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
