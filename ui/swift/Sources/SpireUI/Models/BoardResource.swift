import Foundation

/// One **part** a board declares it carries — a `Device` node, read back by `capabilities/profile`.
///
/// The name (`display`, `rtc`) is what an application's `resources:` refers to; `roles` are the
/// capabilities the part *is* (`media.display`, or both `sensing.temperature` and `sensing.humidity`);
/// `properties` is everything the board wrote beside it (`part`, `bus`, `address`). A part with no role
/// cannot exist: the core refuses a device whose roles the vocabulary does not name, so each role here
/// is either a known path or the part was dropped.
struct BoardDevice: Codable, Hashable, Identifiable {
    let name: String
    /// The capabilities the part **is** — one or several (the SHT20 is `sensing.temperature` *and*
    /// `sensing.humidity`). The core sends `roles: [...]`; a bare `role: "…"` from an older core is
    /// read as a one-element list, so a payload either side of the multi-role change decodes.
    let roles: [String]
    let properties: [String: JSONValue]

    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, roles, role, properties
    }

    init(name: String, roles: [String], properties: [String: JSONValue]) {
        self.name = name
        self.roles = roles
        self.properties = properties
    }

    /// Tolerant on purpose: the canonical `roles: [...]` list and the older scalar `role: "…"` are one
    /// field to the rest of the UI, and the scalar is read as `[role]`. A part that names neither has no
    /// role (the core would have dropped it, so this only guards an older payload).
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        properties = try container.decode([String: JSONValue].self, forKey: .properties)
        if let list = try container.decodeIfPresent([String].self, forKey: .roles) {
            roles = list
        } else if let one = try container.decodeIfPresent(String.self, forKey: .role) {
            roles = [one]
        } else {
            roles = []
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(name, forKey: .name)
        try container.encode(roles, forKey: .roles)
        try container.encode(properties, forKey: .properties)
    }

    /// The part's roles as one line — `sensing.temperature, sensing.humidity`. What a part row shows
    /// where a single role used to be.
    var rolesLabel: String { roles.joined(separator: ", ") }

    /// Whether the part *is* this capability — the read every role-keyed gate uses, now that one part
    /// may carry several.
    func carries(role: String) -> Bool { roles.contains(role) }

    /// The wiring function the part is reached on (`bus:` in the board's `devices:` block), when the
    /// board named one. A function of the board's `pins:`, not a protocol.
    var bus: String { properties["bus"]?.stringValue ?? "" }

    /// Its I²C address, when it has one.
    var address: String { properties["address"]?.stringValue ?? "" }

    /// The part number the board carries it as (`ILI9342C`), when the board named one.
    var part: String { properties["part"]?.stringValue ?? "" }
}

/// A board's **resolved profile** — what `capabilities/profile` reads back from the graph: the
/// capabilities it realizes, the companion silicon it carries, how it is wired, and the parts it
/// declares. The `devices` are the palette the flow offers as selectable parts.
struct BoardProfile: Codable, Hashable {
    let board: String
    let realizes: [CapabilityEdge]
    let carries: [CompanionEdge]
    let pins: [PinFunction]
    let devices: [BoardDevice]
}

/// One **category** of the capability vocabulary, with the paths under it — the `capabilities/
/// vocabulary` reply. The catalogue, not a board's facts: every path here is one a device's `roles:`
/// (or an application's `resources[].roles`) may take, because the same embedded vocabulary validates
/// both.
struct VocabularyCategory: Codable, Hashable, Identifiable {
    let category: String
    let capabilities: [String]

    var id: String { category }
}

/// The `capabilities/vocabulary` reply envelope.
struct VocabularyReply: Decodable {
    let categories: [VocabularyCategory]
}

/// How a declared resource is reached — the mirror of the Rust `ResourceVia`, spelled lowercase on
/// the wire so the draft an application carries and the spec the core checks speak one word.
enum ResourceVia: String, Codable, CaseIterable, Hashable {
    /// A part **of the board**: the BSP or board abstraction reaches it, so no component of this
    /// project drives it.
    case board
    /// A part driven by a **component of the library** the application is built against.
    case library
    /// A part driven by a **published** component — a managed dependency of the application.
    case published

    var label: String {
        switch self {
        case .board: return "Board"
        case .library: return "Library"
        case .published: return "Published"
        }
    }
}

/// One **resource** the flow declares — the in-memory draft of a `resources:` entry.
///
/// Not yet a [`Resource`] the core would accept: a `library`/`published` resource needs a driver
/// unit, which a later step supplies. This is what board-and-resource selection produces, held until
/// the flow has enough to build a spec — so a reviewer sees the set the application will have before
/// anything is generated.
struct ResourceDraft: Identifiable, Hashable {
    var name: String
    /// The capabilities the device **is** — one or several. The spec carries them as `roles: [...]`.
    var roles: [String]
    var via: ResourceVia
    /// The board wiring function it is reached on, when it has one.
    var bus: String = ""
    /// Its I²C address, when it has one.
    var address: String = ""

    var id: String { name }

    /// The roles as one line, for a row that shows what the device is.
    var rolesLabel: String { roles.joined(separator: ", ") }

    /// The `resources:` entry as the spec would carry it: `name`/`roles`/`via` always, the rest only
    /// when stated — an empty string is not an answer, so it is omitted rather than written blank.
    var json: [String: Any] {
        var out: [String: Any] = ["name": name, "roles": roles, "via": via.rawValue]
        if !bus.isEmpty { out["bus"] = bus }
        if !address.isEmpty { out["address"] = address }
        return out
    }
}


/// The board-and-resources flow's state, held apart from the view so its rules are testable without
/// one.
///
/// v1 is deliberately small: choose a board, pick the parts it declares (none start selected — the
/// board *can* supply each, the application decides it *uses* it), add any others the application
/// composes against. What it produces is a **draft** — the `resources:` a later step turns into a
/// spec — not a spec itself, because `library`/`published` resources still need the driver units
/// that only the design step knows.
struct BoardResourceFlowState: Equatable {
    /// The boards offered — the catalogue filtered to what a firmware application is built *for*.
    var boards: [Platform] = []
    var selectedBoardID = ""
    /// The chosen board's profile, read once when it is chosen (`capabilities/profile`).
    var profile: BoardProfile?
    /// The capability catalogue, read once (`capabilities/vocabulary`).
    var catalogue: [VocabularyCategory] = []
    /// The resources declared so far: the board's parts that were *selected*, and anything added —
    /// the union the spec carries, in the order they were chosen.
    var resources: [ResourceDraft] = []
    /// The **description** the person wrote on the first step, in their own words — the input the brief
    /// is read from, and the thing the later steps are guided by.
    var description = ""
    /// The **brief** the describe step produced, when it did. `nil` until something is summarized: the
    /// brief is a guide, so the board and resource steps work without one — they simply lose the steer.
    var brief: DesignBrief?

    /// Which **fork** the person answered, and with which choice — by the fork's index in `brief.forks`.
    ///
    /// The brief itself is never overwritten: it is the model's reading, and keeping it means a pick can
    /// be changed — or taken back — exactly, with no accumulation of the paths a previous answer added.
    /// The *resolved* brief below is what the card shows and what the later steps read.
    var forkPicks: [Int: Int] = [:]

    /// The brief **with the person's fork answers applied** — the draft the describe step edits, and what
    /// the board and actor steps steer by.
    ///
    /// A fork is the one question a brief asks, and answering it has to mean something: picking a choice
    /// merges the `requires` and `suggests` that choice carries, so the `Needed` badges, the board-fit
    /// readout and the units the actor step starts from all follow the answer. That is the whole reason a
    /// fork is a pick rather than a question in prose the model would have to read again.
    var resolvedBrief: DesignBrief? {
        guard var brief else { return nil }
        for (index, pick) in forkPicks {
            guard brief.forks.indices.contains(index),
                brief.forks[index].choices.indices.contains(pick)
            else { continue }
            let choice = brief.forks[index].choices[pick]
            brief.requires = merging(brief.requires, choice.requires)
            brief.suggests = merging(brief.suggests, choice.suggests)
        }
        return brief
    }

    /// Answer a fork with one of its choices — or take the answer back by answering the same choice
    /// twice. The picks are kept and `resolvedBrief` is recomputed from the model's own brief, so the
    /// paths never accumulate across a change of mind.
    mutating func toggleFork(_ fork: Int, _ choice: Int) {
        forkPicks[fork] = forkPicks[fork] == choice ? nil : choice
    }

    /// Forget the fork answers — what the describe step does when a fresh brief is read, so an answer to
    /// the previous brief never lands on the new one.
    mutating func clearForkPicks() {
        forkPicks = [:]
    }

    /// `values` with `added` after them, in order and without repeats — the merge a fork's answer does.
    private func merging(_ values: [String], _ added: [String]) -> [String] {
        added.reduce(into: values) { result, value in
            if !result.contains(value) { result.append(value) }
        }
    }

    /// Whether there is a description to move on with. The brief itself is **not** required: a person
    /// may go on without summarizing, and the later steps simply lose the guidance — a guide, not a gate.
    var canLeaveDescribe: Bool {
        !description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var selectedBoard: Platform? { boards.first { $0.id == selectedBoardID } }

    /// The boards in a `platforms/list` payload: an entry the core calls a `board`, or — for a
    /// payload older than the `kind` field — an embedded platform. A chip is not a thing an
    /// application is built for, so it is never offered.
    static func boards(from platforms: [Platform]) -> [Platform] {
        platforms.filter { platform in
            if let kind = platform.kind { return kind == "board" }
            return platform.embedded
        }
    }

    /// One declared device as a resource draft, reached `via: board` — a part the BSP already
    /// reaches, so no component of this project drives it.
    static func boardPart(_ device: BoardDevice) -> ResourceDraft {
        ResourceDraft(
            name: device.name,
            roles: device.roles,
            via: .board,
            bus: device.bus,
            address: device.address
        )
    }

    /// The board's own parts as resource drafts, each reached `via: board`.
    static func boardParts(_ profile: BoardProfile) -> [ResourceDraft] {
        profile.devices.map(boardPart)
    }

    /// Choose a board: any previous board's *selected* parts go, because the parts a board supplies
    /// are its own — the new board's palette arrives **unselected**. Additions the application
    /// composed against stay: a capability the board does not carry is not the board's to drop.
    mutating func choose(boardID: String, profile: BoardProfile) {
        selectedBoardID = boardID
        self.profile = profile
        resources.removeAll { $0.via == .board }
    }

    /// Whether the board's own part of this name is part of the declared set.
    func isBoardPartSelected(_ name: String) -> Bool {
        resources.contains { $0.via == .board && $0.name == name }
    }

    /// Turn the board's part of this name on or off.
    ///
    /// A board's parts start **unselected**: a board *can* supply a part, but only the application
    /// decides it *uses* it, and the set the spec carries is the union of what was chosen. Selecting
    /// a part the board declares adds it `via: board`; selecting again removes it. The board's part
    /// wins a name an added resource already took — one name is one resource, and the board's is the
    /// real one. A name the board does not declare is ignored: the palette is the board's facts, not
    /// free text.
    mutating func toggleBoardPart(name: String) {
        guard let device = profile?.devices.first(where: { $0.name == name }) else { return }
        if isBoardPartSelected(name) {
            resources.removeAll { $0.via == .board && $0.name == name }
        } else {
            resources.removeAll { $0.via != .board && $0.name == name }
            resources.append(Self.boardPart(device))
        }
    }

    /// Whether the board declares a part for this role — i.e. the board *carries* the capability,
    /// whether or not the application has selected that part. A carried role is the board's own: it is
    /// reached through the board's part row and never added as a library resource, so the two surfaces
    /// cannot disagree about it (`power.pmu` is the board's `pmu` device, not a `power` addition).
    func boardCarriesRole(_ role: String) -> Bool {
        profile?.devices.contains { $0.carries(role: role) } ?? false
    }

    /// Turn a catalogue path on or off — the mirror of `toggleBoardPart` for a resource the board
    /// does **not** carry.
    ///
    /// A role the board carries as a part is left alone whatever the draft holds: the board's part row
    /// is its only control, so nothing here can declare it twice or take it back — one capability is
    /// never shown as both a board part and an addition. Otherwise selecting the path adds it
    /// `via: library`, named by the role's last segment (the forward-compat hook: a later step can
    /// switch it to `published` when the component is a managed dependency). Selecting it again takes
    /// the addition back out, because an addition was the application's own choice. The gate is by
    /// **role**, matching the catalogue's own "one capability once": a role can be carried by more than
    /// one resource (`audio_in` and `audio_out` are both `media.audio`), so a name-only check would let
    /// the same capability in twice.
    mutating func toggleCatalogueResource(role: String) {
        guard !boardCarriesRole(role) else { return }
        if let existing = resources.first(where: { $0.roles.contains(role) }) {
            guard existing.via != .board else { return }
            resources.removeAll { $0.via != .board && $0.roles.contains(role) }
        } else {
            let name = Self.defaultName(for: role)
            guard !name.isEmpty, !resources.contains(where: { $0.name == name }) else { return }
            resources.append(ResourceDraft(name: name, roles: [role], via: .library))
        }
    }

    /// Whether any declared resource already covers this capability — the read the catalogue's
    /// "one capability once" gate is built on. A board part and an added resource are both "covered".
    func declaresRole(_ role: String) -> Bool {
        resources.contains { $0.roles.contains(role) }
    }

    /// How the declared resource covering `role` is reached, when one is — `nil` when nothing covers
    /// it. A path `via: board` is the board's own part (the catalogue cannot take it back); anything
    /// else is an addition the application can still unchoose. This is what tells the catalogue's
    /// "on board" from its "added".
    func declaredVia(role: String) -> ResourceVia? {
        resources.first { $0.roles.contains(role) }?.via
    }

    /// Whether the brief asks for this capability — the read the `Needed` badges are drawn from. Empty
    /// when no brief has been read, so a step without one simply shows no badges.
    func isRequired(_ role: String) -> Bool {
        resolvedBrief?.requires.contains(role) ?? false
    }

    /// The brief's `requires` the board does **not** carry as a part — the capabilities the application
    /// will have to add as `library`/`published` resources.
    func requirementsNotOnBoard(_ brief: DesignBrief) -> [String] {
        brief.requires.filter { !boardCarriesRole($0) }
    }

    /// The brief's `requires` nothing in the draft covers yet — a **soft warning**, never a gate: the
    /// person is mid-design, and the rule the core enforces is the spec's, not the brief's.
    func uncoveredRequirements(_ brief: DesignBrief) -> [String] {
        brief.requires.filter { !declaresRole($0) }
    }

    /// A role's word as a resource name: its last dotted segment (`sensing.particulate` →
    /// `particulate`). Lower_snake already, because the vocabulary spells its paths that way.
    static func defaultName(for role: String) -> String {
        role.split(separator: ".").last.map(String.init) ?? role
    }

    /// The flow may go on when a board is chosen and every resource names a part and a role — the two
    /// things the Rust check insists on, so the button is disabled rather than the draft refused.
    var canContinue: Bool {
        !selectedBoardID.isEmpty && !resources.isEmpty
            && resources.allSatisfy {
                !$0.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    && !$0.roles.isEmpty
                    && $0.roles.allSatisfy {
                        !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    }
            }
    }
}

