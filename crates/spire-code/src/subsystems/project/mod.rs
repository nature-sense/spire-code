// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Project subsystem — project lifecycle: sync, analyze, query, create.

pub mod spec;
pub mod spec_codegen;
pub mod spec_gen;
pub mod spec_graph;
pub mod spec_md;

pub mod project_analyzer;
pub use project_analyzer::{
    LanguageBreakdown, ProjectAnalysis, ProjectAnalyzerActor, ProjectAnalyzerMessage, RoleBreakdown,
};
pub mod project_build;
pub use project_build::{ProjectBuildActor, ProjectBuildMessage};
pub mod project_creation;
pub use project_creation::{
    CreationStep, CreationStepType, PlanGenerationResult, ProjectCreationActor,
    ProjectCreationMessage, StepExecutionResult, StepStatus,
};
pub mod project_install;
pub use project_install::{ProjectInstallActor, ProjectInstallMessage};
pub mod project_lint;
pub use project_lint::{ProjectLintActor, ProjectLintMessage};
pub mod project_query;
pub use project_query::{ProjectQueryActor, ProjectQueryMessage};
pub mod project_sync;
pub use project_sync::{ChangeType, ProjectSyncActor, ProjectSyncMessage, SyncResult};
pub mod project_test;
pub use project_test::{ProjectTestActor, ProjectTestMessage};
