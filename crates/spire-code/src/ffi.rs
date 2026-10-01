// spire-ffi — C FFI bridge for Swift UI to call the Rust core.

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;

use crate::actors::tool_providers::build_default_registry;
use crate::actors::{
    CoordinatorActor, CoordinatorMessage, FfiSharedState, IntentRouterActor, IntentRouterMessage,
    ProjectAnalyzerActor, ProjectAnalyzerMessage, ProjectQueryActor, ProjectQueryMessage,
    ProjectSyncActor, ProjectSyncMessage, SystemActor, SystemMessage,
};
use crate::subsystems::build::build_manager::{
    BuildEventLogActor, BuildEventLogMessage, BuildManagerActor, BuildManagerMessage,
};
use crate::subsystems::planning::plan_orchestrator::PlanOrchestrator;
use crate::subsystems::planning::plan_orchestrator::PlanOrchestratorMessage;
use crate::subsystems::project::project_build::{ProjectBuildActor, ProjectBuildMessage};
use crate::subsystems::project::project_creation::{ProjectCreationActor, ProjectCreationMessage};
use crate::subsystems::project::project_install::{ProjectInstallActor, ProjectInstallMessage};
use crate::subsystems::project::project_lint::{ProjectLintActor, ProjectLintMessage};
use crate::subsystems::project::project_test::{ProjectTestActor, ProjectTestMessage};
use crate::{
    BuildModuleMessage, CargoBuildModule, CmakeBuildModule, GoBuildModule, GradleBuildModule,
    IdfBuildModule, MakeBuildModule, MavenBuildModule, MesonBuildModule, ModuleCapability,
    NodeBuildModule, PythonBuildModule, RubyBuildModule, SwiftBuildModule,
};
use spire_core::actors::rag::{RagActor, RagMessage};
use spire_core::actors::tool_providers::ToolRouterActor;
use spire_core::actors::{
    ActorSystem, ChatActor, ChatMessage, LlmActor, LlmConfig, LlmMessage, McpClientActor,
    McpClientMessage, MemoryGraphActor, MemoryGraphMessage, ProgressActor, ProgressMessage,
    SystemPromptActor, SystemPromptMessage, ToolsActor,
};
use spire_core::build_types::ProjectStructure;
use spire_core::models::embedding::Embedder;
use spire_core::modules::{
    FilesystemMessage, FilesystemModule, GitMessage, GitModule, ProcessMessage, ProcessModule,
    SearchMessage, SearchModule, TerminalMessage, TerminalModule,
};
use spire_core::subsystems::tools::tool_orchestrator::ToolOrchestrator;

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
    /// Actor-owned build-event log, drained by the FFI via `Drain` messages.
    /// (Replaces the old static `Arc<Mutex<Vec>>` buffer shared with the
    /// BuildManager's streaming forwarders.)
    build_event_log: tokio::sync::mpsc::Sender<BuildEventLogMessage>,
    /// Wakeup for `spire_wait_for_build_event` (signalled by the log actor).
    build_notify: std::sync::Arc<tokio::sync::Notify>,
}

unsafe impl Send for AppState {}
unsafe impl Sync for AppState {}

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static STATE: Lazy<Mutex<Option<AppState>>> = Lazy::new(|| Mutex::new(None));

/// Lock the global STATE mutex without panicking on a poisoned lock.
///
/// A panic while one thread holds STATE (e.g. inside a `block_on`) poisons the
/// std mutex. Calling `.unwrap()` in every other FFI entry point would then
/// panic with `PoisonError` and take down the whole UI process (SIGABRT).
/// Recover the guard from the poison instead so the app keeps running.
fn lock_state() -> std::sync::MutexGuard<'static, Option<AppState>> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    // Honor `RUST_LOG`, like the standalone binary, so this crate's `debug!`
    // traces (including the full `serialize_analysis` subproject dump) can be
    // turned on when the dylib is loaded by the app. Without it the filter is
    // fixed at `info` and every debug trace is unreachable. The default stays
    // `info`, matching the previous behavior.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info").add_directive(
                "rust_mcp_sdk::mcp_runtimes::client_runtime=off"
                    .parse()
                    .expect("valid filter directive"),
            )
        }))
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
}

/// Query a build module's capability via DescribeCapabilities.
///
/// A module that does not answer gets a **negative** capability naming it: every operation the
/// manager gates on a `supports_*` flag is then refused with a real message, instead of the
/// registration failing and the module quietly becoming unroutable.
async fn describe_module(
    cap_name: &str,
    module_tx: &tokio::sync::mpsc::Sender<BuildModuleMessage>,
) -> ModuleCapability {
    let (t, r) = tokio::sync::oneshot::channel();
    let _ = module_tx
        .send(BuildModuleMessage::DescribeCapabilities { reply_to: t })
        .await;
    match r.await {
        Ok(cap) => cap,
        Err(_) => {
            tracing::warn!("describe_module: no capability response from '{cap_name}'");
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
                supports_flash: false,
                mcp_servers: vec![],
            }
        }
    }
}

/// Register a build module with the BuildManager and return its capability (so MCP
/// server deps can be collected). Plain async fn (no closure lifetime issues).
async fn register_build_module(
    cap_name: &str,
    module_tx: tokio::sync::mpsc::Sender<BuildModuleMessage>,
    bm_tx: &tokio::sync::mpsc::Sender<BuildManagerMessage>,
) -> ModuleCapability {
    let cap = describe_module(cap_name, &module_tx).await;
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

    // Name the application before anything resolves a config path — the library
    // cannot know it, and the config dir is scoped by it — then adopt a
    // pre-scope `~/.spire/*` layout into that scope, once.
    //
    // Both must precede `init_tracing`, which is what first *creates* a
    // directory under the scope: the adoption moves entries by `rename` and
    // skips any whose destination already exists.
    spire_core::config::set_app_name(env!("CARGO_PKG_NAME"));
    let adopted = spire_core::config::migrate_legacy_layout();

    init_tracing();
    tracing::info!("Spire FFI: startup (no project opened yet)");
    if !adopted.is_empty() {
        tracing::info!(
            "Spire FFI: adopted pre-scope config into {}: {:?}",
            spire_core::config::config_dir().display(),
            adopted
        );
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio");

    // Create the embedding model OUTSIDE the tokio runtime. CandleEmbedder's
    // Hugging Face fallback uses hf-hub's blocking client (HFClientSync), which
    // spins up its own runtime and panics with "Cannot start a runtime from
    // within a runtime" when called from inside ours (observed at startup).
    let rag_embedder: std::sync::Arc<dyn Embedder> =
        match spire_core::embedder::CandleEmbedder::new() {
            Ok(e) => std::sync::Arc::new(e),
            Err(_) => std::sync::Arc::new(spire_core::embedder::NoopEmbedder)
                as std::sync::Arc<dyn Embedder>,
        };

    let build_notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let (coord_tx, event_rx, build_event_log_tx) = runtime.block_on(async {
        // Event broadcast channel: the file-watcher forwarder publishes
        // file-change events here; the UI consumes them via spire_wait_for_event.
        let (event_tx, event_rx) = tokio::sync::broadcast::channel::<String>(256);
        let system = std::sync::Arc::new(ActorSystem::new());

        // ── Core actors ──
        let (chat_tx, _) = system.spawn(ChatActor::new());
        let (progress_tx, _) = system.spawn(ProgressActor::new());
        let (mcp_client_tx, _) = system.spawn(McpClientActor::with_progress(progress_tx.clone()));
        let (system_tx, _) = system.spawn(SystemActor::new());
        // Host actor system + self sender for the SystemActor's startup phases
        // (it spawns managed StartupTask children only when Initialize runs).
        let system_self_tx = system_tx.clone();
        let _ = system_self_tx
            .send(SystemMessage::SetSystemTx {
                system_tx: system_self_tx.clone(),
            })
            .await;
        let _ = system_self_tx
            .send(SystemMessage::SetActorSystem {
                system: system.clone(),
            })
            .await;
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

        // ── Seed the platform + capability graph ──
        // The registry (boards + chips) is user-level, so it belongs in the shared knowledge store
        // rather than a project graph. The CLI runs this through `PlatformBootstrapPhase`; the FFI
        // never sends `SystemMessage::Initialize` (the phase chain waits for `project/open`), so
        // without this step the app would never write the platform or capability nodes and edges —
        // and the capability seeder would be invisible here. `platform_seed_payload` flattens each
        // entry's capability blocks into the shape `spire-core`'s seeder consumes.
        {
            use spire_core::actors::MemoryGraphMessage as MgMsg;

            let platforms: Vec<serde_json::Value> = crate::platform::Platform::load_registry()
                .unwrap_or_default()
                .iter()
                .map(crate::actors::platform_codec::platform_seed_payload)
                .collect();
            let count = platforms.len();
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = knowledge_graph_tx
                .send(MgMsg::BootstrapPlatforms {
                    platforms,
                    reply_to: t,
                })
                .await;
            match r.await {
                Ok(Ok(())) => tracing::info!(
                    "KnowledgeStore: platform + capability graph seeded ({count} entries)"
                ),
                Ok(Err(e)) => tracing::warn!("KnowledgeStore: platform seed failed: {e}"),
                Err(e) => tracing::warn!("KnowledgeStore: platform seed response error: {e}"),
            }

            // Read it back and cache it in-process, the way `PlatformBootstrapPhase` does: the graph
            // is the registry for the rest of the process, and reading it back is what proves the
            // nodes the writer stored are legible to the reader.
            let (t, r) = tokio::sync::oneshot::channel();
            if knowledge_graph_tx
                .send(MgMsg::GetPlatforms { reply_to: t })
                .await
                .is_ok()
            {
                if let Ok(Ok(nodes)) = r.await {
                    let cached: Vec<crate::platform::Platform> = nodes
                        .iter()
                        .filter_map(crate::actors::platform_codec::platform_json_to_spire)
                        .collect();
                    let cached_count = cached.len();
                    crate::platform::set_registry(cached);
                    tracing::info!(
                        "KnowledgeStore: platform registry cached from graph ({cached_count})"
                    );
                }
            }
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
        let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(memory_graph_tx.clone()));
        // Wire the LLM actor so the HAL Stage-1 tools (`hal_generate_impl` /
        // `hal_generate_impl_plan`) can invoke it. The standalone binary does the
        // same (`SystemMessage::SetLlm` in main.rs); without this the
        // BuildManager's `llm_tx` stays None and the app reports a misleading
        // "LLM unavailable" even when a key IS set in Settings.
        let _ = bm_tx
            .send(BuildManagerMessage::SetLlm {
                llm_tx: llm_tx.clone(),
            })
            .await;
        // Attach the UI event broadcast sender so build operations can stream
        // per-line events (e.g. "Compiling serde") to the Swift event stream.
        let _ = bm_tx
            .send(BuildManagerMessage::SetEventTx {
                event_tx: event_tx.clone(),
            })
            .await;
        // Actor-owned build-event log (replaces the static Arc<Mutex<Vec>> buffer
        // the forwarders + FFI shared). Streaming forwarders push here; the FFI
        // drains it via messages.
        let (build_event_log_tx, _build_event_log_handle) =
            system.spawn(BuildEventLogActor::new(build_notify.clone()));
        let _ = bm_tx
            .send(BuildManagerMessage::SetEventLog {
                event_log_tx: build_event_log_tx.clone(),
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
        // Pushed where it is produced, like every other module's below. It used to be pushed after
        // the esp block instead (which pushed cargo's servers twice and the platform module's not
        // at all) — the ordering hid it, because the two registrations are adjacent.
        module_mcp_servers.push(cap);
        // The ESP-IDF module registers BY PLATFORM, not by config file. An ESP-IDF project is
        // *also* a CMake project, so claiming `CMakeLists.txt` would replace the cmake module in the
        // router and send every CMake project in Spire down the ESP-IDF path. Declaring
        // `os: "esp-idf"` routes on what actually differs — the invocation, `idf.py`.
        let idf_module_tx = spawn_module(IdfBuildModule::new());
        let _ = registry.register::<BuildModuleMessage>("build_module_idf", idf_module_tx.clone());
        // Its capability travels with the registration: it is what tells the manager whether a
        // `build_flash` request may be routed here at all.
        let idf_cap = describe_module("esp-idf", &idf_module_tx).await;
        let _ = bm_tx
            .send(BuildManagerMessage::AddPlatformModule {
                os: "esp-idf".to_string(),
                capability: idf_cap.clone(),
                module_tx: idf_module_tx,
            })
            .await;
        // The esp capability, alongside the module it belongs to.
        module_mcp_servers.push(idf_cap);


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
            watcher_out_tx: watcher_out_tx.clone(),
        });
        let _ = coord_tx
            .send(CoordinatorMessage::SetFfiDeps {
                registry: registry.clone(),
                state: ffi_state.clone(),
            })
            .await;

        (coord_tx, event_rx, build_event_log_tx)
    });

    tracing::info!("Spire FFI: ready (analysis=unopened)");
    *guard = Some(AppState {
        coordinator_tx: coord_tx,
        event_rx: std::sync::Mutex::new(Some(event_rx)),
        runtime,
        build_event_log: build_event_log_tx,
        build_notify,
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

    let mg_tx = registry
        .get::<MemoryGraphMessage>("memory_graph")
        .unwrap_or_else(dummy_tx)
        .clone();

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
    let mut bs_by_config: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
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
        let Some(bs_id) = bs_by_config.get(&config_file).cloned() else {
            continue;
        };

        // BuildTarget nodes
        let mut target_ids: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
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
                            m.insert(
                                "kind".to_string(),
                                serde_json::json!(tgt.kind.first().cloned().unwrap_or_default()),
                            );
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

/// The files that say "this directory **is** the project root, not a wrapper around one".
///
/// Mirrors what the build modules register as their own `config_files`, because "is this directory
/// a project?" and "which module claims it?" should not be two different answers. `Cargo.toml`
/// alone was the original answer, and it is what made an ESP-IDF project open as `main/`: a
/// component library's root holds one non-hidden subdirectory — `main/`, the build harness — and
/// a check that only knew `Cargo.toml` descended straight into it.
const PROJECT_ROOT_MARKERS: &[&str] = &[
    "Cargo.toml",
    "CMakeLists.txt",
    "sdkconfig.defaults",
    "meson.build",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "Makefile",
    "pom.xml",
    "build.gradle",
    "Gemfile",
    "Package.swift",
];

/// Resolve the real project root when the user opens a WRAPPER folder.
///
/// When the chosen directory has no build file of its own but contains
/// exactly one non-hidden subdirectory that does, Spire resolves to that
/// nested project root. This is the classic double-nesting artifact from
/// scaffolding `<name>` into a folder already named `<name>` (e.g.
/// `ai-traps-mcp/ai-traps-mcp`) and made every relative file path resolve to
/// a non-existent file ("Unable to read file").
///
/// A **wrapper** is a directory with no build file and one subdirectory. A directory with a build
/// file is a project, whatever the build system is — see [`PROJECT_ROOT_MARKERS`].
pub(crate) fn resolve_project_root(root: &std::path::Path) -> std::path::PathBuf {
    let mut candidate = root.to_path_buf();
    loop {
        // If the candidate already contains a build file at its own root,
        // it IS the project — stop descending.
        let has_own_build = std::fs::read_dir(&candidate)
            .ok()
            .map(|rd| {
                rd.flatten().any(|e| {
                    e.path().is_file()
                        && PROJECT_ROOT_MARKERS
                            .iter()
                            .any(|m| e.file_name().to_string_lossy() == *m)
                })
            })
            .unwrap_or(false);
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
                        e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.')
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
pub(crate) fn collect_tree_files(
    dir: &spire_core::analyzer::models::DirectoryNode,
    out: &mut Vec<serde_json::Value>,
) {
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

/// A label for a build config that has no name of its own, from the build system that found it.
///
/// The label is the tool a reader would recognise, lowercased — which is not always just
/// `to_lowercase()`: SwiftPM is `swift`, because nobody calls it "swiftpm" out loud.
fn build_system_label(build_system: &str) -> String {
    match build_system {
        "Cargo" => "cargo".to_string(),
        "SwiftPM" | "Xcode" => "swift".to_string(),
        "Make" => "make".to_string(),
        other => other.to_lowercase(),
    }
}

pub(crate) fn serialize_analysis(
    analysis: &crate::subsystems::project::project_analyzer::ProjectAnalysis,
) -> serde_json::Value {
    // The *systems*, in the order the analysis found them — not the build files. An ESP-IDF project
    // has two `CMakeLists.txt` (its own, and a component's), so the raw list reads `CMake · CMake`;
    // and this array is used as a `ForEach` id in the UI, where a duplicate is a dropped row.
    let mut build_systems: Vec<String> = Vec::new();
    for bs in &analysis.build_systems {
        if !build_systems.contains(&bs.build_system) {
            build_systems.push(bs.build_system.clone());
        }
    }
    let languages_json: serde_json::Value = analysis
        .languages
        .iter()
        .map(|l| (l.language.clone(), serde_json::json!(l.file_count)))
        .collect();
    // A **component library**'s `main/` is the build harness, not a subproject.
    //
    // It exists so that `idf.py build` compiles the components above it, and it starts nothing,
    // wires nothing and names no device — a library that grows an application in there is a library
    // nobody can use twice. Listing it makes a fresh library look like it already has one, which is
    // the boundary the type exists to hold.
    //
    // An *application*'s `main/` is the opposite: it is the product, and it stays. Decided by the
    // root's own declared structure, so a directory called `main` in any other project is untouched.
    let idf_library = analysis
        .build_systems
        .iter()
        .any(|bs| bs.structure == spire_core::build_types::ProjectStructure::IdfLibrary);
    // Either ESP-IDF type has the SAME `components/<name>/` layout, so "is this a component?" is asked
    // of both. `idf_library` stays for the one place the two differ — the library's `main/` harness.
    let idf_project = analysis.build_systems.iter().any(|bs| {
        matches!(
            bs.structure,
            spire_core::build_types::ProjectStructure::IdfLibrary
                | spire_core::build_types::ProjectStructure::IdfApplication
        )
    });

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
            // The library's build harness — see `idf_library` above.
            if idf_library && rel_path0 == "main" {
                return None;
            }
            // A file **inside** a component — its `test/CMakeLists.txt`, most often — is part of that
            // component rather than a project of its own: ESP-IDF's unit is the component directory,
            // and what a harness does is the component's business. Skipped for the same reason the
            // library's `main/` is, with a second one of its own: a nested file is named by its leaf,
            // so two components that each have a `test/` produce two subprojects called `test` — one
            // id, two rows, and a click that selects both.
            let under_components = rel_path0.strip_prefix("components/");
            if idf_project && under_components.is_some_and(|name| name.contains('/')) {
                return None;
            }
            // A **component** of a component library or an application: a directory directly under
            // `components/`. For a library it is the product, and the one thing in the tree a user
            // adds, edits and removes; for an application it is the composition's own units. Either
            // way it says what it is rather than the generic "library", and it is named by its own
            // directory rather than by the wrapper it sits in (`components`).
            let is_component = idf_project && under_components.is_some();
            // A SpireApp root workspace is the project itself (its single
            // member crate is the app the root describes), so it stays as a
            // first-class subproject. Only the LEGACY multi-platform workspace
            // (core/rpi5/rock3c members) is an aggregate whose members are
            // expanded below instead.
            let is_spire_app = bs.structure == spire_core::build_types::ProjectStructure::SpireApp;
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
            //
            // The root config of a project that *is* its root — a SpireApp monorepo, or either
            // ESP-IDF type — is named after the project. The generic fallback below exists for the
            // opposite case: a root config sitting beside the real project (a bare Makefile next to
            // a Cargo workspace), where the build system is the only label there is. A library whose
            // only subproject is called "cmake" reads as a project with something else in it.
            let root_is_the_project = rel_path0.is_empty()
                && matches!(
                    bs.structure,
                    ProjectStructure::SpireApp
                        | ProjectStructure::IdfLibrary
                        | ProjectStructure::IdfApplication
                );
            let name = if is_component {
                // A component is called by its own directory: `components/sps30` is `sps30`, not
                // `components`.
                rel_path0
                    .rsplit('/')
                    .next()
                    .unwrap_or(rel_path0.as_str())
                    .to_string()
            } else if root_is_the_project {
                // The root subproject is the project: name it after it.
                if analysis.project_name.is_empty() {
                    build_system_label(&bs.build_system)
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
                        build_system_label(&bs.build_system)
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
            let kind = if is_root {
                "project"
            } else if is_component {
                "component"
            } else {
                "library"
            };
            // **What the component is.** Read from the component's own `CMakeLists.txt`, where it is
            // stated, and read here rather than in Swift because it is a fact about the component and a
            // second reading could only disagree with the first. `null` for an entry that is not a
            // component, and for one that states nothing (written by hand) — the UI then shows no kind
            // at all, which is what it is.
            //
            // Beside it, **whose** component it is: a framework component is shipped with every
            // library and cannot be written or removed, which is a different thing from a generated
            // stub however alike the two look. The tool knows the three names (`FRAMEWORK_COMPONENTS`),
            // so the UI does not have to keep a second list that could drift from it.
            let (component_kind, component_framework) = if is_component {
                let component = rel_path0.rsplit('/').next();
                let kind = component
                    .and_then(|name| {
                        crate::build::idf_projects::component_kind(
                            std::path::Path::new(&analysis.project_root),
                            name,
                        )
                    })
                    .map(|kind| kind.as_str());
                let framework = component
                    .filter(|name| crate::build::idf_projects::is_framework_component(name))
                    .map(|name| name.to_string());
                (kind, framework)
            } else {
                (None, None)
            };
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
                "componentKind": component_kind,
                // Which framework component this is, when it is one — `null` for everything else.
                "componentFramework": component_framework,
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
                let sub_path = sub_bs
                    .project_path
                    .as_deref()
                    .unwrap_or("")
                    .trim_matches('/');
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
                // The **workspace's** structure, inherited. A container's member crates are part of
                // the container, and a member that reported nothing here decoded as `native` — which
                // is how the container's growth actions came to appear only when the *workspace* was
                // selected and vanish the moment its crate was, since the UI gates them on this.
                "structure": bs.structure,
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
        // INFO, not debug: `structure` is the key the UI gates per-shape action
        // surfaces on (the container's board/driver actions, say), and the app's
        // tracing filter defaults to `info` (see `init_tracing`) — so a
        // debug-only line could never be seen where the gate actually matters.
        // The file LIST is deliberately not logged (it reaches ~50 KB for Hal
        // projects); only its count is.
        for sp in &subprojects {
            let name = sp.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let path = sp.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let kind = sp.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let structure = sp
                .get("structure")
                .and_then(|v| v.as_str())
                .unwrap_or("<none>");
            let file_count = sp
                .get("files")
                .and_then(|v| v.as_array())
                .map(|arr| arr.len())
                .unwrap_or(0);
            tracing::info!(
                "serialize_analysis: subproject name={} path={} kind={} structure={} files={}",
                name,
                path,
                kind,
                structure,
                file_count
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

/// Send a JSON-RPC request to the Spire core and return the JSON reply.
///
/// # Safety
///
/// `request_ptr` must be a non-null pointer to a NUL-terminated UTF-8 C string
/// that stays valid for the duration of the call. The returned pointer is a
/// Rust-allocated C string that the caller must release with
/// [`spire_free_string`].
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

/// Block until a file-watcher event arrives or `timeout_ms` elapses.
///
/// # Safety
///
/// Takes no pointers; `unsafe` only as a C ABI entry point. The returned pointer
/// is a Rust-allocated C string that the caller must release with
/// [`spire_free_string`], or null when the timeout expires.
#[no_mangle]
pub unsafe extern "C" fn spire_wait_for_event(timeout_ms: u32) -> *mut std::ffi::c_char {
    init_actor_system();
    let timeout = std::time::Duration::from_millis(timeout_ms as u64);

    // Extract the receiver + a runtime handle under the lock, THEN drop the
    // lock before blocking. Holding STATE while blocked deadlocks any RPC
    // (e.g. project/open) that needs the same lock.
    let (mut receiver, runtime_handle) = {
        let guard = lock_state();
        let state = match guard.as_ref() {
            Some(s) => s,
            None => return std::ptr::null_mut(),
        };
        let mut rx_guard = state.event_rx.lock().unwrap();
        let rx = match rx_guard.take() {
            Some(rx) => rx,
            None => return std::ptr::null_mut(),
        };
        (rx, state.runtime.handle().clone())
    }; // STATE lock dropped here

    let payload = runtime_handle.block_on(async {
        match tokio::time::timeout(timeout, receiver.recv()).await {
            Ok(Ok(m)) => Some(m),
            _ => None,
        }
    });

    // Put the receiver back for the next call (re-acquire the lock briefly).
    {
        let guard = lock_state();
        let state = match guard.as_ref() {
            Some(s) => s,
            None => return std::ptr::null_mut(),
        };
        *state.event_rx.lock().unwrap() = Some(receiver);
    }

    match payload {
        Some(m) => CString::new(m).unwrap().into_raw(),
        None => std::ptr::null_mut(),
    }
}

/// Drain the accumulated build events without blocking.
///
/// # Safety
///
/// Takes no pointers; `unsafe` only as a C ABI entry point. The returned pointer
/// is a Rust-allocated C string that the caller must release with
/// [`spire_free_string`], or null when nothing is buffered.
#[no_mangle]
pub unsafe extern "C" fn spire_drain_build_events() -> *mut std::ffi::c_char {
    init_actor_system();
    // Drain the actor-owned build-event log: clone the sender + a runtime
    // handle under the lock, then drop the lock before blocking.
    let payload = {
        let (event_log_tx, runtime) = {
            let guard = lock_state();
            let state = match guard.as_ref() {
                Some(s) => s,
                None => return std::ptr::null_mut(),
            };
            (
                state.build_event_log.clone(),
                state.runtime.handle().clone(),
            )
        };
        runtime.block_on(async move {
            let (t, r) = tokio::sync::oneshot::channel();
            if event_log_tx
                .send(BuildEventLogMessage::Drain { reply_to: t })
                .await
                .is_err()
            {
                return String::new();
            }
            let drained = r.await.unwrap_or_default();
            if drained.is_empty() {
                String::new()
            } else {
                serde_json::json!(drained).to_string()
            }
        })
    };
    if payload.is_empty() {
        std::ptr::null_mut()
    } else {
        CString::new(payload).unwrap().into_raw()
    }
}

/// Wait for a build event (blocking until the log actor notifies or timeout).
/// Returns a JSON array of drained events, or null on timeout. Async push — no
/// polling/timer on the Swift side. The buffer itself lives in the
/// BuildEventLogActor; the FFI holds only its sender + a Notify.
///
/// # Safety
///
/// Takes no pointers; `unsafe` only as a C ABI entry point. The returned pointer
/// is a Rust-allocated C string that the caller must release with
/// [`spire_free_string`], or null when the timeout expires.
#[no_mangle]
pub unsafe extern "C" fn spire_wait_for_build_event(timeout_ms: u32) -> *mut std::ffi::c_char {
    init_actor_system();
    let timeout = std::time::Duration::from_millis(timeout_ms as u64);

    // Clone the sender, notify, and a runtime handle under the lock, then drop
    // the lock before blocking (holding STATE while blocked deadlocks RPCs).
    let (event_log_tx, notify, runtime) = {
        let guard = lock_state();
        let state = match guard.as_ref() {
            Some(s) => s,
            None => return std::ptr::null_mut(),
        };
        (
            state.build_event_log.clone(),
            state.build_notify.clone(),
            state.runtime.handle().clone(),
        )
    };

    // Drain-first-then-wait loop: check the log BEFORE waiting so a wakeup is
    // never missed (Notify coalesces bursts into one permit; anything that
    // arrived while we were not waiting is caught on the next iteration).
    let payload = runtime.block_on(async move {
        loop {
            // 1. Drain any events already logged.
            let (t, r) = tokio::sync::oneshot::channel();
            if event_log_tx
                .send(BuildEventLogMessage::Drain { reply_to: t })
                .await
                .is_err()
            {
                return String::new();
            }
            let drained = r.await.unwrap_or_default();
            if !drained.is_empty() {
                tracing::info!(
                    "spire_wait_for_build_event: drained {} events",
                    drained.len()
                );
                return serde_json::json!(drained).to_string();
            }
            // 2. Nothing buffered — wait for the log actor's notification.
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
/// The core's resolved per-application config directory, e.g. `~/.spire/spire-code`.
///
/// For the host to place its own files beside the core's — the recent-projects
/// list and the scaffold log — so both sides agree on the scope rather than each
/// re-deriving it. Does **not** initialise the actor system; it does adopt a
/// pre-scope `~/.spire/*` layout first (idempotently), because the host asks
/// this before its first call and expects the answer to already hold its files.
///
/// # Safety
///
/// Takes no pointers; `unsafe` only as a C ABI entry point. The returned pointer
/// is a Rust-allocated C string that the caller must release with
/// [`spire_free_string`].
#[no_mangle]
pub unsafe extern "C" fn spire_config_dir() -> *mut std::ffi::c_char {
    // Name the application here too: this may be the host's very first call,
    // before `init_actor_system` has run.
    spire_core::config::set_app_name(env!("CARGO_PKG_NAME"));
    // Adopt a pre-scope `~/.spire/*` layout before answering, so the directory
    // handed back already holds the adopted files. Idempotent.
    spire_core::config::migrate_legacy_layout();
    let dir = spire_core::config::config_dir();
    CString::new(dir.to_string_lossy().as_bytes())
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Free a C string previously returned by one of the `spire_*` entry points.
///
/// # Safety
///
/// `ptr` must be either null or a pointer previously returned by
/// `spire_send_json` / `spire_wait_for_event` / `spire_drain_build_events` /
/// `spire_wait_for_build_event` / `spire_config_dir` and not yet freed. Passing
/// any other pointer, or freeing the same pointer twice, is undefined behaviour.
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

    /// **A container's member crates inherit the workspace's structure.**
    ///
    /// They reported nothing here once, so they decoded as `native` — and the UI, which gates a
    /// container's board/driver actions on this value, showed them for the *workspace* and hid them
    /// the moment its crate was selected, which is the natural thing to click. Reported by hand as
    /// "the actions are inconsistent".
    ///
    /// Inherited rather than re-derived: the member crates of a container are part of it, and that is
    /// the fact an action surface has to key on.
    #[test]
    fn container_members_inherit_the_workspace_structure() {
        let analysis = ProjectAnalysis {
            project_name: "weather-embedded".to_string(),
            build_systems: vec![meta(
                "Cargo",
                Some("weather-embedded"),
                "",
                true,
                vec![WorkspaceMember {
                    name: "weather-embedded".to_string(),
                    path: "crates/weather-embedded".to_string(),
                    version: None,
                }],
                ProjectStructure::Embedded,
            )],
            ..spire_gis_analysis()
        };

        let json = serialize_analysis(&analysis);
        let sp = json
            .get("subprojects")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let member = sp
            .iter()
            .find(|s| s.get("path").and_then(|v| v.as_str()) == Some("crates/weather-embedded"))
            .unwrap_or_else(|| panic!("the member crate is a subproject: {sp:?}"));
        assert_eq!(
            member.get("structure").and_then(|v| v.as_str()),
            Some("embedded"),
            "a container's crate carries the container's structure: {member}"
        );
    }

    /// A component library's `main/` is the build harness, and it is not a subproject.
    ///
    /// Pinned because the harness exists only so `idf.py build` has something to compile, and
    /// listing it makes a fresh library look like it already contains an application — the one thing
    /// the library type is for.
    #[test]
    fn a_librarys_build_harness_is_not_a_subproject() {
        let analysis = ProjectAnalysis {
            project_root: "/tmp/spire-idf".to_string(),
            project_name: "spire-idf".to_string(),
            build_systems: vec![
                meta(
                    "CMake",
                    None,
                    "",
                    false,
                    vec![],
                    ProjectStructure::IdfLibrary,
                ),
                // The harness, analysed like any other `CMakeLists.txt` under the root — which is
                // how it ends up in the list at all.
                meta(
                    "CMake",
                    Some("main"),
                    "main",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
            ],
            ..spire_gis_analysis()
        };

        let json = serialize_analysis(&analysis);
        let paths = subproject_paths(&json);
        assert!(
            !paths.contains(&"main".to_string()),
            "the harness is not a subproject: {paths:?}"
        );
        assert!(
            paths.contains(&String::new()),
            "the project itself still is: {paths:?}"
        );
        // …and it is called by its name, not by the name of its build system.
        let root_name = json
            .get("subprojects")
            .and_then(serde_json::Value::as_array)
            .and_then(|sp| {
                sp.iter()
                    .find(|s| s.get("path").and_then(|v| v.as_str()) == Some(""))
                    .and_then(|s| s.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        assert_eq!(root_name.as_deref(), Some("spire-idf"), "{json}");
    }

    /// A **component's kind** — and whether it is one of the shipped **framework** components — reaches
    /// the UI on the keys the UI reads.
    ///
    /// `componentKind` and `componentFramework` are joins between two languages: `serialize_analysis`
    /// writes them here and `SubprojectInfo` reads those exact spellings there, where a miss decodes as
    /// `nil` — and `nil` is a *meaningful* value on the Swift side ("this component states no kind";
    /// "this component is the library's own work"). So a drift would present as a component spire-code
    /// itself created being shown as one written by hand, or as the framework being offered a Write
    /// button it cannot honour, rather than as an error anywhere.
    ///
    /// The pair is the point of the test: `actors` and `moving_average` state the **same kind**
    /// (`library`), so the kind alone cannot tell a shipped framework component from a generated stub —
    /// which is exactly the mistake this key exists to prevent. The kind is read from the component's
    /// own `CMakeLists.txt`, so this test writes those files the way the emitter writes them.
    #[test]
    fn a_components_kind_travels_on_the_key_the_ui_reads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (name, kind) in [
            ("sps30", "driver"),
            ("moving_average", "library"),
            // Deliberately the same kind as `moving_average`: a framework component is a library, and
            // only `componentFramework` says which of the two it is.
            ("actors", "library"),
        ] {
            let component = root.join("components").join(name);
            std::fs::create_dir_all(&component).unwrap();
            std::fs::write(
                component.join("CMakeLists.txt"),
                format!(
                    "set(SPIRE_COMPONENT_KIND {kind})\n\
                     idf_component_register(SRCS \"src/{name}.cpp\" INCLUDE_DIRS \"include\")\n"
                ),
            )
            .unwrap();
        }
        // …and one that states no kind, which is what a hand-written component looks like.
        let handwritten = root.join("components/handwritten");
        std::fs::create_dir_all(&handwritten).unwrap();
        std::fs::write(
            handwritten.join("CMakeLists.txt"),
            "idf_component_register(SRCS \"src/x.cpp\" INCLUDE_DIRS \"include\")\n",
        )
        .unwrap();
        let analysis = ProjectAnalysis {
            project_root: root.display().to_string(),
            project_name: "sensors".to_string(),
            build_systems: vec![
                meta(
                    "CMake",
                    None,
                    "",
                    false,
                    vec![],
                    ProjectStructure::IdfLibrary,
                ),
                meta(
                    "CMake",
                    Some("sps30"),
                    "components/sps30",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
                meta(
                    "CMake",
                    Some("moving_average"),
                    "components/moving_average",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
                meta(
                    "CMake",
                    Some("handwritten"),
                    "components/handwritten",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
                meta(
                    "CMake",
                    Some("actors"),
                    "components/actors",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
            ],
            ..spire_gis_analysis()
        };

        let json = serialize_analysis(&analysis);
        let sp = json
            .get("subprojects")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let key_of = |path: &str, key: &str| {
            sp.iter()
                .find(|s| s.get("path").and_then(|v| v.as_str()) == Some(path))
                .and_then(|s| s.get(key))
                .cloned()
        };
        let kind_of = |path: &str| key_of(path, "componentKind");
        let framework_of = |path: &str| key_of(path, "componentFramework");

        assert_eq!(
            kind_of("components/sps30"),
            Some(serde_json::json!("driver")),
            "{json}"
        );
        assert_eq!(
            kind_of("components/moving_average"),
            Some(serde_json::json!("library")),
            "{json}"
        );
        // A component that states nothing is `null`, not a default: the sheet shows no kind at all.
        assert_eq!(
            kind_of("components/handwritten"),
            Some(serde_json::Value::Null),
            "{json}"
        );
        // The key is always present — `null` for an entry that is not a component and for one that
        // states nothing — so the Swift side has one shape to decode rather than two absences.
        assert_eq!(kind_of(""), Some(serde_json::Value::Null), "{json}");

        // **And whose component it is.** `actors` is a framework component; the others are the
        // library's own work. The two halves matter equally: a false positive would lock a component
        // the user added out of its own Write button.
        assert_eq!(
            framework_of("components/actors"),
            Some(serde_json::json!("actors")),
            "{json}"
        );
        assert_eq!(
            kind_of("components/actors"),
            Some(serde_json::json!("library")),
            "a framework component is still a library — which is why the kind alone cannot say this"
        );
        for path in [
            "components/sps30",
            "components/moving_average",
            "components/handwritten",
        ] {
            assert_eq!(
                framework_of(path),
                Some(serde_json::Value::Null),
                "{path} is the library's own work, not the framework: {json}"
            );
        }
        assert_eq!(framework_of(""), Some(serde_json::Value::Null), "{json}");
    }
    /// A component's own `test/` harness is **not** a subproject.
    ///
    /// Every component ships one (`components/<name>/test/CMakeLists.txt`), and a nested file is named
    /// by its leaf — so two components that each have a harness were two subprojects called `test`, and
    /// because the UI identifies its rows by that name they were **one row** to anyone clicking them:
    /// selecting one highlighted both. The unit is the component directory; what lives inside it is the
    /// component's business. (`ramen` and `actors` both ship a harness, which is why a fresh library
    /// showed exactly this pair.)
    #[test]
    fn a_components_test_harness_is_not_a_subproject() {
        let analysis = ProjectAnalysis {
            project_root: "/tmp/sensors".to_string(),
            project_name: "sensors".to_string(),
            build_systems: vec![
                meta(
                    "CMake",
                    None,
                    "",
                    false,
                    vec![],
                    ProjectStructure::IdfLibrary,
                ),
                meta(
                    "CMake",
                    Some("ramen"),
                    "components/ramen",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
                meta(
                    "CMake",
                    Some("ramen"),
                    "components/ramen/test",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
                meta(
                    "CMake",
                    Some("actors"),
                    "components/actors",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
                meta(
                    "CMake",
                    Some("actors"),
                    "components/actors/test",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
            ],
            ..spire_gis_analysis()
        };

        let json = serialize_analysis(&analysis);
        let subprojects = json
            .get("subprojects")
            .and_then(serde_json::Value::as_array)
            .expect("subprojects");
        let paths: Vec<&str> = subprojects
            .iter()
            .filter_map(|s| s.get("path").and_then(|v| v.as_str()))
            .collect();
        let names: Vec<&str> = subprojects
            .iter()
            .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
            .collect();

        // The components are there…
        assert!(paths.contains(&"components/ramen"), "{paths:?}");
        assert!(paths.contains(&"components/actors"), "{paths:?}");
        // …and nothing inside one is a project of its own.
        assert!(
            !paths.iter().any(|p| p.matches('/').count() > 1),
            "a component's own files are not subprojects: {paths:?}"
        );
        // The symptom, stated directly: no row called `test` at all — and so nothing for the UI to
        // give one id to.
        assert!(!names.contains(&"test"), "{names:?}");
    }

    /// An application's `main/` is its product. Same directory, opposite meaning — and the
    /// difference is the root's own declared structure, not the name of the directory.
    #[test]
    fn an_applications_main_stays_a_subproject() {
        let analysis = ProjectAnalysis {
            project_root: "/tmp/pm25-meter".to_string(),
            project_name: "pm25-meter".to_string(),
            build_systems: vec![
                meta(
                    "CMake",
                    None,
                    "",
                    false,
                    vec![],
                    ProjectStructure::IdfApplication,
                ),
                meta(
                    "CMake",
                    Some("main"),
                    "main",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
            ],
            ..spire_gis_analysis()
        };

        let json = serialize_analysis(&analysis);
        assert!(
            subproject_paths(&json).contains(&"main".to_string()),
            "an application's main is its product, not a harness: {json}"
        );
    }

    /// An **application's** `components/<name>/` directories are components too — named by their own
    /// directory rather than by the `components/` wrapper.
    ///
    /// The library was the first to grow component rows, and the check that produced them asked the
    /// library's structure alone. An application shares the layout but not that flag, so every one of
    /// its components fell through to the generic branch and took its name from the first path segment
    /// — `components/air_quality` read as `components`, and the whole composition was one repeated row
    /// in the left pane. The unit is the directory, whichever ESP-IDF project it sits in.
    #[test]
    fn an_applications_components_are_named_by_their_directory() {
        let analysis = ProjectAnalysis {
            project_root: "/tmp/pm25-meter".to_string(),
            project_name: "pm25-meter".to_string(),
            build_systems: vec![
                meta(
                    "CMake",
                    None,
                    "",
                    false,
                    vec![],
                    ProjectStructure::IdfApplication,
                ),
                meta(
                    "CMake",
                    Some("air_quality"),
                    "components/air_quality",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
                meta(
                    "CMake",
                    Some("messages"),
                    "components/messages",
                    false,
                    vec![],
                    ProjectStructure::Native,
                ),
            ],
            ..spire_gis_analysis()
        };

        let json = serialize_analysis(&analysis);
        let sp = json
            .get("subprojects")
            .and_then(serde_json::Value::as_array)
            .expect("subprojects");
        let named = |path: &str| {
            sp.iter()
                .find(|s| s.get("path").and_then(|v| v.as_str()) == Some(path))
                .map(|s| {
                    (
                        s.get("name").and_then(|v| v.as_str()).map(str::to_string),
                        s.get("kind").and_then(|v| v.as_str()).map(str::to_string),
                    )
                })
        };
        for name in ["air_quality", "messages"] {
            let path = format!("components/{name}");
            assert_eq!(
                named(&path),
                Some((Some(name.to_string()), Some("component".to_string()))),
                "an application's component is named by its directory: {json}"
            );
        }
        // The symptom, stated directly: nothing in the tree is called `components`.
        let names: Vec<&str> = sp
            .iter()
            .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(!names.contains(&"components"), "{names:?}");
    }

    fn subproject_paths(json: &serde_json::Value) -> Vec<String> {
        json.get("subprojects")
            .and_then(serde_json::Value::as_array)
            .map(|sp| {
                sp.iter()
                    .filter_map(|s| s.get("path").and_then(|v| v.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The descent decides what the project *is*: it becomes `ProjectAnalysis.project_root`, the project
/// name is that directory's own name, and every relative file path is resolved against it. Descend
/// one level too far and the tree, the name and the paths are all wrong together.
#[cfg(test)]
mod resolve_project_root_tests {
    use super::resolve_project_root;

    /// A directory with a build file of its own is the project — **whatever** the build system is.
    ///
    /// Pinned because an ESP-IDF component library's root holds exactly one non-hidden
    /// subdirectory, `main/`, which is its build harness. A check that only knew `Cargo.toml`
    /// descended straight into it, and `spire-idf` opened as a project called `main` whose only
    /// `CMakeLists.txt` registered a harness.
    #[test]
    fn a_project_with_one_component_does_not_descend_into_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("spire-idf");
        std::fs::create_dir_all(root.join("main")).unwrap();
        std::fs::write(root.join("CMakeLists.txt"), "project(spire-idf)\n").unwrap();
        std::fs::write(root.join("sdkconfig.defaults"), "").unwrap();
        std::fs::write(root.join("main").join("CMakeLists.txt"), "").unwrap();

        assert_eq!(resolve_project_root(&root), root);
    }

    /// The case the check was written for, still working: a Cargo project resolves to itself.
    #[test]
    fn a_cargo_project_resolves_to_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("spire-gis");
        std::fs::create_dir_all(root.join("crates")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();

        assert_eq!(resolve_project_root(&root), root);
    }

    /// …and a **wrapper** — no build file of its own, one subdirectory — still descends into the
    /// project inside it. This is the double-nesting artifact the descent exists for.
    #[test]
    fn a_wrapper_still_descends_to_the_project_inside_it() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("ai-traps-mcp");
        let inner = outer.join("ai-traps-mcp");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("Cargo.toml"), "[package]\n").unwrap();

        assert_eq!(resolve_project_root(&outer), inner);
    }
}
