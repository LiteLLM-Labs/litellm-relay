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
