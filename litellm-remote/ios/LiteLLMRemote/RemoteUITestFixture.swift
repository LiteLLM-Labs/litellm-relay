#if DEBUG
import Foundation

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
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        guard let url = request.url else { return }
        let mode = request.value(forHTTPHeaderField: "X-Fixture-Mode")
        let macOnline = mode == "online"
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
        case "/api/devices/mac/sessions" where macOnline && request.httpMethod == "GET":
            let older = url.query != nil
            body = ["sessions": [session("codex", id: older ? "mac-older" : "mac-saved",
                                         title: older ? "Older laptop session" : "Saved laptop session", cwd: "/Users/test/repo")],
                    "nextCursor": older ? NSNull() : "older" as Any]
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
        case "/api/devices/linux/models":
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
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
        }
    }

    private func linuxSession(_ agent: String) -> [String: Any] {
        session(agent, id: "\(agent)-linux", title: agent == "claude" ? "Linux Claude Code session" : "Linux Codex session", cwd: "/srv/project")
    }

    private func session(_ agent: String, id: String, title: String, cwd: String) -> [String: Any] {
        ["id": id, "agent": agent, "title": title, "project": "repo", "cwd": cwd,
         "status": "ready", "canSend": true, "canStop": true, "updatedAt": 1]
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
