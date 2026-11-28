import Foundation
import Observation
import PhotosUI
import SwiftUI

struct UploadTarget {
    let folder: String
    var name: String? = nil
    var revision: (id: String, number: UInt32)? = nil

    var description: String {
        let location = name.map { folder + "/" + $0 } ?? folder
        return revision.map { location + " · 当前 v\($0.number)" } ?? location
    }

    func fields(sourceName: String, photo: Bool) -> [String: Any] {
        if let revision {
            return ["kind": "version", "container_id": revision.id, "expected_version": revision.number]
        }
        return ["kind": photo ? "photo" : "new", "folder": folder, "name": name ?? sourceName]
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
    fileprivate let sources: [UploadSource]
    let photos: Bool

    fileprivate var items: [UploadItem] {
        sources.enumerated().map { index, source in
            UploadItem(id: index, source: source, name: source.name ?? "第 \(index + 1) 个项目", target: target)
        }
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
        selection = UploadSelection(target: target, sources: files.map(UploadSource.document), photos: false)
    }
}

struct UploadMenu: View {
    let folder: String
    let picker: UploadPicker
    @State private var showPhotos = false
    @State private var photos: [PhotosPickerItem] = []

    var body: some View {
        Menu {
            Button("上传文件") { picker.pick(UploadTarget(folder: folder)) }
            Button("上传照片和视频") { showPhotos = true }
        } label: {
            Image(systemName: "square.and.arrow.up")
                .font(.title3.weight(.semibold))
                .frame(width: 56, height: 56)
                .foregroundStyle(MobileTheme.onAccent)
                .background(MobileTheme.accent, in: RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("上传")
        .photosPicker(isPresented: $showPhotos, selection: $photos, matching: .any(of: [.images, .videos]),
                      preferredItemEncoding: .current)
        .onChange(of: photos) { _, selected in
            guard !selected.isEmpty else { return }
            picker.selection = UploadSelection(target: UploadTarget(folder: folder),
                                               sources: selected.map(UploadSource.photo), photos: true)
            photos = []
        }
    }
}

private struct UploadItem: Identifiable {
    let id: Int
    let source: UploadSource
    var name: String
    var target: UploadTarget
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

    private static let statuses: [String: Status] = [
        "committed": .saved, "already_present": .present, "conflict": .conflict
    ]

    static func decode(_ reply: [String: Any], item: UploadItem) throws -> Self {
        guard let outcome = reply["outcome"] as? String, let status = statuses[outcome] else {
            throw ZXSFailure(message: "服务器返回了未知的提交结果")
        }
        if let message = reply["message"] as? String {
            return Self(item: item, status: status, detail: message)
        }
        let commit = reply["commit"] as? [String: Any]
        let target = try UploadTarget.current(commit?["container"] ?? reply["current"])
        return Self(item: item, status: status, detail: target.description, current: target)
    }
}

struct UploadSheet: View {
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
            .navigationTitle(!batch.photos && outcomes.isEmpty ? "确认上传" : "上传结果")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                Button(conflict != nil ? "返回" : outcomes.isEmpty ? "取消" : "完成") {
                    if conflict == nil { dismiss() } else { conflict = nil }
                }
            }
            .disabled(progress.label != nil)
            .overlay { if progress.label != nil { OperationProgress(transfer: progress) } }
        }
        .interactiveDismissDisabled(progress.label != nil)
        .onDisappear { if outcomes.values.contains(where: \.saved) { onSaved() } }
        .task { if batch.photos && outcomes.isEmpty { submit(batch.items) } }
    }

    private var report: some View {
        List {
            Section(batch.photos && !outcomes.isEmpty ? photoSummary : "上传到 \(batch.target.description)") {
                ForEach(batch.items) { item in row(item) }
            }
            if !batch.photos && outcomes.isEmpty {
                Button("上传") { submit(batch.items) }
            } else if !failed.isEmpty {
                Button("重试失败项") { submit(failed) }
            }
        }
    }

    private var photoSummary: String {
        let saved = outcomes.values.filter { $0.status == .saved }.count
        let skipped = outcomes.values.filter { $0.status == .present }.count
        return "已上传 \(saved) · 跳过 \(skipped) · 失败 \(failed.count)"
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
                Text(result.item.name)
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

    private func resolve(_ result: UploadOutcome, to destination: UploadTarget) {
        conflict = nil
        var item = result.item
        item.target = destination
        submit([item])
    }

    private func submit(_ items: [UploadItem]) {
        guard progress.label == nil else { return }
        progress.label = "准备上传"
        Task {
            defer { progress.label = nil }
            do {
                for (index, item) in items.enumerated() {
                    let label = "\(index + 1) / \(items.count)"
                    outcomes[item.id] = try await transfer(item, position: label)
                }
            } catch {
                dismiss()
                onError(error)
            }
        }
    }

    private func transfer(_ selected: UploadItem, position: String) async throws -> UploadOutcome {
        var item = selected
        progress.label = "读取文件 \(position) · \(item.name)"
        do {
            let file = try await item.source.stage()
            defer { file.remove() }
            item.name = file.name
            progress.label = "上传文件 \(position) · \(file.name)"
            let target = item.target.fields(sourceName: file.name, photo: batch.photos)
            let reply = try await progress.track {
                try await Bridge.call("commit_file", [
                    "connection": connection.fields, "file": file.url.path, "target": target
                ])
            }
            return try UploadOutcome.decode(reply, item: item)
        } catch let error as ZXSFailure where error.passwordRejected {
            throw error
        } catch {
            let detail = (error as? CocoaError)?.code == .fileReadNoPermission
                ? "无法读取所选文件，请重新选择" : error.localizedDescription
            return UploadOutcome(item: item, status: .failed, detail: detail)
        }
    }
}
