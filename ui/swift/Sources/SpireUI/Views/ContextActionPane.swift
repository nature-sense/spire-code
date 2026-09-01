import SwiftUI
import AppKit

/// The right-hand pane: contextual actions for the current state.
///
///   • no project   → Open Project… / New Project…
///   • creating     → the new-project wizard (Embedded/Native + HAL)
///   • opening      → progress
///   • empty folder → Structure Project…
///   • project open → project workflows (build/test/lint/fix/plan + HAL),
///                    build log + chat
struct ContextActionPane: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    /// Selection lifted from ContentView (kept consistent with the left pane).
    let selectedSubproject: SubprojectInfo?
    let selectedBuildTarget: String?

    /// Presents the HAL workflow (propose → approve → add target) as a sheet.
    @State private var showingHALWorkflow = false
    /// HAL viewer: documentation + verification are separate floating dialogs.
    @State private var showingHALDocs = false
    @State private var showingHALVerify = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                switch bridge.state {
                case .unconnected:
                    welcomeActions
                case .opening:
                    statusPad("Opening project…")
                case .creating, .scaffolding, .filling:
                    // Once a plan exists the wizard hands off to the execution screen.
                    if bridge.creationPlan != nil {
                        PlanView()
                            .frame(maxWidth: .infinity)
                    } else {
                        NewProjectView()
                            .frame(maxWidth: .infinity)
                    }
                case .error(let message):
                    errorPad(message)
                case .idle(let project):
                    if project.isEmpty {
                        emptyProjectActions
                    } else {
                        projectWorkflows(project)
                    }
                }
            }
            .padding(12)
        }
        .background(theme.background)
        // HAL workflow (propose → approve → add target) presented from the
        // project-context actions.
        .sheet(isPresented: $showingHALWorkflow) {
            HALWorkflowView()
                .environment(bridge)
                .environment(theme)
        }
    }

    // MARK: - No project: open / create

    private var welcomeActions: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Actions")
                .font(.headline)
            actionCard(title: "Open Project…",
                       subtitle: "Choose an existing project folder",
                       icon: "folder",
                       accent: theme.accentBackground) {
                openPanel()
            }
            actionCard(title: "New Project…",
                       subtitle: "Embedded or Native, with optional HAL",
                       icon: "hammer.badge.plus",
                       accent: theme.surface) {
                bridge.closeProject()
                bridge.state = .creating(plan: nil, executing: false)
                bridge.currentMode = .project
            }
        }
    }

    private func openPanel() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Open"
        panel.message = "Choose a project directory, or create a new folder"
        panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser
        if panel.runModal() == .OK, let url = panel.url {
            Task { await bridge.openProject(root: url.path) }
        }
    }

    // MARK: - Empty project

    private var emptyProjectActions: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Set Up Project")
                .font(.headline)
            actionCard(title: "Structure Project…",
                       subtitle: "Choose Embedded or Native, then HAL for embedded Meson",
                       icon: "wand.and.stars",
                       accent: theme.accentBackground) {
                bridge.state = .creating(plan: nil, executing: false)
            }
        }
    }

    // MARK: - Open project: workflows, log + chat

    @ViewBuilder
    private func projectWorkflows(_ project: ProjectInfo) -> some View {
        if let sub = selectedSubproject ?? project.subprojects.first(where: { !$0.buildSystem.isEmpty }) {
            if sub.buildSystem == "Meson" {
                // The HAL contract-format correction plan appears ONLY when the
                // layout tree's `hal/api` row is selected (the inline badge
                // shows OK/issues; clicking the row surfaces the plan here).
                if bridge.showHalContractLint {
                    HALLintStatusView(projectRoot: project.root)
                        .padding(.vertical, 2)
                }
            }
            if sub.buildSystem == "Cargo" {
                dependencyCard
            }
        }

        // Full action surface: build/test/lint/fix/plan + live log + chat.
        // When a HAL platform domain is selected (rpi5/rock3c) and no explicit
        // build target is active, Build targets that platform's executable
        // (ai-trap-<domain>) so the domain-scoped actions are correct.
        ActionPanelView(
            project: project,
            selectedSubproject: selectedSubproject,
            selectedBuildTarget: selectedBuildTarget ?? platformTargetForSelectedDomain(project),
            selectedFilePath: .constant(nil),
            onOpenFile: { _ in }
        )
        .frame(maxWidth: .infinity)
    }

    /// Map the selected HAL platform domain (e.g. "rpi5") to its Meson
    /// executable target (e.g. "ai-trap-rpi5"), when such a target exists.
    private func platformTargetForSelectedDomain(_ project: ProjectInfo) -> String? {
        guard let domain = bridge.selectedDomain,
              domain != "common",
              let hal = project.subprojects.first(where: { $0.structure == "hal" }) else {
            return nil
        }
        let platform = domain
        return hal.buildTargets.first {
            $0.platform == platform || $0.name.hasSuffix("-\(platform)")
        }?.name
    }

    private var dependencyCard: some View {
        card(title: "Dependencies", icon: "shippingbox") {
            Text("Use the Dependencies tab in the detail views.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Status helpers

    private func statusPad(_ text: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Actions")
                .font(.headline)
            ProgressView()
                .controlSize(.small)
            Text(text)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func errorPad(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Connection Error")
                .font(.headline)
            Text(message)
                .font(.caption)
                .foregroundStyle(.secondary)
            Button("Retry") {
                Task { await bridge.checkConnection() }
            }
            .buttonStyle(.borderedProminent)
        }
    }

    // MARK: - Card helpers

    private func actionCard(title: String, subtitle: String, icon: String,
                            accent: Color, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.title3)
                    .foregroundStyle(theme.accent)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.callout.weight(.semibold))
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                Spacer()
            }
            .padding(10)
            .background(RoundedRectangle(cornerRadius: 8).fill(accent))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
        }
        .buttonStyle(.plain)
    }

    private func card<Content: View>(title: String, icon: String,
                                     @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(title, systemImage: icon)
                .font(.callout.weight(.semibold))
            content()
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
    }
}

/// Single contextual surface for the HAL lint state (file-by-file).
///
/// Shows every `hal/api` header with a checkmark (clean) or its inline
/// errors + a `Fix` button. Fix fetches the whole-file rewrite prompt and
/// opens a panel to paste the LLM-corrected header (written + re-linted).
private struct HALLintStatusView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let projectRoot: String

    @State private var lintFiles: [HalDocLintFile]?
    @State private var loading = true
    @State private var fixingPath: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Validate Contract Format", systemImage: "checkmark.seal")
                .font(.callout.weight(.semibold))

            if let lintFiles {
                if lintFiles.allSatisfy({ $0.issues.isEmpty }) {
                    Text("All contracts valid")
                        .font(.caption).foregroundStyle(.green)
                } else {
                    ForEach(Array(lintFiles.enumerated()), id: \.element.path) { _, file in
                        fileRow(file)
                    }
                }
            } else if loading {
                Text("Validating contract format...").font(.caption).foregroundStyle(.secondary)
            } else {
                Text("No HAL contracts found.").font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
        .task { await reload() }
    }

    private func reload() async {
        let report = await bridge.halDocLint(root: projectRoot)
        lintFiles = report?.files
        loading = false
    }

    private func fileRow(_ file: HalDocLintFile) -> some View {
        let clean = file.issues.isEmpty
        return VStack(alignment: .leading, spacing: 3) {
            HStack {
                Text(URL(fileURLWithPath: file.path).lastPathComponent)
                    .font(.caption.monospaced().weight(.semibold))
                Spacer()
                if clean {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                        .font(.caption.weight(.bold))
                } else {
                    if fixingPath == file.path {
                        ProgressView().controlSize(.small)
                    } else {
                        Button("Fix") {
                            openFixPanel(file)
                        }
                        .buttonStyle(.bordered)
                        .controlSize(.mini)
                    }
                }
            }
            if !clean {
                ForEach(file.issues.prefix(4)) { issue in
                    Text("\(issue.symbol): \(issue.message)")
                        .font(.caption2)
                        .foregroundStyle(issue.severity == "error" ? .red : .orange)
                        .lineLimit(1)
                }
                if file.issues.count > 4 {
                    Text("+ \(file.issues.count - 4) more")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
        }
        .padding(6)
        .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface.opacity(0.5)))
    }

    private func openFixPanel(_ file: HalDocLintFile) {
        Task { @MainActor in
            fixingPath = file.path
            guard let proposal = await bridge.halFixPropose(root: projectRoot, path: file.path) else {
                fixingPath = nil
                presentFixAlert("Proposal request failed. Check that the backend/LLM is reachable.")
                return
            }
            guard proposal.status == "proposed", let content = proposal.proposedContent, !content.isEmpty else {
                fixingPath = nil
                presentFixAlert(proposal.error ?? "Fix proposal returned no content (status: \(proposal.status)).")
                return
            }
            var fixWindow: NSWindow?
            let panel = HALFixVerifyPanel(path: file.path, proposedContent: content) {
                Task { await reload() }
                fixWindow?.close()
            }
            let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1000, height: 700),
                             styleMask: [.titled, .closable, .resizable, .miniaturizable],
                             backing: .buffered, defer: false)
            w.title = "Proposed fix: \(URL(fileURLWithPath: file.path).lastPathComponent)"
            fixWindow = w
            w.contentViewController = NSHostingController(rootView: panel.environment(bridge).environment(theme))
            w.setContentSize(NSSize(width: 1000, height: 700))
            w.center()
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            FixWindows.keep(w)
            fixingPath = nil
        }
    }
}



    /// Present a modal alert with the fix-flow failure message.
    private func presentFixAlert(_ message: String) {
        let alert = NSAlert()
        alert.messageText = "HAL Fix"
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

/// Whole-file fix verification popup: shows the LLM-proposed header read-only
/// with Accept (write + re-lint in place) / Reject (discard) buttons.
private struct HALFixVerifyPanel: View {
    @Environment(SpireBridge.self) private var bridge
    let path: String
    let proposedContent: String
    let onDone: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Proposed rewrite of \(URL(fileURLWithPath: path).lastPathComponent)")
                .font(.headline)

            ScrollView {
                Text(SyntaxHighlighter.highlight(proposedContent, language: .cpp))
                    .font(.caption2.monospaced())
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
            .background(RoundedRectangle(cornerRadius: 6).fill(.black.opacity(0.05)))

            HStack {
                Text("Accept writes the file and re-validates the contract.")
                    .font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Reject") { onDone() }
                Button("Accept") {
                    do {
                        try proposedContent.write(toFile: path, atomically: true, encoding: .utf8)
                    } catch {
                        print("hal fix write failed: \(error)")
                    }
                    onDone()
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .padding()
    }
}

private struct MigrateHalCard: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let projectRoot: String

    @State private var migrationPlan: HalMigrationPlan?
    @State private var result: HalMigrationResult?
    @State private var errorMessage: String?

    var body: some View {
        if let plan = migrationPlan, plan.canApply {
            VStack(alignment: .leading, spacing: 6) {
                Label("Migrate HAL", systemImage: "arrow.triangle.2.circlepath")
                    .font(.callout.weight(.semibold))

                Text("\(plan.moves.count) file moves + \(plan.writeFiles.count) writes.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if !plan.conflicts.isEmpty {
                    Text("Conflicts: \(plan.conflicts.joined(separator: ", "))")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                }
                Button("Apply Migration") {
                    apply()
                }
                .buttonStyle(.borderedProminent)
                .disabled(result != nil)

                if let result {
                    Text("Moved \(result.appliedMoves.count), wrote \(result.writtenFiles.count), edited \(result.appliedEdits.count).")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    if !result.errors.isEmpty {
                        Text(result.errors.joined(separator: "\n"))
                            .font(.caption2)
                            .foregroundStyle(.red)
                    }
                }
                if let errorMessage {
                    Text(errorMessage).font(.caption2).foregroundStyle(.red)
                }
            }
            .padding(10)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
        } else if let errorMessage {
            // Surface plan errors only (a canonical project renders nothing).
            Text(errorMessage).font(.caption2).foregroundStyle(.red).padding(10)
        } else if migrationPlan == nil {
            Color.clear
                .frame(height: 1)
                .task { await plan() }
        }
    }

    private func plan() async {
        let (p, err) = await bridge.halMigratePlan(root: projectRoot)
        await MainActor.run {
            if let p {
                migrationPlan = p
            } else {
                errorMessage = err ?? "plan failed"
            }
        }
    }

    private func apply() {
        guard let plan = migrationPlan else { return }
        errorMessage = nil
        Task {
            let (r, err) = await bridge.halMigrateApply(root: projectRoot, plan: plan)
            await MainActor.run {
                if let r {
                    result = r
                } else {
                    errorMessage = err ?? "apply failed"
                }
            }
        }
    }
}


/// Retains HAL fix verify windows so they stay alive (portal pattern).
private enum FixWindows {
    static var list: [NSWindow] = []
    static func keep(_ w: NSWindow) {
        list.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification,
                                               object: w, queue: .main) { _ in
            list.removeAll { $0 === w }
        }
    }
}
