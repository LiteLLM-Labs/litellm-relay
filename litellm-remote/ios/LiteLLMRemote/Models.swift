import Foundation
import UniformTypeIdentifiers

struct GatewayIdentity: Codable, Sendable {
    let subject: String
    let label: String
    let method: String
    let expiresAt: Double
}

struct ServiceConfiguration: Decodable, Sendable {
    let identity: GatewayIdentity?
    let gateway: String
}

struct RegisteredDevice: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let online: Bool
    let lastSeen: Double
    let platform: String
}

struct DevicePage: Decodable, Sendable { let devices: [RegisteredDevice] }
struct WorkspaceDiscovery: Decodable, Sendable { let gatewayUrl: String; let controlUrl: String }

struct CodingSession: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let title: String
    let project: String
    let cwd: String
    let agent: String
    let model: String?
    let status: String
    let canSend: Bool
    let canStop: Bool
    let updatedAt: Double

    var agentName: String { agent == "claude" ? "Claude Code" : "Codex" }
    var needsAttention: Bool { ["needs_approval", "needs_input", "handoff_required"].contains(status) }
    var activity: String {
        switch status {
        case "working": return "Working"
        case "needs_approval": return "Needs approval"
        case "needs_input": return "Needs input"
        case "handoff_required": return "Set up on Mac"
        case "ready": return "Ready"
        case "error": return "Needs attention"
        default: return "Saved"
        }
    }
    var updatedDate: Date { Date(timeIntervalSince1970: updatedAt) }
}

struct SessionRoute: Hashable {
    let session: CodingSession
    let deviceId: String
}

struct SessionPage: Decodable, Sendable {
    let sessions: [CodingSession]
    let nextCursor: String?
    let warnings: [String]?
}

struct SessionMessage: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let role: String
    let text: String

    var streamingPreview: String {
        text.suffix(600).split(omittingEmptySubsequences: false, whereSeparator: \.isNewline)
            .suffix(4).joined(separator: "\n")
    }
}

struct ModelOption: Decodable, Identifiable, Sendable {
    let id: String
    let label: String
    let isDefault: Bool
}

struct ModelPage: Decodable, Sendable {
    let models: [ModelOption]
    let nextCursor: String?
}

struct SessionDetail: Decodable, Equatable, Sendable {
    let session: CodingSession
    let messages: [SessionMessage]
    let olderMessages: Bool
    let error: String?
    let approvals: [SessionApproval]

    var streamingReplyID: String? {
        guard session.canStop else { return nil }
        return messages.last { $0.role == "user" || $0.role == "assistant" }
            .flatMap { $0.role == "assistant" ? $0.id : nil }
    }
}

struct SessionApproval: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let text: String
    let canAccept: Bool
}

struct LoginStart: Decodable, Sendable {
    let flowToken: String
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
    let token: String?
}

struct EmptyReply: Decodable, Sendable {}

enum MessageMarkdown {
    static func parse(_ text: String) -> AttributedString? {
        guard var parsed = try? AttributedString(
            markdown: text,
            options: AttributedString.MarkdownParsingOptions(
                allowsExtendedAttributes: false,
                interpretedSyntax: .inlineOnlyPreservingWhitespace,
                failurePolicy: .returnPartiallyParsedIfPossible
            )
        ) else { return nil }
        let unsafe = parsed.runs.compactMap { run -> Range<AttributedString.Index>? in
            guard let link = run.link, link.scheme?.lowercased() != "https" else { return nil }
            return run.range
        }
        for range in unsafe {
            parsed[range].link = nil
        }
        return parsed
    }
}

// Parses run one at a time, off the main actor, so a long stream cannot stack
// user-initiated work. Different strings all complete; only a cancelled request
// is dropped, and identical queued strings share one parse.
actor MarkdownQueue {
    static let shared = MarkdownQueue()

    private struct Job {
        let id: UUID
        let text: String
        let continuation: CheckedContinuation<AttributedString?, Never>

        consuming func finish(_ value: AttributedString?) {
            continuation.resume(returning: value)
        }
    }

    private var jobs: [Job] = []
    private var pumping = false

    func parse(_ text: String) async -> AttributedString? {
        let id = UUID()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                jobs.append(Job(id: id, text: text, continuation: continuation))
                guard !pumping else { return }
                pumping = true
                Task { await self.pump() }
            }
        } onCancel: {
            Task { await self.drop(id) }
        }
    }

    private func drop(_ id: UUID) {
        guard let index = jobs.firstIndex(where: { $0.id == id }) else { return }
        jobs.remove(at: index).finish(nil)
    }

    private func pump() async {
        while true {
            guard let text = jobs.first?.text else {
                pumping = false
                if jobs.isEmpty { return }
                pumping = true
                continue
            }
            var cohort: [Job] = []
            var rest: [Job] = []
            while !jobs.isEmpty {
                let job = jobs.removeFirst()
                if job.text == text { cohort.append(job) } else { rest.append(job) }
            }
            jobs = rest
            let parsed = await Task.detached(priority: .utility) {
                MessageMarkdown.parse(text)
            }.value
            while let job = cohort.popLast() {
                job.finish(parsed)
            }
        }
    }
}

/// A text file staged in the composer. The agents' control protocols carry
/// plain text, so attachments travel inline as fenced blocks the way pasted
/// content does; binary files are refused rather than silently mangled.
struct DraftAttachment: Identifiable, Equatable, Sendable {
    let id = UUID()
    let name: String
    let text: String

    static let readableTypes: [UTType] = [
        .plainText, .utf8PlainText, .text, .sourceCode, .json, .xml, .yaml,
        .propertyList, .commaSeparatedText, .log,
    ]

    static func read(url: URL) -> Result<DraftAttachment, RemoteFailure> {
        let secured = url.startAccessingSecurityScopedResource()
        defer { if secured { url.stopAccessingSecurityScopedResource() } }
        let refused = RemoteFailure(
            message: "\(url.lastPathComponent) couldn’t be attached. Text files up to 24,000 characters are supported.",
            status: nil
        )
        guard let data = try? Data(contentsOf: url), data.count <= 256_000,
              let text = String(data: data, encoding: .utf8), text.count <= 24_000
        else { return .failure(refused) }
        return .success(DraftAttachment(name: url.lastPathComponent, text: text))
    }

    static func compose(draft: String, attachments: [DraftAttachment]) -> String {
        let body = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !attachments.isEmpty else { return body }
        let blocks = attachments.map { attachment in
            "<attached-file name=\"\(fenceName(attachment.name))\">\n\(attachment.text)\n</attached-file>"
        }
        return ((body.isEmpty ? [] : [body]) + blocks).joined(separator: "\n\n")
    }

    private static func fenceName(_ name: String) -> String {
        let cleaned = String(name.map { character in
            switch character {
            case "\"", "\n", "\r", "<", ">": "_"
            default: character
            }
        }).trimmingCharacters(in: .whitespacesAndNewlines)
        return cleaned.isEmpty ? "file" : String(cleaned.prefix(180))
    }
}

struct RenderedMarkdown: Equatable, Sendable {
    let source: String
    let value: AttributedString

    // A cached parse is only valid for the text it came from; anything newer
    // renders as plain text until its own parse lands.
    func display(for text: String) -> AttributedString {
        source == text ? value : AttributedString(text)
    }
}

struct RemoteFailure: Error, LocalizedError, Sendable {
    let message: String
    let status: Int?
    var errorDescription: String? { message }
}

struct ErrorReply: Decodable { let error: String }

func serviceAddress(_ text: String) -> URL? {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    let normalized = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
    guard let parts = URLComponents(string: normalized), let host = parts.host, !host.isEmpty,
          parts.scheme == "https" || (parts.scheme == "http" && ["127.0.0.1", "localhost", "[::1]"].contains(host)),
          parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
          parts.path.isEmpty || parts.path == "/"
    else { return nil }
    return parts.url
}

func normalizedGateway(_ text: String) -> URL? {
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    let normalized = trimmed.contains("://") ? trimmed : "https://\(trimmed)"
    guard var parts = URLComponents(string: normalized), let host = parts.host, !host.isEmpty,
          parts.scheme == "https", parts.user == nil, parts.password == nil,
          parts.query == nil, parts.fragment == nil else { return nil }
    parts.path = parts.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    if parts.path == "v1" { parts.path = "" }
    else if parts.path.hasSuffix("/v1") { parts.path = String(parts.path.dropLast(3)) }
    if !parts.path.isEmpty { parts.path = "/" + parts.path }
    return parts.url
}

func queryPath(_ path: String, name: String, value: String) -> String {
    var components = URLComponents()
    components.path = path
    components.queryItems = [URLQueryItem(name: name, value: value)]
    return components.string ?? path
}
