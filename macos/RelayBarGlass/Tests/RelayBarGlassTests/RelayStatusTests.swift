import Foundation
import Testing
@testable import RelayBarGlass

@Suite struct RelayStatusTests {
    static let signedIn = """
    {
      "listen": "127.0.0.1:4142",
      "gateway_url": "https://gateway.example.com",
      "runtime": "rust",
      "credential": {"configured": true, "state": "valid"},
      "broker": {
        "signed_in": true,
        "user_id": "user-1",
        "display_name": "Dev Eloper",
        "team": "team-a",
        "environment": "dev",
        "gateway_url": "https://gateway.example.com",
        "key_expires_at": "2027-01-15T08:00:00Z",
        "key_extended_at": null,
        "source": "minted_key",
        "refused_callers": 0
      },
      "account": {
        "user": {"id": "user-1", "email": "dev@example.com"},
        "teams": [{"id": "team-a", "alias": "Team A"}, {"id": "team-b", "alias": null}],
        "teams_error": null,
        "team": {"id": "team-a", "alias": "Team A", "spend": 1.5, "max_budget": 50.0, "budget_reset_at": "2027-02-01T00:00:00+00:00"},
        "budget_error": null,
        "gateway": {"url": "https://gateway.example.com", "reachable": true, "checked_at": "2027-01-15T08:00:00Z", "error": null},
        "polled_at": "2027-01-15T08:00:00Z"
      },
      "environments": {
        "current": "dev",
        "available": [{"name": "dev", "url": "https://gateway.example.com"}, {"name": "uat", "url": "https://uat.example.com"}]
      },
      "mcp": {"catalog_tools": 3, "servers": {"github": {"tools": 2, "active": true}, "my_jira": {"tools": 1, "active": false}}}
    }
    """

    static let signedOut = """
    {
      "listen": "127.0.0.1:49290",
      "broker": {"signed_in": false, "user_id": null, "display_name": null, "team": "relay-qa", "environment": null, "gateway_url": "http://127.0.0.1:59621", "key_expires_at": null, "key_extended_at": null, "source": null, "refused_callers": 0},
      "account": {
        "user": {"id": null, "email": null},
        "teams": null,
        "teams_error": null,
        "team": {"id": "relay-qa", "alias": null, "spend": null, "max_budget": null, "budget_reset_at": null},
        "budget_error": null,
        "gateway": {"url": "http://127.0.0.1:59621", "reachable": true, "checked_at": "2026-10-07T04:40:51Z", "error": null},
        "polled_at": null
      },
      "environments": {"current": null, "available": []},
      "mcp": {"catalog_tools": null, "servers": {}}
    }
    """

    static let staticKey = """
    {
      "listen": "127.0.0.1:4142",
      "credential": {"configured": true, "state": "valid"},
      "broker": {"signed_in": true, "user_id": null, "display_name": null, "team": null, "environment": null, "gateway_url": "https://gateway.example.com", "key_expires_at": null, "key_extended_at": null, "source": "static_key", "refused_callers": 0},
      "account": {
        "user": {"id": null, "email": "dev@example.com"},
        "teams": [{"id": "team-a", "alias": "Team A"}],
        "teams_error": null,
        "team": null,
        "key": {"spend": 3.5, "max_budget": 10.0, "budget_reset_at": "2027-02-01T00:00:00+00:00"},
        "budget_error": null,
        "gateway": {"url": "https://gateway.example.com", "reachable": false, "checked_at": null, "error": null},
        "polled_at": "2027-01-15T08:00:00Z"
      },
      "environments": {"current": null, "available": []},
      "mcp": {"catalog_tools": null, "servers": {}}
    }
    """

    static let teamInfoDenied = """
    {
      "listen": "127.0.0.1:4142",
      "log_path": "/Users/dev/.litellm-relay/events.jsonl",
      "ai_domains": ["api.anthropic.com", "api.openai.com"],
      "notion_domains": [],
      "capture_payloads": false,
      "mitm_ca_path": null,
      "shadow_enabled": false,
      "gateway_url": "https://gateway.example.com",
      "events_loaded": 12,
      "known_apps": [],
      "attribution": {"destination_field": "destination_app", "compatibility_field": "app", "sources": ["known_app_catalog"], "confidences": ["high"]},
      "runtime": "rust",
      "credential": {"configured": true, "state": "valid"},
      "broker": {
        "signed_in": true,
        "user_id": "user-1",
        "display_name": "Dev Eloper",
        "team": "team-a",
        "environment": "uat",
        "gateway_url": "https://uat.example.com",
        "key_expires_at": "2027-01-15T08:00:00Z",
        "key_extended_at": "2027-01-15T07:30:00Z",
        "source": "minted_key",
        "refused_callers": 2
      },
      "mcp": {"catalog_tools": 4, "servers": {"github": {"tools": 4, "active": true}}},
      "account": {
        "user": {"id": "user-1", "email": "dev@example.com"},
        "teams": [{"id": "team-a", "alias": "Team A"}, {"id": "team-b", "alias": "Team B"}],
        "teams_error": null,
        "team": {"id": "team-a", "alias": "Team A", "spend": 7.25, "max_budget": 20.0, "budget_reset_at": "2027-02-01T00:00:00+00:00"},
        "budget_error": "team_id=team-a is not allowed to call /team/info",
        "gateway": {"url": "https://uat.example.com", "reachable": false, "checked_at": "2027-01-15T07:59:30Z", "error": "connection refused"},
        "polled_at": "2027-01-15T07:59:00Z"
      },
      "environments": {"current": "uat", "available": [{"name": "dev", "url": "https://gateway.example.com"}, {"name": "uat", "url": "https://uat.example.com"}]}
    }
    """

    static let noTeamsListed = """
    {
      "listen": "127.0.0.1:4142",
      "gateway_url": "https://gateway.example.com",
      "runtime": "rust",
      "credential": {"configured": true, "state": "valid"},
      "broker": {"signed_in": true, "user_id": "user-2", "display_name": null, "team": "team-c", "environment": null, "gateway_url": "https://gateway.example.com", "key_expires_at": null, "key_extended_at": null, "source": "session_credential", "refused_callers": 0},
      "mcp": {"catalog_tools": 0, "servers": {}},
      "account": {
        "user": {"id": "user-2", "email": null},
        "teams": [{"id": "team-c", "alias": null}],
        "teams_error": "the Gateway lists no teams for this user",
        "team": {"id": "team-c", "alias": null, "spend": 0.5, "max_budget": null, "budget_reset_at": null},
        "budget_error": null,
        "gateway": {"url": "https://gateway.example.com", "reachable": true, "checked_at": "2027-01-15T08:00:05Z", "error": "HTTP 503: warming up"},
        "polled_at": "2027-01-15T08:00:00Z"
      },
      "environments": {"current": null, "available": []}
    }
    """

    static let olderDaemon = """
    {"listen": "127.0.0.1:4142", "runtime": "rust", "broker": null, "account": null, "environments": null, "mcp": null}
    """

    @Test func decodesEveryObjectOfASignedInDaemon() throws {
        let status = try RelayStatus.decode(Data(Self.signedIn.utf8))
        #expect(status.broker?.signedIn == true)
        #expect(status.broker?.displayName == "Dev Eloper")
        #expect(status.broker?.keyExpiresAt == "2027-01-15T08:00:00Z")
        #expect(status.broker?.source == "minted_key")
        #expect(status.account?.user?.email == "dev@example.com")
        #expect(status.account?.teams == [TeamEntry(id: "team-a", alias: "Team A"), TeamEntry(id: "team-b", alias: nil)])
        #expect(status.account?.team?.spend == 1.5)
        #expect(status.account?.team?.maxBudget == 50.0)
        #expect(status.account?.team?.budgetResetAt == "2027-02-01T00:00:00+00:00")
        #expect(status.account?.gateway?.reachable == true)
        #expect(status.environments?.current == "dev")
        #expect(status.environments?.available?.map(\.name) == ["dev", "uat"])
        #expect(status.mcp?.catalogTools == 3)
        #expect(status.mcp?.servers?["github"] == McpServer(tools: 2, active: true))
        #expect(status.mcp?.servers?["my_jira"]?.active == false)
    }

    @Test func decodesEveryFieldTheDaemonSendsWhenTheTeamBudgetFellBack() throws {
        let expected = RelayStatus(
            broker: BrokerStatus(
                signedIn: true,
                userId: "user-1",
                displayName: "Dev Eloper",
                team: "team-a",
                environment: "uat",
                gatewayUrl: "https://uat.example.com",
                keyExpiresAt: "2027-01-15T08:00:00Z",
                source: "minted_key"
            ),
            account: AccountStatus(
                user: AccountUser(id: "user-1", email: "dev@example.com"),
                teams: [TeamEntry(id: "team-a", alias: "Team A"), TeamEntry(id: "team-b", alias: "Team B")],
                teamsError: nil,
                team: TeamBudget(id: "team-a", alias: "Team A", spend: 7.25, maxBudget: 20, budgetResetAt: "2027-02-01T00:00:00+00:00"),
                key: nil,
                budgetError: "team_id=team-a is not allowed to call /team/info",
                gateway: GatewayProbe(url: "https://uat.example.com", reachable: false, checkedAt: "2027-01-15T07:59:30Z", error: "connection refused"),
                polledAt: "2027-01-15T07:59:00Z"
            ),
            environments: EnvironmentsStatus(
                current: "uat",
                available: [EnvironmentEntry(name: "dev", url: "https://gateway.example.com"), EnvironmentEntry(name: "uat", url: "https://uat.example.com")]
            ),
            mcp: McpStatus(catalogTools: 4, servers: ["github": McpServer(tools: 4, active: true)])
        )
        #expect(try RelayStatus.decode(Data(Self.teamInfoDenied.utf8)) == expected)
    }

    @Test func decodesTheTeamsErrorAndAGatewayProbeError() throws {
        let expected = RelayStatus(
            broker: BrokerStatus(
                signedIn: true,
                userId: "user-2",
                displayName: nil,
                team: "team-c",
                environment: nil,
                gatewayUrl: "https://gateway.example.com",
                keyExpiresAt: nil,
                source: "session_credential"
            ),
            account: AccountStatus(
                user: AccountUser(id: "user-2", email: nil),
                teams: [TeamEntry(id: "team-c", alias: nil)],
                teamsError: "the Gateway lists no teams for this user",
                team: TeamBudget(id: "team-c", alias: nil, spend: 0.5, maxBudget: nil, budgetResetAt: nil),
                key: nil,
                budgetError: nil,
                gateway: GatewayProbe(url: "https://gateway.example.com", reachable: true, checkedAt: "2027-01-15T08:00:05Z", error: "HTTP 503: warming up"),
                polledAt: "2027-01-15T08:00:00Z"
            ),
            environments: EnvironmentsStatus(current: nil, available: []),
            mcp: McpStatus(catalogTools: 0, servers: [:])
        )
        #expect(try RelayStatus.decode(Data(Self.noTeamsListed.utf8)) == expected)
    }

    @Test func showsTheFallbackBudgetAndItsErrorFromADaemonStatus() throws {
        let account = try #require(try RelayStatus.decode(Data(Self.teamInfoDenied.utf8)).account)
        #expect(
            StatusFormatter.budgetLine(team: account.team, error: account.budgetError, polled: account.polledAt != nil, timeZone: .gmt)
                == "$7.25 of $20.00, resets 1 Feb\nteam_id=team-a is not allowed to call /team/info"
        )
    }

    @Test func decodesTheStaticKeyBudgetAndReadsAMissingOrNullKeyAsNone() throws {
        #expect(try RelayStatus.decode(Data(Self.staticKey.utf8)).account?.key == KeyBudget(spend: 3.5, maxBudget: 10, budgetResetAt: "2027-02-01T00:00:00+00:00"))
        #expect(try RelayStatus.decode(Data(Self.signedIn.utf8)).account?.key == nil)
        #expect(try RelayStatus.decode(Data(#"{"listen": "127.0.0.1:4142", "account": {"key": null}}"#.utf8)).account?.key == nil)
    }

    @Test func showsTheStaticKeyBudgetFromADaemonStatusWhereNoTeamIsKnown() throws {
        let status = try RelayStatus.decode(Data(Self.staticKey.utf8))
        let account = try #require(status.account)
        #expect(
            StatusFormatter.budgetLine(team: account.team, key: account.key, error: account.budgetError, polled: account.polledAt != nil, timeZone: .gmt)
                == "$3.50 of $10.00, resets 1 Feb"
        )
    }

    @Test func decodesASignedOutDaemonWithNullBudgetFields() throws {
        let status = try RelayStatus.decode(Data(Self.signedOut.utf8))
        #expect(status.broker?.signedIn == false)
        #expect(status.broker?.source == nil)
        #expect(status.account?.teams == nil)
        #expect(status.account?.team?.id == "relay-qa")
        #expect(status.account?.team?.spend == nil)
        #expect(status.environments?.current == nil)
        #expect(status.environments?.available == [])
        #expect(status.mcp?.catalogTools == nil)
        #expect(status.mcp?.servers == [:])
    }

    @Test func namesAStaticKeyDeviceFromTheBrokerSource() throws {
        #expect(try RelayStatus.decode(Data(Self.staticKey.utf8)).usesStaticKey)
        #expect(try RelayStatus.decode(Data(Self.signedIn.utf8)).usesStaticKey == false)
        #expect(try RelayStatus.decode(Data(Self.signedOut.utf8)).usesStaticKey == false)
    }

    @Test func decodesAnOlderDaemonWithEveryObjectNull() throws {
        let status = try RelayStatus.decode(Data(Self.olderDaemon.utf8))
        #expect(status.broker == nil)
        #expect(status.account == nil)
        #expect(status.environments == nil)
        #expect(status.mcp == nil)
    }

    @Test func decodesWhenTheObjectsAreMissingEntirely() throws {
        let status = try RelayStatus.decode(Data(#"{"listen": "127.0.0.1:4142"}"#.utf8))
        #expect(status.broker == nil)
        #expect(status.account == nil)
    }
}
