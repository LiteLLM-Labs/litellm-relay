import Darwin
import Foundation
import Testing
@testable import RelayBarGlass

@Suite struct AppModelTests {
    @MainActor @Test func reportsTheDaemonAsNotRunningWhenNothingListensOnItsPort() async throws {
        let reserved = try ReservedPort()
        defer { reserved.release() }
        let home = FileManager.default.temporaryDirectory.appendingPathComponent("relaybar-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: home) }
        let relayDir = home.appendingPathComponent(".litellm-relay")
        try FileManager.default.createDirectory(at: relayDir, withIntermediateDirectories: true)
        try "relay:\n  port: \(reserved.port)\n".write(
            to: relayDir.appendingPathComponent("config.yaml"), atomically: true, encoding: .utf8
        )

        let model = AppModel(environment: [:], home: home)
        #expect(model.relayPort == reserved.port)
        await model.poll()
        #expect(model.daemonUp == false)
        #expect(model.status == nil)
        #expect(model.signedIn == false)
        #expect(model.dashboardURL?.absoluteString == "http://127.0.0.1:\(reserved.port)/")
    }
}

private struct PortError: Error {}

private struct ReservedPort {
    let descriptor: Int32
    let port: Int

    init() throws {
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
        guard fd >= 0, bound == 0, named == 0 else {
            close(fd)
            throw PortError()
        }
        descriptor = fd
        port = Int(UInt16(bigEndian: address.sin_port))
    }

    func release() {
        close(descriptor)
    }
}
