import Foundation
import PhotosUI
import SwiftUI

@main
struct ZXSApp: App {
    var body: some Scene { WindowGroup { BrowserView() } }
}

private struct BundleLocation {
    let workspace: FileEntry
    var directory = "."

    var isRoot: Bool { directory == "." }

    func at(_ path: String) -> Self { Self(workspace: workspace, directory: path) }

    var parent: Self {
        at(directory.lastIndex(of: "/").map { String(directory[..<$0]) } ?? ".")
    }
}

struct BrowserView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
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
    @State private var bundleLocation: BundleLocation?
    @State private var bundleEntries: [BundleEntry] = []
    @State private var bundleCursor: String?
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
                FilePreview(file: item) { opened = nil }
            }
            .task {
                if connection != nil && entries.isEmpty { perform { try await browse() } }
            }
            .onChange(of: picks) { _, selected in
                if !selected.isEmpty { perform { try await backup(selected) } }
            }
        }
        .tint(MobileTheme.accent)
        .task(id: scenePhase) {
            if scenePhase == .active { try? await FileAccess.shared.update() }
        }
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
            if let bundleLocation {
                bundleSection(bundleLocation)
            } else {
                archiveSections
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .background(MobileTheme.canvas)
        .refreshable {
            guard !working else { return }
            working = true
            defer { working = false }
            do {
                if let bundleLocation { try await browseBundle(bundleLocation) }
                else { try await browse() }
                try? await FileAccess.shared.update(folder: folder)
            } catch { showError(error) }
        }
    }

    @ViewBuilder
    private var archiveSections: some View {
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
                    perform {
                        if entry.glyph == .bundle { try await browseBundle(BundleLocation(workspace: entry)) }
                        else { try await open(entry) }
                    }
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

    private func bundleSection(_ location: BundleLocation) -> some View {
        let path = [folder, location.workspace.name] + (location.isRoot ? [] : [location.directory])
        return Section(path.joined(separator: "/")) {
            Button {
                perform {
                    if location.isRoot { try await browse() }
                    else { try await browseBundle(location.parent) }
                }
            } label: { Label("上一级", systemImage: "arrow.up") }
            ForEach(bundleEntries) { entry in
                Button {
                    perform {
                        if entry.directory { try await browseBundle(location.at(entry.id)) }
                        else { try await openBundleMember(entry, location: location) }
                    }
                } label: {
                    FileRow(title: entry.name, subtitle: "", glyph: entry.directory ? .folder : EntryGlyph(name: entry.name))
                }
            }
            if bundleCursor != nil {
                Button("加载下一页") { perform { try await browseBundle(location, append: true) } }
            }
            if bundleEntries.isEmpty && !working {
                Text("此目录为空").foregroundStyle(MobileTheme.secondary)
            }
        }
        .listRowBackground(MobileTheme.surface)
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
            try? SavedConnection.clear()
            resetConnection()
            Task { try? await FileAccess.shared.update() }
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
        do { try await FileAccess.shared.update() }
        catch { notice = BrowserNotice(text: "文件 App 连接失败 · \(error.localizedDescription)", isError: true) }
        try await browse(target: "/")
    }

    private func logout() async throws {
        let previous = connection
        try SavedConnection.clear()
        resetConnection()
        do { try await FileAccess.shared.update() }
        catch { notice = BrowserNotice(text: "文件 App 退出失败 · \(error.localizedDescription)", isError: true) }
        if let previous { _ = try await Bridge.call("logout", ["connection": previous.fields]) }
    }

    private func resetConnection() {
        connection = nil
        folder = "/"
        entries = []
        directories = []
        bundleLocation = nil
        bundleEntries = []
        bundleCursor = nil
        opened = nil
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
        bundleLocation = nil
        bundleEntries = []
        bundleCursor = nil
    }

    private func browseBundle(_ location: BundleLocation, append: Bool = false) async throws {
        guard let connection else { return }
        var fields: [String: Any] = [
            "connection": connection.fields, "id": location.workspace.id,
            "version": location.workspace.version, "directory": location.directory
        ]
        if append, let bundleCursor { fields["cursor"] = bundleCursor }
        let data = try await Bridge.call("browse_bundle", fields)
        let entries = (data["entries"] as? [[String: Any]] ?? []).compactMap(BundleEntry.init)
        if append { bundleEntries += entries } else { bundleEntries = entries }
        bundleCursor = data["next_cursor"] as? String
        bundleLocation = location
    }

    private func backup(_ selected: [PhotosPickerItem]) async throws {
        defer { picks = [] }
        guard let connection else { return }
        let disk = albumDisk
        guard !disk.isEmpty else { throw ZXSFailure(message: "请先选择相册备份数据盘") }
        let album = disk + "/相册"
        try await backupPhotos(selected, connection: connection, folder: album)
        try await browse(target: album)
        try? await FileAccess.shared.update(folder: album)
        notice = BrowserNotice(text: "已检查并备份 \(selected.count) 个项目", isError: false)
    }

    private func open(_ entry: FileEntry) async throws {
        guard let connection else { return }
        let cache = FileManager.default.temporaryDirectory.appendingPathComponent("ZXS-Documents")
        let result = try await Bridge.call("open", [
            "connection": connection.fields, "id": entry.id, "version": entry.version, "cache": cache.path
        ])
        if result["bundle"] as? Bool == true {
            try await browseBundle(BundleLocation(workspace: entry))
            return
        }
        if let text = result["url"] as? String, let url = URL(string: text) {
            opened = Opened(url: url, media: true, headers: result["headers"] as? [String: String] ?? [:])
            return
        }
        guard let path = result["path"] as? String else { throw ZXSFailure(message: "下载没有返回本地路径") }
        opened = Opened(url: URL(fileURLWithPath: path), media: false)
    }

    private func openBundleMember(_ entry: BundleEntry, location: BundleLocation) async throws {
        guard let connection else { return }
        let cache = FileManager.default.temporaryDirectory.appendingPathComponent("ZXS-Documents")
        let result = try await Bridge.call("open_bundle_member", [
            "connection": connection.fields, "id": location.workspace.id, "version": location.workspace.version,
            "path": entry.id, "cache": cache.path
        ])
        guard let path = result["path"] as? String else { throw ZXSFailure(message: "下载没有返回本地路径") }
        let type = result["type"] as? String ?? ""
        opened = Opened(url: URL(fileURLWithPath: path), media: type.hasPrefix("video/") || type.hasPrefix("audio/"))
    }
}
