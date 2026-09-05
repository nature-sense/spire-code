import Foundation

/// One open design question with the answer the assistant recommends (and any
/// alternatives). Mirrors the Rust `DesignQuestion`.
struct DesignQuestion {
    /// AppSpec section this question belongs to (types | graph | backend | bridge | ui).
    let section: String
    let question: String
    let recommendation: String
    let options: [String]

    init?(json: Any) {
        guard let dict = json as? [String: Any],
              let question = dict["question"] as? String else { return nil }
        self.section = dict["section"] as? String ?? ""
        self.question = question
        self.recommendation = dict["recommendation"] as? String ?? ""
        self.options = dict["options"] as? [String] ?? []
    }
}

/// A point-in-time view of the design session. Mirrors the Rust
/// `SpecDesignState` (snake_case keys). `latest` carries the most recent
/// accepted AppSpec dictionary (the same shape `runSpecDesignCodegen` expects)
/// and is set whenever the assistant submits the design through the chat.
struct SpecDesignState {
    /// "freeform" or "decided" (Rust `DesignMode`, lowercase serialized).
    let mode: String
    let turnCount: Int
    /// The assistant's current draft design outline (markdown) if any.
    let outline: String?
    /// Design questions the assistant still needs answered, each with a
    /// recommended answer the user can accept (the submit gate refuses while any
    /// remain).
    let openQuestions: [DesignQuestion]
    let acceptedCount: Int
    /// Most recent accepted AppSpec (present once the design is submitted).
    let latest: [String: Any]?

    var isDecided: Bool { mode == "decided" }

    init?(json: Any) {
        guard let dict = json as? [String: Any],
              let mode = dict["mode"] as? String else { return nil }
        self.mode = mode
        self.turnCount = dict["turn_count"] as? Int ?? 0
        self.outline = dict["outline"] as? String
        if let raw = dict["open_questions"] as? [Any] {
            self.openQuestions = raw.compactMap { DesignQuestion(json: $0) }
        } else {
            self.openQuestions = []
        }
        if let accepted = dict["accepted"] as? [Any] {
            self.acceptedCount = accepted.count
        } else {
            self.acceptedCount = 0
        }
        self.latest = dict["latest"] as? [String: Any]
    }
}
