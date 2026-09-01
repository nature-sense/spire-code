import Foundation

/// A discrete step in a project creation/planning pipeline (mirrors the Rust types).
struct CreationStep: Codable, Identifiable {
    let id: String
    let stepType: CreationStepType
    let description: String
    var status: StepStatus
    var parameters: AnyCodable?
    var result: String?

    enum CodingKeys: String, CodingKey {
        case id
        case stepType = "stepType"
        case description
        case status
        case parameters
        case result
    }
}

/// The type of a creation step (mirrors Rust `CreationStepType`).
enum CreationStepType: String, Codable, CaseIterable {
    case createDirectory = "create_directory"
    case writeBuildConfig = "write_build_config"
    case addDependency = "add_dependency"
    case writeSourceFile = "write_source_file"
    case parseAndValidate = "parse_and_validate"
    case toolCall = "tool_call"
    case build
    case test

    var label: String {
        switch self {
        case .createDirectory: return "Create Directory"
        case .writeBuildConfig: return "Write Build Config"
        case .addDependency: return "Add Dependency"
        case .writeSourceFile: return "Write Source File"
        case .parseAndValidate: return "Parse & Validate"
        case .toolCall: return "Tool Call"
        case .build: return "Build"
        case .test: return "Test"
        }
    }

    var systemImage: String {
        switch self {
        case .createDirectory: return "folder.badge.plus"
        case .writeBuildConfig: return "wrench.and.screwdriver"
        case .addDependency: return "plus.circle"
        case .writeSourceFile: return "doc.badge.plus"
        case .parseAndValidate: return "checkmark.circle.badge.questionmark"
        case .toolCall: return "wrench.and.screwdriver.fill"
        case .build: return "hammer.fill"
        case .test: return "checkmark.shield"
        }
    }
}

/// Execution status of a step (mirrors Rust `StepStatus`).
enum StepStatus: String, Codable {
    case pending, executing, completed, failed
}

/// The full plan generation result from the Rust core.
struct PlanGenerationResult: Codable {
    let goal: String
    let language: String
    let rootDir: String
    var steps: [CreationStep]
    /// True when the plan is the deterministic template fallback (LLM timed
    /// out / unavailable) rather than an LLM-generated implementation plan.
    let isTemplate: Bool

    enum CodingKeys: String, CodingKey {
        case goal, language
        case rootDir = "root_dir"
        case steps
        case isTemplate = "is_template"
    }
}

/// Result of the single-round-trip `createProject/Plan`: the in-memory
/// structural contract (ScaffoldSpec, NOT yet written to disk) plus the LLM
/// plan that implements the goal inside it. The wizard shows the plan for
/// OK/Reject — on OK it scaffolds (materializes the spec), then executes.
struct PlanScaffoldResult: Codable {
    let plan: PlanGenerationResult
    let spec: ScaffoldSpec

    enum CodingKeys: String, CodingKey {
        case plan, spec
    }
}

/// Result of executing a single step.
struct StepExecutionResult: Codable {
    let stepId: String
    let success: Bool
    let message: String

    enum CodingKeys: String, CodingKey {
        case stepId = "step_id"
        case success
        case message
    }
}