import AppKit

/// The graph: used memory per column, already cut to the view's width by
/// the core. Each area is a gradient (stronger at the top of the graph)
/// under a 1.5 pt line, like Stats and iStat Menus.
/// Groups mode stacks flat bands, each named one under its own line.
///
/// Pressure mode follows each column's peak, coloured by its worst
/// pressure level, so a short spike stays visible. Groups mode stacks the
/// 3 heaviest groups of the period, then everything else in grey.
/// Thin vertical lines mark the round times written under the graph.
/// On hover, a line marks the column under the pointer and a bubble says
/// when it was and what it held.
public final class GraphView: NSView {
    nonisolated public static let size = NSSize(width: 308, height: 72)
    /// The fill's opacity at the top and the bottom of the graph.
    static let fillTop: CGFloat = 0.45
    static let fillBottom: CGFloat = 0.10
    static let lineWidth: CGFloat = 1.5
    /// Groups mode: the named bands, and the quieter "Other".
    static let groupAlpha: CGFloat = 0.45
    static let otherAlpha: CGFloat = 0.22

    public var series: Series? {
        didSet {
            guard series != oldValue else { return }
            needsDisplay = true
            setAccessibilityLabel(series.map(Format.graphLabel) ?? "Memory graph, no data yet.")
        }
    }

    public override init(frame: NSRect) {
        super.init(frame: frame)
        // One image for VoiceOver, its label sums up the period shown.
        setAccessibilityElement(true)
        setAccessibilityRole(.image)
        setAccessibilityLabel("Memory graph, no data yet.")
    }

    public convenience init() {
        self.init(frame: NSRect(origin: .zero, size: Self.size))
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    /// The pointer's x in the view, while it is over the graph.
    var hoverX: CGFloat? {
        didSet { if hoverX != oldValue { needsDisplay = true } }
    }

    public override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                                       owner: self))
    }

    public override func mouseMoved(with event: NSEvent) { hoverX = convert(event.locationInWindow, from: nil).x }
    public override func mouseEntered(with event: NSEvent) { mouseMoved(with: event) }
    public override func mouseExited(with event: NSEvent) { hoverX = nil }

    /// The column under `x`.
    nonisolated static func column(at x: CGFloat, width: CGFloat, columns: Int) -> Int? {
        guard columns > 0, width > 0, x >= 0, x < width else { return nil }
        return min(Int(x / (width / CGFloat(columns))), columns - 1)
    }

    public override var intrinsicContentSize: NSSize { Self.size }
    public override var isFlipped: Bool { false }

    /// Called once per draw, for the click → graph latency trace.
    public var onDraw: (() -> Void)?

    public override func draw(_ dirty: NSRect) {
        let plot = NSRect(x: 0, y: 0, width: bounds.width, height: bounds.height)
        if let s = series, s.width > 0, s.totalMiB > 0 {
            drawTicks(Format.ticks(from: s.from, to: s.to, period: s.period), plot: plot)
            let step = plot.width / CGFloat(s.width)
            let scale = plot.height / CGFloat(s.totalMiB)
            func h(_ mib: Float) -> CGFloat { min(max(CGFloat(mib) * scale, 0), plot.height) }
            switch s.mode {
            case .pressure: drawPressure(s, step: step, h: h, plot: plot)
            case .groups: drawGroups(s, step: step, h: h, plot: plot)
            }
            drawTop(label: Format.axisTop(totalMiB: s.totalMiB), plot: plot)
            if let x = hoverX, let i = Self.column(at: x, width: plot.width, columns: s.width),
               let info = Format.hover(s, column: i) {
                // Pressure mode: a dot on the curve, at the value read out.
                var mark: (y: CGFloat, color: NSColor)?
                if s.mode == .pressure, s.level[i] != Series.noData {
                    mark = (min(CGFloat(s.hi[i]) * scale, plot.height - 3), Theme.pressure(Pressure(raw: s.level[i])))
                }
                drawHover(info, x: (CGFloat(i) + 0.5) * step, mark: mark, plot: plot)
            }
        }
        // Baseline.
        Theme.edge.setFill()
        NSRect(x: 0, y: 0, width: plot.width, height: 1).fill()
        onDraw?()
    }

    /// One area: the columns' rects filled with `color`'s gradient over the
    /// whole graph height, so equal heights look the same everywhere.
    private func fill(_ rects: [NSRect], _ color: NSColor, plot: NSRect, strength: CGFloat = 1) {
        guard !rects.isEmpty else { return }
        NSGraphicsContext.saveGraphicsState()
        let clip = NSBezierPath()
        rects.forEach { clip.appendRect($0) }
        clip.addClip()
        let gradient = NSGradient(starting: color.withAlphaComponent(Self.fillBottom * strength),
                                  ending: color.withAlphaComponent(Self.fillTop * strength))
        gradient?.draw(in: plot, angle: 90)
        NSGraphicsContext.restoreGraphicsState()
    }

    /// A line along the tops `y` of the columns that have one, broken at
    /// gaps; each segment takes the colour of its column.
    private func line(_ tops: [CGFloat?], step: CGFloat, color: (Int) -> NSColor) {
        var paths: [NSColor: NSBezierPath] = [:]
        for i in tops.indices {
            guard let y = tops[i] else { continue }
            let x = CGFloat(i) * step
            let path = paths[color(i)] ?? {
                let p = NSBezierPath()
                p.lineWidth = Self.lineWidth
                p.lineJoinStyle = .round
                paths[color(i)] = p
                return p
            }()
            // Half the width inside so the line is not cut at the top.
            let yy = max(y - Self.lineWidth / 2, Self.lineWidth / 2)
            let prev = i > 0 ? tops[i - 1].map { max($0 - Self.lineWidth / 2, Self.lineWidth / 2) } : nil
            path.move(to: NSPoint(x: x, y: prev ?? yy))
            path.line(to: NSPoint(x: x, y: yy))
            path.line(to: NSPoint(x: x + step, y: yy))
        }
        for (c, p) in paths {
            c.setStroke()
            p.stroke()
        }
    }

    private func drawPressure(_ s: Series, step: CGFloat, h: (Float) -> CGFloat, plot: NSRect) {
        var byLevel: [UInt8: [NSRect]] = [:]
        var tops: [CGFloat?] = Array(repeating: nil, count: s.width)
        for i in 0..<s.width where s.level[i] != Series.noData {
            let top = max(h(s.hi[i]), h(s.lo[i]), 1)
            byLevel[s.level[i], default: []].append(NSRect(x: CGFloat(i) * step, y: 0, width: step, height: top))
            tops[i] = top
        }
        for (lv, rects) in byLevel.sorted(by: { $0.key < $1.key }) {
            fill(rects, Theme.pressure(Pressure(raw: lv)), plot: plot)
        }
        line(tops, step: step) { Theme.pressure(Pressure(raw: s.level[$0])) }
    }

    private func drawGroups(_ s: Series, step: CGFloat, h: (Float) -> CGFloat, plot: NSRect) {
        var layers: [[NSRect]] = Array(repeating: [], count: 4)
        var tops: [[CGFloat?]] = Array(repeating: Array(repeating: nil, count: s.width), count: 4)
        for i in 0..<s.width where s.hasData[i] {
            let x = CGFloat(i) * step
            var y: CGFloat = 0
            for k in 0..<4 {
                let v = h(s.stack[4 * i + k])
                guard v > 0 else { continue }
                let top = min(y + v, plot.height)
                layers[k].append(NSRect(x: x, y: y, width: step, height: top - y))
                tops[k][i] = top
                y = top
            }
        }
        // Flat fills: a gradient per band would darken the lower ones.
        for k in 0..<4 {
            (k < 3 ? Theme.groups[k].withAlphaComponent(Self.groupAlpha) : Theme.groups[3].withAlphaComponent(Self.otherAlpha)).setFill()
            layers[k].forEach { $0.fill() }
        }
        // A line on each named group; "Other" stays a quiet band.
        for k in 0..<3 {
            line(tops[k], step: step) { _ in Theme.groups[k] }
        }
    }

    /// The line at the hovered column and its bubble, beside the line on
    /// the side with room, at the top of the graph.
    private func drawHover(_ info: Format.Hover, x: CGFloat, mark: (y: CGFloat, color: NSColor)?, plot: NSRect) {
        NSColor.labelColor.withAlphaComponent(0.6).setFill()
        NSRect(x: x.rounded(.down), y: 0, width: 1, height: plot.height).fill()
        if let mark {
            let r: CGFloat = 3
            let dotRect = NSRect(x: x.rounded(.down) + 0.5 - r, y: mark.y - r, width: 2 * r, height: 2 * r)
            NSColor.windowBackgroundColor.setFill()
            NSBezierPath(ovalIn: dotRect.insetBy(dx: -1.5, dy: -1.5)).fill()
            mark.color.setFill()
            NSBezierPath(ovalIn: dotRect).fill()
        }

        let font = NSFont.systemFont(ofSize: 10)
        let title: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: 10, weight: .semibold), .foregroundColor: Theme.quiet]
        let text: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.labelColor]
        let value: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .medium), .foregroundColor: NSColor.labelColor]
        let lineH: CGFloat = 12, pad: CGFloat = 4, dot: CGFloat = 6, gap: CGFloat = 8
        let hasDot = info.lines.contains { $0.swatch != nil }
        let labelW = info.lines.map { ($0.label as NSString).size(withAttributes: text).width }.max() ?? 0
        let valueW = info.lines.map { ($0.value as NSString).size(withAttributes: value).width }.max() ?? 0
        let totalW = info.total.map { ($0 as NSString).size(withAttributes: value).width } ?? 0
        let titleW = (info.title as NSString).size(withAttributes: title).width + (totalW > 0 ? gap + totalW : 0)
        let inner = max(titleW, (hasDot ? dot + 4 : 0) + labelW + (valueW > 0 ? gap + max(valueW, totalW) : 0))
        let size = NSSize(width: inner + 2 * pad, height: CGFloat(info.lines.count + 1) * lineH + 2 * pad)
        // Right of the line, or left when only the left has room; when
        // neither has, the roomier side, kept inside the graph.
        let right = plot.maxX - (x + 6), left = x - 6
        var origin = NSPoint(x: x + 6, y: plot.maxY - 3 - size.height)
        if size.width > right, size.width <= left || left > right { origin.x = x - 6 - size.width }
        origin.x = min(max(origin.x, 0), plot.maxX - size.width)
        origin.y = max(origin.y, 0)
        let box = NSRect(origin: origin, size: size)
        let path = NSBezierPath(roundedRect: box, xRadius: 5, yRadius: 5)
        NSColor.windowBackgroundColor.withAlphaComponent(0.95).setFill()
        path.fill()
        Theme.edge.setStroke()
        path.lineWidth = 0.5
        path.stroke()

        var y = box.maxY - pad - lineH
        (info.title as NSString).draw(at: NSPoint(x: box.minX + pad, y: y), withAttributes: title)
        if let total = info.total {
            (total as NSString).draw(at: NSPoint(x: box.maxX - pad - totalW, y: y), withAttributes: value)
        }
        for l in info.lines {
            y -= lineH
            var lx = box.minX + pad
            if hasDot {
                if let sw = l.swatch {
                    let c: NSColor
                    switch sw {
                    case .pressure(let p): c = Theme.pressure(p)
                    case .group(let k): c = Theme.groups[min(k, 3)]
                    }
                    c.setFill()
                    NSBezierPath(ovalIn: NSRect(x: lx, y: y + 3, width: dot, height: dot)).fill()
                }
                lx += dot + 4
            }
            (l.label as NSString).draw(at: NSPoint(x: lx, y: y), withAttributes: text)
            let vw = (l.value as NSString).size(withAttributes: value).width
            (l.value as NSString).draw(at: NSPoint(x: box.maxX - pad - vw, y: y), withAttributes: value)
        }
    }

    private func drawTicks(_ ticks: [Format.Tick], plot: NSRect) {
        Theme.grid.setFill()
        for t in ticks {
            NSRect(x: (plot.width * t.at).rounded(.down), y: 0, width: 1, height: plot.height).fill()
        }
    }

    /// The dashed line at the total, with its label under it on the left.
    private func drawTop(label: String, plot: NSRect) {
        let line = NSBezierPath()
        line.move(to: NSPoint(x: 0, y: plot.maxY - 0.5))
        line.line(to: NSPoint(x: plot.maxX, y: plot.maxY - 0.5))
        line.setLineDash([3, 3], count: 2, phase: 0)
        line.lineWidth = 1
        Theme.edge.setStroke()
        line.stroke()
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10), .foregroundColor: Theme.quiet,
        ]
        let text = label as NSString
        let ts = text.size(withAttributes: attrs)
        let at = NSPoint(x: 4, y: plot.maxY - 2 - ts.height)
        // A backing so the label stays readable when memory reaches the top.
        NSColor.windowBackgroundColor.withAlphaComponent(0.85).setFill()
        NSBezierPath(roundedRect: NSRect(origin: at, size: ts).insetBy(dx: -3, dy: 0), xRadius: 3, yRadius: 3).fill()
        text.draw(at: at, withAttributes: attrs)
    }
}
