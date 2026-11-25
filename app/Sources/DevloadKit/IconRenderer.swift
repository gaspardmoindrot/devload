import AppKit

/// The menu bar icon: a dot in the pressure colour, then the memory used.
/// The image keeps one width (the widest figure this Mac can show, digits
/// at fixed width, right-aligned), so the menu bar never shifts. Colours
/// resolve when the image draws, in the menu bar's light or dark appearance.
public enum IconRenderer {
    public static let height: CGFloat = 18
    static let dot: CGFloat = 8
    static let gap: CGFloat = 5
    static var font: NSFont { NSFont.monospacedDigitSystemFont(ofSize: 13, weight: .regular) }

    public static func size(total: UInt64) -> NSSize {
        let w = ceil((Format.iconWidest(total: total) as NSString).size(withAttributes: [.font: font]).width)
        return NSSize(width: dot + gap + w, height: height)
    }

    public static func image(pressure: Pressure, used: UInt64, total: UInt64) -> NSImage {
        let image = draw(text: Format.icon(used: used), dot: Theme.pressure(pressure), total: total)
        image.accessibilityDescription = "\(Format.pressureTitle(pressure)), \(Format.icon(used: used)) used"
        return image
    }

    /// Before the first sample (a few milliseconds after launch).
    public static func placeholder(total: UInt64 = ProcessInfo.processInfo.physicalMemory) -> NSImage {
        let image = draw(text: "-- GB", dot: .tertiaryLabelColor, total: total)
        image.accessibilityDescription = "Measuring memory"
        return image
    }

    static func draw(text: String, dot color: NSColor, total: UInt64) -> NSImage {
        let size = size(total: total)
        let text = text as NSString
        let image = NSImage(size: size, flipped: false) { rect in
            let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.labelColor]
            let ts = text.size(withAttributes: attrs)
            let tx = rect.maxX - ceil(ts.width)
            text.draw(at: NSPoint(x: tx, y: (rect.height - ts.height) / 2), withAttributes: attrs)
            // The dot sits right before the figure.
            let dx = tx - gap - dot
            color.setFill()
            NSBezierPath(ovalIn: NSRect(x: dx, y: (rect.height - dot) / 2, width: dot, height: dot)).fill()
            return true
        }
        image.isTemplate = false
        return image
    }
}
