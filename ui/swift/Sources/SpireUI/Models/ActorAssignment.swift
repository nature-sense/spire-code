import Foundation

/// One **actor** of an application, as the assignment step edits it — the in-memory draft of an
/// `actors` unit.
///
/// `uid` is a *stable* handle that does not change as the id is typed: the resource assignments and
/// the "sends to" edges refer to an actor by `uid`, never by the id a person is still editing. Only
/// when the spec is assembled does the id become the word the wiring speaks, so renaming an actor
/// never orphans the devices it owns or the peers it posts to.
struct ActorDraft: Hashable {
    /// Stable identity, independent of `id` (which a person edits while the row is on screen).
    var uid = UUID()
    /// The unit id — what the wiring, `uses` and `sends_to` refer to. Lower_snake.
    var id = ""
    /// The `Message` type the actor receives: an actor *is* a message type.
    var message = ""
    /// What the actor holds between messages (optional).
    var state = ""
    /// What the actor **does**, in the person's own words — the prose that heads its generated class
    /// doc-comment. Optional: the message and the state often say it, and an empty one is simply left out.
    var description = ""
    /// The peers this actor posts to, by their `uid` — stable across renames.
    var sendsTo: [UUID] = []
}

/// The **actor-assignment** step's state: the resources a board-and-resource selection declared, and
/// the actors that own them.
///
/// The whole point is ownership — the Rust check's rule 8: **every resource belongs to exactly one
/// unit**, the actor that holds it in its state. Here ownership is *structural*: `owners` maps a
/// resource name to the **one** actor that owns it, so a resource cannot be shared any more than a
/// device can be in two places at once. Giving an owned resource to another actor therefore **moves**
/// it, which is the only honest reading of the gesture.
///
/// The two things the Rust side refuses that this cannot express — a resource two units own, and a
/// unit nobody references — become: an *unowned* resource (0 owners) is reported, and an actor no
/// edge names is reported, because both are reachable from this state while a shared resource is not.
struct ActorAssignmentState: Equatable {
    /// The board the application is built for, read off the board step — for the spec's `board`
    /// (`chip`/`bsp`/`hal`).
    var board: Platform?
    /// The resources the earlier step declared: the board's selected parts and the additions.
    var resources: [ResourceDraft] = []
    /// The actors, in the order they were added.
    var actors: [ActorDraft] = []
    /// Resource name → the `uid` of the **one** actor that owns it. Absent means unowned, which is
    /// the rule-8 failure the step disables Continue on.
    var owners: [String: UUID] = [:]
    /// The **description** the person wrote on the describe step, in their own words — the whole
    /// application in English. It rides in the assembled spec so the generated code is headed with the
    /// same sentence the design was reviewed against. Set by [`sync`] from the flow's state; empty when
    /// nothing was written, which is the ordinary case for an application built without prose.
    var description = ""

    init(board: Platform? = nil, resources: [ResourceDraft] = []) {
        self.board = board
        self.resources = resources
    }

    /// The driver bus a `library` resource starts with — the common case, which the person still
    /// confirms or changes on the resource's own row. A *starting point*, not a claim about the
    /// device: a driver written for the wrong bus never compiles (rule 4), so it is shown in the
    /// field rather than decided behind it.
    static let defaultDriverBus = "i2c"

    /// Bring the step in line with what the board step produced — called whenever the step is
    /// (re)entered. Actors are **kept**: going back to add a device must not throw away the
    /// composition already written. An ownership of a resource that no longer exists is dropped, and
    /// so is a `sends_to` whose peer is gone — a stale reference is a hole in the spec. A `library`
    /// resource the board step left without a driver bus is given [`defaultDriverBus`] to start from.
    mutating func sync(board: Platform?, resources: [ResourceDraft], description: String = "") {
        self.board = board
        self.resources = resources
        // The whole-application prose comes from the describe step on every (re)entry, so going back to
        // edit it and returning carries the edit through rather than keeping a stale sentence.
        self.description = description
        let names = Set(resources.map(\.name))
        owners = owners.filter { names.contains($0.key) }
        let uids = Set(actors.map(\.uid))
        for index in actors.indices {
            actors[index].sendsTo.removeAll { !uids.contains($0) }
        }
        // A resource this project drives needs a bus (rule 4), and `i2c` is the common one, so the
        // field arrives filled rather than the person facing a blank warning. Only a `library`
        // resource: a board part's `bus` is a wiring function the board stated, and a `published`
        // one's bus belongs to its registry component, which this step does not name.
        for index in self.resources.indices {
            let resource = self.resources[index]
            if resource.via == .library
                && resource.bus.trimmingCharacters(in: .whitespaces).isEmpty {
                self.resources[index].bus = Self.defaultDriverBus
            }
        }
    }

    /// Start the step from the **brief's** suggested unit names: a row per suggestion, so the person
    /// begins with the composition the description implied rather than an empty pane.
    ///
    /// **Additive and idempotent** — the step syncs on every (re)entry, so a suggestion already present
    /// is left alone and the person's own rows are never touched or reordered. A suggestion that is not
    /// already a valid `lower_snake` id is *skipped* rather than normalized: a name the person did not
    /// write is exactly the thing not worth putting an error next to, and the core will still refuse a
    /// real collision when the spec is assembled.
    mutating func suggestActors(_ names: [String]) {
        var taken = Set(actors.map(\.id))
        let resourceNames = Set(resources.map(\.name))
        for name in names {
            let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
            guard Self.isIdentifier(name), !resourceNames.contains(name), !taken.contains(name)
            else { continue }
            taken.insert(name)
            actors.append(ActorDraft(id: name))
        }
    }

    // MARK: - Actors

    /// Add a blank actor and return its `uid` — the row arrives empty and the person names it.
    @discardableResult
    mutating func addActor() -> UUID {
        let actor = ActorDraft()
        actors.append(actor)
        return actor.uid
    }

    func actor(_ uid: UUID) -> ActorDraft? { actors.first { $0.uid == uid } }

    /// Remove an actor. Its resources become **unowned** (never quietly handed to another actor — the
    /// person says who owns them next), and every edge into or out of it goes with it, because a
    /// `sends_to` that names a unit no longer in the spec is exactly the hole rule 6 refuses.
    mutating func removeActor(_ uid: UUID) {
        actors.removeAll { $0.uid == uid }
        owners = owners.filter { $0.value != uid }
        for index in actors.indices {
            actors[index].sendsTo.removeAll { $0 == uid }
        }
    }

    /// Turn one peer on or off for an actor — the "sends to" gesture, from which the `wiring` edges
    /// are derived. An actor cannot send to itself, so that pair is ignored.
    mutating func toggleSend(from source: UUID, to target: UUID) {
        guard source != target, let index = actors.firstIndex(where: { $0.uid == source }) else {
            return
        }
        if let at = actors[index].sendsTo.firstIndex(of: target) {
            actors[index].sendsTo.remove(at: at)
        } else {
            actors[index].sendsTo.append(target)
        }
    }

    // MARK: - Ownership

    func ownerUID(of resource: String) -> UUID? { owners[resource] }

    /// The owning actor's **id**, for display — empty when the resource is unowned or its owner is
    /// still unnamed.
    func ownerID(of resource: String) -> String {
        actors.first { $0.uid == owners[resource] }?.id ?? ""
    }

    /// Give a resource to an actor, **moving** it from whoever had it; `nil` takes it back to
    /// unowned. The resource and the actor both have to exist, so a stale name cannot invent an
    /// ownership.
    mutating func assign(_ resource: String, to uid: UUID?) {
        guard resources.contains(where: { $0.name == resource }) else { return }
        if let uid, actors.contains(where: { $0.uid == uid }) {
            owners[resource] = uid
        } else {
            owners[resource] = nil
        }
    }

    /// The resources an actor owns, in the order the resources step declared them — what its `uses`
    /// will be.
    func resourcesOwned(by uid: UUID) -> [String] {
        resources.map(\.name).filter { owners[$0] == uid }
    }

    /// Every declared resource no actor owns — the rule-8 failure, and the one the step can reach.
    var unownedResources: [ResourceDraft] {
        resources.filter { owners[$0.name] == nil }
    }

    // MARK: - Wiring

    /// Every edge the actors state, `from -> to`, deduplicated and sorted so the assembled spec is
    /// stable. The Rust `wiring` is **stated** rather than derived, so the editor writes both from
    /// this one list: "sends to" is the source of truth and the edge is its rendering.
    var wiring: [String] {
        var edges = Set<String>()
        for actor in actors where !actor.id.isEmpty {
            for peer in actor.sendsTo {
                guard let target = self.actor(peer), !target.id.isEmpty else { continue }
                edges.insert("\(actor.id) -> \(target.id)")
            }
        }
        return edges.sorted()
    }

    /// The peer **ids** an actor posts to, sorted — what its `sends_to` carries in the spec.
    func sendsToIDs(of uid: UUID) -> [String] {
        guard let source = actor(uid) else { return [] }
        return source.sendsTo.compactMap { self.actor($0)?.id }.filter { !$0.isEmpty }.sorted()
    }

    /// Whether any edge names this actor, as its source or its target. An actor no edge names is
    /// unreferenced — "invented and unused" is the Rust rule 6, and it is why a lone actor is as
    /// invalid as a lone resource.
    func isReferenced(_ uid: UUID) -> Bool {
        guard let actor = actor(uid), !actor.id.isEmpty else { return false }
        for other in actors where !other.id.isEmpty {
            for peer in other.sendsTo {
                guard let target = self.actor(peer), !target.id.isEmpty else { continue }
                if other.uid == uid || target.uid == uid { return true }
            }
        }
        return false
    }

    // MARK: - What each actor is

    /// What an actor *is*, in one line — the same reading the review's
    /// [`ApplicationDesign.Unit.summary`] gives, derived live from the draft so the actor list reads as
    /// "the name, then what it does": the message it receives, the state it holds, the resources it owns
    /// (its `uses`) and the peers it posts to.
    ///
    /// **Derived, never stored.** The line is a *function* of the same fields the spec is assembled
    /// from, so it cannot drift from what would be built and there is no second description to keep in
    /// step. A field the person has not filled yet is left out rather than invented.
    func summary(of uid: UUID) -> String {
        guard let actor = self.actor(uid) else { return "" }
        var parts: [String] = []
        let message = actor.message.trimmingCharacters(in: .whitespacesAndNewlines)
        parts.append("on \(message.isEmpty ? "an unnamed message" : message)")
        let state = actor.state.trimmingCharacters(in: .whitespacesAndNewlines)
        if !state.isEmpty { parts.append("holds \(state)") }
        let uses = resourcesOwned(by: uid)
        if !uses.isEmpty { parts.append("uses \(uses.joined(separator: ", "))") }
        let sends = sendsToIDs(of: uid)
        if !sends.isEmpty { parts.append("sends to \(sends.joined(separator: ", "))") }
        return parts.joined(separator: " · ")
    }

    // MARK: - What the rules require of an id

    /// A **lower_snake** identifier — the shape a unit id has, since it becomes a component directory
    /// or a class name. Stricter than the Rust reader's own check on purpose: this side only has to
    /// *accept* what it can generate, so a bare `lower_snake` is the whole vocabulary it offers.
    static func isIdentifier(_ text: String) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.first?.isLetter == true else { return false }
        guard text.allSatisfy({ $0.isLowercase || $0.isNumber || $0 == "_" }) else { return false }
        guard !text.hasSuffix("_"), !text.contains("__") else { return false }
        return true
    }


    // MARK: - The rules, restated where they can disable a button

    /// Every reason the step cannot go on — the Rust rules, mirrored where a person can see them
    /// before anything is generated. Empty means the assembled spec should validate (rule 8 is
    /// structural here, so it can only surface as an *unowned* resource).
    var problems: [String] {
        var out: [String] = []

        // The board is the application's target: with no chip there is nothing to compile for.
        if board == nil {
            out.append("no board — this application has no chip to build for")
        } else if (board?.chip ?? "").trimmingCharacters(in: .whitespaces).isEmpty {
            out.append("the board names no chip — the compile target is unknown")
        }

        if resources.isEmpty {
            out.append("no resources — declare at least one device to build against")
        }

        // A resource this project drives needs a driver, and a driver names its bus (rule 4). A
        // `published` one is a managed dependency whose registry name the board step does not collect
        // yet, so it cannot be finished here.
        for resource in resources where resource.via != .board {
            if resource.via == .published {
                out.append(
                    "'\(resource.name)' is a published resource — this step cannot name the registry "
                        + "component it resolves, so declare it `library` for now")
            } else if resource.bus.trimmingCharacters(in: .whitespaces).isEmpty {
                out.append(
                    "'\(resource.name)' needs a driver bus — the protocol its driver speaks, like "
                        + "`i2c` or `spi`")
            }
        }

        if actors.isEmpty {
            out.append("no actors — a resource is owned by the actor that holds it")
        }

        // Rule 8, the reachable half: a resource no actor owns was declared for nothing.
        if !unownedResources.isEmpty {
            let names = unownedResources.map(\.name).joined(separator: ", ")
            out.append("\(names) has no owner — a resource is owned by exactly one actor")
        }

        var seen: Set<String> = []
        for actor in actors {
            // Rule 1: ids are lower_snake and unique, and a unit id may not also be a resource name
            // — that would shadow the board's part (rule 7) or collide with its driver (rule 1).
            if actor.id.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                out.append("an actor has no id")
            } else if !Self.isIdentifier(actor.id) {
                out.append("'\(actor.id)' is not a lower_snake id")
            } else if !seen.insert(actor.id).inserted {
                out.append("'\(actor.id)' is used by two actors")
            }
            if resources.contains(where: { $0.name == actor.id }) {
                out.append(
                    "'\(actor.id)' is both an actor and a resource — one word cannot mean two things")
            }
            // Rule 2: an actor *is* a message type.
            if actor.message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                out.append("'\(actor.id)' has no message — an actor *is* a message type")
            }
        }

        // Rule 6: nothing invented and unused. Every actor has to be named by an edge, so a lone
        // actor is as invalid as a lone resource.
        for actor in actors where !actor.id.isEmpty && !isReferenced(actor.uid) {
            out.append(
                "'\(actor.id)' is wired to nobody — say who it sends to, or have another actor send "
                    + "to it")
        }

        return out
    }

    /// The step may go on when nothing above stands — the button is **disabled** rather than the spec
    /// refused, because every one of these is the person's to fix here.
    var canContinue: Bool { problems.isEmpty }

    // MARK: - The spec

    /// The board as the spec carries it: the silicon, its support package, and the abstraction — read
    /// off the platform the board step chose. `chip`/`bsp` are always written, because the Rust reader
    /// decodes them as non-optional keys; `hal` only when the platform names one.
    private var boardJSON: [String: Any] {
        var out: [String: Any] = [
            "chip": board?.chip ?? "",
            "bsp": board?.bsp ?? "",
        ]
        if let hal = board?.hal?.crateName, !hal.isEmpty { out["hal"] = hal }
        return out
    }

    /// The driver **components** the resources imply — one per `library`/`published` resource, whose
    /// `id` *is* the resource name (the rule the Rust check enforces). A `board` resource has none:
    /// the BSP reaches it, which is what `via: board` means.
    private var driverUnits: [[String: Any]] {
        resources.filter { $0.via != .board }.map { resource in
            var unit: [String: Any] = [
                "id": resource.name,
                "kind": "component",
                "role": "driver",
                "source": resource.via == .published ? "published" : "stub",
            ]
            if !resource.roles.isEmpty {
                // One line a reviewer reads: the capabilities this driver provides, all of them — a
                // multi-role part (`sht20`) states both, so the driver's own line says both too.
                unit["provides"] = resource.roles.joined(separator: ", ")
            }
            if !resource.bus.isEmpty { unit["bus"] = resource.bus }
            return unit
        }
    }

    /// The **actor** units: a message, the state behind it, the resources it owns (`uses`) and the
    /// peers it posts to (`sends_to`). `uses` is what makes each driver component *referenced*, so a
    /// driver is never the invented unit rule 6 refuses.
    private var actorUnits: [[String: Any]] {
        actors.map { actor in
            var unit: [String: Any] = [
                "id": actor.id,
                "kind": "actor",
                "message": actor.message,
            ]
            if !actor.state.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                unit["state"] = actor.state
            }
            if !actor.description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                unit["description"] = actor.description
            }
            let uses = resourcesOwned(by: actor.uid)
            if !uses.isEmpty { unit["uses"] = uses }
            let sends = sendsToIDs(of: actor.uid)
            if !sends.isEmpty { unit["sends_to"] = sends }
            return unit
        }
    }

    /// The board facts the application's *own* drivers need: a bus and an address the product knows,
    /// which a component never does. Only a driver this project writes has one, and only when it is
    /// reached at an address — a `board` resource is reached through the BSP and carries none.
    private var boardFactJSON: [[String: Any]] {
        resources
            .filter { $0.via != .board && !$0.address.trimmingCharacters(in: .whitespaces).isEmpty }
            .map { ["bus": $0.bus, "device": $0.name, "address": $0.address] }
    }

    /// The application spec the step has produced — an `actors` composition with the drivers its
    /// resources imply. Deterministic: the same state always writes the same spec, so the review a
    /// person approves is the spec that validates.
    var specJSON: [String: Any] {
        var spec: [String: Any] = [
            "framework": "actors",
            "board": boardJSON,
            "resources": resources.map(\.json),
            "board_facts": boardFactJSON,
            "units": driverUnits + actorUnits,
            "wiring": wiring,
        ]
        // The whole application in the person's own words, when they wrote any: it rides in the spec so
        // the generated code is headed with the sentence the design was reviewed against. Omitted when
        // empty, so a spec with no prose is exactly the spec it always was.
        if !description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            spec["description"] = description
        }
        return spec
    }

    /// The spec, pretty-printed — what a person reads at the review.
    var specText: String {
        guard
            let data = try? JSONSerialization.data(
                withJSONObject: specJSON, options: [.prettyPrinted, .sortedKeys]),
            let text = String(data: data, encoding: .utf8)
        else { return "{}" }
        return text
    }

    /// The spec as the review screen reads it — decoded through the model that already exists, so
    /// there is one reader for a spec rather than two.
    var design: ApplicationDesign? {
        guard let data = try? JSONSerialization.data(withJSONObject: specJSON) else { return nil }
        return try? JSONDecoder().decode(ApplicationDesign.self, from: data)
    }
}

