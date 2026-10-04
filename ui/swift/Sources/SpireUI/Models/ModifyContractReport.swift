import Foundation

/// Mirrors the Rust `modify_contract` report (`ModifyContractReport`): the drift cascade's verdict —
/// how much drift there was, how much is left, and which interfaces moved.
///
/// The unit of drift is a **method** (a missing one, or one whose signature no longer matches the
/// contract), so `driftBefore`/`driftAfter` are counts and the gap lists are `platform/interface`.
struct ModifyContractReport: Codable, Equatable {
    /// Did the whole cascade end without growing the drift or breaking the build?
    var success: Bool
    /// Total drift (missing + drifted methods) on the platform before any round.
    var driftBefore: Int
    var driftAfter: Int
    /// Interfaces whose gap closed and stayed closed.
    var gapsClosed: [String]
    /// Interfaces a round touched but rolled back — the round grew the drift or broke the build.
    var gapsReverted: [String]
    /// Interfaces still open when the rounds ran out.
    var gapsRemaining: [String]
    /// The human-readable summary the coordinator built (`report.summary()`).
    var output: String
    /// Present only on a refusal (no path, no platform): the cascade never ran.
    var error: String?

    enum CodingKeys: String, CodingKey {
        case success, output, error
        case driftBefore = "drift_before"
        case driftAfter = "drift_after"
        case gapsClosed = "gaps_closed"
        case gapsReverted = "gaps_reverted"
        case gapsRemaining = "gaps_remaining"
    }

    /// Lenient: the refusal shape carries only `success`/`error`/`output`, and a report that refuses to
    /// decode would turn "no platform selected" into an unreadable error.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        success = (try? c.decode(Bool.self, forKey: .success)) ?? false
        driftBefore = (try? c.decode(Int.self, forKey: .driftBefore)) ?? 0
        driftAfter = (try? c.decode(Int.self, forKey: .driftAfter)) ?? 0
        gapsClosed = (try? c.decode([String].self, forKey: .gapsClosed)) ?? []
        gapsReverted = (try? c.decode([String].self, forKey: .gapsReverted)) ?? []
        gapsRemaining = (try? c.decode([String].self, forKey: .gapsRemaining)) ?? []
        output = (try? c.decode(String.self, forKey: .output)) ?? ""
        error = try? c.decode(String.self, forKey: .error)
    }
}
