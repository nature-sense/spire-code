import SwiftUI

/// Sheet showing the project's uncommitted diff with a commit box.
///
/// Commit semantics: Spire stages everything first (`git add -A`) and then
/// commits — the underlying `git_commit` tool only commits the STAGED set, so
/// without staging the button would silently do nothing. Committing is always an
/// explicit, user-driven step; nothing is ever committed automatically.
struct GitChangesSheet: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let project: ProjectInfo
    /// Invoked after a successful commit so the badge refreshes.
    var onChanged: () async -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var diff: String?
    @State private var status: SpireBridge.GitSummary?
    @State private var message = ""
    @State private var busy = false
    @State private var resultText: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            diffView
            Divider()
            commitBar
        }
        .frame(width: 780, height: 580)
        .background(theme.background)
        .task { await reload() }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "arrow.triangle.branch").foregroundStyle(theme.accent)
            Text("Uncommitted changes").font(.headline)
            if let s = status, s.isDirty {
                Text("\(s.changedCount) file\(s.changedCount == 1 ? "" : "s")")
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
            }
            Spacer()
            Button("Refresh") { Task { await reload() } }
                .buttonStyle(.plain)
                .font(.caption)
            Button("Done") { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
        .padding(12)
    }

    @ViewBuilder
    private var diffView: some View {
        ScrollView([.vertical, .horizontal]) {
            if let diff, !diff.isEmpty {
                Text(diff)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            } else {
                Text("No unstaged diff. Untracked files appear in the change count but not in `git diff`.")
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
                    .padding(12)
            }
        }
        .frame(maxHeight: .infinity)
    }

    private var commitBar: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let resultText {
                Text(resultText)
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
                    .textSelection(.enabled)
            }
            HStack(spacing: 8) {
                TextField("Commit message", text: $message)
                    .textFieldStyle(.roundedBorder)
                Button {
                    Task { await commit() }
                } label: {
                    if busy {
                        ProgressView().controlSize(.small)
                    } else {
                        Label("Stage & Commit", systemImage: "checkmark.seal")
                    }
                }
                .disabled(busy || message.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(12)
    }

    private func reload() async {
        status = await bridge.gitStatus(path: project.root)
        diff = await bridge.gitDiff(path: project.root)
    }

    private func commit() async {
        busy = true
        resultText = nil
        let staged = await bridge.gitStage(path: project.root)
        guard staged else {
            busy = false
            resultText = "❌ git add failed"
            return
        }
        let out = await bridge.gitCommit(path: project.root, message: message)
        busy = false
        if let out {
            let first = out.split(separator: "\n").first.map(String.init) ?? out
            resultText = "✅ \(first)"
            message = ""
        } else {
            resultText = "❌ commit failed (nothing staged?)"
        }
        await reload()
        await onChanged()
    }
}
