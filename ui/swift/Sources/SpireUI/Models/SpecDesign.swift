import Foundation

/// A validation finding on the generated AppSpec. Mirrors the Rust `SpecIssue`.
struct SpecIssue {
    let path: String
    let message: String
    /// "error" or "warning" (Rust `SpecIssueSeverity`, lowercase serialized).
    let severity: String

    var isError: Bool { severity == "error" }

    init?(json: Any) {
        guard let dict = json as? [String: Any],
              let message = dict["message"] as? String else { return nil }
        self.path = dict["path"] as? String ?? ""
        self.message = message
        self.severity = dict["severity"] as? String ?? "warning"
    }
}

/// Result of a Convert run (mirrors the Rust `ConvertOutcome`).
struct ConvertOutcome {
    let specMd: String
    let parseError: String?
    let issues: [SpecIssue]
    let state: SpecDesignState

    init?(json: Any) {
        guard let dict = json as? [String: Any],
              let specMd = dict["spec_md"] as? String,
              let state = SpecDesignState(json: dict["state"]) else { return nil }
        self.specMd = specMd
        self.parseError = dict["parse_error"] as? String
        if let raw = dict["issues"] as? [Any] {
            self.issues = raw.compactMap { SpecIssue(json: $0) }
        } else {
            self.issues = []
        }
        self.state = state
    }
}

/// A point-in-time view of the design session. Mirrors the Rust
/// `SpecDesignState` (snake_case keys). `latest` carries the most recently
/// accepted AppSpec (the shape `runSpecDesignCodegen` expects).
struct SpecDesignState {
    /// "freeform" or "decided" (Rust `DesignMode`, lowercase serialized).
    let mode: String
    /// The freeform spec last submitted to Convert.
    let freeformSpec: String
    /// The generated AppSpec markdown, if any.
    let specMd: String?
    let acceptedCount: Int
    let issues: [SpecIssue]
    /// Most recent accepted AppSpec (present once the AppSpec is accepted).
    let latest: [String: Any]?

    var isDecided: Bool { mode == "decided" }

    init?(json: Any) {
        guard let dict = json as? [String: Any],
              let mode = dict["mode"] as? String else { return nil }
        self.mode = mode
        self.freeformSpec = dict["freeform_spec"] as? String ?? ""
        self.specMd = dict["spec_md"] as? String
        if let accepted = dict["accepted"] as? [Any] {
            self.acceptedCount = accepted.count
        } else {
            self.acceptedCount = 0
        }
        if let raw = dict["last_issues"] as? [Any] {
            self.issues = raw.compactMap { SpecIssue(json: $0) }
        } else {
            self.issues = []
        }
        self.latest = dict["latest"] as? [String: Any]
    }
}
