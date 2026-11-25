import Darwin
import Foundation

/// macOS 26's "Allow in the Menu Bar" can file devload's item under the app
/// macOS holds responsible for it. Run straight from a terminal (`make run`,
/// the binary inside the bundle), that is the terminal: with the terminal's
/// switch off, the item exists but is never drawn. A relaunch (disclaimed
/// spawn, or LaunchServices) did not reliably bring it back, so devload
/// only says so. The responsible pid comes from a private call (dlsym):
/// when it is missing, nothing is said.
public enum Responsibility {
    private typealias GetResponsible = @convention(c) (pid_t) -> pid_t
    private static let getResponsible: GetResponsible? = {
        guard let p = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "responsibility_get_pid_responsible_for_pid") // RTLD_DEFAULT
        else { return nil }
        return unsafeBitCast(p, to: GetResponsible.self)
    }()

    /// The process macOS holds responsible for `pid`; nil when unknown.
    public static func responsible(for pid: pid_t) -> pid_t? {
        guard let f = getResponsible else { return nil }
        let r = f(pid)
        return r > 0 ? r : nil
    }

    /// The warning for a devload owned by another process; nil when it is its own.
    public static func warning(own: pid_t, responsible: pid_t?, name: (pid_t) -> String?) -> String? {
        guard let responsible, responsible != own else { return nil }
        let who = name(responsible) ?? "pid \(responsible)"
        return "devload: macOS counts it as part of \(who); if that app's \"Allow in the Menu Bar\" "
            + "switch is off (System Settings > Menu Bar), devload's figure stays hidden. "
            + "Open devload with `open` or from Finder instead."
    }

    /// At launch: says on stderr when the menu bar item may be hidden with another app's.
    public static func warnIfNotOwn() {
        let own = getpid()
        guard let w = warning(own: own, responsible: responsible(for: own), name: appName) else { return }
        FileHandle.standardError.write(Data((w + "\n").utf8))
    }

    /// The executable path of `pid`, shortened to its .app when it has one.
    static func appName(_ pid: pid_t) -> String? {
        var buf = [UInt8](repeating: 0, count: 4 * Int(MAXPATHLEN))
        let n = proc_pidpath(pid, &buf, UInt32(buf.count))
        guard n > 0 else { return nil }
        let path = String(decoding: buf.prefix(Int(n)), as: UTF8.self)
        if let r = path.range(of: ".app/", options: .backwards) { return String(path[..<r.lowerBound]) + ".app" }
        return path
    }
}
