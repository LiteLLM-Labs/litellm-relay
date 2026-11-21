import XCTest
@testable import LiteLLMRemote

final class RemoteTests: XCTestCase {
    @MainActor
    func testSavedSignInRestoresAcrossAppStoreRecreation() async throws {
        try await withSavedSession { defaults, _, token in
            let first = RemoteStore(defaults: defaults, session: restoreSession())
            XCTAssertTrue(first.restoring)
            await first.restore()
            XCTAssertEqual(first.configuration?.identity?.subject, token)
            let reopened = RemoteStore(defaults: defaults, session: restoreSession())
            XCTAssertTrue(reopened.restoring)
            await reopened.restore()
            XCTAssertFalse(reopened.restoring)
            XCTAssertEqual(reopened.configuration?.identity?.subject, token)
            XCTAssertNil(reopened.login)
        }
    }

    @MainActor
    func testOfflineRestoreKeepsSignInForReconnect() async throws {
        try await withSavedSession { defaults, workspace, token in
            let offline = RemoteStore(defaults: defaults, session: restoreSession(response: "offline"))
            await offline.restore()
            XCTAssertFalse(offline.restoring)
            XCTAssertTrue(offline.hasSavedSession)
            XCTAssertNotNil(offline.error)
            XCTAssertEqual(SessionVault.load(workspace: workspace), token)
            let reconnected = RemoteStore(defaults: defaults, session: restoreSession())
            await reconnected.connect()
            XCTAssertEqual(reconnected.configuration?.identity?.subject, token)
        }
    }

    @MainActor
    func testConfirmedExpiredSignInReturnsToLogin() async throws {
        try await withSavedSession { defaults, workspace, _ in
            let expired = RemoteStore(defaults: defaults, session: restoreSession(response: "expired"))
            await expired.restore()
            XCTAssertFalse(expired.hasSavedSession)
            XCTAssertNil(expired.configuration?.identity)
            XCTAssertNil(SessionVault.load(workspace: workspace))
            await expired.restore()
            XCTAssertNil(expired.configuration?.identity)
        }
    }

    @MainActor
    func testRestoreAcceptsControlRefreshReplyWithoutLoginStatus() async throws {
        try await withSavedSession { defaults, workspace, _ in
            defaults.set([workspace], forKey: "refreshWorkspaces")
            let store = RemoteStore(defaults: defaults, session: restoreSession(response: "refresh"))
            await store.restore()
            XCTAssertEqual(store.configuration?.identity?.subject, "qa")
            XCTAssertEqual(SessionVault.load(workspace: workspace), "first-refresh")
        }
    }

    @MainActor
    func testConcurrentRequestsShareOneSignInRefresh() async throws {
        try await withSavedSession { defaults, workspace, _ in
            defaults.set([workspace], forKey: "refreshWorkspaces")
            let store = RemoteStore(defaults: defaults, session: restoreSession(response: "rotate"))
            await store.restore()
            XCTAssertNotNil(store.configuration?.identity)
            async let first = store.models(deviceId: "fixture")
            async let second = store.models(deviceId: "fixture")
            let results = await (first, second)
            XCTAssertTrue(try results.0.get().models.isEmpty)
            XCTAssertTrue(try results.1.get().models.isEmpty)
            XCTAssertEqual(SessionVault.load(workspace: workspace), "next-refresh")
        }
    }

    @MainActor
    func testFailedRefreshSaveKeepsTheRotatedSignInInMemory() async throws {
        try await withSavedSession { defaults, workspace, _ in
            defaults.set([workspace], forKey: "refreshWorkspaces")
            let store = RemoteStore(defaults: defaults, session: restoreSession(response: "rotate")) { token, workspace in
                token == "next-refresh" && SessionVault.save(token, workspace: workspace)
            }
            await store.restore()
            XCTAssertEqual(store.configuration?.identity?.subject, "qa")
            XCTAssertFalse(store.hasSavedSession)
            XCTAssertNil(SessionVault.load(workspace: workspace))
            let result = await store.models(deviceId: "fixture")
            XCTAssertTrue(try result.get().models.isEmpty)
            XCTAssertEqual(SessionVault.load(workspace: workspace), "next-refresh")
        }
    }

    @MainActor
    func testForegroundRestoreDoesNotOverwriteWorkspaceEdits() async throws {
        try await withSavedSession { defaults, workspace, token in
            let changedGateway = RemoteStore(defaults: defaults, session: restoreSession())
            changedGateway.gatewayAddress = "https://another-gateway.example.test"
            let changedService = RemoteStore(defaults: defaults, session: restoreSession())
            changedService.serviceOverride = "https://another-service.example.test"
            for store in [changedGateway, changedService] {
                await store.restore()
                XCTAssertFalse(store.restoring)
                XCTAssertNil(store.configuration)
                XCTAssertNil(store.error)
            }
            XCTAssertEqual(SessionVault.load(workspace: workspace), token)
            XCTAssertEqual(defaults.string(forKey: "workspaceAddress"), workspace)
        }
    }

    @MainActor
    func testCanceledStartupKeepsReconnectFormVisibleOnForeground() async throws {
        try await withSavedSession { defaults, workspace, token in
            let store = RemoteStore(defaults: defaults, session: restoreSession(response: "pending"))
            for initial in [true, false] {
                let started = XCTestExpectation(description: "Restoration request started")
                let observer = NotificationCenter.default.addObserver(
                    forName: RestoreProtocol.started, object: nil, queue: nil
                ) { notification in
                    if (notification.object as? URL)?.absoluteString == workspace + "/api/me" { started.fulfill() }
                }
                let attempt = Task { await store.restore() }
                let result = await XCTWaiter.fulfillment(of: [started], timeout: 5)
                NotificationCenter.default.removeObserver(observer)
                XCTAssertEqual(result, .completed)
                XCTAssertEqual(store.restoring, initial)
                if !initial { store.serviceOverride = "https://edited-service.example.test" }
                attempt.cancel()
                await attempt.value
                XCTAssertFalse(store.restoring)
                XCTAssertFalse(store.busy)
            }
            XCTAssertNil(store.error)
            XCTAssertNil(store.configuration)
            XCTAssertEqual(SessionVault.load(workspace: workspace), token)
            XCTAssertEqual(defaults.string(forKey: "serviceOverride"), nil)
        }
    }

    @MainActor
    private func withSavedSession(_ run: @MainActor (UserDefaults, String, String) async throws -> Void) async throws {
        let name = "restore-\(UUID().uuidString.lowercased())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        let workspace = "https://\(name).example.test"
        let token = UUID().uuidString
        defer {
            SessionVault.remove(workspace: workspace)
            defaults.removePersistentDomain(forName: name)
        }
        defaults.set(RestoreProtocol.gateway, forKey: "gatewayAddress")
        defaults.set(workspace, forKey: "workspaceAddress")
        XCTAssertTrue(SessionVault.save(token, workspace: workspace))
        try await run(defaults, workspace, token)
    }

    private func restoreSession(response: String = "valid") -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RestoreProtocol.self]
        configuration.httpAdditionalHeaders = ["X-Test-Response": response]
        return URLSession(configuration: configuration)
    }

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

    func testIdenticalPollsCompareEqualSoTheViewSkipsRedundantUpdates() throws {
        let payload = """
        {"session":{"id":"claude:session","agent":"claude","title":"Task","project":"repo","cwd":"/repo","status":"working","canSend":false,"canStop":true,"updatedAt":1},
         "messages":[{"id":"turn","role":"user","text":"Start"},{"id":"reply/0","role":"assistant","text":"Streaming"}],
         "olderMessages":false,"approvals":[]}
        """
        let first = try JSONDecoder().decode(SessionDetail.self, from: Data(payload.utf8))
        let second = try JSONDecoder().decode(SessionDetail.self, from: Data(payload.utf8))
        XCTAssertEqual(first, second)
        let grown = payload.replacingOccurrences(of: "Streaming", with: "Streaming more")
        let changed = try JSONDecoder().decode(SessionDetail.self, from: Data(grown.utf8))
        XCTAssertNotEqual(first, changed)
        XCTAssertEqual(first.messages.map(\.id), changed.messages.map(\.id),
                       "a growing streamed reply must keep its row identity instead of adding a row")
    }

    func testMarkdownParsingHandlesLongCodeHeavyRepliesOffTheRenderPath() throws {
        let code = String(repeating: "let value_\(UUID().uuidString.prefix(4)) = items[0] * 2 // 100% _load_\n", count: 400)
        let reply = "Result:\n\n`inline` **bold** and a block:\n" + code
        let parsed = try XCTUnwrap(MessageMarkdown.parse(reply))
        XCTAssertTrue(String(parsed.characters).contains("inline"))
        XCTAssertTrue(String(MessageMarkdown.parse("line one\nline two").map { String($0.characters) } ?? "").contains("line one\nline two"),
                      "inline parsing must preserve newlines between lines")
        XCTAssertNotNil(MessageMarkdown.parse(String(repeating: "** _ ` [ ] ( ) %@ %d ", count: 1600)),
                        "pathological markup must still return a usable value")
    }

    func testMarkdownQueueParsesEveryDistinctString() async throws {
        async let earlier = MarkdownQueue.shared.parse("alpha")
        async let later = MarkdownQueue.shared.parse("beta")
        let (first, second) = await (earlier, later)
        XCTAssertEqual(String(try XCTUnwrap(first).characters), "alpha",
                      "an earlier bubble must not stay unparsed because a newer string was queued")
        XCTAssertEqual(String(try XCTUnwrap(second).characters), "beta")
    }

    func testDenseMarkdownLimitsActiveLinksWithoutRemovingLabels() throws {
        let links = (0..<40).map { "[label\($0)](https://example.test/\($0))" }.joined(separator: "\n")
        let parsed = try XCTUnwrap(MessageMarkdown.parse("[unsafe](http://example.test)\n" + links))
        let active = parsed.runs.compactMap(\.link)
        XCTAssertEqual(active.count, 12)
        XCTAssertEqual(active.first?.absoluteString, "https://example.test/0")
        XCTAssertTrue(String(parsed.characters).hasSuffix("label39"))
    }

    func testCachedMarkdownNeverShowsForNewerStreamedText() throws {
        let parsed = try XCTUnwrap(MessageMarkdown.parse("**bold** start"))
        let cached = RenderedMarkdown(source: "**bold** start", value: parsed)
        XCTAssertEqual(cached.display(for: "**bold** start"), parsed)
        let grown = cached.display(for: "**bold** start and more")
        XCTAssertEqual(String(grown.characters), "**bold** start and more",
                       "newer text must render plainly instead of showing the stale parse")
    }

    func testStreamingPreviewKeepsTheNewestOutputWithoutChangingTheMessage() {
        let text = String(repeating: "🧑🏽‍💻", count: 900) + "\nline 2\nline 3\nline 4\nline 5"
        let message = SessionMessage(id: "reply", role: "assistant", text: text)
        XCTAssertEqual(message.streamingPreview, "line 2\nline 3\nline 4\nline 5")
        XCTAssertEqual(message.text, text)

        let longLine = SessionMessage(id: "other", role: "assistant", text: String(repeating: "🧑🏽‍💻", count: 900))
        XCTAssertEqual(longLine.streamingPreview, String(repeating: "🧑🏽‍💻", count: 600))
    }

    func testPhoneSessionKeepsTheAccessTokenInMemoryAndRotatesRefresh() {
        var session = PhoneSession(refreshToken: "refresh-1")
        XCTAssertEqual(session.bearer(now: 0), .refresh)
        session.rotated(access: "access-1", refresh: "refresh-2", expiresAt: 1_000_000)
        XCTAssertEqual(session.bearer(now: 1_000_000 - 180_001), .current("access-1"))
        XCTAssertEqual(session.bearer(now: 1_000_000 - 180_000), .refresh)
        XCTAssertEqual(session.refreshToken, "refresh-2")
        XCTAssertNil(session.legacyToken)
        XCTAssertEqual(PhoneSession(legacyToken: "week-old").bearer(now: 0), .current("week-old"))
        XCTAssertEqual(PhoneSession().bearer(now: 0), .signedOut)
    }

    func testAttachmentsTravelAsFencedBlocksWithSizeLimits() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("attach-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        let source = directory.appendingPathComponent("proxy.rs")
        try "fn main() {}\n".write(to: source, atomically: true, encoding: .utf8)
        let attachment = try DraftAttachment.read(url: source).get()
        XCTAssertEqual(attachment.name, "proxy.rs")
        let hostile = DraftAttachment(name: "a\"\nb<c>", text: "note")
        XCTAssertEqual(
            DraftAttachment.compose(draft: "prompt", attachments: [hostile]),
            "prompt\n\n<attached-file name=\"a__b_c_\">\nnote\n</attached-file>"
        )

        let composed = DraftAttachment.compose(draft: "fix this", attachments: [attachment])
        XCTAssertEqual(composed, "fix this\n\n<attached-file name=\"proxy.rs\">\nfn main() {}\n\n</attached-file>")
        XCTAssertEqual(DraftAttachment.compose(draft: "  just text  ", attachments: []), "just text")
        XCTAssertTrue(DraftAttachment.compose(draft: "", attachments: [attachment]).hasPrefix("<attached-file"))

        let oversized = directory.appendingPathComponent("big.log")
        try String(repeating: "x", count: 25_000).write(to: oversized, atomically: true, encoding: .utf8)
        guard case .failure(let failure) = DraftAttachment.read(url: oversized) else {
            return XCTFail("an oversized file must be refused")
        }
        XCTAssertTrue(failure.message.contains("big.log"))

        let binary = directory.appendingPathComponent("photo.bin")
        try Data([0xFF, 0xD8, 0xFF, 0x00, 0x80]).write(to: binary)
        XCTAssertThrowsError(try DraftAttachment.read(url: binary).get())
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

private final class RestoreProtocol: URLProtocol, @unchecked Sendable {
    private static let rotationLock = NSLock()
    nonisolated(unsafe) private static var rotations: [String: Int] = [:]
    static let gateway = "https://gateway.example.test"
    static let started = Notification.Name("RemoteRestoreRequestStarted")
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        let mode = request.value(forHTTPHeaderField: "X-Test-Response")
        if mode == "refresh" || mode == "rotate", let url = request.url {
            respondToRotation(url, useLoginStatus: mode == "rotate")
            return
        }
        guard let url = request.url, url.path == "/api/me" else {
            client?.urlProtocol(self, didFailWithError: URLError(.unsupportedURL))
            return
        }
        if request.value(forHTTPHeaderField: "X-Test-Response") == "pending" {
            NotificationCenter.default.post(name: Self.started, object: url)
            return
        }
        if request.value(forHTTPHeaderField: "X-Test-Response") == "offline" {
            client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
            return
        }
        let token = request.value(forHTTPHeaderField: "Authorization").flatMap {
            $0.hasPrefix("Bearer ") ? String($0.dropFirst(7)) : nil
        }
        let identity = request.value(forHTTPHeaderField: "X-Test-Response") == "expired" ? nil : token.map {
            GatewayIdentity(subject: $0, label: "Test account", method: "sso", expiresAt: Date().addingTimeInterval(60).timeIntervalSince1970 * 1000)
        }
        struct Reply: Encodable { let gateway: String; let identity: GatewayIdentity? }
        do {
            let data = try JSONEncoder().encode(Reply(gateway: Self.gateway, identity: identity))
            let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: nil, headerFields: ["Content-Type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    private func respondToRotation(_ url: URL, useLoginStatus: Bool) {
        switch url.path {
        case "/api/auth/refresh":
            let count = Self.rotationLock.withLock {
                Self.rotations[url.host ?? "", default: 0] += 1
                return Self.rotations[url.host ?? "", default: 0]
            }
            switch count {
            case 1:
                var reply: [String: Any] = ["token": "first-access", "refreshToken": "first-refresh", "expiresAt": 0]
                if useLoginStatus { reply["status"] = "ready" }
                respond(url, json: reply)
            case 2:
                DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(500)) { [weak self] in
                    self?.respond(url, json: [
                        "status": "ready", "token": "next-access", "refreshToken": "next-refresh",
                        "expiresAt": Int64(Date().addingTimeInterval(900).timeIntervalSince1970 * 1000)
                    ])
                }
            default:
                respond(url, status: 401, json: ["error": "Sign in again."])
            }
        case "/api/me":
            respond(url, json: ["gateway": Self.gateway, "identity": [
                "subject": "qa", "label": "QA", "method": "sso", "expiresAt": 0
            ]])
        case "/api/devices/fixture/models":
            if request.value(forHTTPHeaderField: "Authorization") == "Bearer next-access" {
                respond(url, json: ["models": []])
            } else {
                respond(url, status: 401, json: ["error": "Sign in again."])
            }
        default:
            respond(url, status: 404, json: ["error": "Unexpected fixture route."])
        }
    }

    private func respond(_ url: URL, status: Int = 200, json: [String: Any]) {
        do {
            let data = try JSONSerialization.data(withJSONObject: json)
            let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: ["Content-Type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }
}
