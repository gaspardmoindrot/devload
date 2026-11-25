import AppKit
import DevloadKit

let args = CommandLine.arguments

if let i = args.firstIndex(of: "--snapshot") {
    // Off-screen captures, then exit. Never starts a second collector unless --live.
    let dir = URL(fileURLWithPath: i + 1 < args.count ? args[i + 1] : "snapshots")
    MainActor.assumeIsolated {
        _ = NSApplication.shared
        let source: CoreSource
        if args.contains("--live") {
            let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("devload-snapshot-\(getpid())")
            guard LiveCore.shared.start(dataDir: tmp, handler: { _ in }) == .ok else {
                FileHandle.standardError.write(Data("could not start the core\n".utf8))
                exit(1)
            }
            let t = Date()
            while LiveCore.shared.snapshot() == nil, Date().timeIntervalSince(t) < 10 {
                RunLoop.current.run(until: Date().addingTimeInterval(0.1))
            }
            LiveCore.shared.setPopoverOpen(true)
            source = LiveCore.shared
        } else {
            source = SampleCore()
        }
        do {
            for url in try Snapshots.render(to: dir, source: source) { print(url.path) }
        } catch {
            FileHandle.standardError.write(Data("\(error)\n".utf8))
            exit(1)
        }
        if source is LiveCore { LiveCore.shared.stop() }
    }
    exit(0)
}

// Run from a terminal, macOS may hide the menu bar item with the terminal's.
Responsibility.warnIfNotOwn()

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.accessory)
    app.run()
}
