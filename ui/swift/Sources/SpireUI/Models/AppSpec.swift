import Foundation

/// Lightweight view-model for a VALIDATED AppSpec returned by
/// `createProject/GenerateSpec`. The full spec JSON is retained verbatim (it
/// is passed straight back to `createProject/GenerateCode`); only a summary is
/// decoded for the wizard review.
struct AppSpecSummary {
    let name: String
    let goal: String
    let typeCount: Int
    let actorCount: Int
    let methodCount: Int
    let screenCount: Int
    let nodeCount: Int
    let edgeCount: Int

    let json: [String: Any]

    init?(json: [String: Any]) {
        guard let app = json["app"] as? [String: Any] else { return nil }
        self.json = json
        name = app["name"] as? String ?? ""
        goal = app["goal"] as? String ?? ""
        typeCount = (json["types"] as? [Any])?.count ?? 0
        actorCount = (json["actors"] as? [Any])?.count ?? 0
        methodCount = (json["bridge"] as? [Any])?.count ?? 0
        screenCount = (json["ui"] as? [Any])?.count ?? 0
        let graph = json["graph"] as? [String: Any]
        nodeCount = (graph?["nodes"] as? [Any])?.count ?? 0
        edgeCount = (graph?["edges"] as? [Any])?.count ?? 0
    }

    /// Human-readable headline for the review panel.
    var headline: String {
        "\(name) — \(goal.isEmpty ? "no goal" : goal)"
    }

    /// Compact summary rows used by the wizard's spec section.
    var rows: [(label: String, value: String)] {
        [
            ("Bridge methods", "\(methodCount)"),
            ("Actors", "\(actorCount)"),
            ("Domain types", "\(typeCount)"),
            ("Screens", "\(screenCount)"),
            ("Graph nodes", "\(nodeCount)"),
            ("Graph edges", "\(edgeCount)"),
        ]
    }

    /// Pretty-printed spec JSON for inspection.
    var prettyJSON: String {
        if let data = try? JSONSerialization.data(withJSONObject: json, options: [.prettyPrinted, .sortedKeys]),
           let text = String(data: data, encoding: .utf8) {
            return text
        }
        return "{}"
    }
}
