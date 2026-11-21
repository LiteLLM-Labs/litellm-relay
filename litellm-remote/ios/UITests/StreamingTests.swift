import Foundation
import Network
import XCTest

final class StreamingTests: XCTestCase {
    @MainActor
    func testLongCodexReplyKeepsStopAndFollowUpResponsive() throws {
        continueAfterFailure = false
        let service = try StreamingService(streamOnFirstSend: true)
        let app = try launch(service)
        defer { app.terminate() }
        let session = app.staticTexts[StreamingService.title]
        let composer = app.descendants(matching: .any)["messageComposer"].firstMatch
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "enabled == true"), object: composer)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 8), .completed)
        composer.tap()
        composer.typeText(StreamingService.firstPrompt)
        app.buttons["sendMessageButton"].tap()

        let back = app.navigationBars.buttons.element(boundBy: 0)
        let latest = app.buttons["Latest message"]
        let chip = app.buttons["sessionChip"]
        XCTAssertTrue(chip.isHittable)
        XCTAssertGreaterThanOrEqual(chip.frame.minX, back.frame.maxX)
        XCTAssertLessThanOrEqual(chip.frame.maxX, latest.frame.minX)
        let stop = app.buttons["stopTurnButton"]
        XCTAssertTrue(stop.waitForExistence(timeout: 10))
        XCTAssertTrue(latest.isHittable)
        latest.tap()

        let reply = app.staticTexts["streamingReply"]
        XCTAssertTrue(reply.waitForExistence(timeout: 10))
        // The fixture sends hundreds of lines per poll. The live bubble should
        // stay small and show fresh output rather than the beginning of the reply.
        let initial = reply.label
        XCTAssertLessThanOrEqual(initial.count, 600)
        XCTAssertTrue(initial.contains("Stream tail"))
        let changes = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label != %@", initial), object: reply)
        XCTAssertEqual(XCTWaiter.wait(for: [changes], timeout: 8), .completed)
        XCTAssertTrue(stop.isHittable)
        stop.tap()
        XCTAssertTrue(stop.waitForNonExistence(timeout: 8))
        XCTAssertTrue(reply.waitForNonExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Stopped reply"].waitForExistence(timeout: 5))
        XCTAssertEqual(service.interruptCount, 1)

        XCTAssertTrue(composer.isEnabled)
        composer.tap()
        composer.typeText("Follow up after stop")
        app.buttons["sendMessageButton"].tap()
        XCTAssertTrue(app.staticTexts["Follow-up acknowledged"].waitForExistence(timeout: 8))
        XCTAssertEqual(service.sentText, "Follow up after stop")
        back.tap()
        XCTAssertTrue(session.waitForExistence(timeout: 5))
    }

    @MainActor
    func testDisconnectedMacPreservesDraftAndRestoresComposer() throws {
        continueAfterFailure = false
        let service = try StreamingService()
        let app = try launch(service)
        defer { app.terminate() }
        XCTAssertTrue(app.staticTexts["Ready"].waitForExistence(timeout: 8))

        let composer = app.descendants(matching: .any)["messageComposer"].firstMatch
        let send = app.buttons["sendMessageButton"]
        composer.tap()
        composer.typeText("Draft before disconnect")
        XCTAssertTrue(send.isEnabled)

        service.setOnline(false)
        XCTAssertTrue(app.staticTexts["Unavailable"].waitForExistence(timeout: 8))
        XCTAssertTrue(app.staticTexts[StreamingService.offlineMessage].exists)
        XCTAssertFalse(send.isEnabled)
        XCTAssertNil(service.sentText)

        service.setOnline(true)
        XCTAssertTrue(app.staticTexts["Ready"].waitForExistence(timeout: 8))
        XCTAssertEqual(composer.value as? String, "Draft before disconnect")
        XCTAssertNil(service.sentText)
        send.tap()
        XCTAssertTrue(app.staticTexts["Follow-up acknowledged"].waitForExistence(timeout: 8))
        XCTAssertEqual(service.sentText, "Draft before disconnect")
    }

    @MainActor
    private func launch(_ service: StreamingService) throws -> XCUIApplication {
        let port = try XCTUnwrap(service.port)
        let app = XCUIApplication()
        app.launchArguments = [
            "-gatewayAddress", StreamingService.gateway,
            "-workspaceAddress", "",
            "-serviceOverride", "http://127.0.0.1:\(port)"
        ]
        app.launch()
        let connect = app.buttons["connectButton"]
        XCTAssertTrue(connect.waitForExistence(timeout: 10))
        connect.tap()
        let signIn = app.buttons["Sign in with LiteLLM"]
        XCTAssertTrue(signIn.waitForExistence(timeout: 10))
        signIn.tap()
        let session = app.staticTexts[StreamingService.title]
        XCTAssertTrue(session.waitForExistence(timeout: 10))
        session.tap()
        return app
    }
}

// The simulator and runner share loopback. Only the remote HTTP service is
// substituted; the app uses its normal workspace, transport and session views.
private final class StreamingService: @unchecked Sendable {
    static let gateway = "https://gateway.example.test"
    static let title = "Generate a complete set of test scenarios for native Codex control from an iPhone on a different network"
    static let firstPrompt = "Generate another long reply"
    static let offlineMessage = "This device is offline. Keep Relay Remote connected on your Mac."

    private let queue = DispatchQueue(label: "RemoteUITests.StreamingService")
    private let listener: NWListener
    private var poll = 0
    private var interrupts = 0
    private var followUp: String?
    private var online = true
    private let streamOnFirstSend: Bool
    private var firstSent = false

    var port: UInt16? { listener.port?.rawValue }
    var interruptCount: Int { queue.sync { interrupts } }
    var sentText: String? { queue.sync { followUp } }

    func setOnline(_ value: Bool) { queue.sync { online = value } }

    init(streamOnFirstSend: Bool = false) throws {
        self.streamOnFirstSend = streamOnFirstSend
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
        let working = firstSent && interrupts == 0
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
            response = ["devices": [["id": "test-mac", "name": "Test Mac", "online": online, "lastSeen": now, "platform": "macOS"]]]
        case base + "/sessions":
            response = ["sessions": [session]]
        case base + "/models":
            response = ["models": []]
        case detail:
            guard online else {
                send(["error": Self.offlineMessage], status: "503 Service Unavailable", on: connection)
                return
            }
            poll += 1
            let stream = String(repeating: "**Streaming** `code` [link](https://example.test)\n", count: 500)
            var messages = [
                ["id": "user", "role": "user", "text": "Generate a long reply"],
                ["id": "reply", "role": "assistant", "text": streamOnFirstSend ? stream : "Previous reply"]
            ]
            if firstSent {
                messages += [
                    ["id": "next-user", "role": "user", "text": Self.firstPrompt],
                    ["id": "next-reply", "role": "assistant", "text": working ? stream + "Stream tail \(poll)" : "Stopped reply"]
                ]
            }
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
            if streamOnFirstSend && !firstSent {
                firstSent = body["text"] == Self.firstPrompt
            } else {
                followUp = body["text"]
            }
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
