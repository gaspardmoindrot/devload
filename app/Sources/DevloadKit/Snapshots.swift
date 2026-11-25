import AppKit
import SwiftUI

/// `Devload --snapshot <dir> [--live]`: PNGs of the icon and the popover in
/// light and dark, from the mockup's sample data (shareable) or from this
/// Mac (`--live`, real project names: keep those local).
@MainActor
public enum Snapshots {
    /// The popover's content as a sized view, on the window background.
    public static func popoverView(model: PopoverModel, icons: AppIcons = AppIcons()) -> NSView {
        // Drawn as the open popover looks: its window is the key one.
        let root = PopoverView(model: model, icons: icons).background(Color(nsColor: .windowBackgroundColor))
            .environment(\.controlActiveState, .key)
        let host = NSHostingView(rootView: root)
        host.frame = NSRect(origin: .zero, size: host.fittingSize)
        return host
    }

    /// `popoverView` laid out in a window that is never shown, after a turn
    /// of the run loop: scroll areas only measure their rows there (the
    /// fades of an unfolded list depend on it).
    public static func settledView(model: PopoverModel, icons: AppIcons = AppIcons(),
                                   scroll: ((NSScrollView) -> Void)? = nil) -> NSView {
        let v = popoverView(model: model, icons: icons)
        let window = NSWindow(contentRect: v.frame, styleMask: .borderless, backing: .buffered, defer: true)
        window.contentView = v
        windows.append(window)
        v.layoutSubtreeIfNeeded()
        RunLoop.current.run(until: Date().addingTimeInterval(0.4))
        if let scroll, let s = scrollViews(v).first {
            scroll(s)
            RunLoop.current.run(until: Date().addingTimeInterval(0.4))
        }
        v.layoutSubtreeIfNeeded()
        // Out of the window again: in a window that is not key, controls
        // draw as inactive (grey), unlike the open popover.
        window.contentView = nil
        return v
    }

    /// Kept alive while their views are drawn.
    private static var windows: [NSWindow] = []

    static func scrollViews(_ v: NSView) -> [NSScrollView] {
        (v as? NSScrollView).map { [$0] } ?? v.subviews.flatMap(scrollViews)
    }

    public static func render(to dir: URL, source: CoreSource) throws -> [URL] {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var out: [URL] = []
        func save(_ rep: NSBitmapImageRep, _ name: String) throws {
            let url = dir.appendingPathComponent(name + ".png")
            try Render.png(rep, to: url)
            out.append(url)
        }
        let snap = source.snapshot()
        for dark in [false, true] {
            let theme = dark ? "dark" : "light"
            let bar = NSColor(name: nil) { _ in dark ? NSColor(white: 0.16, alpha: 1) : NSColor(white: 0.93, alpha: 1) }
            for p in Pressure.allCases {
                let img = IconRenderer.image(pressure: p, used: snap?.used ?? 0, total: snap?.total ?? 24 << 30)
                try save(Render.bitmap(img, dark: dark, scale: 2, background: bar), "icon-\(Format.pressureName(p).lowercased())-\(theme)")
            }
            for mode in GraphMode.allCases {
                let model = PopoverModel(source: source, defaults: MemoryDefaults(), loginItem: SampleLoginItem())
                model.select(mode)
                model.refresh()
                try save(Render.bitmap(popoverView(model: model), dark: dark), "popover-\(mode.label.lowercased())-\(theme)")
                // The hover bubble, right of the pointer, then flipped left
                // near the right edge.
                for (side, x) in [("left", CGFloat(150)), ("right", CGFloat(290))] {
                    let g = GraphView()
                    g.series = model.series
                    g.hoverX = x
                    try save(Render.bitmap(g, dark: dark), "graph-hover-\(mode.label.lowercased())-\(side)-\(theme)")
                }
                if mode == .pressure {
                    model.toggle(.apps)
                    try save(Render.bitmap(settledView(model: model), dark: dark), "popover-apps-unfolded-\(theme)")
                    let middle = settledView(model: model) { s in
                        s.contentView.scroll(to: NSPoint(x: 0, y: 4 * PopoverView.rowHeight + 6))
                        s.reflectScrolledClipView(s.contentView)
                    }
                    try save(Render.bitmap(middle, dark: dark), "popover-apps-scrolled-\(theme)")
                    let end = settledView(model: model) { s in
                        let h = s.documentView?.frame.height ?? 0
                        s.contentView.scroll(to: NSPoint(x: 0, y: h - s.contentView.bounds.height))
                        s.reflectScrolledClipView(s.contentView)
                    }
                    try save(Render.bitmap(end, dark: dark), "popover-apps-end-\(theme)")
                    model.toggle(.apps)
                    if let dev = snap?.dev, dev.count > 1 {
                        model.toggleRow(dev[1].id)
                        try save(Render.bitmap(popoverView(model: model), dark: dark), "popover-row-open-\(theme)")
                        model.askQuit(dev[0].id)
                        try save(Render.bitmap(popoverView(model: model), dark: dark), "popover-row-quit-\(theme)")
                    }
                }
            }
        }
        return out
    }
}
