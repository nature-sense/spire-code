import SwiftUI

/// Project detail — shown when no subproject is selected.
struct ProjectDetailView: View {
    @Environment(AppTheme.self) private var theme
    let project: ProjectInfo

    var body: some View {
        GeometryReader { geo in
            HStack(spacing: 0) {
                VStack(alignment: .leading, spacing: 0) {
                    Text(project.name)
                        .font(.system(size: 28, weight: .medium))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding()
                    Spacer()
                }
                .frame(width: geo.size.width * 0.4)
                .background(theme.surface)

                Rectangle()
                    .fill(theme.divider)
                    .frame(width: 1)

                VStack {
                    Spacer()
                }
                .frame(width: geo.size.width * 0.6)
                .background(theme.textBackground)
            }
        }
    }
}
