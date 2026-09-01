import SwiftUI

/// Grid of clickable subproject cards showing each crate/target.
struct SubprojectGridView: View {
    let project: ProjectInfo
    @Environment(SpireBridge.self) private var bridge

    let columns = [
        GridItem(.adaptive(minimum: 180, maximum: 220), spacing: 12)
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Subprojects", systemImage: "cube.box.fill")
                .font(.headline)
                .foregroundStyle(.primary)

            if project.subprojects.isEmpty {
                ContentUnavailableView(
                    "No Subprojects Yet",
                    systemImage: "cube.box",
                    description: Text("No build config detected. Generate and execute a plan to scaffold the project.")
                )
            } else {
                LazyVGrid(columns: columns, spacing: 12) {
                    ForEach(project.subprojects) { sub in
                        SubprojectCard(subproject: sub, isSelected: bridge.selectedSubproject?.id == sub.id)
                            .onTapGesture {
                                bridge.selectSubproject(sub)
                            }
                    }
                }
            }
        }
    }
}

#Preview {
    let bridge = SpireBridge()
    Task { await bridge.fetchProjectAnalysis() }
    return SubprojectGridView(project: bridge.projectInfo!)
        .environment(bridge)
        .padding()
}