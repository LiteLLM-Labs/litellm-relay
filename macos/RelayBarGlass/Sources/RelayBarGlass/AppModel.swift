import AppKit
import Foundation

enum DaemonState: Equatable {
    case checking
    case up
    case down
}

struct AppTiming: Equatable {
    let pollInterval: TimeInterval
    let requestTimeout: TimeInterval
    let commandTimeout: TimeInterval

    static let standard = AppTiming(pollInterval: 5, requestTimeout: 12, commandTimeout: 30)
}

@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var status: RelayStatus?
    @Published private(set) var daemon = DaemonState.checking
    @Published private(set) var busy = false
    @Published private(set) var signingIn = false
    @Published private(set) var lastError: CommandError?
    @Published private(set) var now = Date()
    @Published private(set) var relayPort: Int

    private let configPath: URL
    private let binary: URL
    private let session: URLSession
    private let commandTimeout: TimeInterval
    private let quitIntent: QuitIntent
    private let terminate: @MainActor () -> Void
    private var pollTimer: Timer?
    private var tickTimer: Timer?
    private var signInTask: Task<Void, Never>?
    private var queued: RelayCommand?
    private var pollsStarted = 0
    private var pollsApplied = 0
    private var pollsRunning = 0

    init(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        home: URL = RelayHome.locate(environment: ProcessInfo.processInfo.environment),
        timing: AppTiming = .standard,
        quitIntent: QuitIntent = .shared,
        terminate: @escaping @MainActor () -> Void = { NSApplication.shared.terminate(nil) }
    ) {
        self.quitIntent = quitIntent
        self.terminate = terminate
        configPath = home.appendingPathComponent(".litellm-relay/config.yaml")
        relayPort = Self.configuredPort(configPath)
        binary = RelayBinary.locate(environment: environment, home: home)
        commandTimeout = timing.commandTimeout
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = timing.requestTimeout
        session = URLSession(configuration: configuration)

        let statusTimer = Timer(timeInterval: timing.pollInterval, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.pollUnlessRunning() }
        }
        RunLoop.main.add(statusTimer, forMode: .common)
        pollTimer = statusTimer
        let countdownTimer = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.now = Date() }
        }
        RunLoop.main.add(countdownTimer, forMode: .common)
        tickTimer = countdownTimer
        Task { await poll() }
    }

    var daemonUp: Bool {
        daemon == .up
    }

    var dashboardURL: URL? {
        URL(string: "http://127.0.0.1:\(relayPort)/")
    }

    var signedIn: Bool {
        status?.broker?.signedIn == true
    }

    var offersSignOut: Bool {
        signedIn && status?.usesStaticKey != true
    }

    var offersSelection: Bool {
        status?.usesStaticKey != true
    }

    var keyExpiresAt: Date? {
        StatusFormatter.timestamp(status?.broker?.keyExpiresAt)
    }

    var teamOptions: [TeamEntry] {
        if let teams = status?.account?.teams, !teams.isEmpty { return teams }
        guard let current = status?.broker?.team else { return [] }
        return [TeamEntry(id: current, alias: status?.account?.team?.alias)]
    }

    var environmentOptions: [EnvironmentEntry] {
        status?.environments?.available ?? []
    }

    var mcpServers: [(name: String, server: McpServer)] {
        (status?.mcp?.servers ?? [:])
            .map { (name: $0.key, server: $0.value) }
            .sorted { $0.name < $1.name }
    }

    func errorMessage(for kind: RelayCommand.Kind) -> String? {
        guard let lastError, lastError.command.kind == kind else { return nil }
        return lastError.message
    }

    func poll() async {
        pollsStarted += 1
        pollsRunning += 1
        let ticket = pollsStarted
        let data = await readStatusRetryingOnce()
        pollsRunning -= 1
        guard ticket > pollsApplied else { return }
        pollsApplied = ticket
        guard let data else {
            daemon = .down
            status = nil
            return
        }
        daemon = .up
        status = try? RelayStatus.decode(data)
    }

    func run(_ command: RelayCommand) {
        if command == .signIn { return signIn() }
        guard !busy else {
            queued = command
            return
        }
        busy = true
        lastError = nil
        let binary = self.binary
        let timeout = commandTimeout
        Task {
            let outcome = await ProcessRunner.run(binary: binary, arguments: command.arguments, timeout: timeout)
            lastError = StatusFormatter.commandFailure(outcome).map { CommandError(command: command, message: $0) }
            await poll()
            busy = false
            guard let next = queued else { return }
            queued = nil
            run(next)
        }
    }

    func cancelSignIn() {
        signInTask?.cancel()
        signInTask = nil
        signingIn = false
    }

    func openDashboard() {
        guard let dashboardURL else { return }
        NSWorkspace.shared.open(dashboardURL)
    }

    func quit() {
        quitIntent.record(.userQuit)
        terminate()
    }

    private func signIn() {
        signInTask?.cancel()
        signingIn = true
        lastError = nil
        let binary = self.binary
        signInTask = Task {
            let outcome = await ProcessRunner.run(binary: binary, arguments: RelayCommand.signIn.arguments, timeout: nil)
            guard !Task.isCancelled else { return }
            signInTask = nil
            signingIn = false
            lastError = StatusFormatter.commandFailure(outcome).map { CommandError(command: .signIn, message: $0) }
            await poll()
        }
    }

    private func pollUnlessRunning() async {
        guard pollsRunning == 0 else { return }
        await poll()
    }

    private func readStatusRetryingOnce() async -> Data? {
        if let data = await readStatus() { return data }
        return await readStatus()
    }

    private func readStatus() async -> Data? {
        relayPort = Self.configuredPort(configPath)
        guard let url = URL(string: "http://127.0.0.1:\(relayPort)/api/status"),
              let (data, response) = try? await session.data(from: url),
              let http = response as? HTTPURLResponse,
              (200...299).contains(http.statusCode)
        else { return nil }
        return data
    }

    private static func configuredPort(_ configPath: URL) -> Int {
        StatusFormatter.relayPort(configYAML: (try? String(contentsOf: configPath, encoding: .utf8)) ?? "")
    }
}
