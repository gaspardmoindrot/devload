import Foundation
import Synchronization
import Testing
@testable import DevloadKit

/// The real Rust core through the C ABI, on this Mac. One test: the core
/// is a process-wide singleton.
@MainActor
@Test(.timeLimit(.minutes(1)))
func liveCoreEndToEnd() async throws {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent("devload-swift-\(getpid())")
    try? FileManager.default.removeItem(at: dir)
    let core = LiveCore.shared
    #expect(core.snapshot() == nil, "nothing before start")

    let events = Mutex<[CoreEvent]>([])
    #expect(core.start(dataDir: dir) { ev in events.withLock { $0.append(ev) } } == .ok)
    #expect(core.start(dataDir: dir) { _ in } == .alreadyStarted)

    let t0 = Date()
    while events.withLock({ $0.isEmpty }) {
        try #require(Date().timeIntervalSince(t0) < 15, "first tick")
        try await Task.sleep(for: .milliseconds(50))
    }
    guard case let .tick(_, used, total) = events.withLock({ $0[0] }) else {
        Issue.record("first event is a tick")
        return
    }
    #expect(total > 4 << 30 && used > 0 && used <= total)

    let s = try #require(core.snapshot())
    #expect(s.total == total)
    #expect(!s.apps.isEmpty && s.apps.count <= 50 && s.dev.count <= 50)
    #expect(s.apps.allSatisfy { !$0.name.isEmpty })
    #expect(zip(s.apps, s.apps.dropFirst()).allSatisfy { $0.footprint >= $1.footprint })
    #expect(s.since <= s.time)

    core.setPopoverOpen(true)
    for period in Period.allCases {
        let p = try #require(core.series(period: period, mode: .pressure, width: 300))
        #expect(p.width == 300 && p.lo.count == 300 && p.hi.count == 300)
        #expect(p.level[299] <= 2, "\(period): the last column has data")
        #expect(p.to.timeIntervalSince(p.from) > 0)
        #expect(p.totalMiB == Float(total >> 20))

        let g = try #require(core.series(period: period, mode: .groups, width: 300))
        #expect(g.width == 300 && g.stack.count == 1200 && g.hasData[299])
        #expect((1...3).contains(g.top.count) && !g.top.contains("System"))
    }
    #expect(core.series(period: .h1, mode: .pressure, width: 0) == nil)
    core.setPopoverOpen(false)

    let prompt = try #require(core.prompt(period: .h6))
    #expect(prompt.contains(dir.appendingPathComponent("devload.sqlite").path))

    core.stop()
    #expect(core.snapshot() == nil)
    core.stop()
    try? FileManager.default.removeItem(at: dir)
}
