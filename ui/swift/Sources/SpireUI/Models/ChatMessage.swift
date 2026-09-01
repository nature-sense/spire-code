import Foundation

struct ChatMessage: Codable, Identifiable {
    let id: String
    let role: MessageRole
    let content: String
    let timestamp: Date
}

enum MessageRole: String, Codable {
    case user
    case assistant
    case system
}