// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Startup phase abstraction — modular, composable initialization phases.
//!
//! Each phase is a self-contained struct implementing `StartupPhase`.
//! Phases can be composed via `PhaseGroup` for parallel execution.
//! The SystemActor holds the current phase and delegates messages to it,
//! rather than blocking its mailbox in a monolithic `run_initialize()`.
//!
//! Phase chain:
//!   GraphInitPhase (serial)
//!     → ParallelInitPhase (fires 5 inits concurrently)
//!     → ParallelConnectPhase (fires 4 connects concurrently)
//!     → RegisterToolsPhase (serial — needs MCP connected)
//!     → Done → publish 100%, set Ready

use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::actors::{
    CoordinatorMessage, LlmMessage, McpClientMessage, MemoryGraphMessage,
    ProgressMessage, ProgressStatus, ProgressUpdate, ProjectAnalyzerMessage, ProjectQueryMessage,
    ProjectSyncMessage, SystemMessage,
};
use spire_core::models::embedding::Embedder;
use spire_core::models::memory_graph::{
    AttrNode, RelationshipInput, RelationshipType, StreamOp, StreamOpResult, TransactionRequest,
};
use spire_core::build_types::Platform as SpirePlatform;
use std::collections::HashMap;

// ============================================================================
// PhaseContext — shared resources available to all phases
// ============================================================================

/// Build an envelope `AttrNode` for a dynamically-typed node stored through a
/// transaction stream (bootstrap phases upsert config-driven node kinds).
fn sp_attr_unknown(
    node_type: &str,
    subtype: Option<String>,
    name: String,
    description: Option<String>,
    properties: HashMap<String, serde_json::Value>,
) -> AttrNode {
    let now = chrono::Utc::now();
    AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: node_type.to_string(),
        subtype,
        name,
        description,
        properties,
        embedding_id: None,
        created_at: now,
        updated_at: now,
        version: 1,
    }
}

/// Shared context passed to every phase during construction and execution.
#[derive(Clone)]
pub struct PhaseContext {
    pub coordinator_tx: mpsc::Sender<CoordinatorMessage>,
    pub memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    pub mcp_client_tx: mpsc::Sender<McpClientMessage>,
    pub project_sync_tx: mpsc::Sender<ProjectSyncMessage>,
    pub project_analyzer_tx: mpsc::Sender<ProjectAnalyzerMessage>,
    pub project_query_tx: mpsc::Sender<ProjectQueryMessage>,
    pub llm_tx: mpsc::Sender<LlmMessage>,
    pub progress_tx: mpsc::Sender<ProgressMessage>,
    pub embedder: Option<Arc<dyn Embedder>>,
    pub data_dir: PathBuf,
    pub project_root: PathBuf,
    /// Sender to the SystemActor's mailbox — phases send completion messages here.
    pub system_tx: mpsc::Sender<SystemMessage>,
}

impl PhaseContext {
    /// Publish a progress update.
    pub async fn publish_progress(&self, phase: &str, message: &str, percent: f64) {
        let update = ProgressUpdate {
            task_id: "system.startup".to_string(),
            message: message.to_string(),
            percent,
            status: ProgressStatus::Running,
            metadata: Some(serde_json::json!({ "phase": phase })),
        };
        let _ = self
            .progress_tx
            .send(ProgressMessage::Publish { update })
            .await;
    }
}

// ============================================================================
// StartupPhase trait
// ============================================================================

/// Result of handling a message in a phase.
pub enum PhaseResult {
    /// Phase is still in progress — keep it as the current phase.
    InProgress,
    /// Phase has completed — the system should advance to the next phase.
    Complete,
    /// Phase encountered a fatal error.
    Failed(String),
}

/// A single initialization phase.
#[async_trait]
pub trait StartupPhase: Send {
    /// Start the phase. Called when this phase becomes active.
    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult;

    /// Handle a message directed at this phase.
    /// Returns whether the phase is complete.
    async fn handle_message(&mut self, msg: SystemMessage, ctx: &PhaseContext) -> PhaseResult;

    /// Name for logging/progress reporting.
    fn name(&self) -> &'static str;
}

// ============================================================================
// PhaseGroup — runs multiple sub-phases concurrently
// ============================================================================

/// A phase that runs multiple sub-phases concurrently and completes when all
/// sub-phases have reported completion.
pub struct PhaseGroup {
    name: &'static str,
    sub_phases: Vec<Box<dyn StartupPhase>>,
    /// Names of sub-phases that have completed.
    completed: HashSet<String>,
    /// Whether start() has been called.
    started: bool,
}

impl PhaseGroup {
    pub fn new(name: &'static str, sub_phases: Vec<Box<dyn StartupPhase>>) -> Self {
        Self {
            name,
            sub_phases,
            completed: HashSet::new(),
            started: false,
        }
    }
}

#[async_trait]
impl StartupPhase for PhaseGroup {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        self.started = true;
        self.completed.clear();

        let sub_names: Vec<&str> = self.sub_phases.iter().map(|p| p.name()).collect();
        info!(
            "PhaseGroup[{}]: starting {} sub-phases: {:?}",
            self.name,
            self.sub_phases.len(),
            sub_names
        );

        // Start all sub-phases and collect results.
        // If any sub-phase completes immediately (synchronous), mark it done.
        let self_name = self.name().to_string();
        for phase in self.sub_phases.iter_mut() {
            let result = phase.start(ctx).await;
            match result {
                PhaseResult::Complete => {
                    self.completed.insert(phase.name().to_string());
                }
                PhaseResult::Failed(e) => {
                    warn!(
                        "PhaseGroup[{}]: sub-phase '{}' failed: {}",
                        self_name,
                        phase.name(),
                        e
                    );
                    self.completed.insert(phase.name().to_string());
                }
                PhaseResult::InProgress => {
                    // Will complete via handle_message
                }
            }
        }

        if self.completed.len() == self.sub_phases.len() {
            PhaseResult::Complete
        } else {
            PhaseResult::InProgress
        }
    }

    async fn handle_message(&mut self, _msg: SystemMessage, ctx: &PhaseContext) -> PhaseResult {
        let total = self.sub_phases.len();
        let before = self.completed.len();

        // Try each sub-phase that hasn't completed yet
        let _self_name = self.name().to_string();
        for phase in self.sub_phases.iter_mut() {
            let name = phase.name().to_string();
            if self.completed.contains(&name) {
                continue;
            }
            let result = phase.handle_message(SystemMessage::PhaseEvent, ctx).await;
            match result {
                PhaseResult::Complete => {
                    info!(
                        "PhaseGroup[{}]: sub-phase '{}' completed ({}/{})",
                        self.name,
                        name,
                        self.completed.len() + 1,
                        total
                    );
                    self.completed.insert(name);
                }
                PhaseResult::Failed(e) => {
                    warn!(
                        "PhaseGroup[{}]: sub-phase '{}' failed: {}",
                        self.name, name, e
                    );
                    self.completed.insert(name);
                }
                PhaseResult::InProgress => {
                    // Still waiting
                }
            }
        }

        let completed = self.completed.len();
        if completed == total {
            info!(
                "PhaseGroup[{}]: all {} sub-phases complete",
                self.name, total
            );
            PhaseResult::Complete
        } else {
            if completed != before {
                debug!(
                    "PhaseGroup[{}]: progress {}/{} sub-phases complete",
                    self.name, completed, total
                );
            }
            PhaseResult::InProgress
        }
    }
}

// ============================================================================
// Concrete Phases
// ============================================================================

// ── GraphInitPhase ──────────────────────────────────────────────────────────

pub struct GraphInitPhase;

#[async_trait]
impl StartupPhase for GraphInitPhase {
    fn name(&self) -> &'static str {
        "graph_init"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("initializing_graph", "Initializing graph database", 5.0)
            .await;
        info!("SystemActor: initializing graph database");

        let (tx, rx) = oneshot::channel();
        if ctx
            .memory_graph_tx
            .send(MemoryGraphMessage::Initialize {
                data_dir: ctx.data_dir.clone(),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return PhaseResult::Failed("Failed to send MemoryGraph::Initialize".to_string());
        }

        match rx.await {
            Ok(Ok(())) => {
                info!("SystemActor: graph database initialized");
                PhaseResult::Complete
            }
            Ok(Err(e)) => PhaseResult::Failed(format!("Graph init failed: {}", e)),
            Err(e) => PhaseResult::Failed(format!("Graph init response error: {}", e)),
        }
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress // Should not receive messages — completes synchronously
    }
}

// ── EmbedderInitPhase ───────────────────────────────────────────────────────

pub struct EmbedderInitPhase;

#[async_trait]
impl StartupPhase for EmbedderInitPhase {
    fn name(&self) -> &'static str {
        "embedder_init"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("initializing_embedder", "Loading embedding model", 15.0)
            .await;
        info!("SystemActor: initializing embedding model");

        if let Some(ref embedder) = ctx.embedder {
            let (tx, rx) = oneshot::channel();
            if ctx
                .memory_graph_tx
                .send(MemoryGraphMessage::InitializeEmbedder {
                    model_path: None,
                    embedder: Some(embedder.clone()),
                    reply_to: tx,
                })
                .await
                .is_err()
            {
                return PhaseResult::Failed("Failed to send InitializeEmbedder".to_string());
            }

            match rx.await {
                Ok(Ok(())) => {
                    info!("SystemActor: embedding model initialized");
                    PhaseResult::Complete
                }
                Ok(Err(e)) => PhaseResult::Failed(format!("Embedder init failed: {}", e)),
                Err(e) => PhaseResult::Failed(format!("Embedder init response error: {}", e)),
            }
        } else {
            info!("SystemActor: no embedder provided, skipping");
            PhaseResult::Complete
        }
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── McpBootstrapPhase ───────────────────────────────────────────────────────

pub struct McpBootstrapPhase {
    mcp_config_path: Option<PathBuf>,
}

impl McpBootstrapPhase {
    pub fn new(mcp_config_path: Option<PathBuf>) -> Self {
        Self { mcp_config_path }
    }
}

#[async_trait]
impl StartupPhase for McpBootstrapPhase {
    fn name(&self) -> &'static str {
        "mcp_bootstrap"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("loading_mcp_config", "Loading MCP configuration", 30.0)
            .await;
        info!("SystemActor: loading MCP configuration");

        // Step 1: Bootstrap MCP config from JSON file into graph
        // Resolve the config path: use the explicitly provided path, or fall back
        // to auto-discovering config/mcp-config.json relative to the project root.
        let resolved_config_path = self.mcp_config_path.clone().or_else(|| {
            let default_path = ctx.project_root.join("config").join("mcp-config.json");
            if default_path.exists() {
                info!(
                    "SystemActor: auto-discovered MCP config at: {}",
                    default_path.display()
                );
                Some(default_path)
            } else {
                None
            }
        });

        if let Some(ref config_path) = resolved_config_path {
            if config_path.exists() {
                info!(
                    "SystemActor: bootstrapping MCP config from: {}",
                    config_path.display()
                );
                let (tx, rx) = oneshot::channel();
                if ctx
                    .memory_graph_tx
                    .send(MemoryGraphMessage::BootstrapMcpConfig {
                        config_path: config_path.clone(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return PhaseResult::Failed("Failed to send BootstrapMcpConfig".to_string());
                }
                match rx.await {
                    Ok(Ok(())) => info!("SystemActor: MCP config bootstrapped into graph"),
                    Ok(Err(e)) => warn!("SystemActor: MCP config bootstrap failed: {}", e),
                    Err(e) => warn!("SystemActor: MCP config bootstrap response error: {}", e),
                }
            } else {
                info!(
                    "SystemActor: MCP config file not found at: {}",
                    config_path.display()
                );
            }
        } else {
            info!(
                "SystemActor: no MCP config path provided or auto-discovered, skipping bootstrap"
            );
        }

        // Step 2: Load MCP config from graph into client
        let (tx, rx) = oneshot::channel();
        if ctx
            .memory_graph_tx
            .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
            .await
            .is_err()
        {
            return PhaseResult::Failed("Failed to send GetMcpConfig".to_string());
        }

        match rx.await {
            Ok(Ok(servers)) => {
                if !servers.is_empty() {
                    info!(
                        "SystemActor: loading {} MCP server configs from graph into client",
                        servers.len()
                    );
                    let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers
                        .into_iter()
                        .filter_map(|entry| {
                            let transport = if let Some(url) = entry.url {
                                spire_core::mcp::client::TransportConfig::Http {
                                    url,
                                    headers: entry.headers.unwrap_or_default(),
                                }
                            } else if let Some(command) = entry.command {
                                spire_core::mcp::client::TransportConfig::Stdio {
                                    command,
                                    args: entry.args,
                                    env: entry.env.unwrap_or_default(),
                                }
                            } else {
                                warn!("SystemActor: MCP server '{}' has no transport config, skipping", entry.name);
                                return None;
                            };
                            Some(spire_core::mcp::client::McpServerConfig {
                                name: entry.name,
                                transport,
                                autostart: entry.autostart,
                            build_type: None,
                            })
                        })
                        .collect();

                    let (tx, rx) = oneshot::channel();
                    if ctx
                        .mcp_client_tx
                        .send(McpClientMessage::LoadConfigFromGraph {
                            servers: configs,
                            reply_to: tx,
                        })
                        .await
                        .is_err()
                    {
                        return PhaseResult::Failed(
                            "Failed to send LoadConfigFromGraph".to_string(),
                        );
                    }
                    let _ = rx.await;
                } else {
                    info!("SystemActor: no MCP server configs in graph, skipping client load");
                }
            }
            Ok(Err(e)) => warn!("SystemActor: failed to fetch MCP config from graph: {}", e),
            Err(e) => warn!("SystemActor: MCP config fetch response error: {}", e),
        }

        PhaseResult::Complete
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── PlatformBootstrapPhase ──────────────────────────────────────────────────

/// Seed cross-compilation platform definitions from the global YAML dir
/// (`~/.spire/platforms/*.yaml` or `$SPIRE_PLATFORM_DIR`) into the graph on
/// startup. The graph is the canonical store — the YAML files are only the
/// seed, mirroring how MCP config is bootstrapped.
pub struct PlatformBootstrapPhase;

#[async_trait]
impl StartupPhase for PlatformBootstrapPhase {
    fn name(&self) -> &'static str {
        "platform_bootstrap"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress(
            "loading_platforms",
            "Loading cross-compilation platforms",
            32.0,
        )
        .await;
        info!("SystemActor: bootstrapping platform definitions into graph");

        let config_dir = SpirePlatform::default_platform_dir();
        let platforms = SpirePlatform::load_directory(&config_dir).unwrap_or_default();
        let platform_json: Vec<serde_json::Value> = platforms
            .iter()
            .map(crate::actors::platform_codec::platform_to_registry_json)
            .collect();

        let (tx, rx) = oneshot::channel();
        if ctx
            .memory_graph_tx
            .send(MemoryGraphMessage::BootstrapPlatforms {
                platforms: platform_json,
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return PhaseResult::Failed(
                "Failed to send BootstrapPlatforms".to_string(),
            );
        }

        match rx.await {
            Ok(Ok(())) => {
                info!("SystemActor: platform definitions bootstrapped into graph");
                PhaseResult::Complete
            }
            Ok(Err(e)) => {
                warn!("SystemActor: platform bootstrap failed: {}", e);
                // Non-fatal — a missing/empty seed dir just yields an empty
                // platform list (and clears any stale nodes).
                PhaseResult::Complete
            }
            Err(e) => {
                warn!("SystemActor: platform bootstrap response error: {}", e);
                PhaseResult::Complete
            }
        }
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── ProjectSyncInitPhase ────────────────────────────────────────────────────

pub struct ProjectSyncInitPhase;

#[async_trait]
impl StartupPhase for ProjectSyncInitPhase {
    fn name(&self) -> &'static str {
        "project_sync_init"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        info!("SystemActor: initializing project sync actor");
        let (tx, rx) = oneshot::channel();
        if ctx
            .project_sync_tx
            .send(ProjectSyncMessage::Initialize {
                memory_graph_tx: ctx.memory_graph_tx.clone(),
                embedder: ctx
                    .embedder
                    .clone()
                    .unwrap_or_else(|| Arc::new(spire_core::embedder::NoopEmbedder)),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return PhaseResult::Failed("Failed to send ProjectSync::Initialize".to_string());
        }

        match rx.await {
            Ok(Ok(())) => {
                info!("SystemActor: project sync actor initialized");
                PhaseResult::Complete
            }
            Ok(Err(e)) => PhaseResult::Failed(format!("ProjectSync init failed: {}", e)),
            Err(e) => PhaseResult::Failed(format!("ProjectSync init response error: {}", e)),
        }
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── ProjectAnalyzerInitPhase ────────────────────────────────────────────────

pub struct ProjectAnalyzerInitPhase;

#[async_trait]
impl StartupPhase for ProjectAnalyzerInitPhase {
    fn name(&self) -> &'static str {
        "project_analyzer_init"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        info!("SystemActor: initializing project analyzer actor");
        let (tx, rx) = oneshot::channel();
        if ctx
            .project_analyzer_tx
            .send(ProjectAnalyzerMessage::Initialize {
                mcp_client_tx: ctx.mcp_client_tx.clone(),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return PhaseResult::Failed("Failed to send ProjectAnalyzer::Initialize".to_string());
        }

        match rx.await {
            Ok(Ok(())) => {
                info!("SystemActor: project analyzer actor initialized");
                PhaseResult::Complete
            }
            Ok(Err(e)) => PhaseResult::Failed(format!("ProjectAnalyzer init failed: {}", e)),
            Err(e) => PhaseResult::Failed(format!("ProjectAnalyzer init response error: {}", e)),
        }
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── ProjectQueryInitPhase ───────────────────────────────────────────────────

pub struct ProjectQueryInitPhase;

#[async_trait]
impl StartupPhase for ProjectQueryInitPhase {
    fn name(&self) -> &'static str {
        "project_query_init"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        info!("SystemActor: initializing project query actor");
        let (tx, rx) = oneshot::channel();
        if ctx
            .project_query_tx
            .send(ProjectQueryMessage::Initialize {
                memory_graph_tx: ctx.memory_graph_tx.clone(),
                project_root: ctx.project_root.clone(),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return PhaseResult::Failed("Failed to send ProjectQuery::Initialize".to_string());
        }

        match rx.await {
            Ok(Ok(())) => {
                info!("SystemActor: project query actor initialized");
                PhaseResult::Complete
            }
            Ok(Err(e)) => PhaseResult::Failed(format!("ProjectQuery init failed: {}", e)),
            Err(e) => PhaseResult::Failed(format!("ProjectQuery init response error: {}", e)),
        }
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── McpConnectPhase ─────────────────────────────────────────────────────────

pub struct McpConnectPhase;

#[async_trait]
impl StartupPhase for McpConnectPhase {
    fn name(&self) -> &'static str {
        "mcp_connect"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("connecting_mcp", "Connecting to MCP servers", 50.0)
            .await;
        info!("SystemActor: connecting to MCP servers (fire-and-forget)");

        // Fire-and-forget: send ConnectAll but don't wait for the reply.
        // MCP server connections happen in the background. If a subprocess
        // hangs during initialization, it won't block startup.
        let (tx, _rx) = oneshot::channel();
        if ctx
            .mcp_client_tx
            .send(McpClientMessage::ConnectAll { reply_to: tx })
            .await
            .is_err()
        {
            warn!("SystemActor: failed to send ConnectAll (non-fatal)");
        }

        // Don't await the reply — continue startup immediately.
        // The MCP client actor will connect servers in the background.
        info!("SystemActor: MCP connect dispatched (background)");
        PhaseResult::Complete
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── ProjectSyncPhase ────────────────────────────────────────────────────────
//
// This phase spawns a transient actor (tokio task) to do the blocking
// filesystem scan and graph operations. Completion is signaled via a
// oneshot channel — no polling needed.

pub struct ProjectSyncPhase {
    /// Receiver for completion signal from the spawned task.
    completion_rx: Option<oneshot::Receiver<()>>,
}

impl ProjectSyncPhase {
    pub fn new() -> Self {
        Self {
            completion_rx: None,
        }
    }
}

impl Default for ProjectSyncPhase {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StartupPhase for ProjectSyncPhase {
    fn name(&self) -> &'static str {
        "project_sync"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("syncing_project", "Syncing project structure", 65.0)
            .await;
        info!("SystemActor: dispatching project sync to transient actor");

        let (tx, rx) = oneshot::channel();
        self.completion_rx = Some(rx);

        let memory_graph_tx = ctx.memory_graph_tx.clone();
        let project_sync_tx = ctx.project_sync_tx.clone();
        let project_root = ctx.project_root.clone();
        let system_tx = ctx.system_tx.clone();

        tokio::spawn(async move {
            // Check if a Project node already exists (warm start)
            let has_existing_project = {
                let (qtx, qrx) = oneshot::channel();
                if memory_graph_tx
                    .send(MemoryGraphMessage::QueryAttrNodes {
                        node_type: Some("Project".to_string()),
                        subtype: None,
                        name: None,
                        limit: Some(1),
                        reply_to: qtx,
                    })
                    .await
                    .is_err()
                {
                    warn!("ProjectSyncPhase: failed to send QueryAttrNodes");
                    let _ = tx.send(());
                    return;
                }
                match qrx.await {
                    Ok(nodes) => nodes.map(|n| !n.is_empty()).unwrap_or(false),
                    Err(_) => false,
                }
            };

            if has_existing_project {
                info!("ProjectSyncPhase: project node exists, performing startup sync");
                let (stx, srx) = oneshot::channel();
                if project_sync_tx
                    .send(ProjectSyncMessage::StartupSync {
                        project_root: project_root.clone(),
                        reply_to: stx,
                    })
                    .await
                    .is_err()
                {
                    warn!("ProjectSyncPhase: failed to send StartupSync");
                } else {
                    match srx.await {
                        Ok(Ok(result)) => {
                            info!("ProjectSyncPhase: startup sync complete: {:?}", result)
                        }
                        Ok(Err(e)) => warn!("ProjectSyncPhase: startup sync had issues: {}", e),
                        Err(e) => warn!("ProjectSyncPhase: startup sync response error: {}", e),
                    }
                }
            } else {
                info!("ProjectSyncPhase: no project node found, performing full bootstrap");
                let (stx, srx) = oneshot::channel();
                if project_sync_tx
                    .send(ProjectSyncMessage::Bootstrap {
                        project_root: project_root.clone(),
                        reply_to: stx,
                    })
                    .await
                    .is_err()
                {
                    warn!("ProjectSyncPhase: failed to send Bootstrap");
                } else {
                    match srx.await {
                        Ok(Ok(result)) => {
                            info!("ProjectSyncPhase: bootstrap complete: {:?}", result)
                        }
                        Ok(Err(e)) => warn!("ProjectSyncPhase: bootstrap had issues: {}", e),
                        Err(e) => warn!("ProjectSyncPhase: bootstrap response error: {}", e),
                    }
                }
            }

            // Write a snapshot after sync
            let (stx, srx) = oneshot::channel();
            if memory_graph_tx
                .send(MemoryGraphMessage::Sync { reply_to: stx })
                .await
                .is_ok()
            {
                match srx.await {
                    Ok(Ok(())) => info!("ProjectSyncPhase: snapshot written after project sync"),
                    Ok(Err(e)) => warn!("ProjectSyncPhase: snapshot write failed: {}", e),
                    Err(e) => warn!("ProjectSyncPhase: snapshot response error: {}", e),
                }
            }

            info!("ProjectSyncPhase: project sync complete");
            let _ = tx.send(());
            let _ = system_tx.send(SystemMessage::PhaseEvent).await;
        });

        PhaseResult::InProgress
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        if let Some(rx) = self.completion_rx.as_mut() {
            if rx.try_recv().is_ok() {
                self.completion_rx = None;
                return PhaseResult::Complete;
            }
        }
        PhaseResult::InProgress
    }
}

// ── ProjectAnalysisPhase ────────────────────────────────────────────────────
//
// This phase spawns a transient actor (tokio task) to do the blocking
// project analysis. Completion is signaled via a oneshot channel.

pub struct ProjectAnalysisPhase {
    /// Receiver for completion signal from the spawned task.
    completion_rx: Option<oneshot::Receiver<()>>,
}

impl ProjectAnalysisPhase {
    pub fn new() -> Self {
        Self {
            completion_rx: None,
        }
    }
}

impl Default for ProjectAnalysisPhase {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StartupPhase for ProjectAnalysisPhase {
    fn name(&self) -> &'static str {
        "project_analysis"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("analyzing_project", "Analyzing project code", 80.0)
            .await;
        info!("SystemActor: dispatching project analysis to transient actor");

        let (tx, rx) = oneshot::channel();
        self.completion_rx = Some(rx);

        let project_analyzer_tx = ctx.project_analyzer_tx.clone();
        let project_root = ctx.project_root.clone();
        let system_tx = ctx.system_tx.clone();
        let memory_graph_tx = ctx.memory_graph_tx.clone();

        tokio::spawn(async move {
            let (atx, arx) = oneshot::channel();
            if project_analyzer_tx
                .send(ProjectAnalyzerMessage::Analyze {
                    project_root: project_root.clone(),
                    reply_to: atx,
                })
                .await
                .is_err()
            {
                warn!("ProjectAnalysisPhase: failed to send Analyze");
            } else {
                match arx.await {
                    Ok(Ok(analysis)) => {
                        info!(
                            "ProjectAnalysisPhase: analysis complete: {} files, {} dirs, {} build systems",
                            analysis.total_files,
                            analysis.total_dirs,
                            analysis.build_systems.len(),
                        );

                        // Store the analysis result in the graph
                        if let Err(e) = store_project_analysis(&memory_graph_tx, &analysis).await {
                            warn!(
                                "ProjectAnalysisPhase: failed to store analysis in graph: {}",
                                e
                            );
                        } else {
                            info!("ProjectAnalysisPhase: analysis stored in graph");
                        }
                    }
                    Ok(Err(e)) => {
                        warn!("ProjectAnalysisPhase: analysis failed: {}", e);
                    }
                    Err(e) => {
                        warn!("ProjectAnalysisPhase: analysis response error: {}", e);
                    }
                }
            }

            let _ = tx.send(());
            let _ = system_tx.send(SystemMessage::PhaseEvent).await;
        });

        PhaseResult::InProgress
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        if let Some(rx) = self.completion_rx.as_mut() {
            if rx.try_recv().is_ok() {
                self.completion_rx = None;
                return PhaseResult::Complete;
            }
        }
        PhaseResult::InProgress
    }
}

// ── LlmConfigPhase ──────────────────────────────────────────────────────────
//
// This phase spawns a transient actor (tokio task) to load LLM config
// from the graph. Completion is signaled via a oneshot channel.

pub struct LlmConfigPhase {
    /// Receiver for completion signal from the spawned task.
    completion_rx: Option<oneshot::Receiver<()>>,
}

impl LlmConfigPhase {
    pub fn new() -> Self {
        Self {
            completion_rx: None,
        }
    }
}

impl Default for LlmConfigPhase {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StartupPhase for LlmConfigPhase {
    fn name(&self) -> &'static str {
        "llm_config"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("loading_llm_config", "Loading LLM configuration", 90.0)
            .await;
        info!("SystemActor: dispatching LLM config load to transient actor");

        let (tx, rx) = oneshot::channel();
        self.completion_rx = Some(rx);

        let _memory_graph_tx = ctx.memory_graph_tx.clone();
        let llm_tx = ctx.llm_tx.clone();
        let system_tx = ctx.system_tx.clone();

        tokio::spawn(async move {
            // Load global LLM config from ~/.spire/llm-config.json (shared across all projects).
            let llm_config = spire_core::config::load_global_llm_config();
            if !llm_config.api_key.is_empty() {
                info!(
                    "LlmConfigPhase: loading persisted DeepSeek config: model={}",
                    llm_config.model
                );
                let (ltx, lrx) = oneshot::channel();
                if llm_tx
                    .send(LlmMessage::UpdateConfig {
                        config: llm_config,
                        reply_to: ltx,
                    })
                    .await
                    .is_ok()
                {
                    let _ = lrx.await;
                }
            } else {
                info!("LlmConfigPhase: no persisted DeepSeek config found, using defaults");
            }
            info!("LlmConfigPhase: LLM config loaded");
            let _ = tx.send(());
            let _ = system_tx.send(SystemMessage::PhaseEvent).await;
        });

        PhaseResult::InProgress
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        if let Some(rx) = self.completion_rx.as_mut() {
            if rx.try_recv().is_ok() {
                self.completion_rx = None;
                return PhaseResult::Complete;
            }
        }
        PhaseResult::InProgress
    }
}

// ── RegisterToolsPhase ──────────────────────────────────────────────────────

pub struct RegisterToolsPhase;

#[async_trait]
impl StartupPhase for RegisterToolsPhase {
    fn name(&self) -> &'static str {
        "register_tools"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress("registering_tools", "Registering tools", 95.0)
            .await;
        info!("SystemActor: registering internal tools");

        // Collect VS Code extension tools
        let vsc_tools: Vec<rust_mcp_sdk::schema::Tool> = crate::actors::vscode_tool_definitions()
            .into_iter()
            .filter_map(|def| {
                let required = def
                    .input_schema
                    .get("required")
                    .cloned()
                    .unwrap_or(serde_json::json!([]));
                let tool_json = serde_json::json!({
                    "name": def.name,
                    "description": def.description,
                    "inputSchema": {
                        "type": "object",
                        "properties": def.input_schema.get("properties"),
                        "required": required,
                    }
                });
                serde_json::from_value(tool_json).ok()
            })
            .collect();

        // Collect project query tools
        let project_tools: Vec<rust_mcp_sdk::schema::Tool> =
            crate::subsystems::project::project_query::ProjectQueryActor::tool_definitions()
                .into_iter()
                .filter_map(|def| {
                    let required = def
                        .input_schema
                        .get("required")
                        .cloned()
                        .unwrap_or(serde_json::json!([]));
                    let tool_json = serde_json::json!({
                        "name": def.name,
                        "description": def.description,
                        "inputSchema": {
                            "type": "object",
                            "properties": def.input_schema.get("properties"),
                            "required": required,
                        }
                    });
                    serde_json::from_value(tool_json).ok()
                })
                .collect();

        // Collect project build tools
        let build_tools: Vec<rust_mcp_sdk::schema::Tool> =
            crate::subsystems::project::project_build::ProjectBuildActor::tool_definitions()
                .into_iter()
                .filter_map(|def| {
                    let required = def
                        .input_schema
                        .get("required")
                        .cloned()
                        .unwrap_or(serde_json::json!([]));
                    let tool_json = serde_json::json!({
                        "name": def.name,
                        "description": def.description,
                        "inputSchema": {
                            "type": "object",
                            "properties": def.input_schema.get("properties"),
                            "required": required,
                        }
                    });
                    serde_json::from_value(tool_json).ok()
                })
                .collect();

        let mut internal_tools = vsc_tools;
        internal_tools.extend(project_tools);
        internal_tools.extend(build_tools);

        let (tx, rx) = oneshot::channel();
        if ctx
            .mcp_client_tx
            .send(McpClientMessage::SetInternalTools {
                tools: internal_tools,
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return PhaseResult::Failed("Failed to send SetInternalTools".to_string());
        }
        let _ = rx.await;

        info!("SystemActor: internal tools registered");
        PhaseResult::Complete
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ============================================================================
// PhaseChain — runs phases sequentially
// ============================================================================

/// A phase that runs sub-phases sequentially. Each sub-phase must complete
/// before the next one starts. This is the top-level orchestrator for the
/// startup sequence.
pub struct PhaseChain {
    name: &'static str,
    sub_phases: Vec<Box<dyn StartupPhase>>,
    /// Index of the currently active sub-phase.
    current_index: usize,
    /// Whether start() has been called.
    started: bool,
}

impl PhaseChain {
    pub fn new(name: &'static str, sub_phases: Vec<Box<dyn StartupPhase>>) -> Self {
        Self {
            name,
            sub_phases,
            current_index: 0,
            started: false,
        }
    }
}

#[async_trait]
impl StartupPhase for PhaseChain {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        self.started = true;
        self.current_index = 0;

        if self.sub_phases.is_empty() {
            info!(
                "PhaseChain[{}]: no sub-phases, complete immediately",
                self.name
            );
            return PhaseResult::Complete;
        }

        let total = self.sub_phases.len();
        info!(
            "PhaseChain[{}]: starting phase chain with {} phases",
            self.name, total
        );

        // Start phases sequentially, advancing as long as each completes synchronously.
        // If a phase returns InProgress, we stop and wait for PhaseEvent.
        while self.current_index < total {
            let phase_name = self.sub_phases[self.current_index].name();
            info!(
                "PhaseChain[{}]: starting phase '{}' ({}/{})",
                self.name,
                phase_name,
                self.current_index + 1,
                total
            );
            let result = self.sub_phases[self.current_index].start(ctx).await;
            match result {
                PhaseResult::Complete => {
                    info!(
                        "PhaseChain[{}]: phase '{}' completed synchronously ({}/{})",
                        self.name,
                        phase_name,
                        self.current_index + 1,
                        total
                    );
                    self.current_index += 1;
                }
                PhaseResult::Failed(e) => return PhaseResult::Failed(e),
                PhaseResult::InProgress => {
                    info!(
                        "PhaseChain[{}]: phase '{}' in progress ({}/{})",
                        self.name,
                        phase_name,
                        self.current_index + 1,
                        total
                    );
                    return PhaseResult::InProgress;
                }
            }
        }

        info!("PhaseChain[{}]: all {} phases complete", self.name, total);
        PhaseResult::Complete
    }

    async fn handle_message(&mut self, msg: SystemMessage, ctx: &PhaseContext) -> PhaseResult {
        if !self.started || self.current_index >= self.sub_phases.len() {
            return PhaseResult::Complete;
        }

        let total = self.sub_phases.len();

        // Forward the message to the current sub-phase
        let current_name = self.sub_phases[self.current_index].name();
        debug!(
            "PhaseChain[{}]: polling phase '{}' ({}/{})",
            self.name,
            current_name,
            self.current_index + 1,
            total
        );

        let result = self.sub_phases[self.current_index]
            .handle_message(msg, ctx)
            .await;
        match result {
            PhaseResult::Complete => {
                info!(
                    "PhaseChain[{}]: phase '{}' completed ({}/{})",
                    self.name,
                    current_name,
                    self.current_index + 1,
                    total
                );
                // Current phase completed — advance to next
                self.current_index += 1;

                // Start subsequent phases as long as they complete synchronously
                while self.current_index < total {
                    let next_name = self.sub_phases[self.current_index].name();
                    info!(
                        "PhaseChain[{}]: advancing to phase '{}' ({}/{})",
                        self.name,
                        next_name,
                        self.current_index + 1,
                        total
                    );
                    let result = self.sub_phases[self.current_index].start(ctx).await;
                    match result {
                        PhaseResult::Complete => {
                            info!(
                                "PhaseChain[{}]: phase '{}' completed synchronously ({}/{})",
                                self.name,
                                next_name,
                                self.current_index + 1,
                                total
                            );
                            self.current_index += 1;
                        }
                        PhaseResult::Failed(e) => return PhaseResult::Failed(e),
                        PhaseResult::InProgress => {
                            info!(
                                "PhaseChain[{}]: phase '{}' in progress ({}/{})",
                                self.name,
                                next_name,
                                self.current_index + 1,
                                total
                            );
                            return PhaseResult::InProgress;
                        }
                    }
                }

                info!("PhaseChain[{}]: all {} phases complete", self.name, total);
                PhaseResult::Complete
            }
            PhaseResult::Failed(e) => PhaseResult::Failed(e),
            PhaseResult::InProgress => PhaseResult::InProgress,
        }
    }
}

// ============================================================================
// Helper: store project analysis in the graph
// ============================================================================

/// Store a `ProjectAnalysis` result in the graph database.
/// Creates a Project node with metadata and BuildSystem nodes for each
/// detected build system, linked via `HAS_BUILD_SYSTEM` relationships.
async fn store_project_analysis(
    memory_graph_tx: &mpsc::Sender<MemoryGraphMessage>,
    analysis: &crate::subsystems::project::project_analyzer::ProjectAnalysis,
) -> anyhow::Result<()> {
    // Open a transaction stream
    let (init_tx, init_rx) = oneshot::channel();
    memory_graph_tx
        .send(MemoryGraphMessage::OpenTransactionStream { reply_to: init_tx })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send OpenTransactionStream: {}", e))?;

    let stream_tx = init_rx
        .await
        .map_err(|e| anyhow::anyhow!("OpenTransactionStream response error: {}", e))?;

    // Helper to send an op and await result
    let send_op = |op: StreamOp| {
        let stream_tx = stream_tx.clone();
        async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            stream_tx
                .send(TransactionRequest {
                    operation: op,
                    reply_to: reply_tx,
                })
                .await
                .map_err(|e| anyhow::anyhow!("Stream send failed: {}", e))?;
            reply_rx
                .await
                .map_err(|e| anyhow::anyhow!("Stream reply error: {}", e))?
                .map_err(|e| anyhow::anyhow!("Stream op failed: {}", e))
        }
    };

    // 1. Create or update the Project node
    let project_props: HashMap<String, serde_json::Value> = [
        (
            "total_files".to_string(),
            serde_json::json!(analysis.total_files),
        ),
        (
            "total_dirs".to_string(),
            serde_json::json!(analysis.total_dirs),
        ),
        (
            "total_lines".to_string(),
            serde_json::json!(analysis.total_lines),
        ),
        (
            "languages".to_string(),
            serde_json::json!(analysis.languages),
        ),
        (
            "build_systems".to_string(),
            serde_json::json!(analysis
                .build_systems
                .iter()
                .map(|bs| bs.build_system.clone())
                .collect::<Vec<_>>()),
        ),
    ]
    .into();

    send_op(StreamOp::MergeNode(sp_attr_unknown(
            "Project",
            None,
            analysis.project_name.clone(),
            None,
            project_props,
        )))
    .await?;

    // 2. Create BuildSystem nodes for each detected build system
    for bs in &analysis.build_systems {
        let bs_props: HashMap<String, serde_json::Value> = [
            (
                "build_system".to_string(),
                serde_json::json!(bs.build_system),
            ),
            (
                "project_type".to_string(),
                serde_json::json!(bs.project_type),
            ),
            (
                "config_files".to_string(),
                serde_json::json!(bs.config_files),
            ),
        ]
        .into();

        send_op(StreamOp::MergeNode(sp_attr_unknown(
                "BuildSystem",
                Some(bs.build_system.clone()),
                bs.build_system.clone(),
                None,
                bs_props,
            )))
        .await?;

        // Link Project → BuildSystem
        let project_id = format!("Project:{}", analysis.project_name);
        let bs_id = format!("BuildSystem:{}", bs.build_system);

        send_op(StreamOp::MergeRelationship(RelationshipInput {
            edge_type: RelationshipType::Custom("HAS_BUILD_SYSTEM".to_string()),
            from_id: project_id,
            to_id: bs_id,
            properties: Some(HashMap::new()),
            weight: None,
        }))
        .await?;
    }

    // 3. Store project root and primary build system as config keys
    //    so the coordinator can read them when dispatching build intents.
    let primary_build_system = analysis
        .build_systems
        .first()
        .map(|bs| bs.build_system.clone())
        .unwrap_or_default();

    send_op(StreamOp::SetConfig {
        key: "project.root".to_string(),
        value: serde_json::Value::String(analysis.project_root.clone()),
    })
    .await?;

    send_op(StreamOp::SetConfig {
        key: "project.build_system".to_string(),
        value: serde_json::Value::String(primary_build_system),
    })
    .await?;

    // 4. Commit the transaction
    send_op(StreamOp::Commit).await?;

    info!(
        "store_project_analysis: stored project '{}' with {} build systems, set config keys",
        analysis.project_name,
        analysis.build_systems.len(),
    );

    Ok(())
}

// ── IntentsBootstrapInitPhase ───────────────────────────────────────────────

pub struct IntentsBootstrapInitPhase;

#[async_trait]
impl StartupPhase for IntentsBootstrapInitPhase {
    fn name(&self) -> &'static str {
        "intents_bootstrap_init"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        info!("SystemActor: initializing intents bootstrap actor");
        let (tx, _rx) = oneshot::channel();
        if ctx
            .mcp_client_tx
            .send(McpClientMessage::GetConnectedServersWithTools { reply_to: tx })
            .await
            .is_err()
        {
            // Not fatal — the bootstrap actor will just not have MCP tool info
            warn!("IntentsBootstrapInitPhase: failed to query connected servers (non-fatal)");
        }
        // We don't need to await here — the bootstrap phase will handle it
        PhaseResult::Complete
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        PhaseResult::InProgress
    }
}

// ── IntentsBootstrapPhase ───────────────────────────────────────────────────
//
// This phase spawns a transient actor (tokio task) to load intents from
// config/intents.json and MCP server tools, then store them in the graph.

pub struct IntentsBootstrapPhase {
    /// Receiver for completion signal from the spawned task.
    completion_rx: Option<oneshot::Receiver<()>>,
}

impl IntentsBootstrapPhase {
    pub fn new() -> Self {
        Self {
            completion_rx: None,
        }
    }
}

impl Default for IntentsBootstrapPhase {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StartupPhase for IntentsBootstrapPhase {
    fn name(&self) -> &'static str {
        "intents_bootstrap"
    }

    async fn start(&mut self, ctx: &PhaseContext) -> PhaseResult {
        ctx.publish_progress(
            "bootstrapping_intents",
            "Bootstrapping intents and tools",
            85.0,
        )
        .await;
        info!("SystemActor: dispatching intents bootstrap to transient actor");

        let (tx, rx) = oneshot::channel();
        self.completion_rx = Some(rx);

        let memory_graph_tx = ctx.memory_graph_tx.clone();
        let mcp_client_tx = ctx.mcp_client_tx.clone();
        let system_tx = ctx.system_tx.clone();
        let project_root = ctx.project_root.clone();

        tokio::spawn(async move {
            // Step 1: Bootstrap static intents from config/intents.json
            let config_path = project_root.join("config").join("intents.json");
            if config_path.exists() {
                info!(
                    "IntentsBootstrapPhase: loading intents from: {}",
                    config_path.display()
                );
                match bootstrap_intents_from_config(&memory_graph_tx, &config_path).await {
                    Ok(count) => info!(
                        "IntentsBootstrapPhase: stored {} intent nodes from config",
                        count
                    ),
                    Err(e) => warn!(
                        "IntentsBootstrapPhase: failed to bootstrap intents from config: {}",
                        e
                    ),
                }
            } else {
                info!("IntentsBootstrapPhase: no config/intents.json found, skipping static bootstrap");
            }

            // Step 2: Bootstrap strategy steps from config/strategy-steps.json
            let strategy_config_path = project_root.join("config").join("strategy-steps.json");
            if strategy_config_path.exists() {
                info!(
                    "IntentsBootstrapPhase: loading strategy steps from: {}",
                    strategy_config_path.display()
                );
                match bootstrap_strategy_steps(&memory_graph_tx, &strategy_config_path).await {
                    Ok(count) => info!(
                        "IntentsBootstrapPhase: stored {} strategy step + provider nodes",
                        count
                    ),
                    Err(e) => warn!(
                        "IntentsBootstrapPhase: failed to bootstrap strategy steps: {}",
                        e
                    ),
                }
            } else {
                info!("IntentsBootstrapPhase: no config/strategy-steps.json found, skipping strategy bootstrap");
            }

            // Step 3: Discover tools from connected MCP servers
            match discover_mcp_tools(&memory_graph_tx, &mcp_client_tx).await {
                Ok(count) => info!("IntentsBootstrapPhase: stored {} MCP tool nodes", count),
                Err(e) => warn!("IntentsBootstrapPhase: failed to discover MCP tools: {}", e),
            }

            // Write a snapshot after bootstrap
            let (stx, srx) = oneshot::channel();
            if memory_graph_tx
                .send(MemoryGraphMessage::Sync { reply_to: stx })
                .await
                .is_ok()
            {
                match srx.await {
                    Ok(Ok(())) => {
                        info!("IntentsBootstrapPhase: snapshot written after intents bootstrap")
                    }
                    Ok(Err(e)) => warn!("IntentsBootstrapPhase: snapshot write failed: {}", e),
                    Err(e) => warn!("IntentsBootstrapPhase: snapshot response error: {}", e),
                }
            }

            info!("IntentsBootstrapPhase: intents bootstrap complete");
            let _ = tx.send(());
            let _ = system_tx.send(SystemMessage::PhaseEvent).await;
        });

        PhaseResult::InProgress
    }

    async fn handle_message(&mut self, _msg: SystemMessage, _ctx: &PhaseContext) -> PhaseResult {
        if let Some(rx) = self.completion_rx.as_mut() {
            if rx.try_recv().is_ok() {
                self.completion_rx = None;
                return PhaseResult::Complete;
            }
        }
        PhaseResult::InProgress
    }
}

/// Bootstrap intents, error types, fix strategies, tools, and states from
/// config/intents.json into the graph database.
pub async fn bootstrap_intents_from_config(
    memory_graph_tx: &mpsc::Sender<MemoryGraphMessage>,
    config_path: &std::path::Path,
) -> anyhow::Result<usize> {
    let config_content = tokio::fs::read_to_string(config_path).await?;
    let config: serde_json::Value = serde_json::from_str(&config_content)?;

    let intents = config
        .get("intents")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let error_types = config
        .get("error_types")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let fix_strategies = config
        .get("fix_strategies")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let tools = config
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let states = config
        .get("states")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Open a transaction stream
    let (init_tx, init_rx) = oneshot::channel();
    memory_graph_tx
        .send(MemoryGraphMessage::OpenTransactionStream { reply_to: init_tx })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send OpenTransactionStream: {}", e))?;

    let stream_tx = init_rx
        .await
        .map_err(|e| anyhow::anyhow!("OpenTransactionStream response error: {}", e))?;

    let send_op = |op: StreamOp| {
        let stream_tx = stream_tx.clone();
        async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            stream_tx
                .send(TransactionRequest {
                    operation: op,
                    reply_to: reply_tx,
                })
                .await
                .map_err(|e| anyhow::anyhow!("Stream send failed: {}", e))?;
            reply_rx
                .await
                .map_err(|e| anyhow::anyhow!("Stream reply error: {}", e))?
                .map_err(|e| anyhow::anyhow!("Stream op failed: {}", e))
        }
    };

    // ── Delete existing nodes of these types before re-inserting ──
    // This prevents accumulation across restarts (graph is persisted).
    // Nodes are stored with label "SpireNode" and subtype property in memory_graph.rs.
    for subtype in &[
        "intent",
        "build_error",
        "fix_strategy",
        "tool",
        "build_state",
    ] {
        let delete_gql = format!(
            "MATCH (n:SpireNode) WHERE n.subtype = '{}' DETACH DELETE n",
            subtype
        );
        if let Err(e) = send_op(StreamOp::RawGql(delete_gql)).await {
            warn!(
                "bootstrap_intents_from_config: failed to clean old {} nodes: {}",
                subtype, e
            );
        }
    }

    let mut count = 0usize;

    // Track node IDs by name for relationship creation
    let mut intent_ids: HashMap<String, String> = HashMap::new();
    let mut error_ids: HashMap<String, String> = HashMap::new();
    let mut fix_ids: HashMap<String, String> = HashMap::new();
    let mut tool_ids: HashMap<String, String> = HashMap::new();
    let mut state_ids: HashMap<String, String> = HashMap::new();

    // Store intent nodes
    for intent in &intents {
        let name = intent
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let description = intent
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let priority = intent.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
        let patterns = intent
            .get("patterns")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let state_reqs = intent
            .get("state_requirements")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let handler = intent.get("handler").and_then(|v| v.as_str()).unwrap_or("");
        let action = intent.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let requires_approval = intent
            .get("requires_approval")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let required_capability = intent
            .get("required_capability")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let props: HashMap<String, serde_json::Value> = [
            ("priority".to_string(), serde_json::json!(priority)),
            ("patterns".to_string(), serde_json::json!(patterns)),
            (
                "state_requirements".to_string(),
                serde_json::json!(state_reqs),
            ),
            ("handler".to_string(), serde_json::json!(handler)),
            ("action".to_string(), serde_json::json!(action)),
            (
                "requires_approval".to_string(),
                serde_json::json!(requires_approval),
            ),
            (
                "required_capability".to_string(),
                serde_json::json!(required_capability),
            ),
        ]
        .into();

        let result = send_op(StreamOp::MergeNode(sp_attr_unknown(
                "Standard",
                Some("intent".to_string()),
                name.to_string(),
                Some(description.to_string()),
                props,
            )))
        .await?;

        if let StreamOpResult::NodeStored(node) = result {
            intent_ids.insert(name.to_string(), node.id().to_string());
        }
        count += 1;
    }

    // Store error type nodes
    for err in &error_types {
        let name = err
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let description = err
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let severity = err
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("medium");
        let patterns = err
            .get("detection_patterns")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let fix_strategies_list = err
            .get("fix_strategies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let props: HashMap<String, serde_json::Value> = [
            ("description".to_string(), serde_json::json!(description)),
            ("severity".to_string(), serde_json::json!(severity)),
            (
                "detection_patterns".to_string(),
                serde_json::json!(patterns),
            ),
            (
                "fix_strategies".to_string(),
                serde_json::json!(fix_strategies_list),
            ),
        ]
        .into();

        let result = send_op(StreamOp::MergeNode(sp_attr_unknown(
                "Blocker",
                Some("build_error".to_string()),
                name.to_string(),
                None,
                props,
            )))
        .await?;

        if let StreamOpResult::NodeStored(node) = result {
            error_ids.insert(name.to_string(), node.id().to_string());
        }
        count += 1;
    }

    // Store fix strategy nodes
    for fix in &fix_strategies {
        let name = fix
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let description = fix
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let category = fix
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("fix");
        let steps = fix
            .get("execution_steps")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let props: HashMap<String, serde_json::Value> = [
            ("description".to_string(), serde_json::json!(description)),
            ("category".to_string(), serde_json::json!(category)),
            ("execution_steps".to_string(), serde_json::json!(steps)),
        ]
        .into();

        let result = send_op(StreamOp::MergeNode(sp_attr_unknown(
                "Standard",
                Some("fix_strategy".to_string()),
                name.to_string(),
                None,
                props,
            )))
        .await?;

        if let StreamOpResult::NodeStored(node) = result {
            fix_ids.insert(name.to_string(), node.id().to_string());
        }
        count += 1;
    }

    // Store tool nodes
    for tool in &tools {
        let name = tool
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let description = tool
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let category = tool
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("tool");
        let capabilities = tool
            .get("capabilities")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let props: HashMap<String, serde_json::Value> = [
            ("description".to_string(), serde_json::json!(description)),
            ("category".to_string(), serde_json::json!(category)),
            ("capabilities".to_string(), serde_json::json!(capabilities)),
        ]
        .into();

        let result = send_op(StreamOp::MergeNode(sp_attr_unknown(
                "Standard",
                Some("tool".to_string()),
                name.to_string(),
                None,
                props,
            )))
        .await?;

        if let StreamOpResult::NodeStored(node) = result {
            tool_ids.insert(name.to_string(), node.id().to_string());
        }
        count += 1;
    }

    // Store state nodes
    for state in &states {
        let name = state
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let description = state
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let conditions = state
            .get("conditions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        // Determine if this state should be active by default.
        // `project_synced` is active because ProjectSyncPhase runs before
        // IntentsBootstrapPhase in the startup chain, so the project has
        // already been synced by the time we create state nodes.
        let is_active = name == "project_synced";

        let props: HashMap<String, serde_json::Value> = [
            ("description".to_string(), serde_json::json!(description)),
            ("conditions".to_string(), serde_json::json!(conditions)),
            ("active".to_string(), serde_json::json!(is_active)),
        ]
        .into();

        let result = send_op(StreamOp::MergeNode(sp_attr_unknown(
                "Standard",
                Some("build_state".to_string()),
                name.to_string(),
                None,
                props,
            )))
        .await?;

        if let StreamOpResult::NodeStored(node) = result {
            state_ids.insert(name.to_string(), node.id().to_string());
        }
        count += 1;
    }

    // ── Create relationships between nodes ──

    // Intent → Tool (SemanticallyRelated)
    for intent in &intents {
        let intent_name = intent.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(intent_id) = intent_ids.get(intent_name) {
            for tool in &tools {
                let tool_name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(tool_id) = tool_ids.get(tool_name) {
                    send_op(StreamOp::CreateRelationship(RelationshipInput {
                        edge_type: RelationshipType::SemanticallyRelated,
                        from_id: intent_id.clone(),
                        to_id: tool_id.clone(),
                        properties: None,
                        weight: Some(0.8),
                    }))
                    .await?;
                }
            }
        }
    }

    // Error type → Fix strategy (SemanticallyRelated)
    for err in &error_types {
        let err_name = err.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(error_id) = error_ids.get(err_name) {
            if let Some(fix_strategies_list) = err.get("fix_strategies").and_then(|v| v.as_array())
            {
                for fix_val in fix_strategies_list {
                    if let Some(fix_name) = fix_val.as_str() {
                        if let Some(fix_id) = fix_ids.get(fix_name) {
                            send_op(StreamOp::CreateRelationship(RelationshipInput {
                                edge_type: RelationshipType::SemanticallyRelated,
                                from_id: error_id.clone(),
                                to_id: fix_id.clone(),
                                properties: None,
                                weight: Some(0.9),
                            }))
                            .await?;
                        }
                    }
                }
            }
        }
    }

    // Intent → State (SemanticallyRelated) — links intents to their required states
    for intent in &intents {
        let intent_name = intent.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(intent_id) = intent_ids.get(intent_name) {
            if let Some(state_reqs) = intent.get("state_requirements").and_then(|v| v.as_array()) {
                for state_val in state_reqs {
                    if let Some(state_name) = state_val.as_str() {
                        if let Some(state_id) = state_ids.get(state_name) {
                            send_op(StreamOp::CreateRelationship(RelationshipInput {
                                edge_type: RelationshipType::SemanticallyRelated,
                                from_id: intent_id.clone(),
                                to_id: state_id.clone(),
                                properties: None,
                                weight: Some(0.7),
                            }))
                            .await?;
                        }
                    }
                }
            }
        }
    }

    // Commit
    send_op(StreamOp::Commit).await?;

    info!(
        "bootstrap_intents_from_config: stored {} nodes + relationships from config",
        count
    );
    Ok(count)
}

/// Discover tools from connected MCP servers and store them as Tool nodes
/// in the graph, annotated with capabilities from the capability mapping.
async fn discover_mcp_tools(
    memory_graph_tx: &mpsc::Sender<MemoryGraphMessage>,
    mcp_client_tx: &mpsc::Sender<McpClientMessage>,
) -> anyhow::Result<usize> {
    // Query connected servers with their tools
    let (tx, rx) = oneshot::channel();
    mcp_client_tx
        .send(McpClientMessage::GetConnectedServersWithTools { reply_to: tx })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send GetConnectedServersWithTools: {}", e))?;

    let servers_with_tools = rx
        .await
        .map_err(|e| anyhow::anyhow!("GetConnectedServersWithTools response error: {}", e))?;

    if servers_with_tools.is_empty() {
        info!("discover_mcp_tools: no connected MCP servers with tools");
        return Ok(0);
    }

    // Open a transaction stream
    let (init_tx, init_rx) = oneshot::channel();
    memory_graph_tx
        .send(MemoryGraphMessage::OpenTransactionStream { reply_to: init_tx })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send OpenTransactionStream: {}", e))?;

    let stream_tx = init_rx
        .await
        .map_err(|e| anyhow::anyhow!("OpenTransactionStream response error: {}", e))?;

    let send_op = |op: StreamOp| {
        let stream_tx = stream_tx.clone();
        async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            stream_tx
                .send(TransactionRequest {
                    operation: op,
                    reply_to: reply_tx,
                })
                .await
                .map_err(|e| anyhow::anyhow!("Stream send failed: {}", e))?;
            reply_rx
                .await
                .map_err(|e| anyhow::anyhow!("Stream reply error: {}", e))?
                .map_err(|e| anyhow::anyhow!("Stream op failed: {}", e))
        }
    };

    let mut count = 0usize;
    // Track tool node IDs for build system bridging
    let mut tool_node_ids: Vec<(String, String)> = Vec::new(); // (server_name, node_id)

    for (server_name, tools) in &servers_with_tools {
        for tool in tools {
            let tool_name = format!("{}/{}", server_name, tool.name);
            let description = tool.description.clone().unwrap_or_default();

            let props: HashMap<String, serde_json::Value> = [
                ("server_name".to_string(), serde_json::json!(server_name)),
                ("description".to_string(), serde_json::json!(description)),
                (
                    "input_schema".to_string(),
                    serde_json::json!(tool.input_schema),
                ),
            ]
            .into();

            let result = send_op(StreamOp::MergeNode(sp_attr_unknown(
                    "Standard",
                    Some("tool".to_string()),
                    tool_name,
                    None,
                    props,
                )))
            .await?;

            if let StreamOpResult::NodeStored(node) = result {
                tool_node_ids.push((server_name.clone(), node.id().to_string()));
            }
            count += 1;
        }
    }

    // Commit the tool nodes transaction
    send_op(StreamOp::Commit).await?;

    info!(
        "discover_mcp_tools: stored {} tool nodes from {} servers",
        count,
        servers_with_tools.len()
    );

    // ── Build system graph bridging ──────────────────────────────────
    // After storing tool nodes, check for build-capable MCP servers and
    // cross-reference with BuildSystem nodes in the graph. This creates
    // SemanticallyRelated edges from MCP tool nodes to BuildSystem nodes,
    // and auto-creates BuildSystem nodes for any build system that hasn't
    // been analyzed yet (e.g. a new mcp-bazel showing up).

    let (bs_tx, bs_rx) = oneshot::channel();
    if mcp_client_tx
        .send(McpClientMessage::GetBuildServers { reply_to: bs_tx })
        .await
        .is_ok()
    {
        if let Ok(build_servers) = bs_rx.await {
            if !build_servers.is_empty() {
                info!(
                    "discover_mcp_tools: bridging {} build-capable MCP servers",
                    build_servers.len()
                );

                // Open a second transaction for the bridging relationships
                let (init_tx, init_rx) = oneshot::channel();
                memory_graph_tx
                    .send(MemoryGraphMessage::OpenTransactionStream { reply_to: init_tx })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to send OpenTransactionStream: {}", e))?;

                let stream_tx = init_rx
                    .await
                    .map_err(|e| anyhow::anyhow!("OpenTransactionStream response error: {}", e))?;

                let send_op = |op: StreamOp| {
                    let stream_tx = stream_tx.clone();
                    async move {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        stream_tx
                            .send(TransactionRequest {
                                operation: op,
                                reply_to: reply_tx,
                            })
                            .await
                            .map_err(|e| anyhow::anyhow!("Stream send failed: {}", e))?;
                        reply_rx
                            .await
                            .map_err(|e| anyhow::anyhow!("Stream reply error: {}", e))?
                            .map_err(|e| anyhow::anyhow!("Stream op failed: {}", e))
                    }
                };

                for (server_name, bs_info) in &build_servers {
                    for build_system_name in &bs_info.build_systems {
                        // Query graph for existing BuildSystem node
                        let (qtx, qrx) = oneshot::channel();
                        memory_graph_tx
                            .send(MemoryGraphMessage::QueryAttrNodes {
                                node_type: Some("BuildSystem".to_string()),
                                subtype: Some(build_system_name.clone()),
                                name: Some(build_system_name.clone()),
                                limit: Some(1),
                                reply_to: qtx,
                            })
                            .await
                            .ok();

                        let build_system_node_id = match qrx.await {
                            Ok(Ok(nodes)) if !nodes.is_empty() => {
                                // Existing BuildSystem node found
                                info!(
                                    "discover_mcp_tools: found existing BuildSystem node '{}'",
                                    build_system_name
                                );
                                nodes[0].id().to_string()
                            }
                            _ => {
                                // Create a new BuildSystem node dynamically
                                info!(
                                    "discover_mcp_tools: auto-creating BuildSystem node '{}' from MCP server '{}'",
                                    build_system_name, server_name
                                );
                                let bs_props: HashMap<String, serde_json::Value> = [
                                    (
                                        "build_system".to_string(),
                                        serde_json::json!(build_system_name),
                                    ),
                                    (
                                        "source".to_string(),
                                        serde_json::json!(format!("mcp:{}", server_name)),
                                    ),
                                    ("server_name".to_string(), serde_json::json!(server_name)),
                                    (
                                        "config_files".to_string(),
                                        serde_json::json!(bs_info.config_files),
                                    ),
                                    (
                                        "build_type".to_string(),
                                        serde_json::json!(bs_info.build_type),
                                    ),
                                    (
                                        "capabilities".to_string(),
                                        serde_json::json!(bs_info.capabilities),
                                    ),
                                ]
                                .into();

                                let result = send_op(StreamOp::MergeNode(sp_attr_unknown(
                                        "BuildSystem",
                                        Some(build_system_name.clone()),
                                        build_system_name.clone(),
                                        Some(format!(
                                        "{} build system provided by MCP server '{}'",
                                        build_system_name, server_name
                                    )),
                                        bs_props,
                                    )))
                                .await?;

                                match result {
                                    StreamOpResult::NodeStored(node) => {
                                        info!(
                                            "discover_mcp_tools: created BuildSystem node '{}' (id={})",
                                            build_system_name, node.id().to_string()
                                        );
                                        node.id().to_string()
                                    }
                                    _ => {
                                        warn!(
                                            "discover_mcp_tools: failed to create BuildSystem node for '{}'",
                                            build_system_name
                                        );
                                        continue;
                                    }
                                }
                            }
                        };

                        // Link this server's tool nodes to the BuildSystem node
                        for (tool_server_name, tool_node_id) in &tool_node_ids {
                            if tool_server_name != server_name {
                                continue;
                            }
                            send_op(StreamOp::MergeRelationship(RelationshipInput {
                                edge_type: RelationshipType::SemanticallyRelated,
                                from_id: tool_node_id.clone(),
                                to_id: build_system_node_id.clone(),
                                properties: Some(HashMap::from([(
                                    "reason".to_string(),
                                    serde_json::json!("build_system_provides"),
                                )])),
                                weight: Some(0.9),
                            }))
                            .await?;
                        }
                    }
                }

                // Commit the bridging transaction
                send_op(StreamOp::Commit).await?;
                info!("discover_mcp_tools: build system bridging complete");
            }
        }
    }

    Ok(count)
}
/// Bootstrap strategy steps, tool providers, and step-to-tool mappings from
/// config/strategy-steps.json into the graph database.
/// This makes the entire tool orchestration pipeline graph-driven:
///   - StepDefinition nodes define what each step does and which tool it uses
///   - ToolProvider nodes define how to reach each tool (extension/MCP/LLM)
///   - ConcreteTool nodes are auto-discovered but also seeded from config
pub async fn bootstrap_strategy_steps(
    memory_graph_tx: &mpsc::Sender<MemoryGraphMessage>,
    config_path: &std::path::Path,
) -> anyhow::Result<usize> {
    let config_content = tokio::fs::read_to_string(config_path).await?;
    let config: serde_json::Value = serde_json::from_str(&config_content)?;

    let step_definitions = config
        .get("step_definitions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let tool_providers = config
        .get("tool_providers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Open a transaction stream
    let (init_tx, init_rx) = oneshot::channel();
    memory_graph_tx
        .send(MemoryGraphMessage::OpenTransactionStream { reply_to: init_tx })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send OpenTransactionStream: {}", e))?;

    let stream_tx = init_rx
        .await
        .map_err(|e| anyhow::anyhow!("OpenTransactionStream response error: {}", e))?;

    let send_op = |op: StreamOp| {
        let stream_tx = stream_tx.clone();
        async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            stream_tx
                .send(TransactionRequest {
                    operation: op,
                    reply_to: reply_tx,
                })
                .await
                .map_err(|e| anyhow::anyhow!("Stream send failed: {}", e))?;
            reply_rx
                .await
                .map_err(|e| anyhow::anyhow!("Stream reply error: {}", e))?
                .map_err(|e| anyhow::anyhow!("Stream op failed: {}", e))
        }
    };

    // Stale strategy nodes from previous bootstraps are replaced by the
    // `MergeNode` UPSERT below (it matches on (node_type, name) and re-inserts
    // under the same id). A raw DETACH-DELETE through the transaction stream's
    // query API can fail in SeleneDB and abort the whole stream, so no explicit
    // cleanup is performed here.

    let mut count = 0usize;

    // ── Store ToolProvider nodes ──
    for provider in &tool_providers {
        let name = provider
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let transport = provider
            .get("transport")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let prefix = provider
            .get("prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let description = provider
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let props: HashMap<String, serde_json::Value> = [
            ("transport".to_string(), serde_json::json!(transport)),
            ("prefix".to_string(), serde_json::json!(prefix)),
            ("description".to_string(), serde_json::json!(description)),
        ]
        .into();

        send_op(StreamOp::MergeNode(sp_attr_unknown(
                "ToolProvider",
                Some("tool_provider".to_string()),
                name.to_string(),
                Some(description.to_string()),
                props,
            )))
        .await?;
        count += 1;
    }

    // ── Store StepDefinition nodes ──
    for step in &step_definitions {
        let name = step
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let description = step
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let concrete_tool = step
            .get("concrete_tool")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let provider = step.get("provider").and_then(|v| v.as_str()).unwrap_or("");
        let arg_template = step
            .get("arg_template")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let depends_on = step
            .get("depends_on")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let output_key = step
            .get("output_key")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let category = step.get("category").and_then(|v| v.as_str()).unwrap_or("");

        let props: HashMap<String, serde_json::Value> = [
            ("description".to_string(), serde_json::json!(description)),
            (
                "concrete_tool".to_string(),
                serde_json::json!(concrete_tool),
            ),
            ("provider".to_string(), serde_json::json!(provider)),
            ("arg_template".to_string(), arg_template),
            ("depends_on".to_string(), serde_json::json!(depends_on)),
            ("output_key".to_string(), serde_json::json!(output_key)),
            ("category".to_string(), serde_json::json!(category)),
        ]
        .into();

        send_op(StreamOp::MergeNode(sp_attr_unknown(
                "StepDefinition",
                Some("step_definition".to_string()),
                name.to_string(),
                Some(description.to_string()),
                props,
            )))
        .await?;
        count += 1;
    }

    // Commit the transaction
    send_op(StreamOp::Commit).await?;

    info!(
        "bootstrap_strategy_steps: stored {} nodes (step definitions + tool providers)",
        count
    );
    Ok(count)
}

// ============================================================================
// Phase chain builder
// ============================================================================

/// Build the full startup phase chain.
pub fn build_startup_phases(mcp_config_path: Option<PathBuf>) -> Box<dyn StartupPhase> {
    // Phase 1: Initialize the graph database (serial — everything depends on this)
    let graph_init = GraphInitPhase;

    // Phase 2: Parallel initialization of sub-actors + embedder + MCP bootstrap
    let parallel_init = PhaseGroup::new(
        "parallel_init",
        vec![
            Box::new(EmbedderInitPhase),
            Box::new(McpBootstrapPhase::new(mcp_config_path)),
            Box::new(PlatformBootstrapPhase),
            Box::new(ProjectSyncInitPhase),
            Box::new(ProjectAnalyzerInitPhase),
            Box::new(ProjectQueryInitPhase),
        ],
    );

    // Phase 3: Parallel connect/sync/analyze/load
    let parallel_connect = PhaseGroup::new(
        "parallel_connect",
        vec![
            Box::new(McpConnectPhase),
            Box::new(ProjectSyncPhase::new()),
            Box::new(ProjectAnalysisPhase::new()),
            Box::new(LlmConfigPhase::new()),
        ],
    );

    // Phase 4: Intents bootstrap (needs MCP connected + project sync + analysis)
    let intents_bootstrap = PhaseGroup::new(
        "intents_bootstrap",
        vec![
            Box::new(IntentsBootstrapInitPhase),
            Box::new(IntentsBootstrapPhase::new()),
        ],
    );

    // Phase 5: Register tools (serial — needs MCP connected)
    // Note: With the ToolRouterActor, tools are now registered dynamically
    // via the ToolRouterActor's ListTools method, which aggregates tools
    // from all backends (extension, embedded, MCP). The old RegisterToolsPhase
    // that pushed internal tools into the MCP client is no longer needed.
    let register_tools = RegisterToolsPhase;

    // Chain them sequentially: graph_init → parallel_init → parallel_connect → intents_bootstrap → register_tools
    Box::new(PhaseChain::new(
        "startup",
        vec![
            Box::new(graph_init),
            Box::new(parallel_init),
            Box::new(parallel_connect),
            Box::new(intents_bootstrap),
            Box::new(register_tools),
        ],
    ))
}
