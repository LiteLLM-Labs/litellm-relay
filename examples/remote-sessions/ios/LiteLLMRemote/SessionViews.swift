import SwiftUI

struct SessionsView: View {
    @Bindable var store: RemoteStore
    @State private var search = ""
    @State private var showingDevice = false
    @State private var liveOnly = true
    @Environment(\.scenePhase) private var scenePhase

    private var visible: [CodingSession] {
        store.sessions.filter {
            (!liveOnly || $0.live) && (search.isEmpty || "\($0.title) \($0.project)".localizedCaseInsensitiveContains(search))
        }.sorted {
            if ($0.status == "active") != ($1.status == "active") { return $0.status == "active" }
            return $0.updatedAt > $1.updatedAt
        }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    HStack(spacing: 10) {
                        Image("LiteLLMWordmark").resizable().scaledToFit().frame(width: 106, height: 22)
                        Spacer()
                        Text("\(store.sessions.filter(\.live).count) connected")
                            .font(.system(size: 9, weight: .medium, design: .monospaced))
                            .foregroundStyle(Palette.success)
                    }
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
                    Picker("Session filter", selection: $liveOnly) {
                        Text("Live sessions").tag(true)
                        Text("All sessions").tag(false)
                    }
                    .pickerStyle(.segmented)
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
                }
                if let error = store.error {
                    Section { Label(error, systemImage: "wifi.exclamationmark").font(.caption).foregroundStyle(.orange) }
                }
                Section {
                    ForEach(visible) { session in
                        NavigationLink(value: session) { SessionRow(session: session) }
                            .listRowBackground(Palette.surface)
                            .listRowSeparatorTint(Palette.border)
                    }
                    if store.nextCursor != nil {
                        Button("Load older sessions") { Task { await store.refresh(older: true) } }
                            .font(.caption)
                            .frame(maxWidth: .infinity)
                            .listRowBackground(Palette.surface)
                    }
                } header: { Text("ON YOUR MAC").font(.system(size: 9, weight: .medium, design: .monospaced)).tracking(2) }
            }
            .scrollContentBackground(.hidden)
            .background(Palette.background)
            .navigationTitle("Sessions")
            .searchable(text: $search, prompt: "Find a session")
            .refreshable { await store.refresh() }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Device and account", systemImage: "desktopcomputer") { showingDevice = true }
                        .labelStyle(.iconOnly)
                }
            }
            .navigationDestination(for: CodingSession.self) { session in SessionView(store: store, session: session) }
            .sheet(isPresented: $showingDevice) { DeviceView(store: store) }
            .overlay {
                if store.sessions.isEmpty && store.error == nil {
                    ContentUnavailableView("No gateway sessions yet", systemImage: "terminal", description: Text("Start a Codex session through your LiteLLM gateway, then pull to refresh."))
                }
            }
            .task {
                await store.refresh()
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(12)) } catch { return }
                    if scenePhase == .active { await store.refresh() }
                }
            }
        }
    }
}

private struct SessionRow: View {
    let session: CodingSession

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(session.project, systemImage: "folder")
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundStyle(Palette.secondary)
                    .lineLimit(1)
                Spacer()
                Text(session.updatedDate, style: .relative)
                    .font(.system(size: 9))
                    .foregroundStyle(Palette.secondary.opacity(0.8))
            }
            Text(session.title).font(.subheadline.weight(.medium)).lineLimit(2).lineSpacing(4)
            HStack(spacing: 5) {
                if session.live { Circle().fill(Palette.success).frame(width: 5, height: 5) }
                Text("Codex · \(session.activity)")
                    .font(.caption2)
                    .foregroundStyle(session.live ? Palette.success : Palette.secondary)
            }
        }
        .padding(.vertical, 12)
    }
}

struct SessionView: View {
    let store: RemoteStore
    let session: CodingSession
    @State private var detail: SessionDetail?
    @State private var error: String?
    @State private var followed = false
    @State private var scrollRequest = 0
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "eye").foregroundStyle(Palette.accent)
                Text("Live view").foregroundStyle(Palette.accent)
                Text("·").foregroundStyle(Palette.secondary)
                Text((detail?.session ?? session).activity).foregroundStyle(Palette.secondary)
                Spacer()
                Text("READ ONLY").font(.system(size: 8, weight: .medium, design: .monospaced)).foregroundStyle(Palette.secondary)
            }
            .font(.caption2)
            .padding(.horizontal, 22)
            .padding(.vertical, 13)
            .background(Palette.surface)
            if let error {
                Text(error).font(.caption).foregroundStyle(.orange).padding(12).frame(maxWidth: .infinity)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 25) {
                        if detail?.olderMessages == true {
                            Text("Showing the latest conversation")
                                .font(.caption2).foregroundStyle(Palette.secondary).frame(maxWidth: .infinity).padding(.bottom, 8)
                        }
                        ForEach(detail?.messages ?? []) { message in MessageView(message: message).id(message.id) }
                        Color.clear.frame(height: 1).id("latest")
                    }
                    .padding(22)
                }
                .onChange(of: detail?.messages.last?.id) {
                    guard !followed else { return }
                    proxy.scrollTo("latest", anchor: .bottom)
                    followed = true
                }
                .onChange(of: scrollRequest) { proxy.scrollTo("latest", anchor: .bottom) }
            }
            HStack(spacing: 11) {
                Image(systemName: "desktopcomputer").foregroundStyle(Palette.accent)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Running on your Mac").font(.caption.weight(.medium))
                    Text("Messages and approvals stay in Codex for this preview.")
                        .font(.system(size: 10)).foregroundStyle(Palette.secondary)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 22)
            .padding(.vertical, 15)
            .background(Palette.surface)
        }
        .background(Palette.background)
        .navigationTitle(session.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Latest message", systemImage: "arrow.down.to.line") { scrollRequest += 1 }
                    .labelStyle(.iconOnly)
            }
        }
        .task(id: session.id) {
            await update()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
                if scenePhase == .active { await update() }
            }
        }
    }

    private func update() async {
        let result = await store.detail(id: session.id)
        guard !Task.isCancelled else { return }
        switch result {
        case .success(let value): detail = value; error = nil
        case .failure(let failure): error = failure.message
        }
    }
}

private struct MessageView: View {
    let message: SessionMessage
    @State private var expanded = false

    var body: some View {
        if message.role == "activity" {
            DisclosureGroup {
                Text(message.text).font(.system(size: 11, design: .monospaced)).foregroundStyle(Palette.secondary).textSelection(.enabled)
            } label: {
                Label(message.text.components(separatedBy: .newlines).first ?? "Activity", systemImage: "terminal")
                    .font(.system(size: 10, design: .monospaced)).foregroundStyle(Palette.secondary).lineLimit(1)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(Palette.border))
        } else {
            VStack(alignment: .leading, spacing: 13) {
                HStack(spacing: 7) {
                    Image(systemName: message.role == "user" ? "person.crop.square" : "command.square.fill")
                        .foregroundStyle(message.role == "user" ? Palette.secondary : Palette.accent)
                    Text(message.role == "user" ? "You" : "Codex")
                        .font(.caption.weight(.medium)).foregroundStyle(Palette.secondary)
                }
                Text(expanded ? message.text : String(message.text.prefix(6500)))
                    .font(.system(size: 14))
                    .lineSpacing(6)
                    .textSelection(.enabled)
                    .foregroundStyle(Palette.ink)
                if message.text.count > 6500 && !expanded {
                    Button("Show full message") { expanded = true }.font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(message.role == "user" ? 16 : 0)
            .background(message.role == "user" ? Palette.surface : .clear, in: RoundedRectangle(cornerRadius: 13))
        }
    }
}

private struct DeviceView: View {
    let store: RemoteStore
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("Connected Mac") {
                    Label(store.device?.name ?? "Your Mac", systemImage: "desktopcomputer")
                    Text(store.address).font(.system(.caption, design: .monospaced)).foregroundStyle(Palette.secondary)
                }
                Section("LiteLLM gateway") {
                    Text(store.configuration?.gateway ?? "").font(.system(.caption, design: .monospaced))
                    if let identity = store.configuration?.identity { Text(identity.label).font(.caption).foregroundStyle(Palette.secondary) }
                    Label("Credentials stay on your Mac", systemImage: "lock.shield").font(.caption).foregroundStyle(Palette.accent)
                }
                Section {
                    Text("Keep this Mac awake and both devices on the same Wi-Fi. If your gateway requires Cloudflare One, connect it on your iPhone too.")
                        .font(.caption).foregroundStyle(Palette.secondary)
                }
                Section {
                    Button("Sign out", role: .destructive) { Task { await store.signOut(); dismiss() } }
                }
            }
            .navigationTitle("Your connection")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } } }
            .task { await store.loadDevice() }
        }
        .presentationDetents([.medium, .large])
    }
}
