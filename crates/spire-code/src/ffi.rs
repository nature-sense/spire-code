// spire-ffi — C FFI bridge for Swift UI to call the Rust core.

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;

use crate::subsystems::build::build_manager::{BuildManagerActor, BuildManagerMessage};
use crate::{
    BuildModuleMessage, CargoBuildModule, CmakeBuildModule, GoBuildModule, GradleBuildModule,
    MakeBuildModule, MavenBuildModule, MesonBuildModule, ModuleCapability, NodeBuildModule,
    PythonBuildModule, RubyBuildModule, SwiftBuildModule,
};
use spire_core::modules::{
    FilesystemMessage, FilesystemModule, GitMessage, GitModule, ProcessMessage, ProcessModule,
    SearchMessage, SearchModule, TerminalMessage, TerminalModule,
};
use spire_core::actors::tool_providers::ToolRouterActor;
use crate::actors::tool_providers::build_default_registry;
use crate::subsystems::project::project_creation::{ProjectCreationActor, ProjectCreationMessage};
use crate::subsystems::project::spec_design::{SpecDesignActor, SpecDesignMessage};
use crate::subsystems::project::project_build::{ProjectBuildActor, ProjectBuildMessage};
use crate::subsystems::project::project_install::{ProjectInstallActor, ProjectInstallMessage};
use crate::subsystems::project::project_lint::{ProjectLintActor, ProjectLintMessage};
use crate::subsystems::project::project_test::{ProjectTestActor, ProjectTestMessage};
use crate::subsystems::planning::plan_orchestrator::PlanOrchestrator;
use crate::subsystems::planning::plan_orchestrator::PlanOrchestratorMessage;
use spire_core::actors::rag::{RagActor, RagMessage};
use spire_core::subsystems::tools::tool_orchestrator::ToolOrchestrator;
use spire_core::actors::{
    ActorSystem, ChatActor, ChatMessage, LlmActor, LlmConfig, LlmMessage, McpClientActor,
    McpClientMessage, MemoryGraphActor, MemoryGraphMessage, ProgressActor, ProgressMessage,
    SystemPromptActor, SystemPromptMessage, ToolsActor,
};
use crate::actors::{
    CoordinatorActor, CoordinatorMessage, FfiSharedState, IntentRouterActor,
    IntentRouterMessage, ProjectAnalyzerActor, ProjectAnalyzerMessage, ProjectQueryActor,
    ProjectQueryMessage, ProjectSyncActor, ProjectSyncMessage, SystemActor, SystemMessage,
};
use spire_core::models::embedding::Embedder;

use spire_actor::registry::ServiceRegistry;

pub(crate) fn dummy_tx<T: Send + 'static>() -> tokio::sync::mpsc::Sender<T> {
    tokio::sync::mpsc::channel::<T>(64).0
}

/// Build an envelope `AttrNode` for a dynamically-typed ("Unknown") node.
fn ffi_attr_unknown(
    subtype: Option<String>,
    name: String,
    description: Option<String>,
    properties: std::collections::HashMap<String, serde_json::Value>,
) -> spire_core::models::memory_graph::AttrNode {
    let now = chrono::Utc::now();
    spire_core::models::memory_graph::AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: "Unknown".to_string(),
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

/// Spawn a spire-modules Actor and return its message sender.
fn spawn_module<A: spire_actor::Actor>(actor: A) -> tokio::sync::mpsc::Sender<A::Message> {
    let (tx, rx) = tokio::sync::mpsc::channel::<A::Message>(32);
    actor.spawn(rx);
    tx
}

struct AppState {
    coordinator_tx: tokio::sync::mpsc::Sender<CoordinatorMessage>,
    event_rx: std::sync::Mutex<Option<tokio::sync::broadcast::Receiver<String>>>,
    runtime: tokio::runtime::Runtime,
    /// Shared buffer of streaming build events, drained directly by the FFI
    /// (bypasses the actor message loop so output can be read during a build).
    build_event_buffer: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

unsafe impl Send for AppState {}
unsafe impl Sync for AppState {}

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static STATE: Lazy<Mutex<Option<AppState>>> = Lazy::new(|| Mutex::new(None));
/// Shared buffer of streaming build events, drained directly by the FFI while
/// a build is running (bypasses the actor message loop).
static BUILD_EVENT_BUFFER: Lazy<std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>> =
    Lazy::new(|| std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
/// Notifier signaled whenever a build event is pushed, so the FFI wait function
/// wakes immediately without polling. Same lifetime as BUILD_EVENT_BUFFER.
static BUILD_NOTIFY: Lazy<std::sync::Arc<tokio::sync::Notify>> =
    Lazy::new(|| std::sync::Arc::new(tokio::sync::Notify::new()));

/// Lock the global STATE mutex without panicking on a poisoned lock.
///
/// A panic while one thread holds STATE (e.g. inside a `block_on`) poisons the
/// std mutex. Calling `.unwrap()` in every other FFI entry point would then
/// panic with `PoisonError` and take down the whole UI process (SIGABRT).
/// Recover the guard from the poison instead so the app keeps running.
fn lock_state() -> std::sync::MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let log_dir = spire_core::config::config_dir().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("spire-ui.log");
    let log_file = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let (writer, guard) = tracing_appender::non_blocking(log_file);
    std::mem::forget(guard);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(
            "info,rust_mcp_sdk::mcp_runtimes::client_runtime=off",
        ))
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
}


/// Query a build module's capabilities via DescribeCapabilities, register it
/// with the BuildManager, and return the capability (so MCP server deps can
/// be collected). Plain async fn (no closure lifetime issues).
async fn register_build_module(
    cap_name: &str,
    module_tx: tokio::sync::mpsc::Sender<BuildModuleMessage>,
    bm_tx: &tokio::sync::mpsc::Sender<BuildManagerMessage>,
) -> ModuleCapability {
    let (t, r) = tokio::sync::oneshot::channel();
    let _ = module_tx
        .send(BuildModuleMessage::DescribeCapabilities { reply_to: t })
        .await;
    let cap = match r.await {
        Ok(cap) => cap,
        Err(_) => {
            tracing::warn!("register_build_module: no capability response from '{}'", cap_name);
            ModuleCapability {
                name: cap_name.to_string(),
                config_files: vec![],
                build_system: cap_name.to_string(),
                language: "".to_string(),
                source_extensions: vec![],
                supports_clean: false,
                supports_lint: false,
                supports_format: false,
                supports_fix: false,
                mcp_servers: vec![],
            }
        }
    };
    let _ = bm_tx
        .send(BuildManagerMessage::AddModule {
            capability: cap.clone(),
            module_tx,
        })
        .await;
    cap
}

fn init_actor_system() {
    if INITIALIZED.load(Ordering::Acquire) {
        return;
    }
    let mut guard = lock_state();
    if guard.is_some() {
        return;
    }

    init_tracing();
    tracing::info!("Spire FFI: startup (no project opened yet)");

    let runtime = tokio::runtime::Runtime::new().expect("tokio");

    // Create the embedding model OUTSIDE the tokio runtime. CandleEmbedder's
    // Hugging Face fallback uses hf-hub's blocking client (HFClientSync), which
    // spins up its own runtime and panics with "Cannot start a runtime from
    // within a runtime" when called from inside ours (observed at startup).
    let rag_embedder: std::sync::Arc<dyn Embedder> = match spire_core::embedder::CandleEmbedder::new() {
        Ok(e) => std::sync::Arc::new(e),
        Err(_) => std::sync::Arc::new(spire_core::embedder::NoopEmbedder) as std::sync::Arc<dyn Embedder>,
    };

    let default_rag_domain: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let (coord_tx, event_rx) = runtime.block_on(async {
        // Event broadcast channel: the file-watcher forwarder publishes
        // file-change events here; the UI consumes them via spire_wait_for_event.
        let (event_tx, event_rx) = tokio::sync::broadcast::channel::<String>(256);
        let system = ActorSystem::new();
        
        // ── Core actors ──
        let (chat_tx, _) = system.spawn(ChatActor::new());
        let (progress_tx, _) = system.spawn(ProgressActor::new());
        let (mcp_client_tx, _) = system.spawn(McpClientActor::with_progress(progress_tx.clone()));
        let (system_tx, _) = system.spawn(SystemActor::new());
        let (memory_graph_tx, _) = system.spawn(MemoryGraphActor::new());
        // The embedder is a shared service: registered once, resolved by any
        // actor that needs it (RAG, graph semantic search, future tools).
        let registry = system.registry().clone();
        let _ = registry.register_service(
            "embedder",
            std::sync::Arc::new(spire_core::actors::rag::EmbedderService(rag_embedder.clone())),
        );
        // ── KnowledgeStore: a SECOND SeleneDB instance at ~/.spire/knowledge ──
        // User-level and shared across projects. The RAG actor's data plane
        // (rag_domain/rag_source/rag_chunk) lives here; the project graph
        // (`memory_graph_tx`) only keeps provenance edges.
        let (knowledge_graph_tx, _) = system.spawn(MemoryGraphActor::new());
        {
            use spire_core::actors::MemoryGraphMessage as MgMsg;
            use spire_core::config::knowledge_dir;
            let dir = knowledge_dir();
            let _ = std::fs::create_dir_all(&dir);
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = knowledge_graph_tx.send(MgMsg::Initialize { data_dir: dir.clone(), reply_to: t }).await;
            if let Ok(Ok(())) = r.await {
                tracing::info!("KnowledgeStore initialized at {}", dir.display());
            } else {
                tracing::warn!("KnowledgeStore init failed at {}", dir.display());
            }
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = knowledge_graph_tx
                .send(MgMsg::InitializeEmbedder {
                    model_path: None,
                    embedder: Some(rag_embedder.clone()),
                    reply_to: t,
                })
                .await;
            let _ = r.await;
        }
        let _ = registry.register::<MemoryGraphMessage>("knowledge_graph", knowledge_graph_tx.clone());
        // RagActor data plane → KnowledgeStore; project store kept for provenance.
        let (rag_tx, _) = system.spawn(RagActor::from_registry(
            knowledge_graph_tx,
            memory_graph_tx.clone(),
            registry.clone(),
        ));
        let (intent_router_tx, _) = system.spawn(IntentRouterActor::new(memory_graph_tx.clone()));
        // ── LLM actor (used by ProjectCreation for plan/source generation) ──
        let mut llm_config = LlmConfig::default();
        let (llm_tx, _) = system.spawn(LlmActor::new(llm_config.clone()));

        // ── Register core services in the shared registry ──
        // Child actor systems look these up by name and cache the sender during init.
        let registry = system.registry().clone();
        let _ = registry.register::<RagMessage>("rag", rag_tx.clone());
        let _ = registry.register::<ChatMessage>("chat", chat_tx.clone());
        let _ = registry.register::<ProgressMessage>("progress", progress_tx.clone());
        let _ = registry.register::<McpClientMessage>("mcp_client", mcp_client_tx.clone());
        let _ = registry.register::<SystemMessage>("system", system_tx.clone());
        let _ = registry.register::<MemoryGraphMessage>("memory_graph", memory_graph_tx.clone());
        let _ = registry.register::<IntentRouterMessage>("intent_router", intent_router_tx.clone());
        let _ = registry.register::<LlmMessage>("llm", llm_tx.clone());

        // NOTE: MemoryGraph is initialized per-project via `project/open`
        // (it creates <root>/.spire/data and sends Initialize + InitializeEmbedder).

        // ── Load persisted global LLM config from ~/.spire/llm-config.json ──
        // Shared across all projects — independent of any project's graph.
        {
            llm_config = spire_core::config::load_global_llm_config();
            let (ltx, lrx) = tokio::sync::oneshot::channel();
            if llm_tx
                .send(LlmMessage::UpdateConfig {
                    config: llm_config.clone(),
                    reply_to: ltx,
                })
                .await
                .is_ok()
            {
                let _ = lrx.await;
            }
        }
        // ── Project actors ──
        let (project_sync_tx, _) = system.spawn(ProjectSyncActor::new());
        let (project_analyzer_tx, _) = system.spawn(ProjectAnalyzerActor::new());
        let (project_query_tx, _) = system.spawn(ProjectQueryActor::new());
        let (system_prompt_tx, _) = system.spawn(SystemPromptActor::new());

        // ── Static build modules + BuildManager ──
        // Spawn each module once at startup, query its capabilities, and
        // register it with the BuildManagerActor's router.
        let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(
            memory_graph_tx.clone(),
            BUILD_EVENT_BUFFER.clone(),
            BUILD_NOTIFY.clone(),
        ));
        // Attach the UI event broadcast sender so build operations can stream
        // per-line events (e.g. "Compiling serde") to the Swift event stream.
        let _ = bm_tx
            .send(BuildManagerMessage::SetEventTx {
                event_tx: event_tx.clone(),
            })
            .await;
        let _ = registry.register::<ProjectAnalyzerMessage>("project.analyzer", project_analyzer_tx.clone());
        let _ = registry.register::<ProjectQueryMessage>("project.query", project_query_tx.clone());
        let _ = registry.register::<ProjectSyncMessage>("project.sync", project_sync_tx.clone());
        let _ = registry.register::<BuildManagerMessage>("build.manager", bm_tx.clone());

        // Collect MCP server dependencies declared by each build module.
        let mut module_mcp_servers: Vec<ModuleCapability> = Vec::new();

        let cargo_module_tx = spawn_module(CargoBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_cargo", cargo_module_tx.clone());
        let cap = register_build_module("cargo", cargo_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let node_module_tx = spawn_module(NodeBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_node", node_module_tx.clone());
        let cap = register_build_module("node", node_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let swift_module_tx = spawn_module(SwiftBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_swift", swift_module_tx.clone());
        let cap = register_build_module("swift", swift_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let python_module_tx = spawn_module(PythonBuildModule::new());
        let _ = registry
            .register::<BuildModuleMessage>("build_module_python", python_module_tx.clone());
        let cap = register_build_module("python", python_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let go_module_tx = spawn_module(GoBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_go", go_module_tx.clone());
        let cap = register_build_module("go", go_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let maven_module_tx = spawn_module(MavenBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_maven", maven_module_tx.clone());
        let cap = register_build_module("maven", maven_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let gradle_module_tx = spawn_module(GradleBuildModule::new());
        let _ = registry
            .register::<BuildModuleMessage>("build_module_gradle", gradle_module_tx.clone());
        let cap = register_build_module("gradle", gradle_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let cmake_module_tx = spawn_module(CmakeBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_cmake", cmake_module_tx.clone());
        let cap = register_build_module("cmake", cmake_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let make_module_tx = spawn_module(MakeBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_make", make_module_tx.clone());
        let cap = register_build_module("make", make_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let meson_module_tx = spawn_module(MesonBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_meson", meson_module_tx.clone());
        let cap = register_build_module("meson", meson_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let ruby_module_tx = spawn_module(RubyBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_ruby", ruby_module_tx.clone());
        let cap = register_build_module("ruby", ruby_module_tx, &bm_tx).await;
        module_mcp_servers.push(cap);

        let _ = registry.register::<BuildManagerMessage>("build_manager", bm_tx.clone());

        // ── Provision MCP servers declared by build modules ──
        // Aggregate `mcp_servers` from all registered module capabilities and
        // register them with the MCP client. The McpClientActor will spawn each
        // server subprocess and expose its tools to the LLM.
        {
            use spire_core::subsystems::mcp::mcp_client::McpClientMessage as McpMsg;
            use spire_core::mcp::client::{McpServerConfig, TransportConfig};
            let mut seen: Vec<String> = Vec::new();
            let mut configs: Vec<McpServerConfig> = Vec::new();
            for cap in &module_mcp_servers {
                for dep in &cap.mcp_servers {
                    if seen.contains(&dep.name) {
                        continue;
                    }
                    seen.push(dep.name.clone());
                    let exe = if !dep.command.is_empty() {
                        dep.command.clone()
                    } else if !dep.package.is_empty() {
                        dep.package.clone()
                    } else {
                        continue;
                    };
                    configs.push(McpServerConfig {
                        name: dep.name.clone(),
                        transport: TransportConfig::Stdio {
                            command: exe,
                            args: dep.args.clone(),
                            env: Default::default(),
                        },
                        autostart: dep.autostart,
                        build_type: dep.build_type.clone(),
                    });
                }
            }
            for config in &configs {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = mcp_client_tx
                    .send(McpMsg::AddConfig {
                        config: config.clone(),
                        reply_to: t,
                    })
                    .await;
                let _ = r.await;
            }
            if !configs.is_empty() {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = mcp_client_tx
                    .send(McpMsg::ConnectAll { reply_to: t })
                    .await;
                let _ = r.await;
            }
        }

        let fs_tx = spawn_module(FilesystemModule::new());
        let _ = registry.register::<FilesystemMessage>("filesystem", fs_tx.clone());

        // ── ProjectCreationActor — scaffolds new projects / plans changes ──
        // Wire in the LLM for LLM-driven plan generation (with template fallback).
        let mut project_creation = ProjectCreationActor::new(
            fs_tx.clone(),
            bm_tx.clone(),
            mcp_client_tx.clone(),
        );
        // Only wire the LLM when an API key is configured — with an empty
        // default key, `LlmConfig::default()` would hang/fail on HTTP and
        // block plan generation. Without llm_tx, the template path is used.
        if !llm_config.api_key.is_empty() {
            project_creation.set_llm(llm_tx.clone());
        }
        // The memory graph is the system's single source of truth — always
        // available, so the AppSpec requirements pass can persist validated
        // specs as graph nodes (linked to their implementation later).
        project_creation.set_memory_graph(memory_graph_tx.clone());
        let (project_creation_tx, _pc_handle) = system.spawn(project_creation);
        let _ = registry
            .register::<ProjectCreationMessage>("project_creation", project_creation_tx.clone());

        // ── SpecDesignActor — the free-form AppSpec design session ────────
        // Summarize/Promote calls go to the same LLM (Planning role) the
        // requirements pass uses; Decide persists via the memory graph.
        let spec_design_llm: crate::subsystems::project::spec_design::LlmCall = {
            let llm_for_design = llm_tx.clone();
            Box::new(move |prompt: String| {
                let llm_tx = llm_for_design.clone();
                Box::pin(async move {
                    let (t, r) = tokio::sync::oneshot::channel();
                    if llm_tx
                        .send(LlmMessage::Complete {
                            prompt,
                            role: spire_core::subsystems::llm::llm::LlmModelRole::Planning,
                            reply_to: t,
                        })
                        .await
                        .is_err()
                    {
                        return Err("LLM actor unavailable".to_string());
                    }
                    match r.await {
                        Ok(Ok(text)) => Ok(text),
                        Ok(Err(e)) => Err(format!("LLM error: {e}")),
                        Err(e) => Err(format!("LLM reply lost: {e}")),
                    }
                })
            })
        };
        let mut spec_design = SpecDesignActor::new(spec_design_llm);
        spec_design.set_memory_graph(memory_graph_tx.clone());
        let (spec_design_tx, _sd_handle) = system.spawn(spec_design);
        let _ = registry.register::<SpecDesignMessage>("spec_design", spec_design_tx.clone());

        let git_tx = spawn_module(GitModule::new());
        let _ = registry.register::<GitMessage>("git", git_tx.clone());

        let process_tx = spawn_module(ProcessModule::new());
        let _ = registry.register::<ProcessMessage>("process", process_tx.clone());

        let search_tx = spawn_module(SearchModule::new());
        let _ = registry.register::<SearchMessage>("search", search_tx.clone());

        let terminal_tx = spawn_module(TerminalModule::new());
        let _ = registry.register::<TerminalMessage>("terminal", terminal_tx.clone());

        // ── Project meta-tool actors (project/build|test|lint|install) ──
        // Spawned once at startup. ProjectBuildActor's root is re-pointed on
        // every project/open via SetProjectRoot (the FFI opens projects
        // dynamically); the others route through ProjectQuery + BuildManager,
        // which are already initialized per-project. Registered in the registry
        // so the coordinator can update them and `tools/call` can dispatch.
        let (project_build_tx, _) = system.spawn(ProjectBuildActor::new(
            project_query_tx.clone(),
            mcp_client_tx.clone(),
            progress_tx.clone(),
            chat_tx.clone(),
            dummy_tx(), // transport_tx — tool events unused in FFI
            memory_graph_tx.clone(),
            bm_tx.clone(),
            std::path::PathBuf::new(), // root set per-project via SetProjectRoot
        ));
        let _ = registry.register::<ProjectBuildMessage>("project.build", project_build_tx.clone());
        let (project_test_tx, _) = system.spawn(ProjectTestActor::new(
            project_query_tx.clone(),
            mcp_client_tx.clone(),
            bm_tx.clone(),
        ));
        let _ = registry.register::<ProjectTestMessage>("project.test", project_test_tx.clone());
        let (project_lint_tx, _) = system.spawn(ProjectLintActor::new(
            project_query_tx.clone(),
            mcp_client_tx.clone(),
            bm_tx.clone(),
        ));
        let _ = registry.register::<ProjectLintMessage>("project.lint", project_lint_tx.clone());
        let (project_install_tx, _) = system.spawn(ProjectInstallActor::new(
            project_query_tx.clone(),
            mcp_client_tx.clone(),
            bm_tx.clone(),
        ));
        let _ = registry.register::<ProjectInstallMessage>("project.install", project_install_tx.clone());

        // ── Tool router + Tools ──
        // Routes tool calls: extension tools → transport; project/* → embedded;
        // filesystem_/git_/process_/search_/terminal_ → core modules;
        // build_ → BuildManager; catch-all → MCP client.
        // The project meta-tools (project/build|test|lint|install) are wired
        // with REAL actor channels above, so tools/call reaches them.
        let tool_registry = build_default_registry(
            dummy_tx(), // transport_tx — tool events unused in FFI
            project_query_tx.clone(),
            Some(project_build_tx.clone()),
            Some(project_test_tx.clone()),
            Some(project_lint_tx.clone()),
            Some(project_install_tx.clone()),
            fs_tx,
            git_tx,
            process_tx,
            search_tx,
            terminal_tx,
            bm_tx.clone(),
            rag_tx.clone(),
            default_rag_domain.clone(),
        )
        .await
        .expect("build tool registry");
        let (tool_router_tx, _) = system.spawn(ToolRouterActor::new(tool_registry, mcp_client_tx.clone()));
        let (tools_tx, _) = system.spawn(ToolsActor::new(tool_router_tx.clone()));

        // ── ToolOrchestrator (executes plan steps) ──
        // PlanOrchestrator dispatches each plan step via ToolOrchestrator.
        // The FFI previously wired a dummy channel with no receiver, which
        // made every step dispatch fail with "channel closed". Spawn a real
        // actor so plan steps can actually execute.
        let (tool_orchestrator_tx, _) = system.spawn(ToolOrchestrator::new(
            memory_graph_tx.clone(),
            dummy_tx(), // transport_tx — tool events unused in FFI
            mcp_client_tx.clone(),
            llm_tx.clone(),
            tool_router_tx.clone(),
        ));

        let (t, r) = tokio::sync::oneshot::channel();
        let pq_tx = project_query_tx.clone();
        let project_tool_caller: spire_core::actors::system_prompt::ProjectToolCaller =
            std::sync::Arc::new(move |tool: String, args: serde_json::Value| {
                let tx = pq_tx.clone();
                Box::pin(async move {
                    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                    if tx
                        .send(crate::subsystems::project::project_query::ProjectQueryMessage::CallTool {
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
            .send(SystemPromptMessage::Initialize {
                project_tool_caller,
                reply_to: t,
            })
            .await;
        let _ = r.await;

        // ── PlanOrchestrator (used by plan/create RPC) ──
        // Wire it with real channels so `plan/create` doesn't fall into the
        // "PlanOrchestrator not available" error branch. Step execution uses
        // the real ToolOrchestrator spawned above.
        let (plan_orchestrator_tx, _) = system.spawn(PlanOrchestrator::new(
            memory_graph_tx.clone(),
            llm_tx.clone(),
            tool_orchestrator_tx.clone(),
            chat_tx.clone(),
            dummy_tx(), // transport_tx — plan widget push is unused in FFI
        ));
        let _ = registry.register::<PlanOrchestratorMessage>("planning.orchestrator", plan_orchestrator_tx.clone());

        // ── Coordinator ──
        let (coord_tx, _) = system.spawn(CoordinatorActor::new(
            chat_tx.clone(),
            tools_tx,
            mcp_client_tx.clone(),
            llm_tx.clone(),
            system_tx,
            memory_graph_tx.clone(),
            project_query_tx.clone(),
            intent_router_tx,
            tool_router_tx,
            plan_orchestrator_tx, // real PlanOrchestrator channel
            dummy_tx(),           // transport_tx — tool events unused in FFI
        ));

        // ── Register internal spire tools ──
        {
            use rust_mcp_schema::{Tool, ToolInputSchema};
            use spire_core::subsystems::mcp::mcp_client::McpClientMessage as McpMsg;
            let internal_tools = vec![
                Tool {
                    name: "system/status".into(),
                    description: Some("Get system status".into()),
                    input_schema: ToolInputSchema::new(vec![], None, None),
                    annotations: None,
                    execution: None,
                    icons: vec![],
                    meta: None,
                    output_schema: None,
                    title: None,
                },
                Tool {
                    name: "chat/getActive".into(),
                    description: Some("Get active chat".into()),
                    input_schema: ToolInputSchema::new(vec![], None, None),
                    annotations: None,
                    execution: None,
                    icons: vec![],
                    meta: None,
                    output_schema: None,
                    title: None,
                },
                Tool {
                    name: "tools/list".into(),
                    description: Some("List all tools".into()),
                    input_schema: ToolInputSchema::new(vec![], None, None),
                    annotations: None,
                    execution: None,
                    icons: vec![],
                    meta: None,
                    output_schema: None,
                    title: None,
                },
            ];
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = mcp_client_tx
                .send(McpMsg::SetInternalTools {
                    tools: internal_tools,
                    reply_to: t,
                })
                .await;
            let _ = r.await;
        }

        // ── Initialize project actors + run analysis ──
        // Give MCP servers a moment to connect
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Init ProjectAnalyzer with MCP client channel
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = project_analyzer_tx
            .send(ProjectAnalyzerMessage::Initialize {
                mcp_client_tx: mcp_client_tx.clone(),
                reply_to: t,
            })
            .await;
        let _ = r.await;

        // Route build analysis through the in-process BuildManager.
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = project_analyzer_tx
            .send(ProjectAnalyzerMessage::SetBuildManager {
                build_manager_tx: bm_tx.clone(),
                reply_to: t,
            })
            .await;
        let _ = r.await;

        // NOTE: ProjectQuery + ProjectSync are bootstrapped per-project
        // inside `project/open`.

        // ── File watcher (Phase 3): watch for live FS changes ──
        // Wire the BuildManager sender into ProjectSync first so file events
        // can trigger AST (re)parses (G2/G3) and BuildSystem rebuilds (G4/G5).
        let _ = project_sync_tx
            .send(ProjectSyncMessage::SetBuildManager {
                build_manager_tx: bm_tx.clone(),
            })
            .await;

        // Spawn the FileWatcherActor (a plain Actor, not a ChildActor) and
        // bridge its debounced batches into ProjectSyncMessage::FileChanged.
        let (file_watcher_tx, _fw_handle) = system
            .spawn(spire_core::subsystems::tools::file_watcher::FileWatcherActor::new());
        let _ = registry.register::<spire_core::subsystems::tools::file_watcher::FileWatcherMessage>(
            "tools.watcher", file_watcher_tx.clone());
        let (watcher_out_tx, mut watcher_out_rx) =
            tokio::sync::mpsc::channel::<spire_core::subsystems::tools::file_watcher::FileChangeNotification>(16);
        let project_sync_tx_for_watcher = project_sync_tx.clone();
        let event_tx_for_events = event_tx.clone();
        tokio::spawn(async move {
            while let Some(notification) = watcher_out_rx.recv().await {
                use spire_core::subsystems::tools::file_watcher::FileChangeKind;
                use crate::subsystems::project::project_sync::ChangeType;
                let events: Vec<(ChangeType, String)> = match notification {
                    spire_core::subsystems::tools::file_watcher::FileChangeNotification::Batch { batch } => {
                        batch
                            .events
                            .iter()
                            .filter_map(|e| {
                                let ct = match e.kind {
                                    FileChangeKind::Create => ChangeType::Created,
                                    FileChangeKind::Modify => ChangeType::Modified,
                                    FileChangeKind::Remove => {
                                        ChangeType::Deleted
                                    }
                                    FileChangeKind::Rename
                                    | FileChangeKind::Other(_) => return None,
                                };
                                Some((ct, e.path.to_string_lossy().to_string()))
                            })
                            .collect()
                    }
                    // Bootstrap already populated the tree; the watcher's
                    // InitialScan is intentionally ignored to avoid duplicates.
                    spire_core::subsystems::tools::file_watcher::FileChangeNotification::InitialScan { .. } => {
                        Vec::new()
                    }
                };
                for (ct, path) in events {
                    // Publish to the UI event stream (consumed via spire_wait_for_event).
                    let kind = match ct {
                        ChangeType::Created => "created",
                        ChangeType::Modified => "modified",
                        ChangeType::Deleted => "deleted",
                    };
                    let payload =
                        serde_json::json!({ "kind": kind, "path": path }).to_string();
                    let _ = event_tx_for_events.send(payload);
                    let _ = project_sync_tx_for_watcher
                        .send(ProjectSyncMessage::FileChanged { change_type: ct, path })
                        .await;
                }
            }
        });

        // NOTE: StartWatching is deferred to `project/open` (per-project root).
        // Register in the registry for programmatic access.
        let _ = registry.register::<spire_core::subsystems::tools::file_watcher::FileWatcherMessage>(
            "file_watcher",
            file_watcher_tx.clone(),
        );

        // ── App-only dispatch deps for the coordinator ──
        // The FFI-inline RPC handlers (project/open, createProject/*, rag/*, …)
        // now live in the single CoordinatorActor router. Attach the shared
        // registry + state so those handlers can resolve actors and remember
        // the opened project / analysis / RAG domain. The standalone binary
        // never sends this — its extension flow uses the tools/ methods.
        let ffi_state = std::sync::Arc::new(FfiSharedState {
            project_root: std::sync::Mutex::new(None),
            analysis: std::sync::Mutex::new(None),
            default_rag_domain: default_rag_domain.clone(),
            watcher_out_tx: watcher_out_tx.clone(),
        });
        let _ = coord_tx
            .send(CoordinatorMessage::SetFfiDeps {
                registry: registry.clone(),
                state: ffi_state.clone(),
            })
            .await;

        (coord_tx, event_rx)
    });

    tracing::info!("Spire FFI: ready (analysis=unopened)");
    *guard = Some(AppState {
        coordinator_tx: coord_tx,
        event_rx: std::sync::Mutex::new(Some(event_rx)),
        runtime,
        build_event_buffer: BUILD_EVENT_BUFFER.clone(),
    });
    INITIALIZED.store(true, Ordering::Release);
}

/// Serialize a `ProjectAnalysis` into the Swift-expected JSON shape
/// (project name, root, languages, buildSystems, architecture, subprojects, fileTree).
/// Find a directory node in the file tree by its relative path ("" = root).
/// Populate first-class BuildTarget / Dependency / Platform nodes into the
/// graph for every BuildSystem discovered during analysis.
///
/// `project/open` bootstraps BuildSystem nodes with generic metadata, but the
/// rich metadata (targets/deps/platforms from BuildManager) is needed so
/// `project/getBuildTarget` can traverse the graph. This helper creates the
/// same node/edge shapes as `rebuild_build_systems` in project_sync:
///   BuildTarget  — subtype "BuildTarget", BelongsTo → BuildSystem
///   Dependency   — subtype "Dependency", BelongsTo → BuildSystem, DEPENDS_ON ← target
///   Platform     — subtype "Platform", BelongsTo → BuildSystem
pub(crate) async fn populate_target_graph(
    registry: &ServiceRegistry,
    build_systems: &[spire_core::analyzer::models::BuildMetadata],
) -> anyhow::Result<()> {
    use spire_core::models::memory_graph::{RelationshipInput, RelationshipType};

    let mg_tx = registry.get::<MemoryGraphMessage>("memory_graph").unwrap_or_else(dummy_tx).clone();

    // Map config_file → BuildSystem node id (from the bootstrap).
    let (t, r) = tokio::sync::oneshot::channel();
    let _ = mg_tx
        .send(MemoryGraphMessage::QueryAttrNodes {
            node_type: Some("Unknown".to_string()),
            subtype: Some("BuildSystem".to_string()),
            name: None,
            limit: None,
            reply_to: t,
        })
        .await;
    let bs_nodes = r.await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let mut bs_by_config: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for n in &bs_nodes {
        if let Some(cfg) = n.get("config_file").and_then(|v| v.as_str()) {
            bs_by_config.insert(cfg.to_string(), n.id().to_string());
        }
    }

    for meta in build_systems {
        let config_file = meta
            .config_files
            .first()
            .cloned()
            .unwrap_or_else(|| "meson.build".to_string());
        let Some(bs_id) = bs_by_config.get(&config_file).cloned() else { continue };

        // BuildTarget nodes
        let mut target_ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for tgt in &meta.targets {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = mg_tx
                .send(MemoryGraphMessage::StoreAttrNode {
                    node: ffi_attr_unknown(
                        Some("BuildTarget".to_string()),
                        format!("{}-{}", config_file.replace('/', "-"), tgt.name),
                        Some(format!("Build target {} ({:?})", tgt.name, tgt.kind)),
                        {
                            let mut m = std::collections::HashMap::new();
                            m.insert("name".to_string(), serde_json::json!(tgt.name));
                            m.insert("kind".to_string(),
                                serde_json::json!(tgt.kind.first().cloned().unwrap_or_default()));
                            m.insert("config_file".to_string(), serde_json::json!(config_file));
                            m
                        },
                    ),
                    reply_to: t,
                })
                .await;
            if let Ok(Ok(node)) = r.await {
                target_ids.insert(tgt.name.clone(), node.id().to_string());
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = mg_tx
                    .send(MemoryGraphMessage::CreateRelationship {
                        rel: RelationshipInput {
                            edge_type: RelationshipType::BelongsTo,
                            from_id: node.id().to_string(),
                            to_id: bs_id.clone(),
                            properties: None,
                            weight: None,
                        },
                        reply_to: t,
                    })
                    .await;
                let _ = r.await;
            }
        }

        // Dependency nodes (deduped)
        for dep in &meta.dependencies {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = mg_tx
                .send(MemoryGraphMessage::StoreAttrNode {
                    node: ffi_attr_unknown(
                        Some("Dependency".to_string()),
                        format!("dep-{}", dep.name.replace('/', "-")),
                        Some(format!("Dependency {}", dep.name)),
                        {
                            let mut m = std::collections::HashMap::new();
                            m.insert("name".to_string(), serde_json::json!(dep.name));
                            if let Some(v) = &dep.version_req {
                                m.insert("version".to_string(), serde_json::json!(v));
                            }
                            m
                        },
                    ),
                    reply_to: t,
                })
                .await;
            if let Ok(Ok(dep_node)) = r.await {
                // Dependency belongs to the BuildSystem
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = mg_tx
                    .send(MemoryGraphMessage::CreateRelationship {
                        rel: RelationshipInput {
                            edge_type: RelationshipType::BelongsTo,
                            from_id: dep_node.id().to_string(),
                            to_id: bs_id.clone(),
                            properties: None,
                            weight: None,
                        },
                        reply_to: t,
                    })
                    .await;
                let _ = r.await;
                // Every target DEPENDS_ON this dependency
                for (tgt_name, tgt_id) in &target_ids {
                    let (t, r) = tokio::sync::oneshot::channel();
                    let _ = mg_tx
                        .send(MemoryGraphMessage::CreateRelationship {
                            rel: RelationshipInput {
                                edge_type: RelationshipType::Custom("DEPENDS_ON".to_string()),
                                from_id: tgt_id.clone(),
                                to_id: dep_node.id().to_string(),
                                properties: None,
                                weight: None,
                            },
                            reply_to: t,
                        })
                        .await;
                    let _ = r.await;
                    let _ = tgt_name;
                }
            }
        }

        // Platform nodes
        for p in &meta.platform_targets {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = mg_tx
                .send(MemoryGraphMessage::StoreAttrNode {
                    node: ffi_attr_unknown(
                        Some("Platform".to_string()),
                        format!("platform-{}", p.replace('/', "-")),
                        Some(format!("Platform {}", p)),
                        {
                            let mut m = std::collections::HashMap::new();
                            m.insert("name".to_string(), serde_json::json!(p));
                            m
                        },
                    ),
                    reply_to: t,
                })
                .await;
            if let Ok(Ok(p_node)) = r.await {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = mg_tx
                    .send(MemoryGraphMessage::CreateRelationship {
                        rel: RelationshipInput {
                            edge_type: RelationshipType::BelongsTo,
                            from_id: p_node.id().to_string(),
                            to_id: bs_id.clone(),
                            properties: None,
                            weight: None,
                        },
                        reply_to: t,
                    })
                    .await;
                let _ = r.await;
            }
        }
    }
    Ok(())
}

/// Resolve the real project root when the user opens a WRAPPER folder.
///
/// When the chosen directory has no `Cargo.toml` of its own but contains
/// exactly one non-hidden subdirectory that does, Spire resolves to that
/// nested project root. This is the classic double-nesting artifact from
/// scaffolding `<name>` into a folder already named `<name>` (e.g.
/// `ai-traps-mcp/ai-traps-mcp`) and made every relative file path resolve to
/// a non-existent file ("Unable to read file").
pub(crate) fn resolve_project_root(root: &std::path::Path) -> std::path::PathBuf {
    let mut candidate = root.to_path_buf();
    loop {
        // If the candidate already contains a Cargo.toml at its own root,
        // it IS the project — stop descending.
        let has_own_build = std::fs::read_dir(&candidate).ok().map(|rd| {
            rd.flatten().any(|e| {
                e.path().is_file()
                    && e.file_name().to_string_lossy() == "Cargo.toml"
            })
        }).unwrap_or(false);
        if has_own_build {
            break;
        }
        // Exactly ONE non-hidden subdirectory? Descend (handles nested
        // wrappers, e.g. a→b→c where only c has the build config).
        let subdirs: Vec<std::path::PathBuf> = std::fs::read_dir(&candidate)
            .ok()
            .map(|rd| {
                rd.flatten()
                    .filter(|e| {
                        e.path().is_dir()
                            && !e.file_name().to_string_lossy().starts_with('.')
                    })
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        if subdirs.len() == 1 {
            candidate = subdirs.into_iter().next().unwrap();
            continue;
        }
        break;
    }
    if candidate != root {
        tracing::info!(
            "resolve_project_root: auto-descended {} → {}",
            root.display(),
            candidate.display()
        );
    }
    candidate
}

pub(crate) fn find_tree_dir<'a>(
    root: &'a spire_core::analyzer::models::DirectoryNode,
    path: &str,
) -> Option<&'a spire_core::analyzer::models::DirectoryNode> {
    if path.is_empty() {
        return Some(root);
    }
    let parts = path.trim_end_matches('/').split('/');
    let mut current = root;
    for part in parts {
        current = current.directories.iter().find(|d| d.name == part)?;
    }
    Some(current)
}

/// Recursively collect all files under a directory as Swift `FileEntry` JSON.
pub(crate) fn collect_tree_files(dir: &spire_core::analyzer::models::DirectoryNode, out: &mut Vec<serde_json::Value>) {
    for f in &dir.files {
        out.push(serde_json::json!({
            "path": f.path,
            "role": f.role,
            "sizeBytes": f.size,
            "language": f.language
        }));
    }
    for sub in &dir.directories {
        collect_tree_files(sub, out);
    }
}

pub(crate) fn serialize_analysis(analysis: &crate::subsystems::project::project_analyzer::ProjectAnalysis) -> serde_json::Value {
    let build_systems: Vec<String> = analysis
        .build_systems
        .iter()
        .map(|bs| bs.build_system.clone())
        .collect();
    let languages_json: serde_json::Value = analysis
        .languages
        .iter()
        .map(|l| (l.language.clone(), serde_json::json!(l.file_count)))
        .collect();
    // Build subproject list from build systems
    let mut subprojects: Vec<serde_json::Value> = analysis
        .build_systems
        .iter()
        .filter_map(|bs| {
            // The root of a Cargo WORKSPACE is not itself a subproject — it is
            // an aggregate whose members are expanded below into their own
            // entries (`core`, `rpi5`, `rock3c`). Without this skip, the root
            // BuildMetadata (which carries project_name=None for a workspace,
            // hence "unknown") would also appear as a subproject with path=""
            // and a file list containing EVERY file, cluttering the UI.
            let rel_path0 = bs
                .project_path
                .as_deref()
                .unwrap_or("")
                .trim_matches('/')
                .to_string();
            // A SpireApp root workspace is the project itself (its single
            // member crate is the app the root describes), so it stays as a
            // first-class subproject. Only the LEGACY multi-platform workspace
            // (core/rpi5/rock3c members) is an aggregate whose members are
            // expanded below instead.
            let is_spire_app =
                bs.structure == spire_core::build_types::ProjectStructure::SpireApp;
            if bs.is_workspace
                && !bs.workspace_members.is_empty()
                && rel_path0.is_empty()
                && !is_spire_app
            {
                return None;
            }
            // Use project_name or derive from project_path. For nested
            // configs (e.g. `rpi/hal/meson.build`) the top-level directory
            // name ("rpi") is the subproject's identity — not the leaf
            // ("hal") — so it matches what the user sees in the graph.
            let name = if is_spire_app && rel_path0.is_empty() {
                // The SpireApp subproject is the project: name it after it.
                if analysis.project_name.is_empty() {
                    "cargo".to_string()
                } else {
                    analysis.project_name.clone()
                }
            } else {
                bs.project_name
                    .clone()
                    .or_else(|| {
                        bs.project_path.as_ref().and_then(|p| {
                            let p = p.trim_matches('/');
                            if p.is_empty() {
                                None
                            } else {
                                Some(
                                    p.split('/')
                                        .next()
                                        .map(str::to_string)
                                        .unwrap_or_else(|| p.to_string()),
                                )
                            }
                        })
                    })
                    .unwrap_or_else(|| {
                        // Root configs without a name (e.g. a bare Makefile
                        // wrapper next to a Cargo workspace) get a label from
                        // their build system instead of the opaque "unknown".
                        match bs.build_system.as_str() {
                            "Cargo" => "cargo".to_string(),
                            "SwiftPM" | "Xcode" => "swift".to_string(),
                            "Make" => "make".to_string(),
                            other => other.to_lowercase(),
                        }
                    })
            };
            let lang = match bs.build_system.as_str() {
                "Cargo" => "Rust",
                "SwiftPM" | "Xcode" => "Swift",
                "npm" | "pnpm" | "yarn" => "JavaScript",
                _ => "Other",
            };
            // project_path is already RELATIVE to the scan root — BuildManager
            // and the MCP fallback both normalize it in ProjectAnalyzerActor
            // (e.g. "toolkit", "rpi/hal"). Use it directly: the UI groups
            // subprojects by the first path component.
            let rel_path: String = bs
                .project_path
                .as_deref()
                .unwrap_or("")
                .trim_matches('/')
                .to_string();
            let is_root = rel_path.is_empty();
            let kind = if is_root { "project" } else { "library" };
            let files_json: Vec<serde_json::Value> = {
                let sp_path = bs.project_path.as_deref().unwrap_or("");
                let mut files = Vec::new();
                if sp_path.is_empty() {
                    collect_tree_files(&analysis.file_tree, &mut files);
                } else if let Some(dir_node) = find_tree_dir(&analysis.file_tree, sp_path) {
                    collect_tree_files(dir_node, &mut files);
                }
                files
            };
            let targets_json: Vec<serde_json::Value> = bs
                .targets
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "kind": t.kind,
                        "platform": t.platform,
                        "sourceKind": t.source_kind,
                        "sourceUnits": t.source_units,
                        "dependencies": t.dependencies,
                    })
                })
                .collect();
            Some(serde_json::json!({
                "name": name,
                "kind": kind,
                "buildSystem": bs.build_system,
                "description": bs.description.clone().unwrap_or_else(|| "".to_string()),
                "path": rel_path,
                "language": lang,
                "platformTargets": bs.platform_targets,
                "structure": bs.structure,
                "domains": bs.domains,
                "buildTargets": targets_json,
                "dependencies": bs.dependencies.iter().map(|d| serde_json::json!({
                    "name": d.name.clone(),
                    "version": d.version_req.clone().unwrap_or_else(|| "".to_string())
                })).collect::<Vec<_>>(),
                "files": files_json
            }))
        })
        .collect();

    // Cargo WORKSPACE members become first-class subprojects. The scanner's
    // workspace de-dup (`is_cargo_workspace_member`) reports only the ROOT
    // workspace Cargo.toml as a build system, so without this expansion the
    // multi-platform scaffold (core/ + rpi5/ + rock3c/) would collapse into a
    // single root subproject — and every member crate's Cargo.toml would only
    // appear via the directory fallback below (empty buildSystem, no files).
    // Emit one Cargo subproject per member (name = member dir, path = member
    // dir, files from the file tree). These entries also mark the member
    // directories as "covered", so the directory fallback skips them.
    let mut member_subprojects: Vec<serde_json::Value> = Vec::new();
    for bs in &analysis.build_systems {
        // SpireApp roots are emitted directly above (they are the project), so
        // never re-expand their members into duplicate subprojects.
        if !bs.is_workspace
            || bs.workspace_members.is_empty()
            || bs.structure == spire_core::build_types::ProjectStructure::SpireApp
        {
            continue;
        }
        for member in &bs.workspace_members {
            let member_path = member.path.trim_matches('/').to_string();
            let member_name = if !member.name.trim().is_empty() {
                member.name.clone()
            } else {
                member_path
                    .split('/')
                    .next_back()
                    .map(str::to_string)
                    .unwrap_or_else(|| member_path.clone())
            };
            let mut files = Vec::new();
            let mut build_targets = Vec::new();
            if let Some(dir_node) = find_tree_dir(&analysis.file_tree, &member_path) {
                collect_tree_files(dir_node, &mut files);
            }
            // Attach the member's own sub-build-system metadata when the
            // analyzer populated it (e.g. targets from the member Cargo.toml).
            for sub_bs in &analysis.build_systems {
                let sub_path = sub_bs.project_path.as_deref().unwrap_or("").trim_matches('/');
                if sub_path == member_path {
                    build_targets = sub_bs
                        .targets
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "name": t.name,
                                "kind": t.kind,
                            })
                        })
                        .collect();
                    break;
                }
            }
            member_subprojects.push(serde_json::json!({
                "name": member_name,
                "kind": "library",
                "buildSystem": bs.build_system,
                "description": "",
                "path": member_path,
                "language": match bs.build_system.as_str() {
                    "Cargo" => "Rust",
                    _ => "Other",
                },
                "platformTargets": [],
                "buildTargets": build_targets,
                "dependencies": bs.dependencies.iter().map(|d| serde_json::json!({
                    "name": d.name.clone(),
                    "version": d.version_req.clone().unwrap_or_else(|| "".to_string())
                })).collect::<Vec<_>>(),
                "files": files
            }));
        }
    }
    subprojects.extend(member_subprojects);

    // Collect full directory paths already covered by subprojects, so nested
    // build configs (e.g. `platforms/radxa/rock-3c/hal`) are each kept as
    // their own subproject instead of collapsing to the first path segment.
    let covered_dirs: Vec<String> = subprojects
        .iter()
        .filter_map(|sp| sp.get("path").and_then(|p| p.as_str()))
        .map(|p| p.trim_matches('/').to_string())
        .collect();

    // If no build-system subprojects were detected but the project has content
    // (files + languages), synthesize a first-class "main project" subproject so
    // the UI shows the root project (files/dependencies) instead of nothing.
    if subprojects.is_empty() {
        let total_files: usize = analysis.file_tree.total_file_count;
        if total_files > 0 || !analysis.languages.is_empty() {
            let main_lang = analysis
                .languages
                .first()
                .map(|l| l.language.clone())
                .unwrap_or_else(|| "Unknown".to_string());
            subprojects.push(serde_json::json!({
                "name": analysis.project_name,
                "kind": "project",
                "buildSystem": "",
                "description": "Main project (no build config detected)",
                "path": "",
                "language": main_lang
            }));
        }
    }

    // Add top-level directories from the file tree that aren't themselves
    // build-config subprojects. Match by full path (already deduped above) so
    // intermediate directories are only listed once. If a build system was
    // found at the project root (path ""), the root project already covers
    // every top-level directory — so don't add spurious "directory" entries
    // that collide with real subprojects.
    let root_has_subproject = covered_dirs.iter().any(|p| p.is_empty());
    if !root_has_subproject {
        for dir in &analysis.file_tree.directories {
            let ancestor_covered = covered_dirs
                .iter()
                .any(|p| !p.is_empty() && p.starts_with(&dir.path));
            if !ancestor_covered
                && !covered_dirs.contains(&dir.path)
                && !covered_dirs.contains(&dir.name)
            {
                subprojects.push(serde_json::json!({
                    "name": dir.name,
                    "kind": "directory",
                    "buildSystem": "",
                    "path": dir.path,
                    "language": dir.role
                }));
            }
        }
    }
    // DIAGNOSTIC: log the FULL serialized subprojects array exactly as Swift
    // will decode it, plus per-subproject detail — so "no subprojects" is
    // localized to decode-vs-render from real bytes, not guesses.
    {
        // Gate the full-JSON dump behind debug: it can exceed 50 KB for Hal
        // projects (domains + per-target deps), and formatting/writing it sits
        // in the project-open path. Debug traces retain the diagnostic value.
        if tracing::enabled!(tracing::Level::DEBUG) {
            let json = serde_json::to_string(&subprojects).unwrap_or_default();
            tracing::debug!(
                "serialize_analysis: project_root={} subprojects_json(compact)={}",
                analysis.project_root,
                json
            );
        }
        for sp in &subprojects {
            let name = sp.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let path = sp.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let files: Vec<&str> = sp
                .get("files")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|f| f.get("path").and_then(|p| p.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            tracing::debug!(
                "serialize_analysis: subproject name={} path={} files={:?}",
                name,
                path,
                files
            );
        }
    }
    // Serialize file tree as a JSON value
    let file_tree_json = serde_json::to_value(&analysis.file_tree).unwrap_or(serde_json::json!({}));

    serde_json::json!({
        "name": analysis.project_name,
        "root": analysis.project_root,
        "languages": languages_json,
        "buildSystems": build_systems,
        "architecture": analysis.architecture_summary,
        "subprojects": subprojects,
        "fileTree": file_tree_json
    })
}

/// Parse a JSON-RPC request and dispatch it to the single CoordinatorActor
/// router. All method routing lives in `coordinator.rs` (including the app-only
/// `project/open`, `createProject/*`, `rag/*`, … handlers, which use the
/// dispatch deps attached via `CoordinatorMessage::SetFfiDeps`). This wrapper
/// only parses, instruments, and forwards — it never branches on the method.
fn process_json_request(request_json: &str) -> String {
    let guard = lock_state();
    if guard.is_none() {
        return r#"{"error":"Not initialized"}"#.to_string();
    }

    let parsed: serde_json::Value = match serde_json::from_str(request_json) {
        Ok(v) => v,
        Err(e) => return format!(r#"{{"error":"JSON: {}"}}"#, e),
    };
    let method = match parsed.get("method").and_then(|v| v.as_str()) {
        Some(m) => m.to_string(),
        None => return r#"{"error":"Missing method"}"#.to_string(),
    };
    // Fast-path RPC instrumentation: logs every request's method/tool at entry
    // so a long gap between "enter" and the next log localizes exactly which
    // request is hanging (e.g. the 2-minute "Opening project…"). The RAII guard
    // logs the elapsed time when this function returns via ANY path.
    let req_tool = parsed
        .get("params")
        .and_then(|p| p.get("tool"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    struct RpcGuard {
        name: String,
        tool: String,
        start: std::time::Instant,
    }
    impl Drop for RpcGuard {
        fn drop(&mut self) {
            tracing::info!(
                "[RPC] exit method={} tool={} elapsed_ms={}",
                self.name,
                self.tool,
                self.start.elapsed().as_millis()
            );
        }
    }
    tracing::info!(
        "[RPC] enter method={} tool={} len={}",
        method,
        req_tool,
        request_json.len()
    );
    let _rpc_guard = RpcGuard {
        name: method.clone(),
        tool: req_tool.to_string(),
        start: std::time::Instant::now(),
    };

    let params = parsed
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    // Release the STATE lock (acquired at the top of this function) before
    // blocking on the coordinator RPC. The build-event waiter
    // (spire_wait_for_build_event) also needs STATE to reach the shared
    // buffer; holding the lock across block_on deadlocked that waiter, so
    // all build/lint lines arrived in one batch at the end instead of
    // streaming incrementally.
    let (coord_tx, runtime_handle) = {
        let state = guard.as_ref().expect("state initialized");
        (state.coordinator_tx.clone(), state.runtime.handle().clone())
    };
    drop(guard); // STATE lock released — waiter can drain during the build

    let result: Result<String, String> = runtime_handle.block_on(async {
        let (t, r) = tokio::sync::oneshot::channel();
        coord_tx
            .send(CoordinatorMessage::HandleRequest {
                method,
                params,
                response_tx: t,
            })
            .await
            .map_err(|e| format!("Coord: {}", e))?;
        r.await
            .map_err(|e| format!("Resp: {}", e))
            .map(|v| serde_json::to_string(&v).unwrap_or_default())
    });
    match result {
        Ok(s) => s,
        Err(e) => format!(r#"{{"error":"{}"}}"#, e),
    }
}

#[no_mangle]
pub unsafe extern "C" fn spire_send_json(
    request_ptr: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    use std::panic;
    let result = panic::catch_unwind(|| {
        init_actor_system();
        let request = match unsafe { CStr::from_ptr(request_ptr) }.to_str() {
            Ok(s) => s.to_string(),
            Err(e) => {
                return CString::new(format!(r#"{{"error":"UTF-8: {}"}}"#, e))
                    .unwrap()
                    .into_raw()
            }
        };
        let response = process_json_request(&request);
        CString::new(response).unwrap().into_raw()
    });
    match result {
        Ok(ptr) => ptr,
        Err(info) => {
            let msg = info
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.downcast_ref::<String>().cloned())
                .unwrap_or("unknown".into());
            CString::new(format!(r#"{{"error":"Panic: {}"}}"#, msg))
                .unwrap()
                .into_raw()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn spire_wait_for_event(timeout_ms: u32) -> *mut std::ffi::c_char {
    init_actor_system();
    let timeout = std::time::Duration::from_millis(timeout_ms as u64);

    // Extract the receiver + a runtime handle under the lock, THEN drop the
    // lock before blocking. Holding STATE while blocked deadlocks any RPC
    // (e.g. project/open) that needs the same lock.
    let (mut receiver, runtime_handle) = {
        let guard = lock_state();
        let state = match guard.as_ref() { Some(s) => s, None => return std::ptr::null_mut() };
        let mut rx_guard = state.event_rx.lock().unwrap();
        let rx = match rx_guard.take() { Some(rx) => rx, None => return std::ptr::null_mut() };
        (rx, state.runtime.handle().clone())
    }; // STATE lock dropped here

    let payload = runtime_handle.block_on(async {
        match tokio::time::timeout(timeout, receiver.recv()).await { Ok(Ok(m)) => Some(m), _ => None }
    });

    // Put the receiver back for the next call (re-acquire the lock briefly).
    {
        let guard = lock_state();
        let state = match guard.as_ref() { Some(s) => s, None => return std::ptr::null_mut() };
        *state.event_rx.lock().unwrap() = Some(receiver);
    }

    match payload { Some(m) => CString::new(m).unwrap().into_raw(), None => std::ptr::null_mut() }
}


#[no_mangle]
pub unsafe extern "C" fn spire_drain_build_events() -> *mut std::ffi::c_char {
    init_actor_system();
    // Lock and drain the shared buffer in place — no message sent to the actor,
    // so events can be read WHILE a build is still running.
    let drained: Vec<serde_json::Value> = {
        let guard = lock_state();
        let state = match guard.as_ref() { Some(s) => s, None => return std::ptr::null_mut() };
        let mut locked = state.build_event_buffer.lock().unwrap();
        std::mem::take(&mut *locked)
    };
    let payload = serde_json::json!(drained).to_string();
    if payload.is_empty() {
        std::ptr::null_mut()
    } else {
        CString::new(payload).unwrap().into_raw()
    }
}

/// Wait for a build event to be pushed (blocking until BUILD_NOTIFY fires or
/// timeout). Returns a JSON array of drained events, or null on timeout.
/// This is an async push — no polling/timer on the Swift side.
#[no_mangle]
pub unsafe extern "C" fn spire_wait_for_build_event(timeout_ms: u32) -> *mut std::ffi::c_char {
    init_actor_system();
    let timeout = std::time::Duration::from_millis(timeout_ms as u64);
    let notify = BUILD_NOTIFY.clone();
    let runtime = { lock_state().as_ref().map(|s| s.runtime.handle().clone()) };
    if runtime.is_none() {
        return std::ptr::null_mut();
    }
    let runtime = runtime.unwrap();

    // Drain-first-then-wait loop. Checking the buffer BEFORE waiting means we
    // can never miss a wakeup: burst notifications coalesce (Notify keeps one
    // permit), and any events that arrived while we weren't waiting are caught
    // on the next iteration.
    let payload = runtime.block_on(async {
        loop {
            // 1. Drain any events already buffered.
            let drained = {
                let guard = lock_state();
                let state = match guard.as_ref() { Some(s) => s, None => return String::new() };
                let mut locked = state.build_event_buffer.lock().unwrap();
                std::mem::take(&mut *locked)
            };
            if !drained.is_empty() {
                tracing::info!("spire_wait_for_build_event: drained {} events", drained.len());
                return serde_json::json!(drained).to_string();
            }
            // 2. Nothing buffered — wait for a notification (or timeout).
            tokio::select! {
                _ = notify.notified() => {
                    // Woken; loop drains whatever accumulated.
                }
                _ = tokio::time::sleep(timeout) => {
                    tracing::info!("spire_wait_for_build_event: timeout after {}ms", timeout_ms);
                    return String::new();
                }
            }
        }
    });

    if payload.is_empty() {
        std::ptr::null_mut()
    } else {
        CString::new(payload).unwrap().into_raw()
    }
}
#[no_mangle]
pub unsafe extern "C" fn spire_free_string(ptr: *mut std::ffi::c_char) {
    if !ptr.is_null() {
        unsafe {
            let _ = CString::from_raw(ptr);
        }
    }
}

#[cfg(test)]
mod serialize_analysis_tests {
    use super::serialize_analysis;
    use crate::subsystems::project::project_analyzer::{
        LanguageBreakdown, ProjectAnalysis, RoleBreakdown,
    };
    use spire_core::analyzer::models::DirectoryNode;
    use spire_core::build_types::{
        BuildMetadata, DomainEditability, ProjectDomain, ProjectStructure, WorkspaceMember,
    };

    fn meta(
        build_system: &str,
        name: Option<&str>,
        path: &str,
        is_workspace: bool,
        members: Vec<WorkspaceMember>,
        structure: ProjectStructure,
    ) -> BuildMetadata {
        BuildMetadata {
            project_name: name.map(str::to_string),
            build_system: build_system.to_string(),
            project_path: Some(path.to_string()),
            is_workspace,
            workspace_members: members,
            structure,
            ..Default::default()
        }
    }

    fn domain(id: &str) -> ProjectDomain {
        ProjectDomain {
            id: id.to_string(),
            name: id.to_string(),
            kind: "common".to_string(),
            files: Vec::new(),
            dependencies: Vec::new(),
            build_spec: None,
            editability: DomainEditability::Fillable,
            contracts: Vec::new(),
        }
    }

    fn spire_gis_analysis() -> ProjectAnalysis {
        let mut cargo = meta(
            "Cargo",
            None,
            "",
            true,
            vec![WorkspaceMember {
                name: "spire-gis".to_string(),
                path: "crates/spire-gis".to_string(),
                version: None,
            }],
            ProjectStructure::SpireApp,
        );
        cargo.domains = vec![domain("core"), domain("ui")];
        ProjectAnalysis {
            project_root: "/tmp/spire-gis".to_string(),
            project_name: "spire-gis".to_string(),
            file_tree: DirectoryNode::default(),
            build_systems: vec![
                cargo,
                meta("Make", None, "", false, vec![], ProjectStructure::Native),
                meta(
                    "SwiftPM",
                    Some("swift"),
                    "ui/swift",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
            ],
            languages: vec![LanguageBreakdown {
                language: "Rust".to_string(),
                file_count: 1,
                line_estimate: 1,
            }],
            directory_roles: vec![],
            file_roles: vec![RoleBreakdown {
                role: "entry".to_string(),
                count: 1,
            }],
            entry_points: vec![],
            architecture_summary: String::new(),
            total_files: 1,
            total_dirs: 0,
            total_lines: 1,
        }
    }

    /// The SpireApp root is the project itself: it must appear as ONE Cargo
    /// subproject (named after the project, with its structure + domains),
    /// alongside the Swift and Make subprojects — never dropped, never
    /// duplicated by the workspace-member expansion.
    #[test]
    fn spire_app_root_is_one_cargo_subproject_not_unknown() {
        let json = serialize_analysis(&spire_gis_analysis());
        let sp = json
            .get("subprojects")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let labels: Vec<String> = sp
            .iter()
            .map(|s| {
                format!(
                    "{}[{}]",
                    s.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                    s.get("buildSystem").and_then(|v| v.as_str()).unwrap_or("")
                )
            })
            .collect();
        eprintln!("subprojects = {labels:?}");

        assert_eq!(sp.len(), 3, "expected 3 subprojects: {labels:?}");

        // The Cargo subproject is the project root, named after the project,
        // carrying the SpireApp structure + core/ui domains.
        let cargo = sp
            .iter()
            .find(|s| s.get("buildSystem").and_then(|v| v.as_str()) == Some("Cargo"))
            .expect("Cargo subproject present");
        assert_eq!(
            cargo.get("name").and_then(|v| v.as_str()),
            Some("spire-gis")
        );
        assert_eq!(
            cargo.get("structure").and_then(|v| v.as_str()),
            Some("spire_app")
        );
        let domains = cargo.get("domains").and_then(|v| v.as_array()).unwrap();
        let domain_ids: Vec<&str> = domains
            .iter()
            .filter_map(|d| d.get("id").and_then(|v| v.as_str()))
            .collect();
        assert!(domain_ids.contains(&"core"));
        assert!(domain_ids.contains(&"ui"));

        // Swift and Make subprojects, neither labelled "unknown".
        let names: Vec<&str> = sp
            .iter()
            .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"swift"), "swift missing: {names:?}");
        assert!(names.contains(&"make"), "make missing: {names:?}");
        assert!(!names.contains(&"unknown"), "no unknown label: {names:?}");
    }

    /// A legacy multi-platform Cargo workspace (platform member crates) must
    /// still expand its members and NOT emit the root — unchanged behaviour.
    #[test]
    fn legacy_cargo_workspace_expands_members() {
        let analysis = ProjectAnalysis {
            project_name: "embedded".to_string(),
            build_systems: vec![meta(
                "Cargo",
                None,
                "",
                true,
                vec![
                    WorkspaceMember {
                        name: "core".to_string(),
                        path: "core".to_string(),
                        version: None,
                    },
                    WorkspaceMember {
                        name: "rpi5".to_string(),
                        path: "rpi5".to_string(),
                        version: None,
                    },
                ],
                ProjectStructure::Native,
            )],
            ..spire_gis_analysis()
        };
        let json = serialize_analysis(&analysis);
        let sp = json
            .get("subprojects")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let names: Vec<&str> = sp
            .iter()
            .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"core"), "member core missing: {names:?}");
        assert!(names.contains(&"rpi5"), "member rpi5 missing: {names:?}");
    }
}

