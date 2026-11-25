import AppKit

/// Off-screen rendering, for tests and `--snapshot` captures.
public enum Render {
    /// Draws `image` at `scale` on an optional background, in light or dark.
    public static func bitmap(_ image: NSImage, dark: Bool, scale: CGFloat = 2, background: NSColor? = nil) -> NSBitmapImageRep {
        let size = image.size
        let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width * scale), pixelsHigh: Int(size.height * scale),
                                   bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                   colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        rep.size = size
        appearance(dark).performAsCurrentDrawingAppearance {
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
            if let background {
                background.setFill()
                NSRect(origin: .zero, size: size).fill()
            }
            image.draw(in: NSRect(origin: .zero, size: size))
            NSGraphicsContext.restoreGraphicsState()
        }
        return rep
    }

    /// Lays out and draws `view` (its own size) in light or dark.
    @MainActor
    public static func bitmap(_ view: NSView, dark: Bool) -> NSBitmapImageRep {
        view.appearance = appearance(dark)
        view.layoutSubtreeIfNeeded()
        view.displayIfNeeded()
        precondition(!view.bounds.isEmpty, "a view with a size")
        let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
        view.cacheDisplay(in: view.bounds, to: rep)
        return rep
    }

    public static func png(_ rep: NSBitmapImageRep, to url: URL) throws {
        try rep.representation(using: .png, properties: [:])!.write(to: url)
    }

    public static func appearance(_ dark: Bool) -> NSAppearance {
        NSAppearance(named: dark ? .darkAqua : .aqua)!
    }
}

extension NSBitmapImageRep {
    /// sRGB components 0–255 of the pixel at (x, y) in points from the top left.
    public func rgba(x: CGFloat, y: CGFloat) -> (r: Int, g: Int, b: Int, a: Int) {
        let sx = Double(pixelsWide) / Double(size.width)
        guard let c = colorAt(x: Int(x * sx), y: Int(y * sx))?.usingColorSpace(.sRGB) else { return (0, 0, 0, 0) }
        return (Int(c.redComponent * 255 + 0.5), Int(c.greenComponent * 255 + 0.5),
                Int(c.blueComponent * 255 + 0.5), Int(c.alphaComponent * 255 + 0.5))
    }
}
