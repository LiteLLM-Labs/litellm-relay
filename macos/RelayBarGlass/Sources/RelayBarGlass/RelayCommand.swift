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
        case .switchTeam(let team): return ["switch-team", "--", team]
        case .switchEnvironment(let name): return ["switch-environment", "--", name]
        }
    }
}

struct CommandResult: Equatable {
    let exitCode: Int32
    let stdout: String
    let stderr: String
}

enum CommandOutcome: Equatable {
    case exited(CommandResult)
    case timedOut(TimeInterval)
    case cancelled
}

struct CommandError: Equatable {
    let command: RelayCommand
    let message: String
}

enum RelayHome {
    static func locate(environment: [String: String]) -> URL {
        if let home = environment["HOME"], !home.isEmpty {
            return URL(fileURLWithPath: home, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser
    }
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
    static func run(binary: URL, arguments: [String], timeout: TimeInterval?) async -> CommandOutcome {
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        let completion = Completion()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                completion.wait(continuation)
                DispatchQueue.global(qos: .userInitiated).async {
                    completion.finish(.exited(runBlocking(process, binary: binary, completion: completion)))
                }
                guard let timeout else { return }
                DispatchQueue.global().asyncAfter(deadline: .now() + timeout) {
                    if completion.finish(.timedOut(timeout)) { stop(process) }
                }
            }
        } onCancel: {
            if completion.finish(.cancelled) { stop(process) }
        }
    }

    private static func stop(_ process: Process) {
        if process.isRunning { process.terminate() }
    }

    private static func runBlocking(_ process: Process, binary: URL, completion: Completion) -> CommandResult {
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        guard !completion.isFinished else {
            return CommandResult(exitCode: -1, stdout: "", stderr: "")
        }
        do {
            try process.run()
        } catch {
            return CommandResult(
                exitCode: -1,
                stdout: "",
                stderr: "cannot run \(binary.path): \(error.localizedDescription)"
            )
        }
        if completion.isFinished { stop(process) }
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

private final class Completion: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<CommandOutcome, Never>?
    private var outcome: CommandOutcome?

    var isFinished: Bool {
        lock.lock()
        defer { lock.unlock() }
        return outcome != nil
    }

    func wait(_ continuation: CheckedContinuation<CommandOutcome, Never>) {
        lock.lock()
        defer { lock.unlock() }
        guard let outcome else {
            self.continuation = continuation
            return
        }
        continuation.resume(returning: outcome)
    }

    @discardableResult
    func finish(_ result: CommandOutcome) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard outcome == nil else { return false }
        outcome = result
        continuation?.resume(returning: result)
        continuation = nil
        return true
    }
}
