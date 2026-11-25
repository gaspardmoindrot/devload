import Darwin
import Testing
@testable import DevloadKit

@Test func ownResponsibleProcessSaysNothing() {
    #expect(Responsibility.warning(own: 10, responsible: 10, name: { _ in "x" }) == nil)
    #expect(Responsibility.warning(own: 10, responsible: nil, name: { _ in "x" }) == nil,
            "private call missing: nothing to go on")
}

@Test func runFromATerminalNamesIt() {
    let w = Responsibility.warning(own: 10, responsible: 3, name: { $0 == 3 ? "/Applications/Ghostty.app" : nil })
    #expect(w?.contains("part of /Applications/Ghostty.app") == true)
    #expect(Responsibility.warning(own: 10, responsible: 4, name: { _ in nil })?.contains("part of pid 4") == true)
}

@Test func theResponsibleProcessIsKnown() {
    // The test runner is started by a terminal or an IDE: some pid, never 0.
    #expect((Responsibility.responsible(for: getpid()) ?? 0) > 0, "private call missing on this macOS")
}

@Test func appNamesStopAtTheBundle() {
    // launchd has no bundle: its plain path.
    #expect(Responsibility.appName(1) == nil || Responsibility.appName(1) == "/sbin/launchd")
    #expect(Responsibility.appName(getpid()).map { !$0.contains(".app/") } == true)
}
