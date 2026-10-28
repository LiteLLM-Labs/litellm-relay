import AppKit
import Foundation
import Testing
@testable import RelayBarGlass

@Suite struct AppLifecycleTests {
    private let earlier = RunningCopy(pid: 900, startedAt: Date(timeIntervalSince1970: 1_800_000_000))
    private let later = RunningCopy(pid: 400, startedAt: Date(timeIntervalSince1970: 1_800_000_060))

    @Test func aLaterCopyYieldsToTheCopyThatStartedFirst() {
        #expect(SingleInstance.olderCopy(of: later, among: [earlier, later]) == earlier)
        #expect(SingleInstance.olderCopy(of: earlier, among: [earlier, later]) == nil)
        #expect(SingleInstance.olderCopy(of: earlier, among: [earlier]) == nil)
    }

    @Test func copiesStartedInTheSameInstantLeaveExactlyOneRunning() {
        let first = RunningCopy(pid: 500, startedAt: earlier.startedAt)
        let second = RunningCopy(pid: 501, startedAt: earlier.startedAt)
        let copies = [first, second]
        let yielding = copies.filter { SingleInstance.olderCopy(of: $0, among: copies) != nil }
        #expect(yielding == [second])
    }

    @Test func readsTheStartTimeOfRealProcesses() throws {
        let own = try #require(SingleInstance.runningCopy(getpid()))
        let child = Process()
        child.executableURL = URL(fileURLWithPath: "/bin/sleep")
        child.arguments = ["5"]
        try child.run()
        defer { child.terminate() }
        let started = try #require(SingleInstance.runningCopy(child.processIdentifier))
        let booted = Date().addingTimeInterval(-ProcessInfo.processInfo.systemUptime - 1)
        #expect(own.startedAt > booted)
        #expect(started.startedAt > Date().addingTimeInterval(-60))
        #expect(started.startedAt <= Date())
        #expect(started.startedAt >= own.startedAt)
        #expect(SingleInstance.olderCopy(of: started, among: [own, started]) == own)
        #expect(SingleInstance.olderCopy(of: own, among: [own, started]) == nil)
    }

    @Test func aRunWithoutABundleIdentifierNeverQuitsItself() {
        #expect(SingleInstance.olderCopy(bundleIdentifier: nil) == nil)
        #expect(SingleInstance.olderCopy(bundleIdentifier: "ai.litellm.relaybar.tests.nobody") == nil)
    }

    @Test func onlyAQuitNobodyAskedForExitsNonZero() {
        #expect(TerminationPolicy.exitStatus(for: .userQuit) == 0)
        #expect(TerminationPolicy.exitStatus(for: .systemPowerOff) == 0)
        #expect(TerminationPolicy.exitStatus(for: .unrequested) != 0)
    }

    @MainActor @Test func theQuitButtonEndsTheProcessWithStatusZero() throws {
        let reserved = try ReservedPort()
        defer { reserved.release() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(reserved.port)
        let intent = QuitIntent()
        var terminations = 0
        var statuses: [Int32] = []
        let delegate = RelayBarDelegate(intent: intent, workspaceCenter: NotificationCenter()) { statuses.append($0) }
        let model = AppModel(
            environment: [:],
            home: home.url,
            timing: AppTiming(pollInterval: 60, requestTimeout: 0.5, commandTimeout: 0.5),
            quitIntent: intent,
            terminate: { terminations += 1 }
        )

        model.quit()
        delegate.applicationWillTerminate(Notification(name: NSApplication.willTerminateNotification))

        #expect(terminations == 1)
        #expect(statuses == [0])
    }

    @MainActor @Test func aTerminateNobodyAskedForExitsNonZeroSoLaunchdRestartsTheTray() {
        var statuses: [Int32] = []
        let delegate = RelayBarDelegate(intent: QuitIntent(), workspaceCenter: NotificationCenter()) { statuses.append($0) }

        delegate.applicationWillTerminate(Notification(name: NSApplication.willTerminateNotification))

        #expect(statuses.count == 1)
        #expect(statuses.first != 0)
    }

    @MainActor @Test func aLogoutRestartOrShutdownExitsWithStatusZero() {
        let workspace = NotificationCenter()
        var statuses: [Int32] = []
        let delegate = RelayBarDelegate(intent: QuitIntent(), workspaceCenter: workspace) { statuses.append($0) }

        workspace.post(name: NSWorkspace.willPowerOffNotification, object: nil)
        delegate.applicationWillTerminate(Notification(name: NSApplication.willTerminateNotification))

        #expect(statuses == [0])
    }
}
