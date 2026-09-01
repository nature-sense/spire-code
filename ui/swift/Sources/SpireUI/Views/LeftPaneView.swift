import SwiftUI

/// Simple file/directory tree browser that selects a file path.
/// When `onOpenFile` is non-nil, clicking a file calls it (floating-window
/// behavior); otherwise the file path is written to `selectedFilePath`.
struct FileTreeBrowser: View {
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme
    let tree: FileTreeDirectory
    @Binding var selectedFilePath: String?
    /// Optional callback used instead of selecting — opens the file in a
    /// floating window.
    var onOpenFile: ((String) -> Void)? = nil

    var body: some View {
        List {
            ForEach(tree.directories) { dir in
                DirectoryRow(dir: dir, selectedFilePath: $selectedFilePath, onOpenFile: onOpenFile)
            }
            ForEach(tree.files) { file in
                Button {
                    if let onOpenFile {
                        onOpenFile(file.path)
                    } else {
                        selectedFilePath = file.path
                    }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: fileIcon(for: file.extension)).foregroundStyle(.secondary)
                        Text(file.name).font(.callout)
                            .foregroundStyle(selectedFilePath == file.path ? theme.accent : theme.textPrimary)
                        if let badge = bridge.diagnosticBadges[file.path] {
                            diagnosticBadgeView(badge)
                        }
                    }
                }
                .buttonStyle(.plain)
            }
        }
        .listStyle(.sidebar)
        // macOS sidebar lists draw a translucent material (can pick up a
        // greenish tint). Make the list background transparent so the opaque
        // pane background (theme.background) shows through instead.
        .scrollContentBackground(.hidden)
    }

    /// Render ⚠️/❌ severity counts next to a file name.
    @ViewBuilder
    private func diagnosticBadgeView(_ counts: [String: Int]) -> some View {
        let errors = counts["error"] ?? 0
        let warnings = counts["warning"] ?? 0
        if errors > 0 {
            Image(systemName: "xmark.circle.fill")
                .font(.system(size: 9)).foregroundStyle(.red)
            Text("\(errors)").font(.caption2.weight(.semibold)).foregroundStyle(.red)
        }
        if warnings > 0 {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 9)).foregroundStyle(.yellow)
            Text("\(warnings)").font(.caption2.weight(.semibold)).foregroundStyle(.yellow)
        }
    }

    private func fileIcon(for ext: String) -> String {
        switch ext.lowercased() {
        case "swift", "rs", "py": return "chevron.left.forwardslash.chevron.right"
        default: return "doc"
        }
    }
}

/// Recursive directory row — allows OutlineGroup/indentation via nested DisclosureGroup.
private struct DirectoryRow: View {
    @Environment(AppTheme.self) private var theme
    let dir: FileTreeDirectory
    @Binding var selectedFilePath: String?
    /// Passed through so nested files also open in a floating window when set.
    var onOpenFile: ((String) -> Void)? = nil

    var body: some View {
        DisclosureGroup {
            ForEach(dir.directories) { sub in
                DirectoryRow(dir: sub, selectedFilePath: $selectedFilePath, onOpenFile: onOpenFile)
            }
            ForEach(dir.files) { file in
                Button {
                    if let onOpenFile {
                        onOpenFile(file.path)
                    } else {
                        selectedFilePath = file.path
                    }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "doc").foregroundStyle(.secondary)
                        Text(file.name).font(.callout)
                            .foregroundStyle(selectedFilePath == file.path ? theme.accent : theme.textPrimary)
                    }
                }
                .buttonStyle(.plain)
            }
        } label: {
            Label(dir.name, systemImage: "folder").font(.callout)
        }
    }
}