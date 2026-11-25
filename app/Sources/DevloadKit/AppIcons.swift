import AppKit
import UniformTypeIdentifiers

/// The icon of an app row: its bundle's, read from the row's path. A row
/// without one is found by name: a running app with that name, else
/// /Applications/<name>.app. Cached for the life of the popover.
@MainActor
public final class AppIcons {
    private var cache: [String: NSImage?] = [:]

    public init() {}

    public func icon(for row: Row) -> NSImage? {
        let key = row.path.isEmpty ? "name:" + row.name : row.path
        if let hit = cache[key] { return hit }
        let found = Self.lookup(row).map(Self.small)
        cache[key] = found
        return found
    }

    /// For an app whose icon is not found: the plain app icon.
    public static let generic = small(NSWorkspace.shared.icon(for: .applicationBundle))

    /// A 16 pt icon drawn once from the icon's large art, at 4x: some apps'
    /// own 16 and 32 px art is blurry, and the large one stays sharp scaled
    /// down at any screen scale.
    static func small(_ icon: NSImage) -> NSImage {
        let side = 16, scale = 4
        let rect = NSRect(x: 0, y: 0, width: side * scale, height: side * scale)
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: side * scale, pixelsHigh: side * scale,
                                         bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                         colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return icon }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSGraphicsContext.current?.imageInterpolation = .high
        icon.draw(in: rect, from: .zero, operation: .copy, fraction: 1)
        NSGraphicsContext.restoreGraphicsState()
        rep.size = NSSize(width: side, height: side)
        let out = NSImage(size: rep.size)
        out.addRepresentation(rep)
        return out
    }

    static func lookup(_ row: Row) -> NSImage? {
        if !row.path.isEmpty, FileManager.default.fileExists(atPath: row.path) {
            return NSWorkspace.shared.icon(forFile: row.path)
        }
        return lookup(row.name)
    }

    static func lookup(_ name: String) -> NSImage? {
        if let app = NSWorkspace.shared.runningApplications.first(where: { $0.localizedName == name }), let icon = app.icon {
            return icon.copy() as? NSImage
        }
        for dir in ["/Applications", "/System/Applications", "/System/Applications/Utilities"] {
            let path = "\(dir)/\(name).app"
            if FileManager.default.fileExists(atPath: path) { return NSWorkspace.shared.icon(forFile: path) }
        }
        return nil
    }
}
