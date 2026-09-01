import Foundation

/// An MCP server as returned by the Rust core.
struct McpServerInfo: Codable, Identifiable {
    var id: String { name }
    let name: String
    let description: String
    let serverType: String
    let toolCount: Int
    let properties: [String: AnyCodable]?
    /// Build system this server serves (e.g. "Cargo", "npm"); nil for general servers.
    let buildType: String?

    enum CodingKeys: String, CodingKey {
        case name, description, serverType = "server_type",
             toolCount = "tool_count", properties, buildType = "build_type"
    }
}

/// An MCP tool exposed by a server.
struct McpToolInfo: Codable, Identifiable {
    var id: String { name }
    let name: String
    let description: String?

    enum CodingKeys: String, CodingKey {
        case name
        case description
    }
}

/// Type-erased codable wrapper for arbitrary JSON values.
struct AnyCodable: Codable {
    let value: Any

    init(_ value: Any) { self.value = value }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let intVal = try? container.decode(Int.self) { value = intVal }
        else if let doubleVal = try? container.decode(Double.self) { value = doubleVal }
        else if let boolVal = try? container.decode(Bool.self) { value = boolVal }
        else if let stringVal = try? container.decode(String.self) { value = stringVal }
        else if let arrayVal = try? container.decode([AnyCodable].self) { value = arrayVal.map { $0.value } }
        else if let dictVal = try? container.decode([String: AnyCodable].self) { value = dictVal.mapValues { $0.value } }
        else { value = "" }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        if let intVal = value as? Int { try container.encode(intVal) }
        else if let doubleVal = value as? Double { try container.encode(doubleVal) }
        else if let boolVal = value as? Bool { try container.encode(boolVal) }
        else if let stringVal = value as? String { try container.encode(stringVal) }
        else if let arrayVal = value as? [Any] { try container.encode(arrayVal.map(AnyCodable.init)) }
        else if let dictVal = value as? [String: Any] { try container.encode(dictVal.mapValues(AnyCodable.init)) }
        else { try container.encode("") }
    }
}