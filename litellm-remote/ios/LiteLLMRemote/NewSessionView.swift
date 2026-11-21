import SwiftUI

struct NewSessionView: View {
    let store: RemoteStore
    let device: RegisteredDevice
    let onCreated: (CodingSession) -> Void
    @State private var cwd = ""
    @State private var agent = "codex"
    @State private var creating = false
    @State private var error: String?
    @State private var requestId = UUID().uuidString
    @Environment(\.dismiss) private var dismiss

    private var projects: [String] { Array(Set(store.sessions(for: device.id).map(\.cwd))).sorted() }
    private var online: Bool { store.deviceIsOnline(device.id) }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Label {
                        Text("A new conversation. Your project, connected through LiteLLM.")
                            .font(.subheadline)
                    } icon: { BrandMark(size: 28) }
                    Text("Starting on \(device.displayName)").font(.subheadline)
                }
                Section {
                    Picker("Coding agent", selection: $agent) {
                        Text("Codex").tag("codex")
                        Text("Claude Code").tag("claude")
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("newSessionAgent")
                }
                Section {
                    if !projects.isEmpty {
                        Picker("Recent projects", selection: $cwd) {
                            Text("Choose a folder").tag("")
                            ForEach(projects, id: \.self) { project in Text(project).tag(project) }
                            if !cwd.isEmpty && !projects.contains(cwd) { Text(cwd).tag(cwd) }
                        }
                        .font(.caption)
                    }
                    TextField("/path/to/project", text: $cwd)
                        .font(.system(.caption, design: .monospaced))
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("newSessionDirectory")
                } header: {
                    Text("Project folder on this device")
                } footer: {
                    Text("Runs the selected agent on this device through LiteLLM. Your project permissions still apply.")
                }
                if !online { Section { Text("This device is offline. Reconnect it or close this form to choose an available device.").font(.caption) } }
                if let error { Section { Text(error).font(.caption).foregroundStyle(.red) } }
                Section {
                    Button { Task { await create() } } label: {
                        HStack {
                            Spacer()
                            if creating { ProgressView() }
                            else { Text("Start session").fontWeight(.semibold) }
                            Spacer()
                        }
                    }
                    .disabled(creating || !online || !cwd.hasPrefix("/"))
                    .accessibilityIdentifier("startSessionButton")
                }
            }
            .disabled(creating)
            .scrollContentBackground(.hidden)
            .background(Palette.background)
            .navigationTitle("New session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
            .onAppear { if cwd.isEmpty { cwd = store.sessions(for: device.id).first?.cwd ?? "" } }
            .onChange(of: agent) { if !creating { requestId = UUID().uuidString } }
            .onChange(of: cwd) { if !creating { requestId = UUID().uuidString } }
        }
    }

    private func create() async {
        guard !creating, online else { return }
        creating = true
        error = nil
        switch await store.create(cwd: cwd, agent: agent, deviceId: device.id, requestId: requestId) {
        case .success(let session): onCreated(session)
        case .failure(let failure): error = failure.message
        }
        creating = false
    }
}
