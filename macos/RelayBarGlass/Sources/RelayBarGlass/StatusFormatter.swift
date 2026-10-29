import Foundation

enum GatewayState: Equatable {
    case reachable
    case unreachable
    case checking
    case unknown
}

enum StatusFormatter {
    static let defaultRelayPort = 4142

    static func relayPort(configYAML: String) -> Int {
        yamlScalar(configYAML, section: "relay", key: "port").flatMap(Int.init) ?? defaultRelayPort
    }

    static func yamlScalar(_ yaml: String, section: String, key: String) -> String? {
        let text = yaml.hasPrefix("\u{FEFF}") ? String(yaml.dropFirst()) : yaml
        var inSection = false
        for rawLine in text.split(whereSeparator: \.isNewline) {
            let line = String(rawLine)
            let content = line.components(separatedBy: " #").first ?? line
            let trimmed = content.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty || trimmed.hasPrefix("#") { continue }
            let indented = line.first == " " || line.first == "\t"
            if !indented {
                let entry = mappingEntry(trimmed)
                inSection = entry?.key == section
                if inSection, let flow = entry?.value, flow.hasPrefix("{") {
                    return flowValue(flow, key: key)
                }
                continue
            }
            guard inSection, let entry = mappingEntry(trimmed), entry.key == key else { continue }
            return entry.value.isEmpty ? nil : entry.value
        }
        return nil
    }

    private static func mappingEntry(_ text: String) -> (key: String, value: String)? {
        guard let colon = text.firstIndex(of: ":") else { return nil }
        return (unquoted(text[..<colon]), unquoted(text[text.index(after: colon)...]))
    }

    private static func flowValue(_ flow: String, key: String) -> String? {
        let inner = flow.trimmingCharacters(in: CharacterSet(charactersIn: "{} "))
        let value = inner.split(separator: ",")
            .compactMap { mappingEntry(String($0)) }
            .first { $0.key == key }?
            .value
        return value?.isEmpty == false ? value : nil
    }

    private static func unquoted(_ text: Substring) -> String {
        text.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
    }

    static func timestamp(_ text: String?) -> Date? {
        guard let text else { return nil }
        let joined = joiningDateAndTime(text)
        let zoned = hasZone(joined) ? joined : joined + "Z"
        let plain = ISO8601DateFormatter()
        if let date = plain.date(from: zoned) { return date }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: zoned)
    }

    private static func joiningDateAndTime(_ text: String) -> String {
        let parts = text.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: false)
        guard parts.count == 2, parts[0].count == 10 else { return text }
        return "\(parts[0])T\(parts[1])"
    }

    private static func hasZone(_ text: String) -> Bool {
        guard let time = text.split(separator: "T", maxSplits: 1).dropFirst().first else { return true }
        return time.contains { "Zz+-".contains($0) }
    }

    static func countdown(_ seconds: TimeInterval) -> String {
        if seconds <= 0 { return "expired" }
        let whole = Int(seconds.rounded(.down))
        let hours = whole / 3600
        let minutes = (whole % 3600) / 60
        if hours > 0 { return String(format: "%dh %02dm", hours, minutes) }
        if minutes > 0 { return "\(minutes)m" }
        return "\(whole)s"
    }

    static func keyLine(source: String?, keyExpiresAt: Date?, now: Date) -> String {
        switch source {
        case "minted_key":
            guard let keyExpiresAt else { return "Key minted, renews itself" }
            let remaining = keyExpiresAt.timeIntervalSince(now)
            if remaining <= 0 { return "Key expired, renews on the next request" }
            return "Key valid \(countdown(remaining)), renews itself"
        case "static_key":
            return "Static key from config.yaml"
        default:
            return "Session credential"
        }
    }

    static func budgetLine(
        team: TeamBudget?,
        key: KeyBudget? = nil,
        error: String?,
        polled: Bool,
        timeZone: TimeZone = NSTimeZone.default
    ) -> String {
        let problem = error?.isEmpty == false ? error : nil
        if team == nil, let key, let spend = key.spend {
            return withProblem(budgetAmounts(spend: spend, cap: key.maxBudget, resetAt: key.budgetResetAt, timeZone: timeZone), problem)
        }
        guard let team else { return problem ?? (polled ? "No team picked" : "Budget not read yet") }
        guard let spend = team.spend else { return problem ?? "Budget not read yet" }
        return withProblem(budgetAmounts(spend: spend, cap: team.maxBudget, resetAt: team.budgetResetAt, timeZone: timeZone), problem)
    }

    private static func withProblem(_ amounts: String, _ problem: String?) -> String {
        guard let problem else { return amounts }
        return "\(amounts)\n\(problem)"
    }

    private static func budgetAmounts(spend: Double, cap: Double?, resetAt: String?, timeZone: TimeZone) -> String {
        let spent = money(spend)
        guard let cap else { return "\(spent), no budget" }
        let reset = timestamp(resetAt).map { ", resets \(resetDay($0, timeZone: timeZone))" } ?? ""
        return "\(spent) of \(money(cap))\(reset)"
    }

    static func money(_ amount: Double) -> String {
        String(format: "$%.2f", amount)
    }

    static func resetDay(_ date: Date, timeZone: TimeZone = NSTimeZone.default) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = timeZone
        formatter.dateFormat = "d MMM"
        return formatter.string(from: date)
    }

    static func gatewayState(_ probe: GatewayProbe?) -> GatewayState {
        guard let probe else { return .unknown }
        guard probe.checkedAt != nil, let reachable = probe.reachable else { return .checking }
        return reachable ? .reachable : .unreachable
    }

    static func displayName(broker: BrokerStatus?, account: AccountStatus?) -> String {
        let candidates = [broker?.displayName, account?.user?.email, broker?.userId]
        return candidates.compactMap { $0 }.first(where: { !$0.isEmpty }) ?? "Signed in"
    }

    static func teamLabel(_ team: TeamEntry) -> String {
        guard let alias = team.alias, !alias.isEmpty else { return team.id }
        return "\(alias) (\(team.id))"
    }

    static func gatewayHost(_ url: String?) -> String? {
        guard let url, !url.isEmpty else { return nil }
        return URLComponents(string: url)?.host ?? url
    }

    static func commandFailure(_ outcome: CommandOutcome) -> String? {
        switch outcome {
        case .exited(let result):
            return result.exitCode == 0 ? nil : failureMessage(stderr: result.stderr, exitCode: result.exitCode)
        case .timedOut(let seconds):
            return "The Relay daemon did not answer within \(String(format: "%g", seconds)) s"
        case .cancelled:
            return nil
        }
    }

    static func failureMessage(stderr: String, exitCode: Int32) -> String {
        let text = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
        if let data = text.data(using: .utf8),
           let refusal = try? JSONDecoder().decode(Refusal.self, from: data),
           let message = refusal.message, !message.isEmpty {
            return message
        }
        return text.isEmpty ? "exited with status \(exitCode)" : text
    }

    private struct Refusal: Decodable {
        let refused: String?
        let message: String?
    }
}
