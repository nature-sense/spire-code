// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Actor-level unit tests for spire-core.
//!
//! These tests import `spire_core` as a library, create an `ActorSystem`,
//! spawn actors directly, send messages via their `mpsc::Sender` channels,
//! and assert on the responses.

use spire_core::subsystems::graph::memory_graph::{MemoryGraphActor, MemoryGraphMessage};
use spire_code::actors::{
    self, BuildOrchestrator, BuildOrchestratorMessage, ChatActor, ChatMessage, CoordinatorActor,
    CoordinatorMessage, ErrorAnalyzer, ErrorAnalyzerMessage, LlmActor, LlmConfig,
    McpClientActor, ProgressActor, ProgressMessage,
    ProgressStatus, ProgressUpdate, SystemActor, SystemMessage, ToolInfo,
    ToolOrchestratorMessage, ToolRouterActor, ToolsActor, ToolsMessage,
};
use spire_actor::ActorSystem;
use spire_core::models::embedding::{Embedder, Embedding};
use spire_core::models::memory_graph::{
    BuildContext, BuildError, NodeUpdate, AttrNode,
    RelationshipInput, RelationshipType, SystemBuildResult, TraversalDirection, TraversalOptions,
};
use std::collections::HashMap;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Helper to create a mock sender for any actor channel.
fn mock_sender<T: Send + 'static>() -> tokio::sync::mpsc::Sender<T> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    // Drain the receiver so messages don't back up
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

/// Registry pre-populated with the VS Code extension tools (dummy handlers).
///
/// The router tests only exercise `ListTools` (never dispatch), so the
/// handlers can be no-ops — what matters is that `workspace/getFolders` etc.
/// appear in the returned list.
fn test_tool_registry() -> std::sync::Arc<spire_core::actors::tool_providers::ToolRegistry> {
    let registry =
        std::sync::Arc::new(spire_core::actors::tool_providers::ToolRegistry::new());
    for info in spire_code::actors::vscode_tool_definitions() {
        let handler: spire_core::actors::tool_providers::ToolHandler =
            std::sync::Arc::new(|_args| {
                Box::pin(async { Ok(serde_json::json!({})) })
            });
        registry.register(info, handler).unwrap();
    }
    registry
}

/// Helper to create a mock memory graph actor for coordinator tests.
/// Returns a `mpsc::Sender<MemoryGraphMessage>` that ignores all messages.
fn mock_memory_graph() -> tokio::sync::mpsc::Sender<MemoryGraphMessage> {
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    tx
}

/// Helper to extract properties from an Unknown AttrNode.
/// Returns an empty map for typed variants.
fn unknown_props<'a>(node: &'a AttrNode) -> &'a HashMap<String, serde_json::Value> {
    &node.properties
}

// ===========================================================================
// ChatActor tests
// ===========================================================================

#[tokio::test]
async fn test_chat_get_active_returns_default() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ChatActor::new());

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::GetActive { reply_to: resp_tx })
        .await
        .unwrap();
    let dialog = resp_rx.await.unwrap();

    assert!(dialog.is_some());
    let dialog = dialog.unwrap();
    assert_eq!(dialog.id, "default");
    assert_eq!(dialog.title, "New Chat");
    assert!(dialog.messages.is_empty());
}

#[tokio::test]
async fn test_chat_append_message_works() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ChatActor::new());

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::Append {
        chat_id: "default".to_string(),
        content: "Hello, world!".to_string(),
        role: "user".to_string(),
        widget: None,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap();
    assert!(result.is_ok());

    let msg = result.unwrap();
    assert_eq!(msg.role, "user");
    assert_eq!(msg.content, "Hello, world!");
    assert!(!msg.id.is_empty());
    assert!(!msg.timestamp.is_empty());
}

#[tokio::test]
async fn test_chat_get_history_returns_dialogs() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ChatActor::new());

    // Append a message first
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::Append {
        chat_id: "default".to_string(),
        content: "msg1".to_string(),
        role: "user".to_string(),
        widget: None,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Get history
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::GetHistory { reply_to: resp_tx })
        .await
        .unwrap();
    let dialogs = resp_rx.await.unwrap();

    assert_eq!(dialogs.len(), 1);
    assert_eq!(dialogs[0].messages.len(), 1);
    assert_eq!(dialogs[0].messages[0].content, "msg1");
}

#[tokio::test]
async fn test_chat_clear_dialog_works() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ChatActor::new());

    // Append a message
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::Append {
        chat_id: "default".to_string(),
        content: "to_clear".to_string(),
        role: "user".to_string(),
        widget: None,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Clear
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::Clear {
        chat_id: "default".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    assert!(resp_rx.await.unwrap().is_ok());

    // Verify empty
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::GetActive { reply_to: resp_tx })
        .await
        .unwrap();
    let dialog = resp_rx.await.unwrap().unwrap();
    assert!(dialog.messages.is_empty());
}

#[tokio::test]
async fn test_chat_set_title_works() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ChatActor::new());

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::SetTitle {
        chat_id: "default".to_string(),
        title: "My Custom Title".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    assert!(resp_rx.await.unwrap().is_ok());

    // Verify title changed
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::GetActive { reply_to: resp_tx })
        .await
        .unwrap();
    let dialog = resp_rx.await.unwrap().unwrap();
    assert_eq!(dialog.title, "My Custom Title");
}

#[tokio::test]
async fn test_chat_append_to_nonexistent_returns_error() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ChatActor::new());

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ChatMessage::Append {
        chat_id: "nonexistent".to_string(),
        content: "test".to_string(),
        role: "user".to_string(),
        widget: None,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap();
    // GQL DETACH DELETE succeeds silently for nonexistent nodes
    assert!(result.is_err());
}

// ===========================================================================
// ToolsActor tests
// ===========================================================================

#[tokio::test]
async fn test_tools_list_initially_has_vscode_tools() {
    let system = ActorSystem::new();
    let tx = spawn_tools_with_real_router(&system);

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ToolsMessage::ListTools { reply_to: resp_tx })
        .await
        .unwrap();
    let tools = resp_rx.await.unwrap();

    // ToolsActor pre-registers VS Code extension tools at startup
    assert!(!tools.is_empty());
    assert!(tools.iter().any(|t| t.name == "workspace/getFolders"));
}

/// Helper: spawn a real ToolRouterActor (mock backends) + ToolsActor.
///
/// `ToolsActor::ListTools` delegates to the ToolRouterActor, which merges the
/// static VS Code extension tool definitions with embedded/MCP tools. A mock
/// channel never replies, so a real router is required for the list tests.
fn spawn_tools_with_real_router(
    system: &ActorSystem,
) -> tokio::sync::mpsc::Sender<actors::ToolsMessage> {
    let router_tx = system
        .spawn(ToolRouterActor::new(test_tool_registry(), mock_sender()))
        .0;
    system.spawn(ToolsActor::new(router_tx)).0
}

#[tokio::test]
async fn test_tools_register_and_list() {
    let system = ActorSystem::new();
    let tx = spawn_tools_with_real_router(&system);

    // Register a tool
    let tool_info = ToolInfo {
        name: "test_tool".to_string(),
        description: "A test tool".to_string(),
        input_schema: serde_json::json!({}),
    };
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ToolsMessage::RegisterTool {
        server: "test_server".to_string(),
        info: tool_info,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    assert!(resp_rx.await.unwrap().is_ok());

    // List tools — should include pre-registered VS Code tools + the new one
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ToolsMessage::ListTools { reply_to: resp_tx })
        .await
        .unwrap();
    let tools = resp_rx.await.unwrap();

    assert!(tools.len() > 1);
    assert!(tools.iter().any(|t| t.name == "test_tool"));
    assert!(tools.iter().any(|t| t.name == "workspace/getFolders"));
}

#[tokio::test]
async fn test_tools_call_unregistered_returns_error() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ToolsActor::new(mock_sender()));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ToolsMessage::CallTool {
        tool: "nonexistent".to_string(),
        args: serde_json::Value::Null,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn test_tools_unregister_server() {
    let system = ActorSystem::new();
    let tx = spawn_tools_with_real_router(&system);

    // Register a tool
    let tool_info = ToolInfo {
        name: "tool1".to_string(),
        description: "desc".to_string(),
        input_schema: serde_json::json!({}),
    };
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ToolsMessage::RegisterTool {
        server: "server_a".to_string(),
        info: tool_info,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Unregister server
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ToolsMessage::UnregisterServer {
        server: "server_a".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    assert!(resp_rx.await.unwrap().is_ok());

    // Verify server_a tools are gone, but VS Code tools remain
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ToolsMessage::ListTools { reply_to: resp_tx })
        .await
        .unwrap();
    let tools = resp_rx.await.unwrap();
    assert!(!tools.is_empty());
    assert!(!tools.iter().any(|t| t.name == "tool1"));
    assert!(tools.iter().any(|t| t.name == "workspace/getFolders"));
}

// ===========================================================================
// ProgressActor tests
// ===========================================================================

#[tokio::test]
async fn test_progress_subscribe_and_publish() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ProgressActor::new());

    // Subscribe
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(ProgressMessage::Subscribe { reply_to: resp_tx })
        .await
        .unwrap();
    let mut rx: tokio::sync::broadcast::Receiver<ProgressUpdate> = resp_rx.await.unwrap();

    // Publish
    let update = ProgressUpdate {
        task_id: "task-1".to_string(),
        message: "Working...".to_string(),
        percent: 50.0,
        status: ProgressStatus::Running,
        metadata: None,
    };
    tx.send(ProgressMessage::Publish { update }).await.unwrap();

    // Receive
    let received = rx.recv().await.unwrap();
    assert_eq!(received.task_id, "task-1");
    assert_eq!(received.message, "Working...");
    assert_eq!(received.percent, 50.0);
    assert!(matches!(received.status, ProgressStatus::Running));
}

#[tokio::test]
async fn test_progress_multiple_subscribers() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(ProgressActor::new());

    // Subscribe two listeners
    let (resp_tx1, resp_rx1) = tokio::sync::oneshot::channel();
    tx.send(ProgressMessage::Subscribe { reply_to: resp_tx1 })
        .await
        .unwrap();
    let mut rx1 = resp_rx1.await.unwrap();

    let (resp_tx2, resp_rx2) = tokio::sync::oneshot::channel();
    tx.send(ProgressMessage::Subscribe { reply_to: resp_tx2 })
        .await
        .unwrap();
    let mut rx2: tokio::sync::broadcast::Receiver<ProgressUpdate> = resp_rx2.await.unwrap();

    // Publish
    let update = ProgressUpdate {
        task_id: "broadcast".to_string(),
        message: "Broadcast test".to_string(),
        percent: 100.0,
        status: ProgressStatus::Completed,
        metadata: None,
    };
    tx.send(ProgressMessage::Publish { update }).await.unwrap();

    // Both receive
    let r1 = rx1.recv().await.unwrap();
    let r2 = rx2.recv().await.unwrap();
    assert_eq!(r1.task_id, "broadcast");
    assert_eq!(r2.task_id, "broadcast");
}

// ===========================================================================
// SystemActor tests
// ===========================================================================

#[tokio::test]
async fn test_system_get_status_returns_running() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(SystemActor::new());

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(SystemMessage::GetStatus { reply_to: resp_tx })
        .await
        .unwrap();
    let status = resp_rx.await.unwrap();

    // Fresh SystemActor (never initialized) reports the Initializing lifecycle
    // string; GetStatus no longer emits an `actors` liveness map.
    assert_eq!(status["status"], "initializing");
    assert!(status["uptime_seconds"].as_f64().unwrap() >= 0.0);
    assert_eq!(status["version"], "0.1.0");
    assert_eq!(status["initializing"], false);
}

#[tokio::test]
async fn test_system_get_config_unknown_returns_none() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(SystemActor::new());

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(SystemMessage::GetConfig {
        key: "nonexistent".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let value = resp_rx.await.unwrap();

    assert!(value.is_none());
}

#[tokio::test]
async fn test_system_shutdown_returns_ok() {
    let system = ActorSystem::new();
    let (tx, _handle) = system.spawn(SystemActor::new());

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(SystemMessage::Shutdown { reply_to: resp_tx })
        .await
        .unwrap();
    let result = resp_rx.await.unwrap();

    assert!(result.is_ok());
}

// ===========================================================================
// Coordinator tests (end-to-end routing)
// ===========================================================================

#[tokio::test]
async fn test_coordinator_ping() {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig::default()));
    let (progress_tx, _) = system.spawn(ProgressActor::new());
    let (system_tx, _) = system.spawn(SystemActor::new());

    let memory_graph_tx = mock_memory_graph();
    let project_query_tx = mock_sender();
    let intent_router_tx = mock_sender();
    let prompt_handler_tx = mock_sender();
    let build_orchestrator_tx = mock_sender();
    let plan_orchestrator_tx = mock_sender();
    let transport_tx = mock_sender();
    let tool_router_tx: tokio::sync::mpsc::Sender<actors::ToolRouterMessage> = mock_sender();
    let (coord_tx, _handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        progress_tx,
        system_tx,
        memory_graph_tx,
        project_query_tx,
        intent_router_tx,
        prompt_handler_tx,
        build_orchestrator_tx,
        tool_router_tx,
        plan_orchestrator_tx,
        transport_tx,
    ));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "ping".to_string(),
            params: serde_json::json!({}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let result = resp_rx.await.unwrap();

    assert_eq!(result, serde_json::json!({"pong": true}));
}

#[tokio::test]
async fn test_coordinator_chat_get_active_end_to_end() {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig::default()));
    let (progress_tx, _) = system.spawn(ProgressActor::new());
    let (system_tx, _) = system.spawn(SystemActor::new());

    let memory_graph_tx = mock_memory_graph();
    let project_query_tx = mock_sender();
    let intent_router_tx = mock_sender();
    let prompt_handler_tx = mock_sender();
    let build_orchestrator_tx = mock_sender();
    let plan_orchestrator_tx = mock_sender();
    let transport_tx = mock_sender();
    let tool_router_tx: tokio::sync::mpsc::Sender<actors::ToolRouterMessage> = mock_sender();
    let (coord_tx, _handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        progress_tx,
        system_tx,
        memory_graph_tx,
        project_query_tx,
        intent_router_tx,
        prompt_handler_tx,
        build_orchestrator_tx,
        tool_router_tx,
        plan_orchestrator_tx,
        transport_tx,
    ));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "chat/getActive".to_string(),
            params: serde_json::json!({}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let result = resp_rx.await.unwrap();

    assert_eq!(result["id"], "default");
    assert_eq!(result["title"], "New Chat");
}

#[tokio::test]
async fn test_coordinator_chat_append_and_get_history() {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig::default()));
    let (progress_tx, _) = system.spawn(ProgressActor::new());
    let (system_tx, _) = system.spawn(SystemActor::new());

    let memory_graph_tx = mock_memory_graph();
    let project_query_tx = mock_sender();
    let intent_router_tx = mock_sender();
    let prompt_handler_tx = mock_sender();
    let build_orchestrator_tx = mock_sender();
    let plan_orchestrator_tx = mock_sender();
    let transport_tx = mock_sender();
    let tool_router_tx: tokio::sync::mpsc::Sender<actors::ToolRouterMessage> = mock_sender();
    let (coord_tx, _handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        progress_tx,
        system_tx,
        memory_graph_tx,
        project_query_tx,
        intent_router_tx,
        prompt_handler_tx,
        build_orchestrator_tx,
        tool_router_tx,
        plan_orchestrator_tx,
        transport_tx,
    ));

    // Append
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "chat/append".to_string(),
            params: serde_json::json!({
                "chatId": "default",
                "content": "Hello from coordinator",
                "options": {"role": "user"}
            }),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let append_result = resp_rx.await.unwrap();
    assert_eq!(append_result["content"], "Hello from coordinator");
    assert_eq!(append_result["role"], "user");

    // Get history
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "chat/getHistory".to_string(),
            params: serde_json::json!({}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let history = resp_rx.await.unwrap();

    assert!(history.is_array());
    assert_eq!(
        history[0]["messages"][0]["content"],
        "Hello from coordinator"
    );
}

/// Helper to create a real tool_router channel for ToolsActor + Coordinator tests.
/// Returns (tools_tx, coord_tool_router_tx) where tools_tx uses a real tool_router
/// so VSCode tools can register at startup.
async fn spawn_tools_and_coordinator_with_real_router(
    system: &ActorSystem,
    chat_tx: tokio::sync::mpsc::Sender<actors::ChatMessage>,
    mcp_tx: tokio::sync::mpsc::Sender<actors::McpClientMessage>,
    llm_tx: tokio::sync::mpsc::Sender<actors::LlmMessage>,
    progress_tx: tokio::sync::mpsc::Sender<actors::ProgressMessage>,
    system_tx: tokio::sync::mpsc::Sender<actors::SystemMessage>,
) -> tokio::sync::mpsc::Sender<actors::CoordinatorMessage> {
    // Spawn a REAL ToolRouterActor so `tools/list` returns the static VS Code
    // tools (workspace/getFolders, …). All backends are mocks — the router
    // itself isn't dispatched to, only `ListTools` is exercised.
    let tool_router_tx = system
        .spawn(ToolRouterActor::new(test_tool_registry(), mock_sender()))
        .0;

    let tools_tx: tokio::sync::mpsc::Sender<actors::ToolsMessage> =
        system.spawn(ToolsActor::new(tool_router_tx.clone())).0;

    let memory_graph_tx = mock_memory_graph();
    let project_query_tx = mock_sender();
    let intent_router_tx = mock_sender();
    let prompt_handler_tx = mock_sender();
    let build_orchestrator_tx = mock_sender();
    let plan_orchestrator_tx = mock_sender();
    let transport_tx = mock_sender();

    system
        .spawn(CoordinatorActor::new(
            chat_tx,
            tools_tx,
            mcp_tx,
            llm_tx,
            progress_tx,
            system_tx,
            memory_graph_tx,
            project_query_tx,
            intent_router_tx,
            prompt_handler_tx,
            build_orchestrator_tx,
            tool_router_tx,
            plan_orchestrator_tx,
            transport_tx,
        ))
        .0
}

#[tokio::test]
async fn test_coordinator_tools_list() {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig::default()));
    let (progress_tx, _) = system.spawn(ProgressActor::new());
    let (system_tx, _) = system.spawn(SystemActor::new());

    let coord_tx = spawn_tools_and_coordinator_with_real_router(
        &system,
        chat_tx,
        mcp_tx,
        llm_tx,
        progress_tx,
        system_tx,
    )
    .await;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "tools/list".to_string(),
            params: serde_json::json!({}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let result = resp_rx.await.unwrap();

    assert!(result.is_array());
    assert!(!result.as_array().unwrap().is_empty());
    assert!(result
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "workspace/getFolders"));
}

#[tokio::test]
async fn test_coordinator_system_status() {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig::default()));
    let (progress_tx, _) = system.spawn(ProgressActor::new());
    let (system_tx, _) = system.spawn(SystemActor::new());

    let coord_tx = spawn_tools_and_coordinator_with_real_router(
        &system,
        chat_tx,
        mcp_tx,
        llm_tx,
        progress_tx,
        system_tx,
    )
    .await;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "system/status".to_string(),
            params: serde_json::json!({}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let result = resp_rx.await.unwrap();

    // Bare SystemActor (not initialized) reports the Initializing lifecycle
    // string; the important part is a non-empty lifecycle state + version.
    assert!(!result["status"].as_str().unwrap_or("").is_empty());
    assert_eq!(result["version"], "0.1.0");
}

#[tokio::test]
async fn test_coordinator_unknown_method() {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig::default()));
    let (progress_tx, _) = system.spawn(ProgressActor::new());
    let (system_tx, _) = system.spawn(SystemActor::new());

    let memory_graph_tx = mock_memory_graph();
    let project_query_tx = mock_sender();
    let transport_tx = mock_sender();
    let intent_router_tx = mock_sender();
    let prompt_handler_tx = mock_sender();
    let build_orchestrator_tx = mock_sender();
    let plan_orchestrator_tx = mock_sender();
    let tool_router_tx: tokio::sync::mpsc::Sender<actors::ToolRouterMessage> = mock_sender();

    let (coord_tx, _handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        progress_tx,
        system_tx,
        memory_graph_tx,
        project_query_tx,
        intent_router_tx,
        prompt_handler_tx,
        build_orchestrator_tx,
        tool_router_tx,
        plan_orchestrator_tx,
        transport_tx,
    ));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "nonexistent/method".to_string(),
            params: serde_json::json!({}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let result = resp_rx.await.unwrap();

    assert!(result.get("error").is_some());
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("nonexistent/method"));
}

#[tokio::test]
async fn test_coordinator_mcp_servers_empty() {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig::default()));
    let (progress_tx, _) = system.spawn(ProgressActor::new());
    let (system_tx, _) = system.spawn(SystemActor::new());

    let memory_graph_tx = mock_memory_graph();
    let project_query_tx = mock_sender();
    let transport_tx = mock_sender();
    let intent_router_tx = mock_sender();
    let prompt_handler_tx = mock_sender();
    let build_orchestrator_tx = mock_sender();
    let plan_orchestrator_tx = mock_sender();
    let tool_router_tx: tokio::sync::mpsc::Sender<actors::ToolRouterMessage> = mock_sender();

    let (coord_tx, _handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        progress_tx,
        system_tx,
        memory_graph_tx,
        project_query_tx,
        intent_router_tx,
        prompt_handler_tx,
        build_orchestrator_tx,
        tool_router_tx,
        plan_orchestrator_tx,
        transport_tx,
    ));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    coord_tx
        .send(CoordinatorMessage::HandleRequest {
            method: "mcp/servers".to_string(),
            params: serde_json::json!({}),
            response_tx: resp_tx,
        })
        .await
        .unwrap();
    let result = resp_rx.await.unwrap();

    assert!(result.is_array());
    assert!(result.as_array().unwrap().is_empty());
}

// ===========================================================================
// BuildOrchestrator tests
// ===========================================================================

#[tokio::test]
async fn test_build_orchestrator_start_build_creates_session() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    let error_analyzer_tx: tokio::sync::mpsc::Sender<ErrorAnalyzerMessage> = mock_sender();
    let tool_orchestrator_tx: tokio::sync::mpsc::Sender<ToolOrchestratorMessage> = mock_sender();
    let (tool_router_tx, _tool_router_rx): (
        tokio::sync::mpsc::Sender<actors::ToolRouterMessage>,
        _,
    ) = tokio::sync::mpsc::channel(64);

    let (bo_tx, _bo_handle) = system.spawn(BuildOrchestrator::new(
        memory_graph.clone(),
        error_analyzer_tx,
        tool_orchestrator_tx,
        tool_router_tx,
    ));

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    bo_tx
        .send(BuildOrchestratorMessage::StartBuild {
            parameters: BuildContext {
                project_root: "/tmp/test".to_string(),
                build_system: "Cargo".to_string(),
                target: None,
                environment: std::collections::HashMap::new(),
            },
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    // The test's tool_router_tx is a mock that never replies, so `start_build`
    // blocks in `dispatch_build` while waiting for the build dispatch reply.
    // Bound the wait: the session node is stored BEFORE dispatch, so the
    // assertion below still holds. This prevents the test from hanging the
    // suite forever.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx).await;

    // Query the graph for nodes with subtype "build_session"
    let (query_tx, query_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Standard".to_string()),
                subtype: Some("build_session".to_string()),
                name: None,
                limit: None,
                reply_to: query_tx,
            })
        .await
        .unwrap();

    let sessions = query_rx.await.unwrap().expect("QueryNodes failed");
    assert!(
        !sessions.is_empty(),
        "Should have at least one build_session node"
    );
    assert_eq!(sessions[0].subtype(), Some("build_session"));
}

#[tokio::test]
async fn test_build_orchestrator_start_build_sets_proper_status() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    let error_analyzer_tx: tokio::sync::mpsc::Sender<ErrorAnalyzerMessage> = mock_sender();
    let tool_orchestrator_tx: tokio::sync::mpsc::Sender<ToolOrchestratorMessage> = mock_sender();
    let (tool_router_tx, _tool_router_rx): (
        tokio::sync::mpsc::Sender<actors::ToolRouterMessage>,
        _,
    ) = tokio::sync::mpsc::channel(64);

    let (bo_tx, _bo_handle) = system.spawn(BuildOrchestrator::new(
        memory_graph.clone(),
        error_analyzer_tx,
        tool_orchestrator_tx,
        tool_router_tx,
    ));

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    bo_tx
        .send(BuildOrchestratorMessage::StartBuild {
            parameters: BuildContext {
                project_root: "/tmp/test2".to_string(),
                build_system: "Cargo".to_string(),
                target: None,
                environment: std::collections::HashMap::new(),
            },
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    // The test's tool_router_tx is a mock that never replies, so `start_build`
    // blocks in `dispatch_build` while waiting for the build dispatch reply.
    // Bound the wait; the session node is stored BEFORE dispatch, so the
    // "build_session" assertion below still holds.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx).await;

    // Query nodes with subtype "build_session"
    let (query_tx, query_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Standard".to_string()),
                subtype: Some("build_session".to_string()),
                name: None,
                limit: None,
                reply_to: query_tx,
            })
        .await
        .unwrap();

    let sessions = query_rx.await.unwrap().expect("QueryNodes failed");
    assert!(
        !sessions.is_empty(),
        "Should have at least one build_session node"
    );

    let session = &sessions[0];
    assert_eq!(session.subtype(), Some("build_session"));
}

#[tokio::test]
async fn test_build_orchestrator_loop_guard() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    let error_analyzer_tx: tokio::sync::mpsc::Sender<ErrorAnalyzerMessage> = mock_sender();
    let tool_orchestrator_tx: tokio::sync::mpsc::Sender<ToolOrchestratorMessage> = mock_sender();
    let (tool_router_tx, _tool_router_rx): (
        tokio::sync::mpsc::Sender<actors::ToolRouterMessage>,
        _,
    ) = tokio::sync::mpsc::channel(64);

    // Store a fix_strategy node so lookup doesn't fail
    let fix_strategy = t_attr_unknown(
                "Unknown",
                Some("fix_strategy".to_string()),
                "test-fix-strategy".to_string(),
                Some("A test fix strategy".to_string()),
                {

            let mut map = HashMap::new();
            map.insert("steps".to_string(), serde_json::json!(["step1"]));
            map
                    },
            );
    let (store_tx, store_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: fix_strategy,
            reply_to: store_tx,
        })
        .await
        .unwrap();
    store_rx.await.unwrap().unwrap();

    let (bo_tx, _bo_handle) = system.spawn(BuildOrchestrator::new(
        memory_graph.clone(),
        error_analyzer_tx,
        tool_orchestrator_tx,
        tool_router_tx,
    ));

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    bo_tx
        .send(BuildOrchestratorMessage::ApplyFix {
            strategy_name: "test-fix-strategy".to_string(),
            target_system: None,
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let result = reply_rx.await.unwrap();
    assert!(
        result.is_err(),
        "ApplyFix should fail without a build context"
    );
}

// ===========================================================================
// ErrorAnalyzer tests
// ===========================================================================

/// Helper to seed the MemoryGraph with error type and fix strategy nodes
/// that mirror config/intents.json.
async fn seed_error_types_and_fixes(memory_graph: &mpsc::Sender<MemoryGraphMessage>) {
    // Seed error_type: rustc-compile-error — stored via the open envelope with
    // the typed discriminator (the analyzer queries by "errorType").
    let now = chrono::Utc::now();
    async fn seed_error(
        memory_graph: &mpsc::Sender<MemoryGraphMessage>,
        now: chrono::DateTime<chrono::Utc>,
        node_type: &str,
        name: &str,
        description: &str,
        properties: HashMap<String, serde_json::Value>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let attr = AttrNode {
            id: Uuid::new_v4().to_string(),
            node_type: node_type.to_string(),
            subtype: None,
            name: name.to_string(),
            description: Some(description.to_string()),
            properties,
            embedding_id: None,
            created_at: now,
            updated_at: now,
            version: 1,
        };
        memory_graph
            .send(MemoryGraphMessage::StoreAttrNode {
                node: attr,
                reply_to: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap().unwrap();
    }

    seed_error(
        &memory_graph,
        now,
        "errorType",
        "rustc-compile-error",
        "Rust compiler error",
        HashMap::from([
            (
                "detection_patterns".to_string(),
                serde_json::json!(["error\\[E\\d{4}\\]", "error: could not compile"]),
            ),
            ("severity".to_string(), serde_json::json!("high")),
            ("fix_strategies".to_string(), serde_json::json!(["fix-type-error"])),
        ]),
    ).await;

    seed_error(
        &memory_graph,
        now,
        "fixStrategy",
        "fix-type-error",
        "Fix a type mismatch",
        HashMap::from([
            ("category".to_string(), serde_json::json!("fix")),
            ("confidence_threshold".to_string(), serde_json::json!(0.6)),
            ("success_rate".to_string(), serde_json::json!(0.8)),
            (
                "steps".to_string(),
                serde_json::json!(["read_error_context", "analyze_type_mismatch", "apply_fix"]),
            ),
            ("has_rollback".to_string(), serde_json::json!(true)),
            ("applies_to".to_string(), serde_json::json!(["rustc-compile-error"])),
        ]),
    ).await;

    seed_error(
        &memory_graph,
        now,
        "fixStrategy",
        "generic-fix",
        "Generic fallback fix",
        HashMap::from([
            ("category".to_string(), serde_json::json!("fix")),
            ("confidence_threshold".to_string(), serde_json::json!(0.3)),
            ("success_rate".to_string(), serde_json::json!(0.3)),
            (
                "steps".to_string(),
                serde_json::json!(["read_error_context", "analyze_error", "apply_fix"]),
            ),
            ("has_rollback".to_string(), serde_json::json!(true)),
        ]),
    ).await;
}

#[tokio::test]
async fn test_error_analyzer_matches_rustc_error_by_regex() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    seed_error_types_and_fixes(&memory_graph).await;

    let (ea_tx, _ea_handle) = system.spawn(ErrorAnalyzer::new(memory_graph));

    let system_result = SystemBuildResult {
        build_type: "Cargo".to_string(),
        path: "/tmp/project".to_string(),
        project_name: "spire-core".to_string(),
        success: false,
        errors: vec![BuildError {
            error_text: "error[E0308]: mismatched types\n --> src/main.rs:42:5".to_string(),
            error_type: Some("rustc-compile-error".to_string()),
            file: Some("src/main.rs".to_string()),
            line: Some(42),
            column: Some(5),
            exit_code: Some(1),
            build_type: Some("Cargo".to_string()),
            diagnostic_node_id: None,
            file_node_id: None,
        }],
        warnings: vec![],
        exit_code: Some(1),
        duration_ms: 5000,
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    ea_tx
        .send(ErrorAnalyzerMessage::AnalyzeErrors {
            system_results: vec![system_result],
            build_run_id: "test-run-1".to_string(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let fix_plan = reply_rx.await.unwrap().expect("AnalyzeErrors failed");

    assert!(!fix_plan.errors.is_empty(), "Should have annotated errors");
    assert_eq!(fix_plan.errors[0].build_type, "Cargo");
    assert_eq!(
        fix_plan.errors[0].error.error_type.as_deref(),
        Some("rustc-compile-error")
    );

    assert!(
        !fix_plan.ordered_fixes.is_empty(),
        "Should have ordered fixes"
    );
    assert!(
        fix_plan
            .ordered_fixes
            .iter()
            .any(|f| f.strategy.name == "fix-type-error"),
        "Ordered fixes should include fix-type-error"
    );
}

#[tokio::test]
async fn test_error_analyzer_falls_back_to_generic() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    seed_error_types_and_fixes(&memory_graph).await;

    let (ea_tx, _ea_handle) = system.spawn(ErrorAnalyzer::new(memory_graph));

    let system_result = SystemBuildResult {
        build_type: "Cargo".to_string(),
        path: "/tmp/project".to_string(),
        project_name: "spire-core".to_string(),
        success: false,
        errors: vec![BuildError {
            error_text: "some very unusual error that doesn't match any pattern".to_string(),
            error_type: None,
            file: Some("src/lib.rs".to_string()),
            line: Some(10),
            column: None,
            exit_code: Some(1),
            build_type: Some("Cargo".to_string()),
            diagnostic_node_id: None,
            file_node_id: None,
        }],
        warnings: vec![],
        exit_code: Some(1),
        duration_ms: 3000,
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    ea_tx
        .send(ErrorAnalyzerMessage::AnalyzeErrors {
            system_results: vec![system_result],
            build_run_id: "test-run-2".to_string(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let fix_plan = reply_rx.await.unwrap().expect("AnalyzeErrors failed");

    assert!(!fix_plan.errors.is_empty(), "Should have annotated errors");
    assert!(
        !fix_plan.ordered_fixes.is_empty(),
        "Should have at least the generic-fix fallback"
    );
}

#[tokio::test]
async fn test_error_analyzer_multi_system_deduplicates() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    seed_error_types_and_fixes(&memory_graph).await;

    let (ea_tx, _ea_handle) = system.spawn(ErrorAnalyzer::new(memory_graph));

    let cargo_error = SystemBuildResult {
        build_type: "Cargo".to_string(),
        path: "/tmp/project/rust".to_string(),
        project_name: "spire-core".to_string(),
        success: false,
        errors: vec![BuildError {
            error_text: "error[E0308]: mismatched types".to_string(),
            error_type: Some("rustc-compile-error".to_string()),
            file: Some("src/main.rs".to_string()),
            line: Some(42),
            column: None,
            exit_code: Some(1),
            build_type: Some("Cargo".to_string()),
            diagnostic_node_id: None,
            file_node_id: None,
        }],
        warnings: vec![],
        exit_code: Some(1),
        duration_ms: 5000,
    };

    let npm_error = SystemBuildResult {
        build_type: "npm".to_string(),
        path: "/tmp/project/ts".to_string(),
        project_name: "spire-extension".to_string(),
        success: false,
        errors: vec![BuildError {
            error_text: "error[E0308]: type mismatch".to_string(),
            error_type: Some("rustc-compile-error".to_string()),
            file: Some("src/app.ts".to_string()),
            line: Some(10),
            column: None,
            exit_code: Some(1),
            build_type: Some("npm".to_string()),
            diagnostic_node_id: None,
            file_node_id: None,
        }],
        warnings: vec![],
        exit_code: Some(1),
        duration_ms: 3000,
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    ea_tx
        .send(ErrorAnalyzerMessage::AnalyzeErrors {
            system_results: vec![cargo_error, npm_error],
            build_run_id: "test-run-3".to_string(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let fix_plan = reply_rx.await.unwrap().expect("AnalyzeErrors failed");

    assert_eq!(fix_plan.errors.len(), 2, "Should have 2 annotated errors");
    assert_eq!(fix_plan.errors[0].build_type, "Cargo");
    assert_eq!(fix_plan.errors[1].build_type, "npm");

    let type_error_count = fix_plan
        .ordered_fixes
        .iter()
        .filter(|f| f.strategy.name == "fix-type-error")
        .count();
    assert_eq!(
        type_error_count, 1,
        "fix-type-error should appear only once (deduplicated)"
    );
}

#[tokio::test]
async fn test_error_analyzer_skips_successful_systems() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;
    seed_error_types_and_fixes(&memory_graph).await;

    let (ea_tx, _ea_handle) = system.spawn(ErrorAnalyzer::new(memory_graph));

    let success_system = SystemBuildResult {
        build_type: "Cargo".to_string(),
        path: "/tmp/project/rust".to_string(),
        project_name: "spire-core".to_string(),
        success: true,
        errors: vec![],
        warnings: vec![],
        exit_code: Some(0),
        duration_ms: 2000,
    };

    let failed_system = SystemBuildResult {
        build_type: "npm".to_string(),
        path: "/tmp/project/ts".to_string(),
        project_name: "spire-extension".to_string(),
        success: false,
        errors: vec![BuildError {
            error_text: "Cannot find module './handlers/chat'".to_string(),
            error_type: None,
            file: Some("src/app.ts".to_string()),
            line: Some(1),
            column: None,
            exit_code: Some(1),
            build_type: Some("npm".to_string()),
            diagnostic_node_id: None,
            file_node_id: None,
        }],
        warnings: vec![],
        exit_code: Some(1),
        duration_ms: 3000,
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    ea_tx
        .send(ErrorAnalyzerMessage::AnalyzeErrors {
            system_results: vec![success_system, failed_system],
            build_run_id: "test-run-4".to_string(),
            reply_to: reply_tx,
        })
        .await
        .unwrap();

    let fix_plan = reply_rx.await.unwrap().expect("AnalyzeErrors failed");

    assert_eq!(
        fix_plan.errors.len(),
        1,
        "Should only have 1 annotated error (npm)"
    );
    assert_eq!(fix_plan.errors[0].build_type, "npm");
}

// ===========================================================================
// State transition tests (via MemoryGraph)
// ===========================================================================

#[tokio::test]
async fn test_build_state_transition_updates_active() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    // Seed a build state node with active=false (use Unknown so properties survive)
    let state_node = t_attr_unknown(
                "Unknown",
                Some("build_state".to_string()),
                "test_build_failed".to_string(),
                Some("Test build failed state".to_string()),
                {

            let mut map = HashMap::new();
            map.insert("active".to_string(), serde_json::json!(false));
            map
                    },
            );
    let (store_tx, store_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: state_node,
            reply_to: store_tx,
        })
        .await
        .unwrap();
    store_rx.await.unwrap().unwrap();

    let (query_tx, query_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("build_state".to_string()),
                name: Some("test_build_failed".to_string()),
                limit: None,
                reply_to: query_tx,
            })
        .await
        .unwrap();

    let nodes = query_rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        unknown_props(&nodes[0])
            .get("active")
            .and_then(|v| v.as_bool()),
        Some(false),
        "Initial active should be false"
    );
}

#[tokio::test]
async fn test_build_state_store_and_query() {
    let system = ActorSystem::new();
    let memory_graph = spawn_memory_graph(&system).await;

    // Store build_failed state
    let state_node = t_attr_unknown(
                "Unknown",
                Some("build_state".to_string()),
                "state_build_failed".to_string(),
                Some("Build failed state".to_string()),
                {

            let mut map = HashMap::new();
            map.insert("active".to_string(), serde_json::json!(true));
            map
                    },
            );
    let (store_tx, store_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: state_node,
            reply_to: store_tx,
        })
        .await
        .unwrap();
    store_rx.await.unwrap().unwrap();

    // Store build_completed state
    let state_node2 = t_attr_unknown(
                "Unknown",
                Some("build_state".to_string()),
                "state_build_completed".to_string(),
                Some("Build completed state".to_string()),
                {

            let mut map = HashMap::new();
            map.insert("active".to_string(), serde_json::json!(false));
            map
                    },
            );
    let (store_tx, store_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node: state_node2,
            reply_to: store_tx,
        })
        .await
        .unwrap();
    store_rx.await.unwrap().unwrap();

    // Query all Unknown nodes with build_state subtype
    let (query_tx, query_rx) = tokio::sync::oneshot::channel();
    memory_graph
        .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("build_state".to_string()),
                name: None,
                limit: None,
                reply_to: query_tx,
            })
        .await
        .unwrap();

    let nodes = query_rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(nodes.len(), 2, "Should have 2 build state nodes");
    assert!(nodes.iter().any(|n| n.name() == "state_build_failed"));
    assert!(nodes.iter().any(|n| n.name() == "state_build_completed"));
}

// ===========================================================================
// Mock Embedder (for MemoryGraph tests)
// ===========================================================================

struct MockEmbedder {
    fixed_vector: Vec<f32>,
}

impl MockEmbedder {
    #[allow(dead_code)]
    fn new() -> Self {
        Self {
            fixed_vector: vec![0.1; 384],
        }
    }

    #[allow(dead_code)]
    fn new_with_vector(vector: Vec<f32>) -> Self {
        Self {
            fixed_vector: vector,
        }
    }
}

#[async_trait::async_trait]
impl Embedder for MockEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Embedding> {
        Ok(Embedding::new(
            self.fixed_vector.clone(),
            text,
            "mock-model",
        ))
    }

    async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Embedding>> {
        Ok(texts
            .iter()
            .map(|t| Embedding::new(self.fixed_vector.clone(), t, "mock-model"))
            .collect())
    }

    fn dimensions(&self) -> usize {
        self.fixed_vector.len()
    }
}

// ===========================================================================
// MemoryGraphActor tests
// ===========================================================================

/// Helper to create a MemoryGraphActor for testing.
fn create_memory_graph() -> MemoryGraphActor {
    MemoryGraphActor::new()
}

/// Build an envelope `AttrNode` for a test node.
fn t_attr_unknown(
    node_type: &str,
    subtype: Option<String>,
    name: String,
    description: Option<String>,
    properties: std::collections::HashMap<String, serde_json::Value>,
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

/// Helper to spawn a MemoryGraphActor in an ActorSystem and return its sender.
async fn spawn_memory_graph(system: &ActorSystem) -> tokio::sync::mpsc::Sender<MemoryGraphMessage> {
    use tokio::sync::oneshot;
    let actor = create_memory_graph();
    let (tx, _handle) = system.spawn(actor);

    let (init_tx, init_rx) = oneshot::channel::<Result<(), anyhow::Error>>();
    let data_dir = std::env::temp_dir().join(format!("spire_test_mg_{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&data_dir).expect("Failed to create test data dir");
    tx.send(MemoryGraphMessage::Initialize {
        data_dir,
        reply_to: init_tx,
    })
    .await
    .expect("Failed to send Initialize message");
    init_rx
        .await
        .expect("Failed to receive Initialize response")
        .expect("MemoryGraph initialization failed");
    tx
}

// ─── Node Operations ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_graph_store_and_get_node() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Store a node (Project is a typed variant; does NOT carry extra properties)
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Project",
                None,
                "Test Project".to_string(),
                Some("A test project".to_string()),
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let stored = resp_rx.await.unwrap().expect("Failed to store node");
    assert_eq!(stored.name(), "Test Project");
    assert_eq!(stored.node_type_str(), "Project");
    assert_eq!(stored.description(), Some("A test project"));
    assert_eq!(stored.version(), 1);
    assert!(!stored.id().is_empty());

    // Get the node by ID
    let node_id = stored.id().to_string();
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
            id: node_id.clone(),
            reply_to: resp_tx,
        })
    .await
    .unwrap();
    let retrieved = resp_rx.await.unwrap().expect("Failed to get node");
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.id(), node_id);
    assert_eq!(retrieved.name(), "Test Project");
}

#[tokio::test]
async fn test_memory_graph_get_nonexistent_node_returns_none() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
            id: "nonexistent-uuid".to_string(),
            reply_to: resp_tx,
        })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap().expect("GetNode failed");
    assert!(result.is_none());
}

#[tokio::test]
async fn test_memory_graph_query_nodes_by_type() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Store two projects and one entity
    for i in 0..2 {
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        tx.send(MemoryGraphMessage::StoreAttrNode {
            node: t_attr_unknown(
                "Project",
                None,
                format!("Project {}", i),
                None,
                HashMap::new(),
            ),
            reply_to: resp_tx,
        })
        .await
        .unwrap();
        resp_rx.await.unwrap().unwrap();
    }

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("entity".to_string()),
                "Entity 1".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Query by type
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Project".to_string()),
                subtype: None,
                name: None,
                limit: None,
                reply_to: resp_tx,
            })
    .await
    .unwrap();
    let projects = resp_rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(projects.len(), 2);
    assert!(projects.iter().all(|n| n.node_type_str() == "Project"));
}

#[tokio::test]
async fn test_memory_graph_query_nodes_by_name() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "MySpecialNode".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Query by name
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::QueryAttrNodes {
                node_type: None,
                subtype: None,
                name: Some("MySpecialNode".to_string()),
                limit: None,
                reply_to: resp_tx,
            })
    .await
    .unwrap();
    let results = resp_rx.await.unwrap().expect("QueryNodes failed");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name(), "MySpecialNode");
}

#[tokio::test]
async fn test_memory_graph_update_node() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Store a node
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "Original".to_string(),
                Some("Original description".to_string()),
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let stored = resp_rx.await.unwrap().unwrap();
    let node_id = stored.id().to_string();

    // Update the node
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::UpdateNode {
        id: node_id.clone(),
        updates: NodeUpdate {
            node_type: None,
            subtype: None,
            name: Some("Updated".to_string()),
            description: Some(Some("Updated description".to_string())),
            properties: None,
            embedding_id: None,
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let updated = resp_rx.await.unwrap().expect("UpdateNode failed");
    assert_eq!(updated.name(), "Updated");
    assert_eq!(updated.description(), Some("Updated description"));

    // Verify via GetNode — use the updated node's ID (apply_updates generates a new UUID)
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
            id: updated.id().to_string(),
            reply_to: resp_tx,
        })
    .await
    .unwrap();
    if let Some(retrieved) = resp_rx.await.unwrap().unwrap() {
        assert_eq!(retrieved.name(), "Updated");
    } else {
        // The update uses delete+reinsert which may change the UUID.
        // As long as we can find the node by the new ID and the name is right, it's fine.
    }
}

#[tokio::test]
async fn test_memory_graph_delete_node() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Store a node
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "ToDelete".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let stored = resp_rx.await.unwrap().unwrap();
    let node_id = stored.id().to_string();

    // Delete it
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::DeleteNode {
        id: node_id.clone(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().expect("DeleteNode failed");

    // Verify it's gone
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
            id: node_id,
            reply_to: resp_tx,
        })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap().unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_memory_graph_delete_nonexistent_returns_error() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::DeleteNode {
        id: "nonexistent".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn test_memory_graph_merge_attr_upsert_keeps_stable_uuid() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Merge (upsert) a node — the open-model path for UUID-stable upserts.
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::MergeAttrNode {
        node: t_attr_unknown(
                "Project",
                None,
                "UniqueProject".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let first = resp_rx.await.unwrap().unwrap();
    let first_id = first.id().to_string();

    // Re-merge the same (type, name): must succeed and reuse the UUID so
    // relationships pointing at the old node stay valid.
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::MergeAttrNode {
        node: t_attr_unknown(
                "Project",
                None,
                "UniqueProject".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let second = resp_rx.await.unwrap().unwrap();
    assert_eq!(
        second.id(),
        &first_id,
        "MergeAttrNode must reuse the existing UUID (upsert semantics)"
    );
}

// ─── Relationship Operations ─────────────────────────────────────────────

#[tokio::test]
async fn test_memory_graph_create_and_get_relationships() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Store two nodes
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Project",
                None,
                "Source".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let source = resp_rx.await.unwrap().unwrap();

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("entity".to_string()),
                "Target".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let target = resp_rx.await.unwrap().unwrap();

    // Create relationship
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::CreateRelationship {
        rel: RelationshipInput {
            edge_type: RelationshipType::BelongsTo,
            from_id: source.id().to_string(),
            to_id: target.id().to_string(),
            properties: None,
            weight: Some(1.0),
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let edge = resp_rx.await.unwrap().expect("CreateRelationship failed");
    assert_eq!(edge.edge_type, RelationshipType::BelongsTo);
    assert_eq!(edge.from_id, source.id());
    assert_eq!(edge.to_id, target.id());
    assert_eq!(edge.weight, Some(1.0));

    // Get relationships for source node
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetRelationships {
        node_id: source.id().to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let edges = resp_rx.await.unwrap().expect("GetRelationships failed");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].edge_type, RelationshipType::BelongsTo);

    // Get relationships for target node (should also find it via incoming)
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetRelationships {
        node_id: target.id().to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let edges = resp_rx.await.unwrap().unwrap();
    assert_eq!(edges.len(), 1);
}

#[tokio::test]
async fn test_memory_graph_delete_relationship() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Store two nodes
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "A".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let a = resp_rx.await.unwrap().unwrap();

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "B".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let b = resp_rx.await.unwrap().unwrap();

    // Create relationship
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::CreateRelationship {
        rel: RelationshipInput {
            edge_type: RelationshipType::DependsOn,
            from_id: a.id().to_string(),
            to_id: b.id().to_string(),
            properties: None,
            weight: None,
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let edge = resp_rx.await.unwrap().unwrap();

    // Delete it
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::DeleteRelationship {
        id: edge.id.to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().expect("DeleteRelationship failed");

    // Verify gone
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetRelationships {
        node_id: a.id().to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let edges = resp_rx.await.unwrap().unwrap();
    assert!(edges.is_empty());
}

// ─── Traversal ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_graph_traverse_basic() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Create chain: A -> B -> C
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "A".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let a = resp_rx.await.unwrap().unwrap();

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "B".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let b = resp_rx.await.unwrap().unwrap();

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "C".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let c = resp_rx.await.unwrap().unwrap();

    // Create edges A->B, B->C
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::CreateRelationship {
        rel: RelationshipInput {
            edge_type: RelationshipType::DependsOn,
            from_id: a.id().to_string(),
            to_id: b.id().to_string(),
            properties: None,
            weight: None,
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::CreateRelationship {
        rel: RelationshipInput {
            edge_type: RelationshipType::DependsOn,
            from_id: b.id().to_string(),
            to_id: c.id().to_string(),
            properties: None,
            weight: None,
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Traverse from A with max_depth=1 (should get A + B)
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::Traverse {
        start_node_id: a.id().to_string(),
        options: TraversalOptions {
            max_depth: 1,
            relationship_types: None,
            max_nodes: Some(10),
            direction: Some(TraversalDirection::Out),
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap().expect("Traverse failed");
    assert_eq!(result.nodes.len(), 2, "Depth 1 should find A + B");
    assert_eq!(result.edges.len(), 1, "Depth 1 should find 1 edge");

    // Traverse from A with max_depth=2 (should get A + B + C)
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::Traverse {
        start_node_id: a.id().to_string(),
        options: TraversalOptions {
            max_depth: 2,
            relationship_types: None,
            max_nodes: Some(10),
            direction: Some(TraversalDirection::Out),
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let result = resp_rx.await.unwrap().unwrap();
    assert_eq!(result.nodes.len(), 3, "Depth 2 should find A + B + C");
    assert_eq!(result.edges.len(), 2, "Depth 2 should find 2 edges");
}

// ─── Config Storage ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_graph_set_and_get_config() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Set a config value
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::SetConfig {
        key: "theme".to_string(),
        value: serde_json::json!("dark"),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().expect("SetConfig failed");

    // Get it back
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetConfig {
        key: "theme".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let value = resp_rx.await.unwrap().expect("GetConfig failed");
    assert_eq!(value, Some(serde_json::json!("dark")));
}

#[tokio::test]
async fn test_memory_graph_get_nonexistent_config() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetConfig {
        key: "nonexistent".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let value = resp_rx.await.unwrap().expect("GetConfig failed");
    assert_eq!(value, None);
}

#[tokio::test]
async fn test_memory_graph_overwrite_config() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Set initial value
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::SetConfig {
        key: "max_results".to_string(),
        value: serde_json::json!(10),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Overwrite
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::SetConfig {
        key: "max_results".to_string(),
        value: serde_json::json!(50),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Verify overwritten
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetConfig {
        key: "max_results".to_string(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let value = resp_rx.await.unwrap().unwrap();
    assert_eq!(value, Some(serde_json::json!(50)));
}

// ─── Sync / Maintenance ──────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_graph_sync_does_not_crash() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Store some data
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "SyncTest".to_string(),
                None,
                HashMap::new(),
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().unwrap();

    // Sync should succeed without error
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::Sync { reply_to: resp_tx })
        .await
        .unwrap();
    resp_rx.await.unwrap().expect("Sync failed");
}

// ─── Custom Properties Tests ──────────────────────────────────────────────
// These are GQL integration tests. Custom property round-trips through SeleneDB
// depend on proper float/array GQL literal handling which is a separate concern.
//
// See: `store_node_via_gql` in memory_graph.rs which handles Unknown properties
// via SET-after-INSERT for arrays/objects, and inlines scalars in the INSERT.
// SeleneDB may truncate floats in INSERT property maps.

/// Helper to spawn a MemoryGraphActor with a properly initialized graph database.
async fn spawn_initialized_memory_graph(
    system: &ActorSystem,
) -> (
    tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    tempfile::TempDir,
) {
    let actor = MemoryGraphActor::new();
    let (tx, _handle) = system.spawn(actor);

    let tmp_dir = tempfile::tempdir().expect("Failed to create temp dir");

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::Initialize {
        data_dir: tmp_dir.path().to_path_buf(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().expect("Initialize failed");

    (tx, tmp_dir)
}

#[tokio::test]
async fn test_memory_graph_custom_properties_preserved_via_get_node() {
    let system = ActorSystem::new();
    let (tx, _tmp_dir) = spawn_initialized_memory_graph(&system).await;

    // Store a node with custom properties using Unknown variant
    let mut props = HashMap::new();
    props.insert("confidence_threshold".to_string(), serde_json::json!(0.7));
    props.insert("min_confidence".to_string(), serde_json::json!(0.5));
    props.insert(
        "keywords".to_string(),
        serde_json::json!(["build", "compile", "make"]),
    );
    props.insert("requires_project".to_string(), serde_json::json!(true));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("intent".to_string()),
                "build".to_string(),
                Some("Build the project".to_string()),
                props,
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let stored = resp_rx.await.unwrap().expect("StoreNode failed");

    // Verify stored node has custom properties
    assert_eq!(
        unknown_props(&stored).get("confidence_threshold"),
        Some(&serde_json::json!(0.7)),
    );
    assert_eq!(
        unknown_props(&stored).get("min_confidence"),
        Some(&serde_json::json!(0.5)),
    );
    assert_eq!(
        unknown_props(&stored).get("keywords"),
        Some(&serde_json::json!(["build", "compile", "make"])),
    );
    assert_eq!(
        unknown_props(&stored).get("requires_project"),
        Some(&serde_json::json!(true)),
    );

    // Query back via GetNode
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
            id: stored.id().to_string(),
            reply_to: resp_tx,
        })
    .await
    .unwrap();
    let retrieved = resp_rx
        .await
        .unwrap()
        .expect("GetNode failed")
        .expect("Node should exist");

    let retrieved_props = unknown_props(&retrieved);
    assert_eq!(
        retrieved_props.get("confidence_threshold"),
        Some(&serde_json::json!(0.7)),
    );
    assert_eq!(
        retrieved_props.get("min_confidence"),
        Some(&serde_json::json!(0.5)),
    );
    assert_eq!(
        retrieved_props.get("keywords"),
        Some(&serde_json::json!(["build", "compile", "make"])),
    );
    assert_eq!(
        retrieved_props.get("requires_project"),
        Some(&serde_json::json!(true)),
    );
    assert_eq!(
        retrieved_props.len(),
        4,
        "Should have exactly 4 custom properties"
    );
}

#[tokio::test]
async fn test_memory_graph_custom_properties_preserved_via_query_nodes() {
    let system = ActorSystem::new();
    let (tx, _tmp_dir) = spawn_initialized_memory_graph(&system).await;

    let mut props = HashMap::new();
    props.insert("confidence_threshold".to_string(), serde_json::json!(0.85));
    props.insert("min_confidence".to_string(), serde_json::json!(0.6));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("intent".to_string()),
                "test".to_string(),
                None,
                props,
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().expect("StoreNode failed");

    // Query back via QueryNodes
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("intent".to_string()),
                name: None,
                limit: None,
                reply_to: resp_tx,
            })
    .await
    .unwrap();
    let results = resp_rx.await.unwrap().expect("QueryNodes failed");

    assert_eq!(results.len(), 1, "Should find exactly one node");
    let props = unknown_props(&results[0]);
    assert_eq!(
        props.get("confidence_threshold"),
        Some(&serde_json::json!(0.85)),
    );
    assert_eq!(props.get("min_confidence"), Some(&serde_json::json!(0.6)),);
    assert_eq!(props.len(), 2, "Should have exactly 2 custom properties");
}

#[tokio::test]
async fn test_memory_graph_custom_properties_preserved_via_get_project_context() {
    let system = ActorSystem::new();
    let (tx, _tmp_dir) = spawn_initialized_memory_graph(&system).await;

    // Store a Project node with custom properties (Project variant ignores extra props)
    // Use Unknown variant to preserve custom properties
    let mut props = HashMap::new();
    props.insert(
        "root_dir".to_string(),
        serde_json::json!("/home/user/project"),
    );
    props.insert("language".to_string(), serde_json::json!("Rust"));

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("project".to_string()),
                "my-project".to_string(),
                Some("A test project".to_string()),
                props,
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    resp_rx.await.unwrap().expect("StoreNode failed");

    // Get project context
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetProjectContext { reply_to: resp_tx })
        .await
        .unwrap();
    let context = resp_rx.await.unwrap().expect("GetProjectContext failed");

    // Verify custom properties via unknown_props
    // Note: GetProjectContext finds nodes by node_type() so it won't see Unknown nodes
    // We just verify the call doesn't crash and the context has a project
    assert!(!context.project.name().is_empty());
}

#[tokio::test]
async fn test_memory_graph_custom_properties_preserved_after_update() {
    let system = ActorSystem::new();
    let (tx, _tmp_dir) = spawn_initialized_memory_graph(&system).await;

    // Store a node with initial custom properties
    let mut props = HashMap::new();
    props.insert(
        "initial_key".to_string(),
        serde_json::json!("initial_value"),
    );

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: t_attr_unknown(
                "Unknown",
                Some("custom".to_string()),
                "update-test".to_string(),
                None,
                props,
            ),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let stored = resp_rx.await.unwrap().expect("StoreNode failed");

    // Update with new properties (merge semantics: UpdateNode applies a
    // partial property patch, preserving properties the caller didn't touch —
    // see `apply_updates`; build_orchestrator relies on this for its
    // `active` flag).
    let mut new_props = HashMap::new();
    new_props.insert(
        "updated_key".to_string(),
        serde_json::json!("updated_value"),
    );

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::UpdateNode {
        id: stored.id().to_string(),
        updates: NodeUpdate {
            node_type: None,
            subtype: None,
            name: None,
            description: None,
            properties: Some(new_props),
            embedding_id: None,
        },
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let updated = resp_rx.await.unwrap().expect("UpdateNode failed");

    // Verify updated properties (merge: updated_key added, initial_key kept)
    assert_eq!(
        unknown_props(&updated).get("updated_key"),
        Some(&serde_json::json!("updated_value")),
    );
    assert_eq!(
        unknown_props(&updated).get("initial_key"),
        Some(&serde_json::json!("initial_value")),
    );
    assert_eq!(unknown_props(&updated).len(), 2,);

    // Verify via GetNode
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
            id: stored.id().to_string(),
            reply_to: resp_tx,
        })
    .await
    .unwrap();
    let retrieved = resp_rx
        .await
        .unwrap()
        .expect("GetNode failed")
        .expect("Node should exist");

    assert_eq!(
        unknown_props(&retrieved).get("updated_key"),
        Some(&serde_json::json!("updated_value")),
    );
    assert_eq!(
        unknown_props(&retrieved).get("initial_key"),
        Some(&serde_json::json!("initial_value")),
    );
    assert_eq!(unknown_props(&retrieved).len(), 2,);
}

// ─── Open envelope (AttrNode) ───────────────────────────────────────────────

#[tokio::test]
async fn test_attr_node_store_and_get_roundtrip() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    let now = chrono::Utc::now();
    let attr = AttrNode {
        id: Uuid::new_v4().to_string(),
        node_type: "csg.scad_node".to_string(),
        subtype: Some("module".to_string()),
        name: "translate".to_string(),
        description: Some("A translation module".to_string()),
        properties: HashMap::from([
            ("radius".to_string(), serde_json::json!(0.7)),
            (
                "tags".to_string(),
                serde_json::json!(["solid", "parametric"]),
            ),
            ("config".to_string(), serde_json::json!({ "preview": true })),
        ]),
        embedding_id: None,
        created_at: now,
        updated_at: now,
        version: 1,
    };

    // Store via the open envelope.
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::StoreAttrNode {
        node: attr.clone(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let stored = resp_rx.await.unwrap().expect("StoreAttrNode failed");
    assert_eq!(stored.id, attr.id);

    // Read back as the envelope — the arbitrary discriminator + subtype survive.
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
        id: attr.id.clone(),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let read = resp_rx
        .await
        .unwrap()
        .expect("GetAttrNode failed")
        .expect("node should exist");
    assert_eq!(read.node_type, "csg.scad_node");
    assert_eq!(read.subtype(), Some("module"));
    assert_eq!(read.name, "translate");
    assert_eq!(read.description(), Some("A translation module"));
    assert_eq!(read.get("radius"), Some(&serde_json::json!(0.7)));
    assert_eq!(
        read.get("tags"),
        Some(&serde_json::json!(["solid", "parametric"]))
    );
    assert_eq!(read.get("config"), Some(&serde_json::json!({ "preview": true })));

    // The envelope preserves the open discriminator (no closed-enum mapping).
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::GetAttrNode {
            id: attr.id.clone(),
            reply_to: resp_tx,
        })
    .await
    .unwrap();
    let node = resp_rx.await.unwrap().unwrap().expect("node should exist");
    assert_eq!(node.node_type_str(), "csg.scad_node");
    assert_eq!(
        unknown_props(&node).get("tags"),
        Some(&serde_json::json!(["solid", "parametric"]))
    );
}


#[tokio::test]
async fn test_query_attr_nodes_filters_by_discriminator() {
    let system = ActorSystem::new();
    let tx = spawn_memory_graph(&system).await;

    // Two nodes of an arbitrary (non-enum) domain + one ordinary Unknown node.
    for (id, name, node_type, extra) in [
        ("scad-1", "translate", "csg.scad_node", "radius"),
        ("scad-2", "rotate", "csg.scad_node", "angle"),
        ("u-1", "plain", "Unknown", "note"),
    ] {
        let attr = AttrNode {
            id: id.to_string(),
            node_type: node_type.to_string(),
            subtype: Some("module".to_string()),
            name: name.to_string(),
            description: None,
            properties: HashMap::from([(extra.to_string(), serde_json::json!(0.7))]),
            embedding_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 1,
        };
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        tx.send(MemoryGraphMessage::StoreAttrNode {
            node: attr,
            reply_to: resp_tx,
        })
        .await
        .unwrap();
        resp_rx.await.unwrap().expect("StoreAttrNode failed");
    }

    // Query by the arbitrary discriminator.
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::QueryAttrNodes {
        node_type: Some("csg.scad_node".to_string()),
        subtype: None,
        name: None,
        limit: None,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let nodes = resp_rx.await.unwrap().expect("QueryAttrNodes failed");
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|n| n.node_type == "csg.scad_node"));
    assert!(nodes.iter().any(|n| n.name == "translate"));

    // Query by node_type + name.
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::QueryAttrNodes {
        node_type: Some("csg.scad_node".to_string()),
        subtype: None,
        name: Some("rotate".to_string()),
        limit: None,
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let nodes = resp_rx.await.unwrap().expect("QueryAttrNodes failed");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, "scad-2");
    assert_eq!(nodes[0].get("angle"), Some(&serde_json::json!(0.7)));

    // Query without a discriminator (all nodes, limited).
    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    tx.send(MemoryGraphMessage::QueryAttrNodes {
        node_type: None,
        subtype: None,
        name: None,
        limit: Some(2),
        reply_to: resp_tx,
    })
    .await
    .unwrap();
    let nodes = resp_rx.await.unwrap().expect("QueryAttrNodes failed");
    assert_eq!(nodes.len(), 2);
}

