import Foundation
import SwiftUI
import UniformTypeIdentifiers

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
    @State private var directories: [String] = []
    @State private var uploadPicker = UploadPicker()
    @State private var transfer = TransferDisplay()
    @State private var opened: Opened?
    @State private var bundle: BundlePage?
    @State private var notice: BrowserNotice?
    @State private var operation: Task<Void, Never>?

    private var working: Bool { operation != nil }

    var body: some View {
        @Bindable var picker = uploadPicker
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
            .sheet(item: $opened) { item in
                FilePreview(file: item) { opened = nil }
            }
            .task {
                if connection != nil && entries.isEmpty { perform { try await browse() } }
            }
        }
        .fileImporter(isPresented: $picker.isPresented, allowedContentTypes: [.data],
                      allowsMultipleSelection: picker.allowsMultiple) { result in
            do { picker.accept(try result.get()) } catch { showError(error) }
        }
        .sheet(item: $picker.selection) { selection in
            if let connection {
                UploadSheet(batch: selection, connection: connection,
                            onSaved: { perform { try await browse() } }, onError: showError)
            }
        }
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
            if let bundle {
                bundleSection(bundle)
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
                if let bundle { try await browseBundle(bundle, latest: true) }
                else { try await browse() }
            }
            await operation?.value
        }
        .safeAreaInset(edge: .bottom) {
            if folder != "/" && bundle == nil {
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
                    perform { try await browse(target: (folder as NSString).deletingLastPathComponent) }
                } label: { Label("上一级", systemImage: "arrow.up") }
            }
            ForEach(directories, id: \.self) { directory in
                Button {
                    perform { try await browse(target: directory) }
                } label: {
                    FileRow(title: (directory as NSString).lastPathComponent, subtitle: "",
                            glyph: folder == "/" ? .disk : .folder)
                }
            }
            ForEach(entries) { entry in
                HStack {
                    Button { perform { try await open(entry) } } label: {
                        FileRow(title: entry.name, subtitle: entry.subtitle, glyph: entry.glyph)
                    }
                    Menu {
                        Button("上传新版本") {
                            uploadPicker.pick(.init(folder: folder, name: entry.name, revision: (entry.id, entry.version)))
                        }
                    } label: {
                        Image(systemName: "ellipsis").frame(width: 44, height: 44)
                            .accessibilityLabel("更多 · \(entry.name)")
                    }
                }
                .buttonStyle(.borderless)
            }
            if cursor != nil { Button("加载下一页") { perform { try await browse(append: true) } } }
            if directories.isEmpty && entries.isEmpty && !working {
                Text("此目录为空").foregroundStyle(MobileTheme.secondary)
            }
        }
        .listRowBackground(MobileTheme.surface)
    }

    private func bundleSection(_ page: BundlePage) -> some View {
        let path = [folder, page.workspace.name] + (page.isRoot ? [] : [page.directory])
        return Section(path.joined(separator: "/")) {
            Button {
                perform {
                    if page.isRoot { try await browse() }
                    else { try await browseBundle(page, directory: page.parent) }
                }
            } label: { Label("上一级", systemImage: "arrow.up") }
            ForEach(page.entries) { entry in
                Button {
                    perform {
                        if entry.directory { try await browseBundle(page, directory: entry.id) }
                        else { try await openBundleMember(entry, page: page) }
                    }
                } label: {
                    FileRow(title: entry.name, subtitle: "", glyph: entry.directory ? .folder : EntryGlyph(name: entry.name))
                }
            }
            if page.cursor != nil {
                Button("加载下一页") { perform { try await browseBundle(page, append: true) } }
            }
            if page.entries.isEmpty && !working {
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
        if let connection, (error as? ZXSFailure)?.passwordRejected == true {
            server = connection.server
            try? SavedConnection.clear()
            resetConnection()
        }
        notice = BrowserNotice(text: error.localizedDescription)
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
        if let previous { _ = try? await Bridge.call("logout", ["connection": previous.fields]) }
    }

    private func resetConnection() {
        connection = nil
        folder = "/"
        entries = []
        directories = []
        bundle = nil
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
        let ids = Set(files.map(\.id))
        entries = (append ? entries.filter { !ids.contains($0.id) } : []) + files
        if !append {
            directories = (data["folders"] as? [[String: Any]] ?? []).compactMap { $0["folder"] as? String }
        }
        (folder, cursor) = (requested, data["next_cursor"] as? String)
        bundle = nil
    }

    private func browseBundle(_ page: BundlePage, directory: String? = nil,
                              append: Bool = false, latest: Bool = false) async throws {
        guard let connection else { return }
        let path = directory ?? page.directory
        var fields: [String: Any] = [
            "connection": connection.fields, "id": page.workspace.id,
            "directory": path, "cache": Bridge.documentCache.path
        ]
        if !latest { fields["version"] = page.version }
        if append, let cursor = page.cursor { fields["cursor"] = cursor }
        try renderBundle(await Bridge.call("browse_bundle", fields), workspace: page.workspace,
                         directory: path, previous: append ? page.entries : [])
    }

    private func renderBundle(_ data: [String: Any], workspace: FileEntry,
                              directory: String = ".", previous: [BundleEntry] = []) throws {
        guard let version = (data["version"] as? NSNumber)?.uint32Value
        else { throw ZXSFailure(message: "目录工作区缺少版本信息") }
        let entries = (data["entries"] as? [[String: Any]] ?? []).compactMap(BundleEntry.init)
        bundle = BundlePage(workspace: workspace, directory: directory, version: version,
                            entries: previous + entries, cursor: data["next_cursor"] as? String)
    }

    private func open(_ entry: FileEntry) async throws {
        guard let connection else { return }
        let request: [String: Any] = [
            "connection": connection.fields, "id": entry.id, "version": entry.version, "cache": Bridge.documentCache.path
        ]
        let result = try await Bridge.call("open", request)
        if result["bundle"] as? Bool == true {
            try renderBundle(result, workspace: entry)
        } else if let text = result["url"] as? String, let url = URL(string: text) {
            opened = Opened(url: url, media: true, headers: result["headers"] as? [String: String] ?? [:]) {
                do {
                    let refreshed = try await Bridge.call("open", request)
                    return refreshed["headers"] as? [String: String] ?? [:]
                } catch let error as ZXSFailure where error.passwordRejected {
                    showError(error)
                    throw error
                }
            }
        } else {
            try showDownload(result)
        }
    }

    private func openBundleMember(_ entry: BundleEntry, page: BundlePage) async throws {
        guard let connection else { return }
        let result = try await Bridge.call("open_bundle_member", [
            "connection": connection.fields, "id": page.workspace.id, "version": page.version,
            "path": entry.id, "cache": Bridge.documentCache.path
        ])
        let type = result["type"] as? String ?? ""
        try showDownload(result, media: type.hasPrefix("video/") || type.hasPrefix("audio/"))
    }

    private func showDownload(_ result: [String: Any], media: Bool = false) throws {
        guard let path = result["path"] as? String else { throw ZXSFailure(message: "下载没有返回本地路径") }
        opened = Opened(url: URL(fileURLWithPath: path), media: media)
    }
}
