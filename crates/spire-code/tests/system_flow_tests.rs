// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! System-level tests: the **real** actors, wired the way the app wires them.
//!
//! The flow unit tests prove each flow's *decisions*; `modify_code_llm_tests` proves one
//! flow's plumbing with the model replaced. These go a layer further: the knowledge graph,
//! the build manager and the tool registry are the real ones, spawned as `ffi.rs` spawns
//! them. So a failure here means the *wiring* is wrong — a tool that is not registered, a
//! request that reaches a mock — rather than a flow's logic.

mod common;

use common::{fake_llm, mock_sender};
use spire_actor::registry::ServiceRegistry;
use spire_actor::{Actor, ActorSystem};
use spire_code::actors::{
    build_default_registry, BuildManagerActor, BuildManagerMessage, ChatActor, CoordinatorActor,
    CoordinatorMessage, FfiSharedState, LlmActor, LlmConfig, McpClientActor, ProjectAnalyzerActor,
    ProjectAnalyzerMessage, ProjectQueryMessage, SystemActor, ToolRouterActor, ToolsActor,
};
use spire_code::build::{BuildModuleMessage, MesonBuildModule};
use spire_core::subsystems::graph::memory_graph::{MemoryGraphActor, MemoryGraphMessage};
use tokio::sync::mpsc;

/// The app's actor graph, minus what a headless run cannot have (the IDE transport, live
/// MCP servers, the plan orchestrator).
struct System {
    coord: mpsc::Sender<CoordinatorMessage>,
}

impl System {
    /// Send one request and wait for the coordinator's answer — the same envelope the UI
    /// sends, so anything reachable here is reachable from the app.
    async fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.coord
            .send(CoordinatorMessage::HandleRequest {
                method: method.to_string(),
                params,
                response_tx: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    /// Call an in-process tool, exactly as the UI does.
    async fn tool(&self, tool: &str, args: serde_json::Value) -> serde_json::Value {
        self.call(
            "tools/call",
            serde_json::json!({ "tool": tool, "args": args }),
        )
        .await
    }
}

/// Spawn a build module and return its sender — `ffi.rs` keeps this private, so the
/// harness keeps its own copy (a handful of lines, and the system test fails loudly if the
/// handshake ever changes).
fn spawn_module<A: Actor>(actor: A) -> mpsc::Sender<A::Message> {
    let (tx, rx) = mpsc::channel::<A::Message>(32);
    actor.spawn(rx);
    tx
}

/// Ask a module what it can do, then register it with the build manager.
///
/// Without this the manager has an **empty router** (`router: HashMap::new()`), and answers
/// "No known build config" for a project it ought to recognise — because it has no module
/// with which to recognise it.
async fn register_build_module(
    cap_name: &str,
    module_tx: mpsc::Sender<BuildModuleMessage>,
    bm_tx: &mpsc::Sender<BuildManagerMessage>,
) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = module_tx
        .send(BuildModuleMessage::DescribeCapabilities { reply_to: reply_tx })
        .await;
    let capability = reply_rx
        .await
        .unwrap_or_else(|_| panic!("module '{cap_name}' did not describe its capabilities"));
    let _ = bm_tx
        .send(BuildManagerMessage::AddModule {
            capability,
            module_tx,
        })
        .await;
}

async fn system(llm_url: &str) -> System {
    let system = ActorSystem::new();

    // Real, because these two are what the Spine actually talks to: the graph the
    // diagnostics are read from, and the build manager that writes them.
    let (memory_graph_tx, _) = system.spawn(MemoryGraphActor::new());
    let (bm_tx, _) = system.spawn(BuildManagerActor::new(memory_graph_tx.clone()));

    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig {
        api_url: llm_url.to_string(),
        api_key: "test-key".to_string(),
        ..LlmConfig::default()
    }));
    let (system_tx, _) = system.spawn(SystemActor::new());

    // The REAL tool registry: this is what `tools/call` resolves against, so a tool that
    // is not reachable from here is a tool the UI cannot call.
    let project_query_tx = mock_sender();
    let tool_registry = build_default_registry(
        mock_sender(),
        project_query_tx.clone(),
        Some(mock_sender()),
        Some(mock_sender()),
        Some(mock_sender()),
        Some(mock_sender()),
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
        bm_tx.clone(),
        mock_sender(),
    )
    .await
    .expect("build the tool registry");

    let (tool_router_tx, _) = system.spawn(ToolRouterActor::new(tool_registry, mcp_tx.clone()));
    let (tools_tx, _) = system.spawn(ToolsActor::new(tool_router_tx.clone()));

    let (coord_tx, _) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        system_tx,
        memory_graph_tx.clone(),
        project_query_tx.clone(),
        mock_sender(),
        tool_router_tx,
        mock_sender(),
        mock_sender(),
    ));

    // ── The FFI dispatch deps ──
    // The analysis and project handlers resolve their actors from a REGISTRY, not from
    // constructor arguments, and answer "FFI dispatch deps not attached" without it. The
    // app attaches this at init (ffi.rs:779); a headless run has to attach it the same way.
    // Getting this wrong is invisible until a handler is called, which is exactly the kind
    // of gap these tests exist to close.
    let registry = std::sync::Arc::new(ServiceRegistry::new());
    let (project_analyzer_tx, _) = system.spawn(ProjectAnalyzerActor::new());
    let _ = registry.register::<ProjectAnalyzerMessage>("project.analyzer", project_analyzer_tx);
    let _ = registry
        .register::<spire_code::actors::BuildManagerMessage>("build.manager", bm_tx.clone());
    let _ = registry.register::<MemoryGraphMessage>("memory_graph", memory_graph_tx.clone());
    let _ = registry.register::<MemoryGraphMessage>("knowledge_graph", memory_graph_tx.clone());
    let _ = registry.register::<ProjectQueryMessage>("project.query", project_query_tx);
    // The language modules. The manager starts with an EMPTY router, so a project's build
    // system is recognised only once its module is registered — the step that made
    // `build_analyze` answer "No known build config". Meson is what this fixture needs; the
    // rest get registered as the scenarios widen.
    let meson_tx = spawn_module(MesonBuildModule::new());
    let _ = registry.register::<BuildModuleMessage>("build_module_meson", meson_tx.clone());
    register_build_module("meson", meson_tx, &bm_tx).await;
    let _ = coord_tx
        .send(CoordinatorMessage::SetFfiDeps {
            registry,
            state: std::sync::Arc::new(FfiSharedState {
                project_root: std::sync::Mutex::new(None),
                analysis: std::sync::Mutex::new(None),
                watcher_out_tx: mock_sender(),
            }),
        })
        .await;

    System { coord: coord_tx }
}

/// The two new flows are registered in the REAL registry, which is what lets the UI (and
/// the model) see them at all. A name handled by the coordinator but not registered is a
/// tool nobody can call.
#[tokio::test]
async fn the_registry_offers_the_new_flows() {
    let sys = system(&fake_llm(vec![])).await;
    let listed = sys
        .call("tools/list", serde_json::json!({}))
        .await
        .to_string();

    for tool in ["build_autofix", "modify_code", "modify_contract"] {
        assert!(listed.contains(tool), "{tool} is not registered: {listed}");
    }
}

/// A build request reaches the REAL build manager, not a mock.
///
/// The distinction matters: with a mock the call quietly answers "ToolRouter actor
/// response error", and every flow above it reads that as "nothing is broken". Here the
/// manager answers for itself, refusing a directory it has no analysis for — the
/// difference between a test that runs the system and one that only looks like it does.
#[tokio::test]
async fn a_build_request_reaches_the_real_build_manager() {
    let tmp = tempfile::tempdir().unwrap();
    let sys = system(&fake_llm(vec![])).await;

    let reply = sys
        .tool(
            "build_build",
            serde_json::json!({ "path": tmp.path().to_string_lossy() }),
        )
        .await
        .to_string();

    assert!(
        !reply.contains("ToolRouter actor response error"),
        "the call never reached the build manager: {reply}"
    );
    assert!(
        reply.to_lowercase().contains("analy"),
        "the manager should refuse a directory it has not analysed: {reply}"
    );
}

/// `modify/code` is dispatched and reaches its own guard: asked for nothing, it says so
/// rather than silently succeeding. End-to-end proof that this session's dispatch is live.
#[tokio::test]
async fn modify_code_is_dispatched_and_refuses_an_empty_request() {
    let sys = system(&fake_llm(vec![])).await;
    let reply = sys.tool("modify_code", serde_json::json!({})).await;

    assert_eq!(reply["success"], serde_json::json!(false), "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .unwrap_or_default()
            .contains("'path' and 'prompt'"),
        "the handler's own guard should answer: {reply}"
    );
}

/// A minimal Meson C++ project, configured for a native build in `build-host`.
///
/// The fixture sets the build directory up with the real toolchain and then hands over, so
/// what the tests exercise is the *workflow* (analyse → build → fix) rather than the
/// project's own discovery rules.
fn meson_project(source: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("meson.build"),
        "project('spine-fixture', 'cpp')\nexecutable('fixture', 'main.cpp')\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("main.cpp"), source).unwrap();

    let out = std::process::Command::new("meson")
        .args(["setup", "build-host"])
        .current_dir(tmp.path())
        .output()
        .expect("meson must be installed for the system tests");
    assert!(
        out.status.success(),
        "meson setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    tmp
}

/// The floor for everything above it: a real C++ project compiles through the real
/// toolchain, along the same path the app uses.
///
/// If this fails, no "Fix & Verify fixed a bug" test built on top of it would mean
/// anything — the failure would be the harness, not the flow.
///
/// The order matters, and none of it is discoverable from a unit test: `project/open` (the
/// handlers resolve against the root it remembers) → `AnalyzeProject` → `build_analyze`
/// (the build manager keeps its OWN analysis, separate from the project one) → `build_build`.
/// The build modules must be registered too, or the manager has no router to recognise a
/// project with.
#[tokio::test]
async fn a_real_cpp_project_analyses_and_builds() {
    let tmp = meson_project("int main() { return 0; }\n");
    let path = tmp.path().to_string_lossy().to_string();
    let sys = system(&fake_llm(vec![])).await;

    // The app's own order: OPEN the project first — the handlers resolve against the root
    // that `project/open` remembers — and then analyse it.
    let opened = sys
        .call("project/open", serde_json::json!({ "root": path }))
        .await;
    assert!(
        !opened.to_string().contains("\"error\""),
        "project/open failed: {opened}"
    );

    let analysed = sys
        .call("AnalyzeProject", serde_json::json!({ "path": path }))
        .await;
    assert!(
        !analysed.to_string().contains("\"error\""),
        "analysis failed: {analysed}"
    );

    // The build manager keeps its OWN analysis (populated by `build_analyze`), separate
    // from the project analysis the coordinator holds. Building without it answers "No
    // stored analysis" — a real ordering requirement, and exactly the sort of thing a
    // system test finds and a unit test cannot.
    let build_analysis = sys
        .tool("build_analyze", serde_json::json!({ "path": path }))
        .await;
    assert!(
        !build_analysis.to_string().contains("\"error\""),
        "build_analyze failed: {build_analysis}"
    );

    let built = sys
        .tool(
            "build_build",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;
    assert_eq!(
        built["success"],
        serde_json::json!(true),
        "the fixture must build before anything is built on it: {built}"
    );
}
