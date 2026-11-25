import AppKit
import Testing
@testable import DevloadKit

/// Empty defaults that leave no plist behind.
@MainActor
private func fresh() -> UserDefaults { MemoryDefaults() }

@MainActor
@Test func startsOnOneHourPressure() {
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
    #expect(m.period == .h1 && m.mode == .pressure)
    #expect(m.snapshot == nil && m.series == nil, "nothing read before refresh")
    m.refresh()
    #expect(m.snapshot?.dev.count == 5)
    #expect(m.series?.mode == .pressure && m.series?.width == Int(GraphView.size.width))
    #expect(m.legend.map(\.name) == ["Normal", "Tight", "Critical"])
    #expect((3...4).contains(m.ticks.count), "one mark per quarter hour: \(m.ticks)")
    #expect(m.collecting == nil, "sample history covers the hour")
}

@MainActor
@Test func lastChoiceIsKept() {
    let d = fresh()
    let m = PopoverModel(source: SampleCore(), defaults: d, loginItem: SampleLoginItem())
    m.refresh()
    m.select(.groups)
    m.select(.d7)
    #expect(m.series?.mode == .groups && m.series?.period == .d7)
    #expect(m.legend.map(\.name) == ["feat/checkout-v2", "Google Chrome", "Docker Desktop", "Other"])
    // The next popover (a new model) opens where the last one was left.
    let next = PopoverModel(source: SampleCore(), defaults: d, loginItem: SampleLoginItem())
    #expect(next.period == .d7 && next.mode == .groups)
}

@MainActor
@Test func garbageInDefaultsFallsBack() {
    let d = fresh()
    d.set(42, forKey: PopoverModel.periodKey)
    d.set(-1, forKey: PopoverModel.modeKey)
    let m = PopoverModel(source: SampleCore(), defaults: d, loginItem: SampleLoginItem())
    #expect(m.period == .h1 && m.mode == .pressure)
}

@MainActor
@Test func analyzeCopiesThePromptOfThePeriodShown() async throws {
    let core = SampleCore()
    let pb = NSPasteboard.withUniqueName()
    defer { pb.releaseGlobally() }
    let m = PopoverModel(source: core, defaults: fresh(), loginItem: SampleLoginItem(), pasteboard: pb,
                         copiedFor: .milliseconds(150))
    m.select(.h24)
    m.analyze()
    #expect(core.prompts == [.h24])
    #expect(pb.string(forType: .string) == "Sample prompt for 24h")
    #expect(m.copied)
    try await Task.sleep(for: .milliseconds(80))
    // A second click restarts the delay.
    m.analyze()
    try await Task.sleep(for: .milliseconds(100))
    #expect(m.copied, "still within 150 ms of the second click")
    try await Task.sleep(for: .milliseconds(150))
    #expect(!m.copied, "label back to Analyze")
}

@MainActor
@Test func launchAtLogin() {
    let item = SampleLoginItem()
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: item)
    #expect(!m.login.isOn && m.login.note == nil)
    item.next = .requiresApproval
    m.setLaunchAtLogin(true)
    #expect(m.login.isOn)
    #expect(m.login.note?.contains("System Settings") == true)
    item.next = nil
    m.setLaunchAtLogin(false)
    #expect(m.login == .disabled)
    #expect(LoginStatus.notFound.note?.contains("/Applications") == true)
}

@MainActor
@Test func quitCallsBack() {
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
    var quit = false
    m.onQuit = { quit = true }
    m.quit()
    #expect(quit)
}

@MainActor
@Test func popoverRendersInBothThemes() throws {
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    let v = Snapshots.popoverView(model: m)
    #expect(v.frame.width == PopoverView.width)
    #expect(v.frame.height > 380 && v.frame.height < 520, "compact: \(v.frame.height)")
    let light = Render.bitmap(v, dark: false), dark = Render.bitmap(v, dark: true)
    // The window background differs between the two.
    let l = light.rgba(x: 5, y: 5), d = dark.rgba(x: 5, y: 5)
    #expect(l.r > 200 && d.r < 60, "\(l) \(d)")
}

@MainActor
@Test func popoverWithoutDataYet() {
    final class Empty: CoreSource {
        func snapshot() -> Snapshot? { nil }
        func series(period: Period, mode: GraphMode, width: Int) -> Series? { nil }
        func prompt(period: Period) -> String? { nil }
        func setPopoverOpen(_ open: Bool) {}
    }
    let m = PopoverModel(source: Empty(), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    m.analyze()
    #expect(!m.copied, "nothing to copy")
    let v = Snapshots.popoverView(model: m)
    #expect(v.frame.height > 150)
    _ = Render.bitmap(v, dark: false)
}

@MainActor
@Test func listsFoldToThreeRows() {
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    let dev = m.list(.dev)
    #expect(dev.rows.map(\.name) == ["feat/checkout-v2", "api-refactor", "landing-page"])
    #expect(dev.foldable && !dev.expanded)
    #expect(dev.more.count == 2)
    #expect(dev.more.footprint == SampleCore.gb(0.421) + SampleCore.gb(0.311))
    let apps = m.list(.apps)
    #expect(apps.rows.count == 3 && apps.more.count == 15)
    let rest = m.snapshot!.apps.dropFirst(3).reduce(UInt64(0)) { $0 + $1.footprint }
    #expect(apps.more.footprint == rest)
}

@MainActor
@Test func moreUnfoldsOneListAtATime() {
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    m.toggle(.apps)
    let apps = m.list(.apps)
    #expect(apps.expanded && apps.rows.count == 18 && apps.more == .none)
    #expect(!m.list(.dev).expanded, "the other list stays folded")
    m.refresh()
    #expect(m.list(.apps).expanded, "a tick keeps it unfolded")
    m.toggle(.apps)
    #expect(m.list(.apps).rows.count == 3)
    // A new popover opens folded.
    m.toggle(.apps)
    #expect(!PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem()).expanded.contains(.apps))
}

@MainActor
@Test func shortListsDoNotFold() {
    final class Few: CoreSource {
        let base = SampleCore()
        func snapshot() -> Snapshot? {
            let s = base.snapshot()!
            return Snapshot(pressure: s.pressure, cpuPercent: s.cpuPercent, time: s.time, since: s.since, used: s.used,
                            total: s.total, dev: Array(s.dev.prefix(4)), devMore: .none,
                            apps: Array(s.apps.prefix(2)), appsMore: More(count: 3, footprint: 7))
        }
        func series(period: Period, mode: GraphMode, width: Int) -> Series? { nil }
        func prompt(period: Period) -> String? { nil }
        func setPopoverOpen(_ open: Bool) {}
    }
    let m = PopoverModel(source: Few(), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    #expect(m.list(.dev).rows.count == 4 && !m.list(.dev).foldable, "+1 more would take a line anyway")
    let apps = m.list(.apps)
    #expect(apps.rows.count == 2 && !apps.foldable && apps.more == More(count: 3, footprint: 7), "beyond the core's cap")
}

@MainActor
@Test func unfoldedListScrollsInsteadOfGrowing() {
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    let folded = Snapshots.popoverView(model: m).frame.height
    m.toggle(.apps)
    let unfolded = Snapshots.popoverView(model: m).frame.height
    let grow = unfolded - folded
    // 3 rows become a scroll area of 7.5 (the cut row says it scrolls), and "+15 more" becomes "Show less".
    #expect(abs(grow - 4.5 * PopoverView.rowHeight - 4) <= 1, "grew by 4.5 rows and the 4 pt gap: \(grow)")
    _ = Render.bitmap(Snapshots.popoverView(model: m), dark: true)
}

@MainActor
@Test func unfoldedListScrollsToItsLastRow() throws {
    let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    m.toggle(.apps)
    let v = Snapshots.popoverView(model: m)
    // The scroll area lays out its rows only inside a window (never shown).
    let window = NSWindow(contentRect: v.frame, styleMask: .borderless, backing: .buffered, defer: true)
    window.contentView = v
    v.layoutSubtreeIfNeeded()
    RunLoop.current.run(until: Date().addingTimeInterval(0.05))
    func scrollViews(_ v: NSView) -> [NSScrollView] {
        (v as? NSScrollView).map { [$0] } ?? v.subviews.flatMap(scrollViews)
    }
    let found = scrollViews(v)
    try #require(found.count == 1, "one scroll area, for the unfolded list")
    let scroll = found[0]
    let visible = scroll.contentView.bounds.height
    let content = try #require(scroll.documentView).frame.height
    #expect(abs(visible - 7.5 * PopoverView.rowHeight) <= 1, "7.5 rows visible: \(visible)")
    #expect(abs(content - 18 * PopoverView.rowHeight) <= 1, "18 rows inside: \(content)")
    // Scrolled to the end, the visible window ends on the last row.
    let bottom = NSPoint(x: 0, y: scroll.documentView!.isFlipped ? content - visible : 0)
    scroll.contentView.scroll(to: bottom)
    scroll.reflectScrolledClipView(scroll.contentView)
    let shown = scroll.documentVisibleRect
    #expect(scroll.documentView!.isFlipped ? shown.maxY >= content - 1 : shown.minY <= 1, "\(shown) of \(content)")
    let rep = Render.bitmap(v, dark: false)
    if let dir = ProcessInfo.processInfo.environment["DEVLOAD_TEST_OUT"] {
        try Render.png(rep, to: URL(fileURLWithPath: dir).appendingPathComponent("scrolled.png"))
    }
}

/// The sample history with its first `empty` columns blanked out.
@MainActor
private final class Late: CoreSource {
    let base = SampleCore()
    let empty: Int
    init(empty: Int) { self.empty = empty }
    func snapshot() -> Snapshot? { base.snapshot() }
    func series(period: Period, mode: GraphMode, width: Int) -> Series? {
        guard let s = base.series(period: period, mode: mode, width: width) else { return nil }
        var level = s.level, has = s.hasData
        for i in 0..<min(empty, width) {
            if !level.isEmpty { level[i] = Series.noData }
            if !has.isEmpty { has[i] = false }
        }
        return Series(period: period, mode: mode, from: s.from, to: s.to, totalMiB: s.totalMiB, lo: s.lo, hi: s.hi,
                      level: level, stack: s.stack, hasData: has, top: s.top)
    }
    func prompt(period: Period) -> String? { nil }
    func setPopoverOpen(_ open: Bool) {}
}

@MainActor
@Test func sparseHistorySaysSinceWhen() throws {
    let w = Int(GraphView.size.width)
    let m = PopoverModel(source: Late(empty: w * 3 / 4), defaults: fresh(), loginItem: SampleLoginItem())
    m.refresh()
    let c = try #require(m.collecting, "three quarters empty")
    #expect(abs(c.width - 0.75) < 0.01)
    #expect(c.note.hasPrefix("Collecting since "), "\(c.note)")
    m.select(.groups)
    #expect(m.collecting != nil, "same in groups mode")
    let small = PopoverModel(source: Late(empty: w / 10), defaults: fresh(), loginItem: SampleLoginItem())
    small.refresh()
    #expect(small.collecting == nil, "a sliver is not worth a note")
    let none = PopoverModel(source: Late(empty: w), defaults: fresh(), loginItem: SampleLoginItem())
    none.refresh()
    #expect(none.collecting?.note == "Collecting…" && none.collecting?.width == 1)
}

@MainActor
@Test func graphFooterKeepsItsHeight() {
    // Long tick labels or long group names never push the lists down.
    var heights: Set<CGFloat> = []
    for mode in GraphMode.allCases {
        for period in Period.allCases {
            let m = PopoverModel(source: SampleCore(), defaults: fresh(), loginItem: SampleLoginItem())
            m.select(mode)
            m.select(period)
            m.refresh()
            heights.insert(Snapshots.popoverView(model: m).frame.height)
        }
    }
    #expect(heights.count == 1, "\(heights)")
}

/// A history that began at `start`: every period's columns before it are empty.
@MainActor
private final class StartedAt: CoreSource {
    let base = SampleCore()
    let start: Date
    init(start: Date) { self.start = start }
    func snapshot() -> Snapshot? { base.snapshot() }
    func series(period: Period, mode: GraphMode, width: Int) -> Series? {
        guard let s = base.series(period: period, mode: mode, width: width) else { return nil }
        let span = s.to.timeIntervalSince(s.from)
        let empty = max(0, Int((start.timeIntervalSince(s.from) / span * Double(width)).rounded(.down)))
        var level = s.level, has = s.hasData
        for i in 0..<min(empty, width) {
            if !level.isEmpty { level[i] = Series.noData }
            if !has.isEmpty { has[i] = false }
        }
        return Series(period: period, mode: mode, from: s.from, to: s.to, totalMiB: s.totalMiB, lo: s.lo, hi: s.hi,
                      level: level, stack: s.stack, hasData: has, top: s.top)
    }
    func prompt(period: Period) -> String? { nil }
    func setPopoverOpen(_ open: Bool) {}
}

@MainActor
@Test func collectingTimeIsTheSameOnEveryPeriod() throws {
    let start = SampleCore().now.addingTimeInterval(-30 * 60)
    var notes: [String] = []
    for p in Period.allCases {
        let m = PopoverModel(source: StartedAt(start: start), defaults: fresh(), loginItem: SampleLoginItem())
        m.select(p)
        m.refresh()
        if let c = m.collecting { notes.append(c.note) }
    }
    // Every period says when it began to the minute, though a 7 d column
    // spans 33 minutes.
    #expect(notes.count == 4, "\(notes)")
    #expect(Set(notes).count == 1, "\(notes)")
    let want = Format.collecting(since: start, now: SampleCore().now)
    let alt = Format.collecting(since: start.addingTimeInterval(60), now: SampleCore().now)
    #expect(notes.first == want || notes.first == alt, "\(notes) vs \(want)")
}

@MainActor
@Test func aLegendShowsAtMostThreeNames() {
    final class Many: CoreSource {
        let base = SampleCore()
        func snapshot() -> Snapshot? { base.snapshot() }
        func series(period: Period, mode: GraphMode, width: Int) -> Series? {
            Series(period: period, mode: mode, from: .now, to: .now, totalMiB: 1, top: ["a", "b", "c", "d", "e"])
        }
        func prompt(period: Period) -> String? { nil }
        func setPopoverOpen(_ open: Bool) {}
    }
    let m = PopoverModel(source: Many(), defaults: fresh(), loginItem: SampleLoginItem())
    m.select(.groups)
    m.refresh()
    #expect(m.legend.map(\.name) == ["a", "b", "c", "Other"])
}

@MainActor
@Test func appIconsComeFromTheRowsBundle() {
    let icons = AppIcons()
    let calc = Row(id: 1, kind: .app, name: "Not A Name", detail: "", footprint: 1,
                   path: "/System/Applications/Calculator.app")
    let icon = icons.icon(for: calc)
    #expect(icon != nil, "found by path, whatever the name")
    #expect(icon?.size == NSSize(width: 16, height: 16))
    #expect((icon?.representations.first as? NSBitmapImageRep)?.pixelsWide == 64, "drawn at 4x")
    #expect(icons.icon(for: calc) === icon, "cached")
    #expect(icons.icon(for: Row(id: 2, kind: .app, name: "No Such App Here", detail: "", footprint: 1)) == nil)
}

@MainActor
@Test func theGraphSaysWhatItShows() {
    let lv: [UInt8] = [Series.noData, 0, 2, 1, Series.noData]
    let p = Series(period: .h6, mode: .pressure, from: .now, to: .now, totalMiB: 1, lo: [0, 0, 0, 0, 0],
                   hi: [0, 0, 0, 0, 0], level: lv)
    #expect(Format.graphLabel(p) == "Memory pressure, last 6h: Tight now, Critical at worst.")
    let empty = Series(period: .h1, mode: .pressure, from: .now, to: .now, totalMiB: 1, level: [Series.noData])
    #expect(Format.graphLabel(empty) == "Memory pressure, last 1h: no data yet.")
    let g = Series(period: .d7, mode: .groups, from: .now, to: .now, totalMiB: 1, hasData: [true], top: ["web", "Slack"])
    #expect(Format.graphLabel(g) == "Memory by group, last 7d: web, Slack, Other.")
    let v = GraphView()
    v.series = p
    #expect(v.accessibilityLabel() == Format.graphLabel(p))
}

@MainActor
@Test func memoryDefaultsLeaveNoPlist() {
    let d = MemoryDefaults()
    d.set(3, forKey: PopoverModel.periodKey)
    d.set("x", forKey: "other")
    #expect(d.integer(forKey: PopoverModel.periodKey) == 3)
    #expect(d.string(forKey: "other") == "x")
    let plist = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Preferences/devload.memory.plist")
    #expect(!FileManager.default.fileExists(atPath: plist.path))
    #expect(MemoryDefaults().integer(forKey: PopoverModel.periodKey) == 0, "each one starts empty")
}
