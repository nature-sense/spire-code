import Foundation

/// What `createProject/RepairFromBuild` answers: the rewrites to execute, and what it would not touch.
///
/// A repair is a **proposal**, in exactly the sense a fill is: the steps go through the same executor
/// (`createProject/ExecutePlan`), and that is where the structural guard lives — so a repair cannot
/// write a file the scaffold locked, whatever the compiler says about it.
///
/// The two lists beside the steps are what a person needs to see. `refused` is a rewrite that would not
/// parse and was therefore not written. `unrepaired` is the compiler's own lines for diagnostics in
/// files *no repair may act on* — a locked file, or a header in the component library the application
/// only consumes — and those are a person's decision rather than another model turn.
struct ApplicationRepair: Codable {
    struct Refusal: Codable, Equatable {
        let path: String
        let reason: String
    }

    let steps: [CreationStep]
    /// How many `error:` lines the build produced, before any of them were attributed to a file.
    let diagnostics: Int
    let refused: [Refusal]
    let unrepaired: [String]
    let next: String?

    /// One line for the sheet: what happened, in the terms somebody waiting reads.
    var summary: String {
        var text = "\(steps.count) rewrite\(steps.count == 1 ? "" : "s") for "
            + "\(diagnostics) error\(diagnostics == 1 ? "" : "s")"
        if !refused.isEmpty { text += ", \(refused.count) refused" }
        if !unrepaired.isEmpty { text += ", \(unrepaired.count) not ours to fix" }
        return text
    }

    /// Whether there is anything to execute. A repair that proposes nothing would leave the next build
    /// exactly as broken, so a caller should stop rather than ask again.
    var hasWork: Bool { !steps.isEmpty }
}
