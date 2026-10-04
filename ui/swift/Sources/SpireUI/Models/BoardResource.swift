import Foundation

/// One **part** a board declares it carries — a `Device` node, read back by `capabilities/profile`.
///
/// The name (`display`, `rtc`) is what an application's `resources:` refers to; `role` is the
/// capability the part *is* (`media.display`); `properties` is everything the board wrote beside it
/// (`part`, `bus`, `address`). A part with no role cannot exist: the core refuses a device whose role
/// the vocabulary does not name, so a `role` here is either a known path or the part was dropped.
struct BoardDevice: Codable, Hashable, Identifiable {
    let name: String
    let role: String?
    let properties: [String: JSONValue]

    var id: String { name }

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
/// vocabulary` reply. The catalogue, not a board's facts: every path here is one a device's `role:`
/// (or an application's `resources[].role`) may take, because the same embedded vocabulary validates
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
    var role: String
    var via: ResourceVia
    /// The board wiring function it is reached on, when it has one.
    var bus: String = ""
    /// Its I²C address, when it has one.
    var address: String = ""

    var id: String { name }

    /// The `resources:` entry as the spec would carry it: `name`/`role`/`via` always, the rest only
    /// when stated — an empty string is not an answer, so it is omitted rather than written blank.
    var json: [String: Any] {
        var out: [String: Any] = ["name": name, "role": role, "via": via.rawValue]
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
            role: device.role ?? "",
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

    /// Turn a catalogue path on or off — the mirror of `toggleBoardPart` for a resource the board
    /// does not carry.
    ///
    /// Selecting the path adds it `via: library`, named by the role's last segment (the forward-compat
    /// hook: a later step can switch it to `published` when the component is a managed dependency).
    /// Selecting it again takes the addition back out, because an addition was the application's own
    /// choice. A path the board's part already carries is left alone — the board's resource is the
    /// real one. The gate is by **role**, matching the catalogue's own "one capability once": a role
    /// can be carried by more than one resource (`audio_in` and `audio_out` are both `media.audio`),
    /// so a name-only check would let the same capability in twice.
    mutating func toggleCatalogueResource(role: String) {
        if let existing = resources.first(where: { $0.role == role }) {
            guard existing.via != .board else { return }
            resources.removeAll { $0.via != .board && $0.role == role }
        } else {
            let name = Self.defaultName(for: role)
            guard !name.isEmpty, !resources.contains(where: { $0.name == name }) else { return }
            resources.append(ResourceDraft(name: name, role: role, via: .library))
        }
    }

    /// Whether any declared resource already covers this capability — the read the catalogue's
    /// "one capability once" gate is built on. A board part and an added resource are both "covered".
    func declaresRole(_ role: String) -> Bool {
        resources.contains { $0.role == role }
    }

    /// How the declared resource covering `role` is reached, when one is — `nil` when nothing covers
    /// it. A path `via: board` is the board's own part (the catalogue cannot take it back); anything
    /// else is an addition the application can still unchoose. This is what tells the catalogue's
    /// "on board" from its "added".
    func declaredVia(role: String) -> ResourceVia? {
        resources.first { $0.role == role }?.via
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
                    && !$0.role.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            }
    }
}

