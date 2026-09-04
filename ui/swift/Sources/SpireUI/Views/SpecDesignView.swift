import SwiftUI
import AppKit

/// One line in the free-form design conversation.
struct SpecDesignLine: Identifiable {
    let id = UUID()
    let role: String   // "user" | "assistant" | "system"
    let text: String
}

/// The interactive AppSpec design step: a free-form brainstorm on the left
/// (user turns + LLM replies), the running summary and the spec document on
/// the right. Everything is changed through prompts; **Decide** is the button
/// that freezes the spec and derives the AppSpec deterministically (reversible
/// via Back to design).
struct SpecDesignView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(\.dismiss) private var dismiss

    let projectName: String
    let goal: String
    /// Called with the decided AppSpec dictionary (feeds the wizard's codegen).
    let onDecided: ([String: Any]) -> Void

    @State private var lines: [SpecDesignLine] = []
    @State private var draft = ""
    @State private var instruction = "add to the summary"
    @State private var summary: SpecDesignArtifact?
    @State private var spec: SpecDesignArtifact?
    @State private var isDecided = false
    @State private var acceptedCount = 0
    @State private var busy = false
    @State private var errorMessage: String?
    /// Request grounding for the next brainstorm question.
    @State private var useDocsRAG = false
    @State private var useWebSearch = false

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            HSplitView {
                chatPane
                documentPane
            }
        }
        .frame(minWidth: 900, minHeight: 560)
        .task {
            let (state, error) = await bridge.specDesignStart(projectName: projectName, goal: goal)
            apply(state: state, error: error)
            if state != nil {
                lines.append(SpecDesignLine(
                    role: "system",
                    text: "Free-form design session for '\(projectName)'. Bounce ideas and ask questions; press Summarize to fold the conversation into the running summary, Promote to draft the spec.md, Decide when it meets the requirements."
                ))
            }
        }
    }

    // MARK: - Header (mode + Decide / Back to design)

    private var header: some View {
        HStack(spacing: 12) {
            Label("Design AppSpec", systemImage: "bubble.left.and.bubble.right")
                .font(.title3.weight(.semibold))
            Text(projectName)
                .font(.callout)
                .foregroundStyle(.secondary)
            Spacer()
            if isDecided {
                Label("Decided", systemImage: "checkmark.seal.fill")
                    .font(.caption)
                    .foregroundStyle(.green)
                if acceptedCount > 1 {
                    Text("v\(acceptedCount)").font(.caption).foregroundStyle(.secondary)
                }
                Button("Back to design") {
                    Task { await reopen() }
                }
                .buttonStyle(.bordered)
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
                Button {
                    Task { await decide() }
                } label: {
                    if busy {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("Decide", systemImage: "checkmark.seal")
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy || spec == nil)
                .help("Freeze the spec and derive the AppSpec deterministically")
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
                Text(line.text)
                    .font(.callout)
                    .padding(8)
                    .background(RoundedRectangle(cornerRadius: 10).fill(.blue.opacity(0.15)))
            }
        case "assistant":
            HStack {
                Text(line.text)
                    .font(.callout)
                    .padding(8)
                    .background(RoundedRectangle(cornerRadius: 10).fill(.quaternary))
                Spacer(minLength: 40)
            }
        default:
            Text(line.text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .italic()
        }
    }

    private var inputBar: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                TextField("Ask a question or bounce an idea…", text: $draft, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(1...4)
                    .onSubmit {
                        Task { await send() }
                    }
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
    }

    // MARK: - Document pane (summary + spec)

    private var documentPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                TextField("Instruction (e.g. \"summarize with techniques X\", \"recreate…\")", text: $instruction)
                    .textFieldStyle(.roundedBorder)
                Button {
                    Task { await summarize() }
                } label: {
                    Label("Summarize", systemImage: "text.badge.checkmark")
                }
                .buttonStyle(.bordered)
                .disabled(busy || isDecided)
            }
            HStack(spacing: 8) {
                TextField("Instruction", text: $instruction)
                    .textFieldStyle(.roundedBorder)
                Button {
                    Task { await promote() }
                } label: {
                    Label("Promote to spec", systemImage: "doc.badge.gearshape")
                }
                .buttonStyle(.bordered)
                .disabled(busy || isDecided || summary == nil)
            }
            if let errorMessage {
                Text(errorMessage)
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            Divider()
            artifactCard(title: "Summary", artifact: summary, systemImage: "text.alignleft")
            artifactCard(title: "Spec", artifact: spec, systemImage: "doc.plaintext")
        }
        .padding(12)
        .frame(minWidth: 440)
    }

    private func artifactCard(title: String, artifact: SpecDesignArtifact?, systemImage: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Label(title, systemImage: systemImage).font(.headline)
                Spacer()
                if let artifact {
                    Text("v\(artifact.version)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            ScrollView {
                Text(artifact?.content ?? "Not drafted yet — chat first, then Summarize / Promote.")
                    .font(.system(.caption, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
            .frame(maxHeight: .infinity)
            .padding(6)
            .background(RoundedRectangle(cornerRadius: 6).fill(.quaternary.opacity(0.4)))
        }
    }

    // MARK: - Actions

    @MainActor
    private func apply(state: SpecDesignState?, error: String?) {
        if let state {
            summary = state.summary
            spec = state.spec
            isDecided = state.isDecided
            acceptedCount = state.acceptedCount
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
        if let answer, !answer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            lines.append(SpecDesignLine(role: "assistant", text: answer))
        } else if error == nil {
            lines.append(SpecDesignLine(
                role: "system",
                text: "No assistant reply (LLM unavailable?) — Summarize/Promote still work on what you have said."
            ))
        }
    }

    @MainActor
    private func summarize() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
        let (artifact, error) = await bridge.specDesignSummarize(projectName: projectName, instruction: instruction)
        if let artifact {
            summary = artifact
        }
        if let error {
            errorMessage = error
        }
    }

    @MainActor
    private func promote() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
        let (artifact, error) = await bridge.specDesignPromote(projectName: projectName, instruction: instruction)
        if let artifact {
            spec = artifact
        }
        if let error {
            errorMessage = error
        }
    }

    @MainActor
    private func decide() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
        let (specDict, error) = await bridge.specDesignDecide(projectName: projectName)
        if let specDict {
            onDecided(specDict)
            dismiss()
        } else if let error {
            errorMessage = error
        } else {
            errorMessage = "Decide returned no spec"
        }
    }

    @MainActor
    private func resetDesign() async {
        errorMessage = nil
        busy = true
        defer { busy = false }
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
        .frame(width: 1100, height: 700)
}
