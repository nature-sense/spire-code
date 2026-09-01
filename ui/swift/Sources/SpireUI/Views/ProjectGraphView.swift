import SwiftUI

/// True graph renderer using SwiftUI Canvas: draws nodes with boxes/icons/labels
/// and connects them with Path edge lines. Supports tap selection.
/// Layout: root on the left, tree grows rightward. Nodes are compact/tall with
/// strong white rounded borders.
struct ProjectGraphView: View {
    let project: ProjectInfo
    @Binding var selectedSubproject: SubprojectInfo?
    @Binding var selectedBuildTarget: String?
    @Environment(SpireBridge.self) private var bridge
    @Environment(AppTheme.self) private var theme

    /// The virtual coordinate size of the graph. Two columns only in the
    /// target-centric view: root (x=100) + target column (x=400).
    private let virtualSize = CGSize(width: 560, height: 600)

    // Column x positions (left → right). In the target-centric view we only
    // need TWO columns: the project root (left) and its build targets (right).
    private let rootX: CGFloat = 100
    private let targetX: CGFloat = 400

    /// Build the node/edge model from project data, with a horizontal (left→right)
    /// layered layout.
    private func graphModel() -> (nodes: [GraphNodeData], edges: [GraphEdgeData]) {
        // ── Build nodes + edges (same grouping as before) ──
        var nodes: [GraphNodeData] = []
        var edges: [GraphEdgeData] = []

        // Root node — name only (no huge directory subtitle).
        let rootId = "root"
        nodes.append(GraphNodeData(
            id: rootId,
            parentId: nil,
            title: project.name,
            subtitle: nil,
            icon: "square.grid.2x2",
            color: theme.accent,
            kind: .root
        ))

        // Build targets are NOT graph nodes — a project is ONE node here.
        // Per-platform targets (rpi5, rock3c, ...) are selected in the right
        // Build pane, never as boxed subprojects (they are cross-compiler
        // settings for the same source, not distinct subprojects).
        // Non-directory subprojects are also not rendered as nodes: a single
        // project with cross targets has exactly one root node. Projects with
        // genuinely separate member subprojects (real Cargo/Meson workspaces)
        // remain visible through the left pane tree, not the graph.

        // ── Horizontal layout: root (left) → build targets (right). ──
        var positioned = nodes

        // Root at center-left.
        if let idx = positioned.firstIndex(where: { $0.id == rootId }) {
            positioned[idx].position = CGPoint(x: rootX, y: virtualSize.height / 2)
        }

        // Everything else (build targets, or subproject fallback) fans out
        // vertically to the right of the root in a single column.
        let children = positioned.filter { $0.parentId == rootId }
        for (i, node) in children.enumerated() {
            guard let idx = positioned.firstIndex(where: { $0.id == node.id }) else { continue }
            let y = virtualSize.height / 2 + (CGFloat(i) - (CGFloat(children.count) - 1) / 2) * 120
            positioned[idx].position = CGPoint(x: targetX, y: min(max(40, y), virtualSize.height - 40))
        }

        positioned.sort { $0.id < $1.id }
        var sortedEdges = edges
        sortedEdges.sort { $0.from < $1.from }

        return (positioned, sortedEdges)
    }

    var body: some View {
        GeometryReader { geo in
            let model = graphModel()
            let scale = min(
                geo.size.width / virtualSize.width,
                geo.size.height / virtualSize.height
            )
            // Center the virtual canvas inside the pane — keeps the graph's
            // aspect ratio and its own center aligned with the pane center.
            let offset = CGPoint(
                x: (geo.size.width - virtualSize.width * scale) / 2,
                y: (geo.size.height - virtualSize.height * scale) / 2
            )

            ZStack {
                // Edges layer (Canvas)
                Canvas { ctx, size in
                    for edge in model.edges {
                        guard let fromNode = model.nodes.first(where: { $0.id == edge.from }),
                              let toNode = model.nodes.first(where: { $0.id == edge.to }) else { continue }
                        let from = scaled(fromNode.position, scale: scale, offset: offset)
                        let to = scaled(toNode.position, scale: scale, offset: offset)
                        // Horizontal curve: connect node right edge → child left edge.
                        // Node boxes are 96pt wide, so edges start/end at ±48.
                        let start = CGPoint(x: from.x + 48, y: from.y)
                        let end = CGPoint(x: to.x - 48, y: to.y)
                        var path = Path()
                        path.move(to: start)
                        let midX = (start.x + end.x) / 2
                        path.addCurve(
                            to: end,
                            control1: CGPoint(x: midX, y: start.y),
                            control2: CGPoint(x: midX, y: end.y)
                        )
                        ctx.stroke(path, with: .color(theme.graphEdge), lineWidth: 1)
                    }
                }
                .frame(width: geo.size.width, height: geo.size.height)
                .allowsHitTesting(false)

                // Nodes layer (SwiftUI views for tap handling)
                ForEach(model.nodes) { node in
                    GraphNodeView(
                        data: node,
                        selected: isSelected(node)
                    ) {
                        handleTap(node)
                    }
                    .position(
                        scaled(node.position, scale: scale, offset: offset)
                    )
                    .zIndex(1)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
        .background(theme.background)
    }

    private func scaled(_ p: CGPoint, scale: CGFloat, offset: CGPoint) -> CGPoint {
        CGPoint(x: p.x * scale + offset.x, y: p.y * scale + offset.y)
    }

    /// Kind-aware selection highlight. Build targets (platforms like rpi5 /
    /// rock3c) highlight ONLY by their own name — never by shared parent
    /// subproject — so a tap on one platform doesn't light up all siblings.
    private func isSelected(_ node: GraphNodeData) -> Bool {
        switch node.kind {
        case .buildTarget:
            return selectedBuildTarget == node.buildTarget?.name
        case .subproject:
            return selectedSubproject?.id == node.subproject?.id
        case .root:
            return selectedSubproject == nil && selectedBuildTarget == nil
        case .directory:
            return false
        }
    }

    private func handleTap(_ node: GraphNodeData) {
        switch node.kind {
        case .root:
            // The project root is the whole-project scope — clear target and
            // subproject selection so the panes show the project overview.
            selectedBuildTarget = nil
            selectedSubproject = nil
            bridge.selectSubproject(nil)
        case .subproject:
            if let sub = node.subproject {
                // Fallback path only (when no build targets were parsed).
                selectedSubproject = sub
                bridge.selectSubproject(sub)
            }
        case .directory:
            // Directory nodes no longer rendered in the graph.
            break
        case .buildTarget:
            // A build target is the primary selection — it drives the center
            // (files/deps/build) and right (build actions) panes directly.
            // ALSO select its parent subproject so the center/right panes
            // (which gate on selectedSubproject) render content.
            if let target = node.buildTarget {
                selectedBuildTarget = target.name
                if let parentSub = node.subproject {
                    selectedSubproject = parentSub
                    bridge.selectSubproject(parentSub)
                }
            }
        }
    }

    /// Short display label for a build target. Leaves names that don't start
    /// with `ai-trap-` unchanged so user-named targets render verbatim.
    private func displayName(for target: String) -> String {
        target.hasPrefix("ai-trap-") ? String(target.dropFirst("ai-trap-".count)) : target
    }

    private func subprojectIcon(_ bs: String) -> String {
        switch bs {
        case "Cargo": return "gearshape.fill"
        case "SwiftPM", "Xcode": return "hammer.fill"
        case "npm", "pnpm", "yarn": return "square.and.pencil"
        default: return "folder.fill"
        }
    }

    private func subprojectColor(_ bs: String) -> Color {
        switch bs {
        case "Cargo": return Color(red: 0.70, green: 0.25, blue: 0.15)
        case "SwiftPM", "Xcode": return .orange
        case "npm", "pnpm", "yarn": return .green
        default: return .gray
        }
    }
}

// MARK: - Model

enum GraphNodeKind: Hashable {
    case root, directory, subproject, buildTarget
}

/// A node in the graph.
struct GraphNodeData: Identifiable {
    let id: String
    var parentId: String?
    var title: String
    var subtitle: String?
    var icon: String
    var color: Color
    var kind: GraphNodeKind
    var subproject: SubprojectInfo?
    /// The Meson/Cargo etc. build target this node represents (nil for
    /// root/directory/subproject nodes).
    var buildTarget: BuildTarget?
    var position: CGPoint = .zero
}

/// A directed edge between two node ids.
struct GraphEdgeData {
    let from: String
    let to: String
}

// MARK: - Node View

/// Rendered node: icon on top, label below — compact width, taller height.
/// Strong white border with slightly rounded corners.
private struct GraphNodeView: View {
    @Environment(AppTheme.self) private var theme
    let data: GraphNodeData
    let selected: Bool
    let action: () -> Void

    private var kindColor: Color {
        switch data.kind {
        case .root: return data.color
        case .directory: return .blue
        case .subproject: return data.color
        case .buildTarget: return .gray
        }
    }

    var body: some View {
        Button(action: {
            // silent: print("[GraphNodeView] button pressed for node: \(data.title) kind=\(data.kind)")
            action()
        }) {
            VStack(spacing: 4) {
                Image(systemName: data.icon)
                    .font(.system(size: 20))
                    .foregroundStyle(.white)
                    .frame(width: 28, height: 28)
                    .background(RoundedRectangle(cornerRadius: 6).fill(kindColor))
                Text(data.title)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(2)
                    .multilineTextAlignment(.center)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .frame(width: 96)
            .background(
                RoundedRectangle(cornerRadius: 7)
                    .fill(selected ? theme.accentBackground : theme.nodeBackground)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 7)
                    .stroke(theme.nodeBorder, lineWidth: selected ? 2 : 1)
            )
        }
        .buttonStyle(.plain)
    }
}
