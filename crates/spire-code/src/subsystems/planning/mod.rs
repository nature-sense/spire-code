// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Planning subsystem — intent routing, plan orchestration, error analysis.

pub mod error_analyzer;
pub use error_analyzer::{ErrorAnalyzer, ErrorAnalyzerMessage};
pub mod intent_router;
pub use intent_router::{IntentRouterActor, IntentRouterMessage, RouteResult};
pub mod plan_orchestrator;
pub use plan_orchestrator::{PlanOrchestrator, PlanOrchestratorMessage};

use tokio::sync::mpsc;

use spire_core::subsystems::chat::chat::ChatMessage;
use spire_core::subsystems::llm::llm::LlmMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::subsystems::tools::tool_orchestrator::ToolOrchestratorMessage;
use spire_core::actors::Actor;
use spire_actor::registry::ServiceRegistry;
use spire_actor::subsystem::Subsystem;
use spire_core::transport::socket::TransportMessage;

/// Handles for the planning subsystem.
pub struct PlanningHandles {
    pub orchestrator_tx: mpsc::Sender<PlanOrchestratorMessage>,
    pub router_tx: mpsc::Sender<IntentRouterMessage>,
    pub error_tx: mpsc::Sender<ErrorAnalyzerMessage>,
}

/// Cross-subsystem dependencies for planning.
pub struct PlanningDeps {
    pub memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    pub llm_tx: mpsc::Sender<LlmMessage>,
    pub tool_orchestrator_tx: mpsc::Sender<ToolOrchestratorMessage>,
    pub chat_tx: mpsc::Sender<ChatMessage>,
    pub transport_tx: mpsc::Sender<TransportMessage>,
}

/// Cohesive actor group for intent → plan → execution.
pub struct PlanningSubsystem {
    pub deps: PlanningDeps,
}

impl Subsystem for PlanningSubsystem {
    type Handles = PlanningHandles;

    fn spawn(self, registry: &ServiceRegistry) -> Self::Handles {
        // 1. Intent router
        let (router_tx, router_rx) = mpsc::channel(64);
        let _ = registry.register::<IntentRouterMessage>("planning.router", router_tx.clone());
        let _join = IntentRouterActor::new(self.deps.memory_graph_tx.clone()).spawn(router_rx);

        // 2. Error analyzer
        let (error_tx, error_rx) = mpsc::channel(64);
        let _ = registry.register::<ErrorAnalyzerMessage>("planning.error_analyzer", error_tx.clone());
        let _join2 = ErrorAnalyzer::new(self.deps.memory_graph_tx.clone()).spawn(error_rx);

        // 3. Plan orchestrator (consumes router/error outputs via deps)
        let (orchestrator_tx, orchestrator_rx) = mpsc::channel(64);
        let _ = registry.register::<PlanOrchestratorMessage>("planning.orchestrator", orchestrator_tx.clone());
        let _join3 = PlanOrchestrator::new(
            self.deps.memory_graph_tx,
            self.deps.llm_tx,
            self.deps.tool_orchestrator_tx,
            self.deps.chat_tx,
            self.deps.transport_tx,
        )
        .spawn(orchestrator_rx);

        PlanningHandles {
            orchestrator_tx,
            router_tx,
            error_tx,
        }
    }

    fn actor_count(&self) -> usize {
        3
    }
}
