import SwiftUI
import AppKit

/// Single-form new-project wizard:
///
/// "Describe the project" — the user enters the goal (and the structure
/// controls: build system, name, platforms, parent directory), then presses
/// **Plan**. The Rust core computes the structural contract IN MEMORY (nothing
/// written to disk) and asks the LLM for an implementation plan inside it,
/// returning both.
///
/// The review step shows ONLY the plan (the LLM's steps) for approval:
///   • OK     → `createProject/Scaffold` (materializes the spec + git
///              baseline) → `createProject/ExecutePlan` step-by-step →
///              `openProject` (analyze + update the dashboard).
///   • Reject → clears the goal input; nothing was written, so the form stays
///              active and the user can describe a different project.
struct ProjectWizardView: View {
    @Environment(SpireBridge.self) private var bridge
    let root: String

    // Structure controls
    @State private var buildSystem = "Cargo"
    @State private var projectName = ""
    @State private var selectedPlatforms: Set<String> = []
    @State private var availablePlatforms: [Platform] = []
    @State private var structure = "native"   // native | spire_app (Cargo), single_source | hal (Meson)

    // Describe / plan
    @State private var goal = ""
    @State private var isPlanning = false
    @State private var planResult: PlanScaffoldResult?
    @State private var errorMessage: String?

    // Execute
    @State private var isExecuting = false
    @State private var executedSteps: [String: StepExecutionResult] = [:]

    // AppSpec requirements pass (SpireApp only, optional)
    @State private var appSpec: [String: Any]?
    @State private var isDerivingSpec = false
    @State private var specError: String?

    private let buildSystems = ["Cargo", "Meson"]

    /// The wizard chose the Spire app structure.
    private var isSpireApp: Bool {
        structure == "spire_app"
    }

    /// Structure options shown for the selected build system.
    private var structureOptions: [(key: String, title: String, subtitle: String)] {
        if buildSystem == "Cargo" {
            return [
                ("native", "Plain Cargo crate", "Single source set, cross-compiled per target"),
                ("spire_app", "Spire app", "Rust core + SwiftUI, built on spire-actor & spire-core"),
            ]
        }
        return [
            ("single_source", "Single source base", "Portable — no hardware-specific layer"),
            ("hal", "Hardware abstraction", "Common core + hal/api contract + per-target implementations"),
        ]
    }

    private var hasDescription: Bool {
        !goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !projectName.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// Resolve the final project directory. The project location is fixed by
    /// the welcome screen (`root`); the project is created in a fresh
    /// `<root>/<projectName>` subdirectory. The double-nesting guard keeps the
    /// scaffold from creating `<root>/<name>/<name>` when `root`'s leaf
    /// already equals the project name — in that case the project is created
    /// directly in `root`.
    private var resolvedProjectDirectory: String {
        let parent = root.hasSuffix("/") ? String(root.dropLast()) : root
        let cleanName = projectName.trimmingCharacters(in: .whitespacesAndNewlines)
        if cleanName.isEmpty {
            return parent
        }
        if parent.isEmpty {
            return cleanName
        }
        let parentLeaf = (parent as NSString).lastPathComponent
        if parentLeaf == cleanName {
            return parent
        }
        return "\(parent)/\(cleanName)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            header

            if planResult == nil {
                describeForm
            } else {
                planForm
            }
        }
        .padding(24)
        .frame(maxWidth: 640)
        .background(.background, in: RoundedRectangle(cornerRadius: 12))
        .task {
            availablePlatforms = await bridge.fetchPlatforms()
            if projectName.isEmpty {
                projectName = (root as NSString).lastPathComponent
            }
        }
        .onChange(of: buildSystem) { _, _ in
            structure = structureOptions.first?.key ?? "native"
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label("New Project", systemImage: "hammer.badge.plus")
                .font(.title2.weight(.semibold))
            Text(planResult == nil
                 ? "Describe the project — Spire will generate a plan"
                 : "Review the plan — accept to scaffold and run it")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Describe the project

    private var describeForm: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Build system")
                .font(.headline)
            Picker("Build system", selection: $buildSystem) {
                ForEach(buildSystems, id: \.self) { system in
                    Text(system).tag(system)
                }
            }
            .pickerStyle(.segmented)

            Text("Project structure")
                .font(.headline)
            Picker("Project structure", selection: $structure) {
                ForEach(structureOptions, id: \.key) { opt in
                    Text(opt.title).tag(opt.key)
                }
            }
            .pickerStyle(.segmented)
            if let selected = structureOptions.first(where: { $0.key == structure }) {
                Label(selected.subtitle, systemImage: "square.grid.3x3")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }

            Text("Project name")
                .font(.headline)
            TextField("e.g. my-app", text: $projectName)
                .textFieldStyle(.roundedBorder)

            if !resolvedProjectDirectory.isEmpty {
                Label("Will create in: \(resolvedProjectDirectory)",
                      systemImage: "folder.badge.plus")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Text("Platforms (optional — empty = host only)")
                .font(.headline)
            if availablePlatforms.isEmpty {
                Text("No cross-compilation platforms registered")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(availablePlatforms.map(\.id), id: \.self) { id in
                    Toggle(id, isOn: Binding(
                        get: { selectedPlatforms.contains(id) },
                        set: { isOn in
                            if isOn { selectedPlatforms.insert(id) } else { selectedPlatforms.remove(id) }
                        }
                    ))
                    .toggleStyle(.checkbox)
                }
            }

            Text("What should the LLM implement?")
                .font(.headline)
            TextField("e.g. A CLI that converts CSV to JSON with async I/O",
                      text: $goal, axis: .vertical)
                .textFieldStyle(.plain)
                .lineLimit(4...8)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(.quaternary))

            if let errorMessage {
                Label(errorMessage, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            // The Plan button appears only once the project has been described
            // AND a location resolved.
            if hasDescription && !resolvedProjectDirectory.isEmpty {
                Button {
                    Task { await plan() }
                } label: {
                    if isPlanning {
                        HStack(spacing: 6) {
                            ProgressView().controlSize(.small)
                            Text("Planning…")
                        }
                    } else {
                        Label("Plan", systemImage: "sparkles")
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(isPlanning)
            }
        }
    }

    // MARK: - Plan review (OK / Reject)

    /// Review shows ONLY the plan — the LLM's ordered steps. No file/scaffold
    /// listing: the user either accepts the plan or rejects it and edits the
    /// description.
    private var planForm: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                if let result = planResult {
                    let plan = result.plan
                    if plan.isTemplate {
                        Label(
                            "LLM unavailable — this is a minimal template, not a generated implementation. Check ~/.spire/logs/spire-scaffold.log and the LLM settings.",
                            systemImage: "exclamationmark.triangle.fill"
                        )
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .padding(6)
                        .background(RoundedRectangle(cornerRadius: 6).fill(.orange.opacity(0.12)))
                    }
                    Text("Plan").font(.headline)
                    ForEach(plan.steps) { step in
                        HStack(spacing: 6) {
                            Image(systemName: step.stepType.systemImage)
                                .foregroundStyle(.secondary)
                                .frame(width: 20)
                            Text(step.description)
                                .font(.callout)
                            Spacer()
                            if let result = executedSteps[step.id] {
                                Image(systemName: result.success ? "checkmark.circle.fill" : "xmark.circle.fill")
                                    .foregroundStyle(result.success ? .green : .red)
                            }
                        }
                    }

                    if isSpireApp {
                        specSection
                    }

                    if let err = errorMessage {
                        Text(err).font(.caption).foregroundStyle(.red)
                    }

                    HStack(spacing: 12) {
                        Button {
                            Task { await reject() }
                        } label: {
                            Label("Reject", systemImage: "xmark.circle")
                        }
                        .buttonStyle(.bordered)
                        .disabled(isExecuting)

                        Button {
                            Task { await confirm(result: result) }
                        } label: {
                            if isExecuting {
                                HStack(spacing: 6) {
                                    ProgressView().controlSize(.small)
                                    Text("Scaffolding & running…")
                                }
                            } else {
                                Label("OK — scaffold and run", systemImage: "checkmark.circle.fill")
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(isExecuting)
                    }
                }
            }
            .padding(.vertical, 1)
        }
    }

    // MARK: - AppSpec (Spire app only)

    /// Optional AppSpec requirements pass: derive a VALIDATED spec from the
    /// goal, review its summary, then let OK run the deterministic skeleton
    /// codegen (types/actors/FFI dispatch + Swift wrappers/screens) instead of
    /// the generic LLM fill plan.
    @ViewBuilder
    private var specSection: some View {
        Divider()
        HStack(spacing: 6) {
            Text("AppSpec").font(.headline)
            Text("spec-driven codegen").font(.caption).foregroundStyle(.secondary)
        }
        if let spec = appSpec, let summary = AppSpecSummary(json: spec) {
            Label(summary.headline, systemImage: "doc.text.magnifyingglass")
                .font(.callout)
                .lineLimit(1)
                .truncationMode(.tail)
            LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: 3), spacing: 6) {
                ForEach(summary.rows, id: \.label) { row in
                    VStack(spacing: 2) {
                        Text(row.value).font(.headline)
                        Text(row.label).font(.caption2).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
                    .background(RoundedRectangle(cornerRadius: 6).fill(.quaternary))
                }
            }
            DisclosureGroup("AppSpec JSON") {
                Text(summary.prettyJSON)
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .font(.caption)
            Text("OK below will scaffold the monorepo, then write the generated skeleton steps (no LLM fill).")
                .font(.caption)
                .foregroundStyle(.secondary)
        } else {
            Button {
                Task { await deriveSpec() }
            } label: {
                if isDerivingSpec {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Deriving AppSpec…")
                    }
                } else {
                    Label("Derive AppSpec from goal (requirements pass)", systemImage: "doc.badge.gearshape")
                }
            }
            .buttonStyle(.bordered)
            .disabled(isDerivingSpec || isExecuting)
            if let specError {
                Text(specError).font(.caption).foregroundStyle(.red)
            }
            Text("Optional: skip to keep the generic LLM fill plan.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Actions

    @MainActor
    private func plan() async {
        errorMessage = nil
        planResult = nil
        isPlanning = true
        defer { isPlanning = false }
        let targetDir = resolvedProjectDirectory
        SpireBridge.logScaffold("ProjectWizardView: Plan pressed (goal='\(goal)' dir=\(targetDir) buildSystem=\(buildSystem))")
        let platforms = Array(selectedPlatforms).sorted()
        if let result = await bridge.planScaffold(
            goal: goal.trimmingCharacters(in: .whitespacesAndNewlines),
            rootDir: targetDir,
            projectName: projectName.trimmingCharacters(in: .whitespaces),
            language: buildSystem,
            platforms: platforms,
            structure: structure
        ) {
            SpireBridge.logScaffold("ProjectWizardView: Plan OK — \(result.plan.steps.count) steps")
            planResult = result
        } else {
            SpireBridge.logScaffold("ProjectWizardView: Plan returned nil")
            errorMessage = "Failed to generate the plan. Check ~/.spire/logs/spire-scaffold.log for details."
        }
    }

    /// Reject: clear the goal + plan; the form stays active because NOTHING
    /// has been scaffolded (the spec was computed in memory only).
    @MainActor
    private func reject() async {
        goal = ""
        planResult = nil
        errorMessage = nil
        executedSteps = [:]
        appSpec = nil
        specError = nil
    }

    /// Derive a VALIDATED AppSpec from the goal (SpireApp requirements pass).
    @MainActor
    private func deriveSpec() async {
        isDerivingSpec = true
        specError = nil
        defer { isDerivingSpec = false }
        let name = projectName.trimmingCharacters(in: .whitespaces)
        let cleanGoal = goal.trimmingCharacters(in: .whitespacesAndNewlines)
        let (spec, err) = await bridge.generateAppSpec(projectName: name, goal: cleanGoal)
        if let spec {
            appSpec = spec
            SpireBridge.logScaffold("ProjectWizardView: AppSpec derived for '\(name)'")
        } else {
            specError = err ?? "Failed to derive the AppSpec — check ~/.spire/logs/spire-scaffold.log."
        }
    }

    /// OK: scaffold (materialize), execute the plan step-by-step, then open +
    /// analyze the project so the dashboard shows the new structure.
    @MainActor
    private func confirm(result: PlanScaffoldResult) async {
        isExecuting = true
        errorMessage = nil
        executedSteps = [:]
        defer { isExecuting = false }

        let targetDir = resolvedProjectDirectory
        let name = projectName.trimmingCharacters(in: .whitespaces)
        let platforms = Array(selectedPlatforms).sorted()

        SpireBridge.logScaffold("ProjectWizardView: OK — scaffolding \(name) at \(targetDir)")
        if let err = await bridge.scaffoldProject(
            buildSystem: buildSystem,
            projectName: name,
            root: targetDir,
            platforms: platforms,
            structure: structure
        ) {
            errorMessage = err
            return
        }

        SpireBridge.logScaffold("ProjectWizardView: executing \(result.plan.steps.count) steps")
        // When the user derived an AppSpec, run the deterministic codegen
        // skeleton instead of the generic LLM fill plan (fall back if codegen
        // is unavailable so the flow never dead-ends).
        var stepsToRun = result.plan.steps
        if isSpireApp, let spec = appSpec {
            if let codegen = await bridge.generateCodeSteps(projectName: name, spec: spec),
               !codegen.isEmpty {
                stepsToRun = codegen
                SpireBridge.logScaffold("ProjectWizardView: using \(codegen.count) AppSpec codegen steps")
            } else {
                SpireBridge.logScaffold("ProjectWizardView: codegen unavailable — falling back to plan steps")
            }
        }
        let results = await bridge.executeCreationPlan(rootDir: targetDir, steps: stepsToRun)
        for stepResult in results {
            executedSteps[stepResult.stepId] = stepResult
        }

        SpireBridge.logScaffold("ProjectWizardView: opening project at \(targetDir)")
        await bridge.openProject(root: targetDir)
    }
}

#Preview {
    ProjectWizardView(root: "/tmp/demo")
        .environment(SpireBridge.shared)
        .frame(width: 640, height: 520)
}