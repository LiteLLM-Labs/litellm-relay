import SwiftUI

/// Dark-glass popover: agent tab row on top, one tab-driven per-tool card below.
struct PopoverView: View {
    @ObservedObject var model: AppModel
    /// Measured height of the card body. A `ScrollView` has no intrinsic
    /// height, and the `MenuBarExtra` window sizes itself to the view's ideal
    /// size, so without an explicit height the body collapses to zero and only
    /// the tab row is visible (seen on macOS 26).
    @State private var bodyHeight: CGFloat = 0

    static let maxBodyHeight: CGFloat = 560

    var body: some View {
        VStack(spacing: 0) {
            AgentTabBar(model: model)
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 14) {
                    UsageCard(model: model)
                    footerBar
                }
                .padding(.horizontal, 18)
                .padding(.top, 14)
                .padding(.bottom, 16)
                .background(
                    GeometryReader { proxy in
                        Color.clear.preference(key: BodyHeightKey.self, value: proxy.size.height)
                    }
                )
            }
            .onPreferenceChange(BodyHeightKey.self) { bodyHeight = $0 }
            .frame(height: PopoverView.visibleBodyHeight(for: bodyHeight))
        }
        .frame(width: 400)
        .background(GlassBackground())
        .environment(\.colorScheme, .dark)
    }

    /// The height the body window gets: its content, capped so long cards scroll.
    static func visibleBodyHeight(for contentHeight: CGFloat) -> CGFloat {
        min(max(contentHeight, 1), maxBodyHeight)
    }

    private var footerBar: some View {
        HStack(spacing: 8) {
            footerButton("Dashboard", "safari") { model.openDashboard() }
            Spacer()
            footerButton("Quit", "power") { model.quit() }
        }
        .padding(.top, 2)
    }

    private func footerButton(_ title: String, _ icon: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 5) {
                Image(systemName: icon).font(.system(size: 10, weight: .semibold))
                Text(title).font(GlassTheme.caption.weight(.medium))
            }
            .foregroundStyle(GlassTheme.muted)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(Color.white.opacity(0.06))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .strokeBorder(Color.white.opacity(0.08), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
    }
}

private struct BodyHeightKey: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}
