import SwiftUI

/// Main project dashboard — no sidebar. Horizontally split into two halves:
/// left = graph (top) + detail tabs (bottom), right = file content viewer.
struct ProjectDashboardView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var selectedFilePath: String? = nil
    @State private var selectedSubproject: SubprojectInfo? = nil
    @State private var selectedBuildTarget: String? = nil
    @State private var selectedTab: String = "Sources"
    /// Target-scoped detail (deps/platform/files) fetched from the graph.
    @State private var targetDetail: BuildTargetDetail? = nil

    private let tabs = ["Sources", "Dependencies", "Build"]

    var body: some View {
        if bridge.loading {
            loadingView
                .onAppear {
                    Task { await bridge.fetchProjectAnalysis() }
                }
        } else if let project = bridge.projectInfo {
            GeometryReader { geo in
                HStack(spacing: 0) {
                // ── Left pane (50%) — graph on top, detail tabs below ──
                VStack(spacing: 0) {
                    // Upper: Project graph
                    ProjectAnalysisView(
                        project: project,
                        selectedSubproject: $selectedSubproject,
                        selectedBuildTarget: $selectedBuildTarget
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .onChange(of: selectedBuildTarget) { _, newValue in
                        Task { await loadTargetDetail(name: newValue) }
                    }

                    Divider()
                        .overlay(theme.divider)

                    // Lower: Tab bar (Sources / Dependencies / Build)
                    VStack(spacing: 0) {
                        HStack(spacing: 0) {
                            ForEach(tabs, id: \.self) { tab in
                                Button {
                                    selectedTab = tab
                                } label: {
                                    HStack(spacing: 4) {
                                        Image(systemName: tabSystemImage(tab))
                                            .font(.caption)
                                        Text(tab)
                                            .font(.subheadline.weight(.medium))
                                    }
                                    .frame(maxWidth: .infinity)
                                    .padding(.vertical, 6)
                                    .foregroundColor(selectedTab == tab ? theme.accent : theme.textSecondary)
                                    .background(
                                        selectedTab == tab
                                            ? theme.accentBackground
                                            : Color.clear
                                    )
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .padding(.horizontal, 8)

                        Divider()
                            .padding(.horizontal, 8)

                        // Tab content
                        switch selectedTab {
                        case "Sources":
                            if let targetFiles = targetDetail?.files, !targetFiles.isEmpty {
                                List {
                                    ForEach(targetFiles) { file in
                                        if let path = file.path {
                                            Button {
                                                selectedFilePath = path
                                            } label: {
                                                HStack(spacing: 4) {
                                                    Image(systemName: "doc")
                                                        .foregroundStyle(.secondary)
                                                    Text(path)
                                                        .font(.callout)
                                                        .foregroundStyle(
                                                            selectedFilePath == path
                                                                ? theme.accent : Color.primary
                                                        )
                                                }
                                            }
                                            .buttonStyle(.plain)
                                        }
                                    }
                                }
                                .listStyle(.sidebar)
                            } else if let root = subprojectTree(for: project) {
                                List {
                                    Section(root.name) {
                                        SubFileTreeSection(
                                            directory: root,
                                            selectedFilePath: $selectedFilePath
                                        )
                                    }
                                }
                                .listStyle(.sidebar)
                            } else {
                                Text(targetDetail == nil ? "Select a build target" : "No source files")
                                    .foregroundStyle(.tertiary)
                                    .padding()
                            }

                        case "Dependencies":
                            if let deps = targetDetail?.dependencies, !deps.isEmpty {
                                Table(deps) {
                                    TableColumn("") { _ in
                                        Image(systemName: "shippingbox")
                                            .foregroundStyle(.secondary)
                                    }
                                    .width(24)
                                    TableColumn("Name") { dep in
                                        Text(dep.name)
                                    }
                                    TableColumn("Version") { dep in
                                        Text(dep.version ?? "")
                                    }
                                }
                            } else {
                                VStack(spacing: 8) {
                                    Image(systemName: "shippingbox")
                                        .font(.system(size: 28))
                                        .foregroundStyle(.tertiary)
                                    Text("No dependencies")
                                        .font(.headline)
                                        .foregroundStyle(.secondary)
                                }
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                            }

                        case "Build":
                            VStack(alignment: .leading, spacing: 0) {
                            if let target = selectedBuildTarget {
                                HStack(spacing: 6) {
                                    Image(systemName: "gearshape.fill")
                                        .foregroundStyle(.secondary)
                                    Text("Build target: ")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                    Text(shortTargetName(target))
                                        .font(.caption.weight(.semibold))
                                    Spacer(minLength: 0)
                                }
                                .padding(.horizontal, 10)
                                .padding(.vertical, 6)
                                Divider()
                            }
                            // Target-scoped build config file (from the graph query).
                            if let detail = targetDetail, let config = detail.configFile, !config.isEmpty {
                                List {
                                    Button {
                                        selectedFilePath = config
                                    } label: {
                                        HStack(spacing: 6) {
                                            Image(systemName: "hammer")
                                                .foregroundColor(
                                                    selectedFilePath == config
                                                        ? theme.accent : theme.textSecondary
                                                )
                                                .font(.caption)
                                            Text(config)
                                                .font(.callout)
                                                .foregroundColor(
                                                    selectedFilePath == config
                                                        ? .primary : .secondary
                                                )
                                            Spacer(minLength: 0)
                                        }
                                    }
                                    .buttonStyle(.plain)
                                }
                                .listStyle(.sidebar)
                                Divider()
                            }
                            if let sub = selectedSubproject, let buildFile = buildFile(for: sub) {
                                List {
                                    Button {
                                        selectedFilePath = buildFilePath(buildFile, subproject: sub)
                                    } label: {
                                        HStack(spacing: 6) {
                                            Image(systemName: "hammer")
                                                .foregroundColor(
                                                    selectedFilePath == buildFilePath(buildFile, subproject: sub)
                                                        ? theme.accent : theme.textSecondary
                                                )
                                                .font(.caption)
                                            Text(buildFile)
                                                .font(.callout)
                                                .foregroundColor(
                                                    selectedFilePath == buildFilePath(buildFile, subproject: sub)
                                                        ? .primary : .secondary
                                                )
                                            Spacer(minLength: 0)
                                        }
                                    }
                                    .buttonStyle(.plain)
                                }
                                .listStyle(.sidebar)
                            } else {
                                VStack(spacing: 8) {
                                    Image(systemName: "hammer")
                                        .font(.system(size: 28))
                                        .foregroundStyle(.tertiary)
                                    Text("No build file")
                                        .font(.headline)
                                        .foregroundStyle(.secondary)
                                }
                                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                                }
                            }

                        default:
                            EmptyView()
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                .frame(width: geo.size.width * 0.5)

                // ── Right pane (50%) — file content viewer or empty-project setup ──
                Rectangle()
                    .fill(theme.divider)
                    .frame(width: 1)

                if isEmptyProject {
                    setupProjectPane
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let path = selectedFilePath {
                    FileViewerPanel(filePath: path)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    VStack(spacing: 8) {
                        Image(systemName: "doc.text.magnifyingglass")
                            .font(.system(size: 32))
                            .foregroundStyle(.tertiary)
                        Text("Select a file to view its content")
                            .font(.headline)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                }
            }
        } else if let error = bridge.connectionError {
            errorView(error)
        } else {
            emptyView
        }
    }

    // MARK: - Helpers

    /// True when the opened directory has no build systems and no subprojects.
    private var isEmptyProject: Bool {
        guard let project = bridge.projectInfo else { return false }
        return project.buildSystems.isEmpty && project.subprojects.isEmpty
    }

    private func tabSystemImage(_ tab: String) -> String {
        switch tab {
        case "Sources":      return "doc.plaintext"
        case "Dependencies": return "shippingbox"
        case "Build":        return "hammer"
        default:             return "questionmark"
        }
    }

    private func subprojectTree(for project: ProjectInfo) -> FileTreeDirectory? {
        guard let root = project.fileTree else { return nil }
        guard let sub = selectedSubproject else { return nil }
        let cleanPath = sub.path.hasSuffix("/") ? String(sub.path.dropLast()) : sub.path
        // A root-level subproject (path "") covers the entire project tree.
        if cleanPath.isEmpty { return root }
        let parts = cleanPath.split(separator: "/").map(String.init)
        return findSubdir(in: root, parts: parts)
    }

    private func findSubdir(in dir: FileTreeDirectory, parts: [String]) -> FileTreeDirectory? {
        guard !parts.isEmpty else { return dir }
        let head = parts[0]
        let rest = Array(parts.dropFirst())
        for child in dir.directories where child.name == head {
            return findSubdir(in: child, parts: rest)
        }
        return nil
    }

    private func buildFile(for sub: SubprojectInfo) -> String? {
        switch sub.buildSystem {
        case "Cargo":        return "Cargo.toml"
        case "SwiftPM", "Xcode": return "Package.swift"
        case "npm", "pnpm", "yarn": return "package.json"
        case "Meson":        return "meson.build"
        default:             return nil
        }
    }

    private func buildFilePath(_ file: String, subproject: SubprojectInfo) -> String {
        let clean = subproject.path.hasSuffix("/") ? String(subproject.path.dropLast()) : subproject.path
        return clean.isEmpty ? file : "\(clean)/\(file)"
    }

    /// Fetch the target-scoped detail (deps/platform/files) when a build
    /// target is selected; clear it when the selection is removed.
    private func loadTargetDetail(name: String?) async {
        guard let name, !name.isEmpty else {
            targetDetail = nil
            return
        }
        do {
            targetDetail = try await bridge.fetchBuildTarget(name: name)
        } catch {
            // silent: log/ignore — the Sources/Build tabs fall back to project views.
            targetDetail = nil
        }
    }

    /// Short display label for a build target (e.g. `ai-trap-rock3c` → `rock3c`).
    private func shortTargetName(_ target: String) -> String {
        target.hasPrefix("ai-trap-") ? String(target.dropFirst("ai-trap-".count)) : target
    }

    // MARK: - State Views

    private var loadingView: some View {
        VStack(spacing: 16) {
            ProgressView()
                .scaleEffect(1.5)
            Text("Analyzing project...")
                .font(.headline)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func errorView(_ error: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 48))
                .foregroundStyle(.orange)
            Text("Connection Error")
                .font(.headline)
            Text(error)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button("Retry") {
                Task { await bridge.fetchProjectAnalysis() }
            }
            .buttonStyle(.borderedProminent)
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// New-project setup pane for an empty directory. The full-screen plan/
    /// execution view is rendered by ContentView (.creating state), NOT here —
    /// this view only ever shows the empty-project setup form.
    private var setupProjectPane: some View {
        VStack(spacing: 16) {
            Image(systemName: "hammer.badge.plus")
                .font(.system(size: 42))
                .foregroundStyle(.orange)
            Text("Set Up Project")
                .font(.title.weight(.semibold))
            Text("Choose Embedded or Native, then pick your structure (HAL for embedded Meson projects).")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
            Button {
                bridge.state = .creating(plan: nil, executing: false)
            } label: {
                Label("Structure Project…", systemImage: "wand.and.stars")
                    .font(.headline)
                    .padding(.horizontal, 8)
            }
            .buttonStyle(.borderedProminent)
        }
        .padding(40)
    }

    private var emptyView: some View {
        VStack(spacing: 16) {
            Image(systemName: "folder.fill")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)
            Text("No project loaded")
                .font(.headline)
                .foregroundStyle(.secondary)
            Button("Analyze Project") {
                Task { await bridge.fetchProjectAnalysis() }
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Right-pane form for an empty project: language + goal, then generate plan.
struct EmptyProjectSetupView: View {
    @Environment(SpireBridge.self) private var bridge

    @State private var goal: String = ""
    @State private var language: String = "Rust"
    @State private var isGenerating = false
    @State private var errorMessage: String?

    private let languages = ["Rust", "Swift", "Python", "JavaScript", "Go"]

    var body: some View {
        VStack(spacing: 20) {
            VStack(spacing: 6) {
                Image(systemName: "hammer.badge.plus")
                    .font(.system(size: 42))
                    .foregroundStyle(.orange)
                Text("Set Up Project")
                    .font(.title.weight(.semibold))
                Text("Describe what you want to build in this empty directory")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Project description").font(.headline)
                TextField("e.g. A CLI tool that converts CSV to JSON", text: $goal, axis: .vertical)
                    .textFieldStyle(.roundedBorder)
                    .lineLimit(6...10)
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Language").font(.headline)
                Picker("Language", selection: $language) {
                    ForEach(languages, id: \.self) { Text($0).tag($0) }
                }
                .pickerStyle(.menu)
                .frame(width: 180)
            }

            Spacer(minLength: 0)

            if let err = errorMessage {
                Text(err).font(.caption).foregroundStyle(.red).lineLimit(2)
            }

            Button { generatePlan() } label: {
                if isGenerating {
                    ProgressView().controlSize(.small)
                    Text("Generating…")
                } else {
                    Label("Generate Plan", systemImage: "sparkles")
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(isGenerating || goal.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(32)
    }

    private func generatePlan() {
        guard let root = bridge.projectRoot ?? bridge.projectInfo?.root else {
            errorMessage = "No project root available"
            return
        }
        isGenerating = true
        errorMessage = nil
        Task {
            if let plan = await bridge.generateCreationPlan(
                goal: goal,
                rootDir: root,
                language: language
            ) {
                await MainActor.run {
                    bridge.state = .creating(plan: plan, executing: false)
                    isGenerating = false
                }
            } else {
                await MainActor.run {
                    isGenerating = false
                    errorMessage = "Failed to generate plan"
                }
            }
        }
    }
}
