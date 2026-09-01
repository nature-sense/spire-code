import Foundation

/// A directory node in the project file tree.
struct FileTreeDirectory: Codable, Identifiable {
    var id: String { path }
    var name: String
    var path: String
    var role: String
    var directories: [FileTreeDirectory] = []
    var files: [FileTreeFile] = []
    var totalFileCount: Int = 0
    var totalLines: Int = 0

    enum CodingKeys: String, CodingKey {
        case name, path, role, directories, files
        case totalFileCount = "total_file_count"
        case totalLines = "total_lines"
    }

    mutating func apply(event: SpireBridge.FileChangeEvent) {
        // Deleted events should never add nodes back into the tree.
        if event.kind == "deleted" { return }

        let parts = event.path.split(separator: "/").map(String.init)
        guard let leaf = parts.last else { return }
        let dirParts = Array(parts.dropLast())

        // Prefer the filesystem-resolved marker (set by ProjectInfo.apply).
        // Fall back to the extension heuristic only when it's unknown, so
        // extension-less files (Makefile, LICENSE, Dockerfile, …) are never
        // misread as phantom empty directories.
        let isDir: Bool
        if let known = event.isDirectory {
            isDir = known
        } else {
            isDir = !leaf.contains(".")
        }

        mutatePath(dirParts) { parent in
            if !isDir {
                if !parent.files.contains(where: { $0.path == event.path }) {
                    parent.files.append(FileTreeFile(name: leaf, path: event.path, extension: (leaf as NSString).pathExtension, language: "", size: 0, linesEstimated: 0, role: ""))
                    parent.totalFileCount += 1
                }
            } else if !parent.directories.contains(where: { $0.name == leaf }) {
                parent.directories.append(FileTreeDirectory(name: leaf, path: event.path, role: ""))
                parent.totalFileCount += 1
            }
        }
    }

    private mutating func mutatePath(_ dirParts: [String], _ body: (inout FileTreeDirectory) -> Void) {
        mutatePath(dirParts, childPath: nil, body)
    }

    /// Recursively locate (or create) directory nodes for `dirParts`, giving
    /// every created node its CORRECT full relative `path` (used as the
    /// `Identifiable` id). The old implementation assigned the remaining
    /// segments (`dirParts.joined("/")`) which produced identity collisions
    /// (e.g. a node named "rpi" with path "rpi/hal" and a node named "hal"
    /// with path "hal") — SwiftUI then rendered duplicate/repeated folders
    /// like `rpi/hal/rpi`.
    private mutating func mutatePath(_ dirParts: [String], childPath: String?, _ body: (inout FileTreeDirectory) -> Void) {
        guard let head = dirParts.first, !head.isEmpty else { body(&self); return }
        let rest = Array(dirParts.dropFirst())

        // The parent node's real relative path ("." for the scan root), so the
        // created child gets path "rpi", then "rpi/hal", … — never "rpi/hal"
        // for a node just named "rpi".
        let base = childPath ?? (path == "." || path.isEmpty ? "" : path)
        let nextPath = base.isEmpty ? head : base + "/" + head

        if let idx = directories.firstIndex(where: { $0.name == head }) {
            directories[idx].mutatePath(rest, childPath: nextPath, body)
        } else {
            var newDir = FileTreeDirectory(name: head, path: nextPath, role: "")
            newDir.mutatePath(rest, childPath: nextPath, body)
            directories.append(newDir)
        }
    }
}

/// A file node in the project file tree.
struct FileTreeFile: Codable, Identifiable {
    var id: String { path }
    let name: String
    let path: String
    let `extension`: String
    let language: String
    let size: Int
    let linesEstimated: Int
    let role: String

    enum CodingKeys: String, CodingKey {
        case name, path
        case `extension`
        case language, size
        case linesEstimated = "lines_estimated"
        case role
    }
}