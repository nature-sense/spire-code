// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Composer wiring — build the default `ToolRegistry` with the full tool set.
//!
//! The registry starts empty; this builder registers everything the app needs:
//! generic tools from this crate (VS Code extension, in-process core modules,
//! web search, RAG) and the coding tools from `spire-core`'s project/build
//! actors (which move to `spire-code` in a later step — the coding registration
//! moves with them). MCP tools stay dynamic (queried per `ListTools`), so they
//! are not registered here.

use crate::subsystems::build::build_manager::BuildManagerMessage;
use crate::subsystems::project::project_build::{ProjectBuildActor, ProjectBuildMessage};
use crate::subsystems::project::project_install::{ProjectInstallActor, ProjectInstallMessage};
use crate::subsystems::project::project_lint::{ProjectLintActor, ProjectLintMessage};
use crate::subsystems::project::project_query::{ProjectQueryActor, ProjectQueryMessage};
use crate::subsystems::project::project_test::{ProjectTestActor, ProjectTestMessage};
use spire_core::actors::rag::RagMessage;
use spire_core::actors::tool_providers::{ToolHandler, ToolRegistry};
use crate::actors::vscode_tool_definitions;
use spire_core::actors::web_search;
use spire_core::actors::ToolInfo;
use spire_core::modules::{
    FilesystemMessage, GitMessage, ProcessMessage, SearchMessage, TerminalMessage,
};
use spire_core::transport::socket::TransportMessage;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

/// Build the registry with the full tool set. Call once at startup.
///
/// MCP tools are not registered here — they are dynamic and stay in the
/// router's catch-all.
#[allow(clippy::too_many_arguments)]
pub async fn build_default_registry(
    transport_tx: mpsc::Sender<TransportMessage>,
    project_query_tx: mpsc::Sender<ProjectQueryMessage>,
    project_build_tx: Option<mpsc::Sender<ProjectBuildMessage>>,
    project_test_tx: Option<mpsc::Sender<ProjectTestMessage>>,
    project_lint_tx: Option<mpsc::Sender<ProjectLintMessage>>,
    project_install_tx: Option<mpsc::Sender<ProjectInstallMessage>>,
    filesystem_tx: mpsc::Sender<FilesystemMessage>,
    git_tx: mpsc::Sender<GitMessage>,
    process_tx: mpsc::Sender<ProcessMessage>,
    search_tx: mpsc::Sender<SearchMessage>,
    terminal_tx: mpsc::Sender<TerminalMessage>,
    build_manager_tx: mpsc::Sender<BuildManagerMessage>,
    rag_tx: mpsc::Sender<RagMessage>,
    default_domain: Arc<Mutex<Option<String>>>,
) -> Result<Arc<ToolRegistry>, String> {
    let registry = Arc::new(ToolRegistry::new());

    // VS Code extension tools (workspace/, document/, diagnostics/, git/, symbols/).
    // The `rag/*` tools are defined in this list but ROUTED to the RAG actor
    // (not the transport), matching the legacy router's behaviour.
    let (rag_defs, ext_defs): (Vec<_>, Vec<_>) = vscode_tool_definitions()
        .into_iter()
        .partition(|t| t.name.starts_with("rag/"));
    register_static(&registry, ext_defs, |name| {
        extension_handler(transport_tx.clone(), name)
    })?;

    // Project meta-actors (static definitions, per-actor handlers).
    register_static(&registry, ProjectQueryActor::tool_definitions(), |name| {
        actor_tool_handler(project_query_tx.clone(), name, |tool, args, reply| {
            ProjectQueryMessage::CallTool { tool, args, reply_to: reply }
        })
    })?;
    if let Some(tx) = project_build_tx {
        register_static(&registry, ProjectBuildActor::tool_definitions(), |name| {
            actor_tool_handler_result(tx.clone(), name, |tool, args, reply| {
                ProjectBuildMessage::CallTool { tool, args, reply_to: reply }
            })
        })?;
    }
    if let Some(tx) = project_test_tx {
        register_static(&registry, ProjectTestActor::tool_definitions(), |name| {
            actor_tool_handler_result(tx.clone(), name, |tool, args, reply| {
                ProjectTestMessage::CallTool { tool, args, reply_to: reply }
            })
        })?;
    }
    if let Some(tx) = project_lint_tx {
        register_static(&registry, ProjectLintActor::tool_definitions(), |name| {
            actor_tool_handler_result(tx.clone(), name, |tool, args, reply| {
                ProjectLintMessage::CallTool { tool, args, reply_to: reply }
            })
        })?;
    }
    if let Some(tx) = project_install_tx {
        register_static(&registry, ProjectInstallActor::tool_definitions(), |name| {
            actor_tool_handler_result(tx.clone(), name, |tool, args, reply| {
                ProjectInstallMessage::CallTool { tool, args, reply_to: reply }
            })
        })?;
    }

    // In-process core modules (dynamic definitions via ListTools).
    register_msg_backend(
        &registry,
        filesystem_tx,
        |reply| FilesystemMessage::ListTools { reply_to: reply },
        |tool_name, args, reply| FilesystemMessage::CallTool { tool_name, args, reply_to: reply },
    )
    .await?;
    register_msg_backend(
        &registry,
        git_tx,
        |reply| GitMessage::ListTools { reply_to: reply },
        |tool_name, args, reply| GitMessage::CallTool { tool_name, args, reply_to: reply },
    )
    .await?;
    register_msg_backend(
        &registry,
        process_tx,
        |reply| ProcessMessage::ListTools { reply_to: reply },
        |tool_name, args, reply| ProcessMessage::CallTool { tool_name, args, reply_to: reply },
    )
    .await?;
    register_msg_backend(
        &registry,
        search_tx,
        |reply| SearchMessage::ListTools { reply_to: reply },
        |tool_name, args, reply| SearchMessage::CallTool { tool_name, args, reply_to: reply },
    )
    .await?;
    register_msg_backend(
        &registry,
        terminal_tx,
        |reply| TerminalMessage::ListTools { reply_to: reply },
        |tool_name, args, reply| TerminalMessage::CallTool { tool_name, args, reply_to: reply },
    )
    .await?;

    // BuildManager (dynamic definitions; routes build_/hal_ tools internally).
    register_msg_backend(
        &registry,
        build_manager_tx,
        |reply| BuildManagerMessage::ListTools { reply_to: reply },
        |tool_name, args, reply| BuildManagerMessage::CallTool { tool_name, args, reply_to: reply },
    )
    .await?;

    // Web search tools.
    register_static(&registry, web_search::tool_definitions(), |name| {
        web_search_handler(name)
    })?;

    // RAG tools (generic — the RAG framework lives in this crate). Their
    // definitions come from `vscode_tool_definitions()` (partitioned above).
    register_rag_tools(&registry, rag_tx, default_domain, rag_defs)?;

    Ok(registry)
}

/// Register a static tool-definition list, building each handler with `make`.
fn register_static(
    registry: &ToolRegistry,
    defs: Vec<ToolInfo>,
    make: impl Fn(String) -> ToolHandler,
) -> Result<(), String> {
    let tools = defs
        .into_iter()
        .map(|info| {
            let handler = make(info.name.clone());
            (info, handler)
        })
        .collect::<Vec<_>>();
    registry.register_many(tools)
}

/// Handler that forwards a call to a message backend via the given
/// message constructor (`ctor` is a zero-capture closure per backend).
fn actor_tool_handler<M, C>(
    tx: mpsc::Sender<M>,
    name: String,
    ctor: C,
) -> ToolHandler
where
    M: Send + 'static,
    C: Fn(String, Value, oneshot::Sender<Value>) -> M + Send + Sync + Clone + 'static,
{
    Arc::new(move |args| {
        let tx = tx.clone();
        let name = name.clone();
        let ctor = ctor.clone();
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            let msg = ctor(name.clone(), args, reply);
            tx.send(msg)
                .await
                .map_err(|e| format!("backend send error: {e}"))?;
            rx.await.map_err(|e| format!("backend response error: {e}"))
        })
    })
}

/// Handler variant for backends whose `CallTool.reply_to` is
/// `Sender<Result<Value, String>>` (the project build/test/lint/install
/// meta-actors, unlike the modules' plain `Sender<Value>`).
fn actor_tool_handler_result<M, C>(
    tx: mpsc::Sender<M>,
    name: String,
    ctor: C,
) -> ToolHandler
where
    M: Send + 'static,
    C: Fn(String, Value, oneshot::Sender<Result<Value, String>>) -> M + Send + Sync + Clone + 'static,
{
    Arc::new(move |args| {
        let tx = tx.clone();
        let name = name.clone();
        let ctor = ctor.clone();
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            let msg = ctor(name.clone(), args, reply);
            tx.send(msg)
                .await
                .map_err(|e| format!("backend send error: {e}"))?;
            rx.await.map_err(|e| format!("backend response error: {e}"))?
        })
    })
}

/// Register a backend's dynamic tool list (ListTools) with per-tool handlers.
async fn register_msg_backend<M, L, C>(
    registry: &ToolRegistry,
    tx: mpsc::Sender<M>,
    list_ctor: L,
    call_ctor: C,
) -> Result<(), String>
where
    M: Send + 'static,
    L: Fn(oneshot::Sender<Vec<ToolInfo>>) -> M + Send + Sync + Clone + 'static,
    C: Fn(String, Value, oneshot::Sender<Value>) -> M + Send + Sync + Clone + 'static,
{
    let (t, r) = oneshot::channel();
    let _ = tx.send(list_ctor(t)).await;
    let defs = r.await.unwrap_or_default();
    let tools = defs
        .into_iter()
        .map(|info| {
            let name = info.name.clone();
            let handler = actor_tool_handler(tx.clone(), name, call_ctor.clone());
            (info, handler)
        })
        .collect::<Vec<_>>();
    registry.register_many(tools)
}

/// Handler for VS Code extension tools (routed through the transport actor).
fn extension_handler(transport_tx: mpsc::Sender<TransportMessage>, name: String) -> ToolHandler {
    Arc::new(move |args| {
        let tx = transport_tx.clone();
        let name = name.clone();
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            tx.send(TransportMessage::CallExtension {
                method: name,
                params: args,
                reply_to: reply,
            })
            .await
            .map_err(|e| format!("Transport send error: {e}"))?;
            rx.await.map_err(|e| format!("Transport response error: {e}"))?
        })
    })
}

/// Handler for web-search tools (free-function backend).
fn web_search_handler(name: String) -> ToolHandler {
    Arc::new(move |args| {
        let name = name.clone();
        Box::pin(async move { web_search::call(&name, args).await })
    })
}

/// Register the RAG tools (`rag/search`, `rag/find-interfaces`,
/// `rag/set-domain`, `rag/list-domains`) with the shared default-domain state.
/// Their definitions come from `vscode_tool_definitions()` (they are listed as
/// extension tools but routed to the RAG actor).
fn register_rag_tools(
    registry: &ToolRegistry,
    rag_tx: mpsc::Sender<RagMessage>,
    default_domain: Arc<Mutex<Option<String>>>,
    defs: Vec<ToolInfo>,
) -> Result<(), String> {
    let mut tools = Vec::with_capacity(defs.len());
    for info in defs {
        let handler = match info.name.as_str() {
            "rag/search" => rag_query_handler(rag_tx.clone(), default_domain.clone()),
            "rag/find-interfaces" => {
                rag_find_interfaces_handler(rag_tx.clone(), default_domain.clone())
            }
            "rag/set-domain" => rag_set_domain_handler(default_domain.clone()),
            "rag/list-domains" => rag_list_domains_handler(rag_tx.clone()),
            other => return Err(format!("unexpected rag tool definition: {other}")),
        };
        tools.push((info, handler));
    }
    registry.register_many(tools)
}

fn rag_query_handler(
    rag_tx: mpsc::Sender<RagMessage>,
    default_domain: Arc<Mutex<Option<String>>>,
) -> ToolHandler {
    Arc::new(move |args| {
        let tx = rag_tx.clone();
        let default_domain = default_domain.clone();
        Box::pin(async move {
            let mut domain = args
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if domain.is_empty() {
                if let Ok(guard) = default_domain.lock() {
                    domain = guard.clone().unwrap_or_default();
                }
            }
            if domain.is_empty() {
                return Err("no RAG domain selected and no 'domain' supplied".to_string());
            }
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let (reply, rx) = oneshot::channel();
            tx.send(RagMessage::Query {
                domain,
                query,
                top_k,
                reply_to: reply,
            })
            .await
            .map_err(|e| format!("RAG send error: {e}"))?;
            rx.await
                .map_err(|e| format!("RAG response error: {e}"))?
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .map_err(|e| e.to_string())
        })
    })
}

fn rag_find_interfaces_handler(
    rag_tx: mpsc::Sender<RagMessage>,
    default_domain: Arc<Mutex<Option<String>>>,
) -> ToolHandler {
    Arc::new(move |args| {
        let tx = rag_tx.clone();
        let default_domain = default_domain.clone();
        Box::pin(async move {
            let mut domain = args
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if domain.is_empty() {
                if let Ok(guard) = default_domain.lock() {
                    domain = guard.clone().unwrap_or_default();
                }
            }
            if domain.is_empty() {
                return Err("no RAG domain selected and no 'domain' supplied".to_string());
            }
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let (reply, rx) = oneshot::channel();
            tx.send(RagMessage::FindInterfaces {
                domain,
                query,
                top_k,
                reply_to: reply,
            })
            .await
            .map_err(|e| format!("RAG send error: {e}"))?;
            rx.await
                .map_err(|e| format!("RAG response error: {e}"))?
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .map_err(|e| e.to_string())
        })
    })
}

fn rag_set_domain_handler(default_domain: Arc<Mutex<Option<String>>>) -> ToolHandler {
    Arc::new(move |args| {
        let default_domain = default_domain.clone();
        Box::pin(async move {
            let domain = args
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Ok(mut guard) = default_domain.lock() {
                *guard = if domain.is_empty() { None } else { Some(domain) };
            }
            Ok(serde_json::json!({ "ok": true }))
        })
    })
}

fn rag_list_domains_handler(rag_tx: mpsc::Sender<RagMessage>) -> ToolHandler {
    Arc::new(move |_args| {
        let tx = rag_tx.clone();
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            tx.send(RagMessage::ListDomains { reply_to: reply })
                .await
                .map_err(|e| format!("RAG send error: {e}"))?;
            rx.await
                .map_err(|e| format!("RAG response error: {e}"))?
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .map_err(|e| e.to_string())
        })
    })
}


