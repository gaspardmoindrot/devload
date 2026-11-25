import AppKit
import Observation

/// Everything the popover shows, read from the core when it opens and on
/// each tick while it stays open.
@MainActor
@Observable
public final class PopoverModel {
    public static let periodKey = "graph.period"
    public static let modeKey = "graph.mode"

    public private(set) var snapshot: Snapshot?
    public private(set) var series: Series?
    public private(set) var period: Period
    public private(set) var mode: GraphMode
    /// "Prompt copied" replaces the Analyze label for a moment.
    public private(set) var copied = false
    public private(set) var login: LoginStatus
    /// Lists unfolded by a click on "+N more". Folded again on each open.
    public private(set) var expanded: Set<ListKind> = []

    /// The row unfolded by a click, and what it is made of (read again on
    /// each tick). One at a time.
    public private(set) var open: UInt32?
    public private(set) var parts: Parts?
    /// What the unfolded row lists under it. Nothing for an app that is a
    /// single process: "Calculator" under "Calculator" says nothing more.
    public var detail: Parts? {
        guard let p = parts else { return nil }
        if let id = open, row(id)?.kind == .app, p.parts.count == 1, p.more.count == 0, p.parts[0].count <= 1,
           p.containers == 0 {
            return nil
        }
        return p
    }
    /// The row whose "×" asks "Quit …?" under it.
    public private(set) var confirming: UInt32?
    /// Rows being quit: greyed, with a spinner.
    public var quitting: Set<UInt32> { quits.quitting }
    /// Apps that did not quit when asked: they offer Force Quit.
    public var stubborn: Set<UInt32> { quits.stubborn }
    /// What a quit left behind, said under its row.
    public var notes: [UInt32: String] { quits.notes }

    @ObservationIgnored private let source: CoreSource
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let loginItem: LoginItemControlling
    @ObservationIgnored private let pasteboard: NSPasteboard
    @ObservationIgnored private let copiedFor: Duration
    @ObservationIgnored private var copiedTask: Task<Void, Never>?
    @ObservationIgnored public var onQuit: () -> Void = { NSApp.terminate(nil) }
    /// Kept by the app controller across popovers; the default only
    /// pretends to quit apps, so tests and screenshots never quit one.
    @ObservationIgnored public let quits: QuitState
    @ObservationIgnored private let ownBundle: String

    public init(source: CoreSource, defaults: UserDefaults = .standard, loginItem: LoginItemControlling,
                pasteboard: NSPasteboard = .general, copiedFor: Duration = .seconds(2),
                quits: QuitState = QuitState(),
                ownBundle: String = Bundle.main.bundleURL.resolvingSymlinksInPath().standardizedFileURL.path) {
        self.source = source
        self.quits = quits
        self.ownBundle = ownBundle
        self.defaults = defaults
        self.loginItem = loginItem
        self.pasteboard = pasteboard
        self.copiedFor = copiedFor
        period = Period(rawValue: UInt8(clamping: defaults.integer(forKey: Self.periodKey))) ?? .h1
        mode = GraphMode(rawValue: UInt8(clamping: defaults.integer(forKey: Self.modeKey))) ?? .pressure
        login = loginItem.status
    }

    /// Reads the header, the lists and the graph again.
    public func refresh() {
        snapshot = source.snapshot()
        refreshSeries()
        refreshRows()
    }

    // MARK: Unfolding and quitting a row

    private var allRows: [Row] { (snapshot?.dev ?? []) + (snapshot?.apps ?? []) }

    private func row(_ id: UInt32) -> Row? { allRows.first { $0.id == id } }

    /// Rows gone from the snapshot fold and forget their state.
    private func refreshRows() {
        let ids = Set(allRows.map(\.id))
        if let o = open, !ids.contains(o) { open = nil }
        if let c = confirming, !ids.contains(c) { confirming = nil }
        if let s = snapshot { quits.update(rows: ids, time: s.time) }
        parts = open.flatMap { source.parts(row: $0) }
    }

    /// A click on a row unfolds it, or folds it back.
    public func toggleRow(_ id: UInt32) {
        if open == id {
            open = nil
            parts = nil
            confirming = nil
            // Seen. A stubborn app keeps its warning: it still runs.
            if !quits.stubborn.contains(id) { quits.clearNote(id) }
            return
        }
        open = id
        parts = source.parts(row: id)
        if confirming != id { confirming = nil }
    }

    /// macOS's own (Finder, Dock, loginwindow…), never quit. The apps it
    /// ships (/System/Applications/Calculator.app, Safari in its cryptex)
    /// are apps like any other. Same list as the core.
    static func isSystemBundle(_ path: String) -> Bool {
        ["/System/Library/", "/usr/", "/Library/Apple/"].contains { path.hasPrefix($0) }
    }

    /// An app row is quit the way the Dock does it, by its bundle.
    private func quitsAsApp(_ r: Row) -> Bool { r.kind == .app && !r.path.isEmpty }

    /// Whether a row gets a "×": not devload itself, and something to stop.
    public func canQuit(_ r: Row) -> Bool {
        if r.kind == .app {
            let path = URL(fileURLWithPath: r.path).resolvingSymlinksInPath().standardizedFileURL.path
            if path == ownBundle || Self.isSystemBundle(path) { return false }
        }
        if quitsAsApp(r) { return true }
        guard let p = (open == r.id ? parts : source.parts(row: r.id)) else { return false }
        return p.processes + p.containers > 0
    }

    /// The "×": unfolds the row and asks under it.
    public func askQuit(_ id: UInt32) {
        guard let r = row(id), canQuit(r), !quitting.contains(id) else { return }
        if open != id { toggleRow(id) }
        quits.clearNote(id)
        confirming = id
    }

    public func cancelQuit() { confirming = nil }

    /// What the confirmation asks.
    public func question(_ id: UInt32) -> String? {
        guard let r = row(id) else { return nil }
        if quitsAsApp(r) { return Format.quitApp(r.name) }
        guard let p = (open == id ? parts : source.parts(row: id)) else { return nil }
        return Format.quitQuestion(name: r.name, processes: p.processes, containers: p.containers)
    }

    public func confirmQuit() {
        guard let id = confirming, let r = row(id) else { return }
        confirming = nil
        // An app quits like the Dock does it. One that runs outside an app
        // NSWorkspace knows (a helper in a bundle) is quit by the core.
        if quitsAsApp(r), quits.quitApp(id, path: r.path, name: r.name) { return }
        if source.quit(row: id) {
            quits.quitByCore(id)
        } else {
            quits.say(id, Format.quitResult(quit: 0, denied: 0, left: 0))
        }
    }

    public func forceQuit(_ id: UInt32) {
        guard let r = row(id) else { return }
        quits.forceQuit(id, name: r.name)
    }

    /// The core's answer to `quit(row:)`.
    public func quitEnded(row id: UInt32, quit: Int, denied: Int, left: Int) {
        quits.ended(row: id, quit: quit, denied: denied, left: left)
        refresh()
    }

    public func select(_ p: Period) {
        guard p != period else { return }
        period = p
        defaults.set(Int(p.rawValue), forKey: Self.periodKey)
        refreshSeries()
    }

    public func select(_ m: GraphMode) {
        guard m != mode else { return }
        mode = m
        defaults.set(Int(m.rawValue), forKey: Self.modeKey)
        refreshSeries()
    }

    /// Copies the prompt for the period on screen.
    public func analyze() {
        guard let prompt = source.prompt(period: period) else { return }
        pasteboard.clearContents()
        pasteboard.setString(prompt, forType: .string)
        copied = true
        copiedTask?.cancel()
        copiedTask = Task { [weak self, copiedFor] in
            try? await Task.sleep(for: copiedFor)
            guard !Task.isCancelled else { return }
            self?.copied = false
        }
    }

    public func setLaunchAtLogin(_ on: Bool) {
        try? loginItem.setEnabled(on)
        login = loginItem.status
    }

    public func refreshLogin() { login = loginItem.status }

    public func quit() { onQuit() }

    public func toggle(_ list: ListKind) {
        if expanded.contains(list) { expanded.remove(list) } else { expanded.insert(list) }
    }

    private func refreshSeries() {
        let width = Int(GraphView.size.width)
        series = source.series(period: period, mode: mode, width: width)
        firstSample = nil
        guard collecting != nil else { return }
        // A column of 24 h or 7 d spans minutes: ask the shorter periods,
        // whose columns are seconds wide, when the history began.
        for p in Period.allCases where p.rawValue <= period.rawValue {
            guard let s = source.series(period: p, mode: .pressure, width: width),
                  let first = Self.firstColumn(s) else { continue }
            if first > 0 || p == period {
                firstSample = s.from.addingTimeInterval(s.to.timeIntervalSince(s.from) * Double(first) / Double(s.width))
                return
            }
        }
    }

    /// When the history began, to the precision of the shortest period
    /// that shows its start. Read only while `collecting` is shown.
    private var firstSample: Date?

    private static func firstColumn(_ s: Series) -> Int? {
        (0..<s.width).first { s.mode == .pressure ? s.level[$0] != Series.noData : s.hasData[$0] }
    }

    // MARK: What the view reads

    public struct LegendItem: Equatable {
        public let name: String
        public let color: NSColor
        /// "Other" is drawn lighter, like its band.
        public let alpha: Double
    }

    public var legend: [LegendItem] {
        switch mode {
        case .pressure:
            return Pressure.allCases.map { LegendItem(name: Format.pressureName($0), color: Theme.pressure($0), alpha: 1) }
        case .groups:
            let names = series?.top ?? []
            return names.prefix(3).enumerated().map { LegendItem(name: $1, color: Theme.groups[$0], alpha: 1) }
                + [LegendItem(name: "Other", color: Theme.groups[3], alpha: 0.5)]
        }
    }

    public enum ListKind: Hashable, Sendable { case dev, apps }

    /// Rows shown while a list is folded.
    public static let foldedRows = 3

    /// What a list shows: its rows, then the "+N more" line, which unfolds
    /// the list when `foldable`. The core sends up to 50 rows per list and
    /// sums the rest in `beyond`, which stays as a plain line when unfolded.
    public struct ListView: Equatable {
        public let rows: [Row]
        public let more: More
        public let foldable: Bool
        public let expanded: Bool
    }

    /// A folded list shows its first rows, and also any row unfolded,
    /// being quit, asking, or saying how a quit ended, in footprint order.
    public func list(_ kind: ListKind) -> ListView {
        guard let s = snapshot else { return ListView(rows: [], more: .none, foldable: false, expanded: false) }
        let (all, beyond) = kind == .dev ? (s.dev, s.devMore) : (s.apps, s.appsMore)
        let shown = all.enumerated().filter { $0.offset < Self.foldedRows || inUse($0.element.id) }.map(\.element)
        // "+1 more" takes a line anyway: show the row instead.
        if all.count <= shown.count + 1 {
            return ListView(rows: all, more: beyond, foldable: false, expanded: false)
        }
        if expanded.contains(kind) {
            return ListView(rows: all, more: beyond, foldable: true, expanded: true)
        }
        let ids = Set(shown.map(\.id))
        let rest = all.filter { !ids.contains($0.id) }
        let more = More(count: rest.count + beyond.count, footprint: rest.reduce(beyond.footprint) { $0 + $1.footprint })
        return ListView(rows: shown, more: more, foldable: true, expanded: false)
    }

    /// A row the user is busy with: folding the list never hides it.
    private func inUse(_ id: UInt32) -> Bool {
        open == id || confirming == id || quitting.contains(id) || notes[id] != nil
    }

    /// The round times under the graph.
    public var ticks: [Format.Tick] {
        guard let s = series else { return [] }
        return Format.ticks(from: s.from, to: s.to, period: period)
    }

    /// The part of the graph before the first sample, when history does not
    /// cover the period yet: its width (0–1 of the graph) and the note to
    /// show in it. Nil once the data starts near the left edge.
    public var collecting: (width: Double, note: String)? {
        guard let s = series, s.width > 0 else { return nil }
        let first = Self.firstColumn(s)
        let width = Double(first ?? s.width) / Double(s.width)
        guard width >= Self.collectingFrom else { return nil }
        guard first != nil, let since = firstSample else { return (1, "Collecting…") }
        return (width, Format.collecting(since: since, now: s.to))
    }

    /// An empty start narrower than this is not worth a note.
    static let collectingFrom = 0.15
}
