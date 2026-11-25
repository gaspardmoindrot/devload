import AppKit
import Testing
@testable import DevloadKit

@MainActor
private func model(_ core: SampleCore = SampleCore(), apps: AppQuitting = SampleAppQuitter(),
                   checkAfter: Duration = .milliseconds(50), own: String = "/Applications/devload.app") -> PopoverModel {
    let m = PopoverModel(source: core, defaults: MemoryDefaults(), loginItem: SampleLoginItem(),
                         quits: QuitState(apps: apps, checkAfter: checkAfter), ownBundle: own)
    m.refresh()
    return m
}

/// Waits for `done`, up to 2 s.
@MainActor
private func until(_ done: () -> Bool) async throws {
    for _ in 0..<200 where !done() { try await Task.sleep(for: .milliseconds(10)) }
}

@MainActor
@Test func aClickUnfoldsOneRowAtATime() {
    let m = model()
    #expect(m.open == nil && m.parts == nil)
    m.toggleRow(1)
    #expect(m.open == 1)
    #expect(m.parts?.parts.map(\.label) == ["Claude sessions", "node", "zsh"])
    m.toggleRow(2)
    #expect(m.open == 2, "another row folds the first")
    #expect(m.parts?.containers == 2)
    m.toggleRow(2)
    #expect(m.open == nil && m.parts == nil)
}

@MainActor
@Test func partsAddUpToTheRow() {
    let core = SampleCore()
    for r in core.snapshot()!.dev + core.snapshot()!.apps {
        let p = core.parts(row: r.id)!
        let sum = p.parts.reduce(p.more.footprint) { $0 + $1.footprint }
        // Within 1 MB: the sample figures are rounded.
        #expect(abs(Double(sum) - Double(r.footprint)) < 1_100_000, "\(r.name)")
    }
}

@MainActor
@Test func partLines() {
    #expect(Format.part(Part(kind: .process, label: "node", count: 4, footprint: 0)) == "node ×4")
    #expect(Format.part(Part(kind: .process, label: "zsh", footprint: 0)) == "zsh")
    #expect(Format.part(Part(kind: .agent, label: "Claude sessions", count: 3, idle: 2, footprint: 0))
        == "3 Claude sessions, 2 idle")
    #expect(Format.part(Part(kind: .agent, label: "Claude sessions", count: 3, idle: 3, footprint: 0))
        == "3 Claude sessions, all idle")
    #expect(Format.part(Part(kind: .agent, label: "Codex session", count: 1, idle: 1, footprint: 0))
        == "Codex session, idle")
    #expect(Format.part(Part(kind: .agent, label: "Codex session", count: 1, footprint: 0)) == "Codex session")
    let stack = Part(kind: .docker, label: "api-refactor", detail: "postgres, redis", count: 2, footprint: 0)
    #expect(Format.part(stack) == "api-refactor")
    #expect(Format.partDetail(stack) == "postgres, redis")
    #expect(Format.partDetail(Part(kind: .docker, label: "x", count: 1, footprint: 0)) == "1 container")
}

@MainActor
@Test func quitQuestions() {
    #expect(Format.quitQuestion(name: "web", processes: 9, containers: 0) == "Quit 9 processes in web?")
    #expect(Format.quitQuestion(name: "web", processes: 1, containers: 2) == "Quit 1 process and 2 containers in web?")
    #expect(Format.quitQuestion(name: "web", processes: 0, containers: 1) == "Quit 1 container in web?")
    let m = model()
    #expect(m.question(2) == "Quit 1 process and 2 containers in api-refactor?", "names the row it was asked on")
    #expect(m.question(13) == "Quit Slack?", "an app is quit as a whole")
}

@MainActor
@Test func quitResults() {
    #expect(Format.quitResult(quit: 9, denied: 0, left: 0) == nil)
    #expect(Format.quitResult(quit: 7, denied: 0, left: 2) == "2 still running")
    #expect(Format.quitResult(quit: 7, denied: 1, left: 1) == "1 still running, 1 not yours to quit")
    #expect(Format.quitResult(quit: 0, denied: 0, left: 0) == "Nothing left to quit")
}

@MainActor
@Test func theCrossAsksBeforeQuitting() {
    let core = SampleCore()
    let m = model(core)
    m.askQuit(1)
    #expect(m.open == 1, "the cross unfolds the row")
    #expect(m.confirming == 1)
    #expect(core.quits.isEmpty, "nothing quit before the answer")
    m.cancelQuit()
    #expect(m.confirming == nil && m.open == 1)
    #expect(core.quits.isEmpty)
    m.askQuit(1)
    m.confirmQuit()
    #expect(core.quits == [1])
    #expect(m.quitting == [1] && m.confirming == nil)
    m.askQuit(1)
    #expect(m.confirming == nil, "no second quit while one runs")
}

@MainActor
@Test func foldingARowDropsItsQuestion() {
    let m = model()
    m.askQuit(1)
    m.toggleRow(1)
    #expect(m.confirming == nil && m.open == nil)
    m.askQuit(1)
    m.toggleRow(2)
    #expect(m.confirming == nil, "the question goes with the row it was asked on")
}

@MainActor
@Test func aCleanQuitStaysGreyUntilTheRowIsGone() {
    let core = SampleCore()
    let m = model(core)
    m.askQuit(4)
    m.confirmQuit()
    m.quitEnded(row: 4, quit: 3, denied: 0, left: 0)
    #expect(m.quitting == [4], "greyed until the next snapshot")
    #expect(m.notes[4] == nil)
    core.gone = [4]
    core.elapsed = 5
    m.refresh()
    #expect(m.quitting.isEmpty)
    #expect(m.open == nil, "a row gone from the snapshot folds")
    #expect(m.list(.dev).rows.map(\.id) == [1, 2, 3, 5])
}

@MainActor
@Test func aRowThatComesBackIsNoLongerGrey() {
    let core = SampleCore()
    let m = model(core)
    m.askQuit(4)
    m.confirmQuit()
    m.quitEnded(row: 4, quit: 3, denied: 0, left: 0)
    m.refresh()
    #expect(m.quitting == [4], "same snapshot: still settling")
    core.elapsed = 5
    m.refresh()
    #expect(m.quitting.isEmpty, "a newer snapshot still has it (a supervisor restarted it)")
}

@MainActor
@Test func whatAQuitLeftIsSaid() {
    let m = model()
    m.askQuit(1)
    m.confirmQuit()
    m.quitEnded(row: 1, quit: 7, denied: 1, left: 1)
    #expect(m.quitting.isEmpty)
    #expect(m.notes[1] == "1 still running, 1 not yours to quit")
    m.askQuit(1)
    #expect(m.notes[1] == nil, "asking again clears the note")
}

@MainActor
@Test func aRowGoneBeforeTheAnswerHasNothingToQuit() {
    let core = SampleCore()
    let m = model(core)
    m.askQuit(4)
    // Its processes ended while the question was open; no tick since.
    core.gone = [4]
    m.confirmQuit()
    #expect(core.quits.isEmpty)
    #expect(m.quitting.isEmpty)
    #expect(m.notes[4] == "Nothing left to quit")
}

@MainActor
@Test func anEndForAnotherPopoverIsIgnored() {
    let m = model()
    m.quitEnded(row: 1, quit: 0, denied: 0, left: 3)
    #expect(m.notes.isEmpty && m.quitting.isEmpty)
}

@MainActor
@Test func appsQuitLikeTheDock() async throws {
    let core = SampleCore()
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    let m = model(core, apps: apps)
    m.askQuit(13)
    m.confirmQuit()
    #expect(apps.asked == ["/Applications/Slack.app"])
    #expect(core.quits.isEmpty, "an app is not signalled by the core")
    #expect(m.quitting == [13])
    try await Task.sleep(for: .milliseconds(120))
    #expect(m.quitting == [13], "quit: greyed until the row is gone")
    #expect(m.stubborn.isEmpty && m.notes.isEmpty)
    core.gone = [13]
    core.elapsed = 5
    m.refresh()
    #expect(m.quitting.isEmpty)
}

@MainActor
@Test func anAppThatQuitBeforeForceQuitIsLeftAlone() async throws {
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    apps.stubborn = ["/Applications/Slack.app"]
    let m = model(apps: apps)
    m.askQuit(13)
    m.confirmQuit()
    try await until { !m.stubborn.isEmpty }
    // It asked about unsaved work; the user answered, and it quit.
    apps.stubborn = []
    _ = apps.quit(path: "/Applications/Slack.app")
    m.forceQuit(13)
    #expect(apps.forced.isEmpty)
    #expect(m.stubborn.isEmpty && m.notes[13] == nil && m.quitting.isEmpty)
}

@MainActor
@Test func aStubbornAppOffersForceQuit() async throws {
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    apps.stubborn = ["/Applications/Slack.app"]
    let m = model(apps: apps)
    m.askQuit(13)
    m.confirmQuit()
    try await until { !m.stubborn.isEmpty }
    #expect(m.stubborn == [13])
    #expect(m.quitting.isEmpty)
    #expect(m.notes[13] == "Slack is still open")
    #expect(apps.forced.isEmpty, "never forced without a second click")
    m.forceQuit(13)
    #expect(apps.forced.count == 1)
    #expect(m.stubborn.isEmpty && m.notes[13] == nil && m.quitting == [13])
}

@MainActor
@Test func anAppTheWorkspaceDoesNotKnowIsQuitByTheCore() {
    // A helper run from inside a bundle, or a path that differs.
    let core = SampleCore()
    let m = model(core, apps: SampleAppQuitter(running: []))
    m.askQuit(13)
    m.confirmQuit()
    #expect(core.quits == [13])
    #expect(m.quitting == [13] && m.notes[13] == nil)
}

@MainActor
@Test func aReopenedAppIsNeverForceQuit() async throws {
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    apps.stubborn = ["/Applications/Slack.app"]
    let m = model(apps: apps)
    m.askQuit(13)
    m.confirmQuit()
    try await until { !m.stubborn.isEmpty }
    let asked = apps.forced
    #expect(asked.isEmpty)
    // The user quits it from the Dock, then opens it again.
    apps.stubborn = []
    _ = apps.quit(path: "/Applications/Slack.app")
    let again = apps.open("/Applications/Slack.app")
    m.forceQuit(13)
    #expect(!apps.forced.contains(again), "only the instance that was asked")
    #expect(apps.isRunning([again]))
}

@MainActor
@Test func aForceQuitThatFailsSaysSo() async throws {
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    apps.stubborn = ["/Applications/Slack.app"]
    apps.unkillable = ["/Applications/Slack.app"]
    let m = model(apps: apps)
    m.askQuit(13)
    m.confirmQuit()
    try await until { !m.stubborn.isEmpty }
    m.forceQuit(13)
    try await until { m.quitting.isEmpty }
    #expect(m.stubborn == [13])
    #expect(m.notes[13] == "Slack could not be quit")
}

@MainActor
@Test func aQuitOutlivesThePopover() async throws {
    // The popover closes while a quit runs; the next one shows how it ended.
    let core = SampleCore()
    let quits = QuitState(apps: SampleAppQuitter(), checkAfter: .milliseconds(50))
    let first = PopoverModel(source: core, defaults: MemoryDefaults(), loginItem: SampleLoginItem(), quits: quits)
    first.refresh()
    let row = first.snapshot!.dev[0].id
    first.askQuit(row)
    first.confirmQuit()
    #expect(core.quits == [row])
    // Closed: the controller hears the core, no model is alive.
    quits.ended(row: row, quit: 2, denied: 0, left: 1)
    let second = PopoverModel(source: core, defaults: MemoryDefaults(), loginItem: SampleLoginItem(), quits: quits)
    second.refresh()
    #expect(second.notes[row] == "1 still running")
    #expect(second.quitting.isEmpty)
}

@MainActor
@Test func foldingARowClearsItsNote() {
    let core = SampleCore()
    let m = model(core)
    let row = m.snapshot!.dev[0].id
    m.askQuit(row)
    m.confirmQuit()
    m.quitEnded(row: row, quit: 0, denied: 0, left: 2)
    #expect(m.notes[row] != nil)
    m.toggleRow(row)
    #expect(m.open == nil && m.notes[row] == nil)
}

@MainActor
@Test func systemAppsHaveNoCross() {
    let m = model()
    let finder = Row(id: 99, kind: .app, name: "Finder", detail: "", footprint: 1,
                     path: "/System/Library/CoreServices/Finder.app")
    #expect(!m.canQuit(finder))
    // The apps macOS ships are quit like any other.
    for path in ["/System/Applications/Calculator.app", "/System/Applications/TextEdit.app",
                 "/System/Volumes/Preboot/Cryptexes/App/System/Applications/Safari.app"] {
        let r = Row(id: 98, kind: .app, name: "x", detail: "", footprint: 1, path: path)
        #expect(m.canQuit(r), "\(path)")
    }
}

@MainActor
@Test func devloadHasNoCross() {
    let m = model(own: "/Applications/Orca.app")
    let orca = m.list(.apps).rows.first { $0.name == "Orca" }!
    #expect(!m.canQuit(orca))
    m.askQuit(orca.id)
    #expect(m.confirming == nil)
    let slack = m.snapshot!.apps.first { $0.name == "Slack" }!
    #expect(m.canQuit(slack))
}

@MainActor
@Test func aDevRowWithNothingToStopHasNoCross() {
    final class Empty: CoreSource {
        let base = SampleCore()
        func snapshot() -> Snapshot? { base.snapshot() }
        func series(period: Period, mode: GraphMode, width: Int) -> Series? { nil }
        func prompt(period: Period) -> String? { nil }
        func setPopoverOpen(_ open: Bool) {}
        func parts(row id: UInt32) -> Parts? { Parts(parts: [], processes: 0) }
    }
    let src = Empty()
    let m = PopoverModel(source: src, defaults: MemoryDefaults(), loginItem: SampleLoginItem())
    m.refresh()
    #expect(!m.canQuit(m.snapshot!.dev[0]))
}

@MainActor
@Test func partsAreReadAgainOnEachTick() {
    let core = SampleCore()
    let m = model(core)
    m.toggleRow(2)
    #expect(m.parts != nil)
    core.gone = [2]
    m.refresh()
    #expect(m.parts == nil && m.open == nil)
}

@MainActor
@Test func anOpenRowAddsItsPartsUnderIt() {
    let m = model()
    func height() -> CGFloat { Snapshots.popoverView(model: m).fittingSize.height }
    let closed = height()
    m.toggleRow(1)
    let open = height()
    // 3 parts of 20 pt and 4 pt under them.
    #expect(abs(open - closed - (3 * RowDetail.partHeight + 4)) < 1, "\(closed) -> \(open)")
    m.askQuit(1)
    #expect(abs(height() - open - 28) < 1, "the question adds one line")
    m.toggleRow(1)
    #expect(abs(height() - closed) < 1, "folding gives the height back")
}

@MainActor
@Test func aStubbornAppKeepsItsWarningFolded() async throws {
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    apps.stubborn = ["/Applications/Slack.app"]
    let m = model(apps: apps)
    m.askQuit(13)
    m.confirmQuit()
    try await until { !m.stubborn.isEmpty }
    m.toggleRow(13)
    #expect(m.open == nil)
    #expect(m.notes[13] == "Slack is still open")
}

@MainActor
@Test func aSingleProcessAppListsNothingUnderIt() {
    final class One: CoreSource {
        let base = SampleCore()
        func snapshot() -> Snapshot? { base.snapshot() }
        func series(period: Period, mode: GraphMode, width: Int) -> Series? { nil }
        func prompt(period: Period) -> String? { nil }
        func setPopoverOpen(_ open: Bool) {}
        func parts(row id: UInt32) -> Parts? {
            Parts(parts: [Part(kind: .process, label: "Slack", count: 1, idle: 0, footprint: 1)], processes: 1)
        }
    }
    let m = PopoverModel(source: One(), defaults: MemoryDefaults(), loginItem: SampleLoginItem())
    m.refresh()
    let app = m.snapshot!.apps[0].id
    m.toggleRow(app)
    #expect(m.parts != nil && m.detail == nil)
    #expect(m.canQuit(m.snapshot!.apps[0]), "the cross stays")
    let dev = m.snapshot!.dev[0].id
    m.toggleRow(dev)
    #expect(m.detail != nil, "a dev row always lists what it is made of")
}

@MainActor
@Test func aStubbornAppQuitWhilePopoverClosedLosesItsWarning() async throws {
    let core = SampleCore()
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    apps.stubborn = ["/Applications/Slack.app"]
    let quits = QuitState(apps: apps, checkAfter: .milliseconds(50))
    let first = PopoverModel(source: core, defaults: MemoryDefaults(), loginItem: SampleLoginItem(), quits: quits)
    first.refresh()
    first.askQuit(13)
    first.confirmQuit()
    try await until { !first.stubborn.isEmpty }
    // Closed: the user quits it from the Dock and opens it again, same row.
    apps.stubborn = []
    _ = apps.quit(path: "/Applications/Slack.app")
    let again = apps.open("/Applications/Slack.app")
    let second = PopoverModel(source: core, defaults: MemoryDefaults(), loginItem: SampleLoginItem(), quits: quits)
    second.refresh()
    #expect(second.stubborn.isEmpty && second.notes[13] == nil, "the warning was about the instance gone")
    second.forceQuit(13)
    #expect(apps.forced.isEmpty && apps.isRunning([again]))
}

@MainActor
@Test func aFoldedListKeepsTheRowsInUse() async throws {
    let core = SampleCore()
    let apps = SampleAppQuitter(running: ["/Applications/Slack.app"])
    apps.stubborn = ["/Applications/Slack.app"]
    let m = model(core, apps: apps)
    let names = { m.list(.apps).rows.map(\.name) }
    #expect(names() == ["Google Chrome", "Orca", "Docker Desktop"])
    m.askQuit(13)
    #expect(names() == ["Google Chrome", "Orca", "Docker Desktop", "Slack"], "asking")
    m.confirmQuit()
    #expect(names().contains("Slack"), "being quit")
    try await until { !m.stubborn.isEmpty }
    m.toggleRow(13)
    #expect(m.open == nil && m.notes[13] != nil)
    let far = m.snapshot!.apps[9]
    m.toggleRow(far.id)
    let list = m.list(.apps)
    // Footprint order, and the "+N more" line counts only what it hides.
    #expect(list.rows.map(\.id) == [10, 11, 12, 13, far.id], "with a note, and unfolded")
    let hidden = m.snapshot!.apps.filter { !list.rows.map(\.id).contains($0.id) }
    #expect(list.more == More(count: hidden.count, footprint: hidden.reduce(0) { $0 + $1.footprint }))
    #expect(list.more.count == 13)
}

@MainActor
@Test func aBarePidIsNeverTrusted() {
    // A running app this quitter never asked: not "running" for it, so a
    // Force Quit could never reach it. Nothing is quit here.
    let q = AppQuitter()
    guard let other = NSWorkspace.shared.runningApplications.first(where: { $0.processIdentifier != getpid() }) else { return }
    #expect(!q.isRunning([other.processIdentifier]))
}
