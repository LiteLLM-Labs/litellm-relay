import Foundation

enum StatusFormatter {
    static let defaultRelayPort = 4142

    static func relayPort(configYAML: String) -> Int {
        yamlScalar(configYAML, section: "relay", key: "port").flatMap(Int.init) ?? defaultRelayPort
    }

    static func yamlScalar(_ yaml: String, section: String, key: String) -> String? {
        var inSection = false
        for rawLine in yaml.split(separator: "\n") {
            let line = String(rawLine)
            let content = line.components(separatedBy: " #").first ?? line
            let trimmed = content.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty || trimmed.hasPrefix("#") { continue }
            let indented = line.first == " " || line.first == "\t"
            if !indented {
                inSection = trimmed == "\(section):"
                continue
            }
            guard inSection, trimmed.hasPrefix("\(key):") else { continue }
            let value = trimmed.dropFirst(key.count + 1)
                .trimmingCharacters(in: .whitespaces)
                .trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
            return value.isEmpty ? nil : value
        }
        return nil
    }

    static func timestamp(_ text: String?) -> Date? {
        guard let text else { return nil }
        let plain = ISO8601DateFormatter()
        if let date = plain.date(from: text) { return date }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: text)
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

    static func budgetLine(team: TeamBudget?, error: String?) -> String {
        if let error, !error.isEmpty { return error }
        guard let team, let spend = team.spend else { return "Budget not read yet" }
        let spent = money(spend)
        guard let cap = team.maxBudget else { return "\(spent), no budget" }
        let reset = timestamp(team.budgetResetAt).map { ", resets \(resetDay($0))" } ?? ""
        return "\(spent) of \(money(cap))\(reset)"
    }

    static func money(_ amount: Double) -> String {
        String(format: "$%.2f", amount)
    }

    static func resetDay(_ date: Date, timeZone: TimeZone = .gmt) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = timeZone
        formatter.dateFormat = "d MMM"
        return formatter.string(from: date)
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
