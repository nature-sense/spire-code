import Foundation

/// The main modes of the Spire app.
enum SpireMode: String, CaseIterable, Identifiable, Hashable, Sendable {
    case explorer  = "Explorer"
    case project   = "Project"
    case planning  = "Planning"
    case tools     = "Tools"

    var id: String { rawValue }

    var icon: String {
        switch self {
        case .explorer: return "folder"
        case .project:  return "hammer"
        case .planning: return "brain"
        case .tools:    return "wrench.and.screwdriver"
        }
    }
}
