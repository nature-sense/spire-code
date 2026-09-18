import SwiftUI
import AppKit

/// Step-wise "Start New Project" wizard: a **tree**, because the shape of a project decides which
/// questions are worth asking.
///
///   Environment ─┬─ Native ────┬─ Spire UI App         → `spire_app`
///                │             └─ CLI                  → `native`
///                └─ Embedded ─┬─ Linux SBC (C++) ───── Targets → Structure
///                             │                          → `single_source` | `hal`  (unchanged)
///                             └─ Controller (Rust) ─┬─ HAL + Frameworks → `embedded_hal`
///                                                  └─ Application      → `embedded_app`
///
/// The steps are therefore **data, not an enum's order**: a Native CLI needs three, a Linux SBC five
/// (exactly the Meson shape this wizard has always had), a Controller five. `path` computes the
/// sequence for the current choices and the footer walks it, so a branch that does not apply is
/// never a step the user has to get past.
///
/// Each leaf is one `ProjectStructure` key plus the `embedded` flag, and the `embedded` flag is the
/// tree's own answer: Native is host-only, an embedded leaf is *defined* by its targets.
struct NewProjectView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    /// Host or hardware — the top of the tree.
    ///
    /// Named `ProjectEnvironment`, not `Environment`: the latter is SwiftUI's property wrapper, and
    /// shadowing it breaks every `@Environment` in this file.
    enum ProjectEnvironment: String, CaseIterable {
        case native, embedded
    }

    /// The two host shapes.
    enum NativeKind: String, CaseIterable {
        /// Rust core + SwiftUI on the Spire framework (`spire_app`).
        case spireApp
        /// A plain Cargo binary — the command-line tool (`native`).
        case cli
    }

    /// Embedded: a Linux SBC (C++/Meson) or a **controller** (Rust/Cargo, firmware).
    enum DeviceClass: String, CaseIterable {
        case linuxSbc
        case controller
    }

    /// A controller project is either the HAL + frameworks themselves, or an application that
    /// depends on one.
    enum ControllerRole: String, CaseIterable {
        /// The contract crate plus one backend per board family — a library (`embedded_hal`).
        case halFrameworks
        /// A firmware binary that path-deps an existing HAL project (`embedded_app`).
        ///
        /// Its own structure, because the two are **separate projects**: the app is not a HAL with a
        /// `main`, it depends on one. Scaffolding it arrives with that structure in the core; the
        /// wizard shows the choice now so the tree is complete, and does not offer it until then —
        /// a selectable card here would scaffold a plain host crate instead, silently.
        case application
    }

    enum Step: String, CaseIterable {
        case environment, nativeKind, deviceClass, controllerRole, targets, structure, halProject, details
    }

    @State private var stepIndex = 0
    @State private var environment: ProjectEnvironment = .embedded
    @State private var nativeKind: NativeKind = .spireApp
    @State private var deviceClass: DeviceClass = .linuxSbc
    @State private var controllerRole: ControllerRole = .halFrameworks
    /// Loaded from the Rust registry via `bridge.fetchPlatforms()`.
    @State private var availableTargets: [Platform] = []
    /// Selected cross-compilation target registry ids (e.g. ["rpi5", "rock3c"]).
    ///
    /// Cleared whenever the branch changes, because the two branches never offer the same platform:
    /// a kept selection would be a build for hardware the branch does not have.
    @State private var selectedTargets: Set<String> = []
    @State private var useHal: Bool = true            // Linux SBC structure: HAL vs single-source
    /// The embedded-HAL project a Controller **application** builds against.
    ///
    /// A directory the user picks, because the two are separate projects: the app's manifest
    /// path-deps that HAL's contract and backend crates, and the core reads its members to name them.
    @State private var halProject: String = ""

    @State private var goal: String = ""
    @State private var projectName: String = ""
    @State private var projectDirectory: String = ""
    @State private var isGenerating = false
    @State private var errorMessage: String?

    private var isNative: Bool { environment == .native }

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

    // MARK: - The branch

    /// The steps this branch actually needs, in order.
    ///
    /// Data rather than an enum's order: `environment → nativeKind → details` for a Native CLI,
    /// five steps for a Linux SBC (the Meson shape, unchanged), five for a Controller. The indicator
    /// and the footer both read this, so a step a branch does not have is not a step at all.
    private var path: [Step] {
        Self.path(environment: environment, deviceClass: deviceClass, controllerRole: controllerRole)
    }

    /// The steps a branch has — **pure and static** so the shape is testable without a view, the same
    /// reason the add-board menu is.
    ///
    /// `controllerRole` decides the *last* question on the Controller branch: an application is built
    /// against a HAL project that nothing else implies, so it is asked for one more answer than the
    /// HAL — which is the project itself.
    static func path(environment: ProjectEnvironment, deviceClass: DeviceClass,
                     controllerRole: ControllerRole = .halFrameworks) -> [Step] {
        var steps: [Step] = [.environment]
        switch environment {
        case .native:
            steps.append(.nativeKind)
        case .embedded:
            steps.append(.deviceClass)
            switch deviceClass {
            case .linuxSbc:
                steps.append(.targets)
                steps.append(.structure)
            case .controller:
                steps.append(.controllerRole)
                steps.append(.targets)
                if controllerRole == .application {
                    steps.append(.halProject)
                }
            }
        }
        steps.append(.details)
        return steps
    }

    /// The step being shown — clamped, because a branch change can shorten the path under the user's
    /// feet (Back from Details, then a shallower branch).
    private var step: Step {
        path[min(stepIndex, path.count - 1)]
    }

    private var isLastStep: Bool { stepIndex >= path.count - 1 }

    /// The targets this branch offers, and therefore the only ones it can select.
    private var offeredTargets: [Platform] {
        Self.offeredTargets(availableTargets, environment: environment, deviceClass: deviceClass)
    }

    /// The branch's targets: boards for a Controller, Linux SBCs for a Linux SBC, none for Native.
    ///
    /// Pure and static so a test can pin the split against the registry flags it reads — `embedded`
    /// and `family` — without a view. A second copy of this rule in the view is exactly how the
    /// wizard and the build would come to disagree about what a board is.
    static func offeredTargets(_ platforms: [Platform], environment: ProjectEnvironment,
                               deviceClass: DeviceClass) -> [Platform] {
        guard environment == .embedded else { return [] }
        switch deviceClass {
        case .linuxSbc: return platforms.filter { $0.embedded && $0.family == nil }
        case .controller: return platforms.filter { $0.embedded && $0.family != nil }
        }
    }

    private var language: String {
        isNative || deviceClass == .controller ? "Rust" : "Meson"
    }

    private var toolchainLabel: String {
        language == "Rust" ? "Rust / Cargo" : "C++ / Meson"
    }

    /// What the leaf becomes: the exact `ProjectStructure` key the core parses.
    private var structureKey: String {
        Self.structureKey(environment: environment, nativeKind: nativeKind,
                          deviceClass: deviceClass, controllerRole: controllerRole, useHal: useHal)
    }

    /// The leaf → `ProjectStructure` mapping: **pure**, so a test can walk the tree without a view.
    ///
    /// This is the whole contract between the wizard and the core. A key the core does not know falls
    /// back to `native`, silently — which is why the mapping is pinned in `SpireUITests` rather than
    /// left implicit in a view's `switch`es.
    static func structureKey(environment: ProjectEnvironment, nativeKind: NativeKind,
                             deviceClass: DeviceClass, controllerRole: ControllerRole,
                             useHal: Bool) -> String {
        switch environment {
        case .native:
            return nativeKind == .spireApp ? "spire_app" : "native"
        case .embedded:
            switch deviceClass {
            case .linuxSbc:
                return useHal ? "hal" : "single_source"
            case .controller:
                return controllerRole == .halFrameworks ? "embedded_hal" : "embedded_app"
            }
        }
    }

    /// The same choice as a sentence, for the Details summary.
    private var structureLabel: String {
        switch environment {
        case .native:
            return nativeKind == .spireApp
                ? "Spire UI App (Rust core + SwiftUI)"
                : "CLI (native Cargo binary)"
        case .embedded:
            switch deviceClass {
            case .linuxSbc:
                return useHal
                    ? "Linux SBC — hardware abstraction (recommended)"
                    : "Linux SBC — single source base (no hardware-specific layer)"
            case .controller:
                return controllerRole == .halFrameworks
                    ? "Controller — HAL + frameworks (contract + one backend per board family)"
                    : "Controller — application (firmware binary that depends on a HAL)"
            }
        }
    }

    /// Next-step gating: a step the user cannot satisfy does not let them past.
    private var canAdvance: Bool {
        switch step {
        case .environment, .nativeKind, .deviceClass, .controllerRole, .structure:
            return true
        case .targets:
            // A branch with no targets in the registry cannot be advanced past on an empty
            // selection, and saying so beats scaffolding a project with nothing to build for.
            return !offeredTargets.isEmpty && !selectedTargets.isEmpty
        case .halProject:
            // The application's dependency: without it the core refuses, so the wizard does not
            // pretend the answer is optional.
            return !halProject.trimmingCharacters(in: .whitespaces).isEmpty
        case .details:
            return !goal.trimmingCharacters(in: .whitespaces).isEmpty
                && !projectName.trimmingCharacters(in: .whitespaces).isEmpty
                && !projectDirectory.isEmpty
        }
    }

    private var stepTitle: String {
        switch step {
        case .environment: return "Environment"
        case .nativeKind: return "Project Type"
        case .deviceClass: return "Device Class"
        case .controllerRole: return "Controller Project"
        case .targets:
            // An application is one board, so the question is singular.
            if controllerRole == .application {
                return "Target Board"
            }
            return deviceClass == .controller ? "Target Board" : "Target Hardware"
        case .structure: return "Project Structure"
        case .halProject: return "HAL Project"
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
            // One dot per step *this branch* has, so the count never promises steps the branch
            // does not ask for.
            ForEach(Array(path.enumerated()), id: \.offset) { index, _ in
                Circle()
                    .fill(index <= stepIndex ? Color.orange : theme.border)
                    .frame(width: 10, height: 10)
                    .overlay(Circle().stroke(theme.border, lineWidth: 1))
            }
            Spacer()
            Text("\(min(stepIndex, path.count - 1) + 1) / \(path.count) · \(stepTitle)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: 420)
    }

    @ViewBuilder
    private var stepContent: some View {
        switch step {
        case .environment: environmentStep
        case .nativeKind: nativeKindStep
        case .deviceClass: deviceClassStep
        case .controllerRole: controllerRoleStep
        case .targets: targetsStep
        case .structure: structureStep
        case .halProject: halProjectStep
        case .details: detailsStep
        }
    }

    // MARK: Step 1 — Environment (the top of the tree)

    private var environmentStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("What kind of project?")
                .font(.headline)
            HStack(spacing: 12) {
                choiceCard(
                    title: "Embedded",
                    subtitle: "Firmware or a Linux SBC — cross-compiled for hardware",
                    systemImage: "cpu",
                    selected: environment == .embedded,
                    select: { selectEnvironment(.embedded) }
                )
                choiceCard(
                    title: "Native",
                    subtitle: "Runs on this machine (macOS)",
                    systemImage: "macbook",
                    selected: environment == .native,
                    select: { selectEnvironment(.native) }
                )
            }
            Label(
                environment == .embedded
                    ? "Targets only — no host build option."
                    : "Host only — the Spire app, or a CLI.",
                systemImage: environment == .embedded ? "exclamationmark.triangle" : "macbook"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: 480, alignment: .leading)
    }

    /// Changing the environment clears any target selection: the two branches never offer the same
    /// platform, so a kept selection would be a build for hardware this branch does not have.
    private func selectEnvironment(_ next: ProjectEnvironment) {
        guard environment != next else { return }
        environment = next
        selectedTargets.removeAll()
        errorMessage = nil
    }

    // MARK: Native — the two host shapes

    private var nativeKindStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("What are you building?")
                .font(.headline)
            HStack(spacing: 12) {
                choiceCard(
                    title: "Spire UI App",
                    subtitle: "Rust core + SwiftUI, built on spire-actor & spire-core",
                    systemImage: "sparkles",
                    selected: nativeKind == .spireApp,
                    select: { nativeKind = .spireApp }
                )
                choiceCard(
                    title: "CLI",
                    subtitle: "A Cargo binary for the command line",
                    systemImage: "terminal",
                    selected: nativeKind == .cli,
                    select: { nativeKind = .cli }
                )
            }
            Label(
                nativeKind == .spireApp
                    ? "A Cargo workspace crate plus the SwiftUI app (ui/swift) that embeds it as a dylib. Host-only (macOS)."
                    : "One Cargo binary, built for this machine. No SwiftUI, no hardware.",
                systemImage: nativeKind == .spireApp ? "sparkles" : "terminal"
            )
            .font(.callout)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: 520, alignment: .leading)
    }

    // MARK: Embedded — Linux SBC (C++) or Controller (Rust)

    private var deviceClassStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("What kind of embedded project?")
                .font(.headline)
            HStack(spacing: 12) {
                choiceCard(
                    title: "Linux SBC (C++)",
                    subtitle: "Cross-compile C++ for a single-board computer",
                    systemImage: "server.rack",
                    selected: deviceClass == .linuxSbc,
                    select: { selectDeviceClass(.linuxSbc) }
                )
                choiceCard(
                    title: "Controller (Rust)",
                    subtitle: "Firmware for a board — Cargo, one backend per family",
                    systemImage: "cpu",
                    selected: deviceClass == .controller,
                    select: { selectDeviceClass(.controller) }
                )
            }
            Label(
                deviceClass == .linuxSbc
                    ? "Meson plus a C++ toolchain per target (rpi5, rock3c, …)."
                    : "Cargo plus a board toolchain (rp2040, esp32, …).",
                systemImage: deviceClass == .linuxSbc ? "server.rack" : "cpu"
            )
            .font(.callout)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: 520, alignment: .leading)
    }

    /// Same reason as `selectEnvironment`: the two classes offer different platforms (boards vs Linux
    /// SBCs), so a selection cannot survive the switch.
    private func selectDeviceClass(_ next: DeviceClass) {
        guard deviceClass != next else { return }
        deviceClass = next
        selectedTargets.removeAll()
        errorMessage = nil
    }

    // MARK: Controller — the HAL + frameworks, or an application that depends on one

    private var controllerRoleStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("HAL or application?")
                .font(.headline)
            HStack(spacing: 12) {
                choiceCard(
                    title: "HAL + Frameworks",
                    subtitle: "Contract crate + one backend per board family",
                    systemImage: "square.stack.3d.up",
                    selected: controllerRole == .halFrameworks,
                    select: { controllerRole = .halFrameworks }
                )
                choiceCard(
                    title: "Application",
                    subtitle: "Firmware that depends on a HAL project",
                    systemImage: "app.badge",
                    selected: controllerRole == .application,
                    select: { controllerRole = .application }
                )
            }
            Label(
                controllerRole == .application
                    ? "One board, one binary — a *separate* project whose Cargo.toml path-deps a HAL's contract and backend crates. The next step asks which HAL."
                    : "The traits a firmware programs against plus the per-family implementations — a library, filled by the drift cascade.",
                systemImage: controllerRole == .application ? "app.badge" : "square.stack.3d.up"
            )
            .font(.callout)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: 520, alignment: .leading)
    }


    // MARK: Targets — the boards (Controller) or the Linux SBCs (Linux SBC)

    private var targetsStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(deviceClass == .controller
                 ? "Select at least one board (no host option)"
                 : "Select at least one hardware target (no host option)")
                .font(.headline)
            if offeredTargets.isEmpty {
                Label(
                    deviceClass == .controller
                        ? "No boards in the registry yet — a board declares a `family`."
                        : "No Linux SBC targets in the registry yet (check ~/.spire/platforms).",
                    systemImage: "tray"
                )
                .font(.callout)
                .foregroundStyle(.secondary)
            } else {
                let columns = [GridItem(.adaptive(minimum: 220), spacing: 10)]
                ScrollView {
                    LazyVGrid(columns: columns, alignment: .leading, spacing: 10) {
                        ForEach(offeredTargets, id: \.id) { platform in
                            let isSelected = selectedTargets.contains(platform.id)
                            Button {
                                // An application is one binary for one board, so a pick *replaces*:
                                // it would be refused by the core otherwise, and silently keeping two
                                // would look like it were allowed.
                                if controllerRole == .application {
                                    selectedTargets = [platform.id]
                                } else if isSelected {
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
        }
        .frame(maxWidth: 520, alignment: .leading)
    }

    // MARK: Structure — the Linux SBC's two shapes (unchanged by this refinement)

    private var structureStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Project structure")
                .font(.headline)
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
        .frame(maxWidth: 520, alignment: .leading)
    }

    // MARK: HAL project (Controller → Application)

    /// Which HAL the application depends on.
    ///
    /// Asked of the user because the two are **separate projects** and nothing else in the wizard
    /// implies the answer: the app's `Cargo.toml` path-deps that HAL's contract crate and the chosen
    /// board's backend crate, and the core reads that project's members to name them. The family in
    /// the hint is the suggestion, not the rule — a HAL whose backend for this board is missing is
    /// refused by the core, with the members it did find.
    private var halProjectStep: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Which HAL does this application build against?")
                .font(.headline)
            Text("The application is a project of its own. This is the embedded-HAL project it path-deps — the same relationship `blink-esp32` has with `spire-hal`.")
                .font(.caption)
                .foregroundStyle(.secondary)

            HStack(spacing: 8) {
                TextField("/path/to/my-hal", text: $halProject)
                    .textFieldStyle(.roundedBorder)
                    .font(.callout.monospaced())
                Button("Choose…") { chooseHalProject() }
            }
            .frame(maxWidth: 520)

            if let expectation = backendCrateHint {
                Label("Expects a backend crate: \(expectation)", systemImage: "shippingbox")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Label(
                "The HAL is not copied and not modified — it is depended on, so several applications can build against one.",
                systemImage: "arrow.triangle.branch"
            )
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: 520, alignment: .leading)
    }

    /// The backend crate the chosen board implies, when a board is chosen: `<hal>-<family>`.
    ///
    /// Derived from the *family*, not the platform id, because one backend serves every chip of a
    /// family — the same rule the core's own refusal names when it cannot find one.
    private var backendCrateHint: String? {
        guard let family = offeredTargets.first(where: { selectedTargets.contains($0.id) })?.family
        else { return nil }
        return "crates/<hal>-\(family)"
    }

    private func chooseHalProject() {
        NSApp.activate(ignoringOtherApps: true)
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose HAL Project"
        panel.message = "Select the embedded-HAL project this application depends on"
        if !halProject.isEmpty {
            panel.directoryURL = URL(fileURLWithPath: halProject)
        }
        if panel.runModal() == .OK, let url = panel.url {
            halProject = url.path
        }
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
                Text(isNative
                     ? "Targets: this machine (host)"
                     : "Targets: \(selectedTargets.sorted().joined(separator: ", "))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("Toolchain: \(toolchainLabel)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if structureKey == "embedded_app" {
                    Text("HAL: \(halProject.isEmpty ? "not chosen" : halProject)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
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
            if stepIndex > 0 {
                Button("Back") {
                    stepIndex = max(0, stepIndex - 1)
                    errorMessage = nil
                }
            }
            Spacer()
            if !isLastStep {
                Button("Next") {
                    if canAdvance {
                        stepIndex = min(path.count - 1, stepIndex + 1)
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

    /// One card. `enabled: false` shows a choice without offering it — for a leaf whose scaffold has
    /// not landed yet, where a selectable card would scaffold something else instead.
    private func choiceCard(title: String, subtitle: String, systemImage: String,
                            selected: Bool, enabled: Bool = true,
                            select: @escaping () -> Void) -> some View {
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
        .opacity(enabled ? 1 : 0.45)
        .disabled(!enabled)
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
            let plan = await bridge.generateProjectPlan(
                goal: goal,
                rootDir: resolvedProjectDirectory,
                language: language,
                // Native is host-only; an embedded leaf is *defined* by its targets, which is why
                // the two branches cannot share a platform list.
                platforms: isNative ? [] : selectedTargets.sorted(),
                structure: structureKey,
                embedded: !isNative,
                // Keyed on the *structure*, not the role: an application is the only leaf whose
                // dependencies come from another project, so that is exactly when it travels.
                halRoot: structureKey == "embedded_app" ? halProject : nil
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