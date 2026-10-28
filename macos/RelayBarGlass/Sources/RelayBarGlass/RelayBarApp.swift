import SwiftUI

@main
struct RelayBarGlassApp: App {
    @NSApplicationDelegateAdaptor(RelayBarDelegate.self) private var delegate
    private let model = AppModel()

    init() {
        switch SingleInstance.decision(bundleIdentifier: Bundle.main.bundleIdentifier) {
        case .run:
            return
        case .yield(let older):
            FileHandle.standardError.write(Data("relaybar: another copy (pid \(older.pid)) is already running; quitting\n".utf8))
            exit(EXIT_SUCCESS)
        case .replace(let copies):
            for copy in copies {
                FileHandle.standardError.write(Data("relaybar: quitting the copy outside \(SingleInstance.installedApp.path) (pid \(copy.pid))\n".utf8))
                NSRunningApplication(processIdentifier: copy.pid)?.terminate()
            }
        }
    }

    var body: some Scene {
        MenuBarExtra {
            PopoverView(model: model)
        } label: {
            Text("✨🚅")
        }
        .menuBarExtraStyle(.window)
    }
}

/// The menu bar glyph: a rounded rectangle "gateway" with two signal bars,
/// matching the prototype's relay mark. Renders as a template (monochrome) icon.
struct RelayMark: View {
    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 3, style: .continuous)
                .stroke(lineWidth: 1.6)
                .frame(width: 16, height: 11)
            VStack(spacing: 2.6) {
                Capsule().frame(width: 8, height: 1.6)
                Capsule().frame(width: 8, height: 1.6)
            }
        }
        .frame(width: 18, height: 14)
    }
}
