import AppKit
import SwiftUI

/// The colours: macOS system colours for meaning (pressure, groups), so
/// they follow light, dark and Increase Contrast like Activity Monitor and
/// Stats; neutral greys of our own for lines.
public enum Theme {
    static func dynamic(_ light: UInt32, _ dark: UInt32) -> NSColor {
        NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
            return rgb(isDark ? dark : light)
        }
    }

    static func rgb(_ v: UInt32) -> NSColor {
        NSColor(srgbRed: CGFloat((v >> 16) & 0xff) / 255, green: CGFloat((v >> 8) & 0xff) / 255,
                blue: CGFloat(v & 0xff) / 255, alpha: 1)
    }

    public static let good = NSColor.systemGreen
    public static let warn = NSColor.systemYellow
    public static let bad = NSColor.systemRed
    public static let accent = NSColor.systemBlue
    public static let grid = dynamic(0xe2e1dc, 0x3a3a36)
    public static let edge = dynamic(0xb9b8b2, 0x5c5b56)
    public static let quiet = NSColor.secondaryLabelColor

    public static func pressure(_ p: Pressure) -> NSColor {
        switch p {
        case .normal: good
        case .tight: warn
        case .critical: bad
        }
    }

    /// Groups mode: top 1, 2, 3, then Other. No green, yellow or red, which
    /// mean pressure.
    public static let groups: [NSColor] = [.systemBlue, .systemOrange, .systemPurple, .systemGray]
}

extension Color {
    init(_ ns: NSColor) { self.init(nsColor: ns) }
}
