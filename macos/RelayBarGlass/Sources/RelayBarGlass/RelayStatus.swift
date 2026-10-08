import Foundation

struct RelayStatus: Decodable, Equatable {
    let broker: BrokerStatus?
    let account: AccountStatus?
    let environments: EnvironmentsStatus?
    let mcp: McpStatus?

    static func decode(_ data: Data) throws -> RelayStatus {
        try JSONDecoder().decode(RelayStatus.self, from: data)
    }
}

struct BrokerStatus: Decodable, Equatable {
    let signedIn: Bool?
    let userId: String?
    let displayName: String?
    let team: String?
    let environment: String?
    let gatewayUrl: String?
    let keyExpiresAt: String?
    let source: String?

    enum CodingKeys: String, CodingKey {
        case signedIn = "signed_in"
        case userId = "user_id"
        case displayName = "display_name"
        case team
        case environment
        case gatewayUrl = "gateway_url"
        case keyExpiresAt = "key_expires_at"
        case source
    }
}

struct AccountStatus: Decodable, Equatable {
    let user: AccountUser?
    let teams: [TeamEntry]?
    let teamsError: String?
    let team: TeamBudget?
    let budgetError: String?
    let gateway: GatewayProbe?
    let polledAt: String?

    enum CodingKeys: String, CodingKey {
        case user
        case teams
        case teamsError = "teams_error"
        case team
        case budgetError = "budget_error"
        case gateway
        case polledAt = "polled_at"
    }
}

struct AccountUser: Decodable, Equatable {
    let id: String?
    let email: String?
}

struct TeamEntry: Decodable, Equatable, Identifiable {
    let id: String
    let alias: String?
}

struct TeamBudget: Decodable, Equatable {
    let id: String?
    let alias: String?
    let spend: Double?
    let maxBudget: Double?
    let budgetResetAt: String?

    enum CodingKeys: String, CodingKey {
        case id
        case alias
        case spend
        case maxBudget = "max_budget"
        case budgetResetAt = "budget_reset_at"
    }
}

struct GatewayProbe: Decodable, Equatable {
    let url: String?
    let reachable: Bool?
    let checkedAt: String?
    let error: String?

    enum CodingKeys: String, CodingKey {
        case url
        case reachable
        case checkedAt = "checked_at"
        case error
    }
}

struct EnvironmentsStatus: Decodable, Equatable {
    let current: String?
    let available: [EnvironmentEntry]?
}

struct EnvironmentEntry: Decodable, Equatable, Identifiable {
    let name: String
    let url: String?

    var id: String { name }
}

struct McpStatus: Decodable, Equatable {
    let catalogTools: Int?
    let servers: [String: McpServer]?

    enum CodingKeys: String, CodingKey {
        case catalogTools = "catalog_tools"
        case servers
    }
}

struct McpServer: Decodable, Equatable {
    let tools: Int?
    let active: Bool?
}
