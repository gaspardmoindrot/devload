import Foundation

/// The values of the spec's mockup (invented project names), for tests and
/// for screenshots that can be shared.
@MainActor
public final class SampleCore: CoreSource {
    static let gib = 1_073_741_824.0
    /// Used memory (GB) over 60 minutes, from the mockup.
    static let mem: [Double] = [
        15.2, 15.4, 15.3, 15.8, 16.1, 16.0, 16.4, 16.9, 17.2, 17.0, 17.5, 17.9, 18.3, 18.8, 19.2, 19.6, 20.1, 20.8, 21.6, 22.4,
        22.8, 22.6, 21.9, 21.0, 20.2, 19.8, 19.6, 19.5, 19.7, 19.9, 19.6, 19.4, 19.5, 19.8, 20.0, 19.7, 19.5, 19.3, 19.4, 19.6,
        19.5, 19.7, 19.9, 20.1, 19.8, 19.6, 19.4, 19.5, 19.6, 19.5, 19.4, 19.6, 19.5, 19.4, 19.5, 19.6, 19.5, 19.4, 19.5, 19.46,
    ]

    public let now: Date
    public var pressure: Pressure = .tight
    public private(set) var popoverOpen = false
    public private(set) var prompts: [Period] = []
    /// Rows `quit(row:)` was asked for.
    public private(set) var quits: [UInt32] = []
    /// Rows left out of the snapshot, as if quit.
    public var gone: Set<UInt32> = []
    /// Added to the snapshot time, as if ticks went by.
    public var elapsed: TimeInterval = 0

    /// `now` defaults to Tue 24 Nov 2026 14:32 in the local time zone.
    public init(now: Date? = nil) {
        if let now {
            self.now = now
        } else {
            var c = DateComponents(year: 2026, month: 11, day: 24, hour: 14, minute: 32)
            c.calendar = Calendar.current
            self.now = c.date!
        }
    }

    static func gb(_ g: Double) -> UInt64 { UInt64(g * gib) }

    public func snapshot() -> Snapshot? {
        Snapshot(
            pressure: pressure, cpuPercent: 11, time: now.addingTimeInterval(elapsed),
            since: now.addingTimeInterval(-(3 * 3600 + 47 * 60)),
            used: Self.gb(19.461), total: Self.gb(24),
            dev: Self.dev.filter { !gone.contains($0.id) }, devMore: .none,
            apps: Self.apps.filter { !gone.contains($0.id) }, appsMore: .none)
    }

    static let dev: [Row] = [
        Row(id: 1, name: "feat/checkout-v2", detail: "3 idle Claude sessions, terminal", footprint: gb(2.401),
            path: "/Users/sample/src/shop/feat-checkout-v2"),
        Row(id: 2, name: "api-refactor", detail: "postgres, redis", footprint: gb(0.881),
            path: "/Users/sample/src/api-refactor"),
        Row(id: 3, name: "landing-page", detail: "Codex session, terminal", footprint: gb(0.751),
            path: "/Users/sample/src/landing-page"),
        Row(id: 4, name: "docs-site", detail: "terminal", footprint: gb(0.421), path: "/Users/sample/src/docs-site"),
        Row(id: 5, name: "fix/login-redirect", detail: "Claude session", footprint: gb(0.311),
            path: "/Users/sample/src/shop/fix-login-redirect"),
    ]

    static let apps: [Row] = ([
        ("Google Chrome", 3.841, "23 processes"), ("Orca", 1.661, "17 processes"),
        ("Docker Desktop", 1.561, "app + engine"), ("Slack", 1.141, "8 processes"),
    ] + smallApps.map { ($0.0, $0.1, $0.2) }).enumerated().map { i, a in
        Row(id: UInt32(10 + i), kind: .app, name: a.0, detail: a.2, footprint: gb(a.1),
            path: "/Applications/\(a.0).app")
    }

    public func parts(row id: UInt32) -> Parts? {
        guard let r = (Self.dev + Self.apps).first(where: { $0.id == id }), !gone.contains(id) else { return nil }
        let g = Self.gb
        switch r.name {
        case "feat/checkout-v2":
            return Parts(parts: [
                Part(kind: .agent, label: "Claude sessions", count: 3, idle: 3, footprint: g(1.621)),
                Part(kind: .process, label: "node", count: 4, footprint: g(0.551)),
                Part(kind: .process, label: "zsh", count: 2, footprint: g(0.229)),
            ], processes: 9)
        case "api-refactor":
            return Parts(parts: [
                Part(kind: .docker, label: "api-refactor", detail: "postgres, redis", count: 2, footprint: g(0.712)),
                Part(kind: .process, label: "node", footprint: g(0.169)),
            ], processes: 1, containers: 2)
        case "landing-page":
            return Parts(parts: [
                Part(kind: .agent, label: "Codex session", count: 1, footprint: g(0.521)),
                Part(kind: .process, label: "zsh", count: 1, footprint: g(0.230)),
            ], processes: 3)
        case "docs-site":
            return Parts(parts: [
                Part(kind: .process, label: "node", count: 2, footprint: g(0.311)),
                Part(kind: .process, label: "zsh", count: 1, footprint: g(0.110)),
            ], processes: 3)
        case "fix/login-redirect":
            return Parts(parts: [Part(kind: .agent, label: "Claude session", count: 1, footprint: g(0.311))], processes: 1)
        case "Google Chrome":
            return Parts(parts: [
                Part(kind: .process, label: "Google Chrome Helper (Renderer)", count: 18, footprint: g(2.902)),
                Part(kind: .process, label: "Google Chrome", count: 1, footprint: g(0.622)),
                Part(kind: .process, label: "Google Chrome Helper (GPU)", count: 1, footprint: g(0.211)),
                Part(kind: .process, label: "Google Chrome Helper", count: 3, footprint: g(0.106)),
            ], processes: 23)
        case "Docker Desktop":
            return Parts(parts: [
                Part(kind: .process, label: "Linux VM", count: 1, footprint: g(1.204)),
                Part(kind: .process, label: "com.docker.backend", count: 2, footprint: g(0.357)),
            ], processes: 3)
        default:
            let n = Int(r.detail.split(separator: " ").first ?? "1") ?? 1
            return Parts(parts: [Part(kind: .process, label: r.name, count: n, footprint: r.footprint)], processes: n)
        }
    }

    public func quit(row id: UInt32) -> Bool {
        guard parts(row: id) != nil else { return false }
        quits.append(id)
        return true
    }

    /// The 14 apps behind the mockup's "+14 more" (2.12 GB).
    static let smallApps: [(String, Double, String)] = [
        ("Figma", 0.41, "4 processes"), ("Spotify", 0.28, "3 processes"), ("Notion", 0.24, "5 processes"),
        ("Xcode", 0.21, "2 processes"), ("Mail", 0.17, "2 processes"), ("Safari", 0.15, "4 processes"),
        ("Messages", 0.12, "2 processes"), ("Finder", 0.11, "1 process"), ("Calendar", 0.09, "1 process"),
        ("Music", 0.08, "1 process"), ("Preview", 0.08, "1 process"), ("Terminal", 0.07, "1 process"),
        ("Photos", 0.06, "2 processes"), ("Notes", 0.05, "1 process"),
    ]

    public func series(period: Period, mode: GraphMode, width: Int) -> Series? {
        guard width > 0 else { return nil }
        let span: TimeInterval = [3600, 6 * 3600, 86_400, 7 * 86_400][Int(period.rawValue)]
        let from = now.addingTimeInterval(-span)
        let at = { (i: Int) in Self.mem[i * Self.mem.count / width] }
        switch mode {
        case .pressure:
            var lo = [Float](), hi = [Float](), level = [UInt8]()
            for i in 0..<width {
                let v = at(i)
                hi.append(Float(v * 1024))
                lo.append(Float((v - 0.15) * 1024))
                level.append(v >= 22 ? 2 : v >= 19 ? 1 : 0)
            }
            return Series(period: period, mode: mode, from: from, to: now, totalMiB: 24 * 1024, lo: lo, hi: hi, level: level)
        case .groups:
            var stack = [Float]()
            for i in 0..<width {
                let v = at(i)
                let a = min(max((v - 15) * 0.6 + 0.8, 0.8), 4.5), b = 3.6 + 0.004 * Double(i) / 5, c = 1.6
                stack += [a, b, c, v - a - b - c].map { Float($0 * 1024) }
            }
            return Series(period: period, mode: mode, from: from, to: now, totalMiB: 24 * 1024, stack: stack,
                          hasData: .init(repeating: true, count: width),
                          top: ["feat/checkout-v2", "Google Chrome", "Docker Desktop"])
        }
    }

    public func prompt(period: Period) -> String? {
        prompts.append(period)
        return "Sample prompt for \(period.label)"
    }

    public func setPopoverOpen(_ open: Bool) { popoverOpen = open }
}

/// A login item that only remembers, for tests and screenshots.
@MainActor
public final class SampleLoginItem: LoginItemControlling {
    public var status: LoginStatus
    public var next: LoginStatus?

    public init(_ status: LoginStatus = .disabled) { self.status = status }

    public func setEnabled(_ on: Bool) throws {
        status = next ?? (on ? .enabled : .disabled)
    }
}
