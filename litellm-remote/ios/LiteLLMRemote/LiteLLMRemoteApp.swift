import SwiftUI

@main
struct LiteLLMRemoteApp: App {
    @State private var store = RemoteStore()
    @State private var restored = false

    var body: some Scene {
        WindowGroup {
            Group {
                if store.configuration?.identity != nil {
                    WorkspaceView(store: store)
                } else {
                    ConnectionView(store: store)
                }
            }
            .preferredColorScheme(.light)
            .foregroundStyle(Palette.ink)
            .tint(Palette.accent)
            .task {
                guard !restored else { return }
                restored = true
                await store.restore()
            }
        }
    }
}

enum Palette {
    static let background = Color(red: 0.967, green: 0.972, blue: 0.984)
    static let surface = Color.white
    static let accent = Color(red: 1 / 255, green: 23 / 255, blue: 190 / 255)
    static let ink = Color(red: 0.067, green: 0.094, blue: 0.153)
    static let secondary = Color(red: 0.42, green: 0.45, blue: 0.50)
    static let border = Color(red: 0.89, green: 0.90, blue: 0.92)
    static let success = Color(red: 0.09, green: 0.50, blue: 0.24)
    static let warning = Color(red: 0.65, green: 0.34, blue: 0.06)
}

struct BrandMark: View {
    var size: CGFloat = 28

    var body: some View {
        Image("LiteLLMMark")
            .resizable()
            .scaledToFit()
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}
