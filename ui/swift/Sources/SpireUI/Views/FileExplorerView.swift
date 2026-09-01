import SwiftUI
import HighlightSwift

/// Classic file explorer mode — directory tree with file viewer/editor.
struct FileExplorerView: View {
    let project: ProjectInfo
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    @State private var selectedFilePath: String?

    var body: some View {
        NavigationSplitView {
            // Sidebar: full directory tree
            if let tree = project.fileTree {
                FileTreeSidebar(projectName: project.name, directory: tree, selectedPath: $selectedFilePath)
                    .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 400)
            } else {
                ContentUnavailableView(
                    "No file tree",
                    systemImage: "tree",
                    description: Text("File tree not available in project analysis")
                )
            }
        } detail: {
            if let path = selectedFilePath {
                FileViewerPanel(filePath: path)
            } else {
                ContentUnavailableView(
                    "Select a file",
                    systemImage: "doc.text",
                    description: Text("Click any file in the explorer to view its contents")
                )
            }
        }
    }
}

/// Recursive sidebar tree — files and directories at the top level shown directly.
struct FileTreeSidebar: View {
    @Environment(AppTheme.self) private var theme
    let projectName: String
    let directory: FileTreeDirectory
    @Binding var selectedPath: String?

    var body: some View {
        List {
            // Project name header
            HStack(spacing: 6) {
                Image(systemName: "square.grid.2x2")
                    .font(.body.weight(.medium))
                Text(projectName)
                    .font(.body.weight(.semibold))
            }
            .padding(.bottom, 4)

            // Top-level directories
            ForEach(directory.directories.sorted(by: { $0.name < $1.name })) { subdir in
                DisclosureGroup {
                    FileTreeSection(directory: subdir, selectedPath: $selectedPath)
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "folder.fill")
                            .foregroundStyle(.secondary)
                            .font(.body)
                        Text(subdir.name)
                            .font(.body)
                            .foregroundStyle(.secondary)
                    }
                }
            }

            // Top-level files
            ForEach(directory.files.sorted(by: { $0.name < $1.name })) { file in
                fileRow(file: file)
            }
        }
        .listStyle(.sidebar)
    }

    private func fileRow(file: FileTreeFile) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon(for: file.`extension`))
                .foregroundStyle(theme.fileIconColor(for: file.`extension`))
                .font(.body)
            Text(file.name)
                .font(.body)
                .fontWeight(selectedPath == file.path ? .semibold : .regular)
                .lineLimit(1)
        }
        .padding(.leading, 4)
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .onTapGesture {
            selectedPath = file.path
        }
    }

    private func icon(for ext: String) -> String {
        switch ext {
        case "swift": return "swift"
        case "rs": return "rust"
        case "toml", "yml", "yaml", "json": return "gearshape"
        case "md", "txt": return "doc.text"
        default: return "doc"
        }
    }

}

/// A recursive disclosure group for a directory and its children.
struct FileTreeSection: View {
    @Environment(AppTheme.self) private var theme
    let directory: FileTreeDirectory
    @Binding var selectedPath: String?

    var body: some View {
        ForEach(directory.directories.sorted(by: { $0.name < $1.name })) { subdir in
            DisclosureGroup {
                FileTreeSection(directory: subdir, selectedPath: $selectedPath)
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "folder.fill")
                        .foregroundStyle(.secondary)
                        .font(.body)
                    Text(subdir.name)
                        .font(.body)
                        .foregroundStyle(.secondary)
                }
            }
        }

        ForEach(directory.files.sorted(by: { $0.name < $1.name })) { file in
            HStack(spacing: 6) {
                Image(systemName: icon(for: file.`extension`))
                    .foregroundStyle(theme.fileIconColor(for: file.`extension`))
                    .font(.body)
                Text(file.name)
                    .font(.body)
                    .fontWeight(selectedPath == file.path ? .semibold : .regular)
                    .lineLimit(1)
            }
            .padding(.leading, 12)
            .padding(.vertical, 2)
            .contentShape(Rectangle())
            .onTapGesture {
                selectedPath = file.path
            }
        }
    }

    private func icon(for ext: String) -> String {
        switch ext {
        case "swift": return "swift"
        case "rs": return "rust"
        case "toml", "yml", "yaml", "json": return "gearshape"
        case "md", "txt": return "doc.text"
        default: return "doc"
        }
    }

}

/// Read-only file viewer pane that uses the backend to fetch file content.
struct FileViewerPanel: View {
    let filePath: String
    @State private var content: String?
    @State private var error: String?
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    var body: some View {
        ScrollView([.horizontal, .vertical]) {
            if let content {
            Text(SyntaxHighlighter.highlight(content, language: SyntaxLanguage.detect(from: filePath)))
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(12)
            } else if let error {
                ContentUnavailableView(
                    "Error",
                    systemImage: "exclamationmark.triangle",
                    description: Text(error)
                )
            } else {
                ProgressView("Loading...")
                    .padding()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(theme.textBackground)
        .task(id: filePath) {
            // Triggers on initial display AND whenever filePath changes
            content = nil
            error = nil
            await loadFile(filePath: filePath)
        }
    }

    private func loadFile(filePath path: String) async {
        // Build absolute path
        let absolutePath: String
        if path.hasPrefix("/") {
            absolutePath = path
        } else if let root = bridge.projectInfo?.root {
            let rc = root.hasSuffix("/") ? String(root.dropLast()) : root
            absolutePath = rc + "/" + path
        } else {
            absolutePath = path
        }
        // silent: print("[FileViewer] loading: \(absolutePath)")
        guard let text = await bridge.readFile(at: absolutePath) else {
            // silent: print("[FileViewer] readFile returned nil")
            error = "Failed to load file: \(filePath)"
            return
        }
        // silent: print("[FileViewer] got \(text.count) chars")
        content = text
    }
}
