import SwiftUI

struct BundleLocation {
    let workspace: FileEntry
    var directory = "."
    var version: UInt32? = nil

    var isRoot: Bool { directory == "." }

    func at(_ path: String) -> Self { Self(workspace: workspace, directory: path, version: version) }

    func atVersion(_ number: UInt32?) -> Self { Self(workspace: workspace, directory: directory, version: number) }

    var parent: Self {
        at(directory.lastIndex(of: "/").map { String(directory[..<$0]) } ?? ".")
    }
}
import UIKit
import UniformTypeIdentifiers

enum MobileTheme {
    static let canvas = adaptive(0xF6F7F9, 0x111317)
    static let surface = adaptive(0xFFFFFF, 0x181B21)
    static let subtle = adaptive(0xEEF1F6, 0x232833)
    static let border = adaptive(0xE2E6EE, 0x2A303C)
    static let primary = adaptive(0x111827, 0xECEFF4)
    static let secondary = adaptive(0x4B5565, 0x9CA5B4)
    static let accent = adaptive(0x1D4ED8, 0x6B97FF)
    static let folder = adaptive(0xB45309, 0xFBBF24)
    static let folderSoft = adaptive(0xFEF3C7, 0x2E2412)
    static let media = adaptive(0x633C96, 0xC4A8F6)
    static let mediaSoft = adaptive(0xF2ECFC, 0x2B2240)
    static let archive = adaptive(0x14655F, 0x73CFBE)
    static let archiveSoft = adaptive(0xE5F5F2, 0x142D2B)
    static let ok = adaptive(0x15803D, 0x34D399)
    static let danger = adaptive(0xB91C1C, 0xF87171)

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
        switch self {
        case .folder: (MobileTheme.folder, MobileTheme.folderSoft)
        case .photo, .video, .audio: (MobileTheme.media, MobileTheme.mediaSoft)
        case .archive: (MobileTheme.archive, MobileTheme.archiveSoft)
        case .disk, .document: (MobileTheme.secondary, MobileTheme.subtle)
        }
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

struct FolderEntry: Identifiable {
    let id: String
    let name: String

    init?(_ map: [String: Any]) {
        guard let path = map["folder"] as? String else { return nil }
        id = path
        name = path.split(separator: "/").last.map(String.init) ?? path
    }
}

struct FileRow: View {
    @ScaledMetric(relativeTo: .subheadline) private var iconSide: CGFloat = 28
    let title: String
    let subtitle: String
    let glyph: EntryGlyph

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: glyph.symbol)
                .font(.subheadline.weight(.medium))
                .frame(width: iconSide, height: iconSide)
                .foregroundStyle(glyph.colors.ink)
                .background(glyph.colors.fill, in: RoundedRectangle(cornerRadius: 6))
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
    let isError: Bool
}

struct OperationProgress: View {
    let label: String?

    var body: some View {
        Group {
            if let label { ProgressView(label) }
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
            Image(systemName: notice.isError ? "exclamationmark.circle" : "checkmark")
                .foregroundStyle(notice.isError ? MobileTheme.danger : MobileTheme.ok)
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
