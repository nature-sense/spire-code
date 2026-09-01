import SwiftUI
import HighlightSwift

/// Middle-pane detail card for a selected subproject.
/// Shows build system, language, description, plus Sources / Dependencies / Build tabs.
struct SubprojectDetailCard: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let subproject: SubprojectInfo
    /// Build target selected in the graph (e.g. ai-trap-rock3c). Build status
    /// is stored PER TARGET in the graph, so the fetch must pass it.
    let buildTarget: String?
    // When a file is tapped, switch the middle pane to the file viewer.
    var onOpenFile: (String) -> Void

    /// Absolute path of this subproject on disk (projectRoot + subproject.path).
    /// Builds/lint persist graph state under the ABSOLUTE path, so graph
    /// queries must use it too — otherwise fetchBuildStatus/fetchDiagnostics
    /// would miss the stored results (key mismatch).
    private var absPath: String {
        guard let root = bridge.projectInfo?.root else { return subproject.path }
        let rootPath = root.hasSuffix("/") ? root : root + "/"
        return subproject.path.hasPrefix("/") ? subproject.path : rootPath + subproject.path
    }

    @State private var selectedTab: String = "Sources"
    /// Dependency doc viewer state: (name, version, markdown)
    @State private var docView: (String, String?, String)?

    private let tabs = ["Sources", "Dependencies", "Build"]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let docView {
                // ── Dependency documentation viewer ──
                dependencyDocView(docView)
            } else {
                // ── Normal tabs ──
                content
            }
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 0) {
            // ── Header ──
            header
            Divider()

            // ── Tab bar ──
            HStack(spacing: 0) {
                ForEach(tabs, id: \.self) { tab in
                    Button {
                        selectedTab = tab
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: tabSystemImage(tab)).font(.caption)
                            Text(tab).font(.subheadline.weight(.medium))
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
            Divider().padding(.horizontal, 8)

            // ── Tab content ──
            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    switch selectedTab {
                    case "Sources":
                        sourceFilesView
                    case "Dependencies":
                        dependenciesView
                    default:
                        buildView
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .center, spacing: 8) {
                Text(subproject.name)
                    .font(.headline)
                Text(subproject.buildSystem)
                    .font(.caption2.weight(.semibold))
                    .foregroundColor(.white)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(badgeColor)
                    .cornerRadius(4)
            }
            if !subproject.description.isEmpty {
                Text(subproject.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            HStack(spacing: 12) {
                if !subproject.language.isEmpty {
                    Label(subproject.language, systemImage: "chevron.left.forwardslash.chevron.right")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                if let files = subproject.files {
                    Text("\(files.count) files")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
        }
        .padding(8)
    }

    // MARK: - Tabs

    private var sourceFilesView: some View {
        Group {
            if let files = subproject.files, !files.isEmpty {
                ForEach(files) { file in
                    Button {
                        onOpenFile(file.path)
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: "doc.text").foregroundStyle(.secondary)
                            Text(file.path).font(.callout).foregroundStyle(.primary)
                            Spacer()
                            if !file.role.isEmpty {
                                Text(file.role)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    .buttonStyle(.plain)
                }
            } else {
                Text("No source files")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
    }

    private var dependenciesView: some View {
        Group {
            if let deps = subproject.dependencies, !deps.isEmpty {
                ForEach(deps) { dep in
                    HStack(spacing: 6) {
                        Image(systemName: "shippingbox").foregroundStyle(.secondary)
                        Text(dep.name).font(.callout)
                        Spacer()
                        Text(dep.version ?? "").font(.caption).foregroundStyle(.secondary)
                        Button {
                            Task {
                                let md = await bridge.fetchDependencyDocs(
                                    name: dep.name,
                                    version: dep.version,
                                    language: subproject.language
                                )
                                await MainActor.run {
                                    docView = (dep.name, dep.version, md ?? "No documentation available for \(dep.name) \(dep.version ?? "").")
                                }
                            }
                        } label: {
                            Image(systemName: "info.circle")
                                .foregroundStyle(theme.accent)
                        }
                        .buttonStyle(.plain)
                        .help("View documentation for \(dep.name)")
                    }
                    .padding(.vertical, 2)
                }
            } else {
                Text("No dependencies")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
    }

    private var buildView: some View {
        BuildDetailView(path: absPath, buildTarget: buildTarget, onOpenFile: onOpenFile)
    }

    /// Markdown documentation viewer with a close button.
    @ViewBuilder
    private func dependencyDocView(_ doc: (String, String?, String)) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(doc.0).font(.headline)
                    if let version = doc.1, !version.isEmpty {
                        Text(version).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Button {
                    docView = nil
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Close documentation")
            }
            .padding(8)
            Divider()
            ScrollView {
                Text(.init(doc.2))
                    .font(.body)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
            }
        }
    }

    private var badgeColor: Color {
        switch subproject.buildSystem {
        case "Cargo": return Color(red: 0.70, green: 0.25, blue: 0.15)
        case "SwiftPM", "Xcode": return .orange
        case "npm", "pnpm", "yarn": return .green
        default: return .gray
        }
    }

    private func tabSystemImage(_ tab: String) -> String {
        switch tab {
        case "Sources": return "folder"
        case "Dependencies": return "shippingbox"
        default: return "hammer"
        }
    }
}

/// Extract `path:line:col: warning|error: message` lines from compiler output.
/// Mirrors the Rust parse_clang_output heuristic: find the severity marker,
/// then rsplit the prefix on ":" to get col, line, then file.
private func parseOutputDiagnostics(_ output: String) -> [DiagnosticEntry] {
    var out: [DiagnosticEntry] = []
    for line in output.split(separator: "\n") {
        let text = line.trimmingCharacters(in: .whitespaces)
        if text.isEmpty { continue }
        // Locate the severity marker.
        guard let sevRange = text.range(of: ":\\s*(warning|error):", options: [.regularExpression, .caseInsensitive]) else { continue }
        let sevLoc = String(text[sevRange.lowerBound...])            // ":391:15: warning: msg"
        let sev = sevLoc.lowercased().contains("error") ? "error" : "warning"
        let prefix = String(text[..<sevRange.lowerBound])            // "../toolkit/.../overlay_actor.cpp:391:15"
        // Split off the ":" that begins the marker, then rsplit ": " to get
        // col, line, then the remaining path (path itself may contain ":").
        let pathPart = prefix.trimmingCharacters(in: .whitespaces)
        let segs = pathPart.split(separator: ":", omittingEmptySubsequences: false)
        guard segs.count >= 2,
              let col = Int(segs[segs.count - 1].trimmingCharacters(in: .whitespaces)),
              let line = Int(segs[segs.count - 2].trimmingCharacters(in: .whitespaces))
        else { continue }
        let file = segs[..<(segs.count - 2)].joined(separator: ":")
        // Message = after the severity word + colon.
        let msg = String(sevLoc.drop(while: { $0 != ":" }))
            .drop(while: { $0 == ":" || $0 == " " })
        let message = String(msg)
        out.append(DiagnosticEntry(severity: sev, file: file, line: line, column: col,
                                   message: message, buildType: "build", buildRunId: nil))
    }
    return out
}

/// Build detail view — shows the last build's status/duration plus a
/// structured list of errors / warnings / lint findings, all sourced from the
/// knowledge graph via `bridge.fetchDiagnostics(path:)`.
struct BuildDetailView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let path: String
    /// Build target selected in the graph — scopes the build-status fetch to
    /// that platform's stored result.
    let buildTarget: String?
    /// Called when a diagnostic's file is tapped — opens it in the middle pane.
    var onOpenFile: (String) -> Void

    @State private var diagnostics: [DiagnosticEntry] = []
    @State private var loaded = false
    @State private var loading = false
    /// Last recorded build status from the graph (nil = never built).
    @State private var buildStatus: BuildStatus?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            statusHeader
            Divider()
            diagnosticsList
        }
        .task(id: path) {
            await load()
        }
    }

    @ViewBuilder
    private var statusHeader: some View {
        if let status = buildStatus ?? bridge.projectInfo?.subprojects.first(where: { $0.path == path })?.buildStatus {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 8) {
                    Image(systemName: status.success == true ? "checkmark.circle.fill" : "xmark.circle.fill")
                        .foregroundStyle(status.success == true ? .green : .red)
                    Text(status.success == true ? "Last build succeeded" : "Last build failed")
                        .font(.callout.weight(.medium))
                    Spacer()
                }
                if let lastBuild = status.lastBuild {
                    Text(lastBuild, format: .dateTime.weekday(.abbreviated).day().month().hour().minute())
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if let dur = status.durationSecs {
                    Text(String(format: "Duration: %.2fs", dur))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        } else if loading {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Loading diagnostics…").font(.callout).foregroundStyle(.secondary)
                Spacer()
            }
        } else {
            HStack(spacing: 8) {
                Image(systemName: "clock")
                    .foregroundStyle(.secondary)
                Text("No build recorded").font(.callout).foregroundStyle(.secondary)
                Spacer()
            }
        }
    }

    @ViewBuilder
    private var buildOutputView: some View {
        if let output = buildStatus?.output, !output.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                Text("Build output").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                ScrollView {
                    Text(output)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 140)
                .padding(6)
                .background(theme.surface, in: RoundedRectangle(cornerRadius: 6))
            }
        }
    }

    @ViewBuilder
    private var diagnosticsList: some View {
        if diagnostics.isEmpty {
            if loaded {
                VStack(spacing: 4) {
                    Image(systemName: "checkmark.seal.fill")
                        .font(.title2)
                        .foregroundStyle(.green)
                    Text("No errors or warnings").font(.callout).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
            }
        } else {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(diagnostics) { diag in
                    diagnosticRow(diag)
                    if diag.id != diagnostics.last?.id {
                        Divider().padding(.leading, 24)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func diagnosticRow(_ diag: DiagnosticEntry) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: icon(for: diag.severity))
                .font(.system(size: 10))
                .foregroundStyle(color(for: diag.severity))
            VStack(alignment: .leading, spacing: 2) {
                // Message + build type tag (build/lint/fix).
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(diag.message)
                        .font(.caption2)
                        .foregroundStyle(color(for: diag.severity))
                        .textSelection(.enabled)
                        .lineLimit(3)
                    if let t = diag.buildType, !t.isEmpty {
                        Text(t)
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 4)
                            .padding(.vertical, 1)
                            .background(theme.badgeBackground)
                            .cornerRadius(3)
                    }
                }
                // File:line — clickable to open the file in the middle pane.
                if let file = diag.file, !file.isEmpty {
                    Button {
                        onOpenFile(resolve(file))
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "doc.text").font(.system(size: 7)).foregroundStyle(.secondary)
                            Text("\(file)\(diag.line.map { ":\($0)" } ?? "")")
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Open \(file)\(diag.line.map { ":\($0)" } ?? "")")
                    .padding(.leading, 4)
                }
            }
        }
        .padding(.vertical, 3)
    }

    /// Resolve a diagnostic file path against the subproject directory.
    /// Cargo emits paths relative to the crate root; SwiftPM emits absolute paths.
    private func resolve(_ file: String) -> String {
        if file.hasPrefix("/") { return file }
        let root = bridge.projectInfo?.root ?? ""
        let base = path.hasSuffix("/") ? String(path.dropLast()) : path
        let dir = base.isEmpty ? root : root + "/" + base
        return dir + "/" + file
    }

    private func icon(for severity: String) -> String {
        switch severity {
        case "error": return "xmark.circle.fill"
        case "warning": return "exclamationmark.triangle.fill"
        default: return "info.circle.fill"
        }
    }

    private func color(for severity: String) -> Color {
        switch severity {
        case "error": return .red
        case "warning": return .yellow
        default: return .secondary
        }
    }

    private func load() async {
        guard !loading else { return }
        loading = true
        defer { loading = false }
        async let status = bridge.fetchBuildStatus(path: path, target: buildTarget)
        async let result = bridge.fetchDiagnostics(path: path)
        let (fetchedStatus, fetchedDiags) = await (status, result)
        await MainActor.run {
            // Last build status from the graph (built/tested/linted).
            buildStatus = fetchedStatus
            // If the graph query returned nothing but the build output has
            // warnings/errors (e.g. ninja prints them while ingesting into the
            // graph silently failed), synthesize the list from the output text
            // so the Build tab reflects the real result.
            var diags = fetchedDiags
            if diags.isEmpty, let output = fetchedStatus?.output {
                diags = parseOutputDiagnostics(output)
            }
            // Belt and suspenders: drop any empty/info entries that may have
            // been stored by older runs before the Rust-side filter existed.
            diagnostics = diags.filter {
                !$0.message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && ($0.severity == "warning" || $0.severity == "error")
            }
            loaded = true
        }
    }
}
