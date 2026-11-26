import Foundation
import Observation
import SwiftUI
import UniformTypeIdentifiers

struct UploadTarget {
    let folder: String
    var name: String? = nil
    var revision: (id: String, number: UInt32)? = nil

    var location: String { name.map { folder + "/" + $0 } ?? folder }

    var description: String {
        revision.map { location + " · 当前 v\($0.number)" } ?? location
    }

    func fields(sourceName: String) -> [String: Any] {
        if let revision {
            return ["kind": "version", "container_id": revision.id, "expected_version": revision.number]
        }
        return ["kind": "new", "folder": folder, "name": name ?? sourceName]
    }

    static func current(_ value: Any?) throws -> Self {
        guard let container = value as? [String: Any], let folder = container["folder"] as? String,
              let name = container["name"] as? String, let id = container["container_id"] as? String,
              let version = container["latest_version"] as? NSNumber else {
            throw ZXSFailure(message: "提交结果缺少文件信息")
        }
        return Self(folder: folder, name: name, revision: (id, version.uint32Value))
    }
}

struct UploadSelection: Identifiable {
    let id = UUID()
    let target: UploadTarget
    let files: [URL]

    fileprivate var items: [UploadItem] {
        files.enumerated().map { UploadItem(id: $0.offset, url: $0.element, target: target) }
    }
}

@Observable
final class UploadPicker {
    var isPresented = false
    var selection: UploadSelection?
    private var target: UploadTarget?

    var allowsMultiple: Bool { target?.revision == nil }

    func pick(_ target: UploadTarget) {
        self.target = target
        isPresented = true
    }

    func accept(_ files: [URL]) {
        defer { target = nil }
        guard let target, !files.isEmpty, target.revision == nil || files.count == 1 else { return }
        selection = UploadSelection(target: target, files: files)
    }
}

struct UploadPresentation: ViewModifier {
    @Bindable var picker: UploadPicker
    let connection: SavedConnection?
    let onSaved: () -> Void
    let onError: (Error) -> Void

    func body(content: Content) -> some View {
        content.fileImporter(isPresented: $picker.isPresented, allowedContentTypes: [.data],
                             allowsMultipleSelection: picker.allowsMultiple) { result in
            do { picker.accept(try result.get()) } catch { onError(error) }
        }.sheet(item: $picker.selection) { selection in
            if let connection {
                UploadSheet(batch: selection, connection: connection, onSaved: onSaved, onError: onError)
            }
        }
    }
}

struct ArchiveFileRow: View {
    let entry: FileEntry
    let folder: String
    let picker: UploadPicker
    let open: () -> Void

    var body: some View {
        HStack {
            Button(action: open) {
                FileRow(title: entry.name, subtitle: entry.subtitle, glyph: entry.glyph)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            Menu {
                Button("上传新版本") {
                    picker.pick(UploadTarget(folder: folder, name: entry.name, revision: (entry.id, entry.version)))
                }
            } label: {
                Image(systemName: "ellipsis").frame(width: 44, height: 44)
                    .accessibilityLabel("更多 · \(entry.name)")
            }
        }.buttonStyle(.borderless)
    }
}

private struct UploadItem: Identifiable {
    let id: Int
    let url: URL
    var target: UploadTarget

    var name: String { url.lastPathComponent }

    func at(_ destination: UploadTarget) -> Self { Self(id: id, url: url, target: destination) }
}

private struct UploadOutcome {
    enum Status: String {
        case saved = "已提交", present = "已存在", conflict = "冲突", failed = "失败", skipped = "已跳过"
    }

    let item: UploadItem
    let status: Status
    let detail: String
    var current: UploadTarget? = nil

    var saved: Bool { status == .saved || status == .present }

    static func decode(_ reply: [String: Any], item: UploadItem) throws -> Self {
        switch reply["outcome"] as? String {
        case "committed", "already_present":
            let commit = reply["commit"] as? [String: Any]
            let target = try UploadTarget.current(commit?["container"])
            let status: Status = reply["outcome"] as? String == "committed" ? .saved : .present
            return Self(item: item, status: status, detail: target.description, current: target)
        case "conflict":
            let target = try UploadTarget.current(reply["current"])
            return Self(item: item, status: .conflict, detail: target.description, current: target)
        default: throw ZXSFailure(message: "服务器返回了未知的提交结果")
        }
    }

    static func failed(_ item: UploadItem, error: Error) -> Self {
        if (error as? ZXSFailure)?.status == 409 {
            return Self(item: item, status: .conflict,
                        detail: error.localizedDescription + "。请改名另存，或返回选择其他目录。")
        }
        let detail = (error as? CocoaError)?.code == .fileReadNoPermission
            ? "无法读取所选文件，请重新选择" : error.localizedDescription
        return Self(item: item, status: .failed, detail: detail)
    }
}

private struct UploadSheet: View {
    @Environment(\.dismiss) private var dismiss
    @State private var outcomes: [Int: UploadOutcome] = [:]
    @State private var progress = TransferDisplay()
    @State private var conflict: UploadOutcome?
    @State private var newName = ""

    let batch: UploadSelection
    let connection: SavedConnection
    let onSaved: () -> Void
    let onError: (Error) -> Void

    var body: some View {
        NavigationStack {
            Group {
                if let conflict { conflictForm(conflict) }
                else { report }
            }
            .navigationTitle(outcomes.isEmpty ? "确认上传" : "上传结果")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { Button(conflict == nil ? "完成" : "返回") { close() } }
            .disabled(progress.label != nil)
            .overlay { if progress.label != nil { OperationProgress(transfer: progress) } }
        }
        .interactiveDismissDisabled(progress.label != nil)
        .onDisappear(perform: refresh)
    }

    private var report: some View {
        List {
            Section("上传到 \(batch.target.description)") {
                ForEach(batch.items) { item in row(item) }
            }
            if outcomes.isEmpty {
                Button("上传") { submit(batch.items) }
            } else if !failed.isEmpty {
                Button("重试失败项") { submit(failed) }
            }
        }
    }

    private var failed: [UploadItem] {
        batch.items.compactMap { item in
            guard let result = outcomes[item.id], result.status == .failed else { return nil }
            return result.item
        }
    }

    @ViewBuilder private func row(_ item: UploadItem) -> some View {
        if let result = outcomes[item.id] {
            let label = VStack(alignment: .leading) {
                Text(item.name)
                Text(result.status.rawValue + (result.detail.isEmpty ? "" : " · " + result.detail))
                    .font(.caption).foregroundStyle(MobileTheme.secondary)
            }
            if result.status == .conflict {
                Button {
                    newName = result.item.target.name ?? result.item.name
                    conflict = result
                } label: { label }
            } else { label }
        } else { Text(item.name) }
    }

    private func conflictForm(_ result: UploadOutcome) -> some View {
        Form {
            Section(result.item.name) { Text(result.detail) }
            if let target = result.current {
                Button("追加到最新版") { resolve(result, to: target) }
            }
            Section("另存到 \(result.item.target.folder)") {
                TextField("新文件名", text: $newName)
                Button("改名另存") {
                    resolve(result, to: UploadTarget(folder: result.item.target.folder, name: newName))
                }
                .disabled(newName.isEmpty)
            }
            Button("跳过") {
                outcomes[result.item.id] = UploadOutcome(item: result.item, status: .skipped, detail: "")
                conflict = nil
            }
        }
    }

    private func close() {
        if conflict != nil { conflict = nil } else { dismiss() }
    }

    private func resolve(_ result: UploadOutcome, to destination: UploadTarget) {
        conflict = nil
        submit([result.item.at(destination)])
    }

    private func submit(_ items: [UploadItem]) {
        guard progress.label == nil else { return }
        progress.label = "准备上传"
        Task {
            defer { progress.label = nil }
            for (index, item) in items.enumerated() {
                let label = "\(index + 1) / \(items.count) · \(item.name)"
                do { outcomes[item.id] = try await transfer(item, label: label) }
                catch let error as ZXSFailure where error.passwordRejected {
                    dismiss()
                    onError(error)
                    return
                } catch { outcomes[item.id] = UploadOutcome.failed(item, error: error) }
            }
        }
    }

    private func transfer(_ item: UploadItem, label: String) async throws -> UploadOutcome {
        progress.label = "读取文件 " + label
        let source = item.url
        let file = try await Task.detached { try stageUpload(source) }.value
        defer { try? FileManager.default.removeItem(at: file) }
        progress.label = "上传文件 " + label
        let reply = try await progress.track {
            try await Bridge.call("commit_file", [
                "connection": connection.fields, "file": file.path, "target": item.target.fields(sourceName: item.name)
            ])
        }
        return try UploadOutcome.decode(reply, item: item)
    }

    private func refresh() {
        let folders = Set(outcomes.values.filter(\.saved).compactMap { $0.current?.folder })
        guard !folders.isEmpty else { return }
        onSaved()
        Task {
            for folder in folders { try? await FileAccess.shared.update(folder: folder) }
        }
    }
}

private func stageUpload(_ source: URL) throws -> URL {
    let scoped = source.startAccessingSecurityScopedResource()
    defer { if scoped { source.stopAccessingSecurityScopedResource() } }
    let staged = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    var coordinationError: NSError?
    var copyError: Error?
    NSFileCoordinator().coordinate(readingItemAt: source, options: [], error: &coordinationError) { url in
        do {
            guard try url.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile == true else {
                throw ZXSFailure(message: "请选择普通文件或 ZIP")
            }
            try FileManager.default.copyItem(at: url, to: staged)
        } catch { copyError = error }
    }
    if let error = copyError ?? coordinationError {
        try? FileManager.default.removeItem(at: staged)
        throw error
    }
    return staged
}
