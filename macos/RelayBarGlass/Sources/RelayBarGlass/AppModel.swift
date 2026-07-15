import SwiftUI
import Foundation
import AppKit
import Darwin

/// Describes a single coding agent shown in the menu bar UI: its brand chrome
/// (icon / accent / fallback), its live usage meters, the model it is routed
/// to, and its cost rollups. Usage-window and cost numbers here are seeded
/// placeholders — the real per-agent usage/cost APIs aren't wired up yet, so
/// these are realistic prototype values, not live data.
struct AgentInfo: Identifiable {
    var id: String            // tag, e.g. "claude-code"
    var name: String          // "Claude"
    var iconResource: String? // bundled SVG basename e.g. "claudecode-color"; nil => fallback chip
    var fallbackText: String  // e.g. "DR"
    var accent: Color         // meter/accent color
    var meter: Double         // 0...1 mini usage meter for the tab
    var routedNote: String    // "Routed through Relay just now"
    var sessionUsed: Double   // 0...1
    var sessionReset: String  // "Resets in 3h 53m"
    var weeklyUsed: Double    // 0...1
    var weeklyReset: String   // "Resets in 3d 20h"
    var paceNote: String      // "Pace: Behind (-42%) · Lasts to reset" ("" to hide)
    var modelLabel: String    // "Claude model"
    var modelSubtitle: String // "Anthropic traffic route"
    var modelOptions: [String]
    var selectedModel: String
    var costToday: String     // "$0.04 · 15K tokens"
    var costMonth: String     // "$254.24 · 218M tokens"
    var tokensToday: Int = 0  // real per-key, per-tool token count for today
    var tokensMonth: Int = 0  // real per-key, per-tool token count for the range
    var dailySpend: [DailyPoint] = []  // this tool's spend per day (ascending), this key only
    var cacheHitRate: Double = 0       // this tool: sum cache_read_input_tokens / sum prompt_tokens (0...1)
    var modelMix: [ModelSlice] = []    // top 3 models for this tool by spend
    var spendMonth: Double = 0     // this key+tool, summed over the returned days
    var spendToday: Double = 0     // this key+tool, the end-date day
    var successRate: Double = 0    // sum successful_requests / sum api_requests (0...1), this key+tool
    var requests: Int = 0          // sum api_requests, this key+tool
    var costPerReq: Double = 0     // spendMonth / successfulRequests (guard 0)
    var topModel: String = ""      // this tool's highest-spend model for this key, SHORTENED
    var routedViaRelay: Bool = false // true only for tools that received real relay data this refresh
}

/// A single day's total spend, used to draw the account usage sparkline.
struct DailyPoint: Identifiable {
    let id = UUID()
    let date: String
    let spend: Double
}

/// A single model's spend within a tool, used to draw the per-tab model mix.
struct ModelSlice: Identifiable {
    let id = UUID()
    let model: String
    let spend: Double
}

@MainActor
final class AppModel: ObservableObject {
    @Published var relayUp: Bool = false
    @Published var routingOn: Bool = false
    @Published var gatewayHost: String = "gateway.litellm-sandbox.ai"
    @Published var agents: [AgentInfo]
    @Published var selectedTag: String = "claude-code"

    // MARK: - Account usage card (populated live from the gateway)

    @Published var accountTodaySpend: Double = 0
    @Published var accountMonthSpend: Double = 0
    @Published var accountTodayTokens: Int = 0
    @Published var accountMonthTokens: Int = 0
    @Published var topModel: String = ""
    @Published var dailySpend: [DailyPoint] = []
    @Published var keyAlias: String = ""
    @Published var lastError: String = ""

    // MARK: - Per-key budget (from /key/info)

    @Published var keySpend: Double = 0       // info.spend
    @Published var keyBudget: Double = 0      // info.max_budget (0 = no budget)
    @Published var budgetResetAt: String = "" // info.budget_reset_at

    // MARK: - Account insight metrics (account-wide, summed across returned days)

    @Published var apiRequests: Int = 0          // sum metrics.api_requests
    @Published var successfulRequests: Int = 0   // sum metrics.successful_requests
    @Published var failedRequests: Int = 0       // sum metrics.failed_requests
    @Published var successRate: Double = 0        // successful / api_requests (0...1)
    @Published var cacheHitRate: Double = 0       // sum cache_read_input_tokens / sum prompt_tokens (0...1)
    @Published var costPerRequest: Double = 0     // accountMonthSpend / successfulRequests
    @Published var costPerMTok: Double = 0        // accountMonthSpend / (accountMonthTokens/1e6)

    /// The sha256 hash identifying our own key, as returned by `/key/info`.
    /// Used to index into the per-key breakdown of the daily-activity report.
    private var keyHash: String = ""

    /// Repeating poll timer (5s). Retained so `start()` can replace it safely.
    private var timer: Timer?

    /// The currently selected agent, falling back to the first agent if the
    /// selected tag doesn't resolve (should not happen with seeded data).
    var selected: AgentInfo {
        agents.first(where: { $0.id == selectedTag }) ?? agents[0]
    }

    init() {
        self.agents = AppModel.seedAgents()
        // Drive polling from the model, not from a view modifier: the
        // MenuBarExtra label's .onAppear does not fire under
        // .menuBarExtraStyle(.window), which left the timer uncreated and the
        // popover stuck on its seed values.
        start()
    }

    // MARK: - Seed data

    private static func seedAgents() -> [AgentInfo] {
        [
            AgentInfo(
                id: "codex-cli",
                name: "Codex CLI",
                iconResource: "codex",
                fallbackText: "CX",
                accent: hexColor(0x8B5CF6),
                meter: 0.64,
                routedNote: "Routed through Relay just now",
                sessionUsed: 0.64,
                sessionReset: "Resets in 3h 53m",
                weeklyUsed: 0.58,
                weeklyReset: "Resets in 3d 20h",
                paceNote: "Pace: Behind (-42%) · Lasts to reset",
                modelLabel: "Codex model",
                modelSubtitle: "OpenAI coding traffic route",
                modelOptions: [
                    "openai/gpt-5.3-codex",
                    "openai/gpt-5-codex",
                    "openai/gpt-5.1-codex-max",
                ],
                selectedModel: "openai/gpt-5.3-codex",
                costToday: "$0.62 · 210K tokens",
                costMonth: "$254.24 · 218M tokens"
            ),
            AgentInfo(
                id: "codex-app",
                name: "Codex App",
                iconResource: "openai",
                fallbackText: "CX",
                accent: hexColor(0x7C6BF0),
                meter: 0.0,
                routedNote: "No traffic yet",
                sessionUsed: 0.0,
                sessionReset: "Resets in 8h 00m",
                weeklyUsed: 0.0,
                weeklyReset: "Resets in 7d 0h",
                paceNote: "",
                modelLabel: "Codex model",
                modelSubtitle: "OpenAI coding traffic route",
                modelOptions: [
                    "openai/gpt-5.3-codex",
                    "openai/gpt-5-codex",
                    "openai/gpt-5.1-codex-max",
                ],
                selectedModel: "openai/gpt-5.3-codex",
                costToday: "$0.00 · 0 tokens",
                costMonth: "$0.00 · 0 tokens"
            ),
            AgentInfo(
                id: "claude-code",
                name: "Claude Code",
                iconResource: "claudecode-color",
                fallbackText: "CC",
                accent: hexColor(0xE0863F),
                meter: 0.21,
                routedNote: "Routed through Relay 2m ago",
                sessionUsed: 0.21,
                sessionReset: "Resets in 4h 12m",
                weeklyUsed: 0.34,
                weeklyReset: "Resets in 5d 6h",
                paceNote: "Pace: On track · Lasts to reset",
                modelLabel: "Claude model",
                modelSubtitle: "Anthropic traffic route",
                modelOptions: [
                    "anthropic/claude-opus-4-8",
                    "anthropic/claude-sonnet-5",
                    "claude-code-sonnet-4-6-converse",
                ],
                selectedModel: "anthropic/claude-opus-4-8",
                costToday: "$0.04 · 15K tokens",
                costMonth: "$88.10 · 74M tokens"
            ),
            AgentInfo(
                id: "claude-desktop",
                name: "Claude App",
                iconResource: "claude-color",
                fallbackText: "CC",
                accent: hexColor(0xCF6A3C),
                meter: 0.0,
                routedNote: "No traffic yet",
                sessionUsed: 0.0,
                sessionReset: "Resets in 8h 00m",
                weeklyUsed: 0.0,
                weeklyReset: "Resets in 7d 0h",
                paceNote: "",
                modelLabel: "Claude model",
                modelSubtitle: "Anthropic traffic route",
                modelOptions: [
                    "anthropic/claude-opus-4-8",
                    "anthropic/claude-sonnet-5",
                    "claude-code-sonnet-4-6-converse",
                ],
                selectedModel: "anthropic/claude-opus-4-8",
                costToday: "$0.00 · 0 tokens",
                costMonth: "$0.00 · 0 tokens"
            ),
            AgentInfo(
                id: "cursor",
                name: "Cursor",
                iconResource: "cursor",
                fallbackText: "CU",
                accent: hexColor(0x9CA3AF),
                meter: 0.08,
                routedNote: "Routed through Relay 18m ago",
                sessionUsed: 0.08,
                sessionReset: "Resets in 6h 40m",
                weeklyUsed: 0.12,
                weeklyReset: "Resets in 6d 2h",
                paceNote: "",
                modelLabel: "Cursor model",
                modelSubtitle: "Cursor traffic route",
                modelOptions: [
                    "anthropic/claude-sonnet-5",
                    "openai/gpt-5-codex",
                    "auto",
                ],
                selectedModel: "anthropic/claude-sonnet-5",
                costToday: "$0.00 · 2K tokens",
                costMonth: "$21.40 · 9M tokens"
            ),
            AgentInfo(
                id: "gemini",
                name: "Gemini",
                iconResource: "gemini-color",
                fallbackText: "GM",
                accent: hexColor(0x60A5FA),
                meter: 0.04,
                routedNote: "Routed through Relay 1h ago",
                sessionUsed: 0.04,
                sessionReset: "Resets in 5h 20m",
                weeklyUsed: 0.06,
                weeklyReset: "Resets in 4d 12h",
                paceNote: "",
                modelLabel: "Gemini model",
                modelSubtitle: "Google traffic route",
                modelOptions: [
                    "google/gemini-2.5-pro",
                    "google/gemini-2.5-flash",
                ],
                selectedModel: "google/gemini-2.5-pro",
                costToday: "$0.01 · 5K tokens",
                costMonth: "$12.75 · 20M tokens"
            ),
            AgentInfo(
                id: "copilot",
                name: "Copilot",
                iconResource: "copilot-color",
                fallbackText: "CP",
                accent: hexColor(0xC084FC),
                meter: 0.02,
                routedNote: "Routed through Relay 3h ago",
                sessionUsed: 0.02,
                sessionReset: "Resets in 7h 05m",
                weeklyUsed: 0.03,
                weeklyReset: "Resets in 6d 18h",
                paceNote: "",
                modelLabel: "Copilot model",
                modelSubtitle: "GitHub Copilot traffic route",
                modelOptions: [
                    "openai/gpt-5-codex",
                    "anthropic/claude-sonnet-5",
                ],
                selectedModel: "openai/gpt-5-codex",
                costToday: "$0.00 · 1K tokens",
                costMonth: "$9.99 · 4M tokens"
            ),
        ]
    }

    // MARK: - Lifecycle

    /// Kicks off an immediate refresh and schedules a repeating 5s poll.
    func start() {
        refresh()
        timer?.invalidate()
        let t = Timer(timeInterval: 5.0, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.refresh() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    /// Recomputes `relayUp`, `routingOn`, and `gatewayHost` off the main thread,
    /// then publishes the results back on the main actor.
    func refresh() {
        Task.detached { [weak self] in
            let up = AppModel.canConnect(host: "127.0.0.1", port: 4142, timeout: 0.6)
            let routing = AppModel.readRoutingEnabled()
            let host = AppModel.parseGatewayHost()
            await MainActor.run {
                guard let self else { return }
                self.relayUp = up
                self.routingOn = routing
                if let host, !host.isEmpty {
                    self.gatewayHost = host
                }
            }
        }
        refreshSpend()
    }

    /// Fetches live usage/spend for the user's own key from the gateway and
    /// publishes it into the account-usage card and per-agent cost/token
    /// rollups. All networking and JSON parsing happens off the main actor;
    /// results are applied on `MainActor`. On any failure `lastError` is set
    /// and previously-published values are left untouched.
    func refreshSpend() {
        Task.detached { [weak self] in
            do {
                let snap = try await AppModel.computeSpend()
                await MainActor.run {
                    guard let self else { return }
                    self.apply(snap)
                }
            } catch {
                await MainActor.run { self?.lastError = "\(error)" }
            }
        }
    }

    /// Applies a freshly-fetched spend snapshot onto the published state.
    private func apply(_ snap: SpendSnapshot) {
        keyHash = snap.keyHash
        keyAlias = snap.keyAlias
        keySpend = snap.keySpend
        keyBudget = snap.keyBudget
        budgetResetAt = snap.budgetResetAt
        accountTodaySpend = snap.accountTodaySpend
        accountMonthSpend = snap.accountMonthSpend
        accountTodayTokens = snap.accountTodayTokens
        accountMonthTokens = snap.accountMonthTokens
        topModel = snap.topModel
        dailySpend = snap.dailySpend
        apiRequests = snap.apiRequests
        successfulRequests = snap.successfulRequests
        failedRequests = snap.failedRequests
        successRate = snap.successRate
        cacheHitRate = snap.cacheHitRate
        costPerRequest = snap.costPerRequest
        costPerMTok = snap.costPerMTok
        lastError = ""

        let maxToolSpend = snap.toolMonthSpend.values.max() ?? 0
        for i in agents.indices {
            let id = agents[i].id
            if id == "claude-code" || id == "codex-cli" {
                let monthSpend = snap.toolMonthSpend[id] ?? 0
                let monthTok = snap.toolMonthTokens[id] ?? 0
                let todaySpend = snap.toolTodaySpend[id] ?? 0
                let todayTok = snap.toolTodayTokens[id] ?? 0
                agents[i].costMonth = AppModel.fmtCost(monthSpend, monthTok)
                agents[i].costToday = AppModel.fmtCost(todaySpend, todayTok)
                agents[i].tokensMonth = monthTok
                agents[i].tokensToday = todayTok
                agents[i].meter = maxToolSpend > 0 ? monthSpend / maxToolSpend : 0
                agents[i].dailySpend = snap.toolDailySpend[id] ?? []
                agents[i].cacheHitRate = snap.toolCacheHitRate[id] ?? 0
                agents[i].modelMix = snap.toolModelMix[id] ?? []
                agents[i].spendMonth = monthSpend
                agents[i].spendToday = todaySpend
                agents[i].successRate = snap.toolSuccessRate[id] ?? 0
                agents[i].requests = snap.toolRequests[id] ?? 0
                agents[i].costPerReq = snap.toolCostPerReq[id] ?? 0
                agents[i].topModel = snap.toolTopModel[id] ?? ""
                // Routed via relay only if this CLI tool actually saw traffic.
                agents[i].routedViaRelay = monthSpend > 0
            } else {
                agents[i].costToday = "$0.00 · 0 tokens"
                agents[i].costMonth = "$0.00 · 0 tokens"
                agents[i].tokensToday = 0
                agents[i].tokensMonth = 0
                agents[i].meter = 0
                agents[i].dailySpend = []
                agents[i].cacheHitRate = 0
                agents[i].modelMix = []
                agents[i].spendMonth = 0
                agents[i].spendToday = 0
                agents[i].successRate = 0
                agents[i].requests = 0
                agents[i].costPerReq = 0
                agents[i].topModel = ""
                // Desktop apps / non-CLI tools never route through the relay key.
                agents[i].routedViaRelay = false
            }
        }
    }

    /// Flips the macOS PAC auto-proxy for Wi-Fi on/off (best-effort), then
    /// re-reads the actual state to keep `routingOn` truthful.
    func setRouting(_ on: Bool) {
        routingOn = on // optimistic; reconciled below
        Task.detached { [weak self] in
            _ = AppModel.runNetworkSetup(["-setautoproxystate", "Wi-Fi", on ? "on" : "off"])
            let actual = AppModel.readRoutingEnabled()
            await MainActor.run { self?.routingOn = actual }
        }
    }

    /// Sets the selected agent's model in-memory and persists it to models.env.
    func selectModel(_ model: String) {
        guard let idx = agents.firstIndex(where: { $0.id == selectedTag }) else { return }
        agents[idx].selectedModel = model
        let agentId = agents[idx].id
        Task.detached { AppModel.persistModel(agentId: agentId, model: model) }
    }

    // MARK: - Navigation actions

    func openDashboard() {
        openURL("http://127.0.0.1:4142/")
    }

    func openStatus() {
        // Status page placeholder — points at the dashboard for now.
        openURL("http://127.0.0.1:4142/")
    }

    func quit() {
        NSApplication.shared.terminate(nil)
    }

    private func openURL(_ string: String) {
        guard let url = URL(string: string) else { return }
        NSWorkspace.shared.open(url)
    }

    // MARK: - Off-main helpers (nonisolated, safe to call from detached tasks)

    /// TCP-connects to `host:port` with a hard timeout using a non-blocking
    /// socket + `poll`. Returns true only if the connection actually completed.
    nonisolated private static func canConnect(host: String, port: UInt16, timeout: TimeInterval) -> Bool {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        if fd < 0 { return false }
        defer { close(fd) }

        // Switch to non-blocking so connect() returns immediately.
        let flags = fcntl(fd, F_GETFL, 0)
        if flags >= 0 {
            _ = fcntl(fd, F_SETFL, flags | O_NONBLOCK)
        }

        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = port.bigEndian
        if inet_pton(AF_INET, host, &addr.sin_addr) != 1 {
            return false
        }

        let connectResult = withUnsafePointer(to: &addr) { ptr -> Int32 in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                connect(fd, sa, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }

        if connectResult == 0 {
            return true // connected immediately
        }
        if errno != EINPROGRESS {
            return false
        }

        var pfd = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
        let millis = Int32(max(0, timeout * 1000))
        let ready = poll(&pfd, 1, millis)
        if ready <= 0 {
            return false // timed out or poll error
        }
        if pfd.revents & Int16(POLLOUT) == 0 {
            return false
        }

        // Connection attempt finished — check for a pending socket error.
        var soError: Int32 = 0
        var len = socklen_t(MemoryLayout<Int32>.size)
        if getsockopt(fd, SOL_SOCKET, SO_ERROR, &soError, &len) != 0 {
            return false
        }
        return soError == 0
    }

    /// Reads whether the Wi-Fi PAC auto-proxy is currently enabled.
    nonisolated private static func readRoutingEnabled() -> Bool {
        guard let out = runNetworkSetup(["-getautoproxyurl", "Wi-Fi"]) else { return false }
        return out.contains("Enabled: Yes")
    }

    /// Runs `/usr/sbin/networksetup` with the given args, returning stdout as a
    /// string. Best-effort — returns nil on any launch/read failure.
    nonisolated private static func runNetworkSetup(_ args: [String]) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/networksetup")
        process.arguments = args
        let stdout = Pipe()
        process.standardOutput = stdout
        process.standardError = Pipe()
        do {
            try process.run()
        } catch {
            return nil
        }
        let data = stdout.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(data: data, encoding: .utf8)
    }

    /// Parses the `gateway.url` host out of `~/.litellm-relay/config.yaml`.
    nonisolated private static func parseGatewayHost() -> String? {
        let path = ("~/.litellm-relay/config.yaml" as NSString).expandingTildeInPath
        guard let content = try? String(contentsOfFile: path, encoding: .utf8) else { return nil }

        var inGateway = false
        for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            if line.isEmpty { continue }

            // Top-level keys start at column 0. Track whether we're inside `gateway:`.
            let isTopLevel = !(line.first == " " || line.first == "\t")
            if isTopLevel {
                inGateway = line.trimmingCharacters(in: .whitespaces).hasPrefix("gateway:")
                continue
            }

            guard inGateway else { continue }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("url:") {
                var value = String(trimmed.dropFirst("url:".count)).trimmingCharacters(in: .whitespaces)
                value = value.trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
                if let host = URLComponents(string: value)?.host, !host.isEmpty {
                    return host
                }
                return value.isEmpty ? nil : value
            }
        }
        return nil
    }

    // MARK: - Spend / usage fetching (off-main)

    /// Immutable result of a spend fetch, carried back to the main actor.
    private struct SpendSnapshot {
        var keyHash: String
        var keyAlias: String
        var keySpend: Double
        var keyBudget: Double
        var budgetResetAt: String
        var accountTodaySpend: Double
        var accountMonthSpend: Double
        var accountTodayTokens: Int
        var accountMonthTokens: Int
        var topModel: String
        var dailySpend: [DailyPoint]
        var toolMonthSpend: [String: Double]
        var toolMonthTokens: [String: Int]
        var toolTodaySpend: [String: Double]
        var toolTodayTokens: [String: Int]
        var toolDailySpend: [String: [DailyPoint]]
        var toolCacheHitRate: [String: Double]
        var toolModelMix: [String: [ModelSlice]]
        var toolSuccessRate: [String: Double]
        var toolRequests: [String: Int]
        var toolCostPerReq: [String: Double]
        var toolTopModel: [String: String]
        var apiRequests: Int
        var successfulRequests: Int
        var failedRequests: Int
        var successRate: Double
        var cacheHitRate: Double
        var costPerRequest: Double
        var costPerMTok: Double
    }

    // Minimal Decodable views over the gateway JSON. Dynamic keys (model ids,
    // key hashes) are modeled as dictionaries; numeric fields are optional so a
    // missing value decodes to `nil` rather than throwing.
    private struct KeyInfoResponse: Decodable {
        let key: String
        let info: Info
        struct Info: Decodable {
            let key_alias: String?
            let spend: Double?
            let max_budget: Double?
            let budget_reset_at: String?
        }
    }
    private struct Metrics: Decodable {
        let spend: Double?
        let total_tokens: Int?
        let prompt_tokens: Int?
        let completion_tokens: Int?
        let cache_read_input_tokens: Int?
        let cache_creation_input_tokens: Int?
        let successful_requests: Int?
        let failed_requests: Int?
        let api_requests: Int?
    }
    private struct ModelEntry: Decodable {
        let metrics: Metrics
    }
    private struct Breakdown: Decodable {
        let models: [String: ModelEntry]?
    }
    private struct DayResult: Decodable {
        let date: String
        let metrics: Metrics
        let breakdown: Breakdown
    }
    private struct DailyActivityResponse: Decodable {
        let results: [DayResult]
    }

    private enum SpendError: Error, CustomStringConvertible {
        case missingConfig
        case http(Int)
        var description: String {
            switch self {
            case .missingConfig: return "gateway url or api_key missing from config.yaml"
            case .http(let code): return "gateway returned HTTP \(code)"
            }
        }
    }

    /// Fetches `/key/info` + `/user/daily/activity` and reduces them into a
    /// `SpendSnapshot`. Runs entirely off the main actor.
    nonisolated private static func computeSpend() async throws -> SpendSnapshot {
        guard
            let base = parseGatewayField("url").map(normalizedBase),
            let apiKey = parseGatewayField("api_key"),
            !base.isEmpty, !apiKey.isEmpty
        else {
            throw SpendError.missingConfig
        }

        // 1. Identify our own key.
        let info: KeyInfoResponse = try await fetchJSON(
            urlString: base + "/key/info", apiKey: apiKey
        )
        let keyHash = info.key
        let keyAlias = info.info.key_alias ?? ""
        let keySpend = info.info.spend ?? 0
        let keyBudget = info.info.max_budget ?? 0
        let budgetResetAt = info.info.budget_reset_at ?? ""

        // 2. UTC-safe window. The gateway buckets by UTC date, so recent traffic
        //    can land on a date that is still "tomorrow" in local time. To avoid
        //    dropping it, run end_date = today + 1 day and start_date = today - 30
        //    days. `today` (local) still identifies the current-day bucket.
        let fmt = DateFormatter()
        fmt.dateFormat = "yyyy-MM-dd"
        fmt.locale = Locale(identifier: "en_US_POSIX")
        let now = Date()
        let endDate = Calendar.current.date(byAdding: .day, value: 1, to: now) ?? now
        let end = fmt.string(from: endDate)
        let startDate = Calendar.current.date(byAdding: .day, value: -30, to: now) ?? now
        let start = fmt.string(from: startDate)

        // The gateway buckets spend under the UTC date, so a local "today" can
        // read $0 while data already exists under the UTC day. Compute the
        // current-day bucket in UTC to match the gateway's bucketing.
        let utcFmt = DateFormatter()
        utcFmt.dateFormat = "yyyy-MM-dd"
        utcFmt.locale = Locale(identifier: "en_US_POSIX")
        utcFmt.timeZone = TimeZone(identifier: "UTC")
        let utcToday = utcFmt.string(from: now)

        // Scope the report to our own key: with `api_key` set, the top-level
        // `results[].metrics` and `results[].breakdown.models` are already the
        // key's real totals (no nested api_key_breakdown traversal needed).
        let activity: DailyActivityResponse = try await fetchJSON(
            urlString: base + "/user/daily/activity?start_date=\(start)&end_date=\(end)&page_size=500&api_key=\(keyHash)",
            apiKey: apiKey
        )

        // "today" = the latest UTC bucket that actually has data (the gateway's
        // freshest day), falling back to the computed UTC today when empty.
        let today = activity.results.map { $0.date }.max() ?? utcToday

        // 3. Account-level rollups + 4. per-key, per-tool rollups.
        var accountMonthSpend = 0.0
        var accountMonthTokens = 0
        var accountTodaySpend = 0.0
        var accountTodayTokens = 0
        var modelSpendAcct: [String: Double] = [:]  // account-wide, for topModel
        var daily: [DailyPoint] = []

        var toolMonthSpend: [String: Double] = [:]
        var toolMonthTokens: [String: Int] = [:]
        var toolTodaySpend: [String: Double] = [:]
        var toolTodayTokens: [String: Int] = [:]

        // Per-tool insight accumulators (this key only, across returned days).
        var toolDailySpendMap: [String: [String: Double]] = [:]  // tool -> date -> spend
        var toolCacheReadTokens: [String: Int] = [:]              // tool -> sum cache_read_input_tokens
        var toolPromptTokens: [String: Int] = [:]                 // tool -> sum prompt_tokens
        var toolModelSpend: [String: [String: Double]] = [:]      // tool -> model -> spend
        var toolApiRequests: [String: Int] = [:]                  // tool -> sum api_requests
        var toolSuccessfulRequests: [String: Int] = [:]           // tool -> sum successful_requests

        // Account-wide insight-metric accumulators (summed across returned days).
        var sumApiRequests = 0
        var sumSuccessfulRequests = 0
        var sumFailedRequests = 0
        var sumCacheReadTokens = 0
        var sumPromptTokens = 0

        for day in activity.results {
            let daySpend = day.metrics.spend ?? 0
            let dayTokens = day.metrics.total_tokens ?? 0
            accountMonthSpend += daySpend
            accountMonthTokens += dayTokens
            sumApiRequests += day.metrics.api_requests ?? 0
            sumSuccessfulRequests += day.metrics.successful_requests ?? 0
            sumFailedRequests += day.metrics.failed_requests ?? 0
            sumCacheReadTokens += day.metrics.cache_read_input_tokens ?? 0
            sumPromptTokens += day.metrics.prompt_tokens ?? 0
            daily.append(DailyPoint(date: day.date, spend: daySpend))
            let isToday = day.date == today
            if isToday {
                accountTodaySpend = daySpend
                accountTodayTokens = dayTokens
            }

            for (modelId, model) in day.breakdown.models ?? [:] {
                // Top-level models breakdown is already key-scoped (the request
                // is filtered by `api_key`), so its metrics are this key's real
                // per-model totals — use them directly.
                let s = model.metrics.spend ?? 0
                let t = model.metrics.total_tokens ?? 0

                // Account-wide (i.e. this key's) per-model spend, for topModel.
                modelSpendAcct[modelId, default: 0] += s

                let tool = toolForModel(modelId)
                toolMonthSpend[tool, default: 0] += s
                toolMonthTokens[tool, default: 0] += t
                toolDailySpendMap[tool, default: [:]][day.date, default: 0] += s
                toolCacheReadTokens[tool, default: 0] += model.metrics.cache_read_input_tokens ?? 0
                toolPromptTokens[tool, default: 0] += model.metrics.prompt_tokens ?? 0
                toolModelSpend[tool, default: [:]][modelId, default: 0] += s
                toolApiRequests[tool, default: 0] += model.metrics.api_requests ?? 0
                toolSuccessfulRequests[tool, default: 0] += model.metrics.successful_requests ?? 0
                if isToday {
                    toolTodaySpend[tool, default: 0] += s
                    toolTodayTokens[tool, default: 0] += t
                }
            }
        }

        let topModel = shortModel(modelSpendAcct.max(by: { $0.value < $1.value })?.key ?? "")
        daily.sort { $0.date < $1.date }

        // Derived rates — guard divide-by-zero (leave 0).
        let successRate = sumApiRequests > 0
            ? Double(sumSuccessfulRequests) / Double(sumApiRequests) : 0
        let cacheHitRate = sumPromptTokens > 0
            ? Double(sumCacheReadTokens) / Double(sumPromptTokens) : 0
        let costPerRequest = sumSuccessfulRequests > 0
            ? accountMonthSpend / Double(sumSuccessfulRequests) : 0
        let costPerMTok = accountMonthTokens > 0
            ? accountMonthSpend / (Double(accountMonthTokens) / 1e6) : 0

        // Per-tool derived: daily spend (ascending), cache-hit rate, top-3 model mix.
        var toolDailySpend: [String: [DailyPoint]] = [:]
        for (tool, byDate) in toolDailySpendMap {
            toolDailySpend[tool] = byDate
                .map { DailyPoint(date: $0.key, spend: $0.value) }
                .sorted { $0.date < $1.date }
        }
        var toolCacheHitRate: [String: Double] = [:]
        for (tool, prompt) in toolPromptTokens where prompt > 0 {
            toolCacheHitRate[tool] = Double(toolCacheReadTokens[tool] ?? 0) / Double(prompt)
        }
        var toolModelMix: [String: [ModelSlice]] = [:]
        var toolTopModel: [String: String] = [:]
        for (tool, byModel) in toolModelSpend {
            let ranked = byModel.sorted { $0.value > $1.value }
            toolModelMix[tool] = ranked
                .prefix(3)
                .map { ModelSlice(model: shortModel($0.key), spend: $0.value) }
            toolTopModel[tool] = shortModel(ranked.first?.key ?? "")
        }

        // Per-tool request rates (this key only).
        var toolSuccessRate: [String: Double] = [:]
        var toolCostPerReq: [String: Double] = [:]
        for (tool, api) in toolApiRequests where api > 0 {
            toolSuccessRate[tool] = Double(toolSuccessfulRequests[tool] ?? 0) / Double(api)
        }
        for (tool, succ) in toolSuccessfulRequests where succ > 0 {
            toolCostPerReq[tool] = (toolMonthSpend[tool] ?? 0) / Double(succ)
        }

        return SpendSnapshot(
            keyHash: keyHash,
            keyAlias: keyAlias,
            keySpend: keySpend,
            keyBudget: keyBudget,
            budgetResetAt: budgetResetAt,
            accountTodaySpend: accountTodaySpend,
            accountMonthSpend: accountMonthSpend,
            accountTodayTokens: accountTodayTokens,
            accountMonthTokens: accountMonthTokens,
            topModel: topModel,
            dailySpend: daily,
            toolMonthSpend: toolMonthSpend,
            toolMonthTokens: toolMonthTokens,
            toolTodaySpend: toolTodaySpend,
            toolTodayTokens: toolTodayTokens,
            toolDailySpend: toolDailySpend,
            toolCacheHitRate: toolCacheHitRate,
            toolModelMix: toolModelMix,
            toolSuccessRate: toolSuccessRate,
            toolRequests: toolApiRequests,
            toolCostPerReq: toolCostPerReq,
            toolTopModel: toolTopModel,
            apiRequests: sumApiRequests,
            successfulRequests: sumSuccessfulRequests,
            failedRequests: sumFailedRequests,
            successRate: successRate,
            cacheHitRate: cacheHitRate,
            costPerRequest: costPerRequest,
            costPerMTok: costPerMTok
        )
    }

    /// Maps a gateway model id to the CLI tool that routes through the relay.
    /// Only CLI tools see relay traffic today, so every model resolves to one of
    /// them. Order matters: codex wins first, then Anthropic/Claude, and any
    /// remaining OpenAI (non-codex) usage is CLI/Responses traffic → codex-cli.
    nonisolated private static func toolForModel(_ model: String) -> String {
        let m = model.lowercased()
        if m.contains("codex") { return "codex-cli" }
        if m.hasPrefix("anthropic/") || m.contains("claude")
            || (m.hasPrefix("bedrock/") && m.contains("anthropic")) {
            return "claude-code"
        }
        return "codex-cli"
    }

    /// Strips provider/region prefixes off a gateway model id down to the bare
    /// model name, e.g. `bedrock/global.anthropic.claude-opus-4-7` →
    /// `claude-opus-4-7`, `openai/gpt-5.3-codex` → `gpt-5.3-codex`.
    nonisolated static func shortModel(_ s: String) -> String {
        // 1. Everything after the last `/` (drops `bedrock/`, `anthropic/`, ...).
        var name = s
        if let slash = name.lastIndex(of: "/") {
            name = String(name[name.index(after: slash)...])
        }
        // 2. Drop everything up to and including a `anthropic.`/`openai.` marker.
        for marker in ["anthropic.", "openai."] {
            if let range = name.range(of: marker) {
                name = String(name[range.upperBound...])
            }
        }
        return name
    }

    /// GETs `urlString` with a Bearer token and decodes the JSON body.
    /// Uses a 20s per-request timeout.
    nonisolated private static func fetchJSON<T: Decodable>(
        urlString: String, apiKey: String
    ) async throws -> T {
        guard let url = URL(string: urlString) else { throw SpendError.missingConfig }
        var request = URLRequest(url: url)
        request.timeoutInterval = 20
        request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await URLSession.shared.data(for: request)
        if let http = response as? HTTPURLResponse, !(200...299).contains(http.statusCode) {
            throw SpendError.http(http.statusCode)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }

    /// Reads an arbitrary scalar field from the `gateway:` block of
    /// `~/.litellm-relay/config.yaml` (e.g. "url", "api_key").
    nonisolated private static func parseGatewayField(_ field: String) -> String? {
        let path = ("~/.litellm-relay/config.yaml" as NSString).expandingTildeInPath
        guard let content = try? String(contentsOfFile: path, encoding: .utf8) else { return nil }

        var inGateway = false
        for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
            if line.isEmpty { continue }
            let isTopLevel = !(line.first == " " || line.first == "\t")
            if isTopLevel {
                inGateway = line.trimmingCharacters(in: .whitespaces).hasPrefix("gateway:")
                continue
            }
            guard inGateway else { continue }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("\(field):") {
                var value = String(trimmed.dropFirst("\(field):".count))
                    .trimmingCharacters(in: .whitespaces)
                value = value.trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
                return value.isEmpty ? nil : value
            }
        }
        return nil
    }

    /// Trims a trailing slash so path concatenation is clean.
    nonisolated private static func normalizedBase(_ url: String) -> String {
        url.hasSuffix("/") ? String(url.dropLast()) : url
    }

    // MARK: - Formatting

    /// Formats a token count compactly: raw under 1000, integer "K", and one
    /// decimal "M"/"B" (e.g. "107K", "1.2M", "437M", "5.9B").
    nonisolated static func fmtTokens(_ n: Int) -> String {
        let d = Double(n)
        if d >= 1_000_000_000 { return String(format: "%.1fB", d / 1_000_000_000) }
        if d >= 1_000_000 { return String(format: "%.1fM", d / 1_000_000) }
        if n >= 1_000 { return "\(n / 1_000)K" }
        return "\(n)"
    }

    /// Builds a "$X.XX · <tokens> tokens" cost string.
    nonisolated private static func fmtCost(_ spend: Double, _ tokens: Int) -> String {
        String(format: "$%.2f · %@ tokens", spend, fmtTokens(tokens))
    }

    /// Persists the selected model to `~/.litellm-relay/models.env`, preserving
    /// all other keys already in the file. Best-effort — never throws.
    nonisolated private static func persistModel(agentId: String, model: String) {
        let key: String
        switch agentId {
        case "claude-code": key = "ANTHROPIC_MODEL"
        case "codex-cli": key = "CODEX_MODEL"
        default: return // no env mapping for this agent
        }

        let dir = ("~/.litellm-relay" as NSString).expandingTildeInPath
        let path = (dir as NSString).appendingPathComponent("models.env")

        var existing: [String] = []
        if let content = try? String(contentsOfFile: path, encoding: .utf8) {
            existing = content
                .split(separator: "\n", omittingEmptySubsequences: false)
                .map(String.init)
        }

        var output: [String] = []
        var replaced = false
        for line in existing {
            if line.isEmpty { continue } // rebuild without blank lines
            if line.hasPrefix("\(key)=") {
                output.append("\(key)=\(model)")
                replaced = true
            } else {
                output.append(line)
            }
        }
        if !replaced {
            output.append("\(key)=\(model)")
        }

        let result = output.joined(separator: "\n") + "\n"
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try? result.write(toFile: path, atomically: true, encoding: .utf8)
    }
}

/// Builds a `Color` from a 24-bit RGB hex value, e.g. `0x53BFA7`.
private func hexColor(_ hex: UInt) -> Color {
    Color(
        red: Double((hex >> 16) & 0xFF) / 255.0,
        green: Double((hex >> 8) & 0xFF) / 255.0,
        blue: Double(hex & 0xFF) / 255.0
    )
}
