import Foundation

/// Encodes and decodes messages between Swift and Rust.
/// Uses JSON for the wire format (the C FFI layer speaks JSON strings).
enum MessageSerializer {
    /// Encode a command name into a JSON request body.
    /// Produces: {"method": "chat/append", "params": {"chatId": "default", "content": command}}
    static func encode(command: String) throws -> Data {
        let body: [String: Any] = [
            "method": command,
            "params": [:]
        ]
        return try JSONSerialization.data(withJSONObject: body)
    }

    /// Encode a chat message into a JSON request body.
    static func encode(chat text: String) throws -> Data {
        let body: [String: Any] = [
            "method": "chat/append",
            "params": [
                "chatId": "default",
                "content": text,
                "options": ["role": "user"]
            ]
        ]
        return try JSONSerialization.data(withJSONObject: body)
    }

    /// Decode a reply buffer into a Decodable type (JSON).
    static func decode<T: Decodable>(_ data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(T.self, from: data)
    }

    /// Decode a chat response from JSON.
    static func decodeChat(_ data: Data) throws -> ChatMessage {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        // Try decoding as a full ChatMessage first
        if let decoded = try? decoder.decode(ChatMessage.self, from: data) {
            return decoded
        }
        // Fallback: extract content from { "content": "..." } JSON
        if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let content = json["content"] as? String {
            return ChatMessage(
                id: UUID().uuidString,
                role: .assistant,
                content: content,
                timestamp: Date()
            )
        }
        throw MessageError.decodingFailed("Could not decode chat response")
    }
}

enum MessageError: Error {
    case notImplemented
    case invalidMessage
    case encodingFailed
    case decodingFailed(String)
}