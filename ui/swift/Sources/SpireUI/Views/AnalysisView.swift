import SwiftUI

/// Analysis / planning mode — overview of project structure, languages, build systems.
struct AnalysisView: View {
    let project: ProjectInfo

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                // Architecture overview
                GroupBox("Architecture") {
                    Text(project.architecture)
                        .font(.body)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                // Languages
                GroupBox("Languages") {
                    ForEach(project.languages.keys.sorted(), id: \.self) { lang in
                        let count = project.languages[lang] ?? 0
                        HStack {
                            Text(lang)
                                .font(.subheadline)
                            Spacer()
                            Text("\(count) file(s)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                // Build systems
                GroupBox("Build Systems") {
                    ForEach(project.buildSystems, id: \.self) { bs in
                        HStack {
                            Image(systemName: "wrench")
                            Text(bs)
                        }
                        .padding(.vertical, 2)
                    }
                }

                // Subprojects
                GroupBox("Subprojects") {
                    ForEach(project.subprojects) { sub in
                        HStack {
                            Text(sub.name)
                                .font(.subheadline)
                            Spacer()
                            Text(sub.buildSystem)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 2)
                    }
                }
            }
            .padding()
        }
    }
}