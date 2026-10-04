import SwiftUI

/// A component's **edit history** — the `idf_component_edit` runs recorded for it, newest first.
///
/// Each row is one run: what was asked, whether the gate kept it, and how far the check reached. The
/// record is read from the graph (`idf-component/history`), not from the last edit's response, so it
/// shows runs from earlier sessions too — the point of keeping a log rather than a status.
///
/// The sheet is a **fixed size**, and it has to be: macOS sizes a sheet to the fitting size of its
/// content at presentation, while these records arrive asynchronously. It was laid out while this
/// view still showed "Reading the record…" — a status bar's height — so it opened at about a third
/// of its height and grew into place afterwards. Pinning the size makes it independent of that.
struct ComponentEditHistorySheet: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @Environment(\.dismiss) private var dismiss

    let root: String
    let component: String

    @State private var records: [ComponentEditRecord] = []
    @State private var loading = true
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("History").font(.title3.weight(.semibold))
                Text(component)
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
            }

            if loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Reading the record…").font(.caption).foregroundStyle(theme.textSecondary)
                }
            } else if let failure {
                Text(failure).font(.caption).foregroundStyle(.red).textSelection(.enabled)
            } else if records.isEmpty {
                Text("No edits recorded yet. The first time this component is written or changed, the "
                     + "run is kept here: what was asked, and what the gate did with it.")
                    .font(.caption)
                    .foregroundStyle(theme.textTertiary)
            } else {
                ScrollView {
                    VStack(spacing: 8) {
                        ForEach(records) { record in
                            row(record)
                        }
                    }
                }
                .frame(maxHeight: 420)
            }

            // Holds the button at the bottom while the list is short — loading, empty, or a
            // component with a run or two — rather than leaving it floating under the last row.
            Spacer(minLength: 0)

            HStack {
                Spacer(minLength: 0)
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
        }
        .padding(20)
        .frame(width: 560, height: 550)
        .background(theme.background)
        .task { await load() }
    }

    // MARK: - One run

    private func row(_ record: ComponentEditRecord) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                verdictBadge(record)
                Text(record.instruction.isEmpty ? "(no instruction recorded)" : record.instruction)
                    .font(.callout.weight(.semibold))
                    .textSelection(.enabled)
                Spacer(minLength: 0)
                Text(record.at)
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
            }
            Text(whatHappened(record))
                .font(.caption2)
                .foregroundStyle(theme.textSecondary)
            ForEach(record.caveats, id: \.self) { caveat in
                Text("· \(caveat)")
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 1))
    }

    /// What the gate did, said as one word: a no-op and a kept change are different outcomes from a
    /// reverted one, and the record keeps them apart.
    private func verdictBadge(_ record: ComponentEditRecord) -> some View {
        let (label, tint): (String, Color) = {
            if record.upToDate { return ("up to date", .blue) }
            return record.success ? ("kept", .green) : ("reverted", .red)
        }()
        return Text(label)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Capsule().fill(tint.opacity(0.15)))
            .foregroundStyle(tint)
    }

    /// What the run did, in one line: the gate that ran, the files that moved, the chip build's word.
    private func whatHappened(_ record: ComponentEditRecord) -> String {
        var parts: [String] = []
        parts.append(record.verified == "WithTarget"
                     ? "verified against the target"
                     : "verified on the host only")
        if record.upToDate {
            parts.append("the files already said what was asked")
        } else if record.filesChanged.isEmpty {
            parts.append("no files changed")
        } else {
            parts.append("changed \(record.filesChanged.joined(separator: ", "))")
        }
        if !record.filesReverted.isEmpty {
            parts.append("reverted \(record.filesReverted.joined(separator: ", "))")
        }
        if !record.filesSkipped.isEmpty {
            parts.append("left \(record.filesSkipped.joined(separator: ", "))")
        }
        if !record.chipBuild.isEmpty {
            parts.append(record.chipBuild)
        }
        return parts.joined(separator: " · ")
    }

    private func load() async {
        let found = await bridge.componentEditHistory(root: root, name: component)
        await MainActor.run {
            records = found
            loading = false
        }
    }
}
