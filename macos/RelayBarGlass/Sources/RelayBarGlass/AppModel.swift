import AppKit
import Foundation

@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var status: RelayStatus?
    @Published private(set) var daemonUp = false
    @Published private(set) var busy = false
    @Published private(set) var lastError: CommandError?
    @Published private(set) var now = Date()

    let relayPort: Int
    private let binary: URL
    private let session: URLSession
    private var pollTimer: Timer?
    private var tickTimer: Timer?

    init(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        home: URL = RelayHome.locate(environment: ProcessInfo.processInfo.environment)
    ) {
        let configPath = home.appendingPathComponent(".litellm-relay/config.yaml")
        let configYAML = (try? String(contentsOf: configPath, encoding: .utf8)) ?? ""
        relayPort = StatusFormatter.relayPort(configYAML: configYAML)
        binary = RelayBinary.locate(environment: environment, home: home)
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 3
        session = URLSession(configuration: configuration)

        let statusTimer = Timer(timeInterval: 5, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.poll() }
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

    var dashboardURL: URL? {
        URL(string: "http://127.0.0.1:\(relayPort)/")
    }

    var signedIn: Bool {
        status?.broker?.signedIn == true
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
        guard let url = URL(string: "http://127.0.0.1:\(relayPort)/api/status") else { return }
        do {
            let (data, response) = try await session.data(from: url)
            guard let http = response as? HTTPURLResponse, (200...299).contains(http.statusCode) else {
                daemonUp = false
                status = nil
                return
            }
            daemonUp = true
            status = try? RelayStatus.decode(data)
        } catch {
            daemonUp = false
            status = nil
        }
    }

    func run(_ command: RelayCommand) {
        guard !busy else { return }
        busy = true
        lastError = nil
        let binary = self.binary
        Task {
            let result = await ProcessRunner.run(binary: binary, arguments: command.arguments)
            if result.exitCode != 0 {
                lastError = CommandError(
                    command: command,
                    message: StatusFormatter.failureMessage(stderr: result.stderr, exitCode: result.exitCode)
                )
            }
            await poll()
            busy = false
        }
    }

    func openDashboard() {
        guard let dashboardURL else { return }
        NSWorkspace.shared.open(dashboardURL)
    }

    func quit() {
        NSApplication.shared.terminate(nil)
    }
}
