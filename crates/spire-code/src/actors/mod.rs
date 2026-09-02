// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Actor system — coding/development actors.
//!
//! These depend on the generic actors in `spire-core` (LLM, tools, MCP, RAG,
//! graph, …) and on this crate's build modules. The composer (the FFI / the
//! standalone binary) wires them.

pub mod coordinator;
pub use coordinator::{CoordinatorActor, CoordinatorMessage, FfiSharedState};

pub mod hal_fill;

pub mod platform_codec;

pub mod startup_phases;

pub mod system;
pub use system::{SystemActor, SystemMessage};

pub mod tool_providers;
pub use tool_providers::build_default_registry;

pub mod vscode_tools;
pub use vscode_tools::vscode_tool_definitions;

// Subsystem-owned actors (build / project / planning) live in
// `crate::subsystems::{build,project,planning}` — each subsystem dir owns its
// actor implementations. Re-export their public types at the legacy flat paths.
pub use crate::subsystems::build::build_manager::{BuildManagerActor, BuildManagerMessage};
pub use crate::subsystems::build::build_orchestrator::{
    BuildOrchestrator, BuildOrchestratorMessage,
};
pub use crate::subsystems::planning::error_analyzer::{ErrorAnalyzer, ErrorAnalyzerMessage};
pub use crate::subsystems::planning::intent_router::{
    IntentRouterActor, IntentRouterMessage, RouteResult,
};
pub use crate::subsystems::planning::plan_orchestrator::{
    PlanOrchestrator, PlanOrchestratorMessage,
};
pub use crate::subsystems::project::project_analyzer::{
    LanguageBreakdown, ProjectAnalysis, ProjectAnalyzerActor, ProjectAnalyzerMessage, RoleBreakdown,
};
pub use crate::subsystems::project::project_build::{ProjectBuildActor, ProjectBuildMessage};
pub use crate::subsystems::project::project_creation::{
    CreationStep, CreationStepType, PlanGenerationResult, ProjectCreationActor,
    ProjectCreationMessage, StepExecutionResult, StepStatus,
};
pub use crate::subsystems::project::project_install::{ProjectInstallActor, ProjectInstallMessage};
pub use crate::subsystems::project::project_lint::{ProjectLintActor, ProjectLintMessage};
pub use crate::subsystems::project::project_query::{ProjectQueryActor, ProjectQueryMessage};
pub use crate::subsystems::project::project_sync::{
    ChangeType, ProjectSyncActor, ProjectSyncMessage, SyncResult,
};
pub use crate::subsystems::project::project_test::{ProjectTestActor, ProjectTestMessage};

// Generic actor types (from `spire-core`) re-exported at this root so the
// coding actors can keep the flat `crate::actors::X` paths they used before
// the split.
pub use spire_core::actors::{
    Actor, ActorError, ActorSystem, ChatActor, ChatMessage, FileChangeKind, FileChangeNotification,
    FileEventBatch, FileEventInfo, FileWatcherActor, FileWatcherMessage, LlmActor, LlmConfig,
    LlmMessage, McpClientActor, McpClientMessage, MemoryGraphActor, MemoryGraphMessage,
    ProgressActor, ProgressMessage, ProgressStatus, ProgressUpdate, PromptContext,
    PromptHandlerActor, PromptHandlerMessage, RagActor, RagMessage, SystemPromptActor,
    SystemPromptMessage, ToolInfo, ToolMessage, ToolOrchestrator, ToolOrchestratorMessage,
    ToolRouterActor, ToolRouterMessage, ToolsActor, ToolsMessage,
};
