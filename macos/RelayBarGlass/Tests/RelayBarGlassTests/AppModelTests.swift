import Darwin
import Foundation
import Testing
@testable import RelayBarGlass

@Suite struct AppModelTests {
    private let quick = AppTiming(pollInterval: 60, requestTimeout: 0.5, commandTimeout: 0.5)

    @MainActor @Test func reportsTheDaemonAsNotRunningWhenNothingListensOnItsPort() async throws {
        let reserved = try ReservedPort()
        defer { reserved.release() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(reserved.port)

        let model = AppModel(environment: [:], home: home.url, timing: quick)
        #expect(model.relayPort == reserved.port)
        #expect(model.daemon == .checking)
        await model.poll()
        #expect(model.daemon == .down)
        #expect(model.status == nil)
        #expect(model.signedIn == false)
        #expect(model.dashboardURL?.absoluteString == "http://127.0.0.1:\(reserved.port)/")
    }

    @MainActor @Test func pollsTheDaemonAtLaunch() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)

        let model = AppModel(environment: [:], home: home.url, timing: quick)
        #expect(await eventually(within: 3) { model.daemonUp })
        #expect(model.signedIn)
    }

    @MainActor @Test func keepsPollingTheDaemonOnItsTimer() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)

        let model = AppModel(environment: [:], home: home.url, timing: AppTiming(pollInterval: 0.2, requestTimeout: 0.5, commandTimeout: 0.5))
        #expect(await eventually(within: 3) { daemon.requests >= 3 })
        _ = model
    }

    @MainActor @Test func readsTheStatusPassivelyWhileThePopoverIsClosedAndPlainlyAsSoonAsItOpens() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)

        let model = AppModel(environment: [:], home: home.url, timing: quick)
        #expect(await eventually(within: 3) { daemon.requestedPaths.count == 1 })
        await model.poll()
        #expect(daemon.requestedPaths == ["/api/status?passive=1", "/api/status?passive=1"])

        model.popoverOpened()
        #expect(await eventually(within: 3) { daemon.requestedPaths.count == 3 })
        await model.poll()
        #expect(daemon.requestedPaths.suffix(2) == ["/api/status", "/api/status"])

        model.popoverClosed()
        await model.poll()
        #expect(daemon.requestedPaths.last == "/api/status?passive=1")
    }

    @MainActor @Test func retriesASlowFirstPollBeforeCallingTheDaemonDown() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn, delays: [2])
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)

        let model = AppModel(environment: [:], home: home.url, timing: quick)
        #expect(await eventually(within: 3) { model.daemon != .checking })
        #expect(model.daemon == .up)
        #expect(daemon.requests == 2)
    }

    @MainActor @Test func followsARelayPortChangeWithoutARestart() async throws {
        let before = try StubDaemon(body: RelayStatusTests.signedOut)
        defer { before.stop() }
        let after = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { after.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(before.port)

        let model = AppModel(environment: [:], home: home.url, timing: quick)
        await model.poll()
        #expect(model.daemonUp)
        #expect(model.signedIn == false)

        try home.writeConfig("\u{FEFF}relay: {host: 127.0.0.1, port: \(after.port)}\r\n")
        await model.poll()
        #expect(model.relayPort == after.port)
        #expect(model.signedIn)
    }

    @MainActor @Test func offersNoSignOutAndNoPickersOnAStaticKeyDevice() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.staticKey)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)

        let model = AppModel(environment: [:], home: home.url, timing: quick)
        await model.poll()
        #expect(model.signedIn)
        #expect(model.offersSignOut == false)
        #expect(model.offersSelection == false)
    }

    @MainActor @Test func offersSignOutAndPickersOnASignedInSession() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)

        let model = AppModel(environment: [:], home: home.url, timing: quick)
        await model.poll()
        #expect(model.offersSignOut)
        #expect(model.offersSelection)
    }

    @MainActor @Test func showsAnErrorWhenASwitchNeverAnswers() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("exec sleep 30")

        let model = AppModel(environment: relay.environment, home: home.url, timing: quick)
        model.run(.switchTeam("team-b"))
        #expect(model.busy)
        #expect(await eventually(within: 3) { !model.busy })
        #expect(model.errorMessage(for: .switchTeam) == "The Relay daemon did not answer within 0.5 s")
        #expect(await eventually(within: 10) { relay.pids(of: "switch-team").allSatisfy { !isAlive($0) } })
    }

    @MainActor @Test func killsATimedOutCommandThatIgnoresTheTerminateSignal() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("trap '' TERM\nexec sleep 30")

        let model = AppModel(environment: relay.environment, home: home.url, timing: AppTiming(pollInterval: 60, requestTimeout: 0.5, commandTimeout: 3))
        model.run(.switchTeam("team-b"))
        try #require(await eventually(within: 3) { relay.pids(of: "switch-team").count == 1 })
        let stuck = relay.pids(of: "switch-team")[0]
        #expect(await eventually(within: 10) { !model.busy })
        #expect(model.errorMessage(for: .switchTeam) == "The Relay daemon did not answer within 3 s")
        #expect(await eventually(within: 10) { !isAlive(stuck) })
    }

    @MainActor @Test func runsASignOutClickedWhileAnotherCommandRunsAndShowsItsFailure() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("""
        case "$1" in
          switch-team) sleep 0.3 ;;
          sign-out) printf '%s' '{"refused":"static_key","message":"this device uses the static key in config.yaml"}' >&2; exit 1 ;;
        esac
        """)

        let model = AppModel(environment: relay.environment, home: home.url, timing: AppTiming(pollInterval: 60, requestTimeout: 0.5, commandTimeout: 5))
        model.run(.switchTeam("team-b"))
        model.run(.signOut)
        #expect(await eventually(within: 5) { model.errorMessage(for: .signOut) != nil })
        #expect(relay.recordedCalls == ["switch-team -- team-b", "sign-out"])
        #expect(model.errorMessage(for: .signOut) == "this device uses the static key in config.yaml")
    }

    @MainActor @Test func leavesRetryUsableWhileABrowserSignInIsPendingAndCancelsIt() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedOut)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("""
        case "$1" in
          sign-in) exec sleep 30 ;;
        esac
        """)

        let model = AppModel(environment: relay.environment, home: home.url, timing: AppTiming(pollInterval: 60, requestTimeout: 0.5, commandTimeout: 5))
        model.run(.signIn)
        #expect(model.signingIn)
        #expect(model.busy == false)
        model.run(.recheck)
        #expect(await eventually(within: 10) { relay.recordedCalls.contains("recheck") && !model.busy })

        try #require(await eventually(within: 10) { relay.pids(of: "sign-in").count == 1 })
        let pending = relay.pids(of: "sign-in")[0]
        model.cancelSignIn()
        #expect(model.signingIn == false)
        #expect(await eventually(within: 10) { !isAlive(pending) })
        #expect(model.errorMessage(for: .signIn) == nil)
    }

    @MainActor @Test func locksThePickersAndSignOutWhileABrowserSignInIsPending() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("exec sleep 30")

        let model = AppModel(environment: relay.environment, home: home.url, timing: quick)
        await model.poll()
        #expect(model.locksAccountChanges == false)
        model.run(.signIn)
        #expect(model.locksAccountChanges)
        model.cancelSignIn()
        #expect(model.locksAccountChanges == false)
    }

    @MainActor @Test func keepsTheSignedInViewThroughItsOwnSwitchWhileTheDaemonReadsSignedOut() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("sleep 1.5")

        let model = AppModel(
            environment: relay.environment,
            home: home.url,
            timing: AppTiming(pollInterval: 0.1, requestTimeout: 0.5, commandTimeout: 5)
        )
        await model.poll()
        #expect(model.signedIn)
        daemon.serve(RelayStatusTests.signedOut)
        model.run(.switchTeam("team-b"))
        #expect(await throughout(0.6) { model.signedIn && model.busy })
        #expect(await eventually(within: 5) { !model.busy })
    }

    @MainActor @Test func startsASignInClickedDuringASwitchOnceTheSwitchEnds() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedIn)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("""
        case "$1" in
          switch-team) sleep 0.3 ;;
        esac
        """)

        let model = AppModel(environment: relay.environment, home: home.url, timing: AppTiming(pollInterval: 60, requestTimeout: 0.5, commandTimeout: 5))
        model.run(.switchTeam("team-b"))
        model.run(.signIn)
        #expect(model.signingIn == false)
        #expect(await eventually(within: 5) { relay.recordedCalls.count == 2 && !model.signingIn && !model.busy })
        #expect(relay.recordedCalls == ["switch-team -- team-b", "sign-in"])
    }

    @MainActor @Test func supersedesAPendingSignInWithANewOne() async throws {
        let daemon = try StubDaemon(body: RelayStatusTests.signedOut)
        defer { daemon.stop() }
        let home = try TemporaryHome()
        defer { home.remove() }
        try home.writeRelayPort(daemon.port)
        let relay = try home.stubRelay("exec sleep 30")

        let model = AppModel(environment: relay.environment, home: home.url, timing: quick)
        model.run(.signIn)
        #expect(await eventually(within: 10) { relay.pids(of: "sign-in").count == 1 })
        model.run(.signIn)
        try #require(await eventually(within: 10) { relay.pids(of: "sign-in").count == 2 })
        let (first, second) = (relay.pids(of: "sign-in")[0], relay.pids(of: "sign-in")[1])
        #expect(await eventually(within: 10) { !isAlive(first) })
        #expect(isAlive(second))
        #expect(model.signingIn)

        model.cancelSignIn()
        #expect(await eventually(within: 10) { !isAlive(second) })
    }
}
