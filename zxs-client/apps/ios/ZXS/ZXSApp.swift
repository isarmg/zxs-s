import Foundation
import SwiftUI

@main struct ZXSApp: App {
    var body: some Scene { WindowGroup { BrowserView() } }
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
    @State private var uploadPicker = UploadPicker()
    @State private var transfer = TransferDisplay()
    @State private var opened: Opened?
    @State private var bundleLocation: BundleLocation?
    @State private var bundleEntries: [BundleEntry] = []
    @State private var bundleCursor: String?
    @State private var notice: BrowserNotice?
    @State private var operation: Task<Void, Never>?

    private var working: Bool { operation != nil }

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
            .disabled(working)
            .overlay {
                if let operation {
                    OperationProgress(transfer: transfer, cancelDownload: { operation.cancel() })
                }
            }
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
        }
        .modifier(UploadPresentation(picker: uploadPicker, connection: connection,
                                     onSaved: { perform { try await browse() } }, onError: showError))
        .tint(MobileTheme.accent)
    }

    private var loginForm: some View {
        Form {
            TextField("局域网服务器地址", text: $server)
                .textInputAutocapitalization(.never)
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
        .refreshable { @MainActor in
            guard !working else { return }
            perform {
                if let bundleLocation { try await browseBundle(bundleLocation.atVersion(nil)) }
                else { try await browse() }
            }
            await operation?.value
        }
        .safeAreaInset(edge: .bottom) {
            if folder != "/" && bundleLocation == nil {
                UploadMenu(folder: folder, picker: uploadPicker)
                    .frame(maxWidth: .infinity, alignment: .trailing)
                    .padding([.trailing, .bottom], 16)
            }
        }
    }

    @ViewBuilder
    private var archiveSections: some View {
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
                ArchiveFileRow(entry: entry, folder: folder, picker: uploadPicker) {
                    perform { try await open(entry) }
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
        operation = Task {
            defer { operation = nil }
            do { try await transfer.track(work) }
            catch is CancellationError {}
            catch { showError(error) }
        }
    }

    private func showError(_ error: Error) {
        if connection != nil, (error as? ZXSFailure)?.passwordRejected == true {
            try? SavedConnection.clear()
            resetConnection()
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
        let previous = connection
        try SavedConnection.clear()
        resetConnection()
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
        cursor = nil
        notice = nil
    }

    private func browse(target: String? = nil, append: Bool = false) async throws {
        guard let connection else { return }
        let requested = target ?? folder
        var fields: [String: Any] = ["connection": connection.fields, "folder": requested]
        if append, let cursor { fields["cursor"] = cursor }
        let data = try await Bridge.call("browse", fields)
        let files = (data["containers"] as? [[String: Any]] ?? []).compactMap(FileEntry.init)
        if append { entries += files } else {
            entries = files
            directories = (data["folders"] as? [[String: Any]] ?? []).compactMap(FolderEntry.init)
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
            "directory": location.directory, "cache": Bridge.documentCache.path
        ]
        if let version = location.version { fields["version"] = version }
        if append, let bundleCursor { fields["cursor"] = bundleCursor }
        try renderBundle(await Bridge.call("browse_bundle", fields), at: location, append: append)
    }

    private func renderBundle(_ data: [String: Any], at location: BundleLocation, append: Bool = false) throws {
        guard let version = (data["version"] as? NSNumber)?.uint32Value
        else { throw ZXSFailure(message: "目录工作区缺少版本信息") }
        let entries = (data["entries"] as? [[String: Any]] ?? []).compactMap(BundleEntry.init)
        if append { bundleEntries += entries } else { bundleEntries = entries }
        bundleCursor = data["next_cursor"] as? String
        bundleLocation = location.atVersion(version)
    }

    private func open(_ entry: FileEntry) async throws {
        guard let connection else { return }
        let request: [String: Any] = [
            "connection": connection.fields, "id": entry.id, "version": entry.version, "cache": Bridge.documentCache.path
        ]
        let result = try await Bridge.call("open", request)
        if result["bundle"] as? Bool == true {
            try renderBundle(result, at: BundleLocation(workspace: entry))
            return
        }
        if let text = result["url"] as? String, let url = URL(string: text) {
            opened = Opened(url: url, media: true, headers: result["headers"] as? [String: String] ?? [:]) {
                do {
                    let refreshed = try await Bridge.call("open", request)
                    return refreshed["headers"] as? [String: String] ?? [:]
                } catch let error as ZXSFailure where error.passwordRejected {
                    showError(error)
                    throw error
                }
            }
            return
        }
        guard let path = result["path"] as? String else { throw ZXSFailure(message: "下载没有返回本地路径") }
        opened = Opened(url: URL(fileURLWithPath: path), media: false)
    }

    private func openBundleMember(_ entry: BundleEntry, location: BundleLocation) async throws {
        guard let connection, let version = location.version else { return }
        let result = try await Bridge.call("open_bundle_member", [
            "connection": connection.fields, "id": location.workspace.id, "version": version,
            "path": entry.id, "cache": Bridge.documentCache.path
        ])
        guard let path = result["path"] as? String else { throw ZXSFailure(message: "下载没有返回本地路径") }
        let type = result["type"] as? String ?? ""
        opened = Opened(url: URL(fileURLWithPath: path), media: type.hasPrefix("video/") || type.hasPrefix("audio/"))
    }
}
