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
    @State private var scrollPosition = ScrollPosition(edge: .bottom)
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
    private var hasCurrentDetail: Bool { detail != nil && loadError == nil }
    private var showsStop: Bool { session.canStop }
    private var working: Bool { hasCurrentDetail && showsStop }
    private var controlsEnabled: Bool { hasCurrentDetail && session.canSend && !sending }

    var body: some View {
        VStack(spacing: 0) {
            transcript
            composer
        }
        .background(Palette.canvas)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { sessionChip }
            ToolbarItem(placement: .topBarTrailing) {
                Button("Latest message", systemImage: "arrow.down", action: scrollToLatest)
                    .labelStyle(.iconOnly).font(Fonts.caption)
            }
        }
        .toolbar(.hidden, for: .tabBar)
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

    private var transcript: some View {
        ScrollView {
            // The service bounds the transcript. Measuring it in one pass
            // keeps the bottom offset stable when rows above it change height.
            VStack(alignment: .leading, spacing: Space.s3) {
                Text(statusLabel).font(Fonts.caption).foregroundStyle(Palette.inkSoft)
                if let error = loadError ?? actionError ?? (session.status == "handoff_required" ? nil : detail?.error) ?? modelError {
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
                let streamingID = detail?.streamingReplyID
                ForEach(detail?.messages ?? []) { message in
                    MessageView(message: message, agentName: session.agentName, streaming: message.id == streamingID)
                }
                ForEach(detail?.approvals ?? []) { approval in approvalCard(approval) }
                if working {
                    HStack(spacing: Space.s2) {
                        ProgressView().controlSize(.mini)
                        Text("\(session.agentName) is working").font(Fonts.caption)
                    }
                    .foregroundStyle(Palette.inkSoft)
                }
            }
            .padding(.horizontal, Space.s4).padding(.vertical, Space.s6)
        }
        .scrollPosition($scrollPosition)
        .scrollDismissesKeyboard(.interactively)
        .onChange(of: detail?.messages.last?.id) {
            if !followed { scrollToLatest(); followed = true }
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
            .frame(maxWidth: 168)
            .background(Palette.surface, in: Capsule())
            .overlay(Capsule().strokeBorder(Palette.border))
        }
        .accessibilityLabel("Session details")
        .accessibilityValue(session.title)
        .accessibilityIdentifier("sessionChip")
    }

    private var statusLabel: String {
        if loadError != nil { return "Unavailable" }
        if detail == nil { return "Connecting…" }
        return session.activity
    }

    private var statusColor: Color {
        if !hasCurrentDetail { return Palette.inkSoft }
        return switch session.status {
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
                        .disabled(!controlsEnabled)
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
            .animation(.snappy(duration: 0.18), value: attachments)
            .onChange(of: attachments) { if !sending { requestId = UUID().uuidString } }
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
        .disabled(!controlsEnabled)
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
        .disabled(!controlsEnabled)
        .accessibilityLabel("Choose model")
        .accessibilityIdentifier("modelPicker")
    }

    private var currentModelLabel: String {
        models.first { $0.id == (selectedModel ?? session.model) }?.label
            ?? selectedModel ?? session.model ?? "Session model"
    }

    private var actionCircle: some View {
        Button {
            if showsStop { Task { await stopTurn() } } else { Task { await send() } }
        } label: {
            Group {
                if sending || stopping {
                    ProgressView().tint(Palette.onAccent)
                } else {
                    Image(systemName: showsStop ? "stop.fill" : "arrow.up")
                        .font(.system(size: showsStop ? 13 : 16, weight: .semibold))
                }
            }
            .frame(width: 40, height: 40)
            .foregroundStyle(actionActive ? Palette.onAccent : Palette.inkSoft)
            .background(actionActive ? Palette.accent : Palette.raised, in: Circle())
        }
        .disabled(!actionActive)
        .accessibilityLabel(showsStop ? "Stop current turn" : "Send message")
        .accessibilityIdentifier(showsStop ? "stopTurnButton" : "sendMessageButton")
    }

    private var actionActive: Bool {
        guard hasCurrentDetail else { return false }
        return showsStop ? !stopping : sendEnabled
    }

    private var sendEnabled: Bool {
        controlsEnabled && composedLength <= 32_000
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
                        .disabled(sending)
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
                    .disabled(!hasCurrentDetail)
                    row(label: "Agent", value: session.agentName)
                    row(label: "Project", value: session.cwd)
                    row(label: "Status", value: statusLabel)
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
                .disabled(!hasCurrentDetail)
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
                    .disabled(!hasCurrentDetail)
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
            .disabled(deciding || !hasCurrentDetail)
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
        guard hasCurrentDetail, !stopping else { return }
        stopping = true
        switch await store.interrupt(id: session.id, deviceId: route.deviceId, requestId: stopRequestId) {
        case .success: actionError = nil; stopRequestId = UUID().uuidString
        case .failure(let failure): actionError = failure.message
        }
        await update()
        stopping = false
    }

    private func send() async {
        guard sendEnabled else { return }
        composing = false
        sending = true
        actionError = nil
        let text = DraftAttachment.compose(draft: draft, attachments: attachments)
        switch await store.send(id: session.id, deviceId: route.deviceId, text: text, requestId: requestId, model: selectedModel) {
        case .success:
            draft = ""
            attachments = []
            requestId = UUID().uuidString
            await update()
            scrollToLatest()
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
        guard hasCurrentDetail, !deciding else { return }
        deciding = true
        switch await store.approve(id: session.id, deviceId: route.deviceId, approvalId: approval.id, decision: decision) {
        case .success: actionError = nil
        case .failure(let failure): actionError = failure.message
        }
        await update()
        deciding = false
    }

    // Scroll without an animation. An in-flight animation against a list whose
    // height is still changing retriggers layout until the process is killed.
    private func scrollToLatest() {
        var transaction = Transaction(animation: nil)
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            scrollPosition.scrollTo(edge: .bottom)
        }
    }
}

private struct MessageView: View {
    let message: SessionMessage
    let agentName: String
    let streaming: Bool
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
                Group {
                    if streaming {
                        // Keep the live row's layout fixed while its text changes.
                        Text(verbatim: message.streamingPreview)
                            .lineLimit(8, reservesSpace: true)
                            .truncationMode(.head)
                            .accessibilityIdentifier("streamingReply")
                    } else {
                        MarkdownText(text: visibleText)
                    }
                }
                .font(Fonts.body).lineSpacing(6).tint(Palette.accent)
                .frame(maxWidth: .infinity, alignment: .leading)
                if !streaming && message.text.count > 8000 && !expanded {
                    Button("Show full message") { expanded = true }.font(Fonts.caption)
                }
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
