import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// One line in the free-form design conversation.
struct SpecDesignLine: Identifiable {
    let id = UUID()
    let role: String   // "user" | "assistant" | "system"
    let text: String
}

/// Opens the AppSpec design session as a large, resizable floating window —
/// the brainstorm is text-heavy and a sheet would be capped by the main
/// window. Mirrors the RagPortal / TerminalPortal pattern.
enum SpecDesignPortal {
    private static var windows: [NSWindow] = []
    @MainActor static func open(bridge: SpireBridge, theme: AppTheme, projectName: String, goal: String = "") {
        let size = NSSize(width: 1240, height: 820)
        let w = NSWindow(contentRect: NSRect(origin: .zero, size: size),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "Design AppSpec — \(projectName)"
        w.isReleasedWhenClosed = false
        let view = SpecDesignView(
            projectName: projectName,
            goal: goal,
            onClose: { [weak w] in w?.close() },
            onDecided: { spec in
                Task { await bridge.runSpecDesignCodegen(spec: spec) }
            }
        )
        .environment(bridge)
        .environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(size)
        if let m = NSApp.mainWindow?.frame {
            w.setFrameOrigin(NSPoint(x: m.midX - size.width / 2, y: m.midY - size.height / 2))
        } else {
            w.center()
        }
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// The AppSpec design step as a single conversation. The assistant proposes a
/// recommended design, asks questions only when a choice genuinely matters, and
/// — once the design is complete — calls the `submit_appspec` tool itself. The
/// view then runs the wizard code generation automatically. No separate
/// Summarize/Promote/Decide steps: everything happens through the chat.
struct SpecDesignView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    let projectName: String
    let goal: String
    /// Called when the session window should close (used by the portal).
    var onClose: () -> Void = {}
    /// Called with the submitted AppSpec dictionary (feeds the wizard's codegen).
    let onDecided: ([String: Any]) -> Void

    @State private var lines: [SpecDesignLine] = []
    @State private var draft = ""
    @State private var isDecided = false
    @State private var acceptedCount = 0
    @State private var busy = false
    @State private var errorMessage: String?
    /// Questions the assistant still needs answered (blocks submission).
    @State private var outline: String?
    @State private var openQuestions: [DesignQuestion] = []
    /// acceptedCount already handed to the code generator (fires once per submit).
    @State private var codegenVersion = 0
    /// Request grounding for the next brainstorm question.
    @State private var useDocsRAG = false
    @State private var useWebSearch = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            HSplitView {
                chatPane
                statusPane
            }
        }
        .frame(minWidth: 1000, minHeight: 700)
        .task {
            let (state, error) = await bridge.specDesignStart(projectName: projectName, goal: goal)
            apply(state: state, error: error)
            if state != nil {
                lines.append(SpecDesignLine(
                    role: "system",
                    text: "Free-form design session for '\(projectName)'. Ask questions, bounce ideas, adjust the plan — when the design is complete the assistant submits the AppSpec itself and code generation starts automatically."
                ))
            }
        }
    }

    // MARK: - Header (mode + Done / Back to design)

    private var header: some View {
        HStack(spacing: 12) {
            Label("Design AppSpec", systemImage: "bubble.left.and.bubble.right")
                .font(.title3.weight(.semibold))
            Text(projectName)
                .font(.callout)
                .foregroundStyle(.secondary)
            Spacer()
            if isDecided {
                Label("AppSpec submitted", systemImage: "checkmark.seal.fill")
                    .font(.caption)
                    .foregroundStyle(.green)
                Text("v\(acceptedCount)").font(.caption).foregroundStyle(.secondary)
                Button("Revise") {
                    Task { await reopen() }
                }
                .buttonStyle(.bordered)
                .disabled(busy)
                .help("Reopen the design: edit in the chat and the assistant submits a new version")
                Button("Done") {
                    onClose()
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)
            } else {
                Label("Free-form", systemImage: "pencil.and.outline")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Start over") {
                    Task { await resetDesign() }
                }
                .buttonStyle(.bordered)
                .disabled(busy)
                .help("Discard the persisted session and design from scratch")
            }
        }
        .padding(12)
    }

    // MARK: - Chat pane

    private var chatPane: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 10) {
                        ForEach(lines) { line in
                            bubble(for: line)
                                .id(line.id)
                        }
                    }
                    .padding(12)
                }
                .onChange(of: lines.count) { _, _ in
                    if let last = lines.last {
                        withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
            Divider()
            inputBar
        }
        .frame(minWidth: 380)
    }

    @ViewBuilder
    private func bubble(for line: SpecDesignLine) -> some View {
        switch line.role {
        case "user":
            HStack {
                Spacer(minLength: 60)
                VStack(alignment: .trailing, spacing: 4) {
                    MarkdownText(line.text)
                        .padding(8)
                        .background(RoundedRectangle(cornerRadius: 10).fill(.blue.opacity(0.15)))
                    messageActions(line)
                }
            }
        case "assistant":
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    MarkdownText(line.text)
                        .padding(8)
                        .background(RoundedRectangle(cornerRadius: 10).fill(.quaternary))
                    messageActions(line)
                }
                Spacer(minLength: 40)
            }
        default:
            Text(line.text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .italic()
        }
    }

    /// Copy + Export actions under a message bubble, mirroring the helpers the
    /// old Summary/Spec artifact cards offered.
    private func messageActions(_ line: SpecDesignLine) -> some View {
        HStack(spacing: 10) {
            Button {
                copyToClipboard(line.text)
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
                    .font(.caption2.weight(.medium))
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("Copy this message's text to the clipboard")
            Button {
                exportMessage(line)
            } label: {
                Label("Export…", systemImage: "square.and.arrow.up")
                    .font(.caption2.weight(.medium))
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("Save this message's text to a .md file")
        }
    }

    /// Put a message's raw text on the clipboard (pasting keeps the markdown
    /// structure).
    private func copyToClipboard(_ content: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(content, forType: .string)
    }

    /// Save a message to a .md file the user chooses.
    @MainActor
    private func exportMessage(_ line: SpecDesignLine) {
        let panel = NSSavePanel()
        panel.title = "Export message"
        panel.nameFieldStringValue = "\(projectName)-\(line.role)-\(Int(Date().timeIntervalSince1970)).md"
        panel.canCreateDirectories = true
        panel.allowedContentTypes = [UTType(filenameExtension: "md") ?? .plainText]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            try line.text.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            errorMessage = "Export failed: \(error.localizedDescription)"
        }
    }

    private var inputBar: some View {
        VStack(spacing: 0) {
            HStack(alignment: .bottom, spacing: 8) {
                // Large multi-line prompt: Return inserts a newline and long
                // input scrolls inside the box (never truncates or submits).
                TextEditor(text: $draft)
                    .font(.callout)
                    .scrollContentBackground(.hidden)
                    .textSelection(.enabled)
                    .padding(5)
                    .frame(height: 130)
                    .background(RoundedRectangle(cornerRadius: 8).fill(theme.nodeBackground))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
                    .overlay(alignment: .topLeading) {
                        if draft.isEmpty {
                            Text("Ask a question or bounce an idea…")
                                .font(.callout)
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 12)
                                .allowsHitTesting(false)
                        }
                    }
                    .disabled(busy || isDecided)
                Button {
                    Task { await send() }
                } label: {
                    Label("Send", systemImage: "arrow.up.circle.fill")
                }
                .buttonStyle(.borderedProminent)
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || busy || isDecided)
            }
            .padding(10)
            HStack(spacing: 10) {
                Toggle("Spire docs", isOn: $useDocsRAG)
                    .toggleStyle(.checkbox)
                    .help("Ground the answer in the spire-actor/spire-core docs corpus")
                Toggle("Web search", isOn: $useWebSearch)
                    .toggleStyle(.checkbox)
                    .help("Ground the answer in a web search (Tavily, Wikipedia fallback)")
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.bottom, 6)
        }
        // ⌘Return sends (plain Return inserts a newline inside the editor).
        .background(Button("sendTurn") {
            Task { await send() }
        }
        .keyboardShortcut(.return, modifiers: [.command])
        .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || busy || isDecided)
        .frame(width: 0, height: 0)
        .opacity(0))
    }

    // MARK: - Status pane

    private var statusPane: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Session", systemImage: "info.circle")
                .font(.headline)
            if isDecided {
                VStack(alignment: .leading, spacing: 4) {
                    Label("AppSpec submitted", systemImage: "checkmark.seal.fill")
                        .font(.headline)
                        .foregroundStyle(.green)
                    Text("v\(acceptedCount) — code generation started automatically.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text("Press Revise (top right) to adjust the design in the chat and submit a new version.")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(.green.opacity(0.08)))
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            if let outline, !outline.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Label("Design outline", systemImage: "square.stack.3d.up")
                        .font(.headline)
                    ScrollView {
                        MarkdownText(outline)
                            .textSelection(.enabled)
                    }
                    .frame(maxHeight: 240)
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(.quaternary.opacity(0.4)))
            }
            if !openQuestions.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 6) {
                        Label("Open questions", systemImage: "questionmark.circle")
                            .font(.headline)
                            .foregroundStyle(.orange)
                        Text("\(openQuestions.count)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    ScrollView {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(Array(openQuestions.enumerated()), id: \.offset) { idx, q in
                                questionCard(index: idx, question: q)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    .frame(maxHeight: 240)
                    Text("Tap a recommended answer (or an option) to reply — it is sent as your message.")
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 8).fill(.orange.opacity(0.06)))
            }
            Divider()
            VStack(alignment: .leading, spacing: 6) {
                Text("How this works")
                    .font(.subheadline.weight(.semibold))
                Text("The assistant proposes one recommended design — types, graph, actors, bridge and UI — and asks only when a choice really matters.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Text("When the design is complete it calls the submit_appspec tool itself; the spec is validated and stored, and code generation starts automatically.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Text("Tick \(Image(systemName: "square.on.square")) Spire docs to ground answers in the spire-actor/spire-core docs corpus.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(minWidth: 300, maxWidth: 380)
    }

    /// One open question with its recommended answer + alternatives; tapping an
    /// answer auto-sends it as the user's next message (existing ask path).
    private func questionCard(index: Int, question: DesignQuestion) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 6) {
                Text("\(index + 1).")
                    .font(.caption.weight(.semibold))
                    .monospacedDigit()
                if !question.section.isEmpty {
                    Text(question.section)
                        .font(.caption2.weight(.medium))
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(Capsule().fill(.blue.opacity(0.15)))
                }
                Text(question.question)
                    .font(.caption.weight(.semibold))
                    .textSelection(.enabled)
            }
            if !question.recommendation.isEmpty {
                Button {
                    acceptAnswer(question, choice: question.recommendation)
                } label: {
                    Label("Use recommendation: \(question.recommendation)", systemImage: "checkmark.circle")
                        .font(.caption2.weight(.medium))
                        .multilineTextAlignment(.leading)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(busy || isDecided)
            }
            if !question.options.isEmpty {
                HStack(spacing: 6) {
                    ForEach(question.options, id: \.self) { opt in
                        Button {
                            acceptAnswer(question, choice: opt)
                        } label: {
                            Text(opt)
                                .font(.caption2)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .disabled(busy || isDecided)
                    }
                }
            }
        }
        .padding(6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 6).fill(.quaternary.opacity(0.35)))
    }

    // MARK: - Actions

    @MainActor
    private func apply(state: SpecDesignState?, error: String?) {
        if let state {
            isDecided = state.isDecided
            acceptedCount = state.acceptedCount
            outline = state.outline
            openQuestions = state.openQuestions
        }
        if let error {
            errorMessage = error
        }
    }

    @MainActor
    private func send() async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        draft = ""
        errorMessage = nil
        lines.append(SpecDesignLine(role: "user", text: text))
        busy = true
        defer { busy = false }
        let (answer, state, error) = await bridge.specDesignAsk(projectName: projectName, text: text, docs: useDocsRAG, web: useWebSearch)
        useDocsRAG = false
        useWebSearch = false
        apply(state: state, error: error)
        if let state, state.isDecided, let latest = state.latest, state.acceptedCount > codegenVersion {
            // The assistant just submitted the AppSpec: feed it to the codegen
            // wizard exactly once and surface the new state in the chat.
            codegenVersion = state.acceptedCount
            onDecided(latest)
            lines.append(SpecDesignLine(
                role: "system",
                text: "AppSpec v\(state.acceptedCount) submitted — code generation started. The spec has been persisted for the project."
            ))
        }
        if let answer, !answer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            lines.append(SpecDesignLine(role: "assistant", text: answer))
        } else if error == nil {
            lines.append(SpecDesignLine(
                role: "system",
                text: "No assistant reply (LLM unavailable?) — try again."
            ))
        }
    }

    /// Answer an open question by sending the chosen answer as the next chat
    /// turn (auto-send through the normal ask path, no grounding requested).
    @MainActor
    private func acceptAnswer(_ question: DesignQuestion, choice: String) {
        errorMessage = nil
        useDocsRAG = false
        useWebSearch = false
        draft = "For \"\(question.question)\", I'll go with: \(choice)"
        Task { await send() }
    }

    @MainActor
    private func resetDesign() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
        codegenVersion = 0
        let (state, error) = await bridge.specDesignStart(projectName: projectName, goal: goal, reset: true)
        apply(state: state, error: error)
        if state != nil {
            lines = []
        }
    }

    @MainActor
    private func reopen() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
        let (state, error) = await bridge.specDesignReopen(projectName: projectName)
        apply(state: state, error: error)
    }
}

#Preview {
    SpecDesignView(projectName: "spire-gis", goal: "view and edit map layers", onDecided: { _ in })
        .environment(SpireBridge.shared)
        .environment(AppTheme())
        .frame(width: 1100, height: 700)
}
