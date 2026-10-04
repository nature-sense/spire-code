import Foundation

/// Mirrors the Rust edit-history record (`component_edit_history`): one `idf_component_edit` run —
/// what was asked, how far the change was checked, and what moved.
///
/// The record is **appended**, never rewritten: asking for the same thing twice and twice nothing
/// moving is a fact the history keeps, so a list of these reads as a log rather than a state.
struct ComponentEditRecord: Codable, Identifiable, Equatable {
    /// The instruction, verbatim (capped in the core).
    var instruction: String
    /// Did the change survive its gate?
    var success: Bool
    /// How far verification reached: `HostOnly` or `WithTarget`.
    var verified: String
    /// The files already said what was asked: nothing moved, but the request is in effect.
    var upToDate: Bool
    var filesChanged: [String]
    var filesReverted: [String]
    var filesSkipped: [String]
    /// What the optional second gate (the chip build) did, in the caller's terms.
    var chipBuild: String
    /// What could not be checked, in the user's words.
    var caveats: [String]
    /// The component and library the record is scoped to — what a row reads as when several
    /// components' histories sit in one list.
    var component: String
    var library: String
    /// RFC-3339 timestamp; the list is newest-first, so this is what orders it.
    var at: String

    var id: String { at + component + instruction }

    enum CodingKeys: String, CodingKey {
        case instruction, success, verified, caveats, component, library, at
        case upToDate = "up_to_date"
        case filesChanged = "files_changed"
        case filesReverted = "files_reverted"
        case filesSkipped = "files_skipped"
        case chipBuild = "chip_build"
    }

    /// Lenient by design: a record written before this pane existed may be missing the newer
    /// properties, and a history that refuses to decode is worse than one with a gap in it.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        instruction = (try? c.decode(String.self, forKey: .instruction)) ?? ""
        success = (try? c.decode(Bool.self, forKey: .success)) ?? false
        verified = (try? c.decode(String.self, forKey: .verified)) ?? ""
        upToDate = (try? c.decode(Bool.self, forKey: .upToDate)) ?? false
        filesChanged = (try? c.decode([String].self, forKey: .filesChanged)) ?? []
        filesReverted = (try? c.decode([String].self, forKey: .filesReverted)) ?? []
        filesSkipped = (try? c.decode([String].self, forKey: .filesSkipped)) ?? []
        chipBuild = (try? c.decode(String.self, forKey: .chipBuild)) ?? ""
        caveats = (try? c.decode([String].self, forKey: .caveats)) ?? []
        component = (try? c.decode(String.self, forKey: .component)) ?? ""
        library = (try? c.decode(String.self, forKey: .library)) ?? ""
        at = (try? c.decode(String.self, forKey: .at)) ?? ""
    }
}
