import AppKit
import Darwin

struct RunningCopy: Equatable {
    let pid: pid_t
    let startedAt: Date
    var installed = false
}

enum LaunchDecision: Equatable {
    case run
    case yield(to: RunningCopy)
    case replace([RunningCopy])
}

enum SingleInstance {
    static let installedApp = URL(fileURLWithPath: "/usr/local/litellm-relay/RelayBarGlass.app")

    static func decision(bundleIdentifier: String?) -> LaunchDecision {
        guard let bundleIdentifier, let started = runningCopy(getpid()) else { return .run }
        let own = RunningCopy(pid: started.pid, startedAt: started.startedAt, installed: isInstalled(Bundle.main.bundleURL))
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: bundleIdentifier)
            .filter { !$0.isTerminated }
            .compactMap { app in
                runningCopy(app.processIdentifier).map {
                    RunningCopy(pid: $0.pid, startedAt: $0.startedAt, installed: isInstalled(app.bundleURL))
                }
            }
        return decision(of: own, among: others)
    }

    static func decision(of own: RunningCopy, among others: [RunningCopy]) -> LaunchDecision {
        let rivals = others.filter { $0.pid != own.pid }
        let older = rivals.filter { launchOrder($0, before: own) }
        guard own.installed else {
            let senior = rivals.filter(\.installed).min { launchOrder($0, before: $1) } ?? olderCopy(of: own, among: rivals)
            return senior.map { .yield(to: $0) } ?? .run
        }
        if let senior = older.filter(\.installed).min(by: { launchOrder($0, before: $1) }) {
            return .yield(to: senior)
        }
        let replaced = older.filter { !$0.installed }
        return replaced.isEmpty ? .run : .replace(replaced)
    }

    static func isInstalled(_ bundle: URL?) -> Bool {
        bundle?.standardizedFileURL.resolvingSymlinksInPath() == installedApp.resolvingSymlinksInPath()
    }

    static func olderCopy(of own: RunningCopy, among others: [RunningCopy]) -> RunningCopy? {
        others
            .filter { $0.pid != own.pid && launchOrder($0, before: own) }
            .min { launchOrder($0, before: $1) }
    }

    static func runningCopy(_ pid: pid_t) -> RunningCopy? {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var request: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid]
        guard sysctl(&request, u_int(request.count), &info, &size, nil, 0) == 0, size > 0 else { return nil }
        let started = info.kp_proc.p_un.__p_starttime
        let seconds = TimeInterval(started.tv_sec) + TimeInterval(started.tv_usec) / 1_000_000
        return RunningCopy(pid: pid, startedAt: Date(timeIntervalSince1970: seconds))
    }

    private static func launchOrder(_ left: RunningCopy, before right: RunningCopy) -> Bool {
        (left.startedAt, left.pid) < (right.startedAt, right.pid)
    }
}

enum TerminationCause: Equatable {
    case unrequested
    case userQuit
    case systemPowerOff
}

enum TerminationPolicy {
    static func exitStatus(for cause: TerminationCause) -> Int32 {
        switch cause {
        case .userQuit, .systemPowerOff: return EXIT_SUCCESS
        case .unrequested: return EXIT_FAILURE
        }
    }
}

final class QuitIntent: @unchecked Sendable {
    static let shared = QuitIntent()

    private let lock = NSLock()
    private var recorded = TerminationCause.unrequested

    var cause: TerminationCause {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func record(_ cause: TerminationCause) {
        lock.lock()
        defer { lock.unlock() }
        recorded = cause
    }
}

final class RelayBarDelegate: NSObject, NSApplicationDelegate {
    private let intent: QuitIntent
    private let exitProcess: (Int32) -> Void
    private let powerOffObserver: NSObjectProtocol

    override convenience init() {
        self.init(intent: .shared, workspaceCenter: NSWorkspace.shared.notificationCenter) { exit($0) }
    }

    init(intent: QuitIntent, workspaceCenter: NotificationCenter, exitProcess: @escaping (Int32) -> Void) {
        self.intent = intent
        self.exitProcess = exitProcess
        powerOffObserver = workspaceCenter.addObserver(forName: NSWorkspace.willPowerOffNotification, object: nil, queue: nil) { [intent] _ in
            intent.record(.systemPowerOff)
        }
        super.init()
    }

    func applicationWillTerminate(_ notification: Notification) {
        let cause = intent.cause
        let status = TerminationPolicy.exitStatus(for: cause)
        if cause == .unrequested {
            FileHandle.standardError.write(Data("relaybar: asked to quit without the Quit button or a logout; exiting \(status) so launchd starts it again\n".utf8))
        }
        exitProcess(status)
    }
}
