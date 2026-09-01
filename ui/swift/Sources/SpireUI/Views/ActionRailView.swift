import SwiftUI

/// Always-on right action rail. Single home for every action in the app,
/// driven entirely by the current context:
///   • Welcome (no project)  → Open Project… / New Project…
///   • Opening / wizard      → status + wizard entry
///   • Empty project folder  → Structure Project…
///   • Open project          → contextually grouped action cards
///     (build/test/lint/fix/plan + log + chat, plus Meson HAL guidance)
struct ActionRailView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    // Selection the rail owns when a project is open (subproject-level actions).
    @State private var railSubproject: SubprojectInfo?
    @State private var railBuildTarget: String?
    @State private var railFilePath: String?

    private let railWidth: CGFloat = 300

    var body: some View {
        VStack(spacing: 0) {
            railHeader

            Divider().overlay(theme.divider)

            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    switch bridge.state {
                    case .unconnected:
                        welcomeActions
                    case .opening:
                        openingStatus
                    case .creating, .scaffolding, .filling:
                        wizardStatus
                    case .error(let message):
                        errorStatus(message)
                    case .idle(let project):
                        if project.isEmpty {
                            emptyProjectActions
                        } else {
                            projectActions(project)
                        }
                    }
                }
                .padding(10)
            }
        }
        .frame(width: railWidth)

        // Seed the subproject-level selection from the opened project once.
        .onAppear {
            seedProjectSelection()
        }
        .onChange(of: bridge.projectInfo?.id) { _, _ in
            seedProjectSelection()
        }
    }

    private func seedProjectSelection() {
        guard let project = bridge.projectInfo else { return }
        if railSubproject == nil {
            railSubproject = project.subprojects.first { !$0.buildSystem.isEmpty }
        }
    }

    private var railHeader: some View {
        HStack(spacing: 6) {
            Image(systemName: "bolt.fill")
                .font(.caption)
                .foregroundStyle(theme.accent)
            Text("Actions")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(theme.textSecondary)
            Spacer()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
    }

    // MARK: - Welcome actions

    private var welcomeActions: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Get Started")
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

    private var openingStatus: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Opening Project")
                .font(.headline)
            ProgressView()
                .controlSize(.small)
            Text("Analyzing the project…")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var wizardStatus: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("New Project")
                .font(.headline)
            Text("Configure the wizard to scaffold a new project.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func errorStatus(_ message: String) -> some View {
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

    // MARK: - Open project: contextual workflows

    @ViewBuilder
    private func projectActions(_ project: ProjectInfo) -> some View {
        // Selected subproject drives the action set; default to the first
        // buildable subproject when nothing is picked in the left pane.
        let sub = railSubproject ?? project.subprojects.first { !$0.buildSystem.isEmpty }

        if let sub {
            // Build-system–aware workflow cards.
            buildVerifyCard(project: project, sub: sub)
            if sub.buildSystem == "Meson" {
                halCard(sub: sub)
            }
            if sub.buildSystem == "Cargo" {
                dependencyCard
            }
        } else {
            Text("Select a subproject to see its actions")
                .font(.caption)
                .foregroundStyle(.secondary)
        }

        // The full action surface: build/test/lint/fix/plan + live log + chat.
        ActionPanelView(
            project: project,
            selectedSubproject: sub,
            selectedBuildTarget: railBuildTarget,
            selectedFilePath: $railFilePath,
            onOpenFile: { _ in }
        )
        .frame(maxWidth: .infinity)
    }

    private func buildVerifyCard(project: ProjectInfo, sub: SubprojectInfo) -> some View {
        card(title: "Build & Verify", icon: "hammer.fill") {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    chipButton("Build", systemImage: "hammer.fill") {
                        runTool("build_build", project: project, sub: sub)
                    }
                    chipButton("Test", systemImage: "checkmark.circle") {
                        runTool("build_test", project: project, sub: sub)
                    }
                    chipButton("Lint", systemImage: "exclamationmark.triangle") {
                        runTool("build_lint", project: project, sub: sub)
                    }
                }
                HStack(spacing: 6) {
                    chipButton("Clean", systemImage: "trash") {
                        runTool("build_clean", project: project, sub: sub)
                    }
                    chipButton("Fix", systemImage: "wrench.and.screwdriver") {
                        runTool("build_fix", project: project, sub: sub)
                    }
                    chipButton("Plan", systemImage: "map.fill") {
                        // Plan opens via the right pane's plan sheet if present;
                        #warning("Plan sheet hook")
                    }
                }
            }
        }
    }

    private func halCard(sub: SubprojectInfo) -> some View {
        card(title: "HAL Workflow", icon: "cpu") {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(["Validate contract", "Write contract", "Generate implementation", "Add target"], id: \.self) { step in
                    Label(step, systemImage: "circle")
                        .font(.caption)
                    Divider()
                }
                Text("Open Tools → HAL for the guided workflow.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
    }

    private var dependencyCard: some View {
        card(title: "Dependencies", icon: "shippingbox") {
            Text("Use the Dependencies tab in the center pane.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Tool invocation

    private func runTool(_ tool: String, project: ProjectInfo, sub: SubprojectInfo) {
        // Reuse the bridge's unified build tool call surface via a temporary
        // BuildPanelViewModel (same path ActionPanelView.runTool uses).
        let vm = BuildPanelViewModel(service: bridge.makeBuildService())
        let cleanPath = sub.path.hasSuffix("/") ? String(sub.path.dropLast()) : sub.path
        let absPath: String
        if cleanPath.hasPrefix("/") {
            absPath = cleanPath
        } else if cleanPath.isEmpty {
            absPath = project.root
        } else {
            let root = project.root.hasSuffix("/") ? String(project.root.dropLast()) : project.root
            absPath = root + "/" + cleanPath
        }
        vm.startEventConsumer()
        Task {
            await vm.runTool(tool, path: absPath, language: sub.language, package: sub.name,
                             platform: nil, target: railBuildTarget)
        }
    }

    // MARK: - Card/chip helpers

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

    private func chipButton(_ title: String, systemImage: String,
                            action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: systemImage)
                .font(.caption.weight(.medium))
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(RoundedRectangle(cornerRadius: 5).fill(theme.nodeBackground))
                .overlay(RoundedRectangle(cornerRadius: 5).stroke(theme.border, lineWidth: 0.5))
        }
        .buttonStyle(.plain)
    }
}