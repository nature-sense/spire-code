import SwiftUI
import AppKit

/// Opens the tool overview as a floating window dialog — same pattern as
/// `PlatformPortal` / `RagPortal` so the Tools button behaves like the other
/// icon-rail items instead of replacing the main workspace.
enum ToolsPortal {
    private static var windows: [NSWindow] = []

    @MainActor static func open(bridge: SpireBridge, theme: AppTheme) {
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1000, height: 600),
                         styleMask: [.titled, .closable, .resizable, .miniaturizable],
                         backing: .buffered, defer: false)
        w.title = "Tools"; w.isReleasedWhenClosed = false
        // Floating windows don't inherit the main window's SwiftUI environment.
        let view = ToolsOverviewView().environment(bridge).environment(theme)
        w.contentViewController = NSHostingController(rootView: view)
        w.setContentSize(NSSize(width: 1000, height: 600))
        if let m = NSApp.mainWindow?.frame { w.setFrameOrigin(NSPoint(x: m.midX - 500, y: m.midY - 300)) } else { w.center() }
        w.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true)
        windows.append(w)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            windows.removeAll { $0 === w }
        }
    }
}

/// Full-screen overview of all available tools: core built-in tools, build-system
/// tools (grouped by build type), and general-purpose MCP servers — each in its
/// own scrollable column, all tools expanded and visible.
struct ToolsOverviewView: View {
    @Environment(SpireBridge.self) private var bridge


    private var coreServer: McpServerInfo? {
        bridge.mcpServers.first { $0.name == "spire" }
    }

    private var buildServers: [McpServerInfo] {
        bridge.mcpServers.filter { $0.name != "spire" && $0.buildType != nil }
    }

    private var generalServers: [McpServerInfo] {
        bridge.mcpServers.filter { $0.name != "spire" && $0.buildType == nil }
    }

    private var buildTypes: [String] {
        Array(Set(buildServers.compactMap { $0.buildType })).sorted()
    }

    // MARK: - Tool filtering

    private var coreTools: [McpToolInfo] {
        bridge.allTools.filter { tool in
            tool.name.hasPrefix("filesystem_")
                || tool.name.hasPrefix("git_")
                || tool.name.hasPrefix("process_")
                || tool.name.hasPrefix("search_")
                || tool.name.hasPrefix("search/")   // web search tools (search/web, search/wikipedia…)
                || tool.name.hasPrefix("terminal_")
                || tool.name.hasPrefix("system/")
                || tool.name.hasPrefix("chat/")
                || tool.name.hasPrefix("tools/")
        }
    }

    private var buildTools: [McpToolInfo] {
        bridge.allTools.filter { $0.name.hasPrefix("build_") }
    }

    private func mcpTools(for buildType: String) -> [McpToolInfo] {
        let serverNames = Set(buildServers.filter { $0.buildType == buildType }.map(\.name))
        return bridge.allTools.filter { tool in
            tool.name.contains("/") && serverNames.contains(String(tool.name.split(separator: "/").first ?? ""))
        }
    }

    private func generalServerTools(for serverName: String) -> [McpToolInfo] {
        bridge.allTools.filter { tool in
            tool.name.contains("/") && tool.name.hasPrefix("\(serverName)/")
        }
    }

    // MARK: - View

    var body: some View {
        HStack(spacing: 0) {
            // Column 1: Core
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    columnHeader(title: "Core", icon: "wrench.and.screwdriver", color: .blue)
                    if bridge.allToolsLoading && coreTools.isEmpty {
                        ProgressView().padding()
                    } else if coreTools.isEmpty {
                        Text("No core tools")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .padding()
                    } else {
                        ForEach(coreTools) { tool in
                            FlatToolRow(tool: tool)
                        }
                    }
                }
            }

            Divider()

            // Column 2: Build
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    columnHeader(title: "Build", icon: "hammer", color: .orange)

                    // Generic build tools (build_analyze, etc.)
                    ForEach(buildTools) { tool in
                        FlatToolRow(tool: tool)
                    }

                    // Build-type sections
                    ForEach(buildTypes, id: \.self) { buildType in
                        let servers = buildServers.filter { $0.buildType == buildType }
                        sectionHeader(title: buildType, icon: "hammer.fill")
                        ForEach(servers) { server in
                            subHeader(title: server.name, toolCount: server.toolCount)
                            ForEach(mcpTools(for: buildType)) { tool in
                                FlatToolRow(tool: tool)
                            }
                        }
                    }
                }
            }

            Divider()

            // Column 3: General MCP
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    columnHeader(title: "General MCP", icon: "server.rack", color: .purple)
                    if generalServers.isEmpty {
                        Text("No general MCP servers")
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .padding()
                    } else {
                        ForEach(generalServers) { server in
                            subHeader(title: server.name, toolCount: server.toolCount)
                            ForEach(generalServerTools(for: server.name)) { tool in
                                FlatToolRow(tool: tool)
                            }
                        }
                    }
                }
            }
        }
        .task {
            // Fetch all tools regardless of server count
            await bridge.fetchAllTools()
            // Retry servers a few times since MCP servers start asynchronously
            for _ in 0..<3 {
                await bridge.fetchMcpServers()
                if !bridge.mcpServers.isEmpty { break }
                if bridge.mcpServersError != nil { break }
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
            await bridge.fetchAllTools()
        }
    }

    private func columnHeader(title: String, icon: String, color: Color) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .foregroundStyle(color)
                .font(.headline)
            Text(title)
                .font(.headline)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }

    private func sectionHeader(title: String, icon: String) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .foregroundStyle(.secondary)
                .font(.subheadline.weight(.semibold))
            Text(title)
                .font(.subheadline.weight(.semibold))
            BuildTypeBadge(buildType: title)
            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.top, 14)
        .padding(.bottom, 4)
    }

    private func subHeader(title: String, toolCount: Int) -> some View {
        HStack(spacing: 4) {
            Text(title)
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
            Text("\(toolCount)")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.tertiary)
                .padding(.horizontal, 5)
                .padding(.vertical, 1)
                .background(.quaternary, in: Capsule())
        }
        .padding(.horizontal, 12)
        .padding(.top, 10)
        .padding(.bottom, 2)
    }
}

/// A flat tool row: monospaced name with description underneath — no collapse.
struct FlatToolRow: View {
    let tool: McpToolInfo

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(tool.name)
                .font(.subheadline.monospaced())
                .foregroundStyle(.primary)
            if let desc = tool.description, !desc.isEmpty {
                Text(desc)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 4)
    }
}

/// Colored capsule badge for a build-system name.
struct BuildTypeBadge: View {
    let buildType: String

    private var color: Color {
        switch buildType.lowercased() {
        case "cargo", "rust": return .orange
        case "npm", "node", "yarn", "pnpm", "bun": return .green
        case "swift", "swiftpm", "spm": return .orange
        case "python", "pip", "poetry", "uv": return .blue
        case "cmake", "make": return .indigo
        case "gradle": return .purple
        case "maven", "mvn": return .red
        case "go", "golang": return .teal
        case "ruby", "gem", "bundler": return .red
        default: return .purple
        }
    }

    var body: some View {
        Text(buildType)
            .font(.caption2.weight(.semibold))
            .foregroundStyle(color)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.12), in: Capsule())
    }
}