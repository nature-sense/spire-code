import SwiftUI
import AppKit

/// Entry-state workspace: the recently opened projects (most recent first, last
/// 5) with open / remove actions. Open/New live in the right-hand Actions pane,
/// so this stays a focused list.
struct RecentProjectsPane: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var isOpening = false

    var body: some View {
        VStack(spacing: 24) {
            VStack(spacing: 6) {
                Image(systemName: "hammer.fill")
                    .font(.system(size: 44))
                    .foregroundStyle(.orange)
                Text("Spire")
                    .font(.system(size: 30, weight: .bold))
                Text("Project intelligence for your codebase")
                    .font(.headline)
                    .foregroundStyle(.secondary)
            }
            .padding(.top, 28)

            recentSection

            Text("Open a folder or start a new project from the Actions pane on the right.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .padding(.bottom, 12)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
        .onAppear { bridge.loadRecentProjects() }
    }

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Text("Recent Projects")
                    .font(.headline)
                    .foregroundStyle(.secondary)
                if !bridge.recentProjects.isEmpty {
                    Text("\(bridge.recentProjects.count)")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
            }

            if bridge.recentProjects.isEmpty {
                Text("No recent projects yet")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 8)
            } else {
                ForEach(bridge.recentProjects) { project in
                    projectRow(project)
                }
            }
        }
        .frame(width: 500)
    }

    private func projectRow(_ project: RecentProject) -> some View {
        HStack(spacing: 0) {
            // Open action — fills the row.
            Button { open(path: project.path) } label: {
                HStack(spacing: 10) {
                    Image(systemName: "folder")
                        .foregroundStyle(.blue)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(project.name).font(.headline)
                        Text(project.path)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer()
                    if isOpening { ProgressView().controlSize(.small) }
                }
            }
            .buttonStyle(.plain)

            // Remove entry — sibling of the open button so tapping the X never
            // opens the project.
            Button {
                bridge.removeRecentProject(path: project.path)
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.system(size: 14))
                    .foregroundStyle(.secondary)
                    .padding(.trailing, 10)
            }
            .buttonStyle(.plain)
            .help("Remove from recent projects")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
    }

    private func open(path: String) {
        guard !isOpening else { return }
        isOpening = true
        defer { isOpening = false }
        Task { await bridge.openProject(root: path) }
    }
}

#Preview {
    RecentProjectsPane()
        .environment(SpireBridge.shared)
        .environment(AppTheme())
        .frame(width: 800, height: 600)
}
