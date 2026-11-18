import SwiftUI

struct InlineError: View {
    let text: String
    var body: some View {
        Label(text, systemImage: "exclamationmark.circle")
            .font(.caption).foregroundStyle(Palette.warning).lineSpacing(3)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14).background(Palette.warning.opacity(0.07), in: RoundedRectangle(cornerRadius: 12))
    }
}

struct ConnectionBadge: View {
    let online: Bool
    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(online ? Palette.success : Palette.secondary).frame(width: 5, height: 5)
            Text(online ? "Online" : "Offline").font(.system(size: 11, weight: .medium))
        }
        .foregroundStyle(online ? Palette.success : Palette.secondary)
    }
}

struct SectionHeading: View {
    let title: String
    let count: Int
    var body: some View {
        HStack(spacing: 7) {
            Text(title).foregroundStyle(Palette.secondary)
            Text("\(count)").foregroundStyle(Palette.secondary.opacity(0.6))
            Spacer()
        }
        .font(.system(size: 11, weight: .semibold)).textCase(.uppercase).tracking(1)
        .padding(.top, 10).padding(.bottom, 2)
    }
}

struct EmptyWorkspace: View {
    let title: String
    let text: String
    let symbol: String
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: symbol).font(.system(size: 30, weight: .light)).foregroundStyle(Palette.accent)
                .frame(width: 76, height: 76).background(Palette.accent.opacity(0.055), in: RoundedRectangle(cornerRadius: 24))
            Text(title).font(.title3.weight(.semibold)).tracking(-0.5)
            Text(text).font(.subheadline).foregroundStyle(Palette.secondary).multilineTextAlignment(.center).lineSpacing(4)
        }
        .frame(maxWidth: .infinity).padding(.horizontal, 24).padding(.vertical, 60)
    }
}
