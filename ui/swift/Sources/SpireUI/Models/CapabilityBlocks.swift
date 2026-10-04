import Foundation

/// The capability blocks the graph holds for an entry — what a board `realizes`, what a chip
/// `provides`, what a board `carries`, and how a board is `pins`-wired.
///
/// Read from the graph by `platforms/config`. The registry YAML is only the seed; the graph is what
/// a build resolves against, so this is what the configuration screen shows.
struct CapabilityBlocks: Codable, Hashable {
    let realizes: [CapabilityEdge]
    let provides: [CapabilityEdge]
    let carries: [CompanionEdge]
    let pins: [PinFunction]
    /// The **parts** the entry declares (`devices:`) — each with the capability it *is* and its own
    /// values (`part`, `bus`, `address`).
    ///
    /// Decoded **tolerantly**: a reply from a core older than the device layer omits the key, and a
    /// required field would fail the whole `platforms/config` decode — an empty screen rather than a
    /// missing group. `[]` says "declares no parts", which is the truth for a chip.
    let devices: [BoardDevice]

    enum CodingKeys: String, CodingKey {
        case realizes, provides, carries, pins, devices
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        realizes = try container.decode([CapabilityEdge].self, forKey: .realizes)
        provides = try container.decode([CapabilityEdge].self, forKey: .provides)
        carries = try container.decode([CompanionEdge].self, forKey: .carries)
        pins = try container.decode([PinFunction].self, forKey: .pins)
        devices = try container.decodeIfPresent([BoardDevice].self, forKey: .devices) ?? []
    }

    /// True when the entry declares nothing at all — so the screen can say so rather than draw empty
    /// groups.
    var isEmpty: Bool {
        realizes.isEmpty && provides.isEmpty && carries.isEmpty && pins.isEmpty && devices.isEmpty
    }
}

/// A capability an entry **realizes** (board) or **provides** (chip), with the values written under
/// it (`interface`, `cores`, `tops`, …).
struct CapabilityEdge: Codable, Hashable, Identifiable {
    let capability: String
    let properties: [String: JSONValue]
    var id: String { capability }
}

/// A companion chip a board **carries** (`companions:`), with what is written beside it
/// (`role`, `link`, `firmware`).
struct CompanionEdge: Codable, Hashable, Identifiable {
    let chip: String
    let properties: [String: JSONValue]
    var id: String { chip }
}

/// A **wiring function** a board declares (`pins:`), with its assignment — `led` → `{ pin: GPIO8 }`.
/// A grouping flattens to a dotted path (`grove.a`).
struct PinFunction: Codable, Hashable, Identifiable {
    let function: String
    let properties: [String: JSONValue]
    var id: String { function }
}

/// The `platforms/config` reply envelope.
struct PlatformConfigReply: Decodable {
    let platforms: [Platform]
}

/// Any JSON value. A config property is open-ended — `pin`, `addressable`, `tops`, `link`, nested
/// lists — so it is held as-is rather than forced into a per-field schema that would have to grow
/// with the vocabulary every time a board states a new fact.
enum JSONValue: Codable, Hashable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case array([JSONValue])
    case object([String: JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let b = try? container.decode(Bool.self) {
            self = .bool(b)
        } else if let n = try? container.decode(Double.self) {
            self = .number(n)
        } else if let s = try? container.decode(String.self) {
            self = .string(s)
        } else if let a = try? container.decode([JSONValue].self) {
            self = .array(a)
        } else if let o = try? container.decode([String: JSONValue].self) {
            self = .object(o)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "value is not JSON")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let s): try container.encode(s)
        case .number(let n): try container.encode(n)
        case .bool(let b): try container.encode(b)
        case .array(let a): try container.encode(a)
        case .object(let o): try container.encode(o)
        case .null: try container.encodeNil()
        }
    }

    /// The scalar text, when this is a string — a property read by name (`bus`, `address`) rather
    /// than rendered. `nil` for anything else, so a caller can tell "absent" from "empty".
    var stringValue: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    /// A scalar's text. A list or object is summarised into one line so a value row stays a row —
    /// `[GPIO2, GPIO1]`, `{ bus: sdio, pins: […] }`.
    var display: String {
        switch self {
        case .string(let s):
            return s
        case .bool(let b):
            return b ? "true" : "false"
        case .number(let n):
            return n == n.rounded() && abs(n) < 1e15 ? String(Int(n)) : String(n)
        case .array(let a):
            return "[" + a.map(\.display).joined(separator: ", ") + "]"
        case .object(let o):
            return "{"
                + o.map { "\($0.key): \($0.value.display)" }.sorted().joined(separator: ", ")
                + "}"
        case .null:
            return "—"
        }
    }
}
