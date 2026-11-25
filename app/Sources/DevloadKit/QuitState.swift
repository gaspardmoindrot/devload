import Foundation
import Observation

/// Quits in progress and what they left behind. The app controller keeps
/// it, so a quit outlives the popover that started it: reopened, the row is
/// still grey, then says how it ended.
@MainActor
@Observable
public final class QuitState {
    /// Rows being quit: greyed, with a spinner.
    public private(set) var quitting: Set<UInt32> = []
    /// Apps that did not quit when asked: they offer Force Quit.
    public private(set) var stubborn: Set<UInt32> = []
    /// What a quit left behind, said under its row.
    public private(set) var notes: [UInt32: String] = [:]

    @ObservationIgnored private let apps: AppQuitting
    @ObservationIgnored private let checkAfter: Duration
    /// Rows whose quit ended, and the snapshot time then: they stay greyed
    /// until a newer snapshot shows them gone, or still there.
    @ObservationIgnored private var settling: [UInt32: Date] = [:]
    /// The apps asked to quit, by row. Checks and Force Quit only ever
    /// touch these, never an instance opened since.
    @ObservationIgnored private var asked: [UInt32: [pid_t]] = [:]
    /// Time of the last snapshot seen.
    @ObservationIgnored private var seen: Date = .distantPast

    /// The default `apps` only pretends, so tests never quit a real app.
    public init(apps: AppQuitting = SampleAppQuitter(), checkAfter: Duration = .seconds(5)) {
        self.apps = apps
        self.checkAfter = checkAfter
    }

    /// A new snapshot: rows gone forget their state; settled rows lose
    /// their grey once a newer snapshot shows them. A stubborn app quit
    /// since (maybe reopened under the same row) loses its warning.
    func update(rows ids: Set<UInt32>, time: Date) {
        seen = time
        stubborn.formIntersection(ids)
        for id in stubborn where !(asked[id].map(apps.isRunning) ?? false) { forget(id) }
        for (id, at) in settling where time > at || !ids.contains(id) {
            settling[id] = nil
            quitting.remove(id)
        }
        quitting.formIntersection(ids)
        notes = notes.filter { ids.contains($0.key) }
        asked = asked.filter { ids.contains($0.key) }
    }

    func clearNote(_ id: UInt32) { notes[id] = nil }

    /// An app, the way the Dock quits it. False if none of it runs.
    func quitApp(_ id: UInt32, path: String, name: String) -> Bool {
        let pids = apps.quit(path: path)
        guard !pids.isEmpty else { return false }
        notes[id] = nil
        asked[id] = pids
        quitting.insert(id)
        check(id, name: name, forced: false)
        return true
    }

    /// A row the core quits (`dl_quit` returned true).
    func quitByCore(_ id: UInt32) {
        notes[id] = nil
        quitting.insert(id)
    }

    func say(_ id: UInt32, _ note: String?) { notes[id] = note }

    /// The core's answer to `dl_quit`; ignored for a row not being quit.
    func ended(row id: UInt32, quit: Int, denied: Int, left: Int) {
        guard quitting.contains(id) else { return }
        if let note = Format.quitResult(quit: quit, denied: denied, left: left) {
            quitting.remove(id)
            notes[id] = note
        } else {
            settle(id)
        }
    }

    func forceQuit(_ id: UInt32, name: String) {
        guard stubborn.contains(id), let pids = asked[id] else { return }
        // Quit since it was asked: nothing left to force.
        guard apps.isRunning(pids) else { return forget(id) }
        stubborn.remove(id)
        notes[id] = nil
        apps.forceQuit(pids)
        quitting.insert(id)
        check(id, name: name, forced: true)
    }

    /// An app may ask about unsaved work, or hang: still running after a
    /// while, it offers Force Quit. Force quit, it is said not to have quit.
    private func check(_ id: UInt32, name: String, forced: Bool) {
        Task { [weak self, checkAfter] in
            try? await Task.sleep(for: forced ? checkAfter / 5 : checkAfter)
            guard let self, quitting.contains(id) else { return }
            if let pids = asked[id], apps.isRunning(pids) {
                quitting.remove(id)
                stubborn.insert(id)
                notes[id] = forced ? Format.couldNotQuit(name) : Format.stillOpen(name)
            } else {
                settle(id)
            }
        }
    }

    /// A stubborn app that quit after all: no warning, no Force Quit.
    private func forget(_ id: UInt32) {
        stubborn.remove(id)
        notes[id] = nil
        asked[id] = nil
    }

    /// Keeps the row greyed until the next snapshot.
    private func settle(_ id: UInt32) {
        settling[id] = seen
    }
}
