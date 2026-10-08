import SwiftUI

@main
struct RelayBarGlassApp: App {
    @StateObject private var model = AppModel()

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
