import CDevloadCore
import Foundation

// Swift values for what the Rust core hands over through `devload.h`. The C
// structs are copied into these right away and freed: nothing in Swift ever
// holds a pointer into the core.

public enum Pressure: UInt8, Sendable, CaseIterable {
    case normal = 0, tight = 1, critical = 2

    init(raw: UInt8) { self = Pressure(rawValue: raw) ?? .critical }
}

public struct Row: Equatable, Sendable {
    public enum Kind: UInt8, Sendable { case dev = 0, app = 1 }

    /// Stable while the app runs: what `parts` and `quit` take.
    public let id: UInt32
    public let kind: Kind
    public let name: String
    public let detail: String
    public let footprint: UInt64
    /// Per-mille of one core.
    public let cpuPermille: UInt32
    /// The .app bundle, or the worktree folder. "" when there is none.
    public let path: String

    public init(id: UInt32 = 0, kind: Kind = .dev, name: String, detail: String, footprint: UInt64,
                cpuPermille: UInt32 = 0, path: String = "") {
        self.id = id
        self.kind = kind
        self.name = name
        self.detail = detail
        self.footprint = footprint
        self.cpuPermille = cpuPermille
        self.path = path
    }
}

/// One part of a row: processes sharing a label ("node ×4"), an agent's
/// sessions, or a docker stack.
public struct Part: Equatable, Sendable {
    public enum Kind: UInt8, Sendable { case process = 0, agent = 1, docker = 2 }

    public let kind: Kind
    public let label: String
    /// Docker: the stack's services. Else "".
    public let detail: String
    /// Processes; agent: sessions; docker: containers.
    public let count: Int
    /// Agent: idle sessions.
    public let idle: Int
    public let footprint: UInt64

    public init(kind: Kind, label: String, detail: String = "", count: Int = 1, idle: Int = 0, footprint: UInt64) {
        self.kind = kind
        self.label = label
        self.detail = detail
        self.count = count
        self.idle = idle
        self.footprint = footprint
    }
}

/// What a row is made of at the last tick, and what quitting it stops.
public struct Parts: Equatable, Sendable {
    public let parts: [Part]
    public let more: More
    public let processes: Int
    public let containers: Int

    public init(parts: [Part], more: More = .none, processes: Int, containers: Int = 0) {
        self.parts = parts
        self.more = more
        self.processes = processes
        self.containers = containers
    }
}

/// The rows past the cut, folded into "+N more".
public struct More: Equatable, Sendable {
    public let count: Int
    public let footprint: UInt64

    public init(count: Int, footprint: UInt64) {
        self.count = count
        self.footprint = footprint
    }

    public static let none = More(count: 0, footprint: 0)
}

public struct Snapshot: Equatable, Sendable {
    public let pressure: Pressure
    public let cpuPercent: Int
    public let time: Date
    /// Since when the pressure level holds.
    public let since: Date
    public let used: UInt64
    public let total: UInt64
    public let dev: [Row]
    public let devMore: More
    public let apps: [Row]
    public let appsMore: More

    public init(pressure: Pressure, cpuPercent: Int, time: Date, since: Date, used: UInt64, total: UInt64,
                dev: [Row], devMore: More, apps: [Row], appsMore: More) {
        self.pressure = pressure
        self.cpuPercent = cpuPercent
        self.time = time
        self.since = since
        self.used = used
        self.total = total
        self.dev = dev
        self.devMore = devMore
        self.apps = apps
        self.appsMore = appsMore
    }
}

public enum Period: UInt8, CaseIterable, Sendable {
    case h1 = 0, h6 = 1, h24 = 2, d7 = 3

    public var label: String {
        switch self {
        case .h1: "1h"
        case .h6: "6h"
        case .h24: "24h"
        case .d7: "7d"
        }
    }
}

public enum GraphMode: UInt8, CaseIterable, Sendable {
    case pressure = 0, groups = 1

    public var label: String {
        switch self {
        case .pressure: "Pressure"
        case .groups: "Groups"
        }
    }
}

/// One graph, already cut to `width` columns by the core.
public struct Series: Equatable, Sendable {
    public static let noData: UInt8 = 255

    public let period: Period
    public let mode: GraphMode
    public let from: Date
    public let to: Date
    /// Top of the y axis, MiB.
    public let totalMiB: Float
    /// Pressure mode: min and max used memory per column (MiB), and the
    /// worst pressure level (`noData` for an empty column).
    public let lo: [Float]
    public let hi: [Float]
    public let level: [UInt8]
    /// Groups mode: 4 values per column (top 1, 2, 3, other; MiB).
    public let stack: [Float]
    public let hasData: [Bool]
    /// Groups mode: the legend, biggest first (0–3 names).
    public let top: [String]

    public var width: Int { mode == .pressure ? level.count : hasData.count }

    public init(period: Period, mode: GraphMode, from: Date, to: Date, totalMiB: Float,
                lo: [Float] = [], hi: [Float] = [], level: [UInt8] = [],
                stack: [Float] = [], hasData: [Bool] = [], top: [String] = []) {
        self.period = period
        self.mode = mode
        self.from = from
        self.to = to
        self.totalMiB = totalMiB
        self.lo = lo
        self.hi = hi
        self.level = level
        self.stack = stack
        self.hasData = hasData
        self.top = top
    }
}

public enum CoreEvent: Equatable, Sendable {
    /// A new sample worth redrawing for (the icon changed, or the popover is open).
    case tick(pressure: Pressure, used: UInt64, total: UInt64)
    /// A `quit(row:)` ended: processes and containers gone, not ours to
    /// signal, and still alive.
    case quit(row: UInt32, quit: Int, denied: Int, left: Int)
}

public enum StartResult: Equatable, Sendable {
    case ok, alreadyStarted, otherInstance, error
}

/// What the popover reads. The live core, or sample data for tests and
/// screenshots.
@MainActor
public protocol CoreSource: AnyObject {
    func snapshot() -> Snapshot?
    func series(period: Period, mode: GraphMode, width: Int) -> Series?
    func prompt(period: Period) -> String?
    func setPopoverOpen(_ open: Bool)
    /// What row `id` is made of. Nil when no row has this id.
    func parts(row id: UInt32) -> Parts?
    /// Stops row `id`'s processes and containers; `CoreEvent.quit` follows.
    /// False when no row has this id or a quit of it runs.
    func quit(row id: UInt32) -> Bool
}

extension CoreSource {
    public func parts(row id: UInt32) -> Parts? { nil }
    public func quit(row id: UInt32) -> Bool { false }
}

/// Holds the event handler for the C callback's `ctx`.
private final class Sink: @unchecked Sendable {
    let handler: @Sendable (CoreEvent) -> Void
    init(_ handler: @escaping @Sendable (CoreEvent) -> Void) { self.handler = handler }
}

private let trampoline: DlCallback = { ctx, ev in
    guard let ctx else { return }
    let sink = Unmanaged<Sink>.fromOpaque(ctx).takeUnretainedValue()
    let p = Pressure(raw: ev.pressure)
    switch Int32(ev.kind) {
    case DL_EVENT_TICK: sink.handler(.tick(pressure: p, used: ev.used, total: ev.total))
    case DL_EVENT_QUIT:
        sink.handler(.quit(row: ev.row, quit: Int(ev.quit), denied: Int(ev.denied), left: Int(ev.left)))
    default: break
    }
}

/// The Rust core, through the C ABI. One per process.
@MainActor
public final class LiveCore: CoreSource {
    public static let shared = LiveCore()

    private var sink: Unmanaged<Sink>?

    private init() {}

    /// Starts the collector and Docker threads. `handler` runs on a
    /// background thread. `dataDir` nil: ~/Library/Application Support/devload.
    public func start(dataDir: URL? = nil, handler: @escaping @Sendable (CoreEvent) -> Void) -> StartResult {
        let retained = Unmanaged.passRetained(Sink(handler))
        let code: Int32
        if let dataDir {
            code = dataDir.path.withCString { dl_start($0, trampoline, retained.toOpaque()) }
        } else {
            code = dl_start(nil, trampoline, retained.toOpaque())
        }
        switch code {
        case DL_OK:
            sink = retained
            return .ok
        case DL_ALREADY_STARTED:
            retained.release()
            return .alreadyStarted
        case DL_OTHER_INSTANCE:
            retained.release()
            return .otherInstance
        default:
            retained.release()
            return .error
        }
    }

    /// Joins the threads and writes what is in memory. Safe to call twice.
    public func stop() {
        dl_stop()
        // No callback can run after dl_stop returns.
        sink?.release()
        sink = nil
    }

    public func setPopoverOpen(_ open: Bool) { dl_set_popover_open(open) }

    public func snapshot() -> Snapshot? {
        guard let p = dl_snapshot_get() else { return nil }
        defer { dl_snapshot_free(p) }
        let s = p.pointee
        guard s.ready else { return nil }
        func rows(_ base: UnsafePointer<DlRow>?, _ n: UInt32) -> [Row] {
            guard let base, n > 0 else { return [] }
            return UnsafeBufferPointer(start: base, count: Int(n)).map {
                Row(id: $0.id, kind: Row.Kind(rawValue: $0.kind) ?? .dev,
                    name: String(cString: $0.name), detail: String(cString: $0.detail),
                    footprint: $0.footprint, cpuPermille: $0.cpu_pm, path: String(cString: $0.path))
            }
        }
        return Snapshot(
            pressure: Pressure(raw: s.pressure),
            cpuPercent: Int(s.cpu_pct),
            time: Date(timeIntervalSince1970: TimeInterval(s.ts)),
            since: Date(timeIntervalSince1970: TimeInterval(s.since)),
            used: s.used, total: s.total,
            dev: rows(s.dev, s.dev_len),
            devMore: More(count: Int(s.dev_more_count), footprint: s.dev_more_footprint),
            apps: rows(s.apps, s.apps_len),
            appsMore: More(count: Int(s.apps_more_count), footprint: s.apps_more_footprint))
    }

    public func series(period: Period, mode: GraphMode, width: Int) -> Series? {
        guard width > 0 else { return nil }
        var meta = DlSeriesMeta()
        let w = UInt32(width)
        let p = period.rawValue
        switch mode {
        case .pressure:
            var lo = [Float](repeating: 0, count: width)
            var hi = [Float](repeating: 0, count: width)
            var level = [UInt8](repeating: Series.noData, count: width)
            let ok = lo.withUnsafeMutableBufferPointer { l in
                hi.withUnsafeMutableBufferPointer { h in
                    level.withUnsafeMutableBufferPointer { v in
                        dl_series_fill(p, mode.rawValue, w, l.baseAddress, h.baseAddress, v.baseAddress, nil, nil, &meta)
                    }
                }
            }
            guard ok else { return nil }
            return Series(period: period, mode: mode, from: date(meta.from), to: date(meta.to),
                          totalMiB: Float(meta.total_mib), lo: lo, hi: hi, level: level)
        case .groups:
            var stack = [Float](repeating: 0, count: 4 * width)
            var has = [Bool](repeating: false, count: width)
            let ok = stack.withUnsafeMutableBufferPointer { s in
                has.withUnsafeMutableBufferPointer { d in
                    dl_series_fill(p, mode.rawValue, w, nil, nil, nil, s.baseAddress, d.baseAddress, &meta)
                }
            }
            guard ok else { return nil }
            var top: [String] = []
            if let names = meta.top_names {
                top = String(cString: names).split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
                dl_string_free(names)
            }
            return Series(period: period, mode: mode, from: date(meta.from), to: date(meta.to),
                          totalMiB: Float(meta.total_mib), stack: stack, hasData: has, top: top)
        }
    }

    public func parts(row id: UInt32) -> Parts? {
        guard let p = dl_parts_get(id) else { return nil }
        defer { dl_parts_free(p) }
        let s = p.pointee
        var parts: [Part] = []
        if let base = s.parts, s.len > 0 {
            parts = UnsafeBufferPointer(start: base, count: Int(s.len)).map {
                Part(kind: Part.Kind(rawValue: $0.kind) ?? .process, label: String(cString: $0.label),
                     detail: String(cString: $0.detail), count: Int($0.count), idle: Int($0.idle),
                     footprint: $0.footprint)
            }
        }
        return Parts(parts: parts, more: More(count: Int(s.more_count), footprint: s.more_footprint),
                     processes: Int(s.processes), containers: Int(s.containers))
    }

    public func quit(row id: UInt32) -> Bool { dl_quit(id) }

    public func prompt(period: Period) -> String? {
        guard let p = dl_prompt_get(period.rawValue) else { return nil }
        defer { dl_string_free(p) }
        return String(cString: p)
    }

    private func date(_ t: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(t)) }
}
