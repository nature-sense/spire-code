import SwiftUI

/// Tree view of subprojects — uses List for reliable hit-testing.
struct ProjectTreeView: View {
    @Environment(AppTheme.self) private var theme
    let project: ProjectInfo
    @Environment(SpireBridge.self) private var bridge

    private struct Row: Identifiable {
        let id: String
        let name: String
        let subproject: SubprojectInfo?
        let isFolder: Bool
        let depth: Int
    }

    @State private var rows: [Row] = []

    var body: some View {
        List {
            ForEach(rows) { row in
                rowView(row)
            }
        }
        .listStyle(.sidebar)
        .onAppear { buildRows() }
    }

    private func buildRows() {
        var result: [Row] = []
        result.append(Row(id: "root", name: project.name, subproject: nil, isFolder: false, depth: 0))

        var tree: [String: Any] = [:]
        for sub in project.subprojects where sub.kind != .directory {
            let path = sub.path.hasSuffix("/") ? String(sub.path.dropLast()) : sub.path
            guard !path.isEmpty else { continue }
            let parts = path.split(separator: "/").map(String.init)
            insert(sub, parts: parts, into: &tree)
        }
        flatten(dict: tree, depth: 1, prefix: "", result: &result)
        rows = result
    }

    private func insert(_ sub: SubprojectInfo, parts: [String], into dict: inout [String: Any]) {
        if parts.count == 1 { dict[parts[0]] = sub }
        else {
            var nested = dict[parts[0]] as? [String: Any] ?? [:]
            insert(sub, parts: Array(parts.dropFirst()), into: &nested)
            dict[parts[0]] = nested
        }
    }

    private func flatten(dict: [String: Any], depth: Int, prefix: String, result: inout [Row]) {
        for key in dict.keys.sorted() {
            let cp = prefix.isEmpty ? key : "\(prefix)/\(key)"
            if let sub = dict[key] as? SubprojectInfo {
                result.append(Row(id: "sp-\(sub.path)-\(sub.name)", name: sub.name, subproject: sub, isFolder: false, depth: depth))
            } else if let nested = dict[key] as? [String: Any], !nested.isEmpty {
                result.append(Row(id: "dir-\(cp)", name: key, subproject: nil, isFolder: true, depth: depth))
                flatten(dict: nested, depth: depth + 1, prefix: cp, result: &result)
            }
        }
    }

    @ViewBuilder
    private func rowView(_ row: Row) -> some View {
        if row.isFolder {
            HStack(spacing: 4) {
                Color.clear.frame(width: CGFloat(row.depth) * 16)
                Image(systemName: "folder.fill")
                    .font(.body).foregroundStyle(.secondary)
                Text(row.name)
                    .font(.body).foregroundStyle(.secondary)
            }
        } else if row.id == "root" {
            Button {
                bridge.selectSubproject(nil)
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "square.grid.2x2")
                        .font(.body.weight(.medium)).foregroundColor(.white)
                    Text(row.name)
                        .font(.body.weight(.medium)).foregroundColor(.white)
                        .lineLimit(1)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .frame(width: 130, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 6).fill(theme.accent))
            }
            .buttonStyle(.plain)
        } else if let sub = row.subproject {
            Button {
                bridge.selectSubproject(sub)
            } label: {
                HStack(spacing: 4) {
                    Color.clear.frame(width: CGFloat(row.depth) * 16)
                    card(for: sub)
                }
            }
            .buttonStyle(.plain)
        }
    }

    private func card(for sub: SubprojectInfo) -> some View {
        let (label, fg, bg) = buildInfo(sub.buildSystem)
        return HStack(spacing: 4) {
            VStack(alignment: .leading, spacing: 2) {
                Text(sub.name).font(.body.weight(.medium)).foregroundColor(fg).lineLimit(1)
                if !sub.description.isEmpty {
                    Text(sub.description)
                        .font(.caption2)
                        .foregroundColor(fg.opacity(0.6))
                        .lineLimit(2)
                }
                Text(label).font(.caption2.weight(.semibold)).foregroundColor(fg.opacity(0.75)).lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8).padding(.vertical, 6)
        .frame(width: 130, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 6).fill(bg))
        .overlay(
            RoundedRectangle(cornerRadius: 6)
                .stroke(bridge.selectedSubproject?.id == sub.id ? fg : Color.clear, lineWidth: 1.5)
        )
    }

    private func buildInfo(_ bs: String) -> (String, Color, Color) {
        switch bs {
        case "Cargo":          return ("Rust", .white, Color(red: 0.70, green: 0.25, blue: 0.15))
        case "SwiftPM","Xcode": return ("Swift", .white, Color(red: 0.90, green: 0.55, blue: 0.10))
        case "npm","pnpm","yarn": return ("JS", .white, Color(red: 0.30, green: 0.65, blue: 0.30))
        default:               return (bs.isEmpty ? "?" : bs, .white, Color(red: 0.50, green: 0.55, blue: 0.60))
        }
    }
}