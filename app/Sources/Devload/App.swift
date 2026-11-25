import AppKit
import DevloadKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var controller: AppController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let c = AppController()
        switch c.start() {
        case .ok:
            controller = c
        case .otherInstance, .alreadyStarted:
            let alert = NSAlert()
            alert.messageText = "devload is already running"
            alert.informativeText = "Look for its figure in the menu bar."
            Self.runInFront(alert)
            NSApp.terminate(nil)
        case .error:
            let alert = NSAlert()
            alert.messageText = "devload could not start"
            alert.informativeText = "It needs to write to ~/Library/Application Support/devload."
            Self.runInFront(alert)
            NSApp.terminate(nil)
        }
    }

    /// A menu-bar app is never frontmost by itself, and macOS's cooperative
    /// activation lets the terminal or Finder that launched it keep the
    /// focus: the alert asks for it and floats above other windows anyway.
    private static func runInFront(_ alert: NSAlert) {
        NSApp.activate(ignoringOtherApps: true)
        alert.window.level = .floating
        alert.runModal()
    }

    func applicationWillTerminate(_ notification: Notification) {
        // Writes the minutes still in memory, so the agent sees them.
        controller?.stop()
    }
}
