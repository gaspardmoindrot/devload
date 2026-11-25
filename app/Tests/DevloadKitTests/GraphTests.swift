import AppKit
import Testing
@testable import DevloadKit

private let total: Float = 24 * 1024
private let w = Int(GraphView.size.width)

private func pressureSeries() -> Series {
    var lo = [Float](repeating: 0, count: w), hi = lo
    var level = [UInt8](repeating: Series.noData, count: w)
    for i in 0..<100 { (lo[i], hi[i], level[i]) = (12 * 1024, 12 * 1024, 0) }
    for i in 200..<w { (lo[i], hi[i], level[i]) = (18 * 1024, 22 * 1024, 2) }
    return Series(period: .h1, mode: .pressure, from: .now, to: .now, totalMiB: total, lo: lo, hi: hi, level: level)
}

private func groupsSeries() -> Series {
    var stack = [Float](repeating: 0, count: 4 * w)
    var has = [Bool](repeating: false, count: w)
    for i in 100..<w {
        has[i] = true
        stack[4 * i..<4 * i + 4] = [6 * 1024, 3 * 1024, 2 * 1024, 4 * 1024]
    }
    return Series(period: .h1, mode: .groups, from: .now, to: .now, totalMiB: total, stack: stack, hasData: has, top: ["a", "b", "c"])
}

typealias Pixel = (r: Int, g: Int, b: Int, a: Int)

/// Pixel at x, at `f` of the height above the baseline (1 = 24 GB).
private func px(_ rep: NSBitmapImageRep, _ x: CGFloat, _ f: CGFloat) -> Pixel {
    rep.rgba(x: x, y: GraphView.size.height * (1 - f))
}

/// `color` as drawn in light or dark, sRGB 0–255.
private func rgb(_ color: NSColor, dark: Bool) -> (Int, Int, Int) {
    var out = (0, 0, 0)
    Render.appearance(dark).performAsCurrentDrawingAppearance {
        let c = color.usingColorSpace(.sRGB)!
        out = (Int(c.redComponent * 255 + 0.5), Int(c.greenComponent * 255 + 0.5), Int(c.blueComponent * 255 + 0.5))
    }
    return out
}

/// A fill pixel: `color`, translucent (the gradient), not empty.
private func filled(_ p: Pixel, _ color: NSColor, dark: Bool = false) -> Bool {
    let c = rgb(color, dark: dark)
    return p.a > 15 && p.a < 200 && abs(p.r - c.0) <= 14 && abs(p.g - c.1) <= 14 && abs(p.b - c.2) <= 14
}

/// A line pixel: `color`, opaque.
private func stroked(_ p: Pixel, _ color: NSColor, dark: Bool = false) -> Bool {
    let c = rgb(color, dark: dark)
    return p.a > 230 && abs(p.r - c.0) <= 8 && abs(p.g - c.1) <= 8 && abs(p.b - c.2) <= 8
}

@MainActor
@Test func pressureAreas() {
    let v = GraphView()
    v.series = pressureSeries()
    let rep = Render.bitmap(v, dark: false)
    #expect(filled(px(rep, 50, 0.25), Theme.good), "normal, under the line: \(px(rep, 50, 0.25))")
    #expect(px(rep, 50, 0.45).a > px(rep, 50, 0.05).a, "the fill is stronger near the top")
    #expect(stroked(px(rep, 50, 0.49), Theme.good), "the line at 12 GB: \(px(rep, 50, 0.49))")
    #expect(px(rep, 50, 0.70).a == 0, "above the line: empty")
    #expect(px(rep, 150, 0.25).a == 0, "no data: nothing drawn")
    #expect(filled(px(rep, 250, 0.40), Theme.bad), "critical: \(px(rep, 250, 0.40))")
    #expect(filled(px(rep, 250, 0.85), Theme.bad), "up to the peak, not the minimum: \(px(rep, 250, 0.85))")
    #expect(stroked(px(rep, 250, 0.905), Theme.bad), "the line at the 22 GB peak: \(px(rep, 250, 0.905))")
    #expect(px(rep, 250, 0.95).a == 0, "above the peak")
}

@MainActor
@Test func pressureAreasDark() {
    let v = GraphView()
    v.series = pressureSeries()
    let rep = Render.bitmap(v, dark: true)
    #expect(filled(px(rep, 250, 0.40), Theme.bad, dark: true), "\(px(rep, 250, 0.40))")
    #expect(stroked(px(rep, 250, 0.905), Theme.bad, dark: true), "\(px(rep, 250, 0.905))")
}

@MainActor
@Test func groupsStackInOrder() {
    let v = GraphView()
    v.series = groupsSeries()
    let rep = Render.bitmap(v, dark: false)
    // 6 + 3 + 2 + 4 GB of 24: 25 %, 12.5 %, 8.3 %, 16.7 % of the height.
    #expect(filled(px(rep, 200, 0.10), Theme.groups[0]), "top 1: \(px(rep, 200, 0.10))")
    #expect(filled(px(rep, 200, 0.31), Theme.groups[1]), "top 2: \(px(rep, 200, 0.31))")
    #expect(filled(px(rep, 200, 0.41), Theme.groups[2]), "top 3: \(px(rep, 200, 0.41))")
    #expect(filled(px(rep, 200, 0.55), Theme.groups[3]), "other: \(px(rep, 200, 0.55))")
    #expect(stroked(px(rep, 200, 0.245), Theme.groups[0]), "a line on top 1: \(px(rep, 200, 0.245))")
    #expect(px(rep, 200, 0.70).a == 0, "above the stack")
    #expect(px(rep, 50, 0.10).a == 0, "no data")
}

@MainActor
@Test func ticksDrawHairlines() {
    let now = Date()
    let pressure = pressureSeries()
    let v = GraphView()
    v.series = Series(period: .d7, mode: .pressure, from: now.addingTimeInterval(-7 * 86_400), to: now,
                      totalMiB: total, lo: pressure.lo, hi: pressure.hi, level: pressure.level)
    let rep = Render.bitmap(v, dark: false)
    // Columns 100–199 are empty: only the midnight lines draw there.
    let ticks = Format.ticks(from: v.series!.from, to: now, period: .d7)
    let inGap = ticks.map { GraphView.size.width * $0.at }.filter { $0 > 101 && $0 < 198 }
    #expect(!inGap.isEmpty)
    for x in inGap {
        #expect(px(rep, x.rounded(.down) + 0.5, 0.5).a > 200, "a line at \(x)")
    }
    #expect(px(rep, inGap[0].rounded(.down) + 4, 0.5).a == 0, "and nothing next to it")
}

@MainActor
@Test func overflowIsClipped() {
    var stack = [Float](repeating: 0, count: 4 * w)
    for i in 0..<w { stack[4 * i] = 40 * 1024 }
    let v = GraphView()
    v.series = Series(period: .h1, mode: .groups, from: .now, to: .now, totalMiB: total, stack: stack,
                      hasData: .init(repeating: true, count: w), top: ["a"])
    let rep = Render.bitmap(v, dark: false)
    #expect(px(rep, 200, 0.98).a > 0, "full height")
}

@MainActor
@Test func redrawsOnlyOnChange() {
    let v = GraphView()
    var draws = 0
    v.onDraw = { draws += 1 }
    let s = pressureSeries()
    v.series = s
    _ = Render.bitmap(v, dark: false)
    v.series = s
    #expect(!v.needsDisplay, "same data: no redraw")
    #expect(draws == 1)
}

@Test func hoverPicksTheColumnUnderThePointer() {
    #expect(GraphView.column(at: 0, width: 308, columns: 308) == 0)
    #expect(GraphView.column(at: 307.9, width: 308, columns: 308) == 307)
    #expect(GraphView.column(at: 154, width: 308, columns: 77) == 38)
    #expect(GraphView.column(at: -1, width: 308, columns: 308) == nil)
    #expect(GraphView.column(at: 308, width: 308, columns: 308) == nil)
}

@Test func hoverSaysWhenAndHowMuch() {
    var cal = Calendar(identifier: .gregorian)
    cal.timeZone = TimeZone(identifier: "UTC")!
    let to = Date(timeIntervalSince1970: 1_700_000_000) // Tue 22:13:20 UTC
    let from = to.addingTimeInterval(-3600)
    let p = pressureSeries()
    let s = Series(period: .h1, mode: .pressure, from: from, to: to, totalMiB: total, lo: p.lo, hi: p.hi, level: p.level)
    let h = Format.hover(s, column: 250, calendar: cal)!
    #expect(h.lines == [.init(swatch: .pressure(.critical), label: "Critical", value: "22.00 GB")])
    #expect(h.title.count == 5 && h.title.hasPrefix("22:"))
    #expect(Format.hover(s, column: 150, calendar: cal)!.lines.first?.label == "No data")
    #expect(Format.hover(s, column: w, calendar: cal) == nil)

    let g = groupsSeries()
    let gs = Series(period: .d7, mode: .groups, from: to.addingTimeInterval(-7 * 86_400), to: to, totalMiB: total,
                    stack: g.stack, hasData: g.hasData, top: g.top)
    let gh = Format.hover(gs, column: 120, calendar: cal)!
    #expect(gh.lines.map(\.label) == ["a", "b", "c", "Other"])
    #expect(gh.total == "15.00 GB")
    #expect(h.total == nil)
    #expect(gh.title.split(separator: " ").count == 2, "an earlier day shows its name: \(gh.title)")
}

@MainActor
@Test func hoverDrawsALineAndABubble() {
    let v = GraphView()
    v.series = pressureSeries()
    let plain = Render.bitmap(v, dark: false)
    v.hoverX = 50
    let hovered = Render.bitmap(v, dark: false)
    // The bubble sits right of the line, at the top of the graph.
    let a = plain.rgba(x: 70, y: 10), b = hovered.rgba(x: 70, y: 10)
    #expect(a.r != b.r || a.g != b.g || a.b != b.b, "bubble drawn")
    v.hoverX = nil
    let back = Render.bitmap(v, dark: false)
    let c = back.rgba(x: 70, y: 10)
    #expect(a.r == c.r && a.g == c.g && a.b == c.b, "gone when the pointer leaves")
}
