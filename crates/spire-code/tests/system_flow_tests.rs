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
use spire_actor::ActorSystem;
use spire_code::actors::{
    build_default_registry, BuildManagerActor, ChatActor, CoordinatorActor, CoordinatorMessage,
    LlmActor, LlmConfig, McpClientActor, SystemActor, ToolRouterActor, ToolsActor,
};
use spire_core::subsystems::graph::memory_graph::MemoryGraphActor;
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
        memory_graph_tx,
        project_query_tx,
        mock_sender(),
        tool_router_tx,
        mock_sender(),
        mock_sender(),
    ));

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
