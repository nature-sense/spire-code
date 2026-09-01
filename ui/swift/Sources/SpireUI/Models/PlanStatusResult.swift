import Foundation

/// Result of a PlanOrchestrator create/query operation, decoded from the
/// Rust `PlanStatusResult` JSON returned by the `plan/create` RPC.
struct PlanStatusResult: Decodable {
    let planId: String
    let goal: String
    let status: String
    let intentName: String?
    let steps: [PlanStepEntry]
    let totalSteps: Int
    let completedSteps: Int
    let failedSteps: Int

    enum CodingKeys: String, CodingKey {
        case planId = "plan_id"
        case goal
        case status
        case intentName = "intent_name"
        case steps
        case totalSteps = "total_steps"
        case completedSteps = "completed_steps"
        case failedSteps = "failed_steps"
    }
}

/// A single step within a plan, decoded from Rust `PlanStepEntry` JSON.
struct PlanStepEntry: Decodable, Identifiable {
    let id: String
    let order: Int
    let description: String
    let stepName: String
    let status: String
    let result: String?
    let error: String?

    enum CodingKeys: String, CodingKey {
        case id
        case order
        case description
        case stepName = "step_name"
        case status
        case result
        case error
    }
}
