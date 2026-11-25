import CoreGraphics
import Testing
@testable import DevloadKit

/// 12 rows of 24 pt, the open row (id 3) 64 pt taller.
private let rows: [(id: UInt32, top: CGFloat, bottom: CGFloat)] = {
    var y: CGFloat = 0
    return (1...12).map { i in
        let h: CGFloat = i == 3 ? 88 : 24
        defer { y += h }
        return (UInt32(i), y, y + h)
    }
}()

@Test func atTheTopNothingIsHiddenAbove() {
    let h = ScrollHint.of(rows: rows, visible: 0...180)
    #expect(h.above == 0)
    // Rows 1–2 (48) + row 3 (88) = 136; row 4 ends at 160, row 5 is cut at 180.
    #expect(h.below == 8)
}

@Test func theHalfRowCountsAsHidden() {
    let flat = (0..<10).map { (id: UInt32($0), top: CGFloat($0) * 24, bottom: CGFloat($0 + 1) * 24) }
    let h = ScrollHint.of(rows: flat, visible: 0...180)
    #expect(h.below == 3) // 7 full rows, the 8th cut in half, then 2 more
}

@Test func inTheMiddleBothSidesAreHidden() {
    let flat = (0..<20).map { (id: UInt32($0), top: CGFloat($0) * 24, bottom: CGFloat($0 + 1) * 24) }
    let h = ScrollHint.of(rows: flat, visible: 100...280)
    #expect(h.above == 5) // rows 0–4: row 4 spans 96–120, cut at the top
    #expect(h.below == 9) // row 11 spans 264–288, cut at the bottom
}

@Test func atTheEndNothingIsHiddenBelow() {
    let flat = (0..<10).map { (id: UInt32($0), top: CGFloat($0) * 24, bottom: CGFloat($0 + 1) * 24) }
    let h = ScrollHint.of(rows: flat, visible: 60...240)
    #expect(h.below == 0)
    #expect(h.above == 3)
}

@Test func subpixelEdgesDoNotCount() {
    let flat = [(id: UInt32(1), top: CGFloat(0), bottom: CGFloat(24.3))]
    #expect(ScrollHint.of(rows: flat, visible: 0...24) == ScrollHint())
}

