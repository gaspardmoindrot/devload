import AppKit
import Testing
@testable import DevloadKit

private let gib: UInt64 = 1 << 30

@Test func iconKeepsOneWidth() {
    let sizes = [0, 5, 9.5, 19.5, 23.9].map {
        IconRenderer.image(pressure: .normal, used: UInt64($0 * Double(gib)), total: 24 * gib).size
    }
    #expect(Set(sizes.map(\.width)).count == 1)
    #expect(sizes[0].height == 18)
    // A 128 GB Mac needs room for one more digit.
    #expect(IconRenderer.size(total: 128 * gib).width > IconRenderer.size(total: 24 * gib).width)
}

/// Finds the dot: the leftmost opaque pixel run on the middle row, and its colour.
private func dot(_ rep: NSBitmapImageRep) -> (x: CGFloat, rgb: (Int, Int, Int))? {
    let y = rep.size.height / 2
    var x: CGFloat = 0
    while x < rep.size.width {
        let p = rep.rgba(x: x, y: y)
        if p.a > 250 {
            let c = rep.rgba(x: x + 4, y: y) // the dot's centre
            return (x, (c.r, c.g, c.b))
        }
        x += 0.5
    }
    return nil
}

@Test func iconDotColourFollowsPressure() throws {
    // The system colours, as drawn in each appearance (they differ).
    func want(_ p: Pressure, _ dark: Bool) -> (Int, Int, Int) {
        var out = (0, 0, 0)
        Render.appearance(dark).performAsCurrentDrawingAppearance {
            let c = Theme.pressure(p).usingColorSpace(.sRGB)!
            out = (Int(c.redComponent * 255 + 0.5), Int(c.greenComponent * 255 + 0.5), Int(c.blueComponent * 255 + 0.5))
        }
        return out
    }
    let cases = [Pressure.normal, .tight, .critical].flatMap { p in [false, true].map { (p, $0) } }
    #expect(want(.critical, false) != want(.critical, true), "dark has its own red")
    for (p, dark) in cases {
        let want = want(p, dark)
        let rep = Render.bitmap(IconRenderer.image(pressure: p, used: 19 * gib, total: 24 * gib), dark: dark)
        let d = try #require(dot(rep))
        #expect(abs(d.rgb.0 - want.0) <= 3 && abs(d.rgb.1 - want.1) <= 3 && abs(d.rgb.2 - want.2) <= 3, "\(p) dark=\(dark): \(d.rgb)")
    }
}

@Test func iconTextFollowsAppearance() {
    let img = IconRenderer.image(pressure: .normal, used: 19 * gib, total: 24 * gib)
    // The darkest / lightest pixel of the figure area.
    func extreme(_ dark: Bool) -> Int {
        let rep = Render.bitmap(img, dark: dark)
        var lum: [Int] = []
        var x = rep.size.width - 30
        while x < rep.size.width {
            var y: CGFloat = 4
            while y < 14 {
                let p = rep.rgba(x: x, y: y)
                if p.a > 200 { lum.append((p.r + p.g + p.b) / 3) }
                y += 0.5
            }
            x += 0.5
        }
        return dark ? (lum.max() ?? 0) : (lum.min() ?? 255)
    }
    #expect(extreme(false) < 60, "dark text on a light menu bar")
    #expect(extreme(true) > 200, "light text on a dark menu bar")
}

@Test func iconDotHugsTheFigure() throws {
    let short = Render.bitmap(IconRenderer.image(pressure: .normal, used: 5 * gib, total: 24 * gib), dark: false)
    let long = Render.bitmap(IconRenderer.image(pressure: .normal, used: 19 * gib, total: 24 * gib), dark: false)
    // One digit fewer: the dot moves right by about one digit width.
    let dx = try #require(dot(short)).x - #require(dot(long)).x
    #expect(dx > 5 && dx < 10, "\(dx)")
}
