import Foundation

enum RelayCommand: Equatable {
    case signIn
    case signOut
    case recheck
    case switchTeam(String)
    case switchEnvironment(String)

    enum Kind: Equatable {
        case signIn
        case signOut
        case recheck
        case switchTeam
        case switchEnvironment
    }

    var kind: Kind {
        switch self {
        case .signIn: return .signIn
        case .signOut: return .signOut
        case .recheck: return .recheck
        case .switchTeam: return .switchTeam
        case .switchEnvironment: return .switchEnvironment
        }
    }

    var arguments: [String] {
        switch self {
        case .signIn: return ["sign-in"]
        case .signOut: return ["sign-out"]
        case .recheck: return ["recheck"]
        case .switchTeam(let team): return ["switch-team", team]
        case .switchEnvironment(let name): return ["switch-environment", name]
        }
    }
}

struct CommandResult: Equatable {
    let exitCode: Int32
    let stdout: String
    let stderr: String
}

struct CommandError: Equatable {
    let command: RelayCommand
    let message: String
}

enum RelayBinary {
    static func locate(environment: [String: String], home: URL) -> URL {
        if let override = environment["RELAY_BIN"], !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
        return home.appendingPathComponent(".litellm-relay/bin/litellm-relay")
    }
}

enum ProcessRunner {
    static func run(binary: URL, arguments: [String]) async -> CommandResult {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: runBlocking(binary: binary, arguments: arguments))
            }
        }
    }

    private static func runBlocking(binary: URL, arguments: [String]) -> CommandResult {
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        do {
            try process.run()
        } catch {
            return CommandResult(
                exitCode: -1,
                stdout: "",
                stderr: "cannot run \(binary.path): \(error.localizedDescription)"
            )
        }
        let group = DispatchGroup()
        var outData = Data()
        var errData = Data()
        group.enter()
        DispatchQueue.global().async {
            outData = stdout.fileHandleForReading.readDataToEndOfFile()
            group.leave()
        }
        group.enter()
        DispatchQueue.global().async {
            errData = stderr.fileHandleForReading.readDataToEndOfFile()
            group.leave()
        }
        process.waitUntilExit()
        group.wait()
        return CommandResult(
            exitCode: process.terminationStatus,
            stdout: String(decoding: outData, as: UTF8.self),
            stderr: String(decoding: errData, as: UTF8.self)
        )
    }
}
