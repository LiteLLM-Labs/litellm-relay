import SwiftUI
import UIKit

@main
struct LiteLLMRemoteApp: App {
    @State private var store = RemoteStore()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            VStack(spacing: 0) {
                if store.configuration?.identity != nil {
                    WorkspaceView(store: store)
                } else if store.restoring {
                    ProgressView("Restoring your session")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Palette.background)
                } else {
                    ConnectionView(store: store)
                }
            }
            .foregroundStyle(Palette.ink)
            .tint(Palette.accent)
            .task { await store.restore() }
            .onChange(of: scenePhase) {
                if scenePhase == .active { Task { await store.restore() } }
            }
        }
    }
}

enum Palette {
    static let background = adaptive(light: (0.967, 0.972, 0.984), dark: (0.051, 0.059, 0.082))
    static let surface = adaptive(light: (1, 1, 1), dark: (0.102, 0.114, 0.149))
    static let accent = adaptive(light: (1 / 255, 23 / 255, 190 / 255), dark: (0.35, 0.42, 1))
    static let ink = adaptive(light: (0.067, 0.094, 0.153), dark: (0.92, 0.93, 0.96))
    static let secondary = adaptive(light: (0.42, 0.45, 0.50), dark: (0.62, 0.65, 0.71))
    static let border = adaptive(light: (0.89, 0.90, 0.92), dark: (0.23, 0.25, 0.31))
    static let success = adaptive(light: (0.09, 0.50, 0.24), dark: (0.37, 0.80, 0.52))
    static let warning = adaptive(light: (0.65, 0.34, 0.06), dark: (0.95, 0.65, 0.33))

    private static func adaptive(
        light: (CGFloat, CGFloat, CGFloat), dark: (CGFloat, CGFloat, CGFloat)
    ) -> Color {
        Color(uiColor: UIColor { traits in
            let (red, green, blue) = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(displayP3Red: red, green: green, blue: blue, alpha: 1)
        })
    }
}

struct BrandMark: View {
    var size: CGFloat = 28
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        // The monogram is solid navy, so dark mode places it on a small light chip.
        Image("LiteLLMMark")
            .resizable()
            .scaledToFit()
            .frame(width: size, height: size)
            .padding(scheme == .dark ? size * 0.14 : 0)
            .background(
                scheme == .dark ? AnyShapeStyle(.white) : AnyShapeStyle(.clear),
                in: RoundedRectangle(cornerRadius: size * 0.3)
            )
            .accessibilityHidden(true)
    }
}
