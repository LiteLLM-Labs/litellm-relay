import SwiftUI

struct PopoverView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            connectivityCard
            if model.daemonUp {
                if model.status == nil {
                    statusLine("Relay daemon answered an unreadable status", color: GlassTheme.orange)
                } else if model.signedIn {
                    accountCard
                    selectionCard
                    mcpCard
                } else {
                    signInCard
                }
            }
            footerBar
        }
        .padding(.horizontal, 18)
        .padding(.top, 14)
        .padding(.bottom, 16)
        .frame(width: 400)
        .background(GlassBackground())
        .environment(\.colorScheme, .dark)
    }

    private var header: some View {
        HStack(spacing: 8) {
            RelayMark().foregroundStyle(GlassTheme.ink)
            Text("LiteLLM Relay").font(GlassTheme.sectionTitle).foregroundStyle(GlassTheme.ink)
            Spacer()
            if model.busy {
                ProgressView().controlSize(.small)
            }
        }
    }

    private var connectivityCard: some View {
        VStack(alignment: .leading, spacing: 6) {
            if !model.daemonUp {
                statusLine("Relay daemon is not running", color: Color.red)
                Text("Port \(model.relayPort)").font(GlassTheme.caption).foregroundStyle(GlassTheme.textFaint)
            } else {
                HStack(alignment: .top, spacing: 8) {
                    gatewayLine
                    Spacer()
                    actionButton("Retry", disabled: model.busy) { model.run(.recheck) }
                }
                errorText(for: .recheck)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassCard()
    }

    private var gatewayLine: some View {
        let probe = model.status?.account?.gateway
        let url = probe?.url ?? model.status?.broker?.gatewayUrl
        let host = StatusFormatter.gatewayHost(url) ?? "Gateway"
        return VStack(alignment: .leading, spacing: 3) {
            switch probe?.reachable {
            case .some(true):
                statusLine("Gateway \(host) reachable", color: GlassTheme.teal)
            case .some(false):
                statusLine("Gateway \(host) unreachable", color: GlassTheme.orange)
            case .none:
                statusLine("Gateway \(host)", color: GlassTheme.muted)
            }
            if let error = probe?.error, !error.isEmpty {
                Text(error).font(GlassTheme.caption).foregroundStyle(GlassTheme.textFaint).lineLimit(3)
            }
        }
    }

    private var signInCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Signed out").font(GlassTheme.label).foregroundStyle(GlassTheme.ink)
            Text("Sign in with your corporate identity; the Relay daemon keeps the credential, this app never sees it.")
                .font(GlassTheme.caption)
                .foregroundStyle(GlassTheme.muted)
                .fixedSize(horizontal: false, vertical: true)
            actionButton("Sign in", disabled: model.busy, prominent: true) { model.run(.signIn) }
            errorText(for: .signIn)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassCard()
    }

    private var accountCard: some View {
        let broker = model.status?.broker
        let keyLine = StatusFormatter.keyLine(source: broker?.source, keyExpiresAt: model.keyExpiresAt, now: model.now)
        return VStack(alignment: .leading, spacing: 6) {
            iconLine("person.crop.circle", StatusFormatter.displayName(broker: broker, account: model.status?.account), font: GlassTheme.label, color: GlassTheme.ink)
            iconLine("key", keyLine, font: GlassTheme.body, color: GlassTheme.muted)
            iconLine("dollarsign.circle", StatusFormatter.budgetLine(team: model.status?.account?.team, error: model.status?.account?.budgetError), font: GlassTheme.body, color: GlassTheme.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassCard()
    }

    private var selectionCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !model.environmentOptions.isEmpty {
                pickerRow("Environment") {
                    Picker("Environment", selection: environmentSelection) {
                        if model.status?.environments?.current == nil {
                            Text("Choose").tag("")
                        }
                        ForEach(model.environmentOptions) { entry in
                            Text(entry.name).tag(entry.name)
                        }
                    }
                }
                errorText(for: .switchEnvironment)
            }
            if model.teamOptions.isEmpty {
                Text("No team selected").font(GlassTheme.body).foregroundStyle(GlassTheme.muted)
            } else {
                pickerRow("Team") {
                    Picker("Team", selection: teamSelection) {
                        if model.status?.broker?.team == nil {
                            Text("Choose").tag("")
                        }
                        ForEach(model.teamOptions) { team in
                            Text(StatusFormatter.teamLabel(team)).tag(team.id)
                        }
                    }
                }
                errorText(for: .switchTeam)
            }
            if let teamsError = model.status?.account?.teamsError, !teamsError.isEmpty {
                Text(teamsError).font(GlassTheme.caption).foregroundStyle(GlassTheme.textFaint).lineLimit(3)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassCard()
    }

    private var mcpCard: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("MCP servers").font(GlassTheme.label).foregroundStyle(GlassTheme.ink)
            if model.mcpServers.isEmpty {
                Text("No MCP servers").font(GlassTheme.body).foregroundStyle(GlassTheme.muted)
            } else {
                ForEach(model.mcpServers, id: \.name) { entry in
                    HStack(spacing: 8) {
                        Circle()
                            .strokeBorder(GlassTheme.teal, lineWidth: 1.5)
                            .background(Circle().fill(entry.server.active == true ? GlassTheme.teal : Color.clear))
                            .frame(width: 8, height: 8)
                        Text(entry.name).font(GlassTheme.body).foregroundStyle(GlassTheme.ink)
                        Spacer()
                        Text(toolCount(entry.server.tools)).font(GlassTheme.caption).foregroundStyle(GlassTheme.muted)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassCard()
    }

    private var footerBar: some View {
        HStack(spacing: 8) {
            footerButton("Dashboard", "safari") { model.openDashboard() }
            Spacer()
            if model.signedIn {
                footerButton("Sign out", "rectangle.portrait.and.arrow.right") { model.run(.signOut) }
            }
            footerButton("Quit", "power") { model.quit() }
        }
        .padding(.top, 2)
    }

    private var environmentSelection: Binding<String> {
        Binding(
            get: { model.status?.environments?.current ?? "" },
            set: { name in
                guard !name.isEmpty, name != model.status?.environments?.current else { return }
                model.run(.switchEnvironment(name))
            }
        )
    }

    private var teamSelection: Binding<String> {
        Binding(
            get: { model.status?.broker?.team ?? "" },
            set: { team in
                guard !team.isEmpty, team != model.status?.broker?.team else { return }
                model.run(.switchTeam(team))
            }
        )
    }

    private func pickerRow<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        HStack(spacing: 10) {
            Text(title).font(GlassTheme.body).foregroundStyle(GlassTheme.muted).frame(width: 90, alignment: .leading)
            content()
                .labelsHidden()
                .pickerStyle(.menu)
                .disabled(model.busy)
                .accessibilityLabel(title)
        }
    }

    private func toolCount(_ tools: Int?) -> String {
        let count = tools ?? 0
        return count == 1 ? "1 tool" : "\(count) tools"
    }

    private func statusLine(_ text: String, color: Color) -> some View {
        HStack(spacing: 8) {
            Circle().fill(color).frame(width: 8, height: 8)
            Text(text).font(GlassTheme.body).foregroundStyle(color == GlassTheme.muted ? GlassTheme.ink : color)
        }
    }

    private func iconLine(_ icon: String, _ text: String, font: Font, color: Color) -> some View {
        HStack(spacing: 8) {
            Image(systemName: icon).font(.system(size: 12, weight: .semibold)).foregroundStyle(GlassTheme.muted).frame(width: 14)
            Text(text).font(font).foregroundStyle(color).lineLimit(2)
        }
    }

    @ViewBuilder
    private func errorText(for kind: RelayCommand.Kind) -> some View {
        if let message = model.errorMessage(for: kind) {
            Text(message)
                .font(GlassTheme.caption)
                .foregroundStyle(GlassTheme.orange)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("error")
        }
    }

    private func actionButton(_ title: String, disabled: Bool, prominent: Bool = false, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(GlassTheme.caption.weight(.semibold))
                .foregroundStyle(prominent ? Color.black : GlassTheme.ink)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .fill(prominent ? GlassTheme.ink : Color.white.opacity(0.10))
                )
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .opacity(disabled ? 0.5 : 1)
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
