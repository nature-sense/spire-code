// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! SystemActor — lifecycle state machine, health, and configuration management.
//!
//! This actor owns the system lifecycle as a state machine. On `Initialize`,
//! it drives the full startup sequence by delegating to modular startup phases.
//! Unlike the previous monolithic `run_initialize()`, the SystemActor now
//! holds a `current_phase` and delegates incoming messages to it, keeping
//! the mailbox responsive.
//!
//! The phase chain is defined in `startup_phases.rs`:
//!   GraphInitPhase → ParallelInitPhase → ParallelConnectPhase → RegisterToolsPhase
//!
//! Background work (embedder download, project sync, project analysis) runs
//! concurrently via the phase system — they don't block the fast path to Ready.

use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

use crate::actors::coordinator::CoordinatorMessage;
use spire_core::subsystems::llm::llm::LlmMessage;
use spire_core::subsystems::mcp::mcp_client::McpClientMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::progress::ProgressMessage;
use crate::subsystems::project::project_analyzer::ProjectAnalyzerMessage;
use crate::subsystems::project::project_query::ProjectQueryMessage;
use crate::subsystems::project::project_sync::ProjectSyncMessage;
use crate::actors::startup_phases::{self, PhaseContext, PhaseResult, StartupPhase};
use crate::actors::{Actor, ActorError};
use spire_core::models::embedding::Embedder;

// ============================================================================
// SystemState — lifecycle state machine
// ============================================================================

/// Lifecycle states for the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemState {
    /// Initial state — not yet initialized.
    Initializing,
    /// System is fully operational.
    Ready,
    /// System is shutting down.
    ShuttingDown,
    /// System encountered a fatal error during initialization.
    Failed(String),
}

impl std::fmt::Display for SystemState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemState::Initializing => write!(f, "initializing"),
            SystemState::Ready => write!(f, "ready"),
            SystemState::ShuttingDown => write!(f, "shutting_down"),
            SystemState::Failed(msg) => write!(f, "failed: {}", msg),
        }
    }
}

// ============================================================================
// SystemMessage
// ============================================================================

/// Messages for the System actor.
pub enum SystemMessage {
    /// Set the system actor's own sender (for sending PhaseEvent to self).
    /// Must be sent before Initialize.
    SetSystemTx {
        system_tx: mpsc::Sender<SystemMessage>,
    },

    /// Start the full initialization sequence.
    /// The SystemActor will drive the state machine by delegating to startup phases.
    Initialize {
        coordinator_tx: mpsc::Sender<CoordinatorMessage>,
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
        project_sync_tx: mpsc::Sender<ProjectSyncMessage>,
        project_analyzer_tx: mpsc::Sender<ProjectAnalyzerMessage>,
        project_query_tx: mpsc::Sender<ProjectQueryMessage>,
        llm_tx: mpsc::Sender<LlmMessage>,
        progress_tx: mpsc::Sender<ProgressMessage>,
        embedder: Arc<dyn Embedder>,
        data_dir: PathBuf,
        project_root: PathBuf,
        reply_to: oneshot::Sender<Result<(), ActorError>>,
    },

    /// A phase has completed or needs attention.
    PhaseEvent,

    /// Get system status.
    GetStatus { reply_to: oneshot::Sender<Value> },
    /// Graceful shutdown.
    Shutdown {
        reply_to: oneshot::Sender<Result<(), ActorError>>,
    },
    /// Get a configuration value by key.
    GetConfig {
        key: String,
        reply_to: oneshot::Sender<Option<Value>>,
    },
}

// ============================================================================
// SystemActor
// ============================================================================

/// Actor that manages system lifecycle via a phase-based state machine.
pub struct SystemActor {
    /// Current lifecycle state.
    state: SystemState,
    /// System start time.
    start_time: std::time::Instant,
    /// Configuration key-value store.
    config: std::collections::HashMap<String, Value>,
    /// The current startup phase (None if not initializing or if complete).
    current_phase: Option<Box<dyn StartupPhase>>,
    /// Shared context for phases.
    ctx: Option<PhaseContext>,
    /// Reply channel for the Initialize caller.
    init_reply: Option<oneshot::Sender<Result<(), ActorError>>>,
    /// Sender to self — used to send PhaseEvent messages.
    system_tx: Option<mpsc::Sender<SystemMessage>>,
}

impl SystemActor {
    pub fn new() -> Self {
        Self {
            state: SystemState::Initializing,
            start_time: std::time::Instant::now(),
            config: std::collections::HashMap::new(),
            current_phase: None,
            ctx: None,
            init_reply: None,
            system_tx: None,
        }
    }

    fn uptime_seconds(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Build the phase chain and start the first phase.
    async fn start_phase_chain(&mut self) {
        let ctx = match self.ctx.as_ref() {
            Some(ctx) => ctx.clone(),
            None => return,
        };

        let mcp_config_path = self
            .config
            .get("mcp_config_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        self.current_phase = Some(startup_phases::build_startup_phases(mcp_config_path));

        info!(
            "SystemActor: starting phase chain with phase '{}'",
            self.current_phase
                .as_ref()
                .map(|p| p.name())
                .unwrap_or("none")
        );

        // Start the first phase
        let result = if let Some(ref mut phase) = self.current_phase {
            phase.start(&ctx).await
        } else {
            return;
        };

        match result {
            PhaseResult::Complete => {
                info!("SystemActor: startup complete");
                self.finish_initialization(ctx).await;
            }
            PhaseResult::Failed(e) => {
                self.fail_initialization(ctx, e).await;
            }
            PhaseResult::InProgress => {
                // Phase is running in background — it will send PhaseEvent when done.
                // No polling needed; completion is signaled via oneshot channels.
            }
        }
    }

    /// Handle a PhaseEvent — check if the current phase has completed.
    /// Phases that spawn background tasks signal completion by sending
    /// PhaseEvent through the system_tx channel. This method is called
    /// when that message is received, avoiding any polling.
    async fn handle_phase_event(&mut self) {
        let ctx = match self.ctx.as_ref() {
            Some(ctx) => ctx.clone(),
            None => return,
        };

        let phase = match self.current_phase.as_mut() {
            Some(phase) => phase,
            None => return,
        };

        // Forward the PhaseEvent to the current phase.
        // PhaseGroup will check its sub-phases' oneshot completion channels.
        let result = phase.handle_message(SystemMessage::PhaseEvent, &ctx).await;
        match result {
            PhaseResult::Complete => {
                info!("SystemActor: phase '{}' complete", phase.name());
                self.current_phase = None;
                if self.state == SystemState::Initializing {
                    self.finish_initialization(ctx).await;
                }
            }
            PhaseResult::Failed(e) => {
                self.fail_initialization(ctx, e).await;
            }
            PhaseResult::InProgress => {
                // Still running — will send another PhaseEvent when done.
                // No polling needed.
            }
        }
    }

    /// Mark initialization as complete and reply to the caller.
    async fn finish_initialization(&mut self, ctx: PhaseContext) {
        self.state = SystemState::Ready;
        ctx.publish_progress("ready", "Starting Spire — complete!", 100.0)
            .await;
        info!("SystemActor: system is ready");

        if let Some(reply) = self.init_reply.take() {
            let _ = reply.send(Ok(()));
        }
    }

    /// Mark initialization as failed and reply to the caller.
    async fn fail_initialization(&mut self, ctx: PhaseContext, error: String) {
        error!("SystemActor: initialization failed: {}", error);
        self.state = SystemState::Failed(error.clone());
        self.current_phase = None;
        ctx.publish_progress("complete", "Starting Spire — complete!", 100.0)
            .await;

        if let Some(reply) = self.init_reply.take() {
            let _ = reply.send(Err(ActorError::SetupFailed(error)));
        }
    }
}

impl Default for SystemActor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for SystemActor {
    type Message = SystemMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            SystemMessage::SetSystemTx { system_tx } => {
                self.system_tx = Some(system_tx);
            }

            SystemMessage::Initialize {
                coordinator_tx,
                memory_graph_tx,
                mcp_client_tx,
                project_sync_tx,
                project_analyzer_tx,
                project_query_tx,
                llm_tx,
                progress_tx,
                embedder,
                data_dir,
                project_root,
                reply_to,
            } => {
                let system_tx = self
                    .system_tx
                    .clone()
                    .expect("SystemActor: SetSystemTx must be sent before Initialize");

                let ctx = PhaseContext {
                    coordinator_tx,
                    memory_graph_tx,
                    mcp_client_tx,
                    project_sync_tx,
                    project_analyzer_tx,
                    project_query_tx,
                    llm_tx,
                    progress_tx,
                    embedder: Some(embedder),
                    data_dir,
                    project_root,
                    system_tx: system_tx.clone(),
                };

                self.ctx = Some(ctx.clone());
                self.init_reply = Some(reply_to);

                info!("SystemActor: received Initialize, starting phase chain");
                // Start the phase chain
                self.start_phase_chain().await;
            }

            SystemMessage::PhaseEvent => {
                self.handle_phase_event().await;
            }

            SystemMessage::GetStatus { reply_to } => {
                let status = serde_json::json!({
                    "status": self.state.to_string(),
                    "uptime_seconds": self.uptime_seconds(),
                    "version": env!("CARGO_PKG_VERSION"),
                    "initializing": self.current_phase.is_some(),
                });
                let _ = reply_to.send(status);
            }

            SystemMessage::Shutdown { reply_to } => {
                info!("SystemActor: initiating graceful shutdown");
                self.state = SystemState::ShuttingDown;
                let _ = reply_to.send(Ok(()));
            }

            SystemMessage::GetConfig { key, reply_to } => {
                let value = self.config.get(&key).cloned();
                let _ = reply_to.send(value);
            }
        }
    }
}
