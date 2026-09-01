import SwiftUI
import AppKit

/// Startup screen: recent projects + open/new actions.
struct WelcomeView: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var isOpening = false

    var body: some View {
        VStack(spacing: 28) {
            VStack(spacing: 8) {
                Image(systemName: "hammer.fill")
                    .font(.system(size: 56))
                    .foregroundStyle(.orange)
                Text("Spire")
                    .font(.system(size: 36, weight: .bold))
                Text("Project intelligence for your codebase")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }

            recentSection

            openCreateButton
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.background)
        .onAppear { bridge.loadRecentProjects() }
    }

    @ViewBuilder
    private var recentSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Recent Projects")
                .font(.headline)
                .foregroundStyle(.secondary)

            if bridge.recentProjects.isEmpty {
                Text("No recent projects yet")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 8)
            } else {
                ForEach(bridge.recentProjects) { project in
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

                        // Delete recent entry — sibling of the open button so
                        // tapping the X never triggers opening the project.
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
            }
        }
        .frame(width: 420)
    }

    /// Two clearly separated actions: open an existing folder, or start a new
    /// project in the wizard (directory handling is inside the wizard flow,
    /// so the folder is never picked twice).
    private var openCreateButton: some View {
        VStack(spacing: 10) {
            Button { openOrCreate() } label: {
                welcomeActionLabel("Open Project…",
                                   subtitle: "Choose an existing project folder",
                                   icon: "folder",
                                   accent: theme.accentBackground)
            }
            .buttonStyle(.plain)
            .disabled(isOpening)

            Button {
                bridge.closeProject()
                bridge.state = .creating(plan: nil, executing: false)
                bridge.currentMode = .project
            } label: {
                welcomeActionLabel("New Project…",
                                   subtitle: "Embedded or Native, with optional HAL",
                                   icon: "hammer.badge.plus",
                                   accent: theme.surface)
            }
            .buttonStyle(.plain)
        }
        .frame(maxWidth: 340)
    }

    private func welcomeActionLabel(_ title: String, subtitle: String,
                                    icon: String, accent: Color) -> some View {
        HStack(spacing: 10) {
            if isOpening && title.hasPrefix("Open") {
                ProgressView().controlSize(.small)
            } else {
                Image(systemName: icon).font(.title3)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: 320, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(accent))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(theme.accent.opacity(0.45), lineWidth: 1))
    }

    private func openOrCreate() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = "Open"
        panel.message = "Choose a project directory, or create a new folder"
        panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser

        if panel.runModal() == .OK, let url = panel.url {
            open(path: url.path)
        }
    }

    private func open(path: String) {
        guard !isOpening else { return }
        isOpening = true
        defer { isOpening = false }
        Task {
            await bridge.openProject(root: path)
        }
    }
}