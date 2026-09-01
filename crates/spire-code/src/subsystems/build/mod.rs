// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Build subsystem — language-module router + orchestration.

pub mod build_manager;
pub use build_manager::{BuildManagerActor, BuildManagerMessage};
pub mod build_orchestrator;
pub use build_orchestrator::{BuildOrchestrator, BuildOrchestratorMessage};

use tokio::sync::mpsc;

use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::Actor;
use spire_actor::registry::ServiceRegistry;
use spire_actor::subsystem::Subsystem;

/// Handles for the build subsystem.
pub struct BuildHandles {
    /// Sender to the build manager actor.
    pub manager_tx: mpsc::Sender<BuildManagerMessage>,
}

/// Cohesive actor group for build/test/install/lint + module routing.
pub struct BuildSubsystem {
    /// Graph sender the build manager reads build configs from.
    pub memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    /// Shared buffer of streaming build events, drained directly by the FFI
    /// while a build is running. Must be the SAME Arc the FFI reads so events
    /// pushed here are visible to the UI poller.
    pub build_event_buffer: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

impl Subsystem for BuildSubsystem {
    type Handles = BuildHandles;

    fn spawn(self, registry: &ServiceRegistry) -> Self::Handles {
        let (manager_tx, manager_rx) = mpsc::channel(64);
        let _ = registry.register::<BuildManagerMessage>("build.manager", manager_tx.clone());
        // Compatibility alias.
        let _ = registry.register::<BuildManagerMessage>("build_manager", manager_tx.clone());
        let _join = BuildManagerActor::new(
            self.memory_graph_tx,
            self.build_event_buffer.clone(),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        )
        .spawn(manager_rx);
        BuildHandles { manager_tx }
    }

    fn actor_count(&self) -> usize {
        1
    }
}
