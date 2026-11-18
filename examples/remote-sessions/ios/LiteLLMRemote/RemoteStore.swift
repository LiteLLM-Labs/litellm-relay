import Foundation
import Observation

actor BridgeTransport {
    private let baseURL: URL
    private let session: URLSession

    init(baseURL: URL) {
        self.baseURL = baseURL
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 15
        configuration.httpShouldSetCookies = true
        configuration.httpCookieAcceptPolicy = .always
        self.session = URLSession(configuration: configuration)
    }

    func request<Value: Decodable & Sendable>(
        _ path: String,
        method: String = "GET",
        as type: Value.Type
    ) async -> Result<Value, RemoteFailure> {
        guard let url = URL(string: path, relativeTo: baseURL)?.absoluteURL,
              url.host == baseURL.host, url.port == baseURL.port,
              url.scheme == baseURL.scheme
        else { return .failure(RemoteFailure(message: "Invalid bridge address.", status: nil)) }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("ios", forHTTPHeaderField: "X-LiteLLM-Client")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if method == "POST" {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = Data("{}".utf8)
        }
        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else {
                return .failure(RemoteFailure(message: "The Mac returned an unreadable response.", status: nil))
            }
            guard (200..<300).contains(http.statusCode) else {
                let reply = try? JSONDecoder().decode(ErrorReply.self, from: data)
                return .failure(RemoteFailure(message: reply?.error ?? "The bridge could not complete this request.", status: http.statusCode))
            }
            return .success(try JSONDecoder().decode(type, from: data))
        } catch is DecodingError {
            return .failure(RemoteFailure(message: "This bridge response is not supported by the app yet.", status: nil))
        } catch {
            return .failure(RemoteFailure(message: "Cannot reach your Mac. Keep it awake and connect both devices to the same Wi-Fi.", status: nil))
        }
    }
}

@MainActor @Observable
final class RemoteStore {
    var address: String
    var configuration: BridgeConfiguration?
    var sessions: [CodingSession] = []
    var nextCursor: String?
    var login: LoginStart?
    var teams: [LoginTeam] = []
    var device: DeviceInfo?
    var busy = false
    var error: String?
    private var transport: BridgeTransport?

    init() {
        #if targetEnvironment(simulator)
        self.address = UserDefaults.standard.string(forKey: "bridgeAddress") ?? "http://127.0.0.1:4174"
        #else
        self.address = UserDefaults.standard.string(forKey: "bridgeAddress") ?? (Bundle.main.object(forInfoDictionaryKey: "LiteLLMBridgeURL") as? String) ?? ""
        #endif
    }

    func connect() async {
        guard let url = bridgeAddress(address) else {
            error = "Enter your Mac’s address, including http:// and the port."
            return
        }
        busy = true
        error = nil
        let connection = BridgeTransport(baseURL: url)
        switch await connection.request("/api/me", as: BridgeConfiguration.self) {
        case .success(let value):
            transport = connection
            configuration = value
            address = url.absoluteString
            UserDefaults.standard.set(address, forKey: "bridgeAddress")
            if value.identity != nil { await refresh() }
        case .failure(let failure): error = failure.message
        }
        busy = false
    }

    func startLogin() async {
        guard let transport else { return }
        busy = true
        error = nil
        switch await transport.request("/api/auth/start", method: "POST", as: LoginStart.self) {
        case .success(let value): login = value
        case .failure(let failure): error = failure.message
        }
        busy = false
    }

    func pollLogin(team: String? = nil) async {
        guard let transport, login != nil else { return }
        let path = team.map { queryPath("/api/auth/poll", name: "team", value: $0) } ?? "/api/auth/poll"
        switch await transport.request(path, as: LoginPoll.self) {
        case .success(let value):
            if value.status == "teams" { teams = value.teams ?? []; return }
            guard value.status == "ready" else { return }
            login = nil
            teams = []
            await reloadIdentity()
        case .failure(let failure): error = failure.message
        }
    }

    func localLogin() async {
        guard let transport else { return }
        busy = true
        error = nil
        switch await transport.request("/api/auth/local", method: "POST", as: EmptyReply.self) {
        case .success: await reloadIdentity()
        case .failure(let failure): error = failure.message
        }
        busy = false
    }

    private func reloadIdentity() async {
        guard let transport else { return }
        switch await transport.request("/api/me", as: BridgeConfiguration.self) {
        case .success(let value):
            configuration = value
            error = nil
            if value.identity != nil { await refresh() }
        case .failure(let failure): error = failure.message
        }
    }

    func refresh(older: Bool = false) async {
        guard let transport else { return }
        let path = older ? nextCursor.map { queryPath("/api/sessions", name: "cursor", value: $0) } ?? "/api/sessions" : "/api/sessions"
        switch await transport.request(path, as: SessionPage.self) {
        case .success(let value):
            sessions = older ? sessions + value.sessions.filter { incoming in !sessions.contains { $0.id == incoming.id } } : value.sessions
            nextCursor = value.nextCursor
            error = nil
        case .failure(let failure):
            error = failure.message
            if failure.status == 401 { await reloadIdentity() }
        }
    }

    func detail(id: String) async -> Result<SessionDetail, RemoteFailure> {
        guard let transport else { return .failure(RemoteFailure(message: "Connect to your Mac first.", status: nil)) }
        return await transport.request("/api/sessions/\(id)", as: SessionDetail.self)
    }

    func loadDevice() async {
        guard let transport else { return }
        switch await transport.request("/api/device", as: DeviceInfo.self) {
        case .success(let value): device = value
        case .failure(let failure): error = failure.message
        }
    }

    func signOut() async {
        guard let transport else { return }
        switch await transport.request("/api/auth/logout", method: "POST", as: EmptyReply.self) {
        case .success:
            sessions = []
            device = nil
            login = nil
            await reloadIdentity()
        case .failure(let failure): error = failure.message
        }
    }

    func disconnect() {
        transport = nil
        configuration = nil
        sessions = []
        login = nil
        teams = []
        device = nil
        error = nil
    }
}
