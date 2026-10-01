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
use spire_code::build::{BuildModuleMessage, IdfBuildModule, MesonBuildModule};
use spire_code::subsystems::project::project_creation::{
    ProjectCreationActor, ProjectCreationMessage,
};
use spire_core::modules::{FilesystemMessage, FilesystemModule};
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

/// The harness with the model replaced: same wiring, a scripted endpoint.
async fn system(llm_url: &str) -> System {
    system_with_llm(LlmConfig {
        api_url: llm_url.to_string(),
        api_key: "test-key".to_string(),
        ..LlmConfig::default()
    })
    .await
}

/// The same system, with whatever `LlmConfig` the caller supplies — including the REAL one
/// from the app's own config (`~/.spire/<app>/llm-config.json`; `SPIRE_APP_NAME=spire-code`
/// when running a *test* binary, which is not named `spire-code`). That is how the live-model
/// tests reach DeepSeek without a key ever appearing in this file:
/// `load_global_llm_config()` is the same loader the app calls at startup
/// (startup_phases.rs:1087), so the test sees exactly what the app sees.
async fn system_with_llm(llm_config: LlmConfig) -> System {
    let system = ActorSystem::new();

    // Real, because these two are what the Spine actually talks to: the graph the
    // diagnostics are read from, and the build manager that writes them.
    let (memory_graph_tx, _) = system.spawn(MemoryGraphActor::new());
    let (bm_tx, _) = system.spawn(BuildManagerActor::new(memory_graph_tx.clone()));

    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    // Read before the config moves into the LLM actor, and kept for the creation actor below.
    let has_llm_key = !llm_config.api_key.is_empty();
    let (llm_tx, _) = system.spawn(LlmActor::new(llm_config));
    let (system_tx, _) = system.spawn(SystemActor::new());
    let creation_mcp_tx = mcp_tx.clone();
    let creation_llm_tx = llm_tx.clone();

    // The build manager needs the LLM too: `hal_generate_impl` answers "LLM unavailable —
    // the build manager is not connected to the LLM service" without it (ffi.rs:315).
    let _ = bm_tx
        .send(BuildManagerMessage::SetLlm {
            llm_tx: llm_tx.clone(),
        })
        .await;

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
    // The **creation actor** and the filesystem it writes through. Without these, every
    // `createProject/*` request answers "lost: channel closed" — the registry has nothing under
    // `project_creation`, the dummy sender swallows the request and the reply channel drops. That is
    // what a creation run discovers and unit tests cannot: the flows are the flows, and the *wiring*
    // is what makes them reachable.
    let fs_tx = spawn_module(FilesystemModule::new());
    let _ = registry.register::<FilesystemMessage>("filesystem", fs_tx.clone());
    let mut project_creation =
        ProjectCreationActor::new(fs_tx.clone(), bm_tx.clone(), creation_mcp_tx);
    if has_llm_key {
        project_creation.set_llm(creation_llm_tx);
    }
    project_creation.set_memory_graph(memory_graph_tx.clone());
    let (project_creation_tx, _) = system.spawn(project_creation);
    let _ = registry
        .register::<ProjectCreationMessage>("project_creation", project_creation_tx.clone());
    // The language modules. The manager starts with an EMPTY router, so a project's build
    // system is recognised only once its module is registered — the step that made
    // `build_analyze` answer "No known build config". Meson is what this fixture needs; the
    // rest get registered as the scenarios widen.
    let meson_tx = spawn_module(MesonBuildModule::new());
    let _ = registry.register::<BuildModuleMessage>("build_module_meson", meson_tx.clone());
    register_build_module("meson", meson_tx, &bm_tx).await;
    // ESP-IDF, for the application-creation run: a scaffold is routed by the **config file** a module
    // claims, so without this the manager answers "no build module owns config file
    // 'sdkconfig.defaults'" and neither ESP-IDF project type can be created at all.
    let idf_tx = spawn_module(IdfBuildModule::new());
    let _ = registry.register::<BuildModuleMessage>("build_module_idf", idf_tx.clone());
    register_build_module("esp-idf", idf_tx, &bm_tx).await;
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

/// The Spine's core promise, at system level: a real defect, found by a real compiler,
/// fixed, verified against that compiler, and kept.
///
/// Only the model's *text* is replaced — the reply is scripted. Everything else is
/// production code: the real build manager compiles, the real graph holds the diagnostic,
/// and the real autofix loop proposes, applies, rebuilds and decides.
#[tokio::test]
async fn fix_and_verify_fixes_a_real_defect() {
    let fixed = "int main() { return 0; }\n";
    let tmp = meson_project(fixed);
    let path = tmp.path().to_string_lossy().to_string();
    let source = tmp.path().join("main.cpp");

    // What the model will answer, scripted. It repeats if asked again, so a retry is
    // harmless.
    let sys = system(&fake_llm(vec![format!("```cpp\n{fixed}```\n")])).await;

    // The baseline, in the order the system requires.
    sys.call("project/open", serde_json::json!({ "root": path }))
        .await;
    sys.call("AnalyzeProject", serde_json::json!({ "path": path }))
        .await;
    sys.tool("build_analyze", serde_json::json!({ "path": path }))
        .await;
    let clean = sys
        .tool(
            "build_build",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;
    assert_eq!(clean["success"], serde_json::json!(true), "{clean}");

    // Break it for real: a brace the compiler will refuse.
    std::fs::write(&source, "int main( { return 0; }\n").unwrap();
    let broken = sys
        .tool(
            "build_build",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;
    assert_ne!(
        broken["success"],
        serde_json::json!(true),
        "the injected defect must actually break the build, or nothing below means \
         anything: {broken}"
    );

    // Fix & Verify, end to end.
    let report = sys
        .tool(
            "build_autofix",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;

    assert_eq!(report["success"], serde_json::json!(true), "{report}");
    // Not an exact count: a missing brace produces several g++ errors, so what matters is
    // that the run ends with none AND names what it fixed.
    let output = report["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("→ 0"),
        "the run should end with no errors left: {report}"
    );
    assert!(
        output.contains("error fixes kept"),
        "and should say which files it fixed: {report}"
    );
    // `contains`, not equality: unwrapping the model's code fence trims the trailing
    // newline, and what matters is that the brace is back on disk.
    let written = std::fs::read_to_string(&source).unwrap();
    assert!(
        written.contains("int main() { return 0; }"),
        "the fix has to be on disk, not just in the report: {written:?}"
    );
}

/// `modify/code` at system level: a prompt in the user's words, a scripted plan, and a real
/// verification against a real compiler.
///
/// The preamble is the same one Fix & Verify needed — which is the point of widening here:
/// it shows the harness generalises to a second flow rather than being fitted to one.
#[tokio::test]
async fn modify_code_applies_a_prompted_change_and_keeps_it() {
    let tmp = meson_project("int main() { return 0; }\n");
    let path = tmp.path().to_string_lossy().to_string();
    let source = tmp.path().join("main.cpp");
    let name = source.to_string_lossy().to_string();
    let rewritten = "int main() { return 42; }";

    // The two answers `plan` needs, in order: which files to touch, then the rewrite.
    let sys = system(&fake_llm(vec![
        name.clone(),
        format!("```cpp\n{rewritten}\n```\n"),
    ]))
    .await;

    sys.call("project/open", serde_json::json!({ "root": path }))
        .await;
    sys.call("AnalyzeProject", serde_json::json!({ "path": path }))
        .await;
    sys.tool("build_analyze", serde_json::json!({ "path": path }))
        .await;

    let report = sys
        .tool(
            "modify_code",
            serde_json::json!({
                "path": path,
                "prompt": "make main return 42",
                "platform": "host",
                "scope": [name.clone()],
            }),
        )
        .await;

    assert_eq!(report["success"], serde_json::json!(true), "{report}");
    assert_eq!(
        report["files_changed"],
        serde_json::json!([name]),
        "the file the model named should be the one changed: {report}"
    );
    let written = std::fs::read_to_string(&source).unwrap();
    assert!(
        written.contains(rewritten),
        "the rewrite has to be on disk, not just in the report: {written:?}"
    );
}

/// A Meson C++ project that also carries a HAL contract with NO implementation — the shape
/// the contract cascade exists for: the contract declares an interface, the platform does
/// not implement it.
fn hal_project() -> tempfile::TempDir {
    let tmp = meson_project("int main() { return 0; }\n");
    std::fs::create_dir_all(tmp.path().join("hal").join("api")).unwrap();
    std::fs::create_dir_all(tmp.path().join("hal").join("implementations").join("host")).unwrap();
    std::fs::write(
        tmp.path().join("hal").join("api").join("camera.hpp"),
        "#pragma once\n\nclass CameraHal {\npublic:\n    virtual ~CameraHal() = default;\n    \
         virtual void start() = 0;\n};\n",
    )
    .unwrap();
    tmp
}

/// The contract cascade at system level: the real coverage analysis finds a real HAL gap,
/// and the run must NOT claim to have closed it.
///
/// The model answers `NONE` here (the fake endpoint's default), so generation yields
/// nothing. That is the honest case to pin: a flow that reported success on an unresolved
/// gap would be worse than one that failed, because it would be believed.
#[tokio::test]
async fn modify_contract_reports_a_real_gap_it_could_not_close() {
    let tmp = hal_project();
    let path = tmp.path().to_string_lossy().to_string();
    let sys = system(&fake_llm(vec![])).await;

    sys.call("project/open", serde_json::json!({ "root": path }))
        .await;
    sys.call("AnalyzeProject", serde_json::json!({ "path": path }))
        .await;
    sys.tool("build_analyze", serde_json::json!({ "path": path }))
        .await;

    let report = sys
        .tool(
            "modify_contract",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;

    // The real coverage analysis saw the contract and found nothing implementing it.
    let drift = report["drift_before"].as_u64().unwrap_or(0);
    assert!(
        drift > 0,
        "the fixture's unimplemented contract should be drift: {report}"
    );
    // And the run says so, rather than reporting a success it did not achieve.
    assert_eq!(report["success"], serde_json::json!(false), "{report}");
    assert!(
        report["gaps_remaining"].to_string().contains("camera"),
        "the unresolved interface should still be listed: {report}"
    );
}

/// The Spine's other half: a change that makes the project **worse** is rolled back, and the
/// file is left exactly as it was.
///
/// The rewrite parses — so the structural check passes it through — but it does not compile,
/// which is precisely the case the build-verification step exists to catch.
#[tokio::test]
async fn modify_code_rolls_back_a_change_that_breaks_the_build() {
    let original = "int main() { return 0; }\n";
    let tmp = meson_project(original);
    let path = tmp.path().to_string_lossy().to_string();
    let source = tmp.path().join("main.cpp");
    let name = source.to_string_lossy().to_string();

    let sys = system(&fake_llm(vec![
        name.clone(),
        "```cpp\nint main() { return undefined_function(); }\n```\n".to_string(),
    ]))
    .await;

    sys.call("project/open", serde_json::json!({ "root": path }))
        .await;
    sys.call("AnalyzeProject", serde_json::json!({ "path": path }))
        .await;
    sys.tool("build_analyze", serde_json::json!({ "path": path }))
        .await;

    let report = sys
        .tool(
            "modify_code",
            serde_json::json!({
                "path": path,
                "prompt": "break it",
                "platform": "host",
                "scope": [name.clone()],
            }),
        )
        .await;

    assert_eq!(report["success"], serde_json::json!(false), "{report}");
    assert_eq!(
        report["files_reverted"],
        serde_json::json!([name]),
        "the change should have been rolled back: {report}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        original,
        "the original bytes must be back on disk"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────
// The one question the eight above cannot answer: does a REAL model fix a real defect?
//
// Everything else in this file holds the model's text constant, so that the plumbing is
// what is on trial. This test inverts that: the plumbing is known good, and the model's
// QUALITY is the only variable left. It is the one question a deterministic assertion
// cannot answer — a real model may legitimately solve this differently every run — so what
// is asserted below is STRUCTURE (a change that builds, verified by the same compiler),
// never exact text.
//
// Gated twice, on purpose: `#[ignore]` so a plain `cargo test` never spends money or
// network, and a runtime check so that even `--ignored` skips cleanly with no key
// configured. The key is read through `load_global_llm_config()` — the same call the app
// makes at startup — so it never lives in this file, and the test passes or skips on the
// machine's own configuration. Run it with:
//
//     cargo test -p spire-code --test system_flow_tests -- --ignored
// ─────────────────────────────────────────────────────────────────────────────────────
#[ignore = "live model: spends real DeepSeek calls — run explicitly with `--ignored`"]
#[tokio::test]
async fn a_real_model_fixes_a_real_defect() {
    // ── The gate ──
    let llm_config = spire_core::config::load_global_llm_config();
    if llm_config.api_key.is_empty() {
        eprintln!(
            "skipped: no deepseek.api_key in {}\n             A test binary is not named `spire-code`, so `SPIRE_APP_NAME=spire-code` is what
             points this at the app's own config dir.",
            spire_core::config::llm_config_path().display()
        );
        return;
    }
    eprintln!(
        "live model: {} at {}",
        llm_config.coding_model, llm_config.api_url
    );

    let fixed = "int main() { return 0; }\n";
    let tmp = meson_project(fixed);
    let path = tmp.path().to_string_lossy().to_string();
    let source = tmp.path().join("main.cpp");

    // The REAL config: real url, real key, real model. No scripted reply, no fake endpoint.
    let sys = system_with_llm(llm_config).await;

    // The baseline, in the order the system requires.
    sys.call("project/open", serde_json::json!({ "root": path }))
        .await;
    sys.call("AnalyzeProject", serde_json::json!({ "path": path }))
        .await;
    sys.tool("build_analyze", serde_json::json!({ "path": path }))
        .await;
    let clean = sys
        .tool(
            "build_build",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;
    assert_eq!(
        clean["success"],
        serde_json::json!(true),
        "the fixture must build before anything is done to it: {clean}"
    );

    // Break it for real — the SAME defect the deterministic test uses, so the only
    // difference between the two runs is who does the fixing.
    std::fs::write(&source, "int main( { return 0; }\n").unwrap();
    let broken = sys
        .tool(
            "build_build",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;
    assert_ne!(
        broken["success"],
        serde_json::json!(true),
        "the defect must actually break the build, or the fix below proves nothing: {broken}"
    );

    // Fix & Verify with the real model in the loop. The timeout is a guard, not an
    // expectation: a reasoning model answering several diagnostics is slow, and a hung
    // call should read as "the network or the API", never as a defect in the flow.
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        sys.tool(
            "build_autofix",
            serde_json::json!({ "path": path, "platform": "host" }),
        ),
    )
    .await
    .expect("the live model call timed out — network/API, not the flow");

    assert_eq!(
        report["success"],
        serde_json::json!(true),
        "the live model did not fix a one-brace defect: {report}"
    );
    // Structure, not text: the model may seat the brace differently or restructure the
    // function, and both are legitimate. What must hold is that the defect is gone from
    // disk and the project builds.
    let written = std::fs::read_to_string(&source).unwrap();
    assert!(
        !written.contains("int main( {"),
        "the defect is still on disk: {written:?}"
    );
    let rebuilt = sys
        .tool(
            "build_build",
            serde_json::json!({ "path": path, "platform": "host" }),
        )
        .await;
    assert_eq!(
        rebuilt["success"],
        serde_json::json!(true),
        "what the model wrote does not build: {rebuilt}"
    );
}
// ─────────────────────────────────────────────────────────────────────────────────────
// The **application-creation loop**, end to end, with a real model.
//
// Gated the same way the live fix run is: `#[ignore]` so a plain `cargo test` never spends money or
// network, and a runtime check so that even `--ignored` skips cleanly with no key configured. Two more
// gates, because this one needs a library to design against and a place to put a product:
//
//   SPIRE_LIVE_LIBRARY=<an ESP-IDF component library>   the library the application is built against
//   SPIRE_LIVE_WORKSPACE=<a directory to create in>     where the product is scaffolded
//   SPIRE_APP_NAME=spire-code                           so the loader finds the app's own config
//
//     SPIRE_APP_NAME=spire-code SPIRE_LIVE_LIBRARY=~/spire-idf SPIRE_LIVE_WORKSPACE=/tmp/app-one \
//       cargo test -p spire-code --test system_flow_tests -- --ignored --nocapture
//
// **The library is copied, never written to.** The copy is what the design reads its facts from and what
// the design is applied to, so a run leaves the original untouched and can be repeated.
// ─────────────────────────────────────────────────────────────────────────────────────
#[ignore = "live model: spends real DeepSeek calls — run explicitly with `--ignored`"]
#[tokio::test]
async fn a_real_model_designs_builds_and_fills_an_application() {
    use spire_code::build::application_spec::{validate, ApplicationSpec, UnitKind, UnitSource};

    // ── The gates ──
    let llm_config = spire_core::config::load_global_llm_config();
    if llm_config.api_key.is_empty() {
        eprintln!(
            "skipped: no deepseek.api_key in {}\n             A test binary is not named `spire-code`, so `SPIRE_APP_NAME=spire-code` is what
             points this at the app's own config dir.",
            spire_core::config::llm_config_path().display()
        );
        return;
    }
    let Ok(library_source) = std::env::var("SPIRE_LIVE_LIBRARY") else {
        eprintln!("skipped: set SPIRE_LIVE_LIBRARY to a component library to design against");
        return;
    };
    let Ok(workspace) = std::env::var("SPIRE_LIVE_WORKSPACE") else {
        eprintln!("skipped: set SPIRE_LIVE_WORKSPACE to a directory to create the product in");
        return;
    };
    let workspace = std::path::PathBuf::from(workspace);
    std::fs::create_dir_all(&workspace).expect("create the workspace");

    // The library is copied: a run reads its facts and writes the design's components into the *copy*,
    // so the original is never touched and the run can be repeated.
    let library = workspace.join("library");
    let _ = std::fs::remove_dir_all(&library);
    copy_tree(std::path::Path::new(&library_source), &library);
    let manifest = std::fs::read_to_string(library.join("CMakeLists.txt")).expect("its manifest");
    assert!(
        spire_code::build::idf_projects::declares_library(&manifest),
        "{library_source} is not a component library"
    );
    eprintln!(
        "live: {} against {}, library {library_source} ({} components)",
        llm_config.planning_model,
        llm_config.api_url,
        spire_code::build::idf_projects::component_names(&library).len()
    );

    let sys = system_with_llm(llm_config).await;

    // ── 1. Design ──
    // The design form's six answers, in the words a person would use, and the real library: the
    // components it names as `existing` are the ones this library actually has.
    let description = "A desktop air-quality meter on an M5Stack Core S3. It reads particulates \
                       (PM1/2.5/4/10) from an SPS30 and temperature/humidity from an SHT20, both on \
                       the internal I2C bus. It shows the current reading and a rolling average on the \
                       built-in screen, and lets me recalibrate from the touch screen. It samples about \
                       once a second and keeps working, with a warning, if one sensor is missing.";
    let designed = sys
        .call(
            "createProject/DesignApplication",
            serde_json::json!({
                "board": { "chip": "esp32s3", "bsp": "m5stack_core_s3", "hal": "m5unified" },
                "description": description,
                "libraryRoot": library.to_string_lossy(),
            }),
        )
        .await;
    let spec: ApplicationSpec = serde_json::from_value(designed["spec"].clone())
        .unwrap_or_else(|e| panic!("the design did not come back as a spec: {e}\n{designed}"));
    validate(&spec).expect("the design phase only returns a spec that passed the checks");
    eprintln!(
        "\n══ THE DESIGN (for review) ══\n{}\n",
        serde_json::to_string_pretty(&spec).unwrap_or_default()
    );

    // Structure, not judgement: a person reviews the composition above; this pins what the loop
    // depends on — the board that was asked for, and a design with something in it.
    assert_eq!(spec.board.chip, "esp32s3");
    assert_eq!(spec.board.bsp, "m5stack_core_s3");
    assert!(
        !spec.units.is_empty(),
        "a design with no units is not a design"
    );

    // ── 2. Apply the design: the library gains the components it asked to be written ──
    let applied = sys
        .tool(
            "idf_apply_design",
            serde_json::json!({
                "root": library.to_string_lossy(),
                "application": spec,
            }),
        )
        .await;
    eprintln!(
        "apply: {}",
        serde_json::to_string_pretty(&applied).unwrap_or_default()
    );
    let after = spire_code::build::idf_projects::component_names(&library);
    for unit in spec.units.iter().filter(|unit| {
        // A **published** component is a managed dependency: `main/idf_component.yml` resolves it, so
        // the library is not expected to have it.
        unit.kind == UnitKind::Component
            && !matches!(
                unit.source,
                Some(UnitSource::Existing) | Some(UnitSource::Published)
            )
    }) {
        let ident = unit.id.replace('-', "_");
        assert!(
            after.contains(&ident),
            "the design asked for `{ident}` and the library does not have it: {after:?}"
        );
        eprintln!(
            "  {ident}: {:?}",
            spire_code::build::idf_projects::component_kind(&library, &ident)
        );
    }

    // ── 3. Scaffold the application, from the same spec ──
    let app_root = workspace.join("pm25-meter");
    let _ = std::fs::remove_dir_all(&app_root);
    let scaffolded = sys
        .call(
            "createProject/Scaffold",
            serde_json::json!({
                "projectName": "pm25-meter",
                "rootDir": app_root.to_string_lossy(),
                "language": "C++",
                "structure": "idf_application",
                "embedded": true,
                "embeddedRoot": library.to_string_lossy(),
                "application": spec,
                // The loop, not the toolchain: a board build here would be about this machine.
                "verifyBackends": false,
            }),
        )
        .await;
    assert!(
        scaffolded.get("error").is_none(),
        "the application did not scaffold: {scaffolded}"
    );
    let cmake = std::fs::read_to_string(app_root.join("CMakeLists.txt")).expect("its manifest");
    let stated = spire_code::build::application_spec::declared_framework(&cmake);
    eprintln!("app: framework stated = {stated:?}");
    assert_eq!(
        stated,
        Ok(Some(spec.framework)),
        "the application states the framework it was designed in:\n{cmake}"
    );
    assert!(
        app_root
            .join(spire_code::build::idf_projects::APPLICATION_FILE)
            .is_file(),
        "and carries the decomposition the fill phase reads"
    );

    // ── 4. Fill: the model writes `main/` from the design it was given ──
    // The scaffold's answer *is* the contract the fill respects, so it is passed straight through
    // rather than reconstructed here.
    let filled = sys
        .call(
            "createProject/Fill",
            serde_json::json!({
                "goal": description,
                "rootDir": app_root.to_string_lossy(),
                "spec": scaffolded,
                "embeddedRoot": library.to_string_lossy(),
            }),
        )
        .await;
    assert!(
        filled.get("error").is_none(),
        "the fill leg failed: {filled}"
    );
    let plan = filled.get("plan").unwrap_or(&filled);
    let steps = plan
        .get("steps")
        .and_then(|steps| steps.as_array())
        .unwrap_or_else(|| panic!("the fill returned no plan: {filled}"));
    eprintln!("fill: {} steps", steps.len());
    let writes: Vec<serde_json::Value> = steps
        .iter()
        .filter(|step| {
            // The plan is serialized for the wire, so the step's type arrives as `stepType`.
            step.get("stepType").and_then(|t| t.as_str()) == Some("write_source_file")
                || step.get("step_type").and_then(|t| t.as_str()) == Some("write_source_file")
        })
        .cloned()
        .collect();
    assert!(
        !writes.is_empty(),
        "a fill that writes nothing is not a fill: {filled}"
    );
    assert!(
        writes.iter().any(|step| step["parameters"]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("main/"))),
        "the composition belongs in `main/`: {writes:?}"
    );

    // ── 5. Write what it planned, and say what it did not ──
    // The write steps only: the plan's parse and build gates are the app's to run (they need the ESP-IDF
    // toolchain and a chip target, and a build failure here would be about this machine, not the loop).
    let written = sys
        .call(
            "createProject/ExecutePlan",
            serde_json::json!({
                "rootDir": app_root.to_string_lossy(),
                "steps": writes,
            }),
        )
        .await;
    eprintln!(
        "executed: {}",
        serde_json::to_string_pretty(&written).unwrap_or_default()
    );
    let main_cpp = app_root.join("main/main.cpp");
    assert!(
        main_cpp.is_file(),
        "the plan said it would write `main/` and did not"
    );
    let body = std::fs::read_to_string(&main_cpp).unwrap_or_default();
    eprintln!(
        "\n══ main/main.cpp ({} bytes) ══\n{}\n",
        body.len(),
        body.chars().take(4000).collect::<String>()
    );

    // The design's units and board facts have to be *in* what was written, not merely in the prompt:
    // what was filled is the application that was reviewed. Names are compared with separators stripped
    // and case folded — the design says `rolling_average` and C++ writes `RollingAverage`, and a test
    // that insisted on one of them would be testing the model's spelling rather than the loop.
    let mut composed = String::new();
    for entry in std::fs::read_dir(app_root.join("main")).expect("the source tree exists") {
        let path = entry.expect("an entry").path();
        if path.is_file() {
            composed.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
        }
    }
    assert!(
        !composed.trim().is_empty(),
        "the fill wrote nothing into main/"
    );
    let folded = composed.to_lowercase().replace(['_', '-'], "");
    for unit in &spec.units {
        let designed_name = unit.id.to_lowercase().replace(['_', '-'], "");
        assert!(
            folded.contains(&designed_name),
            "`{}` was designed and `main/` does not name it",
            unit.id
        );
    }
    // **A stub is a boundary, not an API.** A component the design marked `stub` has a header that is a
    // shape and a `TODO` — nothing callable — so the composition has to leave its calls open rather than
    // invent them. The first live build failed on exactly that: `RollingAverage::push`, an initializer
    // the stub does not take, and a `uint8_t` address passed where a `BusHandle` is wanted. A `TODO`
    // where the call belongs is the shape that compiles, and the shape a person finishes after writing
    // the protocol.
    if spec.units.iter().any(|unit| {
        unit.kind == spire_code::build::application_spec::UnitKind::Component
            && !matches!(
                unit.source,
                Some(spire_code::build::application_spec::UnitSource::Existing)
                    | Some(spire_code::build::application_spec::UnitSource::Published)
            )
    }) {
        assert!(
            composed.contains("TODO"),
            "every component in this design is a stub, and `main/` invented their calls instead of \
             leaving them open — which is a composition that cannot compile"
        );
    }
    // A board fact is the *application's* — the rule the whole component library rests on — so it is in
    // `main/`, which is where a reviewer looks for it.
    for fact in &spec.board_facts {
        if !fact.address.trim().is_empty() {
            assert!(
                composed.contains(&fact.address),
                "the design's board fact for `{}` (at {}) did not reach the application",
                fact.device,
                fact.address
            );
        }
    }

    // ── 5. Is it the composition that was reviewed? ─────────────────────────────────────────────────
    //
    // Every assertion above passes for a `main/` that names each component and then writes **its own**:
    // a live run did exactly that — one flat FreeRTOS poll loop, `xTaskCreate`, and `class Sps30` /
    // `class Sht20` in `main/sensors.h` — and every name the design used was present. Naming a
    // component is not using it. The gate is what tells the two apart, so the loop ends on it rather
    // than on the file having been written.
    let verified = sys
        .call(
            "createProject/VerifyApplication",
            serde_json::json!({
                "rootDir": app_root.to_string_lossy(),
                "application": &spec,
            }),
        )
        .await;
    let gaps = verified["gaps"].as_array().cloned().unwrap_or_default();
    eprintln!(
        "verify: {} — {}",
        if gaps.is_empty() {
            "composed"
        } else {
            "NOT composed"
        },
        serde_json::to_string_pretty(&gaps).unwrap_or_default()
    );
    assert!(
        gaps.is_empty(),
        "the fill was handed the reviewed composition and did not write it:\n{}",
        serde_json::to_string_pretty(&gaps).unwrap_or_default()
    );

    // ── 5. And it is **built**, where this machine can ──────────────────────────────────────────────
    //
    // The scaffold's contract is a file **cmake reads**, and an assertion about a substring is not a
    // reader: four defects of this scaffold were invisible to every test in the suite — a leftover
    // `__FRAMEWORK_BLOCK__` line, an absolute library path concatenated onto the application's own, a
    // `SRCS` glob IDF takes literally, and no `REQUIRES` at all — and a real build found each one. What
    // this step does is put that reader *inside* the loop, so the model's composition is compiled by the
    // compiler and not by the person who opens the project.
    //
    // `SPIRE_LIVE_BUILD_COMMAND` is the whole environment contract: a shell command run **in the
    // application's directory** that builds it. Shelling out to a command rather than calling `idf.py`
    // is deliberate — this machine's `export.sh` is broken (an unsupported system ninja and a venv
    // activation error), so the command is a person's own working invocation. The gate says where it
    // looked when it cannot run, because a gate that skips silently looks exactly like a gate that
    // passed.
    match std::env::var("SPIRE_LIVE_BUILD_COMMAND") {
        Ok(command) => {
            // The **break**: the repair turn is the one part of this loop that only runs when something
            // is wrong, so the only deterministic way to test it is to make something wrong on purpose.
            // A failing `static_assert` appended to `main.cpp` is an error a whole-file rewrite removes —
            // and it is in the one directory the scaffold leaves open.
            let broke = std::env::var("SPIRE_LIVE_BREAK_BUILD").is_ok();
            if broke {
                let broken = app_root.join("main/main.cpp");
                let mut content = std::fs::read_to_string(&broken).unwrap_or_default();
                content.push_str(
                    "\n// Deliberate: this gate proves the repair turn runs.\n\
                     static_assert(sizeof(int) == 0, \"deliberate break\");\n",
                );
                std::fs::write(&broken, content).expect("the break is written");
                eprintln!("break: appended a failing static_assert to main/main.cpp");
            }

            let mut log = build(&app_root, &command);
            let mut errors = compiler_errors(&log);
            eprintln!(
                "\n══ THE BUILD — {} ══",
                if errors.is_empty() {
                    "passed"
                } else {
                    "failed"
                }
            );
            report_errors(&errors);

            // A build that failed is not an answer, so the loop repairs and rebuilds — **twice at
            // most**, because errors cascade: a syntax error hides every type error in the file it
            // broke, so the first pass fixes what the compiler could see and the second fixes what it
            // could see after that. The diagnostics are handed over *verbatim* — the compiler's own
            // lines, notes included, because the note that says what the compiler expected is usually
            // the whole repair — and the repair returns steps the caller executes through the same path
            // the fill's steps take, which is where the structural guard lives.
            for pass in 1..=2 {
                if errors.is_empty() {
                    break;
                }
                let repaired = sys
                    .call(
                        "createProject/RepairFromBuild",
                        serde_json::json!({
                            "rootDir": app_root.to_string_lossy(),
                            "diagnostics": log,
                            "spec": scaffolded,
                        }),
                    )
                    .await;
                let steps = repaired
                    .get("steps")
                    .and_then(|steps| steps.as_array())
                    .cloned()
                    .unwrap_or_default();
                eprintln!(
                    "\n══ THE REPAIR (pass {pass}) ══\n  {} step(s); {} refused; {} outside what a repair \
                     may touch",
                    steps.len(),
                    repaired
                        .get("refused")
                        .and_then(|v| v.as_array())
                        .map(Vec::len)
                        .unwrap_or(0),
                    repaired
                        .get("unrepaired")
                        .and_then(|v| v.as_array())
                        .map(Vec::len)
                        .unwrap_or(0),
                );
                for entry in repaired
                    .get("unrepaired")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    eprintln!("  not ours to fix: {}", entry.as_str().unwrap_or_default());
                }
                assert!(
                    repaired.get("error").is_none(),
                    "the repair leg failed: {repaired}"
                );
                // Nothing to rewrite means nothing will change on a rebuild, and re-asking the same
                // question about the same log is a loop with no exit.
                if steps.is_empty() {
                    break;
                }
                let applied = sys
                    .call(
                        "createProject/ExecutePlan",
                        serde_json::json!({
                            "rootDir": app_root.to_string_lossy(),
                            "steps": steps,
                        }),
                    )
                    .await;
                eprintln!(
                    "  applied: {}",
                    serde_json::to_string(&applied).unwrap_or_default()
                );
                log = build(&app_root, &command);
                errors = compiler_errors(&log);
                eprintln!(
                    "\n══ THE BUILD AFTER REPAIR {pass} — {} ══",
                    if errors.is_empty() { "passed" } else { "failed" }
                );
                report_errors(&errors);
            }
            // The one thing asserted about the *scaffold*: the application's **own** component reached
            // the compiler. That is its contract — a manifest that collects the sources, names the
            // framework's and the design's components and resolves the library — and it is exactly what
            // was broken while every other assertion in this suite passed.
            assert!(
                log.contains("esp-idf/main/CMakeFiles/__idf_main")
                    || log.contains("Project build complete"),
                "the application's own component never reached the compiler, so cmake did not accept \
                 what the scaffold wrote:\n{}",
                log.lines().rev().take(30).collect::<Vec<_>>().join("\n")
            );
            // And the one thing asserted about the **loop**: it ends with a build that passes. With the
            // deliberate break in place that is the repair's verdict and nothing else — the second build
            // cannot pass unless the rewrite removed the break. Without it, the compiler is judging the
            // model's composition, which is what the loop exists to have judged.
            assert!(
                errors.is_empty(),
                "the application did not build{}: the repair turn {} it. The compiler said:\n{}",
                if broke { " after a deliberate break" } else { "" },
                if broke { "did not fix" } else { "was not needed by" },
                errors.join("\n  ")
            );
        }
        Err(_) => eprintln!(
            "\nbuild: skipped — SPIRE_LIVE_BUILD_COMMAND was not set, so nothing built {}. It is the \
             command that builds in the application's directory (e.g. `. ~/esp/esp-idf/export.sh; idf.py \
             build`).",
            app_root.display()
        ),
    }

    eprintln!(
        "\n✓ the loop ran: designed → applied → scaffolded → filled → built, at {}",
        workspace.display()
    );
}

/// Run the build command in the application's directory, in the environment it inherits.
fn build(root: &std::path::Path, command: &str) -> String {
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()
        .expect("the build command runs");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The compiler's own error lines, which is what a repair turn is given.
fn compiler_errors(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| line.contains("error:"))
        .map(|line| line.trim().to_string())
        .collect()
}

fn report_errors(errors: &[String]) {
    if errors.is_empty() {
        eprintln!("  no compiler errors");
    }
    for error in errors {
        eprintln!("  {error}");
    }
}

/// Copy a directory tree — the library is copied rather than written to, so a live run is repeatable.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the source") {
        let entry = entry.expect("an entry");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a file");
        }
    }
}
