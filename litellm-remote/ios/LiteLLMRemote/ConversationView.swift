import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct SessionView: View {
    let store: RemoteStore
    let route: SessionRoute
    @State private var detail: SessionDetail?
    @State private var loadError: String?
    @State private var actionError: String?
    @State private var draft = ""
    @State private var attachments: [DraftAttachment] = []
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
    @State private var showingSession = false
    @State private var importingFiles = false
    @State private var modelSearch = ""
    @FocusState private var composing: Bool
    @Environment(\.scenePhase) private var scenePhase

    private var session: CodingSession { detail?.session ?? route.session }
    private var working: Bool { session.status == "working" }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: Space.s3) {
                    if let error = actionError ?? (session.status == "handoff_required" ? nil : detail?.error) ?? loadError ?? modelError {
                        InlineError(text: error)
                    }
                    if detail?.olderMessages == true {
                        Text("Latest messages").font(Fonts.caption).foregroundStyle(Palette.inkSoft).frame(maxWidth: .infinity)
                    }
                    if session.status == "handoff_required" {
                        handoffCard
                    } else if detail?.messages.isEmpty == true {
                        EmptyWorkspace(title: "What are we building?", text: "Send a message to start working in \(session.project).", symbol: "terminal")
                    }
                    ForEach(detail?.messages ?? []) { message in
                        MessageView(message: message, agentName: session.agentName).id(message.id)
                    }
                    ForEach(detail?.approvals ?? []) { approval in approvalCard(approval) }
                    if working {
                        HStack(spacing: Space.s2) {
                            ProgressView().controlSize(.mini)
                            Text("\(session.agentName) is working").font(Fonts.caption)
                        }
                        .foregroundStyle(Palette.inkSoft)
                    }
                    Color.clear.frame(height: 1).id("latest")
                }
                .padding(.horizontal, Space.s4).padding(.vertical, Space.s6)
            }
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: detail?.messages.last?.id) {
                if !followed { proxy.scrollTo("latest", anchor: .bottom); followed = true }
            }
            .onChange(of: scrollRequest) { proxy.scrollTo("latest", anchor: .bottom) }
        }
        .background(Palette.canvas)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { sessionChip }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Latest message", systemImage: "arrow.down") { scrollRequest += 1 }
                    .labelStyle(.iconOnly).font(Fonts.caption)
            }
        }
        .toolbar(.hidden, for: .tabBar)
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .task(id: route.deviceId) { await loadModels() }
        .sheet(isPresented: $showingModels) { allModels }
        .sheet(isPresented: $showingSession) { sessionSheet }
        .fileImporter(
            isPresented: $importingFiles,
            allowedContentTypes: DraftAttachment.readableTypes,
            allowsMultipleSelection: true
        ) { attachFiles($0) }
        .task(id: route.session.id) {
            await update()
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
                if scenePhase == .active { await update() }
            }
        }
    }

    // MARK: Header

    private var sessionChip: some View {
        Button { showingSession = true } label: {
            HStack(spacing: Space.s1 + 2) {
                Circle().fill(statusColor).frame(width: 6, height: 6)
                Text(session.title).font(Fonts.label).foregroundStyle(Palette.ink).lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold)).foregroundStyle(Palette.inkSoft)
            }
            .padding(.horizontal, Space.s3 + 2).padding(.vertical, Space.s2 - 1)
            .background(Palette.surface, in: Capsule())
            .overlay(Capsule().strokeBorder(Palette.border))
        }
        .accessibilityLabel("Session details")
        .accessibilityIdentifier("sessionChip")
    }

    private var statusColor: Color {
        switch session.status {
        case "working": Palette.accent
        case "needs_approval", "error", "handoff_required": Palette.warn
        default: Palette.good
        }
    }

    private var handoffCard: some View {
        VStack(alignment: .leading, spacing: Space.s3 + 2) {
            Label("Set up mobile control", systemImage: "laptopcomputer").font(Fonts.title)
            Text(detail?.error ?? "Finish the current turn and hand this session over from Relay on your Mac.")
                .font(Fonts.mono).textSelection(.enabled)
        }
        .padding(Space.s5).frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.surface, in: .rect(cornerRadius: Radius.container, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Radius.container, style: .continuous).strokeBorder(Palette.border))
    }

    // MARK: Composer

    private var composer: some View {
        VStack(spacing: Space.s2) {
            VStack(spacing: Space.s3 - 2) {
                if !attachments.isEmpty { attachmentChips }
                HStack(alignment: .bottom, spacing: Space.s2) {
                    if !composing { attachButton }
                    TextField("Message \(session.agentName)…", text: $draft, axis: .vertical)
                        .lineLimit(1...6).font(Fonts.body)
                        .focused($composing)
                        .padding(.vertical, Space.s2 + 2)
                        .padding(.leading, composing ? Space.s1 : 0)
                        .accessibilityIdentifier("messageComposer")
                        .onChange(of: draft) { if !sending { requestId = UUID().uuidString } }
                    if !composing { actionCircle }
                }
                if composing {
                    HStack(spacing: Space.s2) {
                        attachButton
                        modelChip
                        Spacer(minLength: 0)
                        actionCircle
                    }
                }
            }
            .padding(composing ? Space.s3 : Space.s2 - 2)
            .padding(.leading, composing ? 0 : Space.s1)
            .background(Palette.surface, in: .rect(cornerRadius: composerRadius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: composerRadius, style: .continuous).strokeBorder(Palette.border))
            .animation(.snappy(duration: 0.18), value: composing)
            .animation(.snappy(duration: 0.18), value: attachments)
            HStack(spacing: Space.s1 + 1) {
                Image(systemName: "lock.shield").font(.system(size: 9))
                Text("Via your LiteLLM gateway").font(Fonts.caption)
            }
            .foregroundStyle(Palette.inkSoft)
        }
        .padding(.horizontal, Space.s4).padding(.top, Space.s3).padding(.bottom, Space.s2)
        .background(Palette.canvas)
    }

    private var composerRadius: CGFloat { composing || !attachments.isEmpty ? Radius.container : 28 }

    private var attachButton: some View {
        Button { importingFiles = true } label: {
            Image(systemName: "plus")
                .font(.system(size: 17, weight: .light))
                .frame(width: 40, height: 40)
                .foregroundStyle(Palette.inkSoft)
                .background(Palette.raised, in: Circle())
        }
        .disabled(sending || !session.canSend)
        .accessibilityLabel("Attach files")
        .accessibilityIdentifier("attachFilesButton")
    }

    private var modelChip: some View {
        Button { showingModels = true } label: {
            HStack(spacing: Space.s1 + 1) {
                Text(currentModelLabel).font(Fonts.label).lineLimit(1)
                Image(systemName: "chevron.down").font(.system(size: 8, weight: .semibold))
            }
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, Space.s3 + 2).frame(height: 40)
            .background(Palette.raised, in: Capsule())
        }
        .disabled(sending || !session.canSend)
        .accessibilityLabel("Choose model")
        .accessibilityIdentifier("modelPicker")
    }

    private var currentModelLabel: String {
        models.first { $0.id == (selectedModel ?? session.model) }?.label
            ?? selectedModel ?? session.model ?? "Session model"
    }

    private var actionCircle: some View {
        Button {
            if working { Task { await stopTurn() } } else { Task { await send() } }
        } label: {
            Group {
                if sending || stopping {
                    ProgressView().tint(Palette.onAccent)
                } else {
                    Image(systemName: working ? "stop.fill" : "arrow.up")
                        .font(.system(size: working ? 13 : 16, weight: .semibold))
                }
            }
            .frame(width: 40, height: 40)
            .foregroundStyle(sendEnabled || working ? Palette.onAccent : Palette.inkSoft)
            .background(sendEnabled || working ? Palette.accent : Palette.raised, in: Circle())
        }
        .disabled(working ? stopping : !sendEnabled)
        .accessibilityLabel(working ? "Stop current turn" : "Send message")
        .accessibilityIdentifier(working ? "stopTurnButton" : "sendMessageButton")
    }

    private var sendEnabled: Bool {
        !sending && session.canSend && composedLength <= 32_000
            && !(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && attachments.isEmpty)
    }

    private var composedLength: Int {
        draft.count + attachments.reduce(0) { $0 + $1.text.count + $1.name.count + 40 }
    }

    private var attachmentChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Space.s2) {
                ForEach(attachments) { attachment in
                    HStack(spacing: Space.s1 + 2) {
                        Image(systemName: "doc.text").font(.system(size: 11))
                        Text(attachment.name).font(Fonts.caption).lineLimit(1)
                        Button {
                            attachments.removeAll { $0.id == attachment.id }
                        } label: {
                            Image(systemName: "xmark").font(.system(size: 9, weight: .semibold))
                        }
                        .accessibilityLabel("Remove \(attachment.name)")
                    }
                    .foregroundStyle(Palette.inkSoft)
                    .padding(.horizontal, Space.s3).frame(height: 32)
                    .background(Palette.raised, in: Capsule())
                }
            }
            .padding(.horizontal, Space.s1)
        }
    }

    private func attachFiles(_ result: Result<[URL], Error>) {
        guard case .success(let urls) = result else { return }
        for url in urls {
            guard attachments.count < 5 else {
                actionError = "Up to five files per message."
                return
            }
            switch DraftAttachment.read(url: url) {
            case .success(let attachment):
                if composedLength + attachment.text.count > 30_000 {
                    actionError = "That file would push this message past the size limit. Send it in parts."
                } else {
                    attachments.append(attachment)
                    actionError = nil
                }
            case .failure(let failure):
                actionError = failure.message
            }
        }
    }

    // MARK: Sheets

    private var sessionSheet: some View {
        NavigationStack {
            List {
                Section {
                    Button {
                        showingSession = false
                        showingModels = true
                    } label: {
                        row(label: "Model", value: currentModelLabel, chevron: true)
                    }
                    row(label: "Agent", value: session.agentName)
                    row(label: "Project", value: session.cwd)
                    row(label: "Status", value: session.activity)
                } footer: {
                    Text("Inference runs through your LiteLLM gateway. The session itself lives on your paired Mac.")
                        .font(Fonts.caption)
                }
            }
            .navigationTitle(session.title).navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { showingSession = false } } }
        }
        .presentationDetents([.medium])
    }

    private func row(label: String, value: String, chevron: Bool = false) -> some View {
        HStack {
            Text(label).font(Fonts.label).foregroundStyle(Palette.ink)
            Spacer()
            Text(value).font(Fonts.caption).foregroundStyle(Palette.inkSoft)
                .lineLimit(1).truncationMode(.middle)
            if chevron {
                Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).foregroundStyle(Palette.inkSoft)
            }
        }
    }

    private var allModels: some View {
        NavigationStack {
            List {
                Button {
                    selectedModel = nil
                    requestId = UUID().uuidString
                    showingModels = false
                } label: {
                    HStack {
                        Text("Use session default").font(Fonts.label).foregroundStyle(Palette.ink)
                        Spacer()
                        if selectedModel == nil { Image(systemName: "checkmark").foregroundStyle(Palette.accent) }
                    }
                }
                ForEach(models.filter { modelSearch.isEmpty || "\($0.label) \($0.id)".localizedCaseInsensitiveContains(modelSearch) }) { model in
                    Button {
                        selectedModel = model.id
                        requestId = UUID().uuidString
                        showingModels = false
                    } label: {
                        HStack {
                            Text(model.label).font(Fonts.body).foregroundStyle(Palette.ink)
                            Spacer()
                            if (selectedModel ?? session.model) == model.id { Image(systemName: "checkmark").foregroundStyle(Palette.accent) }
                        }
                        .padding(.vertical, Space.s1)
                    }
                }
                if modelError != nil {
                    Button("Retry model list") { Task { await loadModels() } }.font(Fonts.label)
                }
            }
            .searchable(text: $modelSearch, prompt: "Search gateway models")
            .navigationTitle("Choose model").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { showingModels = false } } }
        }
        .presentationDetents([.medium, .large])
    }

    // MARK: Approvals

    private func approvalCard(_ approval: SessionApproval) -> some View {
        VStack(alignment: .leading, spacing: Space.s3) {
            Text("APPROVAL NEEDED").font(Fonts.eyebrow).tracking(1).foregroundStyle(Palette.warn)
            Label(approval.title, systemImage: "hand.raised").font(Fonts.title)
            ScrollView {
                Text(approval.text).font(Fonts.mono).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 240).padding(Space.s3)
            .background(Palette.canvas, in: .rect(cornerRadius: Radius.inset, style: .continuous))
            HStack(spacing: Space.s2) {
                Button { Task { await decide(approval, decision: "decline") } } label: {
                    Text("Decline").font(Fonts.label).frame(maxWidth: .infinity).frame(height: 44)
                }
                .foregroundStyle(Palette.ink)
                .background(Palette.raised, in: Capsule())
                if approval.canAccept {
                    Button { Task { await decide(approval, decision: "accept") } } label: {
                        Text("Allow once").font(Fonts.label).frame(maxWidth: .infinity).frame(height: 44)
                    }
                    .foregroundStyle(Palette.onAccent)
                    .background(Palette.accent, in: Capsule())
                }
            }
            .disabled(deciding)
        }
        .padding(Space.s4).frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.surface, in: .rect(cornerRadius: Radius.container, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Radius.container, style: .continuous).strokeBorder(Palette.warn.opacity(0.3)))
    }

    // MARK: Actions

    private func update() async {
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
        guard !stopping else { return }
        stopping = true
        switch await store.interrupt(id: session.id, deviceId: route.deviceId, requestId: stopRequestId) {
        case .success: actionError = nil; stopRequestId = UUID().uuidString
        case .failure(let failure): actionError = failure.message
        }
        await update()
        stopping = false
    }

    private func send() async {
        guard !sending else { return }
        sending = true
        actionError = nil
        let text = DraftAttachment.compose(draft: draft, attachments: attachments)
        switch await store.send(id: session.id, deviceId: route.deviceId, text: text, requestId: requestId, model: selectedModel) {
        case .success:
            draft = ""
            attachments = []
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
        guard !deciding else { return }
        deciding = true
        switch await store.approve(id: session.id, deviceId: route.deviceId, approvalId: approval.id, decision: decision) {
        case .success: actionError = nil
        case .failure(let failure): actionError = failure.message
        }
        await update()
        deciding = false
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
                Text(message.text).font(Fonts.mono).padding(.top, Space.s2)
            } label: {
                Label(String(message.text.prefix(while: { !$0.isNewline })), systemImage: "terminal")
                    .font(Fonts.mono).lineLimit(1)
            }
            .foregroundStyle(Palette.inkSoft).padding(.horizontal, Space.s3 + 2).padding(.vertical, Space.s2 + 2)
            .background(Palette.raised, in: .rect(cornerRadius: Radius.inset, style: .continuous))
            .contextMenu { copyButton }
        } else if message.role == "user" {
            HStack(alignment: .top, spacing: 0) {
                Spacer(minLength: Space.s12)
                VStack(alignment: .leading, spacing: Space.s2 + 2) {
                    Text(visibleText)
                        .font(Fonts.body).lineSpacing(4)
                        .fixedSize(horizontal: false, vertical: true)
                    if message.text.count > 8000 && !expanded {
                        Button("Show full message") { expanded = true }.font(Fonts.caption).foregroundStyle(Palette.onAccent.opacity(0.85))
                    }
                }
                .padding(.horizontal, Space.s4).padding(.vertical, Space.s3)
                .foregroundStyle(Palette.onAccent)
                .background(Palette.accent, in: .rect(cornerRadius: Radius.container, style: .continuous))
                .layoutPriority(1)
                .contextMenu { copyButton }
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
        } else {
            VStack(alignment: .leading, spacing: Space.s3) {
                HStack(spacing: Space.s2 + 1) {
                    BrandMark(size: 22).frame(width: 30, height: 30)
                        .background(.white, in: .rect(cornerRadius: 10, style: .continuous))
                    Text(agentName).font(Fonts.label).foregroundStyle(Palette.ink)
                }
                MarkdownText(text: visibleText)
                    .font(Fonts.body).lineSpacing(6).tint(Palette.accent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if message.text.count > 8000 && !expanded { Button("Show full message") { expanded = true }.font(Fonts.caption) }
            }
            .padding(Space.s4).frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.surface, in: .rect(cornerRadius: Radius.container, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Radius.container, style: .continuous).strokeBorder(Palette.border.opacity(0.7)))
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

// Parsing markdown inside `body` re-runs on every poll and long selectable text
// rebuilds its selection machinery on each change; both stalled the main thread
// on long streamed turns. Parse once per text value, off the main actor, and
// show the plain text until the parsed version is ready.
private struct MarkdownText: View {
    let text: String
    @State private var rendered: RenderedMarkdown?

    var body: some View {
        Text(rendered?.display(for: text) ?? AttributedString(text))
            .task(id: text) {
                let source = text
                let parsed = await Task.detached(priority: .userInitiated) {
                    MessageMarkdown.parse(source)
                }.value
                if !Task.isCancelled, let parsed {
                    rendered = RenderedMarkdown(source: source, value: parsed)
                }
            }
    }
}
