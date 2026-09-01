import Foundation

/// Domain service for project operations. Repository pattern: all project FFI
/// calls live here (single source of truth for project data), while the view
/// layer stays presentation-only.
actor ProjectService {
    let backend: any UIBackend

    init(backend: any UIBackend) {
        self.backend = backend
    }

    /// Analyze a project directory and return its full ProjectInfo.
    /// Throws on transport/decode failure.
    func analyze(projectRoot: String?) async throws -> ProjectInfo {
        let body: [String: Any] = [
            "method": "AnalyzeProject",
            "params": rootParams(projectRoot)
        ]
        let data = try JSONSerialization.data(withJSONObject: body)
        let reply = try await backend.send(data)
        return try MessageSerializer.decode(reply)
    }

    /// Open a project directory (creating it if needed). Initializes the graph
    /// database, runs analysis, and returns the decoded ProjectInfo.
    func open(root: String) async throws -> ProjectInfo {
        let body: [String: Any] = [
            "method": "project/open",
            "params": ["root": root]
        ]
        let data = try JSONSerialization.data(withJSONObject: body)
        let reply = try await backend.send(data)
        return try MessageSerializer.decode(reply)
    }

    /// Fetch target-scoped detail (deps, platform, files) for a build target
    /// from the knowledge graph via `project/getBuildTarget`.
    ///
    /// The coordinator wraps tool results in an envelope:
    ///   {"result":{"content":[{"type":"text","text":"{...json...}"}], "isError":false}}
    /// so the inner `text` must be extracted and decoded — the raw reply is NOT
    /// a BuildTargetDetail. On failure, throws so callers fall back gracefully.
    func fetchBuildTarget(name: String) async throws -> BuildTargetDetail {
        let body: [String: Any] = [
            "method": "tools/call",
            "params": [
                "tool": "project/getBuildTarget",
                "args": ["name": name]
            ]
        ]
        let data = try JSONSerialization.data(withJSONObject: body)
        let reply = try await backend.send(data)
        guard let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any] else {
            throw NSError(domain: "ProjectService", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Invalid reply from getBuildTarget"])
        }
        if let err = json["error"] as? String {
            throw NSError(domain: "ProjectService", code: 2,
                          userInfo: [NSLocalizedDescriptionKey: err])
        }
        // A project-query tool returns its result directly (not via the
        // tool-module envelope) in most paths; support both shapes.
        var payload = json
        if let result = json["result"] as? [String: Any],
           let content = result["content"] as? [[String: Any]],
           let first = content.first,
           let text = first["text"] as? String,
           let inner = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any] {
            payload = inner
        }
        let payloadData = try JSONSerialization.data(withJSONObject: payload)
        return try MessageSerializer.decode(payloadData)
    }

    /// Read a file's contents via the filesystem module.
    func readFile(at path: String) async -> String? {
        do {
            let body: [String: Any] = [
                "method": "tools/call",
                "params": [
                    "tool": "filesystem_read",
                    "args": ["path": path]
                ]
            ]
            let data = try JSONSerialization.data(withJSONObject: body)
            let reply = try await backend.send(data)
            if let json = try JSONSerialization.jsonObject(with: reply) as? [String: Any] {
                if let err = json["Err"] as? String { return nil }
                if let text = json["Ok"] as? String { return text }
                if let err = json["error"] as? String { return nil }
            }
            return String(data: reply, encoding: .utf8)
        } catch {
            return nil
        }
    }

    // MARK: - Helpers

    /// Build the parameters object for AnalyzeProject requests.
    private func rootParams(_ root: String?) -> [String: Any] {
        if let root, !root.isEmpty {
            return ["root": root]
        }
        return [:]
    }
}