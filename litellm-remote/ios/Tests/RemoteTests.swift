import XCTest
@testable import LiteLLMRemote

final class RemoteTests: XCTestCase {
    func testSignInCanBeSavedUpdatedAndRemovedSecurely() {
        let workspace = "test-\(UUID().uuidString)"
        defer { SessionVault.remove(workspace: workspace) }
        XCTAssertTrue(SessionVault.save("first-token", workspace: workspace))
        XCTAssertEqual(SessionVault.load(workspace: workspace), "first-token")
        XCTAssertTrue(SessionVault.save("refreshed-token", workspace: workspace))
        XCTAssertEqual(SessionVault.load(workspace: workspace), "refreshed-token")
        SessionVault.remove(workspace: workspace)
        XCTAssertNil(SessionVault.load(workspace: workspace))
    }

    func testConnectionRejectsCredentialsAndUnexpectedPaths() {
        XCTAssertNil(serviceAddress("https://username:password@gateway.example.com"))
        XCTAssertNil(serviceAddress("file:///private/data"))
        XCTAssertNil(serviceAddress("https://gateway.example.com/api/sessions"))
        XCTAssertNil(serviceAddress("https://gateway.example.com?redirect=another-host"))
        XCTAssertNil(serviceAddress("http://mac.local:4174"))
        XCTAssertNotNil(serviceAddress("https://remote.example.com"))
        XCTAssertEqual(serviceAddress("remote.example.com")?.scheme, "https")
        XCTAssertNotNil(serviceAddress("http://127.0.0.1:4174"))
        XCTAssertEqual(normalizedGateway("gateway.example.com/v1/")?.absoluteString, "https://gateway.example.com")
        XCTAssertEqual(normalizedGateway("https://example.com/proxy/v1")?.path, "/proxy")
        XCTAssertNil(normalizedGateway("http://gateway.example.com"))
    }

    func testHandoffAndApprovalSessionsShowTheRightControls() throws {
        let input = """
        {"sessions":[
          {"id":"claude:session","agent":"claude","title":"Task","project":"repo","cwd":"/repo","status":"handoff_required","canSend":false,"canStop":false,"updatedAt":1},
          {"id":"codex:session","agent":"codex","title":"Task","project":"repo","cwd":"/repo","status":"needs_approval","canSend":true,"canStop":true,"updatedAt":1}
        ]}
        """
        let page = try JSONDecoder().decode(SessionPage.self, from: Data(input.utf8))
        XCTAssertEqual(page.sessions[0].activity, "Set up on Mac")
        XCTAssertFalse(page.sessions[0].canSend)
        XCTAssertTrue(page.sessions[0].needsAttention)
        XCTAssertEqual(page.sessions[1].activity, "Needs approval")
        XCTAssertTrue(page.sessions[1].canStop)
        XCTAssertNotEqual(page.sessions[0].agentName, page.sessions[1].agentName)
    }

    func testTeamSelectionAndPaginationPreserveOpaqueValues() {
        let team = "engineering & operations + / # ?"
        let teamPath = queryPath("/api/auth/poll", name: "team", value: team)
        let teamComponents = URLComponents(string: teamPath)
        XCTAssertEqual(teamComponents?.path, "/api/auth/poll")
        XCTAssertEqual(teamComponents?.queryItems?.count, 1)
        XCTAssertEqual(teamComponents?.queryItems?.first?.value, team)
        let cursor = "page=two&offset=20+item/next"
        let cursorPath = queryPath("/api/sessions", name: "cursor", value: cursor)
        XCTAssertEqual(URLComponents(string: cursorPath)?.queryItems?.first?.value, cursor)
    }
}
