import Foundation
import Network
import XCTest

final class StreamingTests: XCTestCase {
    @MainActor
    func testLongCodexReplyKeepsStopAndFollowUpResponsive() throws {
        continueAfterFailure = false
        let service = try StreamingService()
        let port = try XCTUnwrap(service.port)
        let app = XCUIApplication()
        app.launchArguments = [
            "-gatewayAddress", StreamingService.gateway,
            "-workspaceAddress", "",
            "-serviceOverride", "http://127.0.0.1:\(port)"
        ]
        app.launch()
        defer { app.terminate() }

        let connect = app.buttons["connectButton"]
        XCTAssertTrue(connect.waitForExistence(timeout: 10))
        connect.tap()
        let signIn = app.buttons["Sign in with LiteLLM"]
        XCTAssertTrue(signIn.waitForExistence(timeout: 10))
        signIn.tap()
        let session = app.staticTexts[StreamingService.title]
        XCTAssertTrue(session.waitForExistence(timeout: 10))
        session.tap()

        let reply = app.staticTexts["streamingReply"]
        XCTAssertTrue(reply.waitForExistence(timeout: 10))
        // The fixture sends hundreds of lines per poll. The live bubble should
        // stay small and show fresh output rather than the beginning of the reply.
        let initial = reply.label
        XCTAssertLessThanOrEqual(initial.count, 600)
        XCTAssertTrue(initial.contains("Stream tail"))
        let changes = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label != %@", initial), object: reply)
        XCTAssertEqual(XCTWaiter.wait(for: [changes], timeout: 8), .completed)
        let latest = app.buttons["Latest message"]
        XCTAssertTrue(latest.isHittable)
        latest.tap()

        let stop = app.buttons["Stop"]
        XCTAssertTrue(stop.isHittable)
        stop.tap()
        XCTAssertTrue(stop.waitForNonExistence(timeout: 8))
        XCTAssertTrue(reply.waitForNonExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Stopped reply"].waitForExistence(timeout: 5))
        XCTAssertEqual(service.interruptCount, 1)

        let composer = app.descendants(matching: .any)["messageComposer"].firstMatch
        XCTAssertTrue(composer.isEnabled)
        composer.tap()
        composer.typeText("Follow up after stop")
        app.buttons["sendMessageButton"].tap()
        XCTAssertTrue(app.staticTexts["Follow-up acknowledged"].waitForExistence(timeout: 8))
        XCTAssertEqual(service.sentText, "Follow up after stop")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(session.waitForExistence(timeout: 5))
    }
}

// The simulator and runner share loopback. Only the remote HTTP service is
// substituted; the app uses its normal workspace, transport and session views.
private final class StreamingService: @unchecked Sendable {
    static let gateway = "https://gateway.example.test"
    static let title = "Long Codex turn"

    private let queue = DispatchQueue(label: "RemoteUITests.StreamingService")
    private let listener: NWListener
    private var poll = 0
    private var interrupts = 0
    private var followUp: String?

    var port: UInt16? { listener.port?.rawValue }
    var interruptCount: Int { queue.sync { interrupts } }
    var sentText: String? { queue.sync { followUp } }

    init() throws {
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: params)
        let started = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready, .failed: started.signal()
            default: break
            }
        }
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { connection.cancel(); return }
            connection.start(queue: self.queue)
            self.read(connection, buffer: Data())
        }
        listener.start(queue: queue)
        _ = started.wait(timeout: .now() + 5)
    }

    deinit { listener.cancel() }

    private func read(_ connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 4096) { [weak self] bytes, _, complete, error in
            guard let self, error == nil else { connection.cancel(); return }
            let data = buffer + (bytes ?? Data())
            if let separator = data.range(of: Data("\r\n\r\n".utf8)) {
                let header = String(decoding: data[..<separator.lowerBound], as: UTF8.self)
                let lines = header.components(separatedBy: "\r\n")
                let length = lines.first { $0.lowercased().hasPrefix("content-length:") }
                    .flatMap { Int($0.dropFirst("content-length:".count).trimmingCharacters(in: .whitespaces)) } ?? 0
                let payload = data[separator.upperBound...]
                if payload.count >= length {
                    let target = lines.first?.split(separator: " ").dropFirst().first.map(String.init) ?? ""
                    let body = (try? JSONSerialization.jsonObject(with: Data(payload.prefix(length)))) as? [String: String]
                    self.respond(to: target, body: body ?? [:], on: connection)
                    return
                }
            }
            if complete || data.count > 32_768 { connection.cancel() }
            else { self.read(connection, buffer: data) }
        }
    }

    private func respond(to path: String, body: [String: String], on connection: NWConnection) {
        let base = "/api/devices/test-mac"
        let detail = base + "/sessions/codex:fixture"
        let now = Date().timeIntervalSince1970
        let working = interrupts == 0
        let session: [String: Any] = [
            "id": "codex:fixture", "title": Self.title, "project": "Fixture", "cwd": "/fixture",
            "agent": "codex", "status": working ? "working" : "ready",
            "canSend": !working, "canStop": working, "updatedAt": now
        ]
        let response: [String: Any]
        switch path {
        case "/api/me":
            // This loopback fixture deliberately skips the external SSO round trip.
            response = loggedIn
                ? ["gateway": Self.gateway, "identity": ["subject": "qa", "label": "QA", "method": "sso", "expiresAt": now + 600]]
                : ["gateway": Self.gateway]
        case "/api/auth/start":
            response = ["flowToken": "fixture", "code": "FIXTURE", "verificationUrl": Self.gateway + "/verify"]
        case "/api/auth/poll":
            loggedIn = true
            response = ["status": "ready", "token": "fixture"]
        case "/api/devices":
            response = ["devices": [["id": "test-mac", "name": "Test Mac", "online": true, "lastSeen": now, "platform": "macOS"]]]
        case base + "/sessions":
            response = ["sessions": [session]]
        case base + "/models":
            response = ["models": []]
        case detail:
            poll += 1
            let stream = String(repeating: "**Streaming** `code` [link](https://example.test)\n", count: 500)
            var messages = [
                ["id": "user", "role": "user", "text": "Generate a long reply"],
                ["id": "reply", "role": "assistant", "text": working ? stream + "Stream tail \(poll)" : "Stopped reply"]
            ]
            if let followUp {
                messages += [
                    ["id": "follow-up", "role": "user", "text": followUp],
                    ["id": "ack", "role": "assistant", "text": "**Follow-up acknowledged**"]
                ]
            }
            response = ["session": session, "messages": messages, "olderMessages": false, "approvals": []]
        case detail + "/interrupt":
            interrupts += 1
            response = [:]
        case detail + "/messages":
            followUp = body["text"]
            response = [:]
        default:
            send(["error": "Unknown fixture path"], status: "404 Not Found", on: connection)
            return
        }
        send(response, status: "200 OK", on: connection)
    }

    private var loggedIn = false

    private func send(_ response: [String: Any], status: String, on connection: NWConnection) {
        guard let data = try? JSONSerialization.data(withJSONObject: response) else { connection.cancel(); return }
        let headers = "HTTP/1.1 \(status)\r\nContent-Type: application/json\r\nContent-Length: \(data.count)\r\nConnection: close\r\n\r\n"
        connection.send(content: Data(headers.utf8) + data, completion: .contentProcessed { _ in connection.cancel() })
    }
}
