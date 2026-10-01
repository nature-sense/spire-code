import Foundation

/// The **design phase's answer**, as the person reviewing it sees it.
///
/// Everything here is decided by the Rust side (`createProject/DesignApplication`, and the six rules in
/// `application_spec`); this decodes what a reviewer has to read, and nothing more. The spec travels
/// back to `createProject/Scaffold` **exactly as it was received** — the wizard keeps the raw JSON — so
/// what is approved is what is scaffolded, with no round trip through this type in between.
///
/// The one line this adds is `marker`: the framework reaches the tree as a single stated line, and a
/// reviewer who is confirming a choice should see the line itself rather than a summary of it.
struct ApplicationDesign: Codable, Equatable {
    struct Board: Codable, Equatable {
        let chip: String
        let bsp: String
        var hal: String?

        /// The board as `createProject/*` takes it. `hal` is left out when empty: it is a vendor
        /// abstraction's name (`m5unified`, `bsp`) and a blank string is not one.
        var json: [String: Any] {
            var out: [String: Any] = ["chip": chip, "bsp": bsp]
            if let hal, !hal.trimmingCharacters(in: .whitespaces).isEmpty { out["hal"] = hal }
            return out
        }
    }

    /// A device the *application* reaches at an address on a bus. `device` is a unit id, so a fact that
    /// names no driver in the design is one a reviewer has to catch — the Rust side checks it too.
    struct BoardFact: Codable, Equatable {
        let bus: String
        let device: String
        var address: String?

        /// One line of the review: `sps30 on i2c at 0x69`.
        var summary: String {
            if let address, !address.trimmingCharacters(in: .whitespaces).isEmpty {
                return "\(device) on \(bus) at \(address)"
            }
            return "\(device) on \(bus)"
        }
    }

    let framework: String
    var justification: String?
    let board: Board
    var boardFacts: [BoardFact]?
    let units: [Unit]
    var wiring: [String]?

    enum CodingKeys: String, CodingKey {
        case framework, justification, board, units, wiring
        case boardFacts = "board_facts"
    }

    /// The line the application will state its framework with — the whole of what the choice becomes in
    /// the tree, which is why it is shown at review.
    var marker: String { "set(SPIRE_APPLICATION_FRAMEWORK \(framework))" }

    var facts: [BoardFact] { boardFacts ?? [] }
    var edges: [String] { wiring ?? [] }

    /// The components, and the units that use them. The split is the one a reader needs: the first list
    /// is what the *library* is made of (and what has to exist before a build), the second is what this
    /// application is.
    var components: [Unit] { units.filter(\.isComponent) }
    var composition: [Unit] { units.filter { !$0.isComponent } }

    /// The components the design says have to be **written** — the stubs. An application with these and
    /// no library has nowhere to put them, which is worth saying at review rather than discovering at a
    /// build that cannot resolve a `REQUIRES`.
    var componentsToWrite: [Unit] { components.filter { $0.source == "stub" } }

    /// The components the design draws from the **registry** rather than the library or a stub — the
    /// managed dependencies the application's `main/idf_component.yml` will name. Shown at review so a
    /// person sees which upstream component is coming, and deliberately *not* in `componentsToWrite`:
    /// nothing is written into the library for these.
    var componentsFromRegistry: [Unit] { components.filter { $0.source == "published" } }

    var frameworkLabel: String {
        switch framework {
        case "actors": return "actors — units that hold state and react to messages"
        case "ramen": return "ramen — values pushed through wired stages"
        default: return framework
        }
    }

    /// A unit of the decomposition. One type with optional fields rather than three types, because the
    /// *spec* has one: the kind decides which fields are meaningful, and a reviewer reads whichever are
    /// there.
    struct Unit: Codable, Equatable {
        let id: String
        /// `component` (framework-agnostic), `actor`, or `stage`.
        let kind: String
        /// Components: `driver` or `library`.
        var role: String?
        /// Components: `existing` (the library has it), `stub` (to be written), or `published` (a
        /// managed dependency from the ESP Component Registry).
        var source: String?
        /// `published` components only: the registry's own `namespace/name` — what `idf_component.yml`
        /// resolves. A reviewer reads it to see *which* upstream component, not just that there is one.
        var registry: String?
        var provides: String?
        var bus: String?
        var message: String?
        var state: String?
        var pulls: String?
        var pushes: String?
        var uses: [String]?
        var sendsTo: [String]?

        enum CodingKeys: String, CodingKey {
            case id, kind, role, source, registry, provides, bus, message, state, pulls, pushes, uses
            case sendsTo = "sends_to"
        }

        var isComponent: Bool { kind == "component" }

        /// One line of the review, saying what this unit *is*: a driver on a bus, a library, an actor on
        /// a message, a stage that pulls and pushes. The shape is the part a reviewer checks — a unit
        /// described only by its name is a unit the person cannot disagree with.
        var summary: String {
            switch kind {
            case "component":
                var text = role ?? "component"
                if let bus, !bus.isEmpty { text += " on \(bus)" }
                if source == "existing" {
                    text += " — the library has it"
                } else if source == "stub" {
                    text += " — to be written"
                } else if source == "published" {
                    text += " — from the registry (\(registry ?? "?"))"
                }
                if let provides, !provides.isEmpty { text += ": \(provides)" }
                return text
            case "actor":
                var text = "actor on \(message ?? "?")"
                if let state, !state.isEmpty { text += ", holding \(state)" }
                if let uses, !uses.isEmpty { text += ", uses \(uses.joined(separator: ", "))" }
                if let sendsTo, !sendsTo.isEmpty {
                    text += ", sends to \(sendsTo.joined(separator: ", "))"
                }
                return text
            default:
                return "stage: pulls \(pulls ?? "nothing"), pushes \(pushes ?? "nothing")"
            }
        }
    }
}
