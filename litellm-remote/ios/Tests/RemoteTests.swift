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
        XCTAssertEqual(page.sessions[0].activity, "Set up on device")
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

    func testMarkdownQueueParsesEveryDistinctString() async {
        async let earlier = MarkdownQueue.shared.parse("alpha")
        async let later = MarkdownQueue.shared.parse("beta")
        let (first, second) = await (earlier, later)
        XCTAssertTrue((first.map { String($0.characters) } ?? "").contains("alpha"),
                      "an earlier bubble must not stay unparsed because a newer string was queued")
        XCTAssertTrue((second.map { String($0.characters) } ?? "").contains("beta"))
    }

    func testCachedMarkdownNeverShowsForNewerStreamedText() throws {
        let parsed = try XCTUnwrap(MessageMarkdown.parse("**bold** start"))
        let cached = RenderedMarkdown(source: "**bold** start", value: parsed)
        XCTAssertEqual(cached.display(for: "**bold** start"), parsed)
        let grown = cached.display(for: "**bold** start and more")
        XCTAssertEqual(String(grown.characters), "**bold** start and more",
                       "newer text must render plainly instead of showing the stale parse")
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

    @MainActor
    func testNewSessionDevicePrefersSelectionThenLinuxThenAnyOnlineDevice() {
        let store = RemoteStore(cacheDirectory: nil)
        let mac = RegisteredDevice(id: "mac", name: "Laptop", online: true, lastSeen: 0, platform: "macOS")
        let linux = RegisteredDevice(id: "linux", name: "Host", online: true, lastSeen: 0, platform: "Linux")
        let other = RegisteredDevice(id: "other", name: "Other", online: true, lastSeen: 0, platform: "Other OS")
        let offline = RegisteredDevice(id: "mac", name: "Laptop", online: false, lastSeen: 0, platform: "macOS")
        XCTAssertEqual(mac.platformLabel, "Mac")
        XCTAssertEqual(linux.platformLabel, "Linux")
        XCTAssertEqual(other.platformLabel, "Other OS")
        XCTAssertEqual(RegisteredDevice(id: "blank", name: "Blank", online: false, lastSeen: 0, platform: "").platformLabel, "Unknown")

        store.selectedDeviceId = mac.id
        store.devices = [other, linux, mac]
        XCTAssertEqual(store.newSessionDevice, mac)
        store.devices = [other, linux, offline]
        XCTAssertEqual(store.newSessionDevice, linux)
        XCTAssertEqual(store.selectedDevice, offline)
        store.devices = [other, offline]
        XCTAssertEqual(store.newSessionDevice, other)
        store.devices = [offline]
        XCTAssertNil(store.newSessionDevice)
    }

    @MainActor
    func testLastSeenListsStayWithTheirDeviceAcrossOfflineFailureAndRelaunch() async throws {
        let name = "cache-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        let directory = FileManager.default.temporaryDirectory.appending(path: name)
        defer {
            defaults.removePersistentDomain(forName: name)
            try? FileManager.default.removeItem(at: directory)
        }
        func connect(mode: String, subject: String = "first") async -> RemoteStore {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.protocolClasses = [RemoteTestProtocol.self]
            configuration.httpAdditionalHeaders = ["X-Fixture-Mode": mode, "X-Fixture-Subject": subject]
            let store = RemoteStore(defaults: defaults, session: URLSession(configuration: configuration), cacheDirectory: directory)
            store.gatewayAddress = RemoteTestProtocol.gateway
            store.serviceOverride = "https://service.fixture.test"
            await store.connect()
            await store.refreshDevices()
            return store
        }

        let first = await connect(mode: "online")
        XCTAssertEqual(first.selectedDeviceId, "mac")
        await first.refresh()
        XCTAssertEqual(first.nextCursor, "older")
        await first.refresh(older: true)
        XCTAssertEqual(first.sessions.map(\.id), ["mac-saved", "mac-older"])
        XCTAssertNil(first.nextCursor)
        await first.refresh()
        await first.selectDevice("linux")
        XCTAssertTrue(first.sessions.isEmpty)
        XCTAssertEqual(first.sessions(for: "mac").map(\.id), ["mac-saved"])

        let reopened = await connect(mode: "offline")
        XCTAssertEqual(reopened.selectedDeviceId, "linux")
        await reopened.selectDevice("mac")
        XCTAssertFalse(reopened.deviceIsOnline("mac"))
        XCTAssertEqual(reopened.sessions.map(\.id), ["mac-saved", "mac-older"])
        XCTAssertNil(reopened.error)
        XCTAssertNil(reopened.nextCursor)
        XCTAssertEqual(reopened.newSessionDevice?.id, "linux")
        XCTAssertTrue(reopened.sessions(for: "linux").isEmpty)

        let otherAccount = await connect(mode: "offline", subject: "second")
        await otherAccount.selectDevice("mac")
        XCTAssertTrue(otherAccount.sessions.isEmpty)
    }

    @MainActor
    func testCreatedAndOpenedSessionsSurviveGoingOfflineWithoutAnotherListFetch() async throws {
        let name = "cache-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        let directory = FileManager.default.temporaryDirectory.appending(path: name)
        defer {
            defaults.removePersistentDomain(forName: name)
            try? FileManager.default.removeItem(at: directory)
        }
        func connect(mode: String) async -> RemoteStore {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.protocolClasses = [RemoteTestProtocol.self]
            configuration.httpAdditionalHeaders = ["X-Fixture-Mode": mode]
            let store = RemoteStore(defaults: defaults, session: URLSession(configuration: configuration), cacheDirectory: directory)
            store.gatewayAddress = RemoteTestProtocol.gateway
            store.serviceOverride = "https://service.fixture.test"
            await store.connect()
            await store.refreshDevices()
            return store
        }

        let store = await connect(mode: "online")
        await store.refresh()
        let created = try await store.create(cwd: "/Users/test/repo", agent: "codex", deviceId: "mac", requestId: "create").get()
        XCTAssertEqual(store.sessions.map(\.id), [created.id, "mac-saved"])
        XCTAssertEqual(store.nextCursor, "older")
        await store.selectDevice("linux")
        let updated = try await store.detail(id: created.id, deviceId: "mac").get()
        let opened = try await store.detail(id: "mac-opened", deviceId: "mac").get()
        let macSessions = store.sessions(for: "mac")
        XCTAssertEqual(macSessions.map(\.id), [opened.session.id, created.id, "mac-saved"])
        XCTAssertEqual(macSessions.first { $0.id == created.id }, updated.session)
        XCTAssertTrue(store.sessions.isEmpty)

        let reopened = await connect(mode: "offline")
        await reopened.selectDevice("mac")
        XCTAssertFalse(reopened.deviceIsOnline("mac"))
        XCTAssertEqual(reopened.sessions.map(\.id), [opened.session.id, created.id, "mac-saved"])
        XCTAssertEqual(reopened.sessions.first { $0.id == created.id }?.status, "working")
        XCTAssertTrue(reopened.sessions(for: "linux").isEmpty)
        let files = try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
        let cache = try String(contentsOf: XCTUnwrap(files.first), encoding: .utf8)
        XCTAssertFalse(cache.contains("Fixture transcript"))
    }
}

private final class RestoreProtocol: URLProtocol, @unchecked Sendable {
    static let gateway = "https://gateway.example.test"
    static let started = Notification.Name("RemoteRestoreRequestStarted")
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
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
}
