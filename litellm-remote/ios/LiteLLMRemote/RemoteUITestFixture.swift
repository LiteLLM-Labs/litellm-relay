#if DEBUG
import Foundation
import Synchronization

@MainActor
enum RemoteUITestFixture {
    private static var mode: String? { ProcessInfo.processInfo.environment["REMOTE_UI_TEST_MODE"] }
    static var enabled: Bool { mode != nil }

    static var fromLaunchEnvironment: RemoteStore? {
        guard let mode else { return nil }
        let run = ProcessInfo.processInfo.environment["REMOTE_UI_TEST_RUN"] ?? UUID().uuidString
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RemoteTestProtocol.self]
        configuration.httpAdditionalHeaders = ["X-Fixture-Mode": mode]
        let cache = FileManager.default.temporaryDirectory.appending(path: "RemoteUITests/\(run)")
        let store = RemoteStore(defaults: UserDefaults(suiteName: "RemoteUITests.\(run)")!,
                                session: URLSession(configuration: configuration), cacheDirectory: cache)
        store.gatewayAddress = RemoteTestProtocol.gateway
        store.serviceOverride = "https://service.fixture.test"
        store.restoring = true
        return store
    }
}

final class RemoteTestProtocol: URLProtocol, @unchecked Sendable {
    static let gateway = "https://gateway.fixture.test"
    private static let sentPrompt = Mutex<(text: String, at: ContinuousClock.Instant)?>(nil)
    private static let codexPath = "/api/devices/mac/sessions/mac-codex-history"
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        guard let url = request.url else { return }
        let mode = request.value(forHTTPHeaderField: "X-Fixture-Mode")
        let codexHistory = mode == "codex-history"
        let macOnline = mode == "online" || codexHistory
        let linuxOnline = mode != "all-offline"
        let paths = url.pathComponents
        let body: [String: Any]
        var status = 200
        switch url.path {
        case "/api/me":
            let subject = request.value(forHTTPHeaderField: "X-Fixture-Subject") ?? "fixture-account"
            body = ["gateway": Self.gateway, "identity": ["subject": subject, "label": "Test account",
                "method": "sso", "expiresAt": 4_000_000_000_000 as Int64]]
        case "/api/devices":
            body = ["devices": [
                ["id": "mac", "name": "Test laptop", "platform": "macOS", "online": macOnline, "lastSeen": 1],
                ["id": "linux", "name": "Test host", "platform": "Linux", "online": linuxOnline, "lastSeen": 1]
            ]]
        case "/api/devices/mac/sessions" where codexHistory:
            body = ["sessions": [historySession()]]
        case Self.codexPath where codexHistory:
            let sent = Self.sentPrompt.withLock { $0 }
            var messages = historyMessages()
            let elapsed = sent.map { $0.at.duration(to: .now) } ?? .zero
            let finished = elapsed >= .seconds(4)
            if let sent {
                messages.append(["id": "turn-5/user", "role": "user", "text": sent.text])
            }
            if elapsed >= .milliseconds(1650) {
                messages.append(["id": "turn-5/comment", "role": "assistant", "text": "I’ll check the workspace and report what I find in the command output."])
            }
            if finished {
                messages.append(["id": "turn-5/command", "role": "activity", "text": "printf 'Workspace check complete'"])
                messages.append(["id": "turn-5/reply", "role": "assistant", "text": "Workspace check complete. The expected build files are present, the command completed normally, and no source files or settings changed."])
            }
            body = ["session": historySession(working: elapsed >= .milliseconds(1650) && !finished), "messages": messages,
                    "olderMessages": false, "approvals": []]
        case Self.codexPath + "/messages" where codexHistory && request.httpMethod == "POST":
            let prompt = requestBody()?["text"] as? String
            Self.sentPrompt.withLock { $0 = prompt.map { ($0, .now) } }
            body = [:]
        case "/api/devices/mac/sessions" where macOnline && request.httpMethod == "GET":
            let older = url.query != nil
            body = ["sessions": [session("codex", id: older ? "mac-older" : "mac-saved",
                                         title: older ? "Older laptop session" : "Saved laptop session", cwd: "/Users/test/repo")],
                    "nextCursor": older ? NSNull() : "older" as Any]
        case "/api/devices/mac/sessions" where macOnline && request.httpMethod == "POST":
            body = session("codex", id: "codex-mac", title: "New laptop Codex session", cwd: "/Users/test/repo")
        case "/api/devices/mac/sessions/codex-mac" where macOnline, "/api/devices/mac/sessions/mac-opened" where macOnline:
            let id = url.lastPathComponent
            let summary = session("codex", id: id, title: id == "codex-mac" ? "New laptop Codex session" : "Opened laptop session",
                                  cwd: "/Users/test/repo", status: "working")
            body = ["session": summary, "messages": [["id": "reply", "role": "assistant", "text": "Fixture transcript"]],
                    "olderMessages": false, "approvals": []]
        case "/api/devices/mac/sessions" where !macOnline:
            status = 503
            body = ["error": "This device is offline."]
        case "/api/devices/linux/sessions" where linuxOnline && request.httpMethod == "POST":
            if let input = requestBody(), let agent = input["agent"] as? String,
               ["claude", "codex"].contains(agent), input["cwd"] as? String == "/srv/project" {
                body = linuxSession(agent)
            } else {
                status = 400
                body = ["error": "Invalid create fixture request."]
            }
        case "/api/devices/linux/sessions" where linuxOnline && request.httpMethod == "GET":
            body = ["sessions": []]
        case "/api/devices/linux/models", "/api/devices/mac/models":
            body = ["models": []]
        case "/api/devices/linux/sessions/claude-linux", "/api/devices/linux/sessions/codex-linux":
            let agent = paths.last == "claude-linux" ? "claude" : "codex"
            body = ["session": linuxSession(agent), "messages": [], "olderMessages": false, "approvals": []]
        default:
            status = 404
            body = ["error": "Unexpected fixture request."]
        }
        do {
            let data = try JSONSerialization.data(withJSONObject: body)
            let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: nil,
                                           headerFields: ["Content-Type": "application/json"])!
            let respond: @Sendable () -> Void = {
                self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                self.client?.urlProtocol(self, didLoad: data)
                self.client?.urlProtocolDidFinishLoading(self)
            }
            if codexHistory && url.path.hasPrefix(Self.codexPath) {
                DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(50), execute: respond)
            } else {
                respond()
            }
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    private func historySession(working: Bool = false) -> [String: Any] {
        session("codex", id: "mac-codex-history", title: "Laptop Codex history", cwd: "/Users/test/repo",
                status: working ? "working" : "ready")
            .merging(["canSend": !working, "model": "gpt-5.4"]) { _, value in value }
    }

    private func historyMessages() -> [[String: String]] {
        (1...4).flatMap { turn in
            func message(_ item: String, _ role: String, _ text: String) -> [String: String] {
                ["id": "turn-\(turn)/\(item)", "role": role, "text": text]
            }
            let prompt = String(String(repeating: "Confirm the expected files and report the result. ", count: 5)
                .prefix([203, 235, 190, 236][turn - 1]))
            let final = """
            I checked the workspace and reviewed the build output.

            - Build files: `Package.swift` and `Sources/App.swift` are present and ready for further inspection.
            - Check status: the command completed normally.

            No files were changed. If you want, I can run the remaining checks and summarize their results.
            """
            var items = [message("user", "user", prompt),
                         message("comment", "assistant", "I’ll check the workspace and report what I find in the output.")]
            if turn == 1 || turn == 3 {
                items.append(message("command", "activity", "cd /fixture/workspace/project && printf 'Workspace check complete: expected files are present'"))
            }
            return items + [message("reply", "assistant", final)]
        }
    }

    private func linuxSession(_ agent: String) -> [String: Any] {
        session(agent, id: "\(agent)-linux", title: agent == "claude" ? "Linux Claude Code session" : "Linux Codex session", cwd: "/srv/project")
    }

    private func session(_ agent: String, id: String, title: String, cwd: String, status: String = "ready") -> [String: Any] {
        ["id": id, "agent": agent, "title": title, "project": "repo", "cwd": cwd,
         "status": status, "canSend": true, "canStop": true, "updatedAt": 1]
    }

    private func requestBody() -> [String: Any]? {
        var data = request.httpBody ?? Data()
        if data.isEmpty, let stream = request.httpBodyStream {
            stream.open()
            defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 1024)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                guard count > 0 else { break }
                data.append(contentsOf: buffer.prefix(count))
            }
        }
        return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }
}
#endif
