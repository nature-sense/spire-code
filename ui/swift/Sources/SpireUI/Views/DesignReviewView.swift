import SwiftUI

/// The **design, shown for approval**.
///
/// This is the wizard's review step, and the whole reason the design phase exists: the decomposition is
/// decided before the tree it describes, so a person says yes to *an architecture* rather than to a pile
/// of generated code it would be expensive to disagree with. So it shows what is reviewable and nothing
/// else — the framework **with its justification and the line it will become**, the units and what each
/// one is, the wiring, and the board facts an application carries because no component may.
///
/// It writes nothing. Approving it hands the spec back untouched to the scaffold, the apply tool and the
/// fill, so what was read here is what gets built.
struct DesignReviewView: View {
    @Environment(AppTheme.self) private var theme

    let design: ApplicationDesign

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            framework

            section(
                "Components (\(design.components.count))",
                lines: design.components.map { ($0.id, $0.summary) },
                empty: "none — this application is all composition"
            )

            section(
                "Composition (\(design.composition.count))",
                lines: design.composition.map { ($0.id, $0.summary) },
                empty: "none — nothing would run"
            )

            section(
                "Wiring",
                lines: design.edges.map { ($0, "") },
                empty: "none stated"
            )

            section(
                "Board facts",
                lines: design.facts.map { ($0.summary, "") },
                empty: "none — no device is reached at an address"
            )

            Text("The board: \(design.board.chip), \(design.board.bsp)\(design.board.hal.map { ", \($0)" } ?? "")")
                .font(.caption2)
                .foregroundStyle(theme.textTertiary)
                .textSelection(.enabled)
        }
    }

    private var framework: some View {
        VStack(alignment: .leading, spacing: 4) {
            SectionHeading("Framework")
            Text(design.frameworkLabel)
                .font(.callout.weight(.medium))
                .textSelection(.enabled)
            if let justification = design.justification, !justification.isEmpty {
                Text(justification)
                    .font(.caption)
                    .foregroundStyle(theme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }
            // The choice becomes *this line* and nothing else in the tree, so it is shown as it will be
            // written — a reviewer confirming a framework is confirming a `set(...)`.
            Text(design.marker)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(theme.textTertiary)
                .textSelection(.enabled)
        }
    }

    private func section(
        _ title: String,
        lines: [(String, String)],
        empty: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            SectionHeading(title)
            if lines.isEmpty {
                Text(empty)
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
                    .textSelection(.enabled)
            }
            ForEach(lines, id: \.0) { id, summary in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(id)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                    if !summary.isEmpty {
                        Text(summary)
                            .font(.caption2)
                            .foregroundStyle(theme.textSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                }
            }
        }
    }
}
