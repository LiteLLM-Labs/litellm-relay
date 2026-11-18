import XCTest
@testable import LiteLLMRemote

final class RemoteTests: XCTestCase {
    func testConnectionRejectsCredentialsAndUnexpectedPaths() {
        XCTAssertNil(bridgeAddress("https://username:password@gateway.example.com"))
        XCTAssertNil(bridgeAddress("file:///private/data"))
        XCTAssertNil(bridgeAddress("https://gateway.example.com/api/sessions"))
        XCTAssertNil(bridgeAddress("https://gateway.example.com?redirect=another-host"))
        XCTAssertNotNil(bridgeAddress("http://mac.local:4174"))
        XCTAssertNotNil(bridgeAddress("https://bridge.example.com"))
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
