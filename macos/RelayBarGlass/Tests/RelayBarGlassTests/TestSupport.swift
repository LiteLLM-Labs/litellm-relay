import Darwin
import Foundation

struct SocketError: Error {}

func loopbackListener(listening: Bool) throws -> (descriptor: Int32, port: Int) {
    let fd = socket(AF_INET, SOCK_STREAM, 0)
    var address = sockaddr_in()
    address.sin_family = sa_family_t(AF_INET)
    address.sin_addr.s_addr = inet_addr("127.0.0.1")
    address.sin_port = 0
    var length = socklen_t(MemoryLayout<sockaddr_in>.size)
    let bound = withUnsafeMutablePointer(to: &address) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, length) }
    }
    let named = withUnsafeMutablePointer(to: &address) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &length) }
    }
    let listened = listening ? listen(fd, 16) : 0
    guard fd >= 0, bound == 0, named == 0, listened == 0 else {
        close(fd)
        throw SocketError()
    }
    return (fd, Int(UInt16(bigEndian: address.sin_port)))
}

struct ReservedPort {
    let descriptor: Int32
    let port: Int

    init() throws {
        (descriptor, port) = try loopbackListener(listening: false)
    }

    func release() {
        close(descriptor)
    }
}

final class StubDaemon: @unchecked Sendable {
    let port: Int
    private let descriptor: Int32
    private let body: Data
    private let delays: [TimeInterval]
    private let lock = NSLock()
    private var served = 0
    private var stopped = false

    init(body: String, delays: [TimeInterval] = []) throws {
        (descriptor, port) = try loopbackListener(listening: true)
        self.body = Data(body.utf8)
        self.delays = delays
        Thread.detachNewThread { [self] in acceptLoop() }
    }

    var requests: Int {
        lock.lock()
        defer { lock.unlock() }
        return served
    }

    func stop() {
        lock.lock()
        stopped = true
        lock.unlock()
    }

    private var isStopped: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopped
    }

    private func acceptLoop() {
        var waiting = pollfd(fd: descriptor, events: Int16(POLLIN), revents: 0)
        while !isStopped {
            guard Darwin.poll(&waiting, 1, 50) > 0 else { continue }
            let client = accept(descriptor, nil, nil)
            guard client >= 0 else { continue }
            var one: Int32 = 1
            setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
            DispatchQueue.global().async { self.answer(client) }
        }
        close(descriptor)
    }

    private func answer(_ client: Int32) {
        defer { close(client) }
        guard readHeaders(client) else { return }
        let delay = nextDelay()
        if delay > 0 { Thread.sleep(forTimeInterval: delay) }
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n"
        let response = Data(head.utf8) + body
        _ = response.withUnsafeBytes { send(client, $0.baseAddress, response.count, 0) }
    }

    private func readHeaders(_ client: Int32) -> Bool {
        var received = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while received.range(of: Data("\r\n\r\n".utf8)) == nil {
            let count = recv(client, &buffer, buffer.count, 0)
            guard count > 0 else { return false }
            received.append(contentsOf: buffer[0..<count])
        }
        return true
    }

    private func nextDelay() -> TimeInterval {
        lock.lock()
        defer { lock.unlock() }
        let index = served
        served += 1
        return index < delays.count ? delays[index] : 0
    }
}

struct TemporaryHome {
    let url: URL

    init() throws {
        url = FileManager.default.temporaryDirectory.appendingPathComponent("relaybar-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: relayDirectory, withIntermediateDirectories: true)
    }

    var relayDirectory: URL {
        url.appendingPathComponent(".litellm-relay")
    }

    func writeConfig(_ yaml: String) throws {
        try yaml.write(to: relayDirectory.appendingPathComponent("config.yaml"), atomically: true, encoding: .utf8)
    }

    func writeRelayPort(_ port: Int) throws {
        try writeConfig("relay:\n  port: \(port)\n")
    }

    func stubRelay(_ script: String) throws -> StubRelay {
        try StubRelay(directory: url, script: script)
    }

    func remove() {
        try? FileManager.default.removeItem(at: url)
    }
}

struct StubRelay {
    let binary: URL
    let log: URL

    init(directory: URL, script: String) throws {
        binary = directory.appendingPathComponent("litellm-relay")
        log = directory.appendingPathComponent("calls.log")
        let source = "#!/bin/sh\nprintf '%s %s\\n' \"$$\" \"$*\" >> '\(log.path)'\n\(script)\n"
        try source.write(to: binary, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: binary.path)
    }

    var environment: [String: String] {
        ["RELAY_BIN": binary.path]
    }

    var recordedCalls: [String] {
        entries.map(\.arguments)
    }

    func pids(of command: String) -> [pid_t] {
        entries.filter { $0.arguments.hasPrefix(command) }.map(\.pid)
    }

    private var entries: [(pid: pid_t, arguments: String)] {
        ((try? String(contentsOf: log, encoding: .utf8)) ?? "")
            .split(separator: "\n")
            .compactMap { line in
                let parts = line.split(separator: " ", maxSplits: 1)
                guard let pid = parts.first.flatMap({ pid_t($0) }) else { return nil }
                return (pid, parts.count > 1 ? String(parts[1]) : "")
            }
    }
}

func isAlive(_ pid: pid_t) -> Bool {
    kill(pid, 0) == 0
}

@MainActor
func eventually(within seconds: TimeInterval = 5, _ condition: () -> Bool) async -> Bool {
    let deadline = Date().addingTimeInterval(seconds)
    while !condition() {
        if Date() > deadline { return false }
        try? await Task.sleep(nanoseconds: 20_000_000)
    }
    return true
}
