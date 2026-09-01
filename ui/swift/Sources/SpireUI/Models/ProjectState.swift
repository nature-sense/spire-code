import Foundation

/// The canonical UI state of the app. Replaces scattered boolean flags
/// (showWelcome, showNewProject, loading, connected, creationExecuting…)
/// with a single state machine so views switch on one value.
enum ProjectState {
    /// App launched; no project opened yet — WelcomeView shown.
    case unconnected
    /// A project directory is being opened/analyzed — loading UI.
    case opening
    /// A project is loaded and analysis is available — project dashboard shown.
    case idle(ProjectInfo)
    /// The new-project planning flow (form → plan → executing).
    case creating(plan: PlanGenerationResult?, executing: Bool)
    /// Two-phase scaffold flow, structure step: the user chooses build system
    /// + platforms; once scaffolded, the spec is available for preview + fill.
    case scaffolding(spec: ScaffoldSpec?)
    /// Two-phase scaffold flow, fill step: the LLM fills the materialized
    /// scaffold inside its fill roots (plan may be nil until generated).
    case filling(spec: ScaffoldSpec, plan: PlanGenerationResult?)
    /// Transient error banner.
    case error(String)
}
