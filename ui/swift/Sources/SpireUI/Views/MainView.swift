import SwiftUI
import AppKit
import os
import HighlightSwift

/// Three-pane main screen.
/// Left = project navigation (files / graph tabs), middle = selection detail,
/// right = interaction (chat / planning / analysis). Draggable vertical dividers.
struct MainView: View {
    let project: ProjectInfo
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    @State private var leftWidth: CGFloat = 0   // 0 = default 50% (left half)
    @State private var topHeight: CGFloat = 0   // 0 = default 50% (left top/bottom)
    @State private var bottomPaneFrac: CGFloat = 0.5 // default 50% split (bottom L/R)
    @State private var selectedFilePath: String?
    @State private var selectedSubproject: SubprojectInfo?
    @State private var selectedBuildTarget: String?
    /// Target-scoped detail (files/deps) fetched from the graph when a
    /// build target is selected — drives the center pane.
    @State private var targetDetail: BuildTargetDetail?
    /// Selected HAL domain (nil = the project root). Drives the Sources /
    /// Dependencies panes and the far-right Actions for HAL Meson projects.
    @State private var selectedDomain: String?
    /// Directory currently browsed via the HAL layout tree (nil = whole
    /// project). Set by the ProjectAnalysisView layout leaves; drives the
    /// Sources pane to show EXACTLY that directory's contents (toolkit/,
    /// hal/api/, hal/implementations/<plat>/, <plat>/).
    @State private var selectedLayoutDirectory: String?
    @State private var isDraggingDivider = false
    @State private var isDraggingBottom = false

    private let dividerWidth: CGFloat = 4
    private let headerHeight: CGFloat = 44

    var body: some View {
        VStack(spacing: 0) {
            // ── Full-width header: shows project / subproject (+ target) ──
            header
                .frame(maxWidth: .infinity)
                .frame(height: headerHeight)
                .background(theme.accentBackground)
            Divider()

            GeometryReader { geo in
                let totalW = geo.size.width
                let totalH = geo.size.height

                // Vertical split: Analysis (top) / Sources + Dependencies (bottom).
                let top = topHeight == 0 ? (totalH / 2) : max(140, min(totalH - 140, topHeight))
                let bottomW = totalW - dividerWidth
                let bottomLeftW = max(160, bottomW * bottomPaneFrac)

                VStack(spacing: 0) {
                    // Top: project analysis (includes the HAL "Domains" card
                    // for HAL Meson projects — same card/row style as
                    // Subprojects/Build Targets, no separate mode toggle).
                    VStack(spacing: 0) {
                        ProjectAnalysisView(
                            project: project,
                            selectedSubproject: $selectedSubproject,
                            selectedBuildTarget: $selectedBuildTarget,
                            selectedDomain: $selectedDomain,
                            selectedLayoutDirectory: $selectedLayoutDirectory
                        )
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                    .frame(height: top)

                    // Draggable horizontal divider (top/bottom).
                    divider
                        .frame(height: dividerWidth)
                        .gesture(dragVertical { delta in
                            topHeight = min(max(140, top + delta), totalH - 140)
                        })

                    // Bottom: file tree + dependencies.
                    HStack(spacing: 0) {
                        VStack(spacing: 0) {
                            paneHeader("Sources")
                            bottomFileTree
                        }
                        .frame(width: bottomLeftW)
                        .background(theme.surface)
                        divider
                            .frame(width: dividerWidth)
                            .gesture(DragGesture(minimumDistance: 0)
                                .onChanged { value in
                                    let delta = value.translation.width / max(bottomW, 1)
                                    bottomPaneFrac = min(max(0.15, bottomPaneFrac + delta), 0.85)
                                })
                        bottomDependencies
                            .frame(maxWidth: .infinity)
                            .background(theme.surface)
                    }
                }
                .frame(maxWidth: .infinity)
                .frame(maxHeight: .infinity)
            }
            .onAppear {
                // HAL projects: default to the project root (no domain selected).
                if selectedDomain == nil, let hal = project.subprojects.first(where: { $0.structure == "hal" }) {
                    selectedSubproject = hal
                }
                // Single-project cross builds have no graph nodes to click, so
                // the main (project) subproject is the default selection —
                // this makes the Dependencies pane render the project's
                // [dependencies] and the right-pane platform picker available
                // without a tap.
                if selectedSubproject == nil,
                   let main = project.subprojects.first(where: {
                       $0.kind == .project && ($0.path.isEmpty || $0.path == "/")
                   }) ?? project.subprojects.first(where: { $0.kind != .directory }) {
                    selectedSubproject = main
                }
            }
            .onChange(of: selectedBuildTarget) { _, newValue in
                Task { await loadTargetDetail(name: newValue) }
            }

        }
    }

    /// Full-width header showing the selected project / subproject / target.
    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "square.grid.2x2")
                .foregroundStyle(theme.accent)
            if let sub = selectedSubproject {
                Text(project.name)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                Image(systemName: "chevron.right")
                    .font(.caption2).foregroundStyle(.tertiary)
                Text(sub.name)
                    .font(.headline)
                if let target = selectedBuildTarget {
                    Image(systemName: "chevron.right")
                        .font(.caption2).foregroundStyle(.tertiary)
                    Text(target)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            } else {
                Text(project.name)
                    .font(.headline)
            }
            Spacer()
        }
        .padding(.horizontal, 12)
    }

    /// The HAL domain selected via the Domains card (nil = project root).
    private var selectedDomainObj: ProjectDomain? {
        guard let id = selectedDomain else { return nil }
        return project.subprojects.first { !$0.domains.isEmpty }?.domains.first { $0.id == id }
    }

    /// Resolve a domain directory path (e.g. "hal/implementations/rpi5")
    /// against the REAL project file tree. Returns nil when the path doesn't
    /// exist on disk (legacy layout, etc.).
    private func findTreeDir(_ path: String) -> FileTreeDirectory? {
        guard let root = project.fileTree else { return nil }
        let clean = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard !clean.isEmpty else { return root }
        var current = root
        for part in clean.split(separator: "/").map(String.init) {
            guard let next = current.directories.first(where: { $0.name == part }) else {
                return nil
            }
            current = next
        }
        return current
    }

    /// Recursively insert every file from a real directory node into `root`,
    /// using the file's full project-relative path (the tree's file paths are
    /// already project-relative).
    private func collectFiles(from dir: FileTreeDirectory, into root: inout FileTreeDirectory) {
        for file in dir.files {
            insertFile(file.path, role: file.role, into: &root)
        }
        for sub in dir.directories {
            collectFiles(from: sub, into: &root)
        }
    }

    /// True when `path` resolves to an individual FILE in the project tree
    /// (e.g. the `common` domain's contract headers `hal/api/*.hpp`).
    private func treeHasFile(_ path: String) -> Bool {
        guard let root = project.fileTree else { return false }
        var stack = [root]
        while let node = stack.popLast() {
            if node.files.contains(where: { $0.path == path }) { return true }
            stack.append(contentsOf: node.directories)
        }
        return false
    }

    /// Build the Sources tree for a HAL domain by resolving its entries
    /// against the REAL project file tree and inserting the files found there
    /// (app sources + hal implementations). The synthesized root uses path "."
    /// so inserted files keep their PROJECT-RELATIVE paths (rpi5/main.cpp,
    /// hal/implementations/rpi5/*.cpp) — no re-rooting under the domain name.
    /// Individual file entries (contract headers) are inserted directly.
    /// Unresolvable paths (legacy layouts) fall back to directory-name rows.
    private func domainTree(_ domain: ProjectDomain) -> FileTreeDirectory {
        var root = FileTreeDirectory(name: domain.name, path: ".", role: "")
        var anyResolved = false
        for entry in domain.files {
            if let real = findTreeDir(entry) {
                anyResolved = true
                collectFiles(from: real, into: &root)
            } else if treeHasFile(entry) {
                anyResolved = true
                insertFile(entry, role: "source", into: &root)
            }
        }
        if !anyResolved {
            for entry in domain.files {
                let parts = entry.split(separator: "/").map(String.init)
                insertDirPath(parts, fileLeaf: "", fullPath: entry, role: "source", into: &root)
            }
        }
        return root
    }

    /// Bottom-left of the left half: source file tree. Shows the whole project
    /// unless a subproject/target/domain is selected (then that slice's files).
    /// File clicks open in a floating window (never inline).
    private var bottomFileTree: some View {
        Group {
            // A HAL layout leaf is selected → browse EXACTLY that directory
            // from the real project tree (toolkit/, hal/api/,
            // hal/implementations/<plat>/, <plat>/). Domain/target selection
            // is still applied (right pane), but the Sources tree mirrors the
            // directory the user clicked.
            if let dir = selectedLayoutDirectory {
                if let tree = findTreeDir(dir) {
                    FileTreeBrowser(tree: tree, selectedFilePath: $selectedFilePath) { path in
                        openFileInPopup(path)
                    }
                } else {
                    ContentUnavailableView("Directory unavailable", systemImage: "folder",
                        description: Text("No project tree entry for \(dir)"))
                }
            } else if let domain = selectedDomainObj {
                FileTreeBrowser(tree: domainTree(domain), selectedFilePath: $selectedFilePath) { path in
                    openFileInPopup(path)
                }
            } else if selectedBuildTarget != nil, let detail = targetDetail, !detail.files.isEmpty {
                // A build target is selected → synthesize the tree from the
                // authoritative graph-backed target detail (files+deps).
                if let tree = treeFromTargetFiles(detail.files) {
                    FileTreeBrowser(tree: tree, selectedFilePath: $selectedFilePath) { path in
                        openFileInPopup(path)
                    }
                } else {
                    ContentUnavailableView("No files", systemImage: "doc",
                        description: Text("No source files for \(detail.name)"))
                }
            } else if let sub = selectedSubproject {
                // Subproject selected (no target) → reuse/synthesize its tree.
                if let tree = subprojectTree(sub) {
                    FileTreeBrowser(tree: tree, selectedFilePath: $selectedFilePath) { path in
                        openFileInPopup(path)
                    }
                } else {
                    ContentUnavailableView("No files", systemImage: "doc",
                        description: Text("No source files for \(sub.name)"))
                }
            } else if let tree = project.fileTree {
                // Project selected → complete file tree.
                FileTreeBrowser(tree: tree, selectedFilePath: $selectedFilePath) { path in
                    openFileInPopup(path)
                }
            } else {
                ContentUnavailableView("No file tree", systemImage: "tree",
                    description: Text("File tree unavailable for this selection"))
            }
        }
    }

    /// Shared header label used above each pane ("Project", "Sources",
    /// "Dependencies", "Actions").
    private func paneHeader(_ title: String) -> some View {
        VStack(spacing: 0) {
            Text(title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(theme.textSecondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 8)
                .padding(.vertical, 5)
            Divider()
                .overlay(theme.divider)
        }
    }

    /// Build a `FileTreeDirectory` from `BuildTargetFile`s (graph-backed).
    private func treeFromTargetFiles(_ files: [BuildTargetFile]) -> FileTreeDirectory? {
        guard !files.isEmpty else { return nil }
        var root = FileTreeDirectory(name: selectedSubproject?.name ?? "target", path: ".", role: "")
        for file in files {
            guard let path = file.path else { continue }
            insertFile(path, role: file.role ?? "", into: &root)
        }
        return root
    }

    /// Bottom-right of the left half: selected subproject's dependencies
    /// (target-scoped detail when a build target is selected, or the selected
    /// HAL platform domain's dependencies when a domain is selected).
    private var bottomDependencies: some View {
        Group {
            // Dependencies appear only for a PLATFORM domain (rpi5 / rock3c).
            // The project root and `common` show files without dependencies.
            let deps: [Dependency]? =
                (selectedBuildTarget != nil ? targetDetail?.dependencies.map {
                    Dependency(name: $0.name, version: $0.version)
                } : nil)
                ?? (selectedDomainObj != nil
                    ? selectedDomainObj?.dependencies.map { Dependency(name: $0.name, version: $0.version) }
                    : nil)
                ?? selectedSubproject?.dependencies
                // Single-project cross builds: the main subproject carries the
                // [dependencies]; fall back to it when nothing is selected.
                ?? project.subprojects.first { $0.kind != .directory }?.dependencies
            if let deps, !deps.isEmpty {
                VStack(alignment: .leading, spacing: 0) {
                    paneHeader("Dependencies")
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 4) {
                            ForEach(deps) { dep in
                                HStack(spacing: 6) {
                                    Image(systemName: "shippingbox")
                                        .foregroundStyle(.secondary)
                                    Text(dep.name)
                                        .font(.callout)
                                    Spacer()
                                    if let v = dep.version, !v.isEmpty {
                                        Text(v)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                .padding(.horizontal, 8)
                            }
                        }
                        .padding(.vertical, 4)
                    }
                }
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "shippingbox")
                        .font(.system(size: 28))
                        .foregroundStyle(.tertiary)
                    Text(selectedSubproject == nil
                         ? "No subproject selected"
                         : "No dependencies")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    /// Build the subproject's file tree. First try to reuse the rich project
    /// tree (which carries dirs + totals) by walking its path; if the walk
    /// fails (path mismatch), synthesize a tree from `sub.files` so the pane
    /// still shows exactly the subproject's files.
    private func subprojectTree(_ sub: SubprojectInfo) -> FileTreeDirectory? {
        // 1. Walk the project tree by the subproject's relative path.
        if let root = project.fileTree, !sub.path.isEmpty {
            let cleanPath = sub.path.hasSuffix("/") ? String(sub.path.dropLast()) : sub.path
            let parts = cleanPath.split(separator: "/").map(String.init)
            var current = root
            var matched = true
            for part in parts {
                if let next = current.directories.first(where: { $0.name == part }) {
                    current = next
                } else {
                    matched = false
                    break
                }
            }
            if matched { return current }
        }

        // 2. Fallback: synthesize from the subproject's analyzed file list.
        guard let files = sub.files, !files.isEmpty else { return nil }
        let cleanName = sub.name
        var rootDir = FileTreeDirectory(name: cleanName, path: cleanName, role: "")
        for file in files {
            insertFile(file.path, role: file.role, into: &rootDir)
        }
        return rootDir
    }

    /// Insert a single (relative) file path into a synthesized directory tree,
    /// creating directory nodes as needed. Recursive so no `&` re-binding is
    /// required (Swift forbids binding `inout` locals to array elements).
    private func insertFile(_ path: String, role: String, into dir: inout FileTreeDirectory) {
        let parts = path.split(separator: "/").map(String.init)
        guard let leaf = parts.last else { return }
        let dirParts = Array(parts.dropLast())
        insertDirPath(dirParts, fileLeaf: leaf, fullPath: path, role: role, into: &dir)
    }

    private func insertDirPath(_ dirParts: [String],
                               fileLeaf: String,
                               fullPath: String,
                               role: String,
                               into dir: inout FileTreeDirectory) {
        guard let head = dirParts.first else {
            if !dir.files.contains(where: { $0.path == fullPath }) {
                dir.files.append(FileTreeFile(
                    name: fileLeaf,
                    path: fullPath,
                    extension: (fileLeaf as NSString).pathExtension,
                    language: "",
                    size: 0,
                    linesEstimated: 0,
                    role: role
                ))
            }
            return
        }
        let rest = Array(dirParts.dropFirst())
        let base = (dir.path.isEmpty || dir.path == ".") ? "" : dir.path
        let childPath = base.isEmpty ? head : base + "/" + head
        if let idx = dir.directories.firstIndex(where: { $0.name == head }) {
            insertDirPath(rest, fileLeaf: fileLeaf, fullPath: fullPath, role: role, into: &dir.directories[idx])
        } else {
            var newDir = FileTreeDirectory(name: head, path: childPath, role: "")
            insertDirPath(rest, fileLeaf: fileLeaf, fullPath: fullPath, role: role, into: &newDir)
            dir.directories.append(newDir)
        }
    }

    private func dragVertical(_ move: @escaping (CGFloat) -> Void) -> some Gesture {
        DragGesture(minimumDistance: 1)
            .onChanged { value in
                isDraggingDivider = true
                move(value.translation.height)
            }
            .onEnded { _ in
                isDraggingDivider = false
            }
    }

    private func dragHorizontal(_ move: @escaping (CGFloat) -> Void) -> some Gesture {
        DragGesture(minimumDistance: 1)
            .onChanged { value in
                isDraggingDivider = true
                move(value.translation.width)
            }
            .onEnded { _ in
                isDraggingDivider = false
            }
    }

    /// Fetch the target-scoped detail when a target is selected so the center
    /// pane shows only that target's files + deps.
    /// Open a source file in a separate, large, NON-modal popup window so the
    /// center pane keeps showing the target's tabs instead of being replaced.
    private func openFileInPopup(_ path: String) {
        FilePortal.open(path, projectRoot: project.root, bridge: bridge)
    }

    private func loadTargetDetail(name: String?) async {
        guard let name, !name.isEmpty else {
            targetDetail = nil
            return
        }
        if let detail = try? await bridge.fetchBuildTarget(name: name) {
            targetDetail = detail
        } else {
            targetDetail = nil
        }
    }

    private var divider: some View {
        Rectangle()
            .fill(isDraggingDivider ? theme.accent : theme.divider)
            .contentShape(Rectangle())
            .onHover { hovering in
                if hovering {
                    NSCursor.resizeLeftRight.push()
                } else {
                    NSCursor.pop()
                }
            }
    }

    private func dragGesture(_ move: @escaping (CGFloat) -> Void) -> some Gesture {
        DragGesture(minimumDistance: 1)
            .onChanged { value in
                isDraggingDivider = true
                move(value.translation.width)
            }
            .onEnded { _ in
                isDraggingDivider = false
            }
    }

    private var middlePane: some View {
        Group {
            if let detail = targetDetail, selectedBuildTarget != nil {
                // A build target is selected in the graph → show ONLY the
                // target's source files + dependencies from the graph query.
                TargetScopedPanel(
                    detail: detail,
                    buildTarget: selectedBuildTarget,
                    projectRoot: project.root
                ) { filePath in
                    openFileInPopup(filePath)
                }
            } else if let sub = selectedSubproject {
                // No target selected → fall back to the subproject overview.
                SubprojectDetailCard(subproject: sub, buildTarget: selectedBuildTarget) { filePath in
                    openFileInPopup(filePath)
                }
            } else {
                ContentUnavailableView(
                    "Select something",
                    systemImage: "cursorarrow.click.2",
                    description: Text("Choose a build target in the left pane to view its files and dependencies.")
                )
            }
        }
    }
}

/// Opens source files in large, NON-modal popup windows so the center pane
/// keeps showing the target's tabs instead of being replaced by a file viewer.
/// Multiple files can be open simultaneously; each has its own resizable window.
private enum FilePortal {
    /// Strong references to open windows so they survive the `open` call.
    private static var windows: [NSWindow] = []

    @MainActor
    static func open(_ path: String, projectRoot: String?, bridge: SpireBridge) {
        // Resolve the absolute path the same way FileDetailPanel does.
        let abs: String
        if path.hasPrefix("/") {
            abs = path
        } else if let root = projectRoot {
            abs = (root as NSString).appendingPathComponent(path)
        } else {
            abs = path
        }

        let title = (abs as NSString).lastPathComponent
        // Large initial size: ~90% of the MAIN WINDOW's frame (falls back to the
        // screen's visible frame if no main window is available yet). Keeps the
        // popup proportionally scaled to the app window. NSHostingController
        // sizing can shrink the window to its SwiftUI fitting size, so we
        // enforce the size AFTER setting the content view controller.
        let baseFrame = NSApp.mainWindow?.frame
            ?? NSScreen.main?.visibleFrame
            ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let initialSize = NSSize(
            width: max(960, min(2200, baseFrame.width * 0.90)),
            height: max(640, min(1600, baseFrame.height * 0.90))
        )
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: initialSize.width, height: initialSize.height),
            styleMask: [.titled, .closable, .resizable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = title
        window.isReleasedWhenClosed = false
        window.setFrameAutosaveName("FilePopup-\(abs)")

        // Build the pane, injecting the bridge environment (a separate NSWindow
        // does NOT inherit the main window's SwiftUI environment).
        let panel = FileDetailPanel(filePath: abs, projectRoot: projectRoot) {
            window.close()
        }
        .environment(bridge)
        .frame(minWidth: 900, minHeight: 600)
        window.contentViewController = NSHostingController(rootView: panel)
        // Force the large initial size AFTER assigning the hosting controller —
        // otherwise AppKit sizes the window to the controller's preferred size
        // (which can be tiny for SwiftUI content).
        window.setContentSize(initialSize)
        // Center the popup over the main window (fall back to screen center).
        if let mainFrame = NSApp.mainWindow?.frame {
            window.setFrameOrigin(NSPoint(
                x: mainFrame.midX - initialSize.width / 2,
                y: mainFrame.midY - initialSize.height / 2
            ))
        } else {
            window.center()
        }
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        // Retain; drop the reference when the window closes.
        windows.append(window)
        NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification,
            object: window,
            queue: .main
        ) { _ in
            windows.removeAll { $0 === window }
        }
    }
}

/// Placeholder detail panel for a selected file.
private struct FileDetailPanel: View {
    @Environment(SpireBridge.self) private var bridge
    let filePath: String
    /// The open project's absolute root (passed synchronously so relative
    /// tree paths resolve even if bridge.projectRoot isn't set yet).
    let projectRoot: String?
    /// Called when the user closes the file viewer.
    var onClose: () -> Void = {}

    @State private var content: String?
    @State private var loading = false

    /// Resolve the (possibly relative) tree path against the project root.
    private var absolutePath: String {
        if filePath.hasPrefix("/") { return filePath }
        if let root = projectRoot ?? bridge.projectRoot {
            return (root as NSString).appendingPathComponent(filePath)
        }
        return filePath
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text((filePath as NSString).lastPathComponent)
                    .font(.headline)
                    .lineLimit(1)
                Spacer()
                Button(action: onClose) {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Close file viewer")
            }
            .padding(8)
            Divider()
            if loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let content, !content.isEmpty {
                ScrollView([.horizontal, .vertical]) {
                    Text(SyntaxHighlighter.highlight(content, language: SyntaxLanguage.detect(from: filePath)))
                        .font(.system(.body, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                }
            } else {
                Text("Unable to read file")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(8)
            }
        }
        .task(id: filePath) {
            loading = true
            // Tree paths are relative — resolve to an absolute path first.
            let abs = absolutePath
            NSLog("[FileDetailPanel] path=%@ abs=%@ root=%@", filePath, abs, projectRoot ?? "nil")
            if let viaBridge = await bridge.readFile(at: abs), !viaBridge.isEmpty {
                content = viaBridge
                NSLog("[FileDetailPanel] loaded via bridge (%d chars)", viaBridge.count)
            } else {
                content = try? String(contentsOfFile: abs, encoding: .utf8)
                NSLog("[FileDetailPanel] disk read result: %d chars", content?.count ?? 0)
            }
            loading = false
        }
    }
}

/// Right pane — context-aware action buttons (hardcoded workflows) with the
/// live build output shown inline beneath the button row.
struct ActionPanelView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let project: ProjectInfo
    let selectedSubproject: SubprojectInfo?
    /// The build target selected in the left-pane graph (e.g. `ai-trap-rock3c`).
    /// Passed straight to the build module — no target detection needed.
    let selectedBuildTarget: String?
    @Binding var selectedFilePath: String?
    /// Called when a build diagnostic's file is tapped — opens a popup viewer.
    var onOpenFile: ((String) -> Void)? = nil

    /// Action result toast state.
    @State private var resultText: String?
    @State private var showResult = false

    /// True while a hardcoded action is running.
    @State private var runningAction: String?

    /// The verb of the most recently completed action ("Build", "Test",
    /// "Lint", "Clean", "Fix warnings") — used by the log header once the
    /// running action is cleared.
    @State private var lastActionVerb: String = "Build"

    /// Persistent, accumulated build log across tasks for the current
    /// subproject (never auto-cleared between runs). Cleared on subproject
    /// switch or via the log's clear button.
    @State private var logHistory: [SpireBridge.BuildEventLine] = []


    /// Container view model for the build panel. Owns async build state + the
    /// single live-event consumer.
    @State private var buildViewModel: BuildPanelViewModel?
    @State private var showPlanSheet: Bool = false
    /// Text for the chat prompt input box.
    @State private var chatInput: String = ""

    /// Lazily create the build panel VM from the bridge's backend.
    private func ensureBuildViewModel() {
        if buildViewModel == nil {
            buildViewModel = BuildPanelViewModel(service: bridge.makeBuildService())
            buildViewModel?.startEventConsumer()
        }
    }

    private var contextTitle: String {
        if let sub = selectedSubproject { return sub.name }
        if let path = selectedFilePath { return (path as NSString).lastPathComponent }
        return project.name
    }

    /// Human verb for the currently running/completed action (build/lint/fix).
    private var actionVerb: String {
        switch runningAction {
        case "build_lint": return "Lint"
        case "build_fix": return "Fix warnings"
        case "build_clean": return "Clean"
        case "build_test": return "Test"
        case "build_build": return "Build"
        default: return "Build"
        }
    }

    private var contextSubtitle: String? {
        if let sub = selectedSubproject { return sub.buildSystem }
        if let _ = selectedFilePath { return "File" }
        return nil
    }

    private var language: String {
        if let sub = selectedSubproject {
            // Normalize the analyzer's language label (e.g. "🐍 Python", "🦀 Rust")
            // so Python/other runtimes get the correct language passed to tools.
            let raw = sub.language
            if raw.contains("Python") { return "Python" }
            if raw.contains("Rust") { return "Rust" }
            if raw.contains("Swift") { return "Swift" }
            if raw.contains("JavaScript") || raw.contains("Node") { return "JavaScript" }
            if raw.contains("Go") { return "Go" }
            if raw.contains("Java") { return "Java" }
            if raw.contains("Ruby") { return "Ruby" }
            return sub.buildSystem
        }
        return "Rust"
    }

    /// The selected HAL domain's kind ("platform" = buildable, "common" = shared).
    /// Only HAL Meson projects have domains; other projects have none.
    private var selectedDomainKind: String? {
        guard let id = bridge.selectedDomain,
              let hal = project.subprojects.first(where: { $0.structure == "hal" }) else {
            return nil
        }
        return hal.domains.first { $0.id == id }?.kind
    }

    /// The selected domain's PLATFORM ID (its `name` — rpi5/rock3c). The core
    /// keys HAL coverage by platform id; the Swift `id` is the synthetic
    /// "domain-platform-rpi5" and must NOT be used for these lookups.
    private var selectedDomainName: String? {
        guard let id = bridge.selectedDomain,
              let hal = project.subprojects.first(where: { $0.structure == "hal" }) else {
            return nil
        }
        return hal.domains.first { $0.id == id }?.name
    }

    // ── Semantic one-shot generation (Fix → plan → approval → apply) ──
    /// Proposed LLM module pair awaiting approval in the viewer.
    @State private var generatedPlan: HalGenerateImplPlan?
    /// True while the semantic plan is being computed / the pair applied.
    @State private var generateBusy = false
    /// Result/error message from the one-shot generation.
    @State private var generateMessage: String?

    var body: some View {
        VStack(spacing: 0) {
            // ── Pane header ──
            VStack(spacing: 0) {
                Text("Actions")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(theme.textSecondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                Divider()
                    .overlay(theme.divider)
            }

            // ── Action buttons ──
            // Empty project (only .spire metadata): show the scaffolding
            // wizard so the user can create the initial build structure.
            if project.isEmpty {
                // Empty project → open the step-wise wizard (Embedded/Native
                // + HAL) instead of the legacy single-form.
                Button {
                    bridge.state = .creating(plan: nil, executing: false)
                } label: {
                    Label("Structure Project…", systemImage: "wand.and.stars")
                        .font(.headline)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 4)
                }
                .buttonStyle(.borderedProminent)
                .padding(8)
            } else if selectedSubproject != nil && selectedDomainKind != "common" {
                // Build/Test/Lint are shown only for a buildable selection:
                // a subproject, or a HAL platform domain. Selecting `common`
                // (or clearing the domain → project root) shows project-wide
                // actions instead — no per-target build buttons. While the
                // hal/api contracts fail the format lint, implementation
                // actions are blocked in favor of the correction plan.
                if halImplementationActionsBlocked {
                    halContractBlockedView
                } else {
                    subprojectActions
                }
            } else if selectedFilePath != nil {
                fileActions
            } else {
                projectActions
            }
            Divider()

            // ── Chat input box ──
            chatInputBox

            Divider()

            // ── Collapsible task list (plan steps) ──
            if bridge.activePlan != nil {
                taskListView
            }

            // ── Live build output (inline, directly below the button row) ──
            let vm = buildViewModel
            if (vm == nil && (!bridge.buildEvents.isEmpty || bridge.buildRunning))
                || (vm != nil && (!vm!.liveEvents.isEmpty || vm!.isRunning))
                || !logHistory.isEmpty {
                buildLogView
            }
            if showPlanSheet {
                PlanSheetView(project: project, selectedSubproject: selectedSubproject)
            }
            Spacer(minLength: 0)
        }
        .sheet(item: $generatedPlan) { plan in
            HALFillFileViewer(item: generatedFillItem(for: plan)) {
                applyGeneratedPlan(plan)
            } onReject: {
                generatedPlan = nil
            }
            .environment(bridge)
            .environment(theme)
        }
        .onChange(of: selectedSubproject?.id) { _, _ in
            // New subproject selected — clear stale build output and state so
            // the right pane starts fresh for the new target.
            resultText = nil
            showResult = false
            runningAction = nil
            logHistory = []
            buildViewModel?.liveEvents = []
            bridge.buildEvents = []
        }
    }

    // MARK: - Chat input box

    /// Prompt input row: TextField + Send, wired to the bridge chat.
    private var chatInputBox: some View {
        HStack(spacing: 6) {
            TextField("Send a prompt to the assistant…", text: $chatInput)
                .textFieldStyle(.roundedBorder)
                .onSubmit { sendChat() }
            Button {
                sendChat()
            } label: {
                Image(systemName: "paperplane.fill")
            }
            .buttonStyle(.borderedProminent)
            .disabled(chatInput.trimmingCharacters(in: .whitespaces).isEmpty || bridge.isProcessing)
        }
        .padding(8)
    }

    private func sendChat() {
        let text = chatInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        chatInput = ""
        Task { await bridge.sendChatMessage(text) }
    }

    // MARK: - Collapsible task list

    /// Collapsible plan-step list. Collapsed when there is no active plan.
    private var taskListView: some View {
        DisclosureGroup {
            if let plan = bridge.activePlan {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(plan.steps.enumerated()), id: \.element.id) { _, step in
                        HStack(spacing: 6) {
                            Image(systemName: statusIcon(step.status))
                                .foregroundStyle(statusColor(step.status))
                            Text("\(step.order). \(step.description)")
                                .font(.callout)
                                .lineLimit(2)
                                .textSelection(.enabled)
                        }
                        .padding(.vertical, 1)
                    }
                }
                .padding(.vertical, 4)
            }
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "checklist")
                    .foregroundStyle(theme.accent)
                Text("Tasks")
                    .font(.subheadline.weight(.semibold))
                Spacer()
                if let plan = bridge.activePlan {
                    Text("\(plan.completedSteps)/\(plan.totalSteps)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .contentShape(Rectangle())
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
    }

    private func statusIcon(_ status: String) -> String {
        switch status {
        case "completed": return "checkmark.circle.fill"
        case "failed": return "xmark.circle.fill"
        case "running", "executing": return "play.circle.fill"
        default: return "circle"
        }
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "completed": return .green
        case "failed": return .red
        case "running", "executing": return .orange
        default: return .secondary
        }
    }

    // MARK: - Live build log

    private var buildLogView: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                if buildViewModel?.isRunning ?? bridge.buildRunning {
                    ProgressView().controlSize(.small)
                    Text("\(actionVerb)…").font(.caption.weight(.semibold))
                } else {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                    Text("\(lastActionVerb) complete").font(.caption.weight(.semibold))
                }
                Spacer()
                Button {
                    logHistory = []
                    buildViewModel?.liveEvents = []
                    bridge.buildEvents = []
                } label: {
                    Image(systemName: "trash")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Clear build log")
            }
            ScrollViewReader { proxy in
                ScrollView(.vertical, showsIndicators: true) {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        // Persistent historical log + live streaming lines for
                        // the currently running task.
                        let combined = logHistory + (buildViewModel?.liveEvents ?? [])
                        ForEach(Array(combined.enumerated()), id: \.element.id) { _, ev in
                            buildEventRow(ev)
                        }
                    }
                    .padding(4)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .onChange(of: bridge.buildEvents.count) { _, _ in
                    if let last = bridge.buildEvents.last {
                        // Instant scroll — animating every line blocks the main thread.
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
                .onChange(of: buildViewModel?.liveEvents.count ?? 0) { _, _ in
                    // Incremental streaming: scroll on every batch of live events
                    // delivered while the tool runs (not just the final result).
                    if let last = (buildViewModel?.liveEvents ?? bridge.buildEvents).last {
                        proxy.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }
        }
        .padding(8)
        .background(theme.surface, in: RoundedRectangle(cornerRadius: 8))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(theme.accent.opacity(0.45), lineWidth: 1)
        )
        .padding(8)
    }

    @ViewBuilder
    private func buildEventRow(_ ev: SpireBridge.BuildEventLine) -> some View {
        switch ev.level {
        case "error":
            structuredBlockRow(ev, icon: "xmark.circle.fill", color: .red)
        case "warning":
            structuredBlockRow(ev, icon: "exclamationmark.triangle.fill", color: .yellow)
        case "compiling":
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Image(systemName: "hammer.fill").font(.system(size: 8)).foregroundStyle(.secondary)
                Text(ev.line).font(.caption2.monospaced()).foregroundStyle(.primary).textSelection(.enabled)
            }
        case "finished":
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Image(systemName: "checkmark.circle.fill").font(.system(size: 8)).foregroundStyle(.green)
                Text(ev.line).font(.caption2).foregroundStyle(.green).textSelection(.enabled)
            }
        default:
            Text(ev.line).font(.caption2.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
        }
    }

    /// Renders a warning/error as a compact structured row: icon + message,
    /// with a file:line location when available. The full raw block (code
    /// context) is shown indented beneath for detail.
    private func structuredBlockRow(_ ev: SpireBridge.BuildEventLine,
                                    icon: String, color: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline, spacing: 4) {
                Image(systemName: icon).font(.system(size: 8)).foregroundStyle(color)
                // Compact: show the extracted message; fall back to raw line.
                Text(ev.message ?? ev.line)
                    .font(.caption2)
                    .foregroundStyle(color)
                    .textSelection(.enabled)
                    .lineLimit(3)
            }
            if let file = ev.file {
                Button {
                    // Open the warning's file in a large non-modal popup window.
                    // Path from cargo is relative to the subproject; resolve
                    // against the project root.
                    let root = project.root.hasSuffix("/") ? String(project.root.dropLast()) : project.root
                    let abs = file.hasPrefix("/") ? file : root + "/" + file
                    onOpenFile?(abs)
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "doc.text").font(.system(size: 7)).foregroundStyle(.secondary)
                        Text("\(file)\(ev.lineNumber.map { ":\($0)" } ?? "")")
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Open \(file)\(ev.lineNumber.map { ":\($0)" } ?? "")")
                .padding(.leading, 12)
            }
        }
    }

    // MARK: - Contextual action groups

    private var projectActions: some View {
        HStack(spacing: 6) {
                planButton

            actionButton("Refresh", systemImage: "arrow.clockwise", tool: "project_refresh")
            actionButton("Analyze", systemImage: "magnifyingglass", tool: "build_analyze")
        }
        .padding(8)
    }

    /// True when the current selection targets a HAL implementation (a
    /// platform domain or a platform build target) but the hal/api contracts
    /// fail the format lint — implementation actions must be blocked until
    /// the contracts are fixed.
    private var halImplementationActionsBlocked: Bool {
        guard !bridge.halContractsValid else { return false }
        if selectedDomainKind == "platform" { return true }
        if selectedBuildTarget != nil { return true }
        return false
    }

    /// Blocking notice replacing implementation actions while the HAL
    /// contracts are invalid. Points the user back to the contract
    /// correction plan (the "Validate Contract Format" card) instead of
    /// letting them build/lint/fix/fill implementations against broken
    /// contracts.
    private var halContractBlockedView: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "exclamationmark.octagon.fill")
                    .foregroundStyle(.red)
                Text("HAL contract invalid")
                    .font(.callout.weight(.semibold))
                    .foregroundStyle(.red)
            }
            Text("\(bridge.halContractIssueCount) contract issue\(bridge.halContractIssueCount == 1 ? "" : "s") must be resolved before this HAL implementation can be built, linted, or modified.")
                .font(.caption)
                .foregroundStyle(.secondary)
            Button {
                bridge.showHalContractLint = true
            } label: {
                actionLabel("Review contract issues", systemImage: "checkmark.seal")
            }
            .buttonStyle(.plain)
        }
        .padding(8)
    }

    private var subprojectActions: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                actionButton("Build", systemImage: "hammer.fill", tool: "build_build")
                actionButton("Test", systemImage: "checkmark.circle", tool: "build_test")
                actionButton("Clean", systemImage: "trash", tool: "build_clean")
                actionButton("Lint", systemImage: "exclamationmark.triangle", tool: "build_lint")
                planButton

            }
            HStack(spacing: 6) {
                actionButton("Fix Warnings", systemImage: "wrench.and.screwdriver", tool: "build_fix")
            }

            // Full per-interface HAL analysis for the selected platform:
            // every contract interface, with its implementation status
            // (Implemented / Partial / Missing) and the missing/drifted
            // functions for that interface.
            if selectedDomainKind == "platform" {
                halInterfacesCard
            }
        }
        .padding(8)
    }

    private var fileActions: some View {
        HStack(spacing: 6) {
            Button {
                if let path = selectedFilePath {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(path, forType: .string)
                    flashResult("Copied \(path)")
                }
            } label: {
                actionLabel("Copy Path", systemImage: "doc.on.doc")
            }
            .buttonStyle(.plain)
        }
        .padding(8)
    }

    // MARK: - HAL interface analysis

    /// Per-interface status rows for the selected platform. Iterates the
    /// contract-defined interface set (`bridge.halInterfaces`) and shows each
    /// interface's implementation state from the AST coverage
    /// (`bridge.halFunctionGaps`), including the concrete missing/drifted
    /// function names.
    private var halInterfacesCard: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Image(systemName: "cpu")
                    .foregroundStyle(theme.accent)
                Text("HAL Interfaces — \(selectedDomainName ?? "platform")")
                    .font(.callout.weight(.semibold))
                Spacer()
                Button {
                    Task {
                        if let root = project.root as String? {
                            await bridge.refreshHalData(root: root)
                        }
                    }
                } label: {
                    Image(systemName: "arrow.clockwise")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .help("Refresh HAL analysis")
            }

            if let generateMessage {
                Label(generateMessage, systemImage: generateMessage.hasPrefix("✅")
                    ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(generateMessage.hasPrefix("✅") ? .green : .orange)
                    .textSelection(.enabled)
            }

            let gaps = bridge.halFunctionGaps[selectedDomainName ?? ""] ?? [:]
            let ifaces = bridge.halInterfaces.isEmpty
                ? Array(gaps.keys).sorted()
                : bridge.halInterfaces.sorted()

            ForEach(ifaces, id: \.self) { iface in
                let gapsForIface = gaps[iface]
                // Implemented: coverage entry exists and is complete.
                // Partial: an impl file exists but methods are missing/drifted.
                // Missing: no coverage entry at all (no impl file for this
                // interface on this platform).
                let implemented = gapsForIface?.implemented ?? false
                let hasEntry = gapsForIface != nil
                let missing = gapsForIface?.missing ?? []
                let drifted = gapsForIface?.drifted ?? []

                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 5) {
                        if implemented {
                            Image(systemName: "checkmark.circle.fill")
                                .foregroundStyle(.green)
                            Text(iface)
                                .font(.system(.callout, design: .monospaced))
                            Text("Implemented")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(.green)
                        } else if !hasEntry {
                            Image(systemName: "xmark.circle.fill")
                                .foregroundStyle(.red)
                            Text(iface)
                                .font(.system(.callout, design: .monospaced))
                            Text("Missing")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(.red)
                        } else {
                            Image(systemName: "exclamationmark.triangle.fill")
                                .foregroundStyle(.orange)
                            Text(iface)
                                .font(.system(.callout, design: .monospaced))
                            Text("Partial — \(missing.count) missing\(drifted.isEmpty ? "" : " · \(drifted.count) drifted")")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(.orange)
                        }
                        Spacer()
                        // One-shot semantic fix: plan → approval viewer → apply.
                        // Green rows (implemented) need no fix.
                        if !implemented {
                            Button("Fix") {
                                fixHALModule(iface)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.mini)
                            .disabled(generateBusy)
                        }
                    }
                    // Concrete per-interface function detail.
                    if !missing.isEmpty {
                        Text("Missing: " + missing.joined(separator: ", "))
                            .font(.caption2.monospaced())
                            .foregroundStyle(.orange)
                            .textSelection(.enabled)
                            .padding(.leading, 20)
                    }
                    if !drifted.isEmpty {
                        Text("Drifted: " + drifted.joined(separator: "; "))
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                            .lineLimit(3)
                            .padding(.leading, 20)
                    }
                }
                .padding(.vertical, 4)
                .padding(.horizontal, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(
                    RoundedRectangle(cornerRadius: 6)
                        .fill(
                            implemented ? Color.green.opacity(0.08)
                                : (!hasEntry ? Color.red.opacity(0.08)
                                    : Color.orange.opacity(0.08))
                        )
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(
                            implemented ? Color.green.opacity(0.5)
                                : (!hasEntry ? Color.red.opacity(0.5)
                                    : Color.orange.opacity(0.5)),
                            lineWidth: 0.5
                        )
                )
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 0.5))
    }

    // MARK: - HAL semantic one-shot generation (Fix → plan → approval → apply)

    /// One-shot semantic fix for ONE module: run the LLM plan (no writes), then
    /// present the proposed module pair in the approval viewer. Surfaces the
    /// real backend error (truncation, contract gate, LLM config) if the plan
    /// can't be produced.
    private func fixHALModule(_ iface: String) {
        guard let platform = selectedDomainName, let root = project.root as String? else { return }
        generateBusy = true
        generateMessage = nil
        Task {
            let (plan, err) = await bridge.halGenerateImplPlan(root: root, interface: iface, platform: platform, libraryHints: nil)
            await MainActor.run {
                if let plan {
                    generatedPlan = plan
                } else {
                    generateMessage = "⚠️ " + (err ?? "Could not generate a plan for \(iface).")
                }
                generateBusy = false
            }
        }
    }

    /// Build a `HalFillItem` from an LLM plan so the existing pair viewer can
    /// render the proposed `.hpp` + `.cpp` as two tabs.
    private func generatedFillItem(for plan: HalGenerateImplPlan) -> HalFillItem {
        HalFillItem(
            platform: plan.platform,
            interface: plan.interface,
            kind: "none",
            action: "generate",
            create_file: plan.cppPath,
            missing_sigs: [],
            content: plan.source,
            declaration_path: plan.hppPath,
            declaration_content: plan.header
        )
    }

    /// Write an APPROVED module pair (`hal_generate_impl_apply`) and refresh
    /// both panes so the module flips to green.
    private func applyGeneratedPlan(_ plan: HalGenerateImplPlan) {
        guard let platform = selectedDomainName, let root = project.root as String? else { return }
        generateBusy = true
        generateMessage = nil
        generatedPlan = nil
        Task {
            let (result, err) = await bridge.halGenerateImplApply(root: root, interface: plan.interface, platform: platform, plan: plan)
            await MainActor.run {
                if let err {
                    generateMessage = "⚠️ Apply failed: \(err)"
                } else if let result {
                    generateMessage = "✅ Wrote \(result.written.count) file(s): " +
                        result.written.map { ($0 as NSString).lastPathComponent }.joined(separator: ", ") +
                        (result.gateStatus.isEmpty ? "" : " · gate: \(result.gateStatus)")
                } else {
                    generateMessage = "⚠️ Apply returned no result"
                }
                generateBusy = false
            }
            if err == nil {
                await bridge.refreshHalData(root: root)
                await bridge.fetchProjectAnalysis(projectRoot: root)
            }
        }
    }

    // MARK: - Helpers

    private func actionButton(_ title: String, systemImage: String, tool: String) -> some View {
        Button {
            runTool(tool)
        } label: {
            actionLabel(title, systemImage: systemImage)
        }
        .buttonStyle(.plain)
        .disabled(runningAction != nil)
    }

    private var planButton: some View {
        Button {
            showPlanSheet = true
        } label: {
            actionLabel("Plan", systemImage: "map.fill")
        }
        .buttonStyle(.plain)
        .help("Generate an LLM modification plan")
    }

    private func actionLabel(_ title: String, systemImage: String) -> some View {
        Label(title, systemImage: systemImage)
            .font(.caption.weight(.medium))
            .foregroundStyle(theme.textPrimary)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(theme.buttonBackground)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(theme.border, lineWidth: 0.5)
            )
    }

    private func runTool(_ tool: String) {
        guard let sub = selectedSubproject else { return }

        // Resolve the subproject directory to an absolute path.
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

        // DEBUG: log which subproject the tool is targeting.
        Logger(subsystem: "spire", category: "runTool").info("tool=\(tool) sub=\(sub.name) selectedTarget=\(selectedBuildTarget ?? "nil") path=\(sub.path) abs=\(absPath)")
        // Ensure the single BuildService consumer exists. It is idempotent —
        // exactly ONE waiter on the Rust Notify, so notifications can't be stolen.
        ensureBuildViewModel()
        guard let vm = buildViewModel else { return }
        runningAction = tool
        flashResult("\(tool) started\n\(absPath)")
        // Platform resolution: when a HAL platform domain is selected, build
        // that platform (selects build-<platform> Meson dir); otherwise fall
        // back to the first platform target when the subproject is multi-target.
        let platform: String? = {
            if let domain = selectedDomainName, selectedDomainKind == "platform" {
                return domain
            }
            let targets = sub.platformTargets
            return targets.count > 1 ? targets.first : nil
        }()
        Task {
            await vm.runTool(tool, path: absPath, language: language, package: sub.name, platform: platform, target: selectedBuildTarget)
            await MainActor.run {
                // Capture the verb BEFORE clearing the running flag —
                // actionVerb falls back to "Build" once runningAction is nil.
                let verb = actionVerb
                runningAction = nil
                lastActionVerb = verb
                if let result = vm.state.value {
                    // Populate the build log with ALL collected lines from the build.
                    // For Meson lint/analyze the diagnostics live in `output`
                    // (not the streaming events), so materialize them as lines
                    // here — giving a persistent, visible build-log entry.
                    var events = result.buildEvents
                    for line in result.output.split(separator: "\n") {
                        let trimmed = line.trimmingCharacters(in: .whitespaces)
                        if trimmed.isEmpty { continue }
                        events.append(SpireBridge.BuildEventLine(
                            line: String(line),
                            level: result.success ? "info" : "error",
                            target: nil
                        ))
                    }
                    // Append this task's output to the persistent log with a
                    // timestamp separator — previous runs stay visible above.
                    let df = DateFormatter()
                    df.dateFormat = "HH:mm:ss"
                    let stamp = df.string(from: Date())
                    let statusLabel = result.success ? "✅ succeeded" : "❌ failed"
                    logHistory.append(SpireBridge.BuildEventLine(
                        line: "──── \(verb) \(stamp) \(statusLabel) ────",
                        level: "finished",
                        target: nil
                    ))
                    logHistory.append(contentsOf: events)
                    buildViewModel?.liveEvents = []
                    bridge.buildEvents = logHistory
                    // Surface the tool's actual output in the status banner so
                    // lint/fix results are immediately visible, not buried.
                    let preview = result.output.split(separator: "\n").prefix(25).joined(separator: "\n")
                    flashResult((result.success ? "✅ \(verb) succeeded" : "❌ \(verb) failed") + "\n\n" + preview)
                    // Also push the result into chat so the user can review full output,
                    let msg = ChatMessage(
                        id: UUID().uuidString,
                        role: .system,
                        content: (result.success ? "✅ \(verb) succeeded" : "❌ \(verb) failed") + "\n\n```\n" + result.output + "\n```",
                        timestamp: Date()
                    )
                    bridge.messages.append(msg)
                } else {
                    flashResult("No response from backend")
                }
            }
        }
    }

    private func flashResult(_ text: String) {
        resultText = text
        showResult = true
        // Auto-dismiss after a few seconds.
        DispatchQueue.main.asyncAfter(deadline: .now() + 4) {
            withAnimation {
                showResult = false
            }
        }
    }
}

/// Center-pane panel showing ONLY the selected build target's source files,
/// dependencies (with version + doc link), and per-platform build status.
/// Same 3-tab layout as `SubprojectDetailCard`: Sources / Dependencies / Builds.
private struct TargetScopedPanel: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let detail: BuildTargetDetail
    /// The selected build target (e.g. ai-trap-rock3c) — scopes the Builds
    /// tab's per-platform status fetch.
    let buildTarget: String?
    /// Project root used to resolve the build path for status queries.
    let projectRoot: String
    /// Called when the user clicks a file to open it.
    var onOpenFile: (String) -> Void

    @State private var selectedTab: String = "Sources"
    /// Dependency doc viewer state: (name, version, markdown)
    @State private var docView: (String, String?, String)?

    private let tabs = ["Sources", "Dependencies", "Builds"]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let docView {
                // ── Dependency documentation viewer ──
                dependencyDocView(docView)
            } else {
                // ── Target header ──
                targetHeader
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
                            sourcesTab
                        case "Dependencies":
                            dependenciesTab
                        default:
                            buildsTab
                        }
                    }
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - Header

    private var targetHeader: some View {
        HStack(spacing: 6) {
            Image(systemName: "gearshape.fill")
                .foregroundStyle(.orange)
            Text(detail.name)
                .font(.headline)
                .lineLimit(1)
            if let kind = detail.kind.isEmpty ? nil : detail.kind {
                Text(kind)
                    .font(.caption2.weight(.semibold))
                    .foregroundColor(.white)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Color.orange)
                    .cornerRadius(4)
            }
            Spacer()
        }
        .padding(8)
    }

    // MARK: - Sources tab

    private var sourcesTab: some View {
        Group {
            if detail.files.isEmpty {
                Text("No source files found for this target")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(detail.files) { file in
                    if let path = file.path {
                        Button {
                            onOpenFile(path)
                        } label: {
                            HStack(spacing: 6) {
                                Image(systemName: "doc.text")
                                    .foregroundStyle(.secondary)
                                Text(path)
                                    .font(.callout)
                                    .foregroundStyle(.primary)
                                Spacer()
                                if let lines = file.lines {
                                    Text("\(lines) lines")
                                        .font(.caption2)
                                        .foregroundStyle(.tertiary)
                                }
                            }
                            .padding(.vertical, 2)
                        }
                        .buttonStyle(.plain)
                    }
                }
            }
        }
    }

    // MARK: - Dependencies tab (version + doc link)

    private var dependenciesTab: some View {
        Group {
            if detail.dependencies.isEmpty {
                Text("No dependencies")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(detail.dependencies) { dep in
                    HStack(spacing: 6) {
                        Image(systemName: "shippingbox")
                            .foregroundStyle(.secondary)
                        Text(dep.name)
                            .font(.callout)
                        Spacer()
                        if let version = dep.version, !version.isEmpty {
                            Text(version)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Button {
                            Task {
                                let md = await bridge.fetchDependencyDocs(
                                    name: dep.name,
                                    version: dep.version,
                                    language: "C++"
                                )
                                await MainActor.run {
                                    docView = (dep.name, dep.version,
                                               md ?? "No documentation available for \(dep.name) \(dep.version ?? "").")
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
            }
        }
    }

    // MARK: - Builds tab (per-platform status)

    private var buildsTab: some View {
        BuildDetailView(path: projectRoot, buildTarget: buildTarget, onOpenFile: onOpenFile)
    }

    // MARK: - Dependency doc viewer

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

    private func tabSystemImage(_ tab: String) -> String {
        switch tab {
        case "Sources": return "folder"
        case "Dependencies": return "shippingbox"
        default: return "hammer"
        }
    }
}
