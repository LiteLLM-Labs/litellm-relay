import Foundation
import Observation

actor RemoteTransport {
    private let baseURL: URL
    private let session: URLSession
    private let token: String?

    init(baseURL: URL, token: String?, session: URLSession? = nil) {
        self.baseURL = baseURL
        self.token = token
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        self.session = session ?? URLSession(configuration: configuration)
    }

    func request<Value: Decodable & Sendable>(
        _ path: String, method: String = "GET", body: [String: String] = [:], as type: Value.Type
    ) async -> Result<Value, RemoteFailure> {
        guard let url = URL(string: path, relativeTo: baseURL)?.absoluteURL,
              url.host == baseURL.host, url.port == baseURL.port, url.scheme == baseURL.scheme
        else { return .failure(RemoteFailure(message: "Invalid workspace address.", status: nil)) }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("ios", forHTTPHeaderField: "X-LiteLLM-Client")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        do {
            if method == "POST" {
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                request.httpBody = try JSONEncoder().encode(body)
            }
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else { return .failure(RemoteFailure(message: "Your workspace returned an unreadable response.", status: nil)) }
            guard (200..<300).contains(http.statusCode) else {
                let reply = try? JSONDecoder().decode(ErrorReply.self, from: data)
                return .failure(RemoteFailure(message: reply?.error ?? "This request couldn’t be completed.", status: http.statusCode))
            }
            return .success(try JSONDecoder().decode(type, from: data))
        } catch let error as URLError where error.code == .cancelled {
            return .failure(RemoteFailure(message: "Request cancelled.", status: 499))
        } catch is CancellationError {
            return .failure(RemoteFailure(message: "Request cancelled.", status: 499))
        } catch is DecodingError {
            return .failure(RemoteFailure(message: "Update the app to connect to this workspace.", status: nil))
        } catch {
            #if DEBUG
            NSLog("Remote request %@ failed with code %ld", path, (error as NSError).code)
            #endif
            return .failure(RemoteFailure(message: "Couldn’t reach your workspace. Check your connection and try again.", status: nil))
        }
    }
}

@MainActor @Observable
final class RemoteStore {
    var address: String
    var gatewayAddress: String
    var serviceOverride: String
    var configuration: ServiceConfiguration?
    var devices: [RegisteredDevice] = []
    var selectedDeviceId: String?
    var sessions: [CodingSession] = []
    var nextCursor: String?
    var login: LoginStart?
    var teams: [LoginTeam] = []
    var busy = false
    var restoring: Bool
    var hasSavedSession = false
    var loading = false
    var error: String?
    private var transport: RemoteTransport?
    private let defaults: UserDefaults
    private let session: URLSession?

    init(defaults: UserDefaults = .standard, session: URLSession? = nil) {
        self.defaults = defaults
        self.session = session
        self.address = defaults.string(forKey: "workspaceAddress")
            ?? ""
        let gateway = defaults.string(forKey: "gatewayAddress") ?? ""
        self.gatewayAddress = gateway
        self.serviceOverride = defaults.string(forKey: "serviceOverride") ?? ""
        self.restoring = !gateway.isEmpty
    }

    var selectedDevice: RegisteredDevice? { devices.first { $0.id == selectedDeviceId } }

    func restore() async {
        guard configuration?.identity == nil, login == nil, !busy else { return }
        defer { restoring = false }
        guard let service = savedService,
              SessionVault.load(workspace: service.absoluteString) != nil else { return }
        await connect()
    }

    private var savedService: URL? {
        guard let gateway = normalizedGateway(gatewayAddress),
              gateway == normalizedGateway(defaults.string(forKey: "gatewayAddress") ?? ""),
              serviceOverride == (defaults.string(forKey: "serviceOverride") ?? "") else { return nil }
        return serviceAddress(address)
    }

    func connect() async {
        guard let gateway = normalizedGateway(gatewayAddress) else { error = "Enter your LiteLLM gateway’s HTTPS address."; return }
        busy = true
        defer { busy = false }
        error = nil
        let override = serviceOverride
        let resolved: Result<URL, RemoteFailure> = if let service = savedService { .success(service) } else { await resolveService(gateway: gateway) }
        guard case .success(let url) = resolved else {
            if case .failure(let failure) = resolved { error = failure.message }
            return
        }
        let normalized = url.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let token = SessionVault.load(workspace: normalized)
        hasSavedSession = token != nil
        let connection = RemoteTransport(baseURL: url, token: token, session: session)
        let result = await connection.request("/api/me", as: ServiceConfiguration.self)
        guard normalizedGateway(gatewayAddress) == gateway, serviceOverride == override else { return }
        switch result {
        case .success(let value):
            guard normalizedGateway(value.gateway) == gateway else {
                error = "This remote service is connected to a different gateway. Check the address with your administrator."
                return
            }
            transport = connection
            configuration = value
            address = normalized
            if value.identity == nil {
                hasSavedSession = false
                if token != nil { SessionVault.remove(workspace: normalized) }
            }
            defaults.set(address, forKey: "workspaceAddress")
            defaults.set(gateway.absoluteString, forKey: "gatewayAddress")
            defaults.set(serviceOverride, forKey: "serviceOverride")
        case .failure(let failure):
            if hasSavedSession { configuration = nil }
            error = failure.message
        }
    }

    func startLogin() async {
        guard let transport else { return }
        busy = true
        error = nil
        switch await transport.request("/api/auth/start", method: "POST", as: LoginStart.self) {
        case .success(let value):
            guard let gateway = normalizedGateway(gatewayAddress),
                  value.verificationUrl.scheme == gateway.scheme, value.verificationUrl.host == gateway.host,
                  value.verificationUrl.port == gateway.port else {
                error = "The sign-in page does not belong to your gateway."
                busy = false
                return
            }
            login = value
        case .failure(let failure): error = failure.message
        }
        busy = false
    }

    func pollLogin(team: String? = nil) async {
        guard let transport, let login else { return }
        let input = ["flowToken": login.flowToken].merging(team.map { ["team": $0] } ?? [:]) { _, value in value }
        switch await transport.request("/api/auth/poll", method: "POST", body: input, as: LoginPoll.self) {
        case .success(let value):
            if value.status == "teams" { teams = value.teams ?? []; return }
            guard value.status == "ready", let token = value.token else { return }
            guard SessionVault.save(token, workspace: address) else { error = "Couldn’t securely save your sign-in. Please try again."; return }
            hasSavedSession = true
            await connect()
            self.login = nil
            teams = []
        case .failure(let failure):
            if failure.status == 410 { self.login = nil }
            error = failure.message
        }
    }

    func refreshDevices() async {
        guard let transport else { return }
        switch await transport.request("/api/devices", as: DevicePage.self) {
        case .success(let value):
            devices = value.devices
            if !devices.contains(where: { $0.id == selectedDeviceId }) {
                selectedDeviceId = devices.first(where: \.online)?.id ?? devices.first?.id
                sessions = []
            }
            error = nil
        case .failure(let failure): await handle(failure)
        }
    }

    func selectDevice(_ id: String) async {
        selectedDeviceId = id
        sessions = []
        nextCursor = nil
        await refresh()
    }

    func refresh(older: Bool = false) async {
        guard let transport, let deviceId = selectedDeviceId else { return }
        loading = true
        let base = "/api/devices/\(deviceId)/sessions"
        let path = older ? nextCursor.map { queryPath(base, name: "cursor", value: $0) } ?? base : base
        switch await transport.request(path, as: SessionPage.self) {
        case .success(let value):
            if deviceId == selectedDeviceId {
                sessions = older ? sessions + value.sessions.filter { incoming in !sessions.contains { $0.id == incoming.id } } : value.sessions
                nextCursor = value.nextCursor
                error = value.warnings.flatMap { $0.isEmpty ? nil : $0.joined(separator: "\n") }
            }
        case .failure(let failure): if deviceId == selectedDeviceId { await handle(failure) }
        }
        loading = false
    }

    func detail(id: String, deviceId: String) async -> Result<SessionDetail, RemoteFailure> {
        guard let transport else { return .failure(disconnected) }
        return await transport.request("/api/devices/\(deviceId)/sessions/\(id)", as: SessionDetail.self)
    }

    func create(cwd: String, agent: String, deviceId: String, requestId: String) async -> Result<CodingSession, RemoteFailure> {
        guard let transport else { return .failure(disconnected) }
        return await transport.request("/api/devices/\(deviceId)/sessions", method: "POST", body: ["cwd": cwd, "agent": agent, "requestId": requestId], as: CodingSession.self)
    }

    func models(deviceId: String, cursor: String? = nil) async -> Result<ModelPage, RemoteFailure> {
        guard let transport else { return .failure(disconnected) }
        let base = "/api/devices/\(deviceId)/models"
        return await transport.request(cursor.map { queryPath(base, name: "cursor", value: $0) } ?? base, as: ModelPage.self)
    }

    func send(id: String, deviceId: String, text: String, requestId: String, model: String? = nil) async -> Result<EmptyReply, RemoteFailure> {
        guard let transport else { return .failure(disconnected) }
        let input = ["text": text, "requestId": requestId].merging(model.map { ["model": $0] } ?? [:]) { _, value in value }
        return await transport.request("/api/devices/\(deviceId)/sessions/\(id)/messages", method: "POST", body: input, as: EmptyReply.self)
    }

    func interrupt(id: String, deviceId: String, requestId: String) async -> Result<EmptyReply, RemoteFailure> {
        guard let transport else { return .failure(disconnected) }
        return await transport.request("/api/devices/\(deviceId)/sessions/\(id)/interrupt", method: "POST", body: ["requestId": requestId], as: EmptyReply.self)
    }

    func approve(id: String, deviceId: String, approvalId: String, decision: String) async -> Result<EmptyReply, RemoteFailure> {
        guard let transport else { return .failure(disconnected) }
        return await transport.request("/api/devices/\(deviceId)/sessions/\(id)/approvals/\(approvalId)", method: "POST", body: ["decision": decision], as: EmptyReply.self)
    }

    func removeDevice(_ id: String) async {
        guard let transport else { return }
        switch await transport.request("/api/devices/\(id)", method: "DELETE", as: EmptyReply.self) {
        case .success: await refreshDevices()
        case .failure(let failure): await handle(failure)
        }
    }

    func signOut() async {
        guard let transport else { return }
        switch await transport.request("/api/auth/logout", method: "POST", as: EmptyReply.self) {
        case .success: disconnect()
        case .failure(let failure):
            if failure.status == 401 { disconnect() }
            else { error = "Couldn’t revoke this sign-in. Reconnect and try again." }
        }
    }

    func disconnect() {
        SessionVault.remove(workspace: address)
        hasSavedSession = false
        transport = nil
        configuration = nil
        selectedDeviceId = nil
        devices = []
        sessions = []
        nextCursor = nil
        login = nil
        teams = []
        error = nil
    }

    private var disconnected: RemoteFailure { RemoteFailure(message: "Sign in to your workspace first.", status: 401) }

    private func resolveService(gateway: URL) async -> Result<URL, RemoteFailure> {
        if !serviceOverride.isEmpty {
            guard let url = serviceAddress(serviceOverride) else { return .failure(RemoteFailure(message: "Enter a valid HTTPS remote-service address.", status: nil)) }
            return .success(url)
        }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 10
        let discovery = session ?? URLSession(configuration: configuration)
        defer { if session == nil { discovery.invalidateAndCancel() } }
        do {
            let (data, response) = try await discovery.data(from: gateway.appendingPathComponent(".well-known/litellm-remote.json"))
            guard (response as? HTTPURLResponse)?.statusCode == 200,
                  let manifest = try? JSONDecoder().decode(WorkspaceDiscovery.self, from: data),
                  normalizedGateway(manifest.gatewayUrl) == gateway,
                  let url = serviceAddress(manifest.controlUrl), url.scheme == "https" else {
                return .failure(RemoteFailure(message: "Remote control isn’t configured at this gateway yet. Add your remote-service address under Advanced.", status: nil))
            }
            return .success(url)
        } catch {
            return .failure(RemoteFailure(message: "Couldn’t discover remote control. Check the gateway address or add a service address under Advanced.", status: nil))
        }
    }

    private func handle(_ failure: RemoteFailure) async {
        if failure.status == 499 { return }
        if failure.status == 401 { disconnect() }
        error = failure.message
    }
}
