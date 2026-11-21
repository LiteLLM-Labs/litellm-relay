import SwiftUI

struct WorkspaceView: View {
    @Bindable var store: RemoteStore
    @Environment(\.scenePhase) private var scenePhase
    var body: some View {
        TabView {
            SessionsView(store: store).tabItem { Label("Sessions", systemImage: "bubble.left.and.bubble.right") }
            DevicesView(store: store).tabItem { Label("Devices", systemImage: "laptopcomputer") }
            AccountView(store: store).tabItem { Label("Account", systemImage: "person.crop.circle") }
        }
        .tint(Palette.accent)
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            while !Task.isCancelled {
                await store.refreshDevices()
                do { try await Task.sleep(for: .seconds(8)) } catch { return }
            }
        }
    }
}

struct SessionsView: View {
    @Bindable var store: RemoteStore
    @State private var search = ""
    @State private var newSessionDevice: RegisteredDevice?
    @State private var path: [SessionRoute] = []
    @Environment(\.scenePhase) private var scenePhase

    private var visible: [CodingSession] {
        store.sessions.filter { search.isEmpty || "\($0.title) \($0.project)".localizedCaseInsensitiveContains(search) }
            .sorted { $0.updatedAt > $1.updatedAt }
    }
    private var browsingOnline: Bool { store.selectedDevice?.online == true }
    private var working: [CodingSession] { browsingOnline ? visible.filter { $0.status == "working" || $0.needsAttention } : [] }
    private var recent: [CodingSession] { browsingOnline ? visible.filter { $0.status != "working" && !$0.needsAttention } : visible }

    var body: some View {
        NavigationStack(path: $path) {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    HStack {
                        VStack(alignment: .leading, spacing: 7) {
                            Text("Sessions").font(.system(size: 35, weight: .bold)).tracking(-1.4)
                            Text("Pick up where you left off.").font(.subheadline).foregroundStyle(Palette.secondary)
                        }
                        Spacer()
                        Button { newSessionDevice = store.newSessionDevice } label: {
                            Image(systemName: "plus").font(.system(size: 19, weight: .medium))
                                .frame(width: 48, height: 48).background(Palette.accent, in: RoundedRectangle(cornerRadius: 16)).foregroundStyle(.white)
                        }
                        .accessibilityLabel("New session").accessibilityIdentifier("newSessionButton")
                        .disabled(store.newSessionDevice == nil)
                    }
                    if let device = store.selectedDevice {
                        Menu {
                            ForEach(store.devices) { item in Button(item.displayName) { Task { await store.selectDevice(item.id) } } }
                        } label: {
                            HStack(spacing: 9) {
                                Image(systemName: "laptopcomputer").foregroundStyle(Palette.ink)
                                Text(device.displayName).fontWeight(.medium).lineLimit(1)
                                Image(systemName: "chevron.down").font(.system(size: 8, weight: .bold)).foregroundStyle(Palette.secondary)
                                Spacer()
                                ConnectionBadge(online: device.online)
                            }
                            .font(.caption).padding(15).background(Palette.surface, in: RoundedRectangle(cornerRadius: 14))
                        }
                        .tint(Palette.ink)
                        .accessibilityIdentifier("sessionDevicePicker")
                    }
                    if !store.devices.isEmpty && store.newSessionDevice == nil {
                        Text("Connect a device to start a new session.")
                            .font(.caption).foregroundStyle(Palette.secondary)
                    }
                    if !store.sessions.isEmpty {
                        HStack(spacing: 10) {
                            Image(systemName: "magnifyingglass").foregroundStyle(Palette.secondary)
                            TextField("Search sessions", text: $search).font(.subheadline).autocorrectionDisabled()
                            if !search.isEmpty { Button { search = "" } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(Palette.secondary) }.accessibilityLabel("Clear search") }
                        }
                        .padding(14).background(Palette.border.opacity(0.42), in: RoundedRectangle(cornerRadius: 13))
                    }
                    if let error = store.error { InlineError(text: error) }
                    if store.devices.isEmpty {
                        EmptyWorkspace(title: "Connect a device", text: "Connect Relay to this workspace on your device. Your devices and sessions will appear here automatically.", symbol: "desktopcomputer")
                    } else if store.loading && store.sessions.isEmpty {
                        ProgressView("Loading your sessions").font(.caption).frame(maxWidth: .infinity).padding(.vertical, 70)
                    } else if !browsingOnline && store.sessions.isEmpty {
                        EmptyWorkspace(title: "Device offline", text: "Connect this device to load its sessions.", symbol: "wifi.slash")
                    } else if visible.isEmpty {
                        EmptyWorkspace(title: search.isEmpty ? "A fresh start" : "No matches", text: search.isEmpty ? "Start a session with the + button. Your work runs on the chosen device through LiteLLM." : "Try another project or session name.", symbol: search.isEmpty ? "square.and.pencil" : "magnifyingglass")
                    }
                    if !working.isEmpty { sessionSection("In progress", sessions: working) }
                    if !recent.isEmpty { sessionSection("Recent", sessions: recent) }
                    if store.nextCursor != nil {
                        Button("Load older sessions") { Task { await store.refresh(older: true) } }.font(.caption.weight(.medium)).frame(maxWidth: .infinity).padding(14)
                    }
                }
                .padding(.horizontal, 22).padding(.top, 18).padding(.bottom, 28)
            }
            .background(Palette.background)
            .scrollDismissesKeyboard(.interactively)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { BrandMark(size: 34) }
                ToolbarItem(placement: .topBarTrailing) {
                    Text("\(store.devices.filter(\.online).count) connected")
                        .font(.system(size: 11, weight: .medium)).foregroundStyle(Palette.secondary)
                }
            }
            .navigationDestination(for: SessionRoute.self) { SessionView(store: store, route: $0) }
            .sheet(item: $newSessionDevice) { device in
                NewSessionView(store: store, device: device) { session in
                    newSessionDevice = nil
                    path.append(SessionRoute(session: session, deviceId: device.id))
                }
            }
            .refreshable { await store.refreshDevices(); await store.refresh() }
            .task(id: store.selectedDeviceId) {
                await store.refresh()
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(8)) } catch { return }
                    if scenePhase == .active { await store.refresh() }
                }
            }
        }
    }

    private func sessionSection(_ title: String, sessions: [CodingSession]) -> some View {
        VStack(spacing: 10) {
            SectionHeading(title: title, count: sessions.count)
            ForEach(sessions) { session in
                if let deviceId = store.selectedDeviceId {
                    NavigationLink(value: SessionRoute(session: session, deviceId: deviceId)) {
                        SessionRow(session: session, online: browsingOnline)
                    }
                    .buttonStyle(.plain).accessibilityIdentifier("sessionRow-\(session.id)")
                }
            }
        }
    }
}

private struct SessionRow: View {
    let session: CodingSession
    let online: Bool
    private var color: Color { session.needsAttention ? Palette.warning : session.status == "working" ? Palette.accent : Palette.secondary }
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 7) {
                Image(systemName: "folder").font(.system(size: 10))
                Text(session.project).lineLimit(1)
                Spacer(minLength: 6)
                Text(session.updatedDate, style: .relative).font(.system(size: 10))
            }
            .font(.system(size: 11, weight: .medium)).foregroundStyle(Palette.secondary)
            Text(session.title).font(.system(size: 16, weight: .semibold)).tracking(-0.3).lineSpacing(4).lineLimit(2).foregroundStyle(Palette.ink)
            HStack(spacing: 6) {
                if online {
                    Circle().fill(color).frame(width: 5, height: 5)
                    Text(session.activity).foregroundStyle(color)
                } else {
                    ConnectionBadge(online: false)
                }
                Spacer()
                Text(session.agentName).foregroundStyle(Palette.secondary.opacity(0.75))
                Image(systemName: "chevron.right").font(.system(size: 9, weight: .semibold)).foregroundStyle(Palette.secondary.opacity(0.5))
            }
            .font(.system(size: 10, weight: .medium))
        }
        .padding(19).frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 18))
        .overlay(RoundedRectangle(cornerRadius: 18).strokeBorder(online && session.status == "working" ? Palette.accent.opacity(0.15) : Palette.border.opacity(0.5)))
    }
}
