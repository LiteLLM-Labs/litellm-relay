import SwiftUI

struct ConnectionView: View {
    @Bindable var store: RemoteStore
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 26) {
                HStack(spacing: 12) {
                    Image("LiteLLMWordmark").resizable().scaledToFit().frame(width: 134, height: 28)
                    Text("Remote").font(.subheadline).foregroundStyle(Palette.secondary)
                    Spacer()
                }
                .padding(.top, 27)

                VStack(alignment: .leading, spacing: 11) {
                    Text(store.configuration == nil ? "Connect your workspace" : "Sign in to LiteLLM")
                        .font(.system(size: 29, weight: .semibold))
                        .tracking(-0.9)
                    Text("Your Codex sessions, connected to your iPhone through your LiteLLM gateway.")
                        .font(.subheadline)
                        .foregroundStyle(Palette.secondary)
                        .lineSpacing(4)
                }
                .padding(.top, 24)

                HStack(spacing: 12) {
                    step("Mac", number: "1", complete: store.configuration != nil, active: store.configuration == nil)
                    Rectangle().fill(Palette.border).frame(height: 1)
                    step("LiteLLM account", number: "2", complete: false, active: store.configuration != nil)
                }
                .padding(.vertical, 4)

                VStack(alignment: .leading, spacing: 21) {
                    if let configuration = store.configuration {
                        signIn(configuration)
                    } else {
                        connectionForm
                    }
                    if let error = store.error {
                        Label(error, systemImage: "exclamationmark.circle")
                            .font(.caption)
                            .foregroundStyle(.red)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .padding(22)
                .background(Palette.surface, in: RoundedRectangle(cornerRadius: 13))
                .overlay(RoundedRectangle(cornerRadius: 13).strokeBorder(Palette.border))

                HStack(alignment: .top, spacing: 11) {
                    Image(systemName: "lock.shield").foregroundStyle(Palette.accent)
                    VStack(alignment: .leading, spacing: 5) {
                        Text("Your work stays on your Mac").font(.caption.weight(.medium))
                        Text("This preview only reads sessions. It never restarts them or sends commands.")
                            .font(.caption).foregroundStyle(Palette.secondary).lineSpacing(3)
                    }
                }
                .padding(.horizontal, 3)

                HStack {
                    Text("Built on LiteLLM Relay")
                    Spacer()
                    Text("v0 preview")
                }
                .font(.caption2)
                .foregroundStyle(Palette.secondary)
                .padding(.top, 24)
            }
            .padding(.horizontal, 25)
            .padding(.bottom, 35)
            .frame(maxWidth: 550)
            .frame(maxWidth: .infinity)
        }
        .background(Palette.background)
        .scrollDismissesKeyboard(.interactively)
        .task(id: store.login?.code) {
            guard store.login != nil else { return }
            while !Task.isCancelled && store.login != nil {
                if scenePhase == .active && store.teams.isEmpty { await store.pollLogin() }
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
            }
        }
    }

    private func step(_ title: String, number: String, complete: Bool, active: Bool) -> some View {
        HStack(spacing: 7) {
            Group {
                if complete { Image(systemName: "checkmark") }
                else { Text(number) }
            }
            .font(.system(size: 10, weight: .semibold))
            .frame(width: 23, height: 23)
            .foregroundStyle(active || complete ? Color.white : Palette.secondary)
            .background(complete ? Palette.success : active ? Palette.accent : Palette.border, in: Circle())
            Text(title).font(.caption.weight(active ? .semibold : .regular)).fixedSize()
                .foregroundStyle(active ? Palette.ink : Palette.secondary)
        }
    }

    private var connectionForm: some View {
        VStack(alignment: .leading, spacing: 17) {
            HStack(spacing: 12) {
                Image(systemName: "desktopcomputer").font(.title3).foregroundStyle(Palette.accent)
                    .frame(width: 43, height: 43)
                    .background(Palette.accent.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
                VStack(alignment: .leading, spacing: 4) {
                    Text("Connect your Mac").font(.subheadline.weight(.semibold))
                    Text("Use the same Wi-Fi on both devices.").font(.caption).foregroundStyle(Palette.secondary)
                }
            }
            Text("Mac bridge address").font(.caption.weight(.medium)).padding(.top, 4)
            TextField("http://your-mac.local:4174", text: $store.address)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.system(size: 13, design: .monospaced))
                .padding(14)
                .background(Palette.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Palette.border))
                .accessibilityIdentifier("bridgeAddress")
                .onSubmit { Task { await store.connect() } }
            Button { Task { await store.connect() } } label: {
                HStack(spacing: 9) {
                    Spacer()
                    if store.busy { ProgressView().tint(.white) }
                    else { Text("Connect to Mac"); Image(systemName: "arrow.right") }
                    Spacer()
                }
                .font(.subheadline.weight(.semibold))
                .padding(.vertical, 9)
            }
            .buttonStyle(.borderedProminent)
            .foregroundStyle(.white)
            .disabled(store.busy || store.address.isEmpty)
            .accessibilityIdentifier("connectButton")
        }
    }

    @ViewBuilder private func signIn(_ configuration: BridgeConfiguration) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 10) {
                BrandMark(size: 34)
                VStack(alignment: .leading, spacing: 5) {
                    Text("LiteLLM workspace").font(.subheadline.weight(.semibold))
                    Text(URL(string: configuration.gateway)?.host ?? configuration.gateway)
                        .font(.system(size: 10, design: .monospaced)).foregroundStyle(Palette.secondary)
                }
            }
            Label("Mac connected", systemImage: "checkmark.circle.fill")
                .font(.caption).foregroundStyle(Palette.success)

            if let login = store.login {
                Text("Enter this code when LiteLLM asks, then return here.")
                    .font(.caption).foregroundStyle(Palette.secondary)
                HStack {
                    Text(login.code).font(.system(size: 25, weight: .semibold, design: .monospaced)).tracking(2)
                    Spacer()
                    Button("Copy verification code", systemImage: "doc.on.doc") { UIPasteboard.general.string = login.code }
                        .labelStyle(.iconOnly)
                }
                .foregroundStyle(Palette.accent)
                .padding(16)
                .background(Palette.accent.opacity(0.045), in: RoundedRectangle(cornerRadius: 9))
                if login.verificationUrl.scheme == "https" {
                    Button { openURL(login.verificationUrl) } label: {
                        HStack { Spacer(); Text("Continue to LiteLLM"); Image(systemName: "arrow.up.right"); Spacer() }
                            .font(.subheadline.weight(.semibold)).padding(.vertical, 9)
                    }
                    .buttonStyle(.borderedProminent).foregroundStyle(.white)
                }
                if store.teams.isEmpty {
                    HStack(spacing: 9) { ProgressView().controlSize(.mini); Text("Waiting for sign-in…").font(.caption) }
                        .foregroundStyle(Palette.secondary)
                } else {
                    Text("Choose your team").font(.caption).foregroundStyle(Palette.secondary)
                    ForEach(store.teams) { team in
                        Button(team.name) { Task { await store.pollLogin(team: team.id) } }.buttonStyle(.bordered)
                    }
                }
            } else {
                Button { Task { await store.startLogin() } } label: {
                    HStack { Spacer(); Text("Sign in with LiteLLM"); Image(systemName: "arrow.up.right"); Spacer() }
                        .font(.subheadline.weight(.semibold)).padding(.vertical, 9)
                }
                .buttonStyle(.borderedProminent)
                .foregroundStyle(.white)
                .disabled(store.busy)
                if configuration.localConnection {
                    Button("Use this Mac’s gateway connection") { Task { await store.localLogin() } }
                        .font(.caption).frame(maxWidth: .infinity).disabled(store.busy)
                        .accessibilityIdentifier("localLoginButton")
                }
                if !configuration.paired {
                    Text("Pair this Mac once using localhost before signing in from your phone.")
                        .font(.caption2).foregroundStyle(Palette.secondary)
                }
            }
            Button("Use another Mac") { store.disconnect() }
                .font(.caption2).foregroundStyle(Palette.secondary).frame(maxWidth: .infinity)
        }
    }
}
