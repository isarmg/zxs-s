import SwiftUI

struct BundlePage {
    let workspace: FileEntry
    let directory: String
    let version: UInt32
    let entries: [BundleEntry]
    let cursor: String?

    var isRoot: Bool { directory == "." }

    var parent: String {
        directory.lastIndex(of: "/").map { String(directory[..<$0]) } ?? "."
    }
}
import UIKit
import UniformTypeIdentifiers

enum MobileTheme {
    static let canvas = adaptive(0xF5F6F8, 0x101216)
    static let surface = adaptive(0xFFFFFF, 0x16191F)
    static let subtle = adaptive(0xF0F2F6, 0x212630)
    static let border = adaptive(0xE2E6EE, 0x282E3A)
    static let primary = adaptive(0x111827, 0xECF0F5)
    static let secondary = adaptive(0x4B5565, 0xA6B0C0)
    static let muted = adaptive(0x656E83, 0x8490A3)
    static let accent = adaptive(0x2150CC, 0x3468E8)
    static let accentText = adaptive(0x1E47B8, 0x8AB0FF)
    static let accentSoft = adaptive(0xEDF2FF, 0x192438)
    static let onAccent = Color.white
    static let warn = adaptive(0x9A4D08, 0xE5A93B)
    static let danger = adaptive(0xB91C1C, 0xF06A6A)

    private static func adaptive(_ light: UInt32, _ dark: UInt32) -> Color {
        Color(uiColor: UIColor { traits in
            let value = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(
                red: CGFloat((value >> 16) & 255) / 255,
                green: CGFloat((value >> 8) & 255) / 255,
                blue: CGFloat(value & 255) / 255,
                alpha: 1
            )
        })
    }
}

enum EntryGlyph: Equatable {
    case disk, folder, photo, video, audio, archive, document

    init(name: String) {
        let ext = (name as NSString).pathExtension.lowercased()
        if ext == "zip" { self = .archive }
        else if !ext.isEmpty, let type = UTType(filenameExtension: ext) {
            if type.conforms(to: .image) { self = .photo }
            else if type.conforms(to: .movie) { self = .video }
            else if type.conforms(to: .audio) { self = .audio }
            else { self = .document }
        } else { self = .document }
    }

    var symbol: String {
        switch self {
        case .disk: "internaldrive"
        case .folder: "folder"
        case .photo: "photo"
        case .video: "film"
        case .audio: "waveform"
        case .archive: "archivebox"
        case .document: "doc.text"
        }
    }

    var colors: (ink: Color, fill: Color) {
        self == .folder
            ? (MobileTheme.accentText, MobileTheme.accentSoft)
            : (MobileTheme.secondary, MobileTheme.subtle)
    }
}

struct FileEntry: Identifiable {
    let id: String
    let version: UInt32
    let name: String
    let subtitle: String
    let glyph: EntryGlyph

    init?(_ map: [String: Any]) {
        guard let id = map["container_id"] as? String, let name = map["name"] as? String,
              let version = map["latest_version"] as? NSNumber,
              let updated = map["updated_at"] as? NSNumber,
              let tags = map["tags"] as? [String] else { return nil }
        self.id = id
        self.version = version.uint32Value
        self.name = name
        glyph = EntryGlyph(name: name)
        let stamp = Date(timeIntervalSince1970: updated.doubleValue)
        subtitle = (["v\(version.uint32Value)", Self.date.string(from: stamp)] + tags).joined(separator: " · ")
    }

    private static let date: DateFormatter = {
        let format = DateFormatter()
        format.locale = Locale(identifier: "en_US_POSIX")
        format.timeZone = .autoupdatingCurrent
        format.dateFormat = "yyyy-MM-dd HH:mm"
        return format
    }()
}

struct BundleEntry: Identifiable {
    let id: String
    let name: String
    let directory: Bool

    init?(_ map: [String: Any]) {
        guard let path = map["path"] as? String, let name = map["name"] as? String,
              let kind = map["kind"] as? String else { return nil }
        id = path
        self.name = name
        directory = kind == "directory"
    }
}

struct FileRow: View {
    @ScaledMetric(relativeTo: .subheadline) private var iconSide: CGFloat = 28
    let title: String
    let subtitle: String
    let glyph: EntryGlyph

    var body: some View {
        let colors = glyph.colors
        HStack(spacing: 8) {
            Image(systemName: glyph.symbol)
                .font(.subheadline.weight(.medium))
                .frame(width: iconSide, height: iconSide)
                .foregroundStyle(colors.ink)
                .background(colors.fill, in: RoundedRectangle(cornerRadius: 6))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.subheadline.weight(.medium)).foregroundStyle(MobileTheme.primary).lineLimit(1)
                Text(subtitle).font(.caption).monospacedDigit().foregroundStyle(MobileTheme.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

struct BrowserNotice: Identifiable {
    let id = UUID()
    let text: String
}

@MainActor @Observable
final class TransferDisplay {
    var label: String? { didSet { sample = nil } }
    private(set) var sample: TransferSample?

    func track<T>(_ work: @MainActor () async throws -> T) async rethrows -> T {
        defer { sample = nil }
        return try await Bridge.$progress.withValue({ sample in
            DispatchQueue.main.sync { MainActor.assumeIsolated { self.sample = sample } }
        }) { try await work() }
    }
}

struct OperationProgress: View {
    let transfer: TransferDisplay
    var cancelDownload: (() -> Void)? = nil
    @State private var cancelling = false

    var body: some View {
        Group {
            if let sample = transfer.sample {
                HStack(spacing: 4) {
                    VStack(alignment: .leading, spacing: 8) {
                        if let label = transfer.label { Text(label).font(.caption).lineLimit(1) }
                        ProgressView(value: sample.fraction) {
                            let direction = sample.direction == 0 ? "上传" : "下载"
                            let rate = ByteCountFormatter.string(fromByteCount: Int64(clamping: sample.rate), countStyle: .file)
                            Text("\(direction) \(Int(sample.fraction * 100))% · \(rate)/s").monospacedDigit()
                        }
                    }.frame(minWidth: 160)
                    if sample.direction == 1, let cancelDownload {
                        Button {
                            cancelling = true
                            cancelDownload()
                        } label: {
                            Image(systemName: "xmark").font(.caption.weight(.semibold)).frame(width: 44, height: 44)
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(MobileTheme.secondary)
                        .accessibilityLabel("取消下载")
                        .disabled(cancelling)
                    }
                }
            } else if let label = transfer.label { ProgressView(label) }
            else { ProgressView() }
        }
        .padding().background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
    }
}

struct NoticeView: View {
    let notice: BrowserNotice
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.circle")
                .foregroundStyle(MobileTheme.danger)
                .accessibilityHidden(true)
            Text(notice.text).font(.subheadline).foregroundStyle(MobileTheme.primary)
            Button(action: dismiss) {
                Image(systemName: "xmark").frame(minWidth: 44, minHeight: 44)
            }
            .buttonStyle(.plain)
            .foregroundStyle(MobileTheme.secondary)
            .accessibilityLabel("关闭提示")
        }
        .padding(.leading, 12)
        .padding(.trailing, 4)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .overlay { RoundedRectangle(cornerRadius: 12).strokeBorder(MobileTheme.border) }
        .padding(.horizontal, 16)
        .padding(.bottom, 12)
        .transition(.opacity)
    }
}
