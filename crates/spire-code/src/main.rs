// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Spire Core — standalone binary entry point.
//!
//! This binary runs as a subprocess of the VS Code extension, communicating
//! via JSON-RPC 2.0 over a TCP loopback socket.
//!
//! Architecture:
//!   VS Code Extension (BidirectionalClient) ←→ spire-core (this binary)
//!     - Core binds to 127.0.0.1:0 and prints "SPIRE_PORT=<port>" to stdout
//!     - Extension reads the port from stdout and connects via TCP
//!     - All JSON-RPC messages flow over the TCP connection
//!
//! Usage:
//!   cargo run --bin spire-core           # standalone (testing)
//!   cargo run --bin spire-core -- --mcp  # with MCP client connections

use chrono::Local;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, warn};

use spire_code::subsystems::build::build_manager::{BuildManagerActor, BuildManagerMessage};
use spire_code::build::CargoBuildModule;
use spire_core::modules::{
    FilesystemMessage, GitMessage, ProcessMessage, SearchMessage, TerminalMessage,
};
use spire_core::actors::tool_providers::ToolRouterActor;
use spire_code::actors::tool_providers::build_default_registry;
use spire_core::actors::{
    ActorSystem, ChatActor, LlmActor, LlmConfig, McpClientActor, MemoryGraphActor, ProgressActor,
    SystemPromptActor, ToolOrchestrator, ToolsActor,
};
use spire_code::actors::{
    CoordinatorActor, CoordinatorMessage, IntentRouterActor,
    PlanOrchestrator, ProjectAnalyzerActor, ProjectBuildActor, ProjectInstallActor, ProjectLintActor,
    ProjectQueryActor, ProjectSyncActor, ProjectTestActor, SystemActor, SystemMessage,
};
use spire_core::embedder::candle_embedder::CandleEmbedder;
use spire_core::models::embedding::Embedder;
use spire_core::transport::socket::{
    IncomingNotification, IncomingRequestMessage, TransportActor, TransportMessage,
};

/// Determine the log directory.
/// Priority: SPIRE_LOG_DIR env var, then {project_root}/.spire/logs,
/// then a temp dir. The directory is created in every branch so the caller
/// can `File::create` a log file without an extra mkdir.
fn log_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SPIRE_LOG_DIR") {
        let dir = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    // Prefer project-local .spire/logs directory
    if let Ok(project_root) = std::env::var("SPIRE_PROJECT_ROOT") {
        let dir = PathBuf::from(project_root).join(".spire").join("logs");
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    // Fallback: use a temp directory
    let dir = std::env::temp_dir().join("spire-core-logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Resolve a log file path that restarts on every extension start.
///
/// Naming scheme:
///   - 1st start on a given day: `spire-core.log.YYYY-MM-DD`
///   - 2nd start on the same day: `spire-core.log.YYYY-MM-DD.1`
///   - 3rd start: `spire-core.log.YYYY-MM-DD.2`
///   - etc.
///
/// The index resets on a new calendar day.
fn resolve_log_path(log_dir: &PathBuf) -> PathBuf {
    let date = Local::now().format("%Y-%m-%d").to_string();
    let base = log_dir.join(format!("spire-core.log.{}", date));

    if !base.exists() {
        return base;
    }

    // Scan for the next available index
    for i in 1.. {
        let candidate = log_dir.join(format!("spire-core.log.{}.{}", date, i));
        if !candidate.exists() {
            return candidate;
        }
    }

    // Safety valve: should never reach here in practice
    base
}

/// Clean up old log files from previous days.
///
/// Log files are named `spire-core.log.YYYY-MM-DD` or `spire-core.log.YYYY-MM-DD.N`.
/// This function deletes any log files whose date does not match today's date,
/// keeping only the current day's logs.
fn cleanup_logs(log_dir: &PathBuf) {
    let today = Local::now().format("%Y-%m-%d").to_string();
    let today_prefix = format!("spire-core.log.{}", today);

    let dir = match std::fs::read_dir(log_dir) {
        Ok(d) => d,
        Err(e) => {
            warn!("Failed to list log directory for cleanup: {}", e);
            return;
        }
    };

    for entry in dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(f) => f.to_string(),
            None => continue,
        };

        // Only consider files matching the log pattern
        if !filename.starts_with("spire-core.log.") {
            continue;
        }

        // Skip today's logs
        if filename == today_prefix || filename.starts_with(&format!("{}.", today_prefix)) {
            continue;
        }

        // Delete old log files
        if let Err(e) = std::fs::remove_file(&path) {
            warn!("Failed to remove old log file {}: {}", filename, e);
        } else {
            info!("Removed old log file: {}", filename);
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── Initialise tracing — log to a file (never stdout, which is JSON-RPC) ──
    let log_dir = log_dir();

    // Export SPIRE_LOG_DIR so that spawned MCP server subprocesses inherit it
    // and can write their own log files to the same directory.
    std::env::set_var("SPIRE_LOG_DIR", &log_dir);

    // Clean up old log files from previous days before creating today's log
    cleanup_logs(&log_dir);

    let log_path = resolve_log_path(&log_dir);

    // Open the log file directly (no daily rolling — we handle rotation ourselves)
    let log_file = std::fs::File::create(&log_path).expect("Failed to create log file");
    let (non_blocking, _guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(128) // flush more frequently (default is 32_768)
        .finish(log_file);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info").add_directive(
                    "rust_mcp_sdk::mcp_runtimes::client_runtime=error"
                        .parse()
                        .expect("valid filter directive"),
                )
            }),
        )
        .with_writer(non_blocking)
        .with_ansi(false) // No ANSI escape codes in log files
        .init();

    info!("Spire Core starting...");
    info!("Logging to: {}", log_path.display());
    info!("SPIRE_LOG_DIR set to: {}", log_dir.display());

    // ── Initialise SeleneDB graph database with WAL persistence ──
    // The data directory is project-root/.spire/data so that
    // SeleneDB's snapshot and WAL files live in a dedicated subdirectory,
    // separate from logs and MCP config.
    let data_dir = if let Ok(dir) = std::env::var("SPIRE_DATA_DIR") {
        PathBuf::from(dir)
    } else if let Ok(project_root) = std::env::var("SPIRE_PROJECT_ROOT") {
        PathBuf::from(project_root).join(".spire").join("data")
    } else {
        std::env::temp_dir().join("spire-core-data")
    };

    std::fs::create_dir_all(&data_dir).expect("Failed to create data directory");

    info!("Data directory: {}", data_dir.display());

    // ── Resolve the project root early ──
    // Used by ProjectBuildActor (and others) to resolve relative build paths
    // to absolute paths before dispatching to MCP build tools.
    let project_root = std::env::var("SPIRE_PROJECT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            // Safety guard: never scan the root filesystem
            if cwd == *"/" {
                let fallback = PathBuf::from(".");
                warn!("SPIRE_PROJECT_ROOT not set and current_dir is '/', falling back to '.' — project sync will be skipped");
                fallback
            } else {
                cwd
            }
        });

    // ── Create the actor system ──
    let system = ActorSystem::new();

    // Spawn the chat actor
    let (chat_tx, _chat_handle) = system.spawn(ChatActor::new());

    // Spawn the progress actor (must be before McpClientActor which needs progress_tx)
    let (progress_tx, _progress_handle) = system.spawn(ProgressActor::new());

    // Spawn the MCP client actor with progress notifications
    let (mcp_client_tx, _mcp_client_handle) =
        system.spawn(McpClientActor::with_progress(progress_tx.clone()));

    // Spawn the LLM actor
    let (llm_tx, _llm_handle) = system.spawn(LlmActor::new(LlmConfig { 
        planning_model: LlmConfig::default().planning_model,
        coding_model: LlmConfig::default().coding_model,
        ..LlmConfig::default()
    }));

    // Spawn the system actor
    let (system_tx, _system_handle) = system.spawn(SystemActor::new());

    // Spawn the memory graph actor (knowledge graph + config storage)
    let (memory_graph_tx, _memory_graph_handle) = system.spawn(MemoryGraphActor::new());

    // Spawn the project sync actor (three-phase project structure sync)
    let (project_sync_tx, _project_sync_handle) = system.spawn(ProjectSyncActor::new());

    // Spawn the project analyzer actor (semantic project analysis for LLM)
    let (project_analyzer_tx, _project_analyzer_handle) = system.spawn(ProjectAnalyzerActor::new());

    // Spawn the project query actor (semantic project queries for LLM)
    let (project_query_tx, _project_query_handle) = system.spawn(ProjectQueryActor::new());

    // Spawn the system prompt actor (caches prefix for DeepSeek prompt caching)
    let (system_prompt_tx, _system_prompt_handle) = system.spawn(SystemPromptActor::new());

    // Initialize the system prompt actor with the project query channel
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let pq_tx = project_query_tx.clone();
        let project_tool_caller: spire_core::actors::system_prompt::ProjectToolCaller =
            std::sync::Arc::new(move |tool: String, args: serde_json::Value| {
                let tx = pq_tx.clone();
                Box::pin(async move {
                    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(spire_code::subsystems::project::project_query::ProjectQueryMessage::CallTool {
                            tool,
                            args,
                            reply_to: reply_tx,
                        })
                        .await
                        .is_err()
                    {
                        return serde_json::json!({"error": "ProjectQuery channel closed"});
                    }
                    reply_rx.await
                        .unwrap_or(serde_json::json!({"error": "ProjectQuery response error"}))
                })
            });
        let _ = system_prompt_tx
            .send(spire_core::actors::SystemPromptMessage::Initialize {
                project_tool_caller,
                reply_to: tx,
            })
            .await;
        if let Ok(Ok(())) = rx.await {
            info!("SystemPromptActor initialized");
        }
    }

    // ── Spawn the intent routing actors ──
    // These actors form the user query → intent routing → context injection → LLM prompt pipeline.

    // Spawn the intent router actor (routes user queries to matched intents)
    let (intent_router_tx, _intent_router_handle) =
        system.spawn(IntentRouterActor::new(memory_graph_tx.clone()));

    // ── Build-fix loop actors ──
    // The ErrorAnalyzer / BuildOrchestrator / PromptHandlerActor build-fix loop is NOT
    // spawned in this binary: it was only reachable through coordinator fields that were
    // never read. The actor implementations remain (covered by actor_tests.rs) in case the
    // loop is wired back up later.
    // NOTE: ToolOrchestrator is spawned later (after Transport + ToolRouter).

    // ── Spawn the TransportActor (replaces old Arc<Mutex<Transport>>) ──
    let (transport_tx, _transport_handle) = system.spawn(TransportActor::new());

    // Set the transport actor's own sender so the reader task can send messages back
    let transport_tx_clone = transport_tx.clone();
    transport_tx
        .send(TransportMessage::SetSelfTx {
            self_tx: transport_tx_clone,
        })
        .await
        .map_err(|e| format!("Failed to set TransportActor self_tx: {}", e))?;

    // ── Bind the transport early and print the port ──
    // We bind BEFORE the blocking initialization so the extension can discover
    // the port immediately, rather than waiting for the full startup sequence
    // (which includes downloading the ~85MB embedding model from HuggingFace).
    let port = {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<u16, String>>();
        transport_tx
            .send(TransportMessage::Bind { reply_to: tx })
            .await
            .map_err(|e| format!("Failed to send Bind to TransportActor: {}", e))?;
        rx.await
            .map_err(|e| format!("TransportActor Bind response error: {}", e))?
            .map_err(|e| format!("TransportActor Bind failed: {}", e))?
    };
    println!("SPIRE_PORT={}", port);
    info!(
        "Spire Core transport bound to port {}. Extension can connect now.",
        port
    );

    // ── Accept the extension's TCP connection immediately ──
    // We accept BEFORE the blocking initialization so that:
    // 1. The extension's TCP connection doesn't hang in the backlog
    // 2. The transport writer is available for sending progress notifications
    //    during the SystemActor initialization
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        transport_tx
            .send(TransportMessage::Accept { reply_to: tx })
            .await
            .map_err(|e| format!("Failed to send Accept to TransportActor: {}", e))?;
        rx.await
            .map_err(|e| format!("TransportActor Accept response error: {}", e))?
            .map_err(|e| format!("TransportActor Accept failed: {}", e))?;
    }
    info!("Spire Core accepted extension connection on port {}.", port);

    // ── Build modules + BuildManager ──
    // Spawn the BuildManager + Cargo module so ALL build dispatch (including
    // the project_* meta-tools) flows through registered build modules. The
    // FFI spawns additional modules (node, swift, meson, …); the standalone
    // binary keeps Cargo as the primary supported runtime.
    let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(
        memory_graph_tx.clone(),
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        std::sync::Arc::new(tokio::sync::Notify::new()),
    ));
    {
        // Stage-1 HAL implementation generation (hal_generate_impl) routes
        // through the LLM actor with the role Coding — wire the same llm_tx
        // used by the planning subsystem so the build manager can invoke it.
        let _ = bm_tx
            .send(BuildManagerMessage::SetLlm {
                llm_tx: llm_tx.clone(),
            })
            .await;
    }
    {
        use spire_code::Actor as _;
        let (cargo_tx, cargo_rx) =
            tokio::sync::mpsc::channel::<spire_code::build::BuildModuleMessage>(8);
        CargoBuildModule::new().spawn(cargo_rx);
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = cargo_tx
            .send(spire_code::build::BuildModuleMessage::DescribeCapabilities {
                reply_to: t,
            })
            .await;
        if let Ok(cap) = r.await {
            let _ = bm_tx
                .send(BuildManagerMessage::AddModule {
                    capability: cap,
                    module_tx: cargo_tx,
                })
                .await;
        }
    }

    // ── Spawn the project build actor (orchestrates multi-system builds) ──
    // Spawned after the transport so it can push real-time chat notifications
    // to the webview via event/chat/message notifications.
    let (project_build_tx, _project_build_handle) = system.spawn(ProjectBuildActor::new(
        project_query_tx.clone(),
        mcp_client_tx.clone(),
        progress_tx.clone(),
        chat_tx.clone(),
        transport_tx.clone(),
        memory_graph_tx.clone(),
        bm_tx.clone(),
        project_root.clone(),
    ));

    // ── Spawn the project meta-tool actors ──
    // These actors provide project/test, project/check, project/lint,
    // project/format, project/install, and project/add_dependency.
    let (project_test_tx, _project_test_handle) = system.spawn(ProjectTestActor::new(
        project_query_tx.clone(),
        mcp_client_tx.clone(),
        bm_tx.clone(),
    ));
    let (project_lint_tx, _project_lint_handle) = system.spawn(ProjectLintActor::new(
        project_query_tx.clone(),
        mcp_client_tx.clone(),
        bm_tx.clone(),
    ));
    let (project_install_tx, _project_install_handle) = system.spawn(ProjectInstallActor::new(
        project_query_tx.clone(),
        mcp_client_tx.clone(),
        bm_tx.clone(),
    ));

    // ── Spawn the ToolRouterActor (replaces old ToolDispatcher) ──
    // The ToolRouterActor routes tool calls by prefix matching:
    //   - Extension tools: workspace/, document/, diagnostics/, git/, symbols/
    //   - Embedded tools: project/, build/, test/, lint/, install/
    //   - MCP tools: (catch-all) everything else
    //
    // The standalone binary does not spawn all in-process core modules, so
    // pass unused dummy channels for those. The BuildManager is real.
    let (dummy_fs_tx, _) = tokio::sync::mpsc::channel::<FilesystemMessage>(8);
    let (dummy_git_tx, _) = tokio::sync::mpsc::channel::<GitMessage>(8);
    let (dummy_process_tx, _) = tokio::sync::mpsc::channel::<ProcessMessage>(8);
    let (dummy_search_tx, _) = tokio::sync::mpsc::channel::<SearchMessage>(8);
    let (dummy_terminal_tx, _) = tokio::sync::mpsc::channel::<TerminalMessage>(8);
    let (dummy_rag_tx, _) =
        tokio::sync::mpsc::channel::<spire_core::actors::rag::RagMessage>(8);
    let dummy_rag_domain: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let tool_registry = build_default_registry(
        transport_tx.clone(),
        project_query_tx.clone(),
        Some(project_build_tx.clone()),
        Some(project_test_tx.clone()),
        Some(project_lint_tx.clone()),
        Some(project_install_tx.clone()),
        // Standalone binary: in-process core modules are unused (MCP catch-all).
        dummy_fs_tx,
        dummy_git_tx,
        dummy_process_tx,
        dummy_search_tx,
        dummy_terminal_tx,
        bm_tx,
        dummy_rag_tx,
        dummy_rag_domain,
    )
    .await
    .expect("build tool registry");
    let (tool_router_tx, _tool_router_handle) =
        system.spawn(ToolRouterActor::new(tool_registry, mcp_client_tx.clone()));

    // Spawn the tools actor with the tool router sender
    let (tools_tx, _tools_handle) = system.spawn(ToolsActor::new(tool_router_tx.clone()));

    // Spawn the tool orchestrator actor (executes tools and tool chains)
    // Must be spawned AFTER TransportActor + ToolRouterActor + MCP client + LLM.
    let (tool_orchestrator_tx, _tool_orchestrator_handle) = system.spawn(ToolOrchestrator::new(
        memory_graph_tx.clone(),
        transport_tx.clone(),
        mcp_client_tx.clone(),
        llm_tx.clone(),
        tool_router_tx.clone(),
    ));

    // Spawn the plan orchestrator actor (creates and executes multi-step plans)
    let (plan_orchestrator_tx, _plan_orchestrator_handle) = system.spawn(PlanOrchestrator::new(
        memory_graph_tx.clone(),
        llm_tx.clone(),
        tool_orchestrator_tx.clone(),
        chat_tx.clone(),
        transport_tx.clone(),
    ));

    // ── Subscribe to progress updates BEFORE SystemActor initialization ──
    // We subscribe synchronously here (not in a spawned task) to ensure the
    // broadcast receiver is registered before the SystemActor starts sending
    // progress updates during initialization. Otherwise, the first few updates
    // would be lost (broadcast channel only keeps messages for active receivers).
    let progress_rx = {
        let (subscribe_tx, subscribe_rx) = tokio::sync::oneshot::channel::<
            tokio::sync::broadcast::Receiver<spire_core::actors::ProgressUpdate>,
        >();
        if progress_tx
            .send(spire_core::actors::ProgressMessage::Subscribe {
                reply_to: subscribe_tx,
            })
            .await
            .is_ok()
        {
            subscribe_rx.await.ok()
        } else {
            None
        }
    };

    // ── Forward progress updates to the extension as JSON-RPC notifications ──
    // This is spawned as a background task that reads from the broadcast receiver
    // and forwards each update to the extension via the transport.
    if let Some(progress_rx) = progress_rx {
        let transport_tx = transport_tx.clone();
        tokio::spawn(async move {
            let mut rx = progress_rx;
            while let Ok(update) = rx.recv().await {
                let params = serde_json::json!({
                    "taskId": update.task_id,
                    "message": update.message,
                    "percent": update.percent,
                    "status": match update.status {
                        spire_core::actors::ProgressStatus::Running => "running",
                        spire_core::actors::ProgressStatus::Completed => "completed",
                        spire_core::actors::ProgressStatus::Failed => "failed",
                    },
                    "metadata": update.metadata,
                });
                let _ = transport_tx
                    .send(TransportMessage::SendNotification {
                        method: "event/system/progress".to_string(),
                        params,
                    })
                    .await;
            }
        });
    }

    // Clone senders before moving originals into CoordinatorActor
    let mcp_client_tx_for_system = mcp_client_tx.clone();
    let llm_tx_for_system = llm_tx.clone();
    let system_tx_for_system = system_tx.clone();

    // Spawn the coordinator actor with all sub-actor senders
    let (coordinator_tx, _coordinator_handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_client_tx,
        llm_tx,
        system_tx,
        memory_graph_tx.clone(),
        project_query_tx.clone(),
        intent_router_tx.clone(),
        tool_router_tx.clone(),
        plan_orchestrator_tx.clone(),
        transport_tx.clone(),
    ));

    // ── Initialize the SystemActor (drives the full startup state machine) ──
    {
        // Create the embedder (spawn_blocking because CandleEmbedder::new()
        // does blocking I/O via hf_hub to load model weights from cache/network).
        // Timeout so a slow/hanging model download degrades to the no-op
        // embedder instead of blocking startup forever.
        let embedder: Arc<dyn Embedder> =
            match tokio::time::timeout(
                std::time::Duration::from_secs(45),
                tokio::task::spawn_blocking(CandleEmbedder::new),
            )
            .await
            {
                Ok(Ok(Ok(embedder))) => {
                    info!("CandleEmbedder created successfully");
                    Arc::new(embedder)
                }
                Ok(Ok(Err(e))) => {
                    error!(
                        "Failed to create CandleEmbedder: {}. Running without embeddings.",
                        e
                    );
                    // Use a no-op embedder so the system can still start
                    Arc::new(spire_core::embedder::NoopEmbedder)
                }
                Ok(Err(e)) => {
                    error!(
                        "CandleEmbedder creation task panicked: {}. Running without embeddings.",
                        e
                    );
                    Arc::new(spire_core::embedder::NoopEmbedder)
                }
                Err(_elapsed) => {
                    error!(
                        "CandleEmbedder creation timed out after 45s. Running without embeddings."
                    );
                    Arc::new(spire_core::embedder::NoopEmbedder)
                }
            };

        // Step 1: Set the system actor's own sender (for PhaseEvent messages)
        let _ = system_tx_for_system
            .send(SystemMessage::SetSystemTx {
                system_tx: system_tx_for_system.clone(),
            })
            .await;

        // Step 2: Send Initialize — the SystemActor will drive the phase chain
        // asynchronously via PhaseEvent messages sent to itself.
        // We do NOT block on the reply here — the phase chain runs in the
        // background and the extension will see progress updates via the
        // broadcast channel.
        let (_tx, rx) =
            tokio::sync::oneshot::channel::<Result<(), spire_core::actors::ActorError>>();
        if system_tx_for_system
            .send(SystemMessage::Initialize {
                coordinator_tx: coordinator_tx.clone(),
                memory_graph_tx: memory_graph_tx.clone(),
                mcp_client_tx: mcp_client_tx_for_system,
                project_sync_tx: project_sync_tx.clone(),
                project_analyzer_tx: project_analyzer_tx.clone(),
                project_query_tx: project_query_tx.clone(),
                llm_tx: llm_tx_for_system,
                progress_tx: progress_tx.clone(),
                embedder,
                data_dir: data_dir.clone(),
                project_root,
                reply_to: _tx,
            })
            .await
            .is_ok()
        {
            // Don't block — the phase chain runs asynchronously.
            // The reply will arrive when all phases complete.
            tokio::spawn(async move {
                match rx.await {
                    Ok(Ok(())) => info!("SystemActor initialization complete"),
                    Ok(Err(e)) => error!("SystemActor initialization failed: {}", e),
                    Err(e) => error!("SystemActor initialization response error: {}", e),
                }
            });
        }
    }

    // Set up the request handler: incoming requests from the extension
    // are forwarded to the coordinator actor.
    let coordinator_tx_clone = coordinator_tx.clone();
    {
        // Create a channel for incoming requests from the transport
        let (handler_tx, mut handler_rx) =
            tokio::sync::mpsc::channel::<IncomingRequestMessage>(256);

        // Spawn a task to forward incoming requests to the coordinator
        let coordinator_tx = coordinator_tx_clone.clone();
        tokio::spawn(async move {
            while let Some(req) = handler_rx.recv().await {
                let coordinator_tx = coordinator_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = coordinator_tx
                        .send(CoordinatorMessage::HandleRequest {
                            method: req.method,
                            params: req.params,
                            response_tx: req.response_tx,
                        })
                        .await
                    {
                        error!("Failed to send request to coordinator: {}", e);
                    }
                });
            }
        });

        transport_tx
            .send(TransportMessage::SetRequestHandler { handler_tx })
            .await
            .map_err(|e| format!("Failed to set request handler: {}", e))?;
    }

    // Set up the notification handler: incoming notifications from the extension
    // are forwarded to the coordinator actor.
    {
        // Create a channel for incoming notifications from the transport
        let (notification_tx, mut notification_rx) =
            tokio::sync::mpsc::channel::<IncomingNotification>(256);

        // Spawn a task to forward incoming notifications to the coordinator
        // Notifications use the HandleRequest flow (requests without response_tx)
        tokio::spawn(async move {
            while let Some(notification) = notification_rx.recv().await {
                info!(
                    "Received notification: {} (params: {:?})",
                    notification.method, notification.params
                );
                // Notifications are intentionally log-only: they carry no
                // response_tx, so they can't use the HandleRequest flow, and the
                // coordinator has no notification sink. The shipping app (FFI)
                // receives file events via the FileWatcherActor + the
                // `spire_wait_for_event` FFI channel, not transport notifications.
            }
        });

        transport_tx
            .send(TransportMessage::SetNotificationHandler { notification_tx })
            .await
            .map_err(|e| format!("Failed to set notification handler: {}", e))?;

        info!("Notification handler registered for file events");
    }

    info!(
        "Spire Core is ready. Connected on 127.0.0.1:{} for JSON-RPC messages.",
        port
    );

    // ── Wait for shutdown signal ──
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    // Sync/flush the WAL before shutdown.
    // Send Sync directly to the MemoryGraphActor to write a snapshot
    // immediately, bypassing the 2-second debounce delay.
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), anyhow::Error>>();
        if memory_graph_tx
            .send(spire_core::actors::MemoryGraphMessage::Sync { reply_to: tx })
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    // Send shutdown to coordinator
    let _ = coordinator_tx.send(CoordinatorMessage::Shutdown).await;

    info!("Spire Core shut down gracefully");
    Ok(())
}
