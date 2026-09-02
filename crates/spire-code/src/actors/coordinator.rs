// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! CoordinatorActor — main orchestrator that routes JSON-RPC methods to actors.
//!
//! The coordinator receives JSON-RPC requests from the transport layer and
//! dispatches them to the appropriate actor (chat, tools, mcp_client, llm, etc.).

use async_trait::async_trait;
use regex::Regex;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use spire_core::subsystems::chat::chat::ChatMessage;
use crate::subsystems::planning::intent_router::{IntentRouterMessage, RouteResult};
use spire_core::subsystems::llm::llm::LlmMessage;
use spire_core::subsystems::mcp::mcp_client::McpClientMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use crate::subsystems::planning::plan_orchestrator::PlanOrchestratorMessage;
use crate::subsystems::project::project_query::ProjectQueryMessage;
use crate::actors::system::SystemMessage;
use spire_core::actors::tool_providers::ToolRouterMessage;
use spire_core::actors::tools::ToolsMessage;
use spire_core::actors::Actor;
use spire_core::models::memory_graph::{McpConfigFile, McpServerConfigEntry};
use spire_core::transport::socket::TransportMessage;

// FFI-inline RPC handlers moved into this single router (see `SetFfiDeps`).
use crate::subsystems::project::project_analyzer::ProjectAnalysis;
use crate::subsystems::project::project_build::ProjectBuildMessage;
use crate::subsystems::project::project_creation::ProjectCreationMessage;
use crate::subsystems::project::project_sync::ProjectSyncMessage;
use crate::subsystems::project::project_analyzer::ProjectAnalyzerMessage;
use spire_core::actors::rag::RagMessage;
use spire_core::subsystems::tools::file_watcher::{FileChangeNotification, FileWatcherMessage};
use spire_actor::registry::ServiceRegistry;
use crate::ffi::{
    dummy_tx, populate_target_graph, resolve_project_root, serialize_analysis,
};

/// Messages for the Coordinator actor.
pub enum CoordinatorMessage {
    /// Handle a JSON-RPC request from the extension.
    HandleRequest {
        method: String,
        params: serde_json::Value,
        response_tx: tokio::sync::oneshot::Sender<serde_json::Value>,
    },
    /// Attach the app-only dispatch dependencies so the FFI-inline RPC methods
    /// (`project/open`, `createProject/*`, `rag/*`, …) can be routed here too.
    /// The FFI composition root sends this once at startup, before any request;
    /// the standalone binary never sends it (its extension flow uses the tools/
    /// coordinator methods, so the moved handlers return a clear error there).
    SetFfiDeps {
        registry: Arc<ServiceRegistry>,
        state: Arc<FfiSharedState>,
    },
    /// Shut down the coordinator.
    Shutdown,
}

/// App-only state that lets the coordinator handle the FFI-inline RPC methods
/// so ALL method routing lives in this one router instead of a parallel
/// dispatch path in `ffi.rs::process_json_request`.
pub struct FfiSharedState {
    /// Currently opened project root (set by `project/open`).
    pub project_root: std::sync::Mutex<Option<PathBuf>>,
    /// Latest project analysis (set by `project/open` / `AnalyzeProject`).
    pub analysis: std::sync::Mutex<Option<ProjectAnalysis>>,
    /// Default RAG domain selected by the UI (`rag/set-domain`). Shared with
    /// the RAG tool registry's `default_domain` handle.
    pub default_rag_domain: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// File-watcher output channel (`project/open` StartWatching).
    pub watcher_out_tx: mpsc::Sender<FileChangeNotification>,
}

/// The Coordinator actor routes requests to the appropriate sub-actors.
pub struct CoordinatorActor {
    /// Sender for the chat actor.
    chat_tx: mpsc::Sender<ChatMessage>,
    /// Sender for the tools actor.
    tools_tx: mpsc::Sender<ToolsMessage>,
    /// Sender for the MCP client actor.
    mcp_client_tx: mpsc::Sender<McpClientMessage>,
    /// Sender for the LLM actor.
    llm_tx: mpsc::Sender<LlmMessage>,
    /// Sender for the system actor.
    system_tx: mpsc::Sender<SystemMessage>,
    /// Sender for the memory graph actor (knowledge graph + config storage).
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    /// Sender for the project query actor (semantic project queries).
    project_query_tx: mpsc::Sender<ProjectQueryMessage>,
    /// Sender for the intent router actor (routes user queries to matched intents).
    intent_router_tx: mpsc::Sender<IntentRouterMessage>,
    /// Sender for the tool router actor (routes tool calls to appropriate backend).
    tool_router_tx: mpsc::Sender<ToolRouterMessage>,
    /// Sender for the plan orchestrator actor (creates and executes multi-step plans).
    plan_orchestrator_tx: mpsc::Sender<PlanOrchestratorMessage>,
    /// Transport sender for forwarding VSC tool calls / notifications to the extension.
    transport_tx: mpsc::Sender<TransportMessage>,
    /// App-only dispatch dependencies (registry + shared state), attached via
    /// `SetFfiDeps`. `None` in the standalone binary.
    registry: Option<Arc<ServiceRegistry>>,
    ffi_state: Option<Arc<FfiSharedState>>,
}

impl CoordinatorActor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chat_tx: mpsc::Sender<ChatMessage>,
        tools_tx: mpsc::Sender<ToolsMessage>,
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
        llm_tx: mpsc::Sender<LlmMessage>,
        system_tx: mpsc::Sender<SystemMessage>,
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        project_query_tx: mpsc::Sender<ProjectQueryMessage>,
        intent_router_tx: mpsc::Sender<IntentRouterMessage>,
        tool_router_tx: mpsc::Sender<ToolRouterMessage>,
        plan_orchestrator_tx: mpsc::Sender<PlanOrchestratorMessage>,
        transport_tx: mpsc::Sender<TransportMessage>,
    ) -> Self {
        Self {
            chat_tx,
            tools_tx,
            mcp_client_tx,
            llm_tx,
            system_tx,
            memory_graph_tx,
            project_query_tx,
            intent_router_tx,
            tool_router_tx,
            plan_orchestrator_tx,
            transport_tx,
            registry: None,
            ffi_state: None,
        }
    }

    /// HAL fix proposal (file-by-file LLM flow): lint -> whole-file rewrite
    /// prompt -> LLM -> strip fences -> structural syntax check (retry once)
    /// -> return {status, path, proposed_content, issues} for Accept/Reject.
    async fn propose_hal_fix(&self, root: &str, path: &str) -> serde_json::Value {
        let issues = crate::build::generic_helpers::hal_doc_lint_file(
            std::path::Path::new(root),
            path,
        );
        if issues.is_empty() {
            return serde_json::json!({ "status": "clean", "path": path });
        }
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let mut prompt = crate::build::generic_helpers::hal_doc_fix_prompt_whole(
            path, &content, &issues,
        );
        let mut proposed = String::new();
        // Call the LLM directly (no actor loop): a missing/panicked actor can
        // never break this path, and an unconfigured key returns a clear Err.
        let llm = spire_core::subsystems::llm::llm::LlmActor::new(
            spire_core::config::load_global_llm_config(),
        );
        for _attempt in 0..2 {
            let text = match llm
                .complete_prompt(&prompt, spire_core::subsystems::llm::llm::LlmModelRole::Coding)
                .await
            {
                Ok(t) => t,
                Err(e) => return serde_json::json!({ "status": "error", "error": e.to_string() }),
            };
            proposed = crate::build::generic_helpers::strip_code_fences(&text);
            let check = crate::build::generic_helpers::cpp_syntax_check(&proposed);
            if check.ok {
                break;
            }
            let hint: Vec<String> = check
                .errors
                .iter()
                .map(|e| format!("line {} col {}: {}", e.line, e.col, e.kind))
                .collect();
            prompt.push_str(&format!(
                "\n\nYour previous attempt had C++ syntax errors: {}. Fix them and return the complete corrected header again.",
                hint.join("; ")
            ));
        }
        serde_json::json!({
            "status": "proposed",
            "path": path,
            "proposed_content": proposed,
            "issues": issues,
        })
    }

    /// Send a tool event notification to the extension via the transport actor.
    async fn send_tool_event(&self, event: &str, payload: &serde_json::Value) {
        let _ = self
            .transport_tx
            .send(TransportMessage::SendNotification {
                method: format!("event/tool/{}", event),
                params: payload.clone(),
            })
            .await;
    }

    /// Call a VS Code extension tool via the TransportActor.
    async fn call_extension_tool(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.transport_tx
            .send(TransportMessage::CallExtension {
                method: tool_name.to_string(),
                params: args.clone(),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Transport send error: {}", e))?;

        rx.await
            .map_err(|e| format!("Transport response error: {}", e))?
    }
}

#[async_trait]
impl Actor for CoordinatorActor {
    type Message = CoordinatorMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            CoordinatorMessage::HandleRequest {
                method,
                params,
                response_tx,
            } => {
                tracing::info!(
                    "[COORDINATOR] REQUEST received: method={}, params_keys={:?}",
                    method,
                    params
                        .as_object()
                        .map(|m| m.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                );
                let result = self.route_request(&method, params).await;
                let _ = response_tx.send(result);
            }
            CoordinatorMessage::SetFfiDeps { registry, state } => {
                self.registry = Some(registry);
                self.ffi_state = Some(state);
                tracing::info!("Coordinator: FFI dispatch deps attached");
            }
            CoordinatorMessage::Shutdown => {
                tracing::info!("Coordinator: shutting down");
            }
        }
    }
}

impl CoordinatorActor {
    async fn route_request(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        // project/getBuildTarget is invoked via the `tools/call` JSON-RPC
        // envelope from Swift:
        //   {"method":"tools/call","params":{"tool":"project/getBuildTarget","args":{"name":...}}}
        // Answer directly from the in-memory analysis (authoritative BuildManager
        // result) for both the bare method and the envelope form.
        let is_build_target_call = method == "project/getBuildTarget"
            || (method == "tools/call"
                && params
                    .get("tool")
                    .and_then(|v| v.as_str())
                    == Some("project/getBuildTarget"));
        if is_build_target_call {
            return self.handle_project_get_build_target(method, &params).await;
        }

        // All rag/* RPCs route to the RAG actor (shared dispatch deps).
        if method.starts_with("rag/") {
            return self.handle_rag(method, &params).await;
        }

        match method {
            // ── App-only (FFI) methods — moved from ffi.rs so ALL routing
            // ── lives in this one router ──
            "project/open" => {
                return self.handle_project_open(&params).await;
            }
            "AnalyzeProject" => {
                return self.handle_analyze_project(&params).await;
            }
            "project/buildStatus" => {
                return self.handle_project_build_status(&params).await;
            }
            "project/diagnostics" => {
                return self.handle_project_diagnostics(&params).await;
            }
            "createProject/Plan" => {
                return self.handle_create_project_plan(&params).await;
            }
            "createProject/GeneratePlan" => {
                return self.handle_create_project_generate_plan(&params).await;
            }
            "createProject/Scaffold" => {
                return self.handle_create_project_scaffold(&params).await;
            }
            "createProject/Fill" => {
                return self.handle_create_project_fill(&params).await;
            }
            "createProject/GenerateSpec" => {
                return self.handle_create_project_generate_spec(&params).await;
            }
            "createProject/GenerateCode" => {
                return self.handle_create_project_generate_code(&params).await;
            }
            "createProject/ExecutePlan" => {
                return self.handle_create_project_execute_plan(&params).await;
            }
            "createProject/ExecuteStep" => {
                return self.handle_create_project_execute_step(&params).await;
            }
            // ── Chat methods ──
            "chat/getActive" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::GetActive { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Some(dialog)) => {
                        serde_json::to_value(dialog).unwrap_or(serde_json::Value::Null)
                    }
                    Ok(None) => serde_json::Value::Null,
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/getHistory" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::GetHistory { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(dialogs) => serde_json::to_value(dialogs).unwrap_or(serde_json::json!([])),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/append" => {
                let chat_id = params
                    .get("chatId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let role = params
                    .get("options")
                    .and_then(|o| o.get("role"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant");
                let widget = params.get("options").and_then(|o| o.get("widget")).cloned();
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::Append {
                        chat_id: chat_id.to_string(),
                        content: content.to_string(),
                        role: role.to_string(),
                        widget,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Ok(msg)) => serde_json::to_value(msg).unwrap_or(serde_json::Value::Null),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/clear" => {
                let chat_id = params
                    .get("chatId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::Clear {
                        chat_id: chat_id.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/setTitle" => {
                let chat_id = params
                    .get("chatId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::SetTitle {
                        chat_id: chat_id.to_string(),
                        title: title.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }

            // ── Tool methods ──
            "tools/list" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .tools_tx
                    .send(ToolsMessage::ListTools { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Tools actor not available"});
                }
                match rx.await {
                    Ok(tools) => serde_json::to_value(tools).unwrap_or(serde_json::json!([])),
                    Err(_) => serde_json::json!({"error": "Tools actor response error"}),
                }
            }
            "tools/call" => {
                let tool = params.get("tool").and_then(|v| v.as_str()).unwrap_or("");
                let args = params
                    .get("args")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                // Emit tool/start event
                let tool_call_id = format!(
                    "call_direct_{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );
                self.send_tool_event(
                    "start",
                    &serde_json::json!({
                        "tool_name": tool,
                        "args": args,
                        "tool_call_id": tool_call_id,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    }),
                )
                .await;

                let start = std::time::Instant::now();
                let result = {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    if self
                        .tool_router_tx
                        .send(ToolRouterMessage::CallTool {
                            tool_name: tool.to_string(),
                            args: args.clone(),
                            reply_to: tx,
                        })
                        .await
                        .is_err()
                    {
                        serde_json::json!({"error": "ToolRouter actor not available"})
                    } else {
                        match rx.await {
                            Ok(Ok(res)) => res,
                            Ok(Err(e)) => serde_json::json!({"error": e}),
                            Err(_) => {
                                serde_json::json!({"error": "ToolRouter actor response error"})
                            }
                        }
                    }
                };
                let duration_ms = start.elapsed().as_millis() as u64;

                if result.get("error").is_some() {
                    self.send_tool_event(
                        "error",
                        &serde_json::json!({
                            "tool_name": tool,
                            "error": result["error"],
                            "duration_ms": duration_ms,
                            "tool_call_id": tool_call_id,
                        }),
                    )
                    .await;
                } else {
                    self.send_tool_event(
                        "result",
                        &serde_json::json!({
                            "tool_name": tool,
                            "result": result,
                            "duration_ms": duration_ms,
                            "tool_call_id": tool_call_id,
                        }),
                    )
                    .await;
                }

                result
            }

            // ── MCP Client methods ──
            "mcp/listServers" | "mcp/servers" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::GetServerDetails { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(details) => serde_json::to_value(details).unwrap_or(serde_json::json!([])),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }

            "mcp/loadConfig" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Memory graph actor not available"});
                }
                match rx.await {
                    Ok(Ok(servers)) => {
                        let count = servers.len();
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
                        let (tx2, rx2) = tokio::sync::oneshot::channel();
                        if self
                            .mcp_client_tx
                            .send(McpClientMessage::LoadConfigFromGraph {
                                servers: configs,
                                reply_to: tx2,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MCP client actor not available"});
                        }
                        let _ = rx2.await;
                        serde_json::json!({"success": true, "serverCount": count})
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Memory graph actor response error"}),
                }
            }
            "mcp/connectAll" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::ConnectAll { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/connect" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::Connect {
                        server_name: server_name.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/disconnect" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::Disconnect {
                        server_name: server_name.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/disconnectAll" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::DisconnectAll { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/listServerTools" | "mcp/getTools" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::GetTools {
                        server_name: server_name.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Some(tools)) => serde_json::to_value(tools).unwrap_or(serde_json::json!([])),
                    Ok(None) => serde_json::json!([]),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/setInternalTools" => {
                let tools: Vec<rust_mcp_sdk::schema::Tool> = params
                    .get("tools")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::SetInternalTools {
                        tools,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/callTool" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let tool_name = params
                    .get("toolName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let arguments = params.get("arguments").and_then(|v| v.as_object()).cloned();
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::CallTool {
                        server_name: server_name.to_string(),
                        tool_name: tool_name.to_string(),
                        arguments,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(result)) => serde_json::to_value(result)
                        .unwrap_or(serde_json::json!({"error": "serialization error"})),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }

            // ── LLM methods ──
            "llm/complete" => {
                tracing::info!("[COORDINATOR] llm/complete called");
                // Extract the prompt (either explicit `prompt` param, or the last user message from `messages`)
                let mut prompt = params
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let prompt_source;
                if prompt.is_empty() {
                    // Fallback: extract the last user message from the messages array
                    prompt_source = "messages fallback";
                    if let Some(messages) = params.get("messages").and_then(|v| v.as_array()) {
                        for msg in messages.iter().rev() {
                            if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                                if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                                    prompt = content.to_string();
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    prompt_source = "explicit prompt param";
                }
                tracing::info!(
                    "[COORDINATOR] PROMPT extracted (source={}): \"{}\"",
                    prompt_source,
                    &prompt.chars().take(200).collect::<String>()
                );

                // Step 0: Route through the IntentRouterActor to determine the handler.
                tracing::info!(
                    "[COORDINATOR] → INTENT_ROUTER: sending RouteQuery (query=\"{}\")",
                    &prompt.chars().take(100).collect::<String>()
                );
                let (intent_tx, intent_rx) = tokio::sync::oneshot::channel();
                if self
                    .intent_router_tx
                    .send(IntentRouterMessage::RouteQuery {
                        query: prompt.to_string(),
                        reply_to: intent_tx,
                    })
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "[COORDINATOR] IntentRouterActor not available, falling through to LLM"
                    );
                } else if let Ok(route_result) = intent_rx.await {
                    tracing::info!("[COORDINATOR] ← INTENT RESULT: {:?}", route_result);
                    match route_result {
                        RouteResult::Build {
                            intent_name,
                            confidence,
                            ref parameters,
                        } => {
                            tracing::info!("[COORDINATOR] INTENT → project/build (intent={}, confidence={}, parameters={:?})", intent_name, confidence, parameters);

                            // Extract scope from the query parameter to pass to the meta build tool
                            let query = parameters.get("query").map(|s| s.as_str()).unwrap_or("");
                            let scope = if query.eq_ignore_ascii_case("build all")
                                || query.eq_ignore_ascii_case("build")
                                || query.is_empty()
                            {
                                None // defaults to "all" in project/build
                            } else if query.starts_with("build ") {
                                Some(query[6..].to_string()) // extract scope after "build "
                            } else {
                                None
                            };

                            let mut build_args = serde_json::Map::new();
                            if let Some(ref s) = scope {
                                build_args.insert(
                                    "scope".to_string(),
                                    serde_json::Value::String(s.clone()),
                                );
                            }
                            if let Some(mode) = parameters.get("mode").map(|s| s.as_str()) {
                                build_args.insert(
                                    "mode".to_string(),
                                    serde_json::Value::String(mode.to_string()),
                                );
                            }

                            tracing::info!("[COORDINATOR] → TOOL_ROUTER: project/build (scope={:?}, args={:?})", scope, build_args);
                            let (build_tx, build_rx) = tokio::sync::oneshot::channel();
                            match self
                                .tool_router_tx
                                .send(ToolRouterMessage::CallTool {
                                    tool_name: "project/build".to_string(),
                                    args: serde_json::Value::Object(build_args),
                                    reply_to: build_tx,
                                })
                                .await
                            {
                                Ok(()) => {
                                    tracing::info!(
                                        "[COORDINATOR] ← TOOL_ROUTER: waiting for build result"
                                    );
                                    return match build_rx.await {
                                        Ok(Ok(result)) => {
                                            // Format build result as a concise text summary instead of raw JSON.
                                            // The detailed build info is already shown via the build-list widget.
                                            let success = result
                                                .get("success")
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            let duration = result
                                                .get("duration_secs")
                                                .and_then(|v| v.as_f64())
                                                .unwrap_or(0.0);
                                            let systems =
                                                result.get("systems").and_then(|v| v.as_array());
                                            let count = systems.map(|a| a.len()).unwrap_or(0);
                                            let summary = if success {
                                                format!("✅ Build completed successfully — {} system(s) in {:.1}s", count, duration)
                                            } else {
                                                "⚠️ Build finished with failures — see build list above for details".to_string()
                                            };
                                            serde_json::json!({"content": summary})
                                        }
                                        Ok(Err(e)) => {
                                            serde_json::json!({"error": format!("Build failed: {}", e)})
                                        }
                                        Err(e) => {
                                            serde_json::json!({"error": format!("Build tool response error: {}", e)})
                                        }
                                    };
                                }
                                Err(e) => {
                                    return serde_json::json!({"error": format!("ToolRouter not available: {}", e)});
                                }
                            }
                        }
                        RouteResult::StateBlocked {
                            intent_name,
                            confidence,
                            ref missing_states,
                        } => {
                            let missing = missing_states.join(", ");
                            tracing::info!("[COORDINATOR] ← INTENT: StateBlocked (intent={}, confidence={}, missing=[{}]) — returning blocked message", intent_name, confidence, missing);
                            return serde_json::json!({
                                "content": format!("⚠️ Cannot run **{}** — required state not ready: **{}**\n\nTry running a project sync first.", intent_name, missing)
                            });
                        }
                        RouteResult::NeedsApproval {
                            intent_name,
                            confidence,
                        } => {
                            tracing::info!("[COORDINATOR] ← INTENT: NeedsApproval (intent={}, confidence={}) — falling to prompt handler", intent_name, confidence);
                        }
                        RouteResult::Plan {
                            intent_name,
                            confidence,
                            ref parameters,
                        } => {
                            tracing::info!("[COORDINATOR] ← INTENT: Plan (intent={}, confidence={}, params={:?}) — dispatching to PlanOrchestrator", intent_name, confidence, parameters);
                            let goal = parameters.get("query").cloned().unwrap_or_else(|| {
                                params
                                    .get("prompt")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string()
                            });
                            // Determine modification scope from parameters, if present:
                            //   scope = "project" → project-level (Level 1)
                            //   scope = "subproject" + scope_path = "<dir>" → subproject-level (Level 2)
                            let scope = parameters
                                .get("scope")
                                .map(|s| s.as_str())
                                .filter(|s| !s.is_empty())
                                .and_then(|s| match s {
                                    "project" => Some(crate::subsystems::planning::plan_orchestrator::ModificationScope::Project),
                                    "subproject" => parameters
                                        .get("scope_path")
                                        .map(|p| crate::subsystems::planning::plan_orchestrator::ModificationScope::Subproject {
                                            path: p.clone(),
                                        }),
                                    _ => None,
                                });
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            if self
                                .plan_orchestrator_tx
                                .send(PlanOrchestratorMessage::CreatePlan {
                                    goal: goal.clone(),
                                    intent_name: Some(intent_name.clone()),
                                    parameters: parameters.clone(),
                                    scope,
                                    workspace_root: None,
                                    reply_to: tx,
                                })
                                .await
                                .is_err()
                            {
                                return serde_json::json!({"error": "PlanOrchestrator not available"});
                            }
                            match rx.await {
                                Ok(Ok(plan)) => {
                                    return serde_json::json!({
                                        "content": format!("📋 **Plan created:** {} — {} steps. Review and approve to begin.", plan.goal, plan.total_steps)
                                    });
                                }
                                Ok(Err(e)) => {
                                    return serde_json::json!({"error": format!("Plan creation failed: {}", e)});
                                }
                                Err(e) => {
                                    return serde_json::json!({"error": format!("PlanOrchestrator response error: {}", e)});
                                }
                            }
                        }
                        RouteResult::Chat => {
                            tracing::info!(
                                "[COORDINATOR] ← INTENT: Chat — proceeding to LLM fall-through"
                            );
                        }
                    }
                }

                // ── Fall through LLM flow ──
                tracing::info!("[COORDINATOR] FALL-THROUGH LLM: gathering chat history and tools");
                let chat_history = {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    if self
                        .chat_tx
                        .send(ChatMessage::GetActive { reply_to: tx })
                        .await
                        .is_err()
                    {
                        tracing::warn!("[COORDINATOR] Chat actor not available for history");
                        None
                    } else {
                        let hist = rx.await.ok().flatten();
                        tracing::info!(
                            "[COORDINATOR] Chat history: {} has {} messages",
                            hist.as_ref().map(|d| d.id.as_str()).unwrap_or("none"),
                            hist.as_ref().map(|d| d.messages.len()).unwrap_or(0)
                        );
                        hist
                    }
                };

                let tools = {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    if self
                        .tools_tx
                        .send(ToolsMessage::ListTools { reply_to: tx })
                        .await
                        .is_err()
                    {
                        tracing::warn!("[COORDINATOR] Tools actor not available");
                        vec![]
                    } else {
                        let t = rx.await.unwrap_or_default();
                        tracing::info!("[COORDINATOR] Tools loaded: {} tools available", t.len());
                        if !t.is_empty() {
                            tracing::info!(
                                "[COORDINATOR] Tool names: {:?}",
                                t.iter().map(|ti| &ti.name).collect::<Vec<_>>()
                            );
                        }
                        t
                    }
                };

                let system_msg = "You are a helpful AI assistant. When you need to use a tool, respond using the native function-calling mechanism (tool_calls) provided by the API — do not describe tool calls in plain text.".to_string();

                let mut messages: Vec<spire_core::subsystems::chat::chat::ChatMessageData> = Vec::new();
                messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                    id: "sys-tools".to_string(),
                    role: "system".to_string(),
                    content: system_msg,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    widget: None,
                });

                if let Some(ref dialog) = chat_history {
                    for msg in &dialog.messages {
                        if msg.role != "system" {
                            messages.push(msg.clone());
                        }
                    }
                }

                let has_user_prompt = messages
                    .last()
                    .map(|m| m.role == "user" && m.content == prompt)
                    .unwrap_or(false);

                if !has_user_prompt {
                    messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                        id: "user-prompt".to_string(),
                        role: "user".to_string(),
                        content: prompt.to_string(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        widget: None,
                    });
                }

                tracing::info!("[COORDINATOR] FALL-THROUGH: built messages array with {} msgs (has_user_prompt={}), {} tools",
                    messages.len(), has_user_prompt, tools.len());
                for (i, m) in messages.iter().enumerate() {
                    tracing::info!(
                        "[COORDINATOR]   message[{}]: role={}, id={}, content_len={}",
                        i,
                        m.role,
                        m.id,
                        m.content.len()
                    );
                }

                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(LlmMessage::CompleteWithTools {
                        messages: messages.clone(),
                        tools: tools.clone(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "LLM actor not available"});
                }

                let llm_response = match rx.await {
                    Ok(Ok(content)) => content,
                    Ok(Err(e)) => return serde_json::json!({"error": e.to_string()}),
                    Err(_) => return serde_json::json!({"error": "LLM actor response error"}),
                };

                tracing::info!(
                    "[COORDINATOR] ← LLM first response received (len={} chars)",
                    llm_response.len()
                );
                let final_content = if let Ok(json_msg) =
                    serde_json::from_str::<serde_json::Value>(&llm_response)
                {
                    let has_tc = json_msg
                        .get("tool_calls")
                        .and_then(|t| t.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    tracing::info!(
                        "[COORDINATOR] LLM response parsed as JSON, has_tool_calls={}",
                        has_tc
                    );
                    if let Some(tool_calls) = json_msg["tool_calls"].as_array() {
                        if !tool_calls.is_empty() {
                            let mut tool_results: Vec<serde_json::Value> = Vec::new();
                            for tc in tool_calls {
                                let function_name =
                                    tc["function"]["name"].as_str().unwrap_or("unknown");
                                let function_args: serde_json::Value = tc["function"]["arguments"]
                                    .as_str()
                                    .and_then(|s| serde_json::from_str(s).ok())
                                    .unwrap_or(serde_json::Value::Null);
                                let tool_call_id = tc["id"].as_str().unwrap_or("call_unknown");
                                tracing::info!(
                                    "Coordinator: executing tool call: {} with args: {:?}",
                                    function_name,
                                    function_args
                                );
                                self.send_tool_event("start", &serde_json::json!({
                                    "tool_name": function_name, "args": function_args, "tool_call_id": tool_call_id, "timestamp": chrono::Utc::now().to_rfc3339(),
                                })).await;
                                let is_vsc_tool = function_name.starts_with("workspace/")
                                    || function_name.starts_with("document/")
                                    || function_name.starts_with("diagnostics/")
                                    || function_name.starts_with("git/")
                                    || function_name.starts_with("symbols/");
                                let is_project_tool = function_name.starts_with("project/");
                                let tool_start = std::time::Instant::now();
                                let tool_result: Result<serde_json::Value, String> = if is_vsc_tool
                                {
                                    self.call_extension_tool(function_name, &function_args)
                                        .await
                                } else if is_project_tool {
                                    let (tx, rx) = tokio::sync::oneshot::channel();
                                    if self
                                        .project_query_tx
                                        .send(ProjectQueryMessage::CallTool {
                                            tool: function_name.to_string(),
                                            args: function_args.clone(),
                                            reply_to: tx,
                                        })
                                        .await
                                        .is_ok()
                                    {
                                        match rx.await {
                                            Ok(result) => Ok(result),
                                            Err(e) => Err(format!(
                                                "ProjectQuery actor response error: {}",
                                                e
                                            )),
                                        }
                                    } else {
                                        Err("ProjectQuery actor not available".to_string())
                                    }
                                } else {
                                    let (tool_tx, tool_rx) = tokio::sync::oneshot::channel();
                                    if self
                                        .mcp_client_tx
                                        .send(McpClientMessage::CallTool {
                                            server_name: String::new(),
                                            tool_name: function_name.to_string(),
                                            arguments: function_args.as_object().cloned(),
                                            reply_to: tool_tx,
                                        })
                                        .await
                                        .is_ok()
                                    {
                                        match tool_rx.await {
                                            Ok(Ok(result)) => Ok(serde_json::to_value(result).unwrap_or(serde_json::json!({"error": "serialization error"}))),
                                            Ok(Err(e)) => Err(e.to_string()),
                                            Err(_) => Err("MCP client response error".to_string()),
                                        }
                                    } else {
                                        Err("MCP client not available".to_string())
                                    }
                                };
                                let tool_duration_ms = tool_start.elapsed().as_millis() as u64;
                                match &tool_result {
                                    Ok(result) => {
                                        self.send_tool_event("result", &serde_json::json!({
                                            "tool_name": function_name, "result": result, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id,
                                        })).await;
                                        tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "result": result}));
                                    }
                                    Err(e) => {
                                        self.send_tool_event("error", &serde_json::json!({
                                            "tool_name": function_name, "error": e, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id,
                                        })).await;
                                        tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "error": e.to_string()}));
                                    }
                                }
                            }
                            let tool_results_text = serde_json::to_string_pretty(&tool_results)
                                .unwrap_or_else(|_| "[]".to_string());
                            messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                                id: "tool-results".to_string(),
                                role: "user".to_string(),
                                content: format!("Tool execution results:\n{}", tool_results_text),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                widget: None,
                            });
                            let (tx2, rx2) = tokio::sync::oneshot::channel();
                            if self
                                .llm_tx
                                .send(LlmMessage::CompleteWithMessages {
                                    messages,
                                    reply_to: tx2,
                                })
                                .await
                                .is_err()
                            {
                                return serde_json::json!({"error": "LLM actor not available", "tool_results": tool_results});
                            }
                            match rx2.await {
                                Ok(Ok(content)) => content,
                                Ok(Err(e)) => {
                                    return serde_json::json!({"error": e.to_string(), "tool_results": tool_results})
                                }
                                Err(_) => {
                                    return serde_json::json!({"error": "LLM actor response error", "tool_results": tool_results})
                                }
                            }
                        } else {
                            llm_response
                        }
                    } else {
                        json_msg["content"]
                            .as_str()
                            .unwrap_or(&llm_response)
                            .to_string()
                    }
                } else {
                    tracing::info!(
                        "[COORDINATOR] LLM response is NOT valid JSON, checking for XML tool calls"
                    );
                    if let Some(xml_tool_calls) = Self::parse_xml_tool_calls(&llm_response) {
                        tracing::info!(
                            "[COORDINATOR] XML PARSE: detected {} XML-format tool call(s)",
                            xml_tool_calls.len()
                        );
                        let mut tool_results: Vec<serde_json::Value> = Vec::new();
                        for tc in &xml_tool_calls {
                            let function_name =
                                tc["function"]["name"].as_str().unwrap_or("unknown");
                            let function_args: serde_json::Value = tc["function"]["arguments"]
                                .as_str()
                                .and_then(|s| serde_json::from_str(s).ok())
                                .unwrap_or(serde_json::Value::Null);
                            let tool_call_id = tc["id"].as_str().unwrap_or("call_xml_unknown");
                            self.send_tool_event("start", &serde_json::json!({
                                "tool_name": function_name, "args": function_args, "tool_call_id": tool_call_id, "timestamp": chrono::Utc::now().to_rfc3339(),
                            })).await;
                            let is_vsc_tool = function_name.starts_with("workspace/")
                                || function_name.starts_with("document/")
                                || function_name.starts_with("diagnostics/")
                                || function_name.starts_with("git/")
                                || function_name.starts_with("symbols/");
                            let is_project_tool = function_name.starts_with("project/");
                            let tool_start = std::time::Instant::now();
                            let tool_result: Result<serde_json::Value, String> = if is_vsc_tool {
                                self.call_extension_tool(function_name, &function_args)
                                    .await
                            } else if is_project_tool {
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .project_query_tx
                                    .send(ProjectQueryMessage::CallTool {
                                        tool: function_name.to_string(),
                                        args: function_args.clone(),
                                        reply_to: tx,
                                    })
                                    .await
                                    .is_ok()
                                {
                                    match rx.await {
                                        Ok(result) => Ok(result),
                                        Err(e) => {
                                            Err(format!("ProjectQuery actor response error: {}", e))
                                        }
                                    }
                                } else {
                                    Err("ProjectQuery actor not available".to_string())
                                }
                            } else {
                                let (tool_tx, tool_rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::CallTool {
                                        server_name: String::new(),
                                        tool_name: function_name.to_string(),
                                        arguments: function_args.as_object().cloned(),
                                        reply_to: tool_tx,
                                    })
                                    .await
                                    .is_ok()
                                {
                                    match tool_rx.await {
                                        Ok(Ok(result)) => Ok(serde_json::to_value(result)
                                            .unwrap_or(
                                                serde_json::json!({"error": "serialization error"}),
                                            )),
                                        Ok(Err(e)) => Err(e.to_string()),
                                        Err(_) => Err("MCP client response error".to_string()),
                                    }
                                } else {
                                    Err("MCP client not available".to_string())
                                }
                            };
                            let tool_duration_ms = tool_start.elapsed().as_millis() as u64;
                            match &tool_result {
                                Ok(result) => {
                                    self.send_tool_event("result", &serde_json::json!({ "tool_name": function_name, "result": result, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id, })).await;
                                    tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "result": result}));
                                }
                                Err(e) => {
                                    self.send_tool_event("error", &serde_json::json!({ "tool_name": function_name, "error": e, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id, })).await;
                                    tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "error": e.to_string()}));
                                }
                            }
                        }
                        let tool_results_text = serde_json::to_string_pretty(&tool_results)
                            .unwrap_or_else(|_| "[]".to_string());
                        messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                            id: "tool-results".to_string(),
                            role: "user".to_string(),
                            content: format!("Tool execution results:\n{}", tool_results_text),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            widget: None,
                        });
                        let (tx2, rx2) = tokio::sync::oneshot::channel();
                        if self
                            .llm_tx
                            .send(LlmMessage::CompleteWithMessages {
                                messages,
                                reply_to: tx2,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "LLM actor not available", "tool_results": tool_results});
                        }
                        match rx2.await {
                            Ok(Ok(content)) => content,
                            Ok(Err(e)) => {
                                return serde_json::json!({"error": e.to_string(), "tool_results": tool_results})
                            }
                            Err(_) => {
                                return serde_json::json!({"error": "LLM actor response error", "tool_results": tool_results})
                            }
                        }
                    } else {
                        llm_response
                    }
                };

                serde_json::json!({"content": final_content})
            }
            "llm/stream" => {
                let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(LlmMessage::Stream {
                        prompt: prompt.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "LLM actor not available"});
                }
                match rx.await {
                    Ok(Ok(mut chunk_rx)) => {
                        let mut full = String::new();
                        while let Some(chunk) = chunk_rx.recv().await {
                            full.push_str(&chunk);
                        }
                        serde_json::json!({"content": full})
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "LLM actor response error"}),
                }
            }
            "llm/updateConfig" => {
                let api_key = params
                    .get("apiKey")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let model = params
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&spire_core::subsystems::llm::llm::LlmConfig::default().model)
                    .to_string();
                let api_url = params
                    .get("apiUrl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let max_tokens = params
                    .get("maxTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(4096) as u32;
                let coding_max_tokens = params
                    .get("codingMaxTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(spire_core::subsystems::llm::llm::LlmConfig::default().coding_max_tokens as u64) as u32;
                let temperature = params
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.7) as f32;
                let strict_mode = params
                    .get("strictMode")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(LlmMessage::UpdateConfig {
                        config: spire_core::subsystems::llm::llm::LlmConfig {
                            api_key,
                            model,
                            api_url,
                            max_tokens,
                            coding_max_tokens,
                            temperature,
                            strict_mode,
                            planning_model: spire_core::subsystems::llm::llm::LlmConfig::default().planning_model,
                            coding_model: spire_core::subsystems::llm::llm::LlmConfig::default().coding_model,
                        },
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "LLM actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "LLM actor response error"}),
                }
            }

            // ── Platform methods ──
            "platforms/list" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetPlatforms { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Memory graph actor not available"});
                }
                match rx.await {
                    Ok(Ok(nodes)) => {
                        // Rebuild the typed Platform view from the generic
                        // registry JSON nodes the knowledge crate returned.
                        let platforms: Vec<spire_core::build_types::Platform> = nodes
                            .iter()
                            .filter_map(crate::actors::platform_codec::platform_json_to_spire)
                            .collect();
                        if !platforms.is_empty() {
                            serde_json::to_value(platforms)
                                .unwrap_or(serde_json::json!([]))
                        } else {
                            // The startup phase chain may not have seeded the
                            // graph yet (or a fresh DB cleared it). The YAML
                            // seed is the source of truth for the toolchain —
                            // fall back to reading it directly so the viewer
                            // always shows the registered platforms.
                            let dir = spire_core::build_types::Platform::default_platform_dir();
                            let from_seed =
                                spire_core::build_types::Platform::load_directory(&dir)
                                    .unwrap_or_default();
                            serde_json::to_value(from_seed)
                                .unwrap_or(serde_json::json!([]))
                        }
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("Memory graph response error: {}", e)}),
                }
            }

            // ── System methods ──
            "system/status" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .system_tx
                    .send(SystemMessage::GetStatus { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "System actor not available"});
                }
                match rx.await {
                    Ok(status) => status,
                    Err(_) => serde_json::json!({"error": "System actor response error"}),
                }
            }
            "system/shutdown" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .system_tx
                    .send(SystemMessage::Shutdown { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "System actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "System actor response error"}),
                }
            }
            "system/config/get" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .system_tx
                    .send(SystemMessage::GetConfig {
                        key: key.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "System actor not available"});
                }
                match rx.await {
                    Ok(Some(value)) => serde_json::json!({"value": value}),
                    Ok(None) => serde_json::json!({"value": null}),
                    Err(_) => serde_json::json!({"error": "System actor response error"}),
                }
            }

            // ── Config Storage (via MemoryGraph) ──
            "config/get" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetConfig {
                        key: key.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(Some(value))) => serde_json::json!({"value": value}),
                    Ok(Ok(None)) => serde_json::json!({"value": null}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "config/getAll" => {
                spire_core::config::global_config_json()
            }
            "config/set" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let value_str = params
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !key.starts_with("deepseek.") && !key.starts_with("tavily.") {
                    return serde_json::json!({"error": "Only deepseek.* and tavily.* keys supported"});
                }
                let new_config = match spire_core::config::set_global_llm_config_key(key, &value_str) {
                    Ok(cfg) => cfg,
                    Err(e) => return serde_json::json!({"error": e}),
                };
                let (tx_llm, rx_llm) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(crate::actors::LlmMessage::UpdateConfig {
                        config: new_config,
                        reply_to: tx_llm,
                    })
                    .await
                    .is_ok()
                {
                    let _ = rx_llm.await;
                }
                serde_json::json!({"success": true})
            }

            // ── Config Sync (flush WAL) ──
            "config/sync" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::Sync { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }

            // ── MCP Config (stored in MemoryGraph) ──
            "mcp/config/get" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(servers)) => serde_json::json!({"servers": servers}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "mcp/config/import" => {
                let servers: Vec<McpServerConfigEntry> = if let Some(config_val) =
                    params.get("config")
                {
                    match serde_json::from_value::<McpConfigFile>(config_val.clone()) {
                        Ok(cfg) => cfg.servers,
                        Err(e) => {
                            return serde_json::json!({"error": format!("Invalid config format: {}", e)});
                        }
                    }
                } else if let Some(config_path) = params.get("path").and_then(|v| v.as_str()) {
                    if config_path.is_empty() {
                        return serde_json::json!({"error": "Missing 'path' parameter"});
                    }
                    let content = match std::fs::read_to_string(config_path) {
                        Ok(c) => c,
                        Err(e) => {
                            return serde_json::json!({"error": format!("Failed to read config file: {}", e)})
                        }
                    };
                    match serde_json::from_str::<McpConfigFile>(&content) {
                        Ok(cfg) => cfg.servers,
                        Err(e) => {
                            return serde_json::json!({"error": format!("Failed to parse config file: {}", e)})
                        }
                    }
                } else {
                    return serde_json::json!({"error": "Missing 'config' or 'path' parameter"});
                };

                let (get_tx, get_rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: get_tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                let existing_servers = match get_rx.await {
                    Ok(Ok(srv)) => srv,
                    Ok(Err(e)) => {
                        return serde_json::json!({"error": format!("Failed to get existing config: {}", e)})
                    }
                    Err(_) => {
                        return serde_json::json!({"error": "MemoryGraph actor response error"})
                    }
                };

                let imported_names: std::collections::HashSet<&str> =
                    servers.iter().map(|s| s.name.as_str()).collect();

                for existing in &existing_servers {
                    if !imported_names.contains(existing.name.as_str()) {
                        tracing::info!(
                            "Coordinator: removing stale MCP server '{}' from import",
                            existing.name
                        );
                        let (del_tx, del_rx) = tokio::sync::oneshot::channel();
                        if self
                            .memory_graph_tx
                            .send(MemoryGraphMessage::SetConfig {
                                key: format!("mcp.server.{}", existing.name),
                                value: serde_json::Value::Null,
                                reply_to: del_tx,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MemoryGraph actor not available"});
                        }
                        if let Err(e) = del_rx.await {
                            tracing::warn!(
                                "Coordinator: failed to delete stale server '{}': {}",
                                existing.name,
                                e
                            );
                        }
                    }
                }

                for server in &servers {
                    let entry_json =
                        serde_json::to_value(server).unwrap_or(serde_json::Value::Null);
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    if self
                        .memory_graph_tx
                        .send(MemoryGraphMessage::SetConfig {
                            key: format!("mcp.server.{}", server.name),
                            value: entry_json,
                            reply_to: tx,
                        })
                        .await
                        .is_err()
                    {
                        return serde_json::json!({"error": "MemoryGraph actor not available"});
                    }
                    if let Err(e) = rx.await {
                        return serde_json::json!({"error": format!("Failed to save server '{}': {}", server.name, e)});
                    }
                }

                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(servers)) => {
                        let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers
                            .into_iter()
                            .filter_map(|entry| {
                                let transport = if let Some(url) = entry.url {
                                    spire_core::mcp::client::TransportConfig::Http {
                                        url, headers: entry.headers.unwrap_or_default(),
                                    }
                                } else if let Some(command) = entry.command {
                                    spire_core::mcp::client::TransportConfig::Stdio {
                                        command, args: entry.args, env: entry.env.unwrap_or_default(),
                                    }
                                } else {
                                    tracing::warn!("Coordinator: MCP server '{}' has no transport config, skipping", entry.name);
                                    return None;
                                };
                                Some(spire_core::mcp::client::McpServerConfig {
                                    name: entry.name, transport, autostart: entry.autostart,
                                build_type: None,
                                })
                            })
                            .collect();

                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .mcp_client_tx
                            .send(McpClientMessage::LoadConfigFromGraph {
                                servers: configs,
                                reply_to: tx,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "McpClient actor not available"});
                        }
                        let _ = rx.await;

                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .mcp_client_tx
                            .send(McpClientMessage::ConnectAll { reply_to: tx })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "McpClient actor not available"});
                        }
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx).await;

                        serde_json::json!({"success": true})
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "mcp/config/save" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return serde_json::json!({"error": "Missing 'name' parameter"});
                }
                let entry = McpServerConfigEntry {
                    name: name.clone(),
                    command: params
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    args: params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    env: params.get("env").and_then(|v| v.as_object()).map(|obj| {
                        let mut map = std::collections::HashMap::new();
                        for (k, v) in obj {
                            if let Some(val) = v.as_str() {
                                map.insert(k.clone(), val.to_string());
                            }
                        }
                        map
                    }),
                    url: params
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    headers: params
                        .get("headers")
                        .and_then(|v| v.as_object())
                        .map(|obj| {
                            let mut map = std::collections::HashMap::new();
                            for (k, v) in obj {
                                if let Some(val) = v.as_str() {
                                    map.insert(k.clone(), val.to_string());
                                }
                            }
                            map
                        }),
                    autostart: params
                        .get("autostart")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                };
                let entry_json = serde_json::to_value(entry).unwrap_or(serde_json::Value::Null);
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::SetConfig {
                        key: format!("mcp.server.{}", name),
                        value: entry_json,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .memory_graph_tx
                            .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MemoryGraph actor not available"});
                        }
                        match rx.await {
                            Ok(Ok(servers)) => {
                                let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers.into_iter().filter_map(|entry| {
                                    let transport = if let Some(url) = entry.url { spire_core::mcp::client::TransportConfig::Http { url, headers: entry.headers.unwrap_or_default() } }
                                    else if let Some(command) = entry.command { spire_core::mcp::client::TransportConfig::Stdio { command, args: entry.args, env: entry.env.unwrap_or_default() } }
                                    else { tracing::warn!("Coordinator: MCP server '{}' has no transport config, skipping", entry.name); return None; };
                                    Some(spire_core::mcp::client::McpServerConfig { name: entry.name, transport, autostart: entry.autostart, build_type: None })
                                }).collect();
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::LoadConfigFromGraph {
                                        servers: configs,
                                        reply_to: tx,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = rx.await;
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::ConnectAll { reply_to: tx })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
                                    .await;
                                serde_json::json!({"success": true})
                            }
                            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                            Err(_) => {
                                serde_json::json!({"error": "MemoryGraph actor response error"})
                            }
                        }
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "mcp/config/delete" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return serde_json::json!({"error": "Missing 'name' parameter"});
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::SetConfig {
                        key: format!("mcp.server.{}", name),
                        value: serde_json::Value::Null,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .memory_graph_tx
                            .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MemoryGraph actor not available"});
                        }
                        match rx.await {
                            Ok(Ok(servers)) => {
                                let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers.into_iter().filter_map(|entry| {
                                    let transport = if let Some(url) = entry.url { spire_core::mcp::client::TransportConfig::Http { url, headers: entry.headers.unwrap_or_default() } }
                                    else if let Some(command) = entry.command { spire_core::mcp::client::TransportConfig::Stdio { command, args: entry.args, env: entry.env.unwrap_or_default() } }
                                    else { tracing::warn!("Coordinator: MCP server '{}' has no transport config, skipping", entry.name); return None; };
                                    Some(spire_core::mcp::client::McpServerConfig { name: entry.name, transport, autostart: entry.autostart, build_type: None })
                                }).collect();
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::LoadConfigFromGraph {
                                        servers: configs,
                                        reply_to: tx,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = rx.await;
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::ConnectAll { reply_to: tx })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
                                    .await;
                                serde_json::json!({"success": true})
                            }
                            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                            Err(_) => {
                                serde_json::json!({"error": "MemoryGraph actor response error"})
                            }
                        }
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }

            // ── Ping / Health ──
            "plan/create" => {
                let goal = params.get("goal").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if goal.is_empty() {
                    return serde_json::json!({"error": "Missing 'goal' parameter"});
                }
                // Project root (from project/open via the shared state). Used so
                // the LLM generates paths under the real project, not the CWD.
                // Prefer an explicit `workspace_root` param; fall back to the
                // FFI-shared project root (the FFI previously injected this).
                let workspace_root = params
                    .get("workspace_root")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        self.ffi_state.as_ref().and_then(|s| {
                            s.project_root
                                .lock()
                                .unwrap()
                                .as_ref()
                                .map(|p| p.to_string_lossy().to_string())
                        })
                    })
                    .unwrap_or_default();
                let scope = params.get("scope").and_then(|v| v.as_str()).unwrap_or("project");
                let scope_val = if scope == "subproject" {
                    params.get("scope_path").and_then(|v| v.as_str()).map(|p| crate::subsystems::planning::plan_orchestrator::ModificationScope::Subproject { path: p.to_string() })
                } else {
                    Some(crate::subsystems::planning::plan_orchestrator::ModificationScope::Project)
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self.plan_orchestrator_tx.send(PlanOrchestratorMessage::CreatePlan {
                    goal,
                    intent_name: Some("modification".to_string()),
                    parameters: std::collections::HashMap::new(),
                    scope: scope_val,
                    workspace_root: if workspace_root.is_empty() { None } else { Some(workspace_root) },
                    reply_to: tx,
                }).await.is_err() {
                    return serde_json::json!({"error": "PlanOrchestrator not available"});
                }
                match rx.await {
                    Ok(Ok(plan)) => serde_json::to_value(plan).unwrap_or(serde_json::json!({"error": "Serialization error"})),
                    Ok(Err(e)) => serde_json::json!({"error": format!("Plan creation failed: {}", e)}),
                    Err(e) => serde_json::json!({"error": format!("PlanOrchestrator response error: {}", e)}),
                }
            }

            "plan/approve" => {
                let plan_id = params.get("plan_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if plan_id.is_empty() {
                    return serde_json::json!({"error": "Missing 'plan_id' parameter"});
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self.plan_orchestrator_tx.send(PlanOrchestratorMessage::ApprovePlan {
                    plan_id,
                    reply_to: tx,
                }).await.is_err() {
                    return serde_json::json!({"error": "PlanOrchestrator not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"ok": true}),
                    Ok(Err(e)) => serde_json::json!({"error": format!("Plan approval failed: {}", e)}),
                    Err(e) => serde_json::json!({"error": format!("PlanOrchestrator response error: {}", e)}),
                }
            }


            "ping" => {
                serde_json::json!({"pong": true})
            }

            "hal/fixPropose" => {
                let root = params.get("root").and_then(|v| v.as_str()).unwrap_or("");
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
                self.propose_hal_fix(root, path).await
            }

            // ── Unknown method ──
            _ => {
                serde_json::json!({"error": format!("Method not found: {}", method)})
            }
        }
    }

    /// Parse XML/Claude-format tool calls from a response content string.
    fn parse_xml_tool_calls(content: &str) -> Option<Vec<serde_json::Value>> {
        if !content.contains("function_calls") {
            return None;
        }

        let invoke_re = Regex::new(
            r#"(?s)<(?:｜DSML｜)?invoke\s+name\s*=\s*"([^"]+)">(.*?)</(?:｜DSML｜)?invoke>"#,
        )
        .ok()?;

        let mut tool_calls = Vec::new();
        let mut call_id_counter = 0u64;

        for cap in invoke_re.captures_iter(content) {
            let function_name = cap.get(1)?.as_str().to_string();
            let params_body = cap.get(2)?.as_str();

            let param_re = Regex::new(
                r#"<(?:｜DSML｜)?parameter\s+name\s*=\s*"([^"]+)"(?:\s+string\s*=\s*"(true|false)")?\s*>(.*?)</(?:｜DSML｜)?parameter>"#
            ).ok()?;

            let mut args = serde_json::Map::new();
            for param_cap in param_re.captures_iter(params_body) {
                let param_name = param_cap.get(1)?.as_str().to_string();
                let param_value = param_cap.get(3)?.as_str().to_string();
                args.insert(param_name, serde_json::json!(param_value));
            }

            call_id_counter += 1;
            tool_calls.push(serde_json::json!({
                "id": format!("call_xml_{}", call_id_counter),
                "type": "function",
                "function": {
                    "name": function_name,
                    "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string()),
                }
            }));
        }

        if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        }
    }
}

// ============================================================================
// FFI-inline RPC handlers — moved from `ffi.rs::process_json_request` so ALL
// method routing lives in this one router. They require the app-only dispatch
// deps attached via `CoordinatorMessage::SetFfiDeps` (registry + shared state).
// ============================================================================

impl CoordinatorActor {
    /// Borrow the app-only dispatch deps (registry + shared state). `None` in
    /// the standalone binary, whose extension flow uses the tools/ methods.
    fn ffi_deps(&self) -> Result<(&Arc<ServiceRegistry>, &Arc<FfiSharedState>), &'static str> {
        match (&self.registry, &self.ffi_state) {
            (Some(r), Some(s)) => Ok((r, s)),
            _ => Err("FFI dispatch deps not attached (standalone binary)"),
        }
    }

    /// Open + analyze a project directory. Sets the shared project root and
    /// analysis, bootstraps per-project graph/MCP/project actors, and returns
    /// the Swift-shaped ProjectInfo.
    async fn handle_project_open(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let root = params.get("root").and_then(|v| v.as_str()).map(PathBuf::from);
        let root = match root {
            Some(r) if !r.as_os_str().is_empty() => r,
            _ => return serde_json::json!({"error": "Missing root"}),
        };

        // Guard against opening the user's home directory (or the filesystem
        // root) — scanning them hangs for minutes.
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
        let is_root_dir = root_canon.as_os_str() == std::ffi::OsStr::new("/");
        let is_home = std::env::var("HOME")
            .ok()
            .map(|h| {
                let hp = PathBuf::from(h);
                let hc = hp.canonicalize().unwrap_or(hp);
                root_canon == hc
            })
            .unwrap_or(false);
        if is_root_dir || is_home {
            return serde_json::json!({
                "error": "Please choose a project directory, not your home directory"
            });
        }
        if !root.exists() {
            if let Err(e) = std::fs::create_dir_all(&root) {
                return serde_json::json!({
                    "error": format!("Failed to create project dir {}: {}", root.display(), e)
                });
            }
            tracing::info!("project/open: created {}", root.display());
        }

        // Auto-descend wrapper directories (double-nesting artifact from
        // scaffolding <name> into a folder already named <name>).
        let root = resolve_project_root(&root);
        let data_dir = root.join(".spire").join("data");
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            return serde_json::json!({"error": format!("Failed to create data dir: {}", e)});
        }

        let result: Result<ProjectAnalysis, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<MemoryGraphMessage>("memory_graph")
                .unwrap_or_else(dummy_tx)
                .send(MemoryGraphMessage::Initialize {
                    data_dir: data_dir.clone(),
                    reply_to: t,
                })
                .await;
            if let Err(e) = r.await {
                return Err(format!("MemoryGraph init lost: {}", e));
            }
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<MemoryGraphMessage>("memory_graph")
                .unwrap_or_else(dummy_tx)
                .send(MemoryGraphMessage::InitializeEmbedder {
                    model_path: None,
                    embedder: Some(
                        Arc::new(spire_core::embedder::NoopEmbedder)
                            as Arc<dyn spire_core::models::embedding::Embedder>,
                    ),
                    reply_to: t,
                })
                .await;
            if let Err(e) = r.await {
                return Err(format!("Embedder init lost: {}", e));
            }

            // ── Bootstrap MCP config into the project graph ──
            // Prefer the project's own config/mcp-config.json; fall back to the
            // bundled global config so fresh/empty projects still get their
            // language MCP servers seeded into the graph.
            let mut config_path = root.join("config").join("mcp-config.json");
            if !config_path.exists() {
                let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .map(|p| p.join("config").join("mcp-config.json"))
                    .unwrap_or_default();
                if bundled.exists() {
                    config_path = bundled;
                }
            }
            if config_path.exists() {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = registry
                    .get::<MemoryGraphMessage>("memory_graph")
                    .unwrap_or_else(dummy_tx)
                    .send(MemoryGraphMessage::BootstrapMcpConfig {
                        config_path: config_path.clone(),
                        reply_to: t,
                    })
                    .await;
                let _ = r.await;
            }
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<MemoryGraphMessage>("memory_graph")
                .unwrap_or_else(dummy_tx)
                .send(MemoryGraphMessage::GetMcpConfig { reply_to: t })
                .await;
            if let Ok(Ok(servers)) = r.await {
                if !servers.is_empty() {
                    use spire_core::mcp::client::{McpServerConfig, TransportConfig};
                    let configs: Vec<McpServerConfig> = servers
                        .into_iter()
                        .filter_map(|entry| {
                            let transport = if let Some(url) = entry.url {
                                TransportConfig::Http {
                                    url,
                                    headers: entry.headers.unwrap_or_default(),
                                }
                            } else if let Some(cmd) = entry.command {
                                TransportConfig::Stdio {
                                    command: cmd,
                                    args: entry.args,
                                    env: entry.env.unwrap_or_default(),
                                }
                            } else {
                                return None;
                            };
                            Some(McpServerConfig {
                                name: entry.name,
                                transport,
                                autostart: entry.autostart,
                                build_type: None,
                            })
                        })
                        .collect();
                    if !configs.is_empty() {
                        let (t, _r) = tokio::sync::oneshot::channel();
                        let _ = self
                            .mcp_client_tx
                            .send(McpClientMessage::LoadConfigFromGraph {
                                servers: configs,
                                reply_to: t,
                            })
                            .await;
                        let (t, r) = tokio::sync::oneshot::channel();
                        let _ = self
                            .mcp_client_tx
                            .send(McpClientMessage::ConnectAll { reply_to: t })
                            .await;
                        // Wait for the servers to actually connect before
                        // continuing — avoids a race where plan generation
                        // starts before MCP tools are available.
                        let _ = r.await;
                    }
                }
            }

            // ── Bootstrap per-project actors ──
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectQueryMessage>("project.query")
                .unwrap_or_else(dummy_tx)
                .send(ProjectQueryMessage::Initialize {
                    memory_graph_tx: registry
                        .get::<MemoryGraphMessage>("memory_graph")
                        .unwrap_or_else(dummy_tx)
                        .clone(),
                    project_root: root.clone(),
                    reply_to: t,
                })
                .await;
            let _ = r.await;
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectSyncMessage>("project.sync")
                .unwrap_or_else(dummy_tx)
                .send(ProjectSyncMessage::Bootstrap {
                    project_root: root.clone(),
                    reply_to: t,
                })
                .await;
            let _ = r.await;
            let _ = registry
                .get::<FileWatcherMessage>("tools.watcher")
                .unwrap_or_else(dummy_tx)
                .send(FileWatcherMessage::StopWatching)
                .await;
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<FileWatcherMessage>("tools.watcher")
                .unwrap_or_else(dummy_tx)
                .send(FileWatcherMessage::StartWatching {
                    root: root.clone(),
                    output: ffi_state.watcher_out_tx.clone(),
                    reply_to: t,
                })
                .await;
            let _ = r.await;
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectAnalyzerMessage>("project.analyzer")
                .unwrap_or_else(dummy_tx)
                .send(ProjectAnalyzerMessage::Analyze {
                    project_root: root.clone(),
                    reply_to: t,
                })
                .await;
            match r.await {
                Ok(Ok(a)) => Ok(a),
                Ok(Err(e)) => Err(format!("Analysis: {}", e)),
                Err(e) => Err(format!("Analysis lost: {}", e)),
            }
        }
        .await;

        match result {
            Ok(analysis) => {
                *ffi_state.analysis.lock().unwrap() = Some(analysis.clone());
                *ffi_state.project_root.lock().unwrap() = Some(root.clone());
                // Keep ProjectBuildActor's root in sync so relative build paths
                // resolve against the newly-opened project.
                let _ = registry
                    .get::<ProjectBuildMessage>("project.build")
                    .unwrap_or_else(dummy_tx)
                    .send(ProjectBuildMessage::SetProjectRoot { root: root.clone() })
                    .await;
                // Populate first-class target nodes so the graph can be queried
                // via project/getBuildTarget (deps/platform/files).
                let _ = populate_target_graph(registry, &analysis.build_systems).await;
                serialize_analysis(&analysis)
            }
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// Re-run a fresh project analysis (always a disk scan, never cached).
    async fn handle_analyze_project(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let custom_root = params.get("root").and_then(|v| v.as_str()).map(PathBuf::from);
        if let Some(root) = custom_root {
            // Resolve wrapper folders (same auto-descend as project/open) so a
            // refresh/analyze on the OUTER path never re-points the project root.
            let resolved = resolve_project_root(&root);
            let analysis: Option<ProjectAnalysis> = async {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = registry
                    .get::<ProjectAnalyzerMessage>("project.analyzer")
                    .unwrap_or_else(dummy_tx)
                    .send(ProjectAnalyzerMessage::Analyze {
                        project_root: resolved.clone(),
                        reply_to: t,
                    })
                    .await;
                r.await.ok().and_then(|r| r.ok())
            }
            .await;
            match analysis {
                Some(a) => {
                    // Keep the shared project root in sync so subsequent relative
                    // file reads resolve against the REAL project root.
                    *ffi_state.project_root.lock().unwrap() = Some(resolved.clone());
                    // Re-point ProjectBuildActor (it resolves relative build paths).
                    let _ = registry
                        .get::<ProjectBuildMessage>("project.build")
                        .unwrap_or_else(dummy_tx)
                        .send(ProjectBuildMessage::SetProjectRoot { root: resolved.clone() })
                        .await;
                    return serialize_analysis(&a);
                }
                None => {
                    return serde_json::json!({
                        "error": format!("Failed to analyze project at {}", resolved.display())
                    });
                }
            }
        }

        // Return real analysis if available, otherwise an actionable error.
        if let Some(ref analysis) = *ffi_state.analysis.lock().unwrap() {
            return serialize_analysis(analysis);
        }
        serde_json::json!({
            "error": "No project opened; call project/open with a root directory"
        })
    }

    /// Fetch the last persisted build status for a directory (+ optional target)
    /// from the graph config key `build.last.<path>[.<target>]`.
    async fn handle_project_build_status(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let status_path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status_target = params
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let key = if status_target.trim().is_empty() {
            format!("build.last.{}", status_path)
        } else {
            format!("build.last.{}.{}", status_path, status_target)
        };

        let (t, r) = tokio::sync::oneshot::channel();
        let _ = registry
            .get::<MemoryGraphMessage>("memory_graph")
            .unwrap_or_else(dummy_tx)
            .send(MemoryGraphMessage::GetConfig { key, reply_to: t })
            .await;
        match r.await {
            Ok(Ok(Some(value))) => value,
            _ => serde_json::Value::Null,
        }
    }

    /// Query the knowledge graph for Diagnostic nodes and return them as a JSON
    /// array, optionally filtered by subproject directory path.
    async fn handle_project_diagnostics(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let filter_path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_default();

        let (t, r) = tokio::sync::oneshot::channel();
        let _ = registry
            .get::<MemoryGraphMessage>("memory_graph")
            .unwrap_or_else(dummy_tx)
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Diagnostic".to_string()),
                subtype: None,
                name: None,
                limit: Some(2000),
                reply_to: t,
            })
            .await;
        let result: Vec<serde_json::Value> = match r.await {
            Ok(Ok(nodes)) => {
                let mut filtered: Vec<serde_json::Value> = Vec::new();
                let mut all_build: Vec<serde_json::Value> = Vec::new();
                for node in nodes {
                    let message = node
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let file = node.get("file").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let line = node.get("line").and_then(|v| v.as_u64()).map(|n| n as u32);
                    let column = node.get("column").and_then(|v| v.as_u64()).map(|n| n as u32);
                    let severity = node
                        .get("severity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let build_type = node
                        .get("build_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let build_run_id = node
                        .get("build_run_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if severity != "warning" && severity != "error" {
                        continue;
                    }
                    if message.trim().is_empty() {
                        continue;
                    }
                    let f = file.clone().unwrap_or_default();
                    let entry = serde_json::json!({
                        "severity": severity,
                        "file": file,
                        "line": line,
                        "column": column,
                        "message": message,
                        "buildType": build_type,
                        "buildRunId": build_run_id,
                    });
                    if build_type == "build" {
                        all_build.push(entry);
                        continue;
                    }
                    if filter_path.is_empty() || f.is_empty() || f.starts_with(&filter_path) {
                        filtered.push(entry);
                    }
                }
                if filtered.is_empty() && !all_build.is_empty() && !filter_path.is_empty() {
                    // Subproject has no diagnostics of its own yet — show the
                    // shared build's warnings so the pane isn't misleading.
                    filtered = all_build;
                }
                filtered
            }
            _ => Vec::new(),
        };
        serde_json::to_value(result).unwrap_or(serde_json::Value::Null)
    }

    /// Answer `project/getBuildTarget` (bare method or the `tools/call`
    /// envelope from Swift) directly from the in-memory analysis — the
    /// authoritative BuildManager result — so target-scoped data is always
    /// available the moment analysis completes.
    async fn handle_project_get_build_target(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (_registry, ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let (target_name, _is_envelope) = if method == "tools/call" {
            let name = params
                .get("args")
                .and_then(|a| a.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (name, true)
        } else {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (name, false)
        };

        let Some(analysis) = ffi_state.analysis.lock().unwrap().as_ref().cloned() else {
            return serde_json::json!({"error": "No project analyzed yet"});
        };

        // Find the BuildMetadata whose targets contain the requested name.
        let Some(meta) = analysis
            .build_systems
            .iter()
            .find(|bs| bs.targets.iter().any(|t| t.name == target_name))
        else {
            return serde_json::json!({
                "error": format!("Build target not found: {}", target_name)
            });
        };

        // Use the analyzer's parsed source files for this target (the exact set
        // compiled for it) instead of guessing directories.
        let target = meta.targets.iter().find(|t| t.name == target_name);
        let declared_sources: Vec<String> = target
            .map(|t| t.source_files.iter().cloned().collect())
            .unwrap_or_default();

        let mut files: Vec<serde_json::Value> = Vec::new();
        if !declared_sources.is_empty() {
            let scope_dir = target_name
                .strip_prefix("ai-trap-")
                .map(|s| s.to_string())
                .unwrap_or_default();
            for s in &declared_sources {
                let path = if s.starts_with("src/") || s.starts_with("include/") {
                    format!("toolkit/{s}")
                } else if !scope_dir.is_empty() {
                    format!("{scope_dir}/{s}")
                } else {
                    s.clone()
                };
                files.push(serde_json::json!({
                    "path": path,
                    "role": "source",
                    "language": "C++",
                }));
            }
        } else {
            // Fallback when source_files weren't parsed — scope to the platform dir.
            let scope_dir = target_name
                .strip_prefix("ai-trap-")
                .map(|s| s.to_string())
                .unwrap_or_default();
            if scope_dir.is_empty() {
                crate::ffi::collect_tree_files(&analysis.file_tree, &mut files);
            } else if let Some(dir_node) = crate::ffi::find_tree_dir(&analysis.file_tree, &scope_dir)
            {
                crate::ffi::collect_tree_files(dir_node, &mut files);
            }
        }

        // Dependencies are PER-TARGET — serve the target's own list parsed from
        // meson.build, not the flat metadata-level list.
        let target_deps = meta
            .targets
            .iter()
            .find(|t| t.name == target_name)
            .map(|t| t.dependencies.clone())
            .unwrap_or_default();
        let deps: Vec<serde_json::Value> = if !target_deps.is_empty() {
            target_deps
                .iter()
                .map(|d| serde_json::json!({
                    "name": d.name,
                    "version": d.version_req,
                }))
                .collect()
        } else {
            meta.dependencies
                .iter()
                .map(|d| serde_json::json!({
                    "name": d.name,
                    "version": d.version_req,
                }))
                .collect()
        };

        serde_json::json!({
            "name": target_name,
            "kind": meta.targets.iter().find(|t| t.name == target_name)
                .and_then(|t| t.kind.first()).cloned().unwrap_or_default(),
            "configFile": meta.config_files.first().cloned().unwrap_or_default(),
            "platform": meta.platform_targets,
            "dependencies": deps,
            "files": files,
        })
    }

    /// `createProject/Plan` — single-round-trip describe-the-project flow:
    /// compute the in-memory structural contract (no disk writes) + LLM plan,
    /// returning both `{plan, spec}` for UI approval.
    /// Read the wizard's `structure`/`embedded` params (shared by all
    /// createProject/* handlers). `structure` defaults to None (→ Native).
    fn params_structure_embedded(
        params: &serde_json::Value,
    ) -> (
        Option<spire_core::build_types::ProjectStructure>,
        bool,
    ) {
        let structure = params
            .get("structure")
            .and_then(|v| v.as_str())
            .map(spire_core::build_types::ProjectStructure::from_str);
        let embedded = params
            .get("embedded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        (structure, embedded)
    }

    async fn handle_create_project_plan(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("Rust")
            .to_string();
        let platforms: Vec<String> = params
            .get("platforms")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let (structure, embedded) = Self::params_structure_embedded(params);

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::PlanScaffold {
                    goal,
                    root_dir: PathBuf::from(root_dir),
                    project_name,
                    language,
                    platforms,
                    structure,
                    embedded,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(res)) => serde_json::json!({
                "plan": res.plan,
                "spec": res.spec,
            }),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/GeneratePlan` — LLM-decomposed plan for a new project.
    async fn handle_create_project_generate_plan(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("Rust")
            .to_string();
        let platforms: Vec<String> = params
            .get("platforms")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let (structure, embedded) = Self::params_structure_embedded(params);

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::GeneratePlan {
                    goal,
                    root_dir: PathBuf::from(root_dir),
                    language,
                    platforms,
                    structure,
                    embedded,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(plan)) => serde_json::to_value(plan).unwrap_or(serde_json::json!({"error": "serialize"})),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/Scaffold` — materialize the structural scaffold offline.
    async fn handle_create_project_scaffold(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("Rust")
            .to_string();
        let platforms: Vec<String> = params
            .get("platforms")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let (structure, embedded) = Self::params_structure_embedded(params);

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::ScaffoldProject {
                    project_name,
                    root_dir: PathBuf::from(root_dir),
                    language,
                    platforms,
                    structure,
                    embedded,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(spec)) => serde_json::to_value(spec).unwrap_or(serde_json::json!({"error": "serialize"})),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/Fill` — constrained LLM fill of a materialized scaffold.
    async fn handle_create_project_fill(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let spec: crate::subsystems::build::build_manager::ScaffoldSpec = params
            .get("spec")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::FillProject {
                    goal,
                    root_dir: PathBuf::from(root_dir),
                    spec,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(plan)) => serde_json::to_value(plan).unwrap_or(serde_json::json!({"error": "serialize"})),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/GenerateSpec` — SpireApp requirements pass: derive a
    /// VALIDATED AppSpec JSON contract from the goal (self-healed against
    /// `spec::validate`). Writes nothing to disk; the spec drives the later
    /// fill/codegen phase.
    async fn handle_create_project_generate_spec(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::GenerateAppSpec {
                    project_name,
                    goal,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {e}"))
        }
        .await;
        match result {
            Ok(Ok(spec)) => serde_json::to_value(spec).unwrap_or(serde_json::json!({"error": "serialize"})),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/GenerateCode` — deterministic skeleton steps from a
    /// VALIDATED AppSpec (types/actors/FFI dispatch + Swift wrappers/screens,
    /// bridge-derived routing). Returns the `write_source_file` steps; the
    /// caller executes them via `createProject/ExecutePlan`.
    async fn handle_create_project_generate_code(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let spec: crate::subsystems::project::spec::AppSpec = match params
            .get("spec")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            Some(s) => s,
            None => return serde_json::json!({"error": "missing or invalid 'spec'"}),
        };

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::GenerateCode {
                    project_name,
                    spec,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {e}"))
        }
        .await;
        match result {
            Ok(Ok(steps)) => serde_json::to_value(steps).unwrap_or(serde_json::json!({"error": "serialize"})),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/ExecutePlan` — execute the entire plan sequentially.
    async fn handle_create_project_execute_plan(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let steps: Vec<crate::subsystems::project::project_creation::CreationStep> = params
            .get("steps")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::ExecutePlan {
                    root_dir: PathBuf::from(root_dir),
                    steps,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(results)) => serde_json::to_value(results).unwrap_or(serde_json::json!({"error": "serialize"})),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/ExecuteStep` — execute a single creation step.
    async fn handle_create_project_execute_step(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let step: crate::subsystems::project::project_creation::CreationStep = params
            .get("step")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(crate::subsystems::project::project_creation::CreationStep {
                id: String::new(),
                step_type: crate::subsystems::project::project_creation::CreationStepType::Build,
                description: String::new(),
                status: crate::subsystems::project::project_creation::StepStatus::Pending,
                parameters: serde_json::json!({}),
                result: None,
            });

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::ExecuteStep {
                    root_dir: PathBuf::from(root_dir),
                    step,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(res)) => serde_json::to_value(res).unwrap_or(serde_json::json!({"error": "serialize"})),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `rag/*` RPCs — semantic search, interface lookup, domain/source listing,
    /// ingest. Routed to the RAG actor; `rag/set-domain` writes shared state.
    async fn handle_rag(&self, method: &str, params: &serde_json::Value) -> serde_json::Value {
        let (registry, ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let rag_tx = match registry.get::<RagMessage>("rag") {
            Some(tx) => tx.clone(),
            None => return serde_json::json!({"error": "RAG subsystem not available"}),
        };

        match method {
            "rag/search" => {
                let domain = params.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let top_k = params.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx.send(RagMessage::Query { domain, query, top_k, reply_to: t }).await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/find-interfaces" => {
                let domain = params.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let top_k = params.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx.send(RagMessage::FindInterfaces { domain, query, top_k, reply_to: t }).await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/list-domains" => {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx.send(RagMessage::ListDomains { reply_to: t }).await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/list-manifests" => {
                let project_root = params
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default();
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx.send(RagMessage::ListManifests { project_root, reply_to: t }).await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/set-domain" => {
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Ok(mut guard) = ffi_state.default_rag_domain.lock() {
                    *guard = if domain.is_empty() { None } else { Some(domain) };
                }
                serde_json::json!({"ok": true})
            }
            "rag/list-sources" => {
                let domain = params.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx.send(RagMessage::ListSources { domain, reply_to: t }).await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/ingest-graph-config" => {
                let manifest_path = params
                    .get("manifest_path")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default();
                let project_root = params
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .filter(|p| !p.as_os_str().is_empty());
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx
                    .send(RagMessage::IngestGraphConfig { manifest_path, project_root, reply_to: t })
                    .await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            _ => serde_json::json!({"error": "unknown rag method"}),
        }
    }
}





