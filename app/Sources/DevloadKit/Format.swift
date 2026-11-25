import Foundation

/// Every string the UI shows, in one place. GB means GiB, like Activity
/// Monitor: 24 GB of RAM is 24 × 2³⁰ bytes.
public enum Format {
    static let gib = 1_073_741_824.0

    /// The menu bar figure: "19.5 GB".
    public static func icon(used: UInt64) -> String {
        String(format: "%.1f GB", Double(used) / gib)
    }

    /// The widest figure the icon can show on this Mac, to size it once.
    public static func iconWidest(total: UInt64) -> String {
        Double(total) / gib >= 99.95 ? "888.8 GB" : "88.8 GB"
    }

    /// "19.46 / 24 GB" (or "19.46 / 25.5 GB" for an odd total).
    public static func usedOfTotal(used: UInt64, total: UInt64) -> String {
        let t = Double(total) / gib
        let total = abs(t - t.rounded()) < 0.05 ? String(format: "%.0f", t) : String(format: "%.1f", t)
        return String(format: "%.2f / ", Double(used) / gib) + total + " GB"
    }

    /// A row's footprint: "2.40 GB".
    public static func footprint(_ bytes: UInt64) -> String {
        String(format: "%.2f GB", Double(bytes) / gib)
    }

    /// MiB from the graph, as a row's footprint: "18.40 GB".
    static func mib(_ v: Float) -> String { footprint(UInt64(max(v, 0) * 1_048_576)) }

    /// What the hover bubble says about column `i`: when, then one line per
    /// value (a colour index into `Theme.groups` in Groups mode, the
    /// pressure level in Pressure mode). Nil past the columns.
    public struct Hover: Equatable, Sendable {
        public struct Line: Equatable, Sendable {
            public enum Swatch: Equatable, Sendable { case pressure(Pressure), group(Int) }
            public let swatch: Swatch?
            public let label: String
            public let value: String
        }
        public let title: String
        /// Groups mode, on the title's line: the column's sum.
        public var total: String? = nil
        public let lines: [Line]
    }

    public static func hover(_ s: Series, column i: Int, calendar: Calendar = .current) -> Hover? {
        guard i >= 0, i < s.width else { return nil }
        let span = s.to.timeIntervalSince(s.from)
        let at = s.from.addingTimeInterval(span * (Double(i) + 0.5) / Double(s.width))
        // The day only when the column is not on the graph's last day.
        let title = calendar.isDate(at, inSameDayAs: s.to) ? clock(at, calendar) : stamp("EEE HH:mm", at, calendar)
        typealias L = Hover.Line
        switch s.mode {
        case .pressure:
            guard s.level[i] != Series.noData else { return Hover(title: title, lines: [L(swatch: nil, label: "No data", value: "")]) }
            let p = Pressure(raw: s.level[i])
            return Hover(title: title, lines: [L(swatch: .pressure(p), label: pressureName(p), value: mib(s.hi[i]))])
        case .groups:
            guard s.hasData[i] else { return Hover(title: title, lines: [L(swatch: nil, label: "No data", value: "")]) }
            var lines: [L] = []
            for k in 0..<4 {
                let v = s.stack[4 * i + k]
                guard v > 0 else { continue }
                let name = k < 3 ? (k < s.top.count ? s.top[k] : nil) : "Other"
                guard let name else { continue }
                lines.append(L(swatch: .group(k), label: name, value: mib(v)))
            }
            // The sum on the title's line: four groups and a title fill the
            // 72 pt graph.
            let sum = s.stack[4 * i..<4 * i + 4].reduce(0, +)
            return Hover(title: title, total: lines.count > 1 ? mib(sum) : nil, lines: lines)
        }
    }

    public static func cpu(_ percent: Int) -> String { "CPU \(percent) %" }

    public static func pressureTitle(_ p: Pressure) -> String {
        switch p {
        case .normal: "Memory normal"
        case .tight: "Memory tight"
        case .critical: "Memory critical"
        }
    }

    public static func pressureName(_ p: Pressure) -> String {
        switch p {
        case .normal: "Normal"
        case .tight: "Tight"
        case .critical: "Critical"
        }
    }

    /// "since 10:45" today, "since Tue 10:45" this week, "since Nov 18" before.
    public static func since(_ date: Date, now: Date, calendar: Calendar = .current) -> String {
        "since " + when(date, now: now, calendar: calendar)
    }

    static func when(_ date: Date, now: Date, calendar: Calendar) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return clock(date, calendar) }
        if now.timeIntervalSince(date) < 6 * 86_400 { return stamp("EEE HH:mm", date, calendar) }
        return stamp("MMM d", date, calendar)
    }

    /// A mark under the graph: where it falls (0 = left edge, 1 = right)
    /// and its label.
    public struct Tick: Equatable, Sendable {
        public let at: Double
        public let label: String
    }

    /// Round clock times under the graph: every 15 min for 1 hour, 2 hours
    /// for 6, 6 hours for 24 (midnight shows the day), each midnight for 7
    /// days. Marks too close to an edge for their label are left out.
    public static func ticks(from: Date, to: Date, period: Period, calendar: Calendar = .current) -> [Tick] {
        let span = to.timeIntervalSince(from)
        guard span > 0 else { return [] }
        let step: DateComponents = switch period {
        case .h1: DateComponents(minute: 15)
        case .h6: DateComponents(hour: 2)
        case .h24: DateComponents(hour: 6)
        case .d7: DateComponents(day: 1)
        }
        var out: [Tick] = []
        // The first round time after `from`: walk from the day's midnight.
        var t = calendar.startOfDay(for: from)
        while t <= to {
            if t > from {
                let at = t.timeIntervalSince(from) / span
                let midnight = calendar.component(.hour, from: t) == 0 && calendar.component(.minute, from: t) == 0
                let label = period == .d7 || (period == .h24 && midnight) ? stamp("EEE", t, calendar) : clock(t, calendar)
                if at > edge && at < 1 - edge { out.append(Tick(at: at, label: label)) }
            }
            guard let next = calendar.date(byAdding: step, to: t) else { break }
            t = next
        }
        return out
    }

    /// Half a label's width over the graph's: marks closer to an edge are dropped.
    static let edge = 0.05

    /// "Collecting since 14:02" over an empty start of the graph.
    public static func collecting(since date: Date, now: Date, calendar: Calendar = .current) -> String {
        "Collecting since " + when(date, now: now, calendar: calendar)
    }

    /// The y axis top: "24 GB".
    public static func axisTop(totalMiB: Float) -> String {
        let g = Double(totalMiB) / 1024
        return abs(g - g.rounded()) < 0.05 ? String(format: "%.0f GB", g) : String(format: "%.1f GB", g)
    }

    public static func more(_ m: More) -> String { "+\(m.count) more" }

    /// What VoiceOver says of the graph: "Memory pressure, last 1h: Tight
    /// now, Critical at worst." or "Memory by group, last 7d: a, b, Other."
    public static func graphLabel(_ s: Series) -> String {
        let head = (s.mode == .pressure ? "Memory pressure" : "Memory by group") + ", last \(s.period.label)"
        switch s.mode {
        case .pressure:
            let levels = s.level.filter { $0 != Series.noData }.map(Pressure.init(raw:))
            guard let now = levels.last, let worst = levels.max(by: { $0.rawValue < $1.rawValue }) else {
                return head + ": no data yet."
            }
            return head + ": \(pressureName(now)) now, \(pressureName(worst)) at worst."
        case .groups:
            return head + ": " + (s.top.prefix(3) + ["Other"]).joined(separator: ", ") + "."
        }
    }

    /// The notification of a pressure episode.
    public static func alert(pressure: Pressure, used: UInt64, total: UInt64, biggest: Row?) -> (title: String, body: String) {
        var body = usedOfTotal(used: used, total: total) + " used."
        if let b = biggest { body += " Biggest: \(b.name) (\(footprint(b.footprint)))." }
        return (pressureTitle(pressure), body)
    }

    private static func clock(_ date: Date, _ calendar: Calendar) -> String { stamp("HH:mm", date, calendar) }

    private static func stamp(_ pattern: String, _ date: Date, _ calendar: Calendar) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.dateFormat = pattern
        return f.string(from: date)
    }

    // MARK: Row parts and quitting

    static func count(_ n: Int, _ one: String, _ many: String) -> String { "\(n) \(n == 1 ? one : many)" }

    /// A part's line: "node ×4", "3 Claude sessions, 2 idle", "Linux VM".
    public static func part(_ p: Part) -> String {
        switch p.kind {
        case .process:
            return p.count > 1 ? "\(p.label) ×\(p.count)" : p.label
        case .agent:
            let base = p.count > 1 ? "\(p.count) \(p.label)" : p.label
            if p.idle == 0 { return base }
            if p.idle >= p.count { return base + (p.count > 1 ? ", all idle" : ", idle") }
            return base + ", \(p.idle) idle"
        case .docker:
            return p.label
        }
    }

    /// What a docker part runs, after its name: "api, db", or "2 containers"
    /// when the services are unknown. Else "".
    public static func partDetail(_ p: Part) -> String {
        guard p.kind == .docker else { return "" }
        return p.detail.isEmpty ? count(p.count, "container", "containers") : p.detail
    }

    /// "Quit 12 processes and 2 containers in api-refactor?": the row is
    /// named, so a click on a row that moved never confirms another one.
    /// Where it does not fit, the popover shows `quitApp(name)` instead.
    public static func quitQuestion(name: String, processes: Int, containers: Int) -> String {
        var what: [String] = []
        if processes > 0 { what.append(count(processes, "process", "processes")) }
        if containers > 0 { what.append(count(containers, "container", "containers")) }
        return "Quit " + what.joined(separator: " and ") + " in \(name)?"
    }

    public static func quitApp(_ name: String) -> String { "Quit \(name)?" }

    /// Said under a row whose quit did not stop everything. Nil when it did.
    public static func quitResult(quit: Int, denied: Int, left: Int) -> String? {
        var said: [String] = []
        if left > 0 { said.append("\(left) still running") }
        if denied > 0 { said.append("\(denied) not yours to quit") }
        if said.isEmpty { return quit == 0 ? "Nothing left to quit" : nil }
        return said.joined(separator: ", ")
    }

    public static func stillOpen(_ name: String) -> String { "\(name) is still open" }

    /// After Force Quit, the app still runs.
    public static func couldNotQuit(_ name: String) -> String { "\(name) could not be quit" }
}
