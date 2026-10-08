import Foundation
import Testing
@testable import RelayBarGlass

@Suite struct StatusFormatterTests {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    @Test func countdownFormats() {
        #expect(StatusFormatter.countdown(27 * 60) == "27m")
        #expect(StatusFormatter.countdown(27 * 60 + 59) == "27m")
        #expect(StatusFormatter.countdown(3600 + 3 * 60) == "1h 03m")
        #expect(StatusFormatter.countdown(2 * 3600 + 45 * 60) == "2h 45m")
        #expect(StatusFormatter.countdown(45) == "45s")
        #expect(StatusFormatter.countdown(0) == "expired")
        #expect(StatusFormatter.countdown(-5) == "expired")
    }

    @Test func keyLineCountsDownAMintedKey() {
        let expiry = now.addingTimeInterval(27 * 60 + 10)
        #expect(StatusFormatter.keyLine(source: "minted_key", keyExpiresAt: expiry, now: now) == "Key valid 27m, renews itself")
        #expect(StatusFormatter.keyLine(source: "minted_key", keyExpiresAt: now, now: now) == "Key expired, renews on the next request")
        #expect(StatusFormatter.keyLine(source: "minted_key", keyExpiresAt: nil, now: now) == "Key minted, renews itself")
    }

    @Test func keyLineNamesTheOtherSources() {
        #expect(StatusFormatter.keyLine(source: "session_credential", keyExpiresAt: nil, now: now) == "Session credential")
        #expect(StatusFormatter.keyLine(source: "identity_token", keyExpiresAt: nil, now: now) == "Session credential")
        #expect(StatusFormatter.keyLine(source: nil, keyExpiresAt: nil, now: now) == "Session credential")
        #expect(StatusFormatter.keyLine(source: "static_key", keyExpiresAt: nil, now: now) == "Static key from config.yaml")
    }

    @Test func timestampReadsTheDaemonAndGatewayShapes() {
        #expect(StatusFormatter.timestamp("2027-01-15T08:00:00Z")?.timeIntervalSince1970 == 1_800_000_000)
        #expect(StatusFormatter.timestamp("2027-01-15T08:00:00+00:00") == StatusFormatter.timestamp("2027-01-15T08:00:00Z"))
        #expect(StatusFormatter.timestamp("2027-01-15T08:00:00.123Z") != nil)
        #expect(StatusFormatter.timestamp(nil) == nil)
        #expect(StatusFormatter.timestamp("soon") == nil)
    }

    @Test func budgetLineShowsSpendAgainstTheCap() {
        let team = TeamBudget(id: "team-a", alias: "Team A", spend: 12.4, maxBudget: 50, budgetResetAt: "2026-11-01T00:00:00+00:00")
        #expect(StatusFormatter.budgetLine(team: team, error: nil) == "$12.40 of $50.00, resets 1 Nov")
    }

    @Test func budgetLineWithoutACapOrResetDate() {
        let uncapped = TeamBudget(id: "team-b", alias: nil, spend: 12.4, maxBudget: nil, budgetResetAt: nil)
        #expect(StatusFormatter.budgetLine(team: uncapped, error: nil) == "$12.40, no budget")
        let noReset = TeamBudget(id: "team-b", alias: nil, spend: 0.000062, maxBudget: 20, budgetResetAt: nil)
        #expect(StatusFormatter.budgetLine(team: noReset, error: nil) == "$0.00 of $20.00")
    }

    @Test func budgetLineShowsTheErrorOrTheUnreadState() {
        let team = TeamBudget(id: "team-a", alias: nil, spend: 1, maxBudget: 2, budgetResetAt: nil)
        #expect(StatusFormatter.budgetLine(team: team, error: "HTTP 403: not authorized") == "HTTP 403: not authorized")
        let unread = TeamBudget(id: "relay-qa", alias: nil, spend: nil, maxBudget: nil, budgetResetAt: nil)
        #expect(StatusFormatter.budgetLine(team: unread, error: nil) == "Budget not read yet")
        #expect(StatusFormatter.budgetLine(team: nil, error: nil) == "Budget not read yet")
    }

    @Test func displayNameFallsBackFromNameToEmailToId() {
        let account = AccountStatus(user: AccountUser(id: "user-1", email: "dev@example.com"), teams: nil, teamsError: nil, team: nil, budgetError: nil, gateway: nil, polledAt: nil)
        #expect(StatusFormatter.displayName(broker: broker(displayName: "Dev Eloper", userId: "user-1"), account: account) == "Dev Eloper")
        #expect(StatusFormatter.displayName(broker: broker(displayName: nil, userId: "user-1"), account: account) == "dev@example.com")
        #expect(StatusFormatter.displayName(broker: broker(displayName: "", userId: "user-1"), account: account) == "dev@example.com")
        #expect(StatusFormatter.displayName(broker: broker(displayName: nil, userId: "user-1"), account: nil) == "user-1")
        #expect(StatusFormatter.displayName(broker: nil, account: nil) == "Signed in")
    }

    @Test func teamLabelShowsAliasAndId() {
        #expect(StatusFormatter.teamLabel(TeamEntry(id: "team-a", alias: "Team A")) == "Team A (team-a)")
        #expect(StatusFormatter.teamLabel(TeamEntry(id: "team-b", alias: nil)) == "team-b")
        #expect(StatusFormatter.teamLabel(TeamEntry(id: "team-c", alias: "")) == "team-c")
    }

    @Test func relayPortComesFromTheRelaySectionOnly() {
        let config = """
        gateway:
          url: https://gateway.example.com
          port: 9999
        relay:
          host: 127.0.0.1
          port: 49290  # a pilot port
        idp:
          issuer: https://login.example.com
        """
        #expect(StatusFormatter.relayPort(configYAML: config) == 49290)
        #expect(StatusFormatter.relayPort(configYAML: "gateway:\n  port: 9999\n") == 4142)
        #expect(StatusFormatter.relayPort(configYAML: "") == 4142)
        #expect(StatusFormatter.relayPort(configYAML: "relay:\n  port: \"4143\"\n") == 4143)
        #expect(StatusFormatter.relayPort(configYAML: "relay:\n  port: many\n") == 4142)
    }

    @Test func gatewayHost() {
        #expect(StatusFormatter.gatewayHost("https://gateway.example.com/") == "gateway.example.com")
        #expect(StatusFormatter.gatewayHost("http://127.0.0.1:59621") == "127.0.0.1")
        #expect(StatusFormatter.gatewayHost(nil) == nil)
        #expect(StatusFormatter.gatewayHost("") == nil)
    }

    @Test func failureMessagePrefersTheJsonMessage() {
        #expect(
            StatusFormatter.failureMessage(stderr: #"{"refused":"unknown_team","message":"no team \"team-z\" for this user"}"#, exitCode: 1)
                == "no team \"team-z\" for this user"
        )
        #expect(
            StatusFormatter.failureMessage(stderr: "relay credential: the Relay daemon is not running (broker.sock)\n", exitCode: 1)
                == "relay credential: the Relay daemon is not running (broker.sock)"
        )
        #expect(StatusFormatter.failureMessage(stderr: "", exitCode: 1) == "exited with status 1")
    }

    @Test func commandArguments() {
        #expect(RelayCommand.signIn.arguments == ["sign-in"])
        #expect(RelayCommand.signOut.arguments == ["sign-out"])
        #expect(RelayCommand.recheck.arguments == ["recheck"])
        #expect(RelayCommand.switchTeam("team-b").arguments == ["switch-team", "team-b"])
        #expect(RelayCommand.switchEnvironment("uat").arguments == ["switch-environment", "uat"])
        #expect(RelayCommand.switchTeam("x").kind == RelayCommand.switchTeam("y").kind)
    }

    @Test func relayBinaryHonoursTheOverride() {
        let home = URL(fileURLWithPath: "/Users/dev")
        #expect(RelayBinary.locate(environment: [:], home: home).path == "/Users/dev/.litellm-relay/bin/litellm-relay")
        #expect(RelayBinary.locate(environment: ["RELAY_BIN": "/opt/relay/litellm-relay"], home: home).path == "/opt/relay/litellm-relay")
        #expect(RelayBinary.locate(environment: ["RELAY_BIN": ""], home: home).path == "/Users/dev/.litellm-relay/bin/litellm-relay")
    }

    @Test func relayHomeFollowsTheHomeVariableTheDaemonReads() {
        #expect(RelayHome.locate(environment: ["HOME": "/tmp/l85/after/home"]).path == "/tmp/l85/after/home")
        #expect(RelayHome.locate(environment: ["HOME": ""]) == FileManager.default.homeDirectoryForCurrentUser)
        #expect(RelayHome.locate(environment: [:]) == FileManager.default.homeDirectoryForCurrentUser)
    }

    private func broker(displayName: String?, userId: String?) -> BrokerStatus {
        BrokerStatus(signedIn: true, userId: userId, displayName: displayName, team: nil, environment: nil, gatewayUrl: nil, keyExpiresAt: nil, source: nil)
    }
}
