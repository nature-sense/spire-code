import Foundation

/// A node in the project tree — directory folders with optional subproject metadata.
struct ProjectTreeNode: Identifiable {
    let id: String
    let name: String
    let path: String
    /// Non-nil means this leaf directory IS a subproject
    let subproject: SubprojectInfo?
    var children: [ProjectTreeNode]?
}

/// Builds a multi-level tree from subproject paths, keeping only directory (folder) nodes.
/// The root of the tree is the project name (e.g. "spire-app").
func buildProjectTree(from project: ProjectInfo) -> [ProjectTreeNode] {
    // The main project itself (root config, e.g. Cargo.toml at the root) is a
    // first-class subproject (kind == .project). Use it as the root node so its
    // files + dependencies render at the top of the tree; directory-based
    // subprojects become its children.
    let isMain = { (sub: SubprojectInfo) -> Bool in
        sub.kind == .project && (sub.path.isEmpty || sub.path == "/")
    }
    let main = project.subprojects.first(where: isMain)

    var root: [String: Any] = [:]
    for sub in project.subprojects where !isMain(sub) {
        let cleanPath = sub.path.hasSuffix("/") ? String(sub.path.dropLast()) : sub.path
        let parts = cleanPath.split(separator: "/").map(String.init).filter { !$0.isEmpty }
        insertSubproject(sub, parts: parts, into: &root)
    }

    let children = buildNodes(from: root, parentPath: "").sorted { $0.name < $1.name }
    return [ProjectTreeNode(
        id: "root",
        name: main?.name ?? project.name,
        path: "",
        subproject: main,
        children: children
    )]
}

private func insertSubproject(_ sub: SubprojectInfo, parts: [String], into dict: inout [String: Any]) {
    guard !parts.isEmpty else { return }
    let key = parts[0]
    if parts.count == 1 {
        dict[key] = sub
    } else {
        var nested = dict[key] as? [String: Any] ?? [:]
        insertSubproject(sub, parts: Array(parts.dropFirst()), into: &nested)
        dict[key] = nested
    }
}

/// Returns a display name for the build system / language.
func badgeLabel(for buildSystem: String) -> String {
    switch buildSystem {
    case "Cargo": return "Rust"
    case "SwiftPM", "Xcode": return "Swift"
    case "npm", "pnpm", "yarn": return "JS"
    default: return buildSystem
    }
}

/// Returns a badge color for the build system.
func badgeColor(for buildSystem: String) -> String {
    switch buildSystem {
    case "Cargo": return "#DE3C3C"     // Rust orange-red
    case "SwiftPM", "Xcode": return "#F05138"  // Swift orange
    case "npm", "pnpm", "yarn": return "#68A063" // Node green
    default: return "#6B7280"          // gray
    }
}

private func buildNodes(from dict: [String: Any], parentPath: String) -> [ProjectTreeNode] {
    dict.compactMap { (key, value) -> ProjectTreeNode? in
        let path = parentPath.isEmpty ? key : "\(parentPath)/\(key)"
        if let sub = value as? SubprojectInfo {
            // Leaf subproject — show with its language badge
            return ProjectTreeNode(
                id: sub.id,
                name: sub.name,
                path: sub.path,
                subproject: sub,
                children: nil
            )
        } else if let nested = value as? [String: Any] {
            let children = buildNodes(from: nested, parentPath: path).sorted { $0.name < $1.name }
            // Intermediate folder — only show if it has children
            return children.isEmpty ? nil : ProjectTreeNode(
                id: path,
                name: key,
                path: path + "/",
                subproject: nil,
                children: children
            )
        }
        return nil
    }
}