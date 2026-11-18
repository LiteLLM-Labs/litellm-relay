import Foundation

struct GatewayIdentity: Decodable, Sendable {
    let subject: String
    let label: String
    let method: String
    let expiresAt: Double
}

struct BridgeConfiguration: Decodable, Sendable {
    let identity: GatewayIdentity?
    let gateway: String
    let paired: Bool
    let localConnection: Bool
    let canPair: Bool
}

struct CodingSession: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let title: String
    let project: String
    let cwd: String
    let provider: String
    let status: String
    let activeFlags: [String]
    let live: Bool
    let updatedAt: Double

    var activity: String {
        if activeFlags.contains("waitingOnApproval") { return "Needs approval" }
        if activeFlags.contains("waitingOnUserInput") { return "Needs input" }
        if status == "active" { return "Running" }
        return live ? "Ready" : "Saved"
    }

    var updatedDate: Date { Date(timeIntervalSince1970: updatedAt) }
}

struct SessionPage: Decodable, Sendable {
    let sessions: [CodingSession]
    let nextCursor: String?
}

struct SessionMessage: Decodable, Identifiable, Sendable {
    let id: String
    let role: String
    let text: String
}

struct SessionDetail: Decodable, Sendable {
    let session: CodingSession
    let messages: [SessionMessage]
    let olderMessages: Bool
}

struct LoginStart: Decodable, Sendable {
    let code: String
    let verificationUrl: URL
}

struct LoginTeam: Decodable, Identifiable, Sendable {
    let id: String
    let name: String
}

struct LoginPoll: Decodable, Sendable {
    let status: String
    let teams: [LoginTeam]?
}

struct DeviceInfo: Decodable, Sendable {
    let name: String
    let gateway: String
    let paired: Bool
}

struct EmptyReply: Decodable, Sendable {}

struct RemoteFailure: Error, LocalizedError, Sendable {
    let message: String
    let status: Int?
    var errorDescription: String? { message }
}

struct ErrorReply: Decodable {
    let error: String
}

func bridgeAddress(_ text: String) -> URL? {
    guard let parts = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
          let host = parts.host, !host.isEmpty,
          let scheme = parts.scheme, ["http", "https"].contains(scheme),
          parts.user == nil, parts.password == nil,
          parts.query == nil, parts.fragment == nil,
          parts.path.isEmpty || parts.path == "/"
    else { return nil }
    return parts.url
}

func queryPath(_ path: String, name: String, value: String) -> String {
    var components = URLComponents()
    components.path = path
    components.queryItems = [URLQueryItem(name: name, value: value)]
    return components.string ?? path
}
