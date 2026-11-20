import SwiftUI

struct ConnectionView: View {
    @Bindable var store: RemoteStore
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                BrandMark(size: 46).padding(.top, 38)
                Spacer().frame(height: 72)
                Text("Your workspace.\nWithin reach.")
                    .font(.system(size: 41, weight: .semibold)).tracking(-1.8).lineSpacing(-2)
                    .fixedSize(horizontal: false, vertical: true)
                Text("Keep building, wherever you are.")
                    .font(.body).foregroundStyle(Palette.inkSoft).padding(.top, 16)
                VStack(alignment: .leading, spacing: 22) {
                    if store.configuration == nil { workspaceForm }
                    else { signIn }
                    if let error = store.error { InlineError(text: error) }
                }
                .padding(.top, 48)
                HStack(spacing: 8) {
                    Image(systemName: "lock.shield")
                    Text("Your account. Your devices. Your LiteLLM gateway.")
                }
                .font(Fonts.caption).foregroundStyle(Palette.inkSoft)
                .padding(.top, 32)
                .padding(.bottom, 48)
            }
            .padding(.horizontal, 28)
            .frame(maxWidth: 520)
            .frame(maxWidth: .infinity, alignment: .center)
        }
        .background(Palette.canvas)
        .scrollDismissesKeyboard(.interactively)
        .task(id: store.login?.flowToken) {
            while store.login != nil && !Task.isCancelled {
                if scenePhase == .active && store.teams.isEmpty { await store.pollLogin() }
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
            }
        }
    }

    private var workspaceForm: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("LITELLM GATEWAY").font(Fonts.eyebrow).tracking(1.5).foregroundStyle(Palette.inkSoft)
            TextField("gateway.your-company.com", text: $store.gatewayAddress)
                .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                .font(Fonts.body).padding(Space.s4 + 2)
                .background(Palette.surface, in: Capsule())
                .overlay(Capsule().strokeBorder(Palette.border))
                .accessibilityIdentifier("workspaceAddress")
                .onSubmit { Task { await store.connect() } }
            DisclosureGroup("Advanced") {
                TextField("Remote-service address", text: $store.serviceOverride)
                    .keyboardType(.URL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    .font(Fonts.label).padding(Space.s3 + 2).background(Palette.surface, in: Capsule())
                    .accessibilityIdentifier("remoteServiceAddress")
            }
            .font(.caption).foregroundStyle(Palette.inkSoft).padding(.vertical, 5)
            Button { Task { await store.connect() } } label: {
                HStack {
                    Spacer()
                    if store.busy { ProgressView().tint(Palette.onAccent) }
                    else { Text(store.hasSavedSession ? "Reconnect" : "Continue").fontWeight(.semibold); Image(systemName: "arrow.right") }
                    Spacer()
                }
                .padding(.vertical, 10)
            }
            .buttonStyle(.borderedProminent).foregroundStyle(Palette.onAccent).controlSize(.large).buttonBorderShape(.capsule)
            .disabled(store.busy || store.gatewayAddress.isEmpty).accessibilityIdentifier("connectButton")
            Text(store.hasSavedSession ? "Your sign-in is saved. Reconnect to continue." : "Connect to your own gateway. Sign in with your work account.")
                .font(.caption).foregroundStyle(Palette.inkSoft).padding(.top, 4)
        }
    }

    @ViewBuilder private var signIn: some View {
        VStack(alignment: .leading, spacing: 20) {
            Label(URL(string: store.configuration?.gateway ?? "")?.host ?? "LiteLLM workspace", systemImage: "building.2")
                .font(.subheadline).foregroundStyle(Palette.inkSoft)
            if let login = store.login {
                Text("Your sign-in code").font(.subheadline.weight(.medium))
                HStack {
                    Text(login.code).font(.system(size: 30, weight: .semibold, design: .monospaced)).tracking(3)
                    Spacer()
                    Button("Copy code", systemImage: "doc.on.doc") { UIPasteboard.general.string = login.code }.labelStyle(.iconOnly)
                }
                .padding(Space.s5).background(Palette.surface, in: .rect(cornerRadius: Radius.container, style: .continuous))
                Text("Enter this code on the sign-in page, then return here.")
                    .font(.subheadline).foregroundStyle(Palette.inkSoft)
                if login.verificationUrl.scheme == "https" {
                    Button { openURL(login.verificationUrl) } label: {
                        HStack { Spacer(); Text("Open secure sign-in").fontWeight(.semibold); Image(systemName: "arrow.up.right"); Spacer() }.padding(.vertical, 9)
                    }
                    .buttonStyle(.borderedProminent).foregroundStyle(Palette.onAccent).controlSize(.large).buttonBorderShape(.capsule)
                }
                if store.teams.isEmpty {
                    HStack(spacing: 10) { ProgressView().controlSize(.small); Text("Waiting for you to sign in").font(.caption) }.foregroundStyle(Palette.inkSoft)
                } else {
                    ForEach(store.teams) { team in Button(team.name) { Task { await store.pollLogin(team: team.id) } }.buttonStyle(.bordered) }
                }
            } else {
                Button { Task { await store.startLogin() } } label: {
                    HStack { Spacer(); Text("Sign in with LiteLLM").fontWeight(.semibold); Image(systemName: "arrow.up.right"); Spacer() }.padding(.vertical, 9)
                }
                .buttonStyle(.borderedProminent).foregroundStyle(Palette.onAccent).controlSize(.large).buttonBorderShape(.capsule).disabled(store.busy)
            }
            Button("Use another workspace") { store.disconnect() }.font(.caption).foregroundStyle(Palette.inkSoft)
        }
    }
}
