// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Project subsystem — project lifecycle: sync, analyze, query, create.

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

use tokio::sync::mpsc;

use crate::subsystems::build::build_manager::BuildManagerMessage;
use spire_core::subsystems::llm::llm::LlmMessage;
use spire_core::subsystems::mcp::mcp_client::McpClientMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::Actor;
use spire_actor::registry::ServiceRegistry;
use spire_actor::subsystem::Subsystem;

/// Filesystem module sender (in-process module from spire-modules).
pub type FilesystemTx = mpsc::Sender<spire_core::modules::FilesystemMessage>;

/// Handles for the project subsystem.
pub struct ProjectHandles {
    pub sync_tx: mpsc::Sender<ProjectSyncMessage>,
    pub analyzer_tx: mpsc::Sender<ProjectAnalyzerMessage>,
    pub query_tx: mpsc::Sender<ProjectQueryMessage>,
    pub creation_tx: mpsc::Sender<ProjectCreationMessage>,
}

/// Dependencies the project subsystem needs from other subsystems.
pub struct ProjectDeps {
    pub filesystem_tx: FilesystemTx,
    pub build_manager_tx: mpsc::Sender<BuildManagerMessage>,
    pub mcp_client_tx: mpsc::Sender<McpClientMessage>,
    pub memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    pub llm_tx: mpsc::Sender<LlmMessage>,
}

/// Cohesive actor group for project lifecycle.
pub struct ProjectSubsystem {
    pub deps: ProjectDeps,
}

impl Subsystem for ProjectSubsystem {
    type Handles = ProjectHandles;

    fn spawn(self, registry: &ServiceRegistry) -> Self::Handles {
        // 1. Sync
        let (sync_tx, sync_rx) = mpsc::channel(64);
        let _ = registry.register::<ProjectSyncMessage>("project.sync", sync_tx.clone());
        let _join = ProjectSyncActor::new().spawn(sync_rx);

        // 2. Analyzer
        let (analyzer_tx, analyzer_rx) = mpsc::channel(64);
        let _ = registry.register::<ProjectAnalyzerMessage>("project.analyzer", analyzer_tx.clone());
        let _join2 = ProjectAnalyzerActor::new().spawn(analyzer_rx);

        // 3. Query
        let (query_tx, query_rx) = mpsc::channel(64);
        let _ = registry.register::<ProjectQueryMessage>("project.query", query_tx.clone());
        let _join3 = ProjectQueryActor::new().spawn(query_rx);

        // 4. Creation (needs analyzer + llm wired via setters)
        let (creation_tx, creation_rx) = mpsc::channel(64);
        let _ = registry.register::<ProjectCreationMessage>("project.creation", creation_tx.clone());
        let mut creation = ProjectCreationActor::new(
            self.deps.filesystem_tx,
            self.deps.build_manager_tx,
            self.deps.mcp_client_tx,
        );
        creation.set_project_analyzer(analyzer_tx.clone());
        creation.set_llm(self.deps.llm_tx);
        let _join4 = creation.spawn(creation_rx);

        ProjectHandles {
            sync_tx,
            analyzer_tx,
            query_tx,
            creation_tx,
        }
    }

    fn actor_count(&self) -> usize {
        4
    }
}
