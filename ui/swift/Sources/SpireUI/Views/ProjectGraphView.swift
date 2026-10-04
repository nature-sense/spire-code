import SwiftUI
import Foundation

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

    /// Which graph the pane renders: the project's structural tree (built from `ProjectInfo` in
    /// memory), or the knowledge graph the core keeps (fetched from `project/queryGraph`).
    enum GraphMode: String, CaseIterable, Identifiable {
        case structure
        case knowledge

        var id: String { rawValue }

        var label: String { self == .structure ? "Structure" : "Knowledge graph" }
    }

    @State private var mode: GraphMode = .structure
    /// The knowledge graph, positioned for drawing — empty until knowledge mode is first shown.
    @State private var knowledgeGraphNodes: [GraphNodeData] = []
    @State private var knowledgeEdges: [GraphEdgeData] = []
    @State private var knowledgeLoading = false
    @State private var knowledgeError: String?
    /// The knowledge node the user tapped, if any — the knowledge graph's own selection.
    @State private var selectedKnowledgeNodeId: String?

    /// The virtual coordinate size of the graph. Two columns only in the
    /// target-centric view: root (x=100) + target column (x=400).
    private let virtualSize = CGSize(width: 560, height: 600)

    // Column x positions (left → right). In the target-centric view we only
    // need TWO columns: the project root (left) and its build targets (right).
    private let rootX: CGFloat = 100
    private let targetX: CGFloat = 400

    /// Build the node/edge model for the current mode.
    private func graphModel() -> (nodes: [GraphNodeData], edges: [GraphEdgeData]) {
        switch mode {
        case .structure: return structureModel()
        case .knowledge: return knowledgeModel()
        }
    }

    /// The structural model: project data with a horizontal (left→right) layered layout.
    private func structureModel() -> (nodes: [GraphNodeData], edges: [GraphEdgeData]) {
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
                        // Centre-to-centre: the node boxes are drawn over the edges, so each line
                        // shows only between its two boxes. That is direction-agnostic, which the
                        // knowledge graph needs — its edges are not a left→right tree.
                        let from = scaled(fromNode.position, scale: scale, offset: offset)
                        let to = scaled(toNode.position, scale: scale, offset: offset)
                        var path = Path()
                        path.move(to: from)
                        path.addLine(to: to)
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
        .overlay(alignment: .topLeading) {
            modePicker
        }
        .task(id: mode) {
            if mode == .knowledge { await loadKnowledgeGraph() }
        }
    }

    /// The mode toggle, and — in knowledge mode — what the graph reader found.
    private var modePicker: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picker("", selection: $mode) {
                ForEach(GraphMode.allCases) { m in
                    Text(m.label).tag(m)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 260)

            if mode == .knowledge {
                if knowledgeLoading {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Reading the knowledge graph…")
                            .font(.caption)
                            .foregroundStyle(theme.textSecondary)
                    }
                } else if let knowledgeError {
                    Text(knowledgeError).font(.caption).foregroundStyle(.red)
                } else if knowledgeGraphNodes.isEmpty {
                    Text("The knowledge graph is empty — nothing has been recorded yet.")
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                } else {
                    Text("\(knowledgeGraphNodes.count) node(s) · \(knowledgeEdges.count) edge(s)")
                        .font(.caption)
                        .foregroundStyle(theme.textSecondary)
                }
            }
        }
        .padding(8)
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
        case .knowledge:
            return selectedKnowledgeNodeId == node.id
        }
    }

    private func handleTap(_ node: GraphNodeData) {
        switch node.kind {
        case .root:
            // The project root is the whole-project scope — clear target and
            // subproject selection so the panes show the project overview.
            selectedBuildTarget = nil
            bridge.selectedBuildTarget = nil
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
                // Mirror to the shared bridge: the right-hand ContextActionPane
                // reads `bridge.selectedBuildTarget` (the graph's @Binding is
                // MainView-local), so without this the Build action never learns
                // which target is selected and the row is hidden.
                bridge.selectedBuildTarget = target.name
                if let parentSub = node.subproject {
                    selectedSubproject = parentSub
                    bridge.selectSubproject(parentSub)
                }
            }
        case .knowledge:
            // A knowledge node selects itself — it is not a subproject or a target, so it drives no
            // pane; the graph's own highlight is the whole effect.
            selectedKnowledgeNodeId = (selectedKnowledgeNodeId == node.id) ? nil : node.id
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

    // MARK: - Knowledge graph

    /// The knowledge model: nodes positioned ring by ring around the project node, with the typed
    /// edges the query reported.
    private func knowledgeModel() -> (nodes: [GraphNodeData], edges: [GraphEdgeData]) {
        (knowledgeGraphNodes.sorted { $0.id < $1.id },
         knowledgeEdges.sorted { $0.from < $1.from })
    }

    /// Read the knowledge graph and lay it out — run once per entry into knowledge mode.
    private func loadKnowledgeGraph() async {
        guard !knowledgeLoading else { return }
        knowledgeLoading = true
        knowledgeError = nil

        let query = await bridge.queryGraph(limit: 250, includeRelationships: true)
        var nodes = query?.nodes ?? []
        var edges: [GraphEdgeData] = []
        var ids = Set(nodes.map { $0.id })
        var seen = Set<String>()
        for node in nodes {
            for rel in node.relationships ?? [] {
                // The same edge is reported by both endpoints; keep it once.
                guard seen.insert("\(rel.fromId)->\(rel.toId):\(rel.type)").inserted else { continue }
                // A relationship to a node the query did not return is dropped rather than drawn to
                // nowhere.
                guard ids.contains(rel.fromId), ids.contains(rel.toId) else { continue }
                edges.append(GraphEdgeData(from: rel.fromId, to: rel.toId, label: rel.type))
            }
        }

        // Some graphs answer a bare query with nothing yet do have a traversal from the project node;
        // fall back to that rather than showing an empty pane.
        if nodes.isEmpty, let traversal = await bridge.graphRelationships(depth: 3, direction: "both") {
            nodes = traversal.nodes
            ids = Set(nodes.map { $0.id })
            let known = ids
            edges = traversal.relationships
                .filter { known.contains($0.fromId) && known.contains($0.toId) }
                .map { GraphEdgeData(from: $0.fromId, to: $0.toId, label: $0.type) }
        }

        let positioned = layoutKnowledge(nodes)
        let message: String? = (query == nil && positioned.isEmpty)
            ? "The knowledge graph could not be read."
            : nil
        await MainActor.run {
            knowledgeGraphNodes = positioned
            knowledgeEdges = edges.sorted { $0.from < $1.from }
            knowledgeError = message
            knowledgeLoading = false
        }
    }

    /// A ring layout: the `Project` node (or the first node) at the centre, everything else fanning
    /// out in rings of eight. The structural view's two columns assume a tree; the knowledge graph is
    /// not one, so it gets a layout that does not imply an order it does not have.
    private func layoutKnowledge(_ nodes: [KnowledgeNode]) -> [GraphNodeData] {
        guard !nodes.isEmpty else { return [] }
        let center = CGPoint(x: virtualSize.width / 2, y: virtualSize.height / 2)
        let ordered = nodes.sorted { a, b in
            let aProject = a.type == "Project"
            let bProject = b.type == "Project"
            if aProject != bProject { return aProject }
            return a.id < b.id
        }
        var out: [GraphNodeData] = []
        for (i, node) in ordered.enumerated() {
            var position = center
            if i > 0 {
                let index = i - 1
                let perRing = 8
                let ring = index / perRing
                let slot = index % perRing
                let radius = min(CGFloat(220), 110 + CGFloat(ring) * 80)
                let angle = (CGFloat(slot) / CGFloat(perRing)) * 2 * .pi
                    - .pi / 2 + CGFloat(ring) * 0.35
                position = CGPoint(
                    x: center.x + cos(angle) * radius,
                    y: center.y + sin(angle) * radius
                )
            }
            out.append(GraphNodeData(
                id: node.id,
                parentId: nil,
                title: node.name.isEmpty ? node.id : node.name,
                subtitle: node.subtype ?? node.type,
                icon: knowledgeIcon(node.type),
                color: knowledgeColor(node.type),
                kind: .knowledge,
                position: position
            ))
        }
        return out.sorted { $0.id < $1.id }
    }

    private func knowledgeIcon(_ type: String) -> String {
        switch type {
        case "Project": return "square.grid.2x2"
        case "Entity": return "cube.fill"
        case "Decision": return "checkmark.seal.fill"
        case "ActiveContext": return "scope"
        case "Blocker": return "exclamationmark.octagon.fill"
        case "Milestone": return "flag.fill"
        case "Standard": return "book.closed.fill"
        case "Conversation", "Session": return "bubble.left.and.text.bubble.right.fill"
        default: return "circle.grid.2x2.fill"
        }
    }

    private func knowledgeColor(_ type: String) -> Color {
        switch type {
        case "Project": return theme.accent
        case "Entity": return Color(red: 0.20, green: 0.45, blue: 0.75)
        case "Decision": return .green
        case "ActiveContext": return .teal
        case "Blocker": return .red
        case "Milestone": return .orange
        case "Standard": return .purple
        case "Conversation", "Session": return .pink
        default: return .gray
        }
    }
}

// MARK: - Model

enum GraphNodeKind: Hashable {
    case root, directory, subproject, buildTarget
    /// A node of the knowledge graph (`project/queryGraph`), drawn beside the structural tree.
    case knowledge
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
    /// The relationship type, when the edge came from the knowledge graph (`nil` for the structural
    /// tree, which draws no typed edges).
    var label: String? = nil
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
        case .knowledge: return data.color
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
