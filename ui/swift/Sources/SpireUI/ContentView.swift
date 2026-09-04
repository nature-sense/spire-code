import SwiftUI
import AppKit

/// Root content view: a CONSTANT window shell. Only the pane *content* changes
/// with state — the shell (icon sidebar, dividers, right action pane, bottom
/// status bar) always stays the same.
///
///   ┌──────┬────────────────┬──────┬──────────────────────┐
///   │ icons│  workspace     │ ▍ |  │ actions (ContextPane) │
///   └──────┴────────────────┴──────┴──────────────────────┘
///   │ status bar                        │
///
/// Left pane content is a pure function of `bridge.state` + `bridge.currentMode`.
struct ContentView: View {
    /// The bridge is owned by ContentView as @State so SwiftUI's observation
    /// wiring is guaranteed. This is the SAME singleton instance that menu
    /// commands reference, and also the one injected into child views via .environment.
    @State private var bridge = SpireBridge.shared
    @Environment(AppTheme.self) private var theme

    /// Left-pane fraction of the pane AREA (after the icon sidebar),
    /// anchored for smooth dragging. Default 0.5 = a true 50:50 split.
    @State private var leftFrac: CGFloat = 0.5
    /// The fraction committed when a drag ends; drags are offsets from this.
    @State private var committedLeftFrac: CGFloat = 0.5

    private let dividerWidth: CGFloat = 4

    var body: some View {
        VStack(spacing: 0) {
            GeometryReader { geo in
                let totalW = geo.size.width
                // Pane AREA = window minus the icon sidebar + its divider.
                let sidebar = 48.0 + 1.0
                let available = max(200, totalW - sidebar)
                let leftW = max(320, min(available - 340, available * leftFrac))
                let rightW = max(340, available - leftW - dividerWidth)

                HStack(spacing: 0) {
                    // ── Icon sidebar (constant shell) ──
                    iconSidebar
                    Divider().overlay(theme.divider)

                    // ── Left pane: project details / workspace (state-driven) ──
                    workspacePane
                        .frame(width: leftW)
                        .frame(maxHeight: .infinity)

                    // Draggable vertical divider.
                    divider
                        .frame(width: dividerWidth)
                        .gesture(dragDivider(totalW: totalW))

                    // ── Right pane: contextual actions (state-driven) ──
                    ContextActionPane(
                        selectedSubproject: bridge.selectedSubproject,
                        selectedBuildTarget: bridge.selectedBuildTarget
                    )
                        .frame(width: rightW)
                        .frame(maxHeight: .infinity)
                }
            }

            // ── Bottom status bar (constant shell) ──
            Divider().overlay(theme.divider)
            statusBar
        }
        .environment(bridge)
        .background(theme.background)
        .onAppear {
            Task { await bridge.fetchLlmConfig() }
        }
        // When the main window is closed (Cmd-W), reset the shared singleton
        // so a subsequent "New Window" (Cmd-N) does NOT reopen a stale project.
        .onDisappear {
            bridge.closeProject()
        }
        .task {
            // Verify the Rust core is reachable on launch. If the dylib
            // is missing or the core can't respond, surface an actionable
            // error instead of a generic failure.
            await bridge.checkConnection()

            // Push-driven UI refresh: the Rust file-watcher actor pushes
            // file-change events through the FFI; each arrives here and we
            // refresh the observable project state (no polling).
            for await event in bridge.eventStream() {
                // Directory tree updates are pure structure — patch in-place,
                // no analyzer round-trip. Re-assign the .idle payload to
                // trigger @Observable re-render.
                if case .idle(var info) = bridge.state {
                    info.apply(event: event)
                    bridge.state = .idle(info)
                }
            }
        }
        // ── macOS menu-bar commands ──
        // MenuCommand notifications are observed by SpireBridge for the entire
        // app lifetime, so they work even when no window is displayed. Here we
        // only present the Settings sheet bound to the bridge state set by the
        // menu-bar command.
        .sheet(isPresented: Binding(
            get: { bridge.showSettings },
            set: { bridge.showSettings = $0 }
        )) {
            LLMSettingsView()
                .environment(bridge)
        }
        // Open-project AppSpec design session (scaffold-first: the design
        // persists into the project graph of the already-open project).
        .sheet(isPresented: Binding(
            get: { bridge.showSpecDesign },
            set: { bridge.showSpecDesign = $0 }
        )) {
            specDesignSheet
        }
    }

    /// The design sheet content; empty when no project is open.
    @ViewBuilder
    private var specDesignSheet: some View {
        let name = bridge.designProjectName
        if name.isEmpty {
            EmptyView()
        } else {
            SpecDesignView(
                projectName: name,
                goal: "",
                onDecided: { spec in
                    bridge.showSpecDesign = false
                    Task { await bridge.runSpecDesignCodegen(spec: spec) }
                }
            )
            .environment(bridge)
            .environment(theme)
        }
    }

    // MARK: - Left pane: state-driven content

    @ViewBuilder
    private var workspacePane: some View {
        switch bridge.state {
        case .unconnected:
            emptyDetailsPane
        case .opening:
            progressPane("Opening project…")
        case .creating, .scaffolding, .filling:
            progressPane("Setting up project…")
        case .error(let message):
            errorPane(message)
        case .idle(let project):
            if project.isEmpty {
                emptyDetailsPane
            } else {
                // Mode switch — still driven by state, no ad-hoc flags.
                switch bridge.currentMode {
                case .explorer:
                    FileExplorerView(project: project)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .tools:
                    ToolsOverviewView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .project, .planning:
                    // The full project workspace: Analysis (top-left of the
                    // left half) + Sources (bottom-left) + Dependencies
                    // (bottom-right), with its draggable splitters.
                    MainView(project: project)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        }
    }

    // MARK: - Placeholders

    private var emptyDetailsPane: some View {
        VStack(spacing: 12) {
            Image(systemName: "hammer")
                .font(.system(size: 44))
                .foregroundStyle(theme.divider)
            Text("No project loaded")
                .font(.title3.weight(.semibold))
                .foregroundStyle(theme.textSecondary)
            Text("Open or create a project from the Actions pane.")
                .font(.caption)
                .foregroundStyle(theme.textTertiary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
    }

    private func progressPane(_ text: String) -> some View {
        VStack(spacing: 12) {
            ProgressView()
                .scaleEffect(1.2)
            Text(text)
                .font(.subheadline)
                .foregroundStyle(theme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
    }

    private func errorPane(_ message: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 36))
                .foregroundStyle(.orange)
            Text("Connection Error")
                .font(.headline)
            Text(message)
                .font(.caption)
                .foregroundStyle(theme.textSecondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 20)
            Button("Retry") {
                Task { await bridge.checkConnection() }
            }
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
    }

    // MARK: - Icon sidebar (constant shell)

    /// Narrow left-edge icon rail: Platforms / Tools / RAG / LLM Settings / Appearance.
    private var iconSidebar: some View {
        VStack(spacing: 6) {
            sidebarButton(icon: "shippingbox", title: "Platforms") {
                PlatformPortal.open(bridge: bridge, theme: theme)
            }
            sidebarButton(icon: "wrench.and.screwdriver", title: "Tools") {
                ToolsPortal.open(bridge: bridge, theme: theme)
            }
            // Interactive shell in the open project's root (or home if none).
            sidebarButton(icon: "terminal", title: "Terminal") {
                let cwd: String? = {
                    if case .idle(let project) = bridge.state {
                        return project.root
                    }
                    return nil
                }()
                TerminalPortal.open(cwd: cwd)
            }
            sidebarButton(icon: "books.vertical", title: "RAG Knowledge") {
                RagPortal.open(bridge: bridge, theme: theme)
            }
            sidebarButton(icon: "gearshape", title: "LLM Settings") {
                bridge.showSettings = true
            }
            Menu {
                Picker("Theme", selection: Binding(
                    get: { theme.tier },
                    set: { theme.tier = $0 }
                )) {
                    ForEach(AppTheme.Tier.allCases) { tier in
                        Label(tier.displayName, systemImage: tier.systemImage)
                            .tag(tier)
                    }
                }
                .labelsHidden()
            } label: {
                Image(systemName: theme.effectiveTier.systemImage)
                    .font(.system(size: 18))
                    .foregroundStyle(theme.textPrimary)
                    .frame(width: 34, height: 34)
                    .background(RoundedRectangle(cornerRadius: 7).fill(theme.buttonBackground))
                    .overlay(RoundedRectangle(cornerRadius: 7).stroke(theme.border, lineWidth: 0.5))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Appearance: \(theme.effectiveTier.displayName)")

            Spacer()
        }
        .padding(.vertical, 8)
        .frame(width: 48)
        .background(theme.surface)
    }

    private func sidebarButton(icon: String, title: String, active: Bool = false,
                               action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 18))
                .foregroundStyle(active ? theme.accent : theme.textPrimary)
                .frame(width: 34, height: 34)
                .background(RoundedRectangle(cornerRadius: 7)
                    .fill(active ? theme.accentBackground : theme.buttonBackground))
                .overlay(RoundedRectangle(cornerRadius: 7)
                    .stroke(theme.border, lineWidth: active ? 1 : 0.5))
        }
        .buttonStyle(.plain)
        .help(title)
    }

    // MARK: - Bottom status bar (constant shell)

    private var statusBar: some View {
        HStack(spacing: 10) {
            switch bridge.state {
            case .unconnected:
                Label("No project", systemImage: "tray")
            case .opening:
                Label("Opening…", systemImage: "arrow.down.circle")
            case .creating, .scaffolding, .filling:
                Label("New project", systemImage: "hammer.badge.plus")
            case .error:
                Label("Connection error", systemImage: "exclamationmark.triangle")
            case .idle(let project):
                Label(project.name, systemImage: "folder")
                if let sub = bridge.selectedSubproject {
                    Text("• \(sub.name)").foregroundStyle(theme.textSecondary)
                }
                if let target = bridge.selectedBuildTarget {
                    Text("• \(target)").foregroundStyle(theme.textSecondary)
                }
            }
            Spacer()
            if bridge.isProcessing {
                ProgressView().controlSize(.small)
            }
        }
        .font(.caption)
        .padding(.horizontal, 10)
        .frame(height: 24)
        .background(theme.surface)
    }

    // MARK: - Divider

    private var divider: some View {
        Rectangle()
            .fill(theme.divider)
            .contentShape(Rectangle().inset(by: -6))
            .onHover { hovering in
                if hovering {
                    NSCursor.resizeLeftRight.push()
                } else {
                    NSCursor.pop()
                }
            }
    }

    private func dragDivider(totalW: CGFloat) -> some Gesture {
        // Pane area excludes the icon sidebar + its divider.
        let sidebar = 48.0 + 1.0
        let available = max(200, totalW - sidebar)
        return DragGesture(minimumDistance: 0)
            .onChanged { value in
                let delta = value.translation.width / max(available, 1)
                leftFrac = min(max(0.30, committedLeftFrac + delta), 0.75)
            }
            .onEnded { _ in
                committedLeftFrac = leftFrac
            }
    }
}

#Preview {
    ContentView()
        .frame(width: 1280, height: 820)
}