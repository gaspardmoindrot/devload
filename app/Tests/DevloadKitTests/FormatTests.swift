import Foundation
import Testing
@testable import DevloadKit

private let gib: UInt64 = 1 << 30
private var paris: Calendar {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = TimeZone(identifier: "Europe/Paris")!
    return c
}
private func at(_ s: String) -> Date {
    let f = ISO8601DateFormatter()
    f.timeZone = TimeZone(identifier: "Europe/Paris")!
    return f.date(from: s)!
}

@Test func iconFigure() {
    #expect(Format.icon(used: gib * 195 / 10) == "19.5 GB")
    #expect(Format.icon(used: 0) == "0.0 GB")
    #expect(Format.iconWidest(total: 24 * gib) == "88.8 GB")
    #expect(Format.iconWidest(total: 128 * gib) == "888.8 GB")
}

@Test func header() {
    #expect(Format.usedOfTotal(used: gib * 1946 / 100 + 1, total: 24 * gib) == "19.46 / 24 GB")
    #expect(Format.usedOfTotal(used: gib, total: gib * 51 / 2) == "1.00 / 25.5 GB")
    #expect(Format.cpu(11) == "CPU 11 %")
    #expect(Format.pressureTitle(.tight) == "Memory tight")
}

@Test func footprintAndMore() {
    #expect(Format.footprint(gib * 240 / 100 + 1) == "2.40 GB")
    #expect(Format.footprint(gib * 88 / 100 + 1) == "0.88 GB")
    #expect(Format.more(More(count: 3, footprint: 1)) == "+3 more")
}

@Test func sinceLabel() {
    let now = at("2026-11-24T14:32:00+01:00") // a Tuesday
    #expect(Format.since(at("2026-11-24T10:45:00+01:00"), now: now, calendar: paris) == "since 10:45")
    #expect(Format.since(at("2026-11-23T22:05:00+01:00"), now: now, calendar: paris) == "since Mon 22:05")
    #expect(Format.since(at("2026-11-10T09:00:00+01:00"), now: now, calendar: paris) == "since Nov 10")
}

@Test func ticksPerPeriod() {
    let now = at("2026-11-24T14:32:00+01:00") // a Tuesday
    func labels(_ p: Period, _ span: TimeInterval) -> [String] {
        Format.ticks(from: now.addingTimeInterval(-span), to: now, period: p, calendar: paris).map(\.label)
    }
    // 14:30 is 3 % from the right edge: its label would be cut.
    #expect(labels(.h1, 3600) == ["13:45", "14:00", "14:15"])
    #expect(labels(.h6, 6 * 3600) == ["10:00", "12:00", "14:00"])
    #expect(labels(.h24, 86_400) == ["18:00", "Tue", "06:00", "12:00"], "midnight shows the day")
    #expect(labels(.d7, 7 * 86_400) == ["Wed", "Thu", "Fri", "Sat", "Sun", "Mon", "Tue"])
    let t = Format.ticks(from: now.addingTimeInterval(-3600), to: now, period: .h1, calendar: paris)
    #expect(abs(t[1].at - 28.0 / 60) < 1e-9, "14:00 is 28 min into the hour")
    #expect(t.allSatisfy { $0.at > 0.05 && $0.at < 0.95 })
    #expect(Format.ticks(from: now, to: now, period: .h1, calendar: paris).isEmpty)
}

@Test func collectingNote() {
    let now = at("2026-11-24T14:32:00+01:00")
    #expect(Format.collecting(since: at("2026-11-24T14:02:00+01:00"), now: now, calendar: paris) == "Collecting since 14:02")
    #expect(Format.collecting(since: at("2026-11-22T09:00:00+01:00"), now: now, calendar: paris) == "Collecting since Sun 09:00")
}

@Test func axisTop() {
    #expect(Format.axisTop(totalMiB: 24 * 1024) == "24 GB")
    #expect(Format.axisTop(totalMiB: 36 * 1024) == "36 GB")
}

@Test func alertText() {
    let row = Row(name: "feat/checkout-v2", detail: "", footprint: gib * 240 / 100 + 1)
    let a = Format.alert(pressure: .critical, used: gib * 2250 / 100 + 1, total: 24 * gib, biggest: row)
    #expect(a.title == "Memory critical")
    #expect(a.body == "22.50 / 24 GB used. Biggest: feat/checkout-v2 (2.40 GB).")
    #expect(Format.alert(pressure: .tight, used: gib, total: 24 * gib, biggest: nil).body == "1.00 / 24 GB used.")
}
