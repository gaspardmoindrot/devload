import AppKit

/// Quits a row of the Apps list the way the Dock does: the app gets to
/// ask about unsaved work. Force quit is a second, explicit step, and only
/// ever reaches the instances that were asked, by pid.
@MainActor
public protocol AppQuitting: AnyObject {
    /// Asks the apps run from the bundle at `path` to quit; their pids,
    /// empty if none runs.
    func quit(path: String) -> [pid_t]
    /// Whether one of these still runs.
    func isRunning(_ pids: [pid_t]) -> Bool
    func forceQuit(_ pids: [pid_t])
}

@MainActor
public final class AppQuitter: AppQuitting {
    /// The apps asked to quit, by pid. A pid alone could name another
    /// process once its app is gone; these objects never do.
    private var asked: [pid_t: NSRunningApplication] = [:]

    public init() {}

    public func quit(path: String) -> [pid_t] {
        asked = asked.filter { !$0.value.isTerminated }
        let apps = Self.running(path)
        for a in apps {
            asked[a.processIdentifier] = a
            a.terminate()
        }
        return apps.map(\.processIdentifier)
    }

    /// Only ever true for an app this quitter asked, still the same one.
    public func isRunning(_ pids: [pid_t]) -> Bool { pids.contains { live($0) != nil } }

    public func forceQuit(_ pids: [pid_t]) { pids.forEach { live($0)?.forceTerminate() } }

    /// The apps run from `path`, devload aside.
    static func running(_ path: String, own: pid_t = getpid()) -> [NSRunningApplication] {
        let want = URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
        return NSWorkspace.shared.runningApplications.filter {
            $0.bundleURL?.resolvingSymlinksInPath().standardizedFileURL.path == want && $0.processIdentifier != own
        }
    }

    /// The app asked under `pid`, if it still runs: whatever runs under
    /// that pid now must be the same bundle, launched at the same time.
    private func live(_ pid: pid_t) -> NSRunningApplication? {
        guard pid != getpid(), let a = asked[pid], !a.isTerminated,
              let now = NSRunningApplication(processIdentifier: pid), !now.isTerminated,
              now.bundleURL == a.bundleURL, now.launchDate == a.launchDate else { return nil }
        return a
    }
}

/// An app quitter that only remembers, for tests and screenshots.
@MainActor
public final class SampleAppQuitter: AppQuitting {
    public private(set) var asked: [String] = []
    public private(set) var forced: [pid_t] = []
    /// Paths whose apps refuse to quit (an unsaved document, a hung app).
    public var stubborn: Set<String> = []
    /// Paths whose apps ignore even Force Quit.
    public var unkillable: Set<String> = []
    /// Running apps: path → pid.
    private var running: [String: pid_t] = [:]
    private var nextPid: pid_t = 100

    public init(running: Set<String> = []) {
        for p in running.sorted() { open(p) }
    }

    /// Opens (or reopens) the app at `path`, under a new pid.
    @discardableResult public func open(_ path: String) -> pid_t {
        nextPid += 1
        running[path] = nextPid
        return nextPid
    }

    public func quit(path: String) -> [pid_t] {
        asked.append(path)
        guard let pid = running[path] else { return [] }
        if !stubborn.contains(path) { running[path] = nil }
        return [pid]
    }

    public func isRunning(_ pids: [pid_t]) -> Bool { running.values.contains { pids.contains($0) } }

    public func forceQuit(_ pids: [pid_t]) {
        forced += pids
        for (path, pid) in running where pids.contains(pid) && !unkillable.contains(path) { running[path] = nil }
    }
}
