import SwiftUI
import Foundation

/// Review sheet for the compile-error fix loop.
///
/// For each file with compiler diagnostics it asks the backend for a whole-file
/// rewrite (`build/fixPropose`), shows it for review, and writes it ONLY when
/// the user accepts — then offers a rebuild. Nothing is written implicitly and
/// no toolchain auto-fix is involved: C/C++ has no reliable `--fix`, so this is
/// an LLM propose → review → apply loop.
struct FixErrorsSheet: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss

    let project: ProjectInfo
    let subproject: SubprojectInfo
    /// Build target the errors belong to (drives the platform of the rebuild).
    let target: String?
    /// Called when the user asks to rebuild after accepting fixes.
    var onRebuild: () -> Void

    @State private var files: [String] = []
    @State private var index = 0
    @State private var proposal: HalFixProposeResult?
    @State private var busy = false
    @State private var status = ""
    @State private var accepted = 0

    private var absPath: String { subproject.absolutePath(in: project.root) }
    private var currentFile: String? { index < files.count ? files[index] : nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            content
            Divider()
            footer
        }
        .frame(width: 880, height: 620)
        .background(theme.background)
        .task { await start() }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: "wand.and.stars").foregroundStyle(theme.accent)
            Text("Fix compile errors").font(.headline)
            if !files.isEmpty {
                Text("file \(min(index + 1, files.count)) of \(files.count)")
                    .font(.caption).foregroundStyle(theme.textSecondary)
            }
            Spacer()
            if accepted > 0 {
                Text("\(accepted) written").font(.caption).foregroundStyle(.green)
            }
            Button("Close") { dismiss() }.keyboardShortcut(.cancelAction)
        }
        .padding(12)
    }

    @ViewBuilder
    private var content: some View {
        if files.isEmpty {
            Text("No compiler errors recorded for this target. Run Build first so the diagnostics exist.")
                .font(.callout).foregroundStyle(theme.textSecondary)
                .padding(16).frame(maxHeight: .infinity, alignment: .topLeading)
        } else if let file = currentFile {
            VStack(alignment: .leading, spacing: 6) {
                Text(file).font(.caption.monospaced()).textSelection(.enabled)
                if busy {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Asking the LLM for a fix…").font(.caption)
                            .foregroundStyle(theme.textSecondary)
                    }
                } else if let c = proposal?.proposedContent, !c.isEmpty {
                    Text("Review the proposed rewrite (nothing is written until you accept):")
                        .font(.caption2).foregroundStyle(theme.textSecondary)
                    ScrollView {
                        Text(SyntaxHighlighter.highlight(c, language: .cpp))
                            .font(.caption2.monospaced())
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                    }
                    .background(RoundedRectangle(cornerRadius: 6).fill(theme.surface))
                } else {
                    Text(status.isEmpty ? "No proposal yet — press \"Propose fix\"." : status)
                        .font(.caption).foregroundStyle(theme.textSecondary)
                }
            }
            .padding(12)
            .frame(maxHeight: .infinity, alignment: .topLeading)
        } else {
            VStack(alignment: .leading, spacing: 8) {
                Text("Reviewed \(files.count) file\(files.count == 1 ? "" : "s") — \(accepted) written.")
                    .font(.callout)
                Text("Rebuild to verify the fixes.")
                    .font(.caption).foregroundStyle(theme.textSecondary)
            }
            .padding(16).frame(maxHeight: .infinity, alignment: .topLeading)
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            if let file = currentFile {
                Button("Propose fix") { Task { await propose(file) } }.disabled(busy)
                Button("Skip") { advance() }.disabled(busy)
                Spacer()
                Button("Accept & write") { Task { await accept() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || (proposal?.proposedContent ?? "").isEmpty)
            } else {
                Spacer()
                Button("Rebuild now") { onRebuild(); dismiss() }
                    .buttonStyle(.borderedProminent)
                    .disabled(files.isEmpty)
            }
        }
        .padding(12)
    }

    // MARK: - Flow

    /// Collect the files with ERROR diagnostics (distinct, absolute) and start
    /// with the first proposal.
    private func start() async {
        let diags = await bridge.fetchDiagnostics(path: absPath)
        let rootPrefix = project.root.hasSuffix("/") ? project.root : project.root + "/"
        let errFiles = diags
            .filter { $0.severity == "error" }
            .compactMap { $0.file }
            .map { $0.hasPrefix("/") ? $0 : rootPrefix + $0 }
        var seen = Set<String>()
        files = errFiles.filter { seen.insert($0).inserted }
        if let f = currentFile { await propose(f) }
    }

    private func propose(_ file: String) async {
        busy = true
        status = ""
        proposal = await bridge.proposeCompileFix(root: project.root, file: file)
        busy = false
        if let p = proposal, p.status != "proposed" {
            status = p.error
                ?? (p.status == "clean" ? "No diagnostics recorded for this file." : "status: \(p.status)")
        }
    }

    /// Write the reviewed proposal, then move to the next file. This is the
    /// ONLY place the fix loop touches the filesystem.
    private func accept() async {
        guard let file = currentFile,
              let content = proposal?.proposedContent, !content.isEmpty else { return }
        do {
            try content.write(toFile: file, atomically: true, encoding: .utf8)
            accepted += 1
        } catch {
            status = "write failed: \(error)"
        }
        advance()
    }

    private func advance() {
        proposal = nil
        status = ""
        index += 1
        if let f = currentFile { Task { await propose(f) } }
    }
}
