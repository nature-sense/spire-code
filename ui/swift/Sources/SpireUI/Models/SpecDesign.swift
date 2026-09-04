import Foundation

/// A condensed document in the free-form design session (summary or spec).
/// Mirrors the Rust `DesignArtifact`. Timestamps are ignored.
struct SpecDesignArtifact {
    let version: Int
    let content: String

    init?(json: Any) {
        guard let dict = json as? [String: Any],
              let version = dict["version"] as? Int,
              let content = dict["content"] as? String else { return nil }
        self.version = version
        self.content = content
    }
}

/// A point-in-time view of the design session. Mirrors the Rust
/// `SpecDesignState` (snake_case keys). `latest` carries the most recent
/// accepted AppSpec dictionary (the same shape `runSpecDesignCodegen` expects)
/// and is set whenever the assistant submits the design through the chat.
struct SpecDesignState {
    /// "freeform" or "decided" (Rust `DesignMode`, lowercase serialized).
    let mode: String
    let summary: SpecDesignArtifact?
    let spec: SpecDesignArtifact?
    let turnCount: Int
    let acceptedCount: Int
    /// Questions/options the assistant raised that are still unanswered - the
    /// assistant must not submit the AppSpec while any remain.
    let openQuestions: [String]
    /// Most recent accepted AppSpec (present once the design is submitted).
    let latest: [String: Any]?

    var isDecided: Bool { mode == "decided" }

    init?(json: Any) {
        guard let dict = json as? [String: Any],
              let mode = dict["mode"] as? String else { return nil }
        self.mode = mode
        self.turnCount = dict["turn_count"] as? Int ?? 0
        if let accepted = dict["accepted"] as? [Any] {
            self.acceptedCount = accepted.count
        } else {
            self.acceptedCount = 0
        }
        self.openQuestions = dict["open_questions"] as? [String] ?? []
        self.summary = SpecDesignArtifact(json: dict["summary"])
        self.spec = SpecDesignArtifact(json: dict["spec"])
        self.latest = dict["latest"] as? [String: Any]
    }
}
