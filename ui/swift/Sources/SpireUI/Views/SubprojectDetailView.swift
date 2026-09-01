import SwiftUI
import HighlightSwift

/// Two-pane detail view for a selected subproject — shows file tree and file content.
struct SubprojectDetailView: View {
    @Environment(AppTheme.self) private var theme
    let subproject: SubprojectInfo
    let fileTree: FileTreeDirectory?
    @Environment(SpireBridge.self) private var bridge
    @State private var selectedFilePath: String? = nil
    @State private var fileContent: String? = nil
    @State private var fileContentLoading: Bool = false
    @State private var selectedTab: String = "Sources"

    private let tabs = ["Sources", "Dependencies", "Build"]

    var body: some View {
        GeometryReader { geo in
            HStack(spacing: 0) {
                // Left pane (40%)
                VStack(alignment: .leading, spacing: 0) {
                    // Header
                    HStack(alignment: .center, spacing: 12) {
                        Text(subproject.name)
                            .font(.system(size: 28, weight: .medium))
                        Text(badgeLabel(for: subproject.buildSystem))
                            .font(.caption.weight(.semibold))
                            .foregroundColor(.white)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3)
                            .background(badgeColor(for: subproject.buildSystem))
                            .cornerRadius(4)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding()

                    // Tab bar
                    HStack(spacing: 0) {
                        ForEach(tabs, id: \.self) { tab in
                            Button {
                                selectedTab = tab
                            } label: {
                                HStack(spacing: 4) {
                                    Image(systemName: tabSystemImage(tab))
                                        .font(.caption)
                                    Text(tab)
                                        .font(.subheadline.weight(.medium))
                                }
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 6)
                                .foregroundColor(selectedTab == tab ? theme.accent : theme.textSecondary)
                                .background(
                                    selectedTab == tab
                                        ? theme.accentBackground
                                        : Color.clear
                                )
                            }
                            .buttonStyle(.plain)
                        }
                    }
                    .padding(.horizontal, 8)

                    Divider()
                        .padding(.horizontal, 8)

                    // Tab content
                    switch selectedTab {
                    case "Sources":
                        if let root = subprojectTree() {
                            List {
                                Section(root.name) {
                                    SubFileTreeSection(directory: root, selectedFilePath: $selectedFilePath)
                                }
                            }
                            .listStyle(.sidebar)
                        } else {
                            Text("No file tree data")
                                .foregroundStyle(.tertiary)
                                .padding()
                        }

                    case "Dependencies":
                        if let deps = subproject.dependencies, !deps.isEmpty {
                            Table(deps) {
                                TableColumn("") { _ in
                                    Image(systemName: "shippingbox")
                                        .foregroundStyle(.secondary)
                                }
                                .width(24)
                                TableColumn("Name") { dep in
                                    Text(dep.name)
                                }
                                TableColumn("Version") { dep in
                                    Text(dep.version ?? "")
                                }
                            }
                        } else {
                            VStack(spacing: 8) {
                                Image(systemName: "shippingbox")
                                    .font(.system(size: 28))
                                    .foregroundStyle(.tertiary)
                                Text("No dependencies")
                                    .font(.headline)
                                    .foregroundStyle(.secondary)
                            }
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                        }

                    case "Build":
                        if let buildFile = buildFile() {
                            List {
                                Button {
                                    selectedFilePath = buildFilePath(buildFile)
                                } label: {
                                    HStack(spacing: 6) {
                                        Image(systemName: "hammer")
                                            .foregroundColor(selectedFilePath == buildFilePath(buildFile) ? theme.accent : theme.textSecondary)
                                            .font(.caption)
                                        Text(buildFile)
                                            .font(.callout)
                                            .foregroundColor(selectedFilePath == buildFilePath(buildFile) ? .primary : .secondary)
                                        Spacer(minLength: 0)
                                    }
                                }
                                .buttonStyle(.plain)
                            }
                            .listStyle(.sidebar)
                        } else {
                            VStack(spacing: 8) {
                                Image(systemName: "hammer")
                                    .font(.system(size: 28))
                                    .foregroundStyle(.tertiary)
                                Text("No build file")
                                    .font(.headline)
                                    .foregroundStyle(.secondary)
                            }
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                        }

                    default:
                        EmptyView()
                    }
                }
                .frame(width: geo.size.width * 0.4)
                .background(theme.surface)

                // Divider
                Rectangle()
                    .fill(theme.divider)
                    .frame(width: 1)

                // Right pane (60%)
                VStack(alignment: .leading, spacing: 0) {
                    if let path = selectedFilePath {
                        HStack {
                            Image(systemName: "doc.fill")
                                .foregroundStyle(.secondary)
                                .font(.caption)
                            Text(path.split(separator: "/").last.map(String.init) ?? path)
                                .font(.headline)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding()

                        if fileContentLoading {
                            ProgressView()
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                        } else if let content = fileContent {
                            ScrollView([.horizontal, .vertical]) {
                    Text(SyntaxHighlighter.highlight(content, language: SyntaxLanguage.detect(from: path)))
                        .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(12)
                            }
                        } else {
                            Text("Could not load file")
                                .foregroundStyle(.tertiary)
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                        }
                    } else {
                        VStack(spacing: 8) {
                            Image(systemName: "doc.text.magnifyingglass")
                                .font(.system(size: 32))
                                .foregroundStyle(.tertiary)
                            Text("Select a file to view its content")
                                .font(.headline)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                }
                .frame(width: geo.size.width * 0.6)
                .background(theme.textBackground)
            }
        }
        .onChange(of: selectedFilePath) { _, newPath in
            guard let path = newPath else {
                fileContent = nil
                return
            }
            loadFile(at: path)
        }
    }

    /// The build file name for this subproject's build system.
    private func buildFile() -> String? {
        switch subproject.buildSystem {
        case "Cargo":        return "Cargo.toml"
        case "SwiftPM", "Xcode": return "Package.swift"
        case "npm", "pnpm", "yarn": return "package.json"
        default:             return nil
        }
    }

    /// Full relative path to the build file (e.g. "crates/spire-core/Cargo.toml").
    private func buildFilePath(_ file: String) -> String {
        let clean = subproject.path.hasSuffix("/") ? String(subproject.path.dropLast()) : subproject.path
        return clean.isEmpty ? file : "\(clean)/\(file)"
    }

    private func tabSystemImage(_ tab: String) -> String {
        switch tab {
        case "Sources":      return "doc.plaintext"
        case "Dependencies": return "shippingbox"
        case "Build":        return "hammer"
        default:             return "questionmark"
        }
    }

    private func loadFile(at path: String) {
        fileContentLoading = true
        fileContent = nil
        Task {
            let absolutePath: String
            if path.hasPrefix("/") {
                absolutePath = path
            } else if let root = bridge.projectInfo?.root {
                let rc = root.hasSuffix("/") ? String(root.dropLast()) : root
                absolutePath = rc + "/" + path
            } else {
                absolutePath = path
            }
            // silent: print("[SubprojectDetail] loading: \(absolutePath)")
            let content = await bridge.readFile(at: absolutePath)
            await MainActor.run {
                fileContent = content
                fileContentLoading = false
            }
        }
    }

    private func subprojectTree() -> FileTreeDirectory? {
        guard let root = fileTree else { return nil }
        let cleanPath = subproject.path.hasSuffix("/") ? String(subproject.path.dropLast()) : subproject.path
        if cleanPath.isEmpty { return nil }
        let parts = cleanPath.split(separator: "/").map(String.init)
        return findSubdir(in: root, parts: parts)
    }

    private func findSubdir(in dir: FileTreeDirectory, parts: [String]) -> FileTreeDirectory? {
        guard !parts.isEmpty else { return dir }
        let head = parts[0]
        let rest = Array(parts.dropFirst())
        for child in dir.directories where child.name == head {
            return findSubdir(in: child, parts: rest)
        }
        return nil
    }

    private func badgeLabel(for bs: String) -> String {
        switch bs {
        case "Cargo":          return "Rust"
        case "SwiftPM","Xcode": return "Swift"
        case "npm","pnpm","yarn": return "JS"
        default:               return bs.isEmpty ? "?" : bs
        }
    }

    private func badgeColor(for bs: String) -> Color {
        switch bs {
        case "Cargo":          return Color(red: 0.70, green: 0.25, blue: 0.15)
        case "SwiftPM","Xcode": return Color(red: 0.90, green: 0.55, blue: 0.10)
        case "npm","pnpm","yarn": return Color(red: 0.30, green: 0.65, blue: 0.30)
        default:               return Color(red: 0.50, green: 0.55, blue: 0.60)
        }
    }
}

/// A recursive disclosure group for a directory and its children.
struct SubFileTreeSection: View {
    @Environment(AppTheme.self) private var theme
    let directory: FileTreeDirectory
    @Binding var selectedFilePath: String?

    var body: some View {
        ForEach(directory.directories.sorted(by: { $0.name < $1.name })) { subdir in
            DisclosureGroup {
                SubFileTreeSection(directory: subdir, selectedFilePath: $selectedFilePath)
            } label: {
                HStack(spacing: 4) {
                    Image(systemName: "folder.fill")
                        .foregroundStyle(.secondary)
                        .font(.caption)
                    Text(subdir.name)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
        }

        ForEach(directory.files.sorted(by: { $0.name < $1.name })) { file in
            Button {
                selectedFilePath = file.path
            } label: {
                HStack(spacing: 4) {
                    Image(systemName: selectedFilePath == file.path ? "doc.fill" : "doc")
                        .foregroundColor(selectedFilePath == file.path ? theme.accent : theme.textTertiary)
                        .font(.caption)
                    Text(file.name)
                        .font(.callout)
                        .foregroundColor(selectedFilePath == file.path ? .primary : .secondary)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.leading, 8)
                .padding(.vertical, 1)
            }
            .buttonStyle(.plain)
        }
    }
}