import SwiftUI

/// Uncommitted-changes indicator for the project working tree.
///
/// Renders a compact chip ("● 54 uncommitted" / "✓ clean") in the main header
/// and opens a sheet with the full `git diff` plus a commit box. This is the
/// safety net for mutating actions — a mislabelled "Fix Warnings" once
/// reformatted 54 files with nothing in the UI drawing attention to it.
///
/// Refreshes when the project changes and whenever `bridge.buildCompletionTick`
/// advances (i.e. after any completed build / lint / test / clean / fix).
struct GitChangesBadge: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let project: ProjectInfo

    @State private var summary: SpireBridge.GitSummary?
    @State private var loading = false
    @State private var showSheet = false

    private var refreshKey: String { "\(project.root)|\(bridge.buildCompletionTick)" }

    var body: some View {
        Button {
            showSheet = true
        } label: {
            HStack(spacing: 4) {
                if loading && summary == nil {
                    ProgressView().controlSize(.mini)
                } else {
                    Image(systemName: icon).font(.caption2).foregroundStyle(color)
                    Text(label).font(.caption2).foregroundStyle(color)
                }
            }
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(RoundedRectangle(cornerRadius: 5).fill(theme.buttonBackground))
            .overlay(RoundedRectangle(cornerRadius: 5).stroke(theme.border, lineWidth: 0.5))
        }
        .buttonStyle(.plain)
        .help(help)
        .task(id: refreshKey) { await load() }
        .sheet(isPresented: $showSheet) {
            GitChangesSheet(project: project) { await load() }
        }
    }

    private var icon: String {
        guard let summary else { return "questionmark.circle" }   // git unavailable
        return summary.isDirty ? "exclamationmark.triangle.fill" : "checkmark.circle"
    }

    private var color: Color {
        guard let summary else { return theme.textSecondary }
        return summary.isDirty ? .orange : .green
    }

    private var label: String {
        guard let summary else { return "no git" }
        guard summary.isDirty else { return "clean" }
        return "\(summary.changedCount) uncommitted"
    }

    private var help: String {
        guard let summary else { return "Git status unavailable for this project" }
        guard summary.isDirty else { return "Working tree clean — nothing to commit" }
        var text = "\(summary.changedCount) uncommitted change\(summary.changedCount == 1 ? "" : "s")"
        if summary.untrackedCount > 0 { text += " (\(summary.untrackedCount) untracked)" }
        return text + " — click to review the diff"
    }

    private func load() async {
        loading = true
        summary = await bridge.gitStatus(path: project.root)
        loading = false
    }
}
