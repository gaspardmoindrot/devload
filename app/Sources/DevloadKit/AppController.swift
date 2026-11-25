import AppKit
import SwiftUI

/// The menu bar item and its popover. The popover's view tree is built when
/// it opens and released when it closes; while closed, the app only redraws
/// the icon when the core says its figure changed.
@MainActor
public final class AppController: NSObject, NSPopoverDelegate {
    private let core: LiveCore
    private let loginItem = LoginItem()
    /// Outlives each popover: a quit started in one ends in the next.
    private let quits = QuitState(apps: AppQuitter())
    /// Made once the core starts: a second launch shows no "-- GB" while
    /// it says devload already runs.
    private var statusItem: NSStatusItem?
    private var popover: NSPopover?
    private var model: PopoverModel?
    private var shown: (text: String, pressure: Pressure)?
    private let trace = ProcessInfo.processInfo.environment["DEVLOAD_TRACE"] != nil
    private var clickedAt: UInt64?

    public init(core: LiveCore = .shared) {
        self.core = core
        super.init()
    }

    private func makeStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem = item
        guard let button = item.button else { return }
        button.image = IconRenderer.placeholder()
        button.imagePosition = .imageOnly
        button.target = self
        button.action = #selector(toggle)
        button.sendAction(on: [.leftMouseDown, .rightMouseDown])
        button.setAccessibilityLabel("devload")
    }

    /// Starts the core. `DEVLOAD_DATA_DIR` overrides the data folder
    /// (~/Library/Application Support/devload), for development.
    public func start() -> StartResult {
        let dir = ProcessInfo.processInfo.environment["DEVLOAD_DATA_DIR"].map { URL(fileURLWithPath: $0) }
        let result = core.start(dataDir: dir) { [weak self] event in
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.handle(event) }
            }
        }
        if result == .ok {
            makeStatusItem()
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
                MainActor.assumeIsolated { self?.checkItemShown() }
            }
        }
        return result
    }

    public func stop() { core.stop() }

    func handle(_ event: CoreEvent) {
        switch event {
        case let .tick(pressure, used, total):
            let text = Format.icon(used: used)
            if shown?.text != text || shown?.pressure != pressure {
                shown = (text, pressure)
                statusItem?.button?.image = IconRenderer.image(pressure: pressure, used: used, total: total)
            }
            model?.refresh()
        case let .quit(row, quit, denied, left):
            quits.ended(row: row, quit: quit, denied: denied, left: left)
            model?.refresh()
        }
    }

    // MARK: Popover

    @objc private func toggle() {
        if let popover, popover.isShown {
            popover.performClose(nil)
            return
        }
        clickedAt = DispatchTime.now().uptimeNanoseconds
        open()
    }

    private func open() {
        guard let button = statusItem?.button else { return }
        core.setPopoverOpen(true)
        let model = PopoverModel(source: core, loginItem: loginItem, quits: quits)
        model.refresh()
        self.model = model
        let view = PopoverView(model: model, icons: AppIcons(), onGraphDraw: { [weak self] in self?.graphDrawn() })
        // The macOS 26 glass alone lets the wallpaper through the text: a
        // window-coloured layer over it keeps the figures readable.
        let host = NSHostingController(rootView: view.background(
            Color(nsColor: .windowBackgroundColor).opacity(0.8).ignoresSafeArea()))
        host.sizingOptions = .preferredContentSize
        let popover = self.popover ?? makePopover()
        popover.contentViewController = host
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        popover.contentViewController?.view.window?.makeKey()
    }

    /// One NSPopover for the app's lifetime. On macOS 26 each new NSPopover
    /// leaves its glass background (about 6 MB of IOSurface) behind after it
    /// closes, so only the content is built and released per open.
    private func makePopover() -> NSPopover {
        let popover = NSPopover()
        popover.behavior = .transient
        popover.animates = false
        popover.delegate = self
        // No arrow under the menu bar figure, like the system's own menu
        // bar popovers. Not public API: asked only if this macOS has it.
        let hide = NSSelectorFromString("setShouldHideAnchor:")
        if popover.responds(to: hide) {
            popover.setValue(true, forKey: "shouldHideAnchor")
        }
        self.popover = popover
        return popover
    }

    public func popoverDidClose(_ notification: Notification) {
        core.setPopoverOpen(false)
        popover?.contentViewController = nil
        model = nil
    }

    /// Says on stderr when macOS does not draw the item: one hidden by
    /// "Allow in the Menu Bar" still exists, but its window is not visible.
    private func checkItemShown() {
        guard let w = statusItem?.button?.window else { return }
        let shown = w.occlusionState.contains(.visible)
        guard trace || !shown else { return }
        let line = "devload: menu bar item \(shown ? "shown" : "hidden by macOS") at \(NSStringFromRect(w.frame))\n"
        FileHandle.standardError.write(Data(line.utf8))
    }

    private func graphDrawn() {
        guard trace, let t = clickedAt else { return }
        clickedAt = nil
        let ms = Double(DispatchTime.now().uptimeNanoseconds - t) / 1e6
        FileHandle.standardError.write(Data(String(format: "devload: click -> graph drawn %.1f ms\n", ms).utf8))
    }
}
