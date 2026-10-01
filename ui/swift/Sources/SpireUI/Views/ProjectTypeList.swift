import SwiftUI

/// The **New project** list.
///
/// One list, in three places that all mean the same thing: the welcome screen, the actions for a
/// folder that has no project in it yet, and the empty state of the project dashboard. Shared rather
/// than written three times, because a row that means the same thing in three places should not be
/// three rows that can drift apart.
///
/// A flat list, deliberately. The shape of a project is one of a handful of things, and a tree of
/// questions to arrive at one of them is a tree of questions to get past.
struct ProjectTypePicker: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    /// Where the project goes.
    ///
    /// `nil` on the welcome screen, which asks. A directory everywhere else: the folder is already
    /// open, so it is already decided, and the sheet only needs a name.
    var fixedLocation: String? = nil

    @State private var creating: ProjectType?

    var body: some View {
        VStack(alignment: .leading, spacing: ProjectListMetrics.columnSpacing) {
            ForEach(ProjectTypeGroup.all) { group in
                VStack(alignment: .leading, spacing: ProjectListMetrics.groupSpacing) {
                    SectionHeading(group.title)
                    VStack(spacing: ProjectListMetrics.groupSpacing) {
                        ForEach(group.types) { type in
                            ProjectTypeRow(type: type) { choose(type) }
                        }
                    }
                }
            }
        }
        .sheet(item: $creating) { type in
            // Only a row carrying a structure key can open this — `choose` refuses the rest — so the
            // sheet taking a non-optional key is a fact about this call site, not a check inside it.
            if let structure = type.structure {
                CreateProjectSheet(type: type, structure: structure, fixedLocation: fixedLocation)
                    .environment(bridge)
                    .environment(theme)
            }
        }
    }

    /// A row whose structure key is not written yet does nothing. It is still shown, because what is
    /// coming is worth seeing, but it does not open a sheet that could not scaffold anything.
    private func choose(_ type: ProjectType) {
        guard type.structure != nil else { return }
        creating = type
    }
}

/// A sub-heading over a group of cards — "Native", "Embedded", "Recently Opened Projects".
///
/// Shared so the welcome screen's two columns can measure themselves against the same line: the left
/// column passes a blank string to reserve the line the right column spends on "Native", and that is
/// what keeps the Open button and the first project type level with each other.
struct SectionHeading: View {
    @Environment(AppTheme.self) private var theme

    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(theme.textSecondary)
    }
}

/// The vertical rhythm these lists are laid out on.
///
/// Named once because the welcome screen's columns have to agree on it: the agreement *is* the
/// alignment, and two columns that each spell `14` and `6` are two columns that can drift.
enum ProjectListMetrics {
    /// Between a heading and the first card under it.
    static let columnSpacing: CGFloat = 14
    /// Between a section's sub-heading and its cards, and between the cards themselves.
    static let groupSpacing: CGFloat = 6
}

/// One project type. The whole card is the target, and its shape is the one both columns' first
/// cards are measured against — the Open button in particular, which matches these metrics so the
/// two columns' first cards are the same height.
struct ProjectTypeRow: View {
    @Environment(AppTheme.self) private var theme

    let type: ProjectType
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(type.title).font(.callout.weight(.semibold))
                    Text(type.subtitle).font(.caption).foregroundStyle(theme.textSecondary)
                }
                Spacer(minLength: 8)
                Image(systemName: "chevron.right")
                    .font(.caption2)
                    .foregroundStyle(theme.textTertiary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 8).fill(theme.surface))
            .overlay(
                RoundedRectangle(cornerRadius: 8).stroke(theme.border, lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        // A row with no structure key has nothing to scaffold yet. It says so on hover rather than
        // opening a sheet that would have to refuse.
        .help(type.structure == nil ? "\(type.title) is not available yet" : type.subtitle)
    }
}

#Preview {
    ProjectTypePicker()
        .environment(SpireBridge.shared)
        .environment(AppTheme())
        .padding(20)
        .frame(width: 460)
}
