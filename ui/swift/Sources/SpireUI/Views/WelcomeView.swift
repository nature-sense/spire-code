import SwiftUI
import AppKit

/// The welcome screen — **one pane**, beside the icon bar.
///
/// Open on the left, new on the right. The project types are a flat list rather than a wizard: the
/// shape of a project is one of a handful of things, and a tree of questions to arrive at one of them
/// is a tree of questions to get past.
///
/// The icon bar stays. It is the application's own navigation, and nothing here is about a project —
/// there is not one open yet.
struct WelcomeView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var isOpening = false

    var body: some View {
        VStack(spacing: 32) {
            masthead
            HStack(alignment: .top, spacing: 28) {
                openColumn
                Divider().overlay(theme.divider)
                newColumn
            }
            .frame(maxWidth: 960)
            Spacer(minLength: 0)
        }
        .padding(.top, 44)
        .padding(.horizontal, 28)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(theme.background)
        .onAppear { bridge.loadRecentProjects() }
    }

    // MARK: - Masthead

    private var masthead: some View {
        VStack(spacing: 6) {
            Image(systemName: "hammer.fill")
                .font(.system(size: 50))
                .foregroundStyle(.orange)
            Text("Spire")
                .font(.system(size: 32, weight: .bold))
            Text("Project intelligence for your codebase")
                .font(.title3)
                .foregroundStyle(theme.textSecondary)
        }
    }

    // MARK: - Left: open

    private var openColumn: some View {
        VStack(alignment: .leading, spacing: ProjectListMetrics.columnSpacing) {
            columnHeading("Open project")
            // The right column spends a line on its first group sub-heading ("Native") before its
            // first row. This is that same line, empty, so the Open button and the first project
            // type share a top edge — a line of the sub-heading's own font rather than a fixed
            // offset, so the two columns stay level when the app's text scale changes.
            VStack(alignment: .leading, spacing: ProjectListMetrics.groupSpacing) {
                SectionHeading(" ")
                openButton
            }
            // The list is a named section too, so the left column has the same shape as the right —
            // heading, then sub-headings over their cards — rather than a button with a loose tail.
            VStack(alignment: .leading, spacing: ProjectListMetrics.groupSpacing) {
                SectionHeading("Recently Opened Projects")
                recentList
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Metrics match `typeRow` exactly — same fonts, same padding, same corner radius — so the two
    /// columns' first cards are the same height. The accent fill is what marks this as the primary
    /// action, not the size; a bigger card was doing a job the highlight already does.
    private var openButton: some View {
        Button { chooseProjectFolder() } label: {
            HStack(spacing: 10) {
                if isOpening {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: "folder").font(.title3)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text("Open project…").font(.callout.weight(.semibold))
                    Text("Choose an existing project folder")
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                }
                Spacer(minLength: 8)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.accentBackground))
            .overlay(
                RoundedRectangle(cornerRadius: 8)
                    .stroke(theme.accent.opacity(0.45), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .disabled(isOpening)
    }

    @ViewBuilder
    private var recentList: some View {
        if bridge.recentProjects.isEmpty {
            Text("No projects opened yet")
                .font(.callout)
                .foregroundStyle(theme.textTertiary)
                .padding(.vertical, 6)
        } else {
            VStack(spacing: 6) {
                ForEach(bridge.recentProjects) { project in
                    recentRow(project)
                }
            }
        }
    }

    private func recentRow(_ project: RecentProject) -> some View {
        HStack(spacing: 0) {
            Button { open(path: project.path) } label: {
                HStack(spacing: 10) {
                    Image(systemName: "folder").foregroundStyle(.blue)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(project.name).font(.callout.weight(.medium))
                        Text(project.path)
                            .font(.caption)
                            .foregroundStyle(theme.textSecondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 8)
                }
            }
            .buttonStyle(.plain)

            // A sibling of the open button, so the X never opens the project by accident.
            Button { bridge.removeRecentProject(path: project.path) } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 13))
                    .foregroundStyle(theme.textTertiary)
            }
            .buttonStyle(.plain)
            .help("Remove from recent projects")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
    }

    // MARK: - Right: new

    private var newColumn: some View {
        VStack(alignment: .leading, spacing: ProjectListMetrics.columnSpacing) {
            columnHeading("New project")
            ProjectTypePicker()
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func columnHeading(_ title: String) -> some View {
        Text(title)
            .font(.title3.weight(.semibold))
    }

    // MARK: - Actions

    /// The folder picker, unchanged: it works, and what it does — choose a directory — is exactly
    /// what this column is for.
    private func chooseProjectFolder() {
        NSApp.activate(ignoringOtherApps: true)

        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Open"
        panel.message = "Choose a project directory"
        panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser

        if panel.runModal() == .OK, let url = panel.url {
            open(path: url.path)
        }
    }

    private func open(path: String) {
        guard !isOpening else { return }
        isOpening = true
        defer { isOpening = false }
        Task { await bridge.openProject(root: path) }
    }
}

#Preview {
    WelcomeView()
        .environment(SpireBridge.shared)
        .environment(AppTheme())
        .frame(width: 1100, height: 700)
}
