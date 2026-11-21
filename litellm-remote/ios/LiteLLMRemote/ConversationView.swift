import SwiftUI
import UIKit

struct SessionView: View {
    let store: RemoteStore
    let route: SessionRoute
    @State private var detail: SessionDetail?
    @State private var loadError: String?
    @State private var actionError: String?
    @State private var draft = ""
    @State private var sending = false
    @State private var deciding = false
    @State private var stopping = false
    @State private var stopRequestId = UUID().uuidString
    @State private var requestId = UUID().uuidString
    @State private var followed = false
    @State private var scrollRequest = 0
    @State private var selectedModel: String?
    @State private var models: [ModelOption] = []
    @State private var modelError: String?
    @State private var showingModels = false
    @State private var modelSearch = ""
    @Environment(\.scenePhase) private var scenePhase

    private var session: CodingSession { detail?.session ?? route.session }
    private var online: Bool { store.deviceIsOnline(route.deviceId) }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: "folder").font(.system(size: 10))
                Text(session.project).font(.system(size: 11, weight: .medium)).lineLimit(1)
                Spacer()
                if online {
                    Circle().fill(session.status == "working" ? Palette.accent : Palette.success).frame(width: 5, height: 5)
                    Text(session.activity).font(.system(size: 11, weight: .medium))
                } else {
                    ConnectionBadge(online: false)
                }
            }
            .foregroundStyle(Palette.secondary).padding(.horizontal, 22).padding(.vertical, 13)
            .background(Palette.surface)
            if !online {
                InlineError(text: "This device is offline. Reconnect it to continue this session.").padding(.horizontal, 18).padding(.top, 8)
            } else if let error = actionError ?? (session.status == "handoff_required" ? nil : detail?.error) ?? loadError ?? modelError {
                InlineError(text: error).padding(.horizontal, 18).padding(.top, 8)
            }
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 22) {
                        if detail?.olderMessages == true { Text("Latest messages").font(.caption2).foregroundStyle(Palette.secondary).frame(maxWidth: .infinity) }
                        if online && session.status == "handoff_required" {
                            VStack(alignment: .leading, spacing: 14) {
                                Label("Set up mobile control", systemImage: "laptopcomputer").font(.headline)
                                Text(detail?.error ?? "Finish the current turn and hand this session over from Relay on the device.")
                                    .font(.system(size: 13, design: .monospaced)).textSelection(.enabled)
                            }
                            .padding(20).frame(maxWidth: .infinity, alignment: .leading)
                            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 18))
                        } else if online && detail?.messages.isEmpty == true {
                            EmptyWorkspace(title: "What are we building?", text: "Send a message to start working in \(session.project).", symbol: "terminal")
                        }
                        ForEach(detail?.messages ?? []) { message in MessageView(message: message, agentName: session.agentName).id(message.id) }
                        ForEach(detail?.approvals ?? []) { approval in approvalCard(approval) }
                        if online && session.status == "working" {
                            HStack(spacing: 10) { ProgressView().controlSize(.mini); Text("\(session.agentName) is working").font(.caption) }.foregroundStyle(Palette.secondary)
                        }
                        Color.clear.frame(height: 1).id("latest")
                    }
                    .padding(.horizontal, 22).padding(.vertical, 26)
                }
                .scrollDismissesKeyboard(.interactively)
                .onChange(of: detail?.messages.last?.id) {
                    if !followed { scrollToLatest(proxy); followed = true }
                }
                .onChange(of: scrollRequest) { scrollToLatest(proxy) }
            }
        }
        .background(Palette.background)
        .navigationTitle(session.title).navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if session.canStop {
                    Button { Task { await stopTurn() } } label: {
                        if stopping { ProgressView().controlSize(.small).frame(width: 18, height: 18) }
                        else { Label("Stop", systemImage: "stop.fill").labelStyle(.titleAndIcon).font(.caption.weight(.semibold)) }
                    }
                    .disabled(stopping || !online).accessibilityLabel("Stop current turn")
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Latest message", systemImage: "arrow.down") { scrollRequest += 1 }.labelStyle(.iconOnly)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .task(id: online) { if online { await loadModels() } }
        .sheet(isPresented: $showingModels) { allModels }
        .task(id: online) {
            guard online else { return }
            await update()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
                if scenePhase == .active { await update() }
            }
        }
    }

    private var composer: some View {
        VStack(spacing: 10) {
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    Menu {
                        Button("Use session default") { selectedModel = nil; requestId = UUID().uuidString }
                        ForEach(Array(models.prefix(12))) { model in
                            Button {
                                selectedModel = model.id
                                requestId = UUID().uuidString
                            } label: {
                                if (selectedModel ?? session.model) == model.id { Label(model.label, systemImage: "checkmark") }
                                else { Text(model.label) }
                            }
                        }
                        if models.count > 12 { Button("All models…") { showingModels = true } }
                        if modelError != nil { Button("Retry model list") { Task { await loadModels() } } }
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "sparkles").font(.system(size: 10))
                            Text(models.first { $0.id == (selectedModel ?? session.model) }?.label ?? selectedModel ?? session.model ?? "Session model")
                                .font(.system(size: 11, weight: .medium)).lineLimit(1)
                            Image(systemName: "chevron.down").font(.system(size: 8, weight: .semibold))
                        }
                        .foregroundStyle(Palette.ink).padding(.vertical, 6)
                    }
                    .accessibilityLabel("Choose model").accessibilityIdentifier("modelPicker")
                    .disabled(sending || !online || !session.canSend)
                    Spacer()
                    Text(session.status == "working" ? "Working" : "Gateway").font(.system(size: 10)).foregroundStyle(Palette.secondary)
                }
                .padding(.horizontal, 16).padding(.top, 6).padding(.bottom, 3)
                Divider().overlay(Palette.border.opacity(0.6)).padding(.horizontal, 12)
            HStack(alignment: .bottom, spacing: 12) {
                TextField("Message \(session.agentName)…", text: $draft, axis: .vertical).lineLimit(1...6).font(.system(size: 15))
                    .padding(.horizontal, 16).padding(.vertical, 14)
                    .disabled(sending || !online || !session.canSend).accessibilityIdentifier("messageComposer")
                    .onChange(of: draft) { if !sending { requestId = UUID().uuidString } }
                Button { Task { await send() } } label: {
                    Group {
                        if sending { ProgressView().tint(.white) }
                        else { Image(systemName: "arrow.up").font(.system(size: 16, weight: .semibold)) }
                    }
                    .frame(width: 38, height: 38).foregroundStyle(.white)
                    .background(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? Palette.secondary.opacity(0.3) : Palette.accent, in: RoundedRectangle(cornerRadius: 12))
                }
                .padding(6).accessibilityLabel("Send message").accessibilityIdentifier("sendMessageButton")
                .disabled(sending || !online || !session.canSend || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || draft.count > 32000)
            }
            }
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 19))
            .overlay(RoundedRectangle(cornerRadius: 19).strokeBorder(Palette.border))
            HStack(spacing: 5) {
                Image(systemName: "lock.shield").font(.system(size: 9))
                Text("Via your LiteLLM gateway").font(.system(size: 10))
            }
            .foregroundStyle(Palette.secondary)
        }
        .padding(.horizontal, 16).padding(.top, 12).padding(.bottom, 8).background(Palette.background)
    }

    private var allModels: some View {
        NavigationStack {
            List(models.filter { modelSearch.isEmpty || "\($0.label) \($0.id)".localizedCaseInsensitiveContains(modelSearch) }) { model in
                Button {
                    selectedModel = model.id
                    requestId = UUID().uuidString
                    showingModels = false
                } label: {
                    HStack {
                        Text(model.label).font(.subheadline).foregroundStyle(Palette.ink)
                        Spacer()
                        if (selectedModel ?? session.model) == model.id { Image(systemName: "checkmark").foregroundStyle(Palette.accent) }
                    }
                    .padding(.vertical, 5)
                }
            }
            .searchable(text: $modelSearch, prompt: "Search gateway models")
            .navigationTitle("Choose model").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { showingModels = false } } }
        }
        .presentationDetents([.medium, .large])
    }

    private func approvalCard(_ approval: SessionApproval) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("APPROVAL NEEDED").font(.system(size: 10, weight: .bold)).tracking(1).foregroundStyle(Palette.warning)
            Label(approval.title, systemImage: "hand.raised").font(.subheadline.weight(.semibold))
            ScrollView { Text(approval.text).font(.system(.caption, design: .monospaced)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                .frame(maxHeight: 240).padding(12).background(Palette.background, in: RoundedRectangle(cornerRadius: 10))
            HStack {
                Button("Decline", role: .destructive) { Task { await decide(approval, decision: "decline") } }.buttonStyle(.bordered)
                if approval.canAccept { Button("Allow once") { Task { await decide(approval, decision: "accept") } }.buttonStyle(.borderedProminent).foregroundStyle(.white) }
            }
            .disabled(deciding || !online)
            .controlSize(.large)
        }
        .padding(18).frame(maxWidth: .infinity, alignment: .leading).background(Palette.surface, in: RoundedRectangle(cornerRadius: 17))
        .overlay(RoundedRectangle(cornerRadius: 17).strokeBorder(Palette.warning.opacity(0.3)))
    }

    private func update() async {
        guard online else { return }
        switch await store.detail(id: session.id, deviceId: route.deviceId) {
        case .success(let value):
            guard !Task.isCancelled else { return }
            // Polling returns identical payloads most of the time; skipping those
            // keeps the message list from re-rendering every two seconds.
            if detail != value { detail = value }
            loadError = nil
        case .failure(let failure): if !Task.isCancelled && failure.status != 499 { loadError = failure.message }
        }
    }

    private func stopTurn() async {
        guard !stopping, online else { return }
        stopping = true
        switch await store.interrupt(id: session.id, deviceId: route.deviceId, requestId: stopRequestId) {
        case .success: actionError = nil; stopRequestId = UUID().uuidString
        case .failure(let failure): actionError = failure.message
        }
        await update()
        stopping = false
    }

    private func send() async {
        guard !sending, online else { return }
        sending = true
        actionError = nil
        switch await store.send(id: session.id, deviceId: route.deviceId, text: draft, requestId: requestId, model: selectedModel) {
        case .success:
            draft = ""
            requestId = UUID().uuidString
            await update()
            scrollRequest += 1
        case .failure(let failure): actionError = failure.message
        }
        sending = false
    }

    private func loadModels() async {
        var cursor: String?
        var loaded: [ModelOption] = []
        repeat {
            switch await store.models(deviceId: route.deviceId, cursor: cursor) {
            case .success(let page): loaded += page.models; cursor = page.nextCursor
            case .failure(let failure):
                if !Task.isCancelled { modelError = failure.message }
                return
            }
        } while cursor != nil && !Task.isCancelled
        if !Task.isCancelled { models = loaded; modelError = nil }
    }

    private func decide(_ approval: SessionApproval, decision: String) async {
        guard !deciding, online else { return }
        deciding = true
        switch await store.approve(id: session.id, deviceId: route.deviceId, approvalId: approval.id, decision: decision) {
        case .success: actionError = nil
        case .failure(let failure): actionError = failure.message
        }
        await update()
        deciding = false
    }

    // ScrollViewProxy.scrollTo animates by default. An in-flight animation
    // against a LazyVStack whose height is still changing (Stop, or a send
    // that replaces the streamed turn) retriggers layout until the process
    // is killed.
    private func scrollToLatest(_ proxy: ScrollViewProxy) {
        var transaction = Transaction(animation: nil)
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            proxy.scrollTo("latest", anchor: .bottom)
        }
    }
}

private struct MessageView: View {
    let message: SessionMessage
    let agentName: String
    @State private var expanded = false

    private var visibleText: String {
        expanded ? message.text : String(message.text.prefix(8000))
    }

    var body: some View {
        if message.role == "activity" {
            DisclosureGroup {
                Text(message.text).font(.system(size: 11, design: .monospaced)).padding(.top, 8)
            } label: {
                Label(String(message.text.prefix(while: { !$0.isNewline })), systemImage: "terminal")
                    .font(.system(size: 11, design: .monospaced)).lineLimit(1)
            }
            .foregroundStyle(Palette.secondary).padding(13)
            .background(Palette.border.opacity(0.3), in: RoundedRectangle(cornerRadius: 12))
            .contextMenu { copyButton }
        } else if message.role == "user" {
            HStack(alignment: .top, spacing: 0) {
                Spacer(minLength: 48)
                VStack(alignment: .leading, spacing: 10) {
                    Text(visibleText)
                        .font(.system(size: 15)).lineSpacing(4)
                        .fixedSize(horizontal: false, vertical: true)
                    if message.text.count > 8000 && !expanded {
                        Button("Show full message") { expanded = true }.font(.caption)
                    }
                }
                .padding(.horizontal, 17).padding(.vertical, 13)
                .foregroundStyle(Palette.ink)
                .background(Palette.accent.opacity(0.09), in: RoundedRectangle(cornerRadius: 20))
                .overlay(RoundedRectangle(cornerRadius: 20).strokeBorder(Palette.accent.opacity(0.1)))
                .layoutPriority(1)
                .contextMenu { copyButton }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
        } else {
            VStack(alignment: .leading, spacing: 13) {
                HStack(spacing: 9) {
                    BrandMark(size: 22).frame(width: 30, height: 30)
                        .background(.white, in: RoundedRectangle(cornerRadius: 10))
                    Text(agentName).font(.system(size: 12, weight: .semibold)).foregroundStyle(Palette.ink)
                }
                MarkdownText(text: visibleText)
                    .font(.system(size: 15)).lineSpacing(7).tint(Palette.accent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if message.text.count > 8000 && !expanded { Button("Show full message") { expanded = true }.font(.caption) }
            }
            .padding(17).frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 20))
            .overlay(RoundedRectangle(cornerRadius: 20).strokeBorder(Palette.border.opacity(0.7)))
            .contextMenu { copyButton }
        }
    }

    private var copyButton: some View {
        Button {
            UIPasteboard.general.string = message.text
        } label: {
            Label("Copy message", systemImage: "doc.on.doc")
        }
    }
}

// Parsing markdown inside `body` stalled the main thread on long streamed turns.
// Show plain text until a parse for this exact string exists. Parses run one at
// a time off the main actor. A cancelled poll is dropped; every other string,
// including neighboring bubbles, still gets its own parse.
private struct MarkdownText: View {
    let text: String
    @State private var rendered: RenderedMarkdown?

    var body: some View {
        Group {
            if let rendered, rendered.source == text {
                Text(rendered.value)
            } else {
                Text(verbatim: text)
            }
        }
        .task(id: text) {
            let source = text
            guard let parsed = await MarkdownQueue.shared.parse(source) else { return }
            guard !Task.isCancelled, source == text else { return }
            rendered = RenderedMarkdown(source: source, value: parsed)
        }
    }
}
