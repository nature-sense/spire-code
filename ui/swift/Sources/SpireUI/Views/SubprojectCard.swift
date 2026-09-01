import SwiftUI

/// A single subproject card in the grid: name, language icon, type, build system, and optional build status indicator.
struct SubprojectCard: View {
    @Environment(AppTheme.self) private var theme
    let subproject: SubprojectInfo
    let isSelected: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(subproject.language.hasPrefix("🦀") ? "🦀" : "🐦")
                    .font(.title2)
                Spacer()
                kindBadge
            }

            Text(subproject.name)
                .font(.headline)
                .fontWeight(.semibold)
                .lineLimit(1)

            Text(subproject.buildSystem)
                .font(.caption)
                .foregroundStyle(.secondary)

            Spacer()

            // Build status indicator
            if let status = subproject.buildStatus {
                HStack(spacing: 4) {
                    Circle()
                        .fill(status.success == true ? Color.green : (status.success == false ? Color.red : Color.yellow))
                        .frame(width: 6, height: 6)
                    Text(status.success == true ? "Passing" : (status.success == false ? "Failing" : "Unknown"))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    if let output = status.output {
                        Text("·")
                            .foregroundStyle(.tertiary)
                        Text(output)
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                    }
                }
            }
        }
        .padding(12)
        .frame(height: 130)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .fill(isSelected ? theme.accentBackground : Color.clear)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .stroke(isSelected ? theme.accent : theme.border, lineWidth: isSelected ? 2 : 0.5)
        )
        .contentShape(Rectangle())
    }

    private var kindBadge: some View {
        Text(subproject.kind.rawValue)
            .font(.caption2)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(theme.badgeBackground)
            .clipShape(Capsule())
    }
}