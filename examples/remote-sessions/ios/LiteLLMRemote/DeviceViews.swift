import SwiftUI

struct DevicesView: View {
    let store: RemoteStore
    @State private var removing: RegisteredDevice?
    @State private var showingSetup = false
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    Text("Your devices").font(.system(size: 34, weight: .bold)).tracking(-1.3)
                    Text("Your workspace stays with you. Your files stay where they belong.")
                        .font(.subheadline).foregroundStyle(Palette.secondary).lineSpacing(4)
                    if store.devices.isEmpty {
                        EmptyWorkspace(title: "Connect your first Mac", text: "Sign in to the Relay Remote connector on your Mac using this LiteLLM account.", symbol: "laptopcomputer")
                    }
                    ForEach(store.devices) { device in
                        VStack(alignment: .leading, spacing: 20) {
                            HStack(spacing: 16) {
                                Image(systemName: "laptopcomputer").font(.system(size: 26, weight: .light))
                                    .frame(width: 55, height: 55).background(Palette.background, in: RoundedRectangle(cornerRadius: 17))
                                VStack(alignment: .leading, spacing: 7) {
                                    Text(device.name).font(.subheadline.weight(.semibold)).lineLimit(1)
                                    ConnectionBadge(online: device.online)
                                }
                                Spacer(minLength: 0)
                                Menu {
                                    Button("Disconnect device", role: .destructive) { removing = device }
                                } label: { Image(systemName: "ellipsis").frame(width: 30, height: 30).foregroundStyle(Palette.secondary) }.accessibilityLabel("Device options")
                            }
                            Divider().overlay(Palette.border)
                            HStack {
                                Text(device.platform).font(.caption).foregroundStyle(Palette.secondary)
                                Spacer()
                                if !device.online {
                                    Text(Date(timeIntervalSince1970: device.lastSeen / 1000), style: .relative).font(.caption2).foregroundStyle(Palette.secondary)
                                } else {
                                    Label("Encrypted connection", systemImage: "lock").font(.system(size: 10)).foregroundStyle(Palette.secondary)
                                }
                            }
                        }
                        .padding(20).background(.white, in: RoundedRectangle(cornerRadius: 20))
                    }
                    Button { showingSetup = true } label: {
                        Label("Connect a device", systemImage: "plus").font(.subheadline.weight(.semibold)).frame(maxWidth: .infinity).padding(12)
                    }
                    .buttonStyle(.bordered).buttonBorderShape(.roundedRectangle(radius: 14))
                    if let error = store.error { InlineError(text: error) }
                }
                .padding(24)
            }
            .background(Palette.background)
            .toolbar { ToolbarItem(placement: .topBarLeading) { BrandMark(size: 34) } }
            .refreshable { await store.refreshDevices() }
            .task { await store.refreshDevices() }
            .alert("Connect your Mac", isPresented: $showingSetup) {
                Button("Got it", role: .cancel) {}
            } message: {
                Text("Start the Relay Remote connector on your Mac and sign in to this workspace. Your Mac will appear here automatically. No IP address or port forwarding is needed.")
            }
            .confirmationDialog("Disconnect \(removing?.name ?? "this device")?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }), titleVisibility: .visible) {
                Button("Disconnect device", role: .destructive) {
                    if let device = removing { Task { await store.removeDevice(device.id) } }
                    removing = nil
                }
            } message: { Text("This revokes remote access. Your coding sessions and files are not deleted.") }
        }
    }
}

struct AccountView: View {
    let store: RemoteStore
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 26) {
                    Text("Account").font(.system(size: 34, weight: .bold)).tracking(-1.3)
                    VStack(alignment: .leading, spacing: 20) {
                        Label("Your LiteLLM workspace", systemImage: "building.2").font(.subheadline.weight(.semibold))
                        Text(URL(string: store.address)?.host ?? store.address).font(.subheadline).foregroundStyle(Palette.secondary)
                        Divider()
                        HStack {
                            Label("Signed in securely", systemImage: "checkmark.shield").font(.caption).foregroundStyle(Palette.success)
                            Spacer()
                            Text("SSO").font(.system(size: 10, weight: .semibold)).foregroundStyle(Palette.secondary)
                        }
                    }
                    .padding(22).background(.white, in: RoundedRectangle(cornerRadius: 20))
                    VStack(alignment: .leading, spacing: 14) {
                        Text("CONNECTED GATEWAY").font(.system(size: 10, weight: .semibold)).tracking(1.3).foregroundStyle(Palette.secondary)
                        Text(URL(string: store.configuration?.gateway ?? "")?.host ?? "LiteLLM").font(.subheadline.weight(.medium))
                        Text("Inference runs through your gateway. Your provider credentials never need to be on this phone.")
                            .font(.caption).foregroundStyle(Palette.secondary).lineSpacing(4)
                    }
                    .padding(22).background(.white, in: RoundedRectangle(cornerRadius: 20))
                    if let error = store.error { InlineError(text: error) }
                    Button("Sign out", role: .destructive) { Task { await store.signOut() } }
                        .font(.subheadline.weight(.medium)).frame(maxWidth: .infinity).padding(16)
                        .background(.white, in: RoundedRectangle(cornerRadius: 15))
                    Text("LiteLLM Remote · Preview")
                        .font(.system(size: 11)).foregroundStyle(Palette.secondary).frame(maxWidth: .infinity).padding(.top, 12)
                }
                .padding(24)
            }
            .background(Palette.background)
            .toolbar { ToolbarItem(placement: .topBarLeading) { BrandMark(size: 34) } }
        }
    }
}
