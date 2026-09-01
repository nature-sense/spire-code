import SwiftUI
import AppKit

/// The HAL contract workflow (Stage 0 → Stage 2), driven by the deterministic
/// core tools over `tools/call`.
///
///   Propose  → an editable `hal/api/*.hpp` abstract-class header draft
///   Approve  → `halValidateContract` (the binding gate — rejects non-abstract
///              headers before anything is written)
///   Add Target → `halAddTarget` writes one placeholder per contract interface
///              under `hal/implementations/<plat>` + wires `hal/meson.build`
///              + re-analyzes (the `missing_implementation` queue surfaces)
///   Reconcile → `halDiffContracts` compares an edited draft against the last
///              approved summary, surfacing stale-implementation impact.
struct HALWorkflowView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    /// Default proposed contract (canonical shape: virtual dtor + pure-virtual
    /// public methods — required by the validating extractor).
    @State private var headerDraft: String = """
    #pragma once
    #include <cstdint>

    class CameraHAL {
    public:
        virtual ~CameraHAL() = default;
        virtual bool start() = 0;
        virtual std::uint32_t capture(int timeout_ms) = 0;
    };
    """
    @State private var summary: String?
    @State private var validationError: String?
    @State private var writtenPath: String?
    @State private var projectRoot: String = ""
    @State private var platforms: [Platform] = []
    @State private var selectedPlatform: String?
    @State private var placeholders: [String] = []
    @State private var addError: String?
    @State private var missingImpls: [String: [String]] = [:]
    @State private var queueError: String?
    @State private var diffAdded: [String] = []
    @State private var diffRemoved: [String] = []
    @State private var diffChanged: [[String]] = []
    @State private var diffError: String?
    @State private var isWorking = false

    // MARK: Semantic LLM generation (Stage-1 module pair)
    /// Platform selected for semantic generation.
    @State private var genPlatform: String?
    /// Interface (contract stem) selected for semantic generation.
    @State private var genInterface: String?
    /// Optional library/technique hints override (empty = platform default).
    @State private var genHints: String = ""
    /// Built prompt preview (from `hal_build_impl_prompt`).
    @State private var genPrompt: String?
    /// Last generation result (written module pair + gate status).
    @State private var genResult: HalGenerateImplResult?
    @State private var genError: String?
    @State private var genBusy = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                contractEditor
                validationCard
                Divider()
                addTargetSection
                missingImplsCard
                Divider()
                generateImplCard
                Divider()
                contractChangeCard
            }
            .padding(24)
        }
        .frame(width: 760, height: 860)
        .background(theme.background)
        .task {
            platforms = await bridge.fetchPlatforms()
            if let proj = bridge.projectRoot {
                projectRoot = proj
            }
            if !projectRoot.isEmpty {
                await loadMissingImpls()
            }
        }
        .onChange(of: projectRoot) { _, newRoot in
            guard !newRoot.isEmpty else { return }
            Task { await loadMissingImpls() }
        }
        .onChange(of: genPlatform) { _, _ in
            genInterface = nil
            genPrompt = nil
            genResult = nil
            genError = nil
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("HAL Contract Workflow")
                .font(.title2.weight(.semibold))
            Text("Step 1: approve a binding abstract-class contract. Step 2: add a target — one placeholder per interface, wired into the build.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    private var contractEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Contract header (hal/api/<name>.hpp)")
                .font(.headline)
            TextEditor(text: $headerDraft)
                .font(.system(.body, design: .monospaced))
                .frame(height: 170)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(theme.textBackground))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 1))

            HStack {
                TextField("Project root (optional — for Add Target)", text: $projectRoot)
                    .textFieldStyle(.roundedBorder)
                Button("Choose…") { chooseRoot() }
            }
            .frame(maxWidth: .infinity)
        }
    }

    private var validationCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Approve (binding)")
                .font(.headline)
            if let writtenPath {
                Label("Approved & written: \(writtenPath)", systemImage: "checkmark.circle.fill")
                    .font(.callout)
                    .foregroundStyle(.green)
            }
            if let summary {
                Text(summary)
                    .font(.system(.callout, design: .monospaced))
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
                    .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.green.opacity(0.6), lineWidth: 1))
            }
            if let validationError {
                Label(validationError, systemImage: "xmark.octagon.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
            }
            HStack {
                Button {
                    validate()
                } label: {
                    if isWorking { ProgressView().scaleEffect(0.7) }
                    else { Label("Validate", systemImage: "checkmark.seal") }
                }
                .buttonStyle(.borderedProminent)
                .disabled(isWorking)

                Button {
                    approveAndWrite()
                } label: {
                    if isWorking { ProgressView().scaleEffect(0.7) }
                    else { Label("Approve & Write", systemImage: "pencil.circle.fill") }
                }
                .buttonStyle(.borderedProminent)
                .tint(.green)
                .disabled(isWorking || projectRoot.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
    }

    private var missingImplsCard: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Missing implementations")
                .font(.headline)
            if let queueError {
                Label(queueError, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
            } else if missingImpls.isEmpty {
                Text("No missing implementations — every contract interface is implemented or no analysis is stored yet.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(missingImpls.sorted(by: { $0.key < $1.key }), id: \.key) { iface, platforms in
                    HStack(alignment: .top) {
                        Text(iface)
                            .font(.system(.callout, design: .monospaced))
                            .frame(width: 160, alignment: .leading)
                        Text(platforms.joined(separator: ", "))
                            .font(.callout)
                            .foregroundStyle(.orange)
                        Spacer()
                    }
                    .padding(6)
                    .background(RoundedRectangle(cornerRadius: 4).fill(theme.surface))
                }
            }
        }
    }

    /// Stage 3 (reconcile): diff the edited draft against the last approved
    /// summary so the user sees which implementations are stale (added,
    /// removed, signature-changed) before regenerating them per target.
    private var contractChangeCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Contract change (reconcile)")
                .font(.headline)
            Text("Edit the draft above, then diff it against the last approved summary to see stale-implementation impact.")
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack {
                Button {
                    diffDraft()
                } label: {
                    if isWorking { ProgressView().scaleEffect(0.7) }
                    else { Label("Diff edited draft vs approved", systemImage: "arrow.left.arrow.right") }
                }
                .buttonStyle(.borderedProminent)
                .disabled(isWorking || summary == nil)
            }

            if let diffError {
                Label(diffError, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
            }

            if !diffAdded.isEmpty {
                Text("Added (needs new impls):")
                    .font(.caption).foregroundStyle(.secondary)
                ForEach(diffAdded, id: \.self) { name in
                    Text("+ \(name)").font(.system(.callout, design: .monospaced)).foregroundStyle(.green)
                }
            }
            if !diffRemoved.isEmpty {
                Text("Removed (can drop impls):")
                    .font(.caption).foregroundStyle(.secondary)
                ForEach(diffRemoved, id: \.self) { name in
                    Text("- \(name)").font(.system(.callout, design: .monospaced)).foregroundStyle(.red)
                }
            }
            if !diffChanged.isEmpty {
                Text("Signature-changed (stale impls):")
                    .font(.caption).foregroundStyle(.secondary)
                ForEach(diffChanged, id: \.self) { pair in
                    Text(pair[1])
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.orange)
                }
            }
        }
    }

    private var addTargetSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Add a target")
                .font(.headline)
            Picker("Platform", selection: $selectedPlatform) {
                Text("Select platform…").tag(String?.none)
                ForEach(platforms, id: \.id) { p in
                    Text("\(p.name) (\(p.id))").tag(String?.some(p.id))
                }
            }
            .frame(maxWidth: 300)

            if !placeholders.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Placeholders written:")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    ForEach(placeholders, id: \.self) { p in
                        Text(p)
                            .font(.system(.caption, design: .monospaced))
                    }
                }
            }
            if let addError {
                Label(addError, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
            }

            Button {
                addTarget()
            } label: {
                if isWorking { ProgressView().scaleEffect(0.7) }
                else { Label("Generate placeholders + wire meson", systemImage: "plus.circle") }
            }
            .buttonStyle(.borderedProminent)
            .disabled(isWorking || selectedPlatform == nil || projectRoot.trimmingCharacters(in: .whitespaces).isEmpty)
        }
    }

    private var generateImplCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Generate implementation (LLM)")
                .font(.headline)
            Text("Semantic module-pair generation: a deterministic declaration header + an LLM-written .cpp with real bodies. The prompt targets the concrete derived class (e.g. `CameraHalRpi5 : CameraHAL`) — no stubs, no sentinel.")
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack {
                Picker("Platform", selection: $genPlatform) {
                    Text("Select platform…").tag(String?.none)
                    ForEach(platforms, id: \.id) { p in
                        Text("\(p.name) (\(p.id))").tag(String?.some(p.id))
                    }
                }
                .frame(maxWidth: 280)

                Picker("Interface", selection: $genInterface) {
                    Text("Select interface…").tag(String?.none)
                    ForEach(missingInterfaces(for: genPlatform), id: \.self) { iface in
                        Text(iface).tag(String?.some(iface))
                    }
                }
                .frame(maxWidth: 280)
                .disabled(genPlatform == nil)
            }

            TextField("Library hints (optional — platform default used when empty)", text: $genHints, axis: .vertical)
                .lineLimit(1...3)
                .textFieldStyle(.roundedBorder)
                .font(.system(.caption, design: .monospaced))

            HStack {
                Button {
                    buildGenPrompt()
                } label: {
                    if genBusy { ProgressView().scaleEffect(0.7) }
                    else { Label("Build prompt", systemImage: "doc.text.magnifyingglass") }
                }
                .disabled(genBusy || genPlatform == nil || genInterface == nil || projectRoot.trimmingCharacters(in: .whitespaces).isEmpty)

                Button {
                    generateImpl()
                } label: {
                    if genBusy { ProgressView().scaleEffect(0.7) }
                    else { Label("Generate", systemImage: "sparkles") }
                }
                .buttonStyle(.borderedProminent)
                .tint(.indigo)
                .disabled(genBusy || genPlatform == nil || genInterface == nil || projectRoot.trimmingCharacters(in: .whitespaces).isEmpty)
            }

            if let genError {
                Label(genError, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
            }

            if let genPrompt {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Prompt preview (what the LLM will receive)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    ScrollView {
                        Text(genPrompt)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(maxHeight: 140)
                    .padding(8)
                    .background(RoundedRectangle(cornerRadius: 6).fill(theme.textBackground))
                    .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 1))
                }
            }

            if let genResult {
                VStack(alignment: .leading, spacing: 4) {
                    Label("Module pair written", systemImage: "checkmark.circle.fill")
                        .font(.callout)
                        .foregroundStyle(.green)
                    ForEach(genResult.written, id: \.self) { path in
                        Text((path as NSString).lastPathComponent)
                            .font(.system(.caption, design: .monospaced))
                    }
                    if !genResult.removedStubs.isEmpty {
                        Text("Removed stale stubs: \(genResult.removedStubs.joined(separator: ", "))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    if genResult.syntax != "ok" {
                        Label("Syntax: \(genResult.syntax)", systemImage: "exclamationmark.octagon")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                    Label("\(genResult.gate): \(genResult.gateStatus)", systemImage: "hammer")
                        .font(.caption)
                        .foregroundStyle(genResult.gateStatus.contains("passed") ? .green : .orange)
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 0.5))
            }
        }
    }

    // MARK: - Actions

    private func validate() {
        isWorking = true
        validationError = nil
        summary = nil
        Task {
            let result = await bridge.halValidateContract(headerDraft)
            await MainActor.run {
                if result.valid {
                    summary = result.summary
                } else {
                    validationError = result.error
                }
                isWorking = false
            }
        }
    }

    private func loadMissingImpls() async {
        let (payload, err) = await bridge.halMissingImpls(root: projectRoot)
        let missing = (payload["missing"] as? [String: [String]]) ?? [:]
        await MainActor.run {
            missingImpls = missing
            queueError = err
        }
    }

    /// After a file-mutating HAL action, refresh the SHARED bridge state so the
    /// main window's panes reflect the change too (not just this sheet):
    /// `fetchProjectAnalysis` re-derives the project tree and, for HAL projects,
    /// refreshes the right pane's HAL status; `loadMissingImpls` keeps this
    /// sheet's own cards + the Generate interface picker in sync.
    private func refreshAfterHalMutation() async {
        await bridge.fetchProjectAnalysis(projectRoot: projectRoot)
        await loadMissingImpls()
    }

    /// Stage 0 approve: validate-then-persist the draft contract to
    /// <root>/hal/api/<name>.hpp (invalid header never touches disk).
    private func approveAndWrite() {
        isWorking = true
        validationError = nil
        writtenPath = nil
        summary = nil
        Task {
            let stem = contractStem()
            let result = await bridge.halWriteContract(
                root: projectRoot, filename: stem, content: headerDraft
            )
            await MainActor.run {
                if result.valid {
                    writtenPath = result.written
                    summary = result.summary
                } else {
                    validationError = result.error
                }
                isWorking = false
            }
            if result.valid {
                await refreshAfterHalMutation()
            }
        }
    }

    /// Stage 3: validate the edited draft, then diff it against the stored
    /// (last approved) summary via halDiffContracts.
    private func diffDraft() {
        guard let stored = summary else { return }
        isWorking = true
        diffError = nil
        diffAdded = []
        diffRemoved = []
        diffChanged = []
        Task {
            let val = await bridge.halValidateContract(headerDraft)
            guard let draft = val.summary else {
                await MainActor.run {
                    diffError = val.error ?? "draft invalid"
                    isWorking = false
                }
                return
            }
            let diff = await bridge.halDiffContracts(old: stored, new: draft)
            await MainActor.run {
                if let err = diff.error {
                    diffError = err
                } else {
                    diffAdded = diff.added
                    diffRemoved = diff.removed
                    diffChanged = diff.changed
                }
                isWorking = false
            }
        }
    }

    /// Derive the header filename (e.g. "camera_hal") from the draft's first
    /// `class NAME` — falls back to "camera_hal" for an empty draft.
    private func contractStem() -> String {
        let pattern = #"(?m)^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)"#
        guard let range = headerDraft.range(of: pattern, options: .regularExpression) else {
            return "camera_hal"
        }
        let line = headerDraft[range].trimmingCharacters(in: .whitespaces)
        let name = line.split(separator: " ").last.map(String.init) ?? "camera_hal"
        return name.lowercased()
    }

    private func addTarget() {
        guard let platform = selectedPlatform else { return }
        isWorking = true
        addError = nil
        placeholders = []
        Task {
            let result = await bridge.halAddTarget(root: projectRoot, platform: platform)
            await MainActor.run {
                if let err = result.error {
                    addError = err
                } else {
                    placeholders = result.placeholders
                }
                isWorking = false
            }
            if result.error == nil {
                await refreshAfterHalMutation()
            }
        }
    }

    private func chooseRoot() {
        NSApp.activate(ignoringOtherApps: true)
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose Project Root"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        projectRoot = url.path
    }

    /// Contract stems currently missing on the given platform (from the
    /// missing-impl queue) — the interfaces the semantic generator can fill.
    private func missingInterfaces(for platform: String?) -> [String] {
        guard let platform else { return [] }
        return missingImpls
            .filter { $0.value.contains(platform) }
            .map(\.key)
            .sorted()
    }

    /// Semantic generation — step 1 (read-only): build + preview the module-pair
    /// implementation prompt for the selected interface × platform.
    private func buildGenPrompt() {
        guard let platform = genPlatform, let interface = genInterface else { return }
        genBusy = true
        genError = nil
        genPrompt = nil
        Task {
            let result = await bridge.halBuildImplPrompt(
                root: projectRoot, interface: interface, platform: platform, libraryHints: genHints
            )
            await MainActor.run {
                if let result {
                    genPrompt = result.prompt
                } else {
                    genError = "Failed to build the prompt — is the contract valid and the platform registered?"
                }
                genBusy = false
            }
        }
    }

    /// Semantic generation — step 2: run the LLM, write the module pair
    /// (header + .cpp), wire meson, and refresh the missing-impl queue.
    private func generateImpl() {
        guard let platform = genPlatform, let interface = genInterface else { return }
        genBusy = true
        genError = nil
        genResult = nil
        genPrompt = nil
        Task {
            let (result, err) = await bridge.halGenerateImpl(
                root: projectRoot, interface: interface, platform: platform, libraryHints: genHints
            )
            await MainActor.run {
                if let err {
                    genError = err
                } else if let result {
                    genResult = result
                } else {
                    genError = "Generation returned no result"
                }
                genBusy = false
            }
            if result != nil {
                await refreshAfterHalMutation()
            }
        }
    }
}