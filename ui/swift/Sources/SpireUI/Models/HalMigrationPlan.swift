import Foundation

/// Mirror of the Rust `hal_migration` plan/result types, decoded from the
/// `hal_migrate_plan` / `hal_migrate_apply` tool replies.
struct HalMigrationPlan: Codable {
    var layout: String
    var layoutName: String
    var moves: [HalMove]
    var writeFiles: [HalWrite]
    var buildFileEdits: [HalBuildEdit]
    var conflicts: [String]
    var reasons: [String]
    var canApply: Bool
    var notes: [String]

    enum CodingKeys: String, CodingKey {
        case layout
        case layoutName = "layout_name"
        case moves
        case writeFiles = "write_files"
        case buildFileEdits = "build_file_edits"
        case conflicts
        case reasons
        case canApply = "can_apply"
        case notes
    }
}

struct HalMove: Codable {
    var from: String
    var to: String
}

struct HalWrite: Codable {
    var path: String
    var content: String
}

struct HalBuildEdit: Codable {
    var file: String
    var before: String
    var after: String
}

struct HalMigrationResult: Codable {
    var appliedMoves: [String]
    var writtenFiles: [String]
    var appliedEdits: [String]
    var errors: [String]

    enum CodingKeys: String, CodingKey {
        case appliedMoves = "applied_moves"
        case writtenFiles = "written_files"
        case appliedEdits = "applied_edits"
        case errors
    }
}

// MARK: - HAL sanity (first-open health check)

struct HalSanityReport: Codable {
    var status: String
    var layout: String
    var issues: [HalIssue]
    var platforms: [String]
    var interfaces: [String]
}

struct HalIssue: Codable {
    var severity: String
    var title: String
    var path: String
    var suggestedFix: String

    enum CodingKeys: String, CodingKey {
        case severity, title, path
        case suggestedFix = "suggested_fix"
    }
}
