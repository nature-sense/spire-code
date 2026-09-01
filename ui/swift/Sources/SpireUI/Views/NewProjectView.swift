import SwiftUI
import AppKit

/// Step-wise "Start New Project" wizard (embedded-first).
///
/// Flow:
///   1. Environment   → Embedded | Native
///   2. Targets       → multi-select platforms (≥1 for embedded, NO host)
///   3. Toolchain     → C++ / Meson | Rust / Cargo
///   4. Structure     → Meson: Hardware abstraction (recommended) | Single source base
///                      Cargo/Rust: fixed "multi-target build" label
///   5. Details       → name + parent dir + description (collected ONCE on the
///                      final step — description only drives plan *content*,
///                      structure is already fixed by steps 1–4)
///   Generate Plan    → bridge.generateProjectPlan(…)
///
/// The Rust core's `ProjectStructure` enum (`native | single_source | hal`)
/// and `embedded` flag ride through `generateProjectPlan`, so the scaffold
/// (Meson HAL container vs single-source; no-host Cargo) matches the choice.
struct NewProjectView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    enum Step: Int, CaseIterable {
        case environment = 0
        case targets = 1
        case toolchain = 2
        case structure = 3
        case details = 4
    }

    @State private var step: Step = .environment
    @State private var isEmbedded: Bool = true
    /// Loaded from the Rust registry via `bridge.fetchPlatforms()`.
    @State private var availableTargets: [Platform] = []
    /// Selected cross-compilation target registry ids (e.g. ["rpi5", "rock3c"]).
    @State private var selectedTargets: Set<String> = []
    @State private var isRust: Bool = false           // false = C++ / Meson
    @State private var useHal: Bool = true            // Meson structure: HAL vs single-source

    @State private var goal: String = ""
    @State private var projectName: String = ""
    @State private var projectDirectory: String = ""
    @State private var isGenerating = false
    @State private var errorMessage: String?

    private var isNative: Bool { !isEmbedded }

    // MARK: - Derived wizard state

    /// Resolve the final project directory (same semantics as the legacy form):
    /// empty → derive from name; leaf == name → use it; else append name.
    private var resolvedProjectDirectory: String {
        if projectDirectory.isEmpty {
            return ""
        }
        let dir = projectDirectory.hasSuffix("/") ? String(projectDirectory.dropLast()) : projectDirectory
        let leaf = (dir as NSString).lastPathComponent
        let cleanName = projectName.trimmingCharacters(in: .whitespacesAndNewlines)
        if cleanName.isEmpty {
            return dir
        }
        if leaf == cleanName {
            return dir
        }
        return "\(dir)/\(cleanName)"
    }

    private var toolchainLabel: String {
        isRust ? "Rust / Cargo" : "C++ / Meson"
    }

    private var structureLabel: String {
        if isRust { return "Multi-target build (one source set, cross-compiled per target)" }
        return useHal ? "Hardware abstraction (recommended)" : "Single source base (no hardware-specific layer)"
    }

    private var structureKey: String {
        if isRust { return "native" }
        return useHal ? "hal" : "single_source"
    }

    /// Next-step gating: each step must satisfy its requirement before the
    /// user can move on (embedded requires ≥1 target).
    private var canAdvance: Bool {
        switch step {
        case .environment: return true
        case .targets:
            return isNative || !selectedTargets.isEmpty
        case .toolchain: return true
        case .structure: return true
        case .details:
            return !goal.trimmingCharacters(in: .whitespaces).isEmpty
                && !projectName.trimmingCharacters(in: .whitespaces).isEmpty
                && !projectDirectory.isEmpty
        }
    }

    private var stepTitle: String {
        switch step {
        case .environment: return "Environment"
        case .targets: return "Target Hardware"
        case .toolchain: return "Toolchain"
        case .structure: return "Project Structure"
        case .details: return "Name & Description"
        }
    }

    // MARK: - Body

    var body: some View {
        VStack(spacing: 20) {
            header
            stepIndicator
            stepContent
            footer
        }
        .padding(32)
        .frame(width: 680)
        .background(theme.background)
        .task {
            // Preload the cross-compilation targets from the Rust registry
            // (rpi5, rock3c, …) so the embedded target list is populated.
            if let root = bridge.projectRoot, !root.isEmpty {
                projectDirectory = root
                if projectName.isEmpty {
                    projectName = (root as NSString).lastPathComponent
                }
            }
            let platforms = await bridge.fetchPlatforms()
            availableTargets = platforms
        }
    }

    private var header: some View {
        VStack(spacing: 6) {
            Image(systemName: "hammer.badge.plus")
                .font(.system(size: 44))
                .foregroundStyle(.orange)
            Text("Start New Project")
                .font(.title.weight(.semibold))
            Text("Choose a structure — Spire will scaffold it and generate a plan")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
    }

    private var stepIndicator: some View {
        HStack(spacing: 8) {
            ForEach(Step.allCases, id: \.self) { s in
                Circle()
                    .fill(s.rawValue <= step.rawValue ? Color.orange : theme.border)
                    .frame(width: 10, height: 10)
                    .overlay(Circle().stroke(theme.border, lineWidth: 1))
            }
            Spacer()
            Text("\(step.rawValue + 1) / \(Step.allCases.count)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: 420)
    }

    @ViewBuilder
    private var stepContent: some View {
        switch step {
        case .environment: environmentStep
        case .targets: targetsStep
        case .toolchain: toolchainStep
        case .structure: structureStep
        case .details: detailsStep
        }
    }

    // MARK: Step 1 — Environment

    private var environmentStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("What kind of project?")
                .font(.headline)
            HStack(spacing: 12) {
                choiceCard(
                    title: "Embedded",
                    subtitle: "Cross-compile for specific hardware targets",
                    systemImage: "cpu",
                    selected: isEmbedded,
                    select: { isEmbedded = true }
                )
                choiceCard(
                    title: "Native",
                    subtitle: "Build for this machine (host)",
                    systemImage: "macbook",
                    selected: isNative,
                    select: { isEmbedded = false }
                )
            }
            if isEmbedded {
                Label("Targets only — no host build option.", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: 480, alignment: .leading)
    }

    // MARK: Step 2 — Targets (embedded only)

    private var targetsStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            if isEmbedded {
                Text("Select at least one hardware target (no host option)")
                    .font(.headline)
                if availableTargets.isEmpty {
                    Label("No targets in the registry yet (check ~/.spire/platforms).",
                          systemImage: "tray")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else {
                    let columns = [GridItem(.adaptive(minimum: 220), spacing: 10)]
                    ScrollView {
                        LazyVGrid(columns: columns, alignment: .leading, spacing: 10) {
                            ForEach(availableTargets, id: \.id) { platform in
                                let isSelected = selectedTargets.contains(platform.id)
                                Button {
                                    if isSelected {
                                        selectedTargets.remove(platform.id)
                                    } else {
                                        selectedTargets.insert(platform.id)
                                    }
                                } label: {
                                    HStack(spacing: 8) {
                                        Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                                            .foregroundStyle(isSelected ? Color.orange : .secondary)
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(platform.name)
                                                .font(.callout.weight(.medium))
                                                .foregroundStyle(.primary)
                                            Text(platform.id)
                                                .font(.caption)
                                                .foregroundStyle(.secondary)
                                        }
                                        Spacer()
                                    }
                                    .padding(8)
                                    .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
                                    .overlay(
                                        RoundedRectangle(cornerRadius: 6)
                                            .stroke(isSelected ? Color.orange : theme.border, lineWidth: 1)
                                    )
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .frame(maxHeight: 220)
                }
            } else {
                // Native path — the target list is implicit (host only).
                Label("Native project — one host build target.", systemImage: "macbook")
                    .font(.headline)
            }
        }
        .frame(maxWidth: 520, alignment: .leading)
    }

    // MARK: Step 3 — Toolchain

    private var toolchainStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Choose your toolchain")
                .font(.headline)
            HStack(spacing: 12) {
                choiceCard(
                    title: "C++ / Meson",
                    subtitle: isEmbedded
                        ? "Per-target executables with shared + HAL sources"
                        : "Meson build system",
                    systemImage: "hammer",
                    selected: !isRust,
                    select: { isRust = false }
                )
                choiceCard(
                    title: "Rust / Cargo",
                    subtitle: "One source set, cross-compiled per target",
                    systemImage: "gear",
                    selected: isRust,
                    select: { isRust = true }
                )
            }
        }
        .frame(maxWidth: 480, alignment: .leading)
    }

    // MARK: Step 4 — Structure

    private var structureStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Project structure")
                .font(.headline)
            if isRust {
                Label("Cargo uses a single source set, cross-compiled for every selected target (via .cargo/config.toml).", systemImage: "gearshape")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                HStack(spacing: 12) {
                    choiceCard(
                        title: "Hardware abstraction",
                        subtitle: "Common core + hal/api contract + per-target implementations",
                        systemImage: "cpu",
                        selected: useHal,
                        select: { useHal = true }
                    )
                    choiceCard(
                        title: "Single source base",
                        subtitle: "Portable — no hardware-specific layer",
                        systemImage: "doc.text",
                        selected: !useHal,
                        select: { useHal = false }
                    )
                }
                if useHal {
                    Label("Recommended default for most embedded projects.", systemImage: "checkmark.seal")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(maxWidth: 520, alignment: .leading)
    }

    // MARK: Step 5 — Details

    private var detailsStep: some View {
        VStack(alignment: .leading, spacing: 14) {
            // Project goal — multi-line TextField (macOS).
            VStack(alignment: .leading, spacing: 6) {
                Text("Project description / goal")
                    .font(.headline)
                TextField(
                    "e.g. An AI camera trap server with NPU inference, MJPEG streaming and WiFi provisioning.",
                    text: $goal,
                    axis: .vertical
                )
                .textFieldStyle(.plain)
                .font(.body)
                .lineLimit(4...8)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(theme.textBackground))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 1))
            }

            // Project name.
            VStack(alignment: .leading, spacing: 6) {
                Text("Project name")
                    .font(.headline)
                TextField("e.g. ai-trap-embedded", text: $projectName)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 340)
                    .onSubmit {
                        if projectName.isEmpty { projectName = "my-project" }
                    }
            }

            // Parent directory.
            VStack(alignment: .leading, spacing: 6) {
                Text("Parent directory")
                    .font(.headline)
                HStack {
                    Text(projectDirectory.isEmpty ? "Choose parent directory..." : projectDirectory)
                        .font(.callout)
                        .foregroundStyle(projectDirectory.isEmpty ? .secondary : .primary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Button("Choose…") {
                        chooseDirectory()
                    }
                }
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 1))
                .frame(maxWidth: 340)
            }

            if !resolvedProjectDirectory.isEmpty {
                Label("Will create in: \(resolvedProjectDirectory)", systemImage: "folder.badge.plus")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            if let err = errorMessage {
                Label(err, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            // Structure summary so the user sees what will be scaffolded.
            VStack(alignment: .leading, spacing: 4) {
                Text(structureLabel)
                    .font(.callout.weight(.medium))
                if !isNative {
                    Text("Targets: \(selectedTargets.isEmpty ? "host" : selectedTargets.sorted().joined(separator: ", "))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Text("Toolchain: \(toolchainLabel)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(theme.border, lineWidth: 1))
        }
        .frame(maxWidth: 520, alignment: .leading)
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 12) {
            if step != .environment {
                Button("Back") {
                    step = Step(rawValue: step.rawValue - 1) ?? .environment
                    errorMessage = nil
                }
            }
            Spacer()
            if step != .details {
                Button("Next") {
                    if canAdvance {
                        step = Step(rawValue: step.rawValue + 1) ?? .details
                        errorMessage = nil
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canAdvance)
            } else {
                Button {
                    generatePlan()
                } label: {
                    if isGenerating {
                        ProgressView().scaleEffect(0.8)
                            .frame(width: 180)
                    } else {
                        Text("Generate Plan")
                            .font(.headline)
                            .padding(.horizontal, 24)
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(
                    goal.trimmingCharacters(in: .whitespaces).isEmpty
                    || projectName.trimmingCharacters(in: .whitespaces).isEmpty
                    || projectDirectory.isEmpty
                    || isGenerating
                )
            }
        }
        .frame(maxWidth: 520)
    }

    // MARK: - Helpers

    private func choiceCard(title: String, subtitle: String, systemImage: String,
                            selected: Bool, select: @escaping () -> Void) -> some View {
        Button(action: select) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 8) {
                    Image(systemName: systemImage)
                        .font(.system(size: 20))
                        .foregroundStyle(selected ? Color.orange : .secondary)
                    Text(title)
                        .font(.callout.weight(.semibold))
                        .foregroundStyle(.primary)
                }
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.leading)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(selected ? Color.orange : theme.border, lineWidth: 1.5)
            )
        }
        .buttonStyle(.plain)
    }

    private func chooseDirectory() {
        NSApp.activate(ignoringOtherApps: true)

        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Choose Parent Directory"
        panel.message = "Select where to create your new project"

        let suggestedName: String?
        if !projectName.isEmpty {
            suggestedName = projectName
        } else {
            let words = goal.split(separator: " ").prefix(3).map(String.init)
            if !words.isEmpty {
                suggestedName = words.joined(separator: "-").lowercased()
                    .replacingOccurrences(of: "[^a-z0-9-]", with: "-", options: .regularExpression)
                    .replacingOccurrences(of: "--+", with: "-", options: .regularExpression)
            } else {
                suggestedName = nil
            }
        }
        panel.nameFieldLabel = "Project folder:"
        if suggestedName != nil {
            panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser
        }

        if panel.runModal() == .OK, let url = panel.url {
            projectDirectory = url.path
            let leaf = url.lastPathComponent
            if projectName.isEmpty || suggestedName != leaf {
                projectName = leaf
            }
        }
    }

    private func generatePlan() {
        isGenerating = true
        errorMessage = nil
        Task {
            let language = isRust ? "Rust" : "Meson"
            let plan = await bridge.generateProjectPlan(
                goal: goal,
                rootDir: resolvedProjectDirectory,
                language: language,
                platforms: isNative ? [] : selectedTargets.sorted(),
                structure: structureKey,
                embedded: isEmbedded
            )
            await MainActor.run {
                if let plan {
                    bridge.state = .creating(plan: plan, executing: false)
                    bridge.currentMode = .project
                } else {
                    errorMessage = "Failed to generate plan. Check the core connection."
                    isGenerating = false
                }
            }
        }
    }
}