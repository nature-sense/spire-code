import Foundation

/// A point-in-time view of the design session. Mirrors the Rust
/// `SpecDesignState` (snake_case keys). `latest` carries the most recent
/// accepted AppSpec dictionary (the same shape `runSpecDesignCodegen` expects)
/// and is set whenever the assistant submits the design through the chat.
struct SpecDesignState {
    /// "freeform" or "decided" (Rust `DesignMode`, lowercase serialized).
    let mode: String
    let turnCount: Int
    let acceptedCount: Int
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
        self.latest = dict["latest"] as? [String: Any]
    }
}
