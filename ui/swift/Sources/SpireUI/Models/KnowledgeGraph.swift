import Foundation

/// Mirrors one node of the `project/queryGraph` / `project/getRelationships` payload.
///
/// The same shape answers both: a query returns nodes with optional `description`/`properties`, a
/// traversal returns nodes with the same identity fields and no properties. Only what the graph
/// pane draws is modelled — the rest is ignored rather than guessed at.
struct KnowledgeNode: Codable, Equatable, Identifiable {
    var id: String
    var name: String
    /// The node's type, e.g. `Project`, `Entity`, `Decision`, `Blocker`.
    var type: String
    /// The finer label, when the node has one (e.g. `Function`, `Class`).
    var subtype: String?
    var description: String?
    /// Present only when the query asked for it (`includeRelationships`).
    var relationships: [KnowledgeRelationship]?
}

/// One edge: what links two nodes, in one direction.
struct KnowledgeRelationship: Codable, Equatable {
    var id: String?
    /// The relationship type, e.g. `HasEdit`, `DependsOn`, `BelongsTo`.
    var type: String
    var fromId: String
    var toId: String
    var weight: Double?
}

/// Full `project/queryGraph` payload: the count and the matched nodes.
struct KnowledgeGraphResult: Codable, Equatable {
    var total: Int
    var nodes: [KnowledgeNode]
}

/// Full `project/getRelationships` payload: the anchor node, the traversed nodes and the edges
/// between them.
struct GraphRelationshipsResult: Codable, Equatable {
    var startNodeId: String
    var nodes: [KnowledgeNode]
    var relationships: [KnowledgeRelationship]
    var totalNodes: Int
    var totalRelationships: Int
}
