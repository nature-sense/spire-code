import SwiftUI

/// Top summary card showing the project name, root path, languages, and build systems.
/// Rendered as a vertical list for readability in the sidebar.
struct ProjectOverviewCard: View {
    @Environment(AppTheme.self) private var theme
    let project: ProjectInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Name
            Text(project.name)
                .font(.headline)
                .fontWeight(.bold)
                .lineLimit(1)

            // Root path
            Label(project.root, systemImage: "folder")
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)

            Divider()

            // Languages
            HStack(spacing: 4) {
                Image(systemName: "text.alignleft")
                    .foregroundStyle(.blue)
                    .font(.caption)
                ForEach(Array(project.languages.keys.sorted()), id: \.self) { lang in
                    if let count = project.languages[lang] {
                        Text("\(lang) \(count)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            // Build systems
            HStack(spacing: 4) {
                Image(systemName: "hammer.fill")
                    .foregroundStyle(.orange)
                    .font(.caption)
                Text(project.buildSystems.joined(separator: " · "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // Subproject count
            HStack(spacing: 4) {
                Image(systemName: "cube.box.fill")
                    .foregroundStyle(.purple)
                    .font(.caption)
                Text("\(project.subprojects.count) crates")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // Connection status
            HStack(spacing: 6) {
                Circle()
                    .fill(.green)
                    .frame(width: 6, height: 6)
                Text("spire-core connected")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(theme.surface)
                .shadow(color: .black.opacity(0.2), radius: 2, y: 1)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(theme.border, lineWidth: 0.5)
        )
    }
}

#Preview {
    let stub = SpireBridge()
    Task { await stub.fetchProjectAnalysis() }
    return ProjectOverviewCard(project: stub.projectInfo!)
        .padding()
        .frame(width: 280)
}