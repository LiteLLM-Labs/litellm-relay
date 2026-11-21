import SwiftUI

struct InlineError: View {
    let text: String
    var body: some View {
        Label(text, systemImage: "exclamationmark.circle")
            .font(Fonts.caption).foregroundStyle(Palette.warn).lineSpacing(3)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Space.s3 + 2)
            .background(Palette.warn.opacity(0.07), in: .rect(cornerRadius: Radius.inset, style: .continuous))
    }
}

struct ConnectionBadge: View {
    let online: Bool
    var body: some View {
        HStack(spacing: Space.s1 + 1) {
            Circle().fill(online ? Palette.good : Palette.inkSoft).frame(width: 6, height: 6)
            Text(online ? "Online" : "Offline").font(Fonts.caption.weight(.medium))
        }
        .foregroundStyle(online ? Palette.good : Palette.inkSoft)
    }
}

struct SectionHeading: View {
    let title: String
    let count: Int
    var body: some View {
        HStack(spacing: Space.s2 - 1) {
            Text(title).foregroundStyle(Palette.inkSoft)
            Text("\(count)").foregroundStyle(Palette.inkSoft.opacity(0.6)).monospacedDigit()
            Spacer()
        }
        .font(Fonts.eyebrow).textCase(.uppercase).tracking(1)
        .padding(.top, Space.s2 + 2).padding(.bottom, Space.s1 - 2)
    }
}

struct EmptyWorkspace: View {
    let title: String
    let text: String
    let symbol: String
    var body: some View {
        VStack(spacing: Space.s4) {
            Image(systemName: symbol).font(.system(size: 30, weight: .light)).foregroundStyle(Palette.accent)
                .frame(width: 76, height: 76)
                .background(Palette.accent.opacity(0.055), in: .rect(cornerRadius: Radius.container, style: .continuous))
            Text(title).font(.title3.weight(.semibold)).tracking(-0.5)
            Text(text).font(Fonts.label).foregroundStyle(Palette.inkSoft)
                .multilineTextAlignment(.center).lineSpacing(4)
        }
        .frame(maxWidth: .infinity).padding(.horizontal, Space.s6).padding(.vertical, Space.s12 + Space.s3)
    }
}
