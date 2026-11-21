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
                        .background(Palette.canvas)
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

// The LiteLLM Remote design language. Every color, distance, radius, and font
// in the app resolves through these four enums.

enum Palette {
    static let canvas = adaptive(light: (0.969, 0.973, 0.984), dark: (0.043, 0.051, 0.071))
    static let surface = adaptive(light: (1, 1, 1), dark: (0.090, 0.102, 0.129))
    static let raised = adaptive(light: (0.933, 0.941, 0.961), dark: (0.137, 0.157, 0.208))
    static let border = adaptive(light: (0.890, 0.902, 0.925), dark: (0.169, 0.188, 0.235))
    static let ink = adaptive(light: (0.067, 0.094, 0.169), dark: (0.925, 0.933, 0.957))
    static let inkSoft = adaptive(light: (0.365, 0.396, 0.455), dark: (0.608, 0.631, 0.682))
    static let accent = adaptive(light: (0.004, 0.090, 0.745), dark: (0.353, 0.408, 1))
    static let onAccent = Color.white
    static let good = adaptive(light: (0.090, 0.502, 0.267), dark: (0.306, 0.796, 0.494))
    static let warn = adaptive(light: (0.651, 0.341, 0.059), dark: (0.949, 0.651, 0.353))

    private static func adaptive(
        light: (CGFloat, CGFloat, CGFloat), dark: (CGFloat, CGFloat, CGFloat)
    ) -> Color {
        Color(uiColor: UIColor { traits in
            let (red, green, blue) = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(displayP3Red: red, green: green, blue: blue, alpha: 1)
        })
    }
}

enum Space {
    static let s1: CGFloat = 4
    static let s2: CGFloat = 8
    static let s3: CGFloat = 12
    static let s4: CGFloat = 16
    static let s5: CGFloat = 20
    static let s6: CGFloat = 24
    static let s8: CGFloat = 32
    static let s12: CGFloat = 48
}

enum Radius {
    static let inset: CGFloat = 12
    static let container: CGFloat = 24
}

enum Fonts {
    static let title = Font.headline
    static let body = Font.body
    static let label = Font.subheadline.weight(.medium)
    static let caption = Font.footnote
    static let eyebrow = Font.caption2.weight(.bold)
    static let mono = Font.footnote.monospaced()
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
