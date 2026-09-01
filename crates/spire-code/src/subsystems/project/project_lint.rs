// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! ProjectLintActor — meta-tool for linting and formatting across all build systems.
//!
//! Provides `project/lint` and `project/format` which:
//! 1. Call `project/getBuildConfig` to discover all build systems
//! 2. Resolve the scope parameter to determine which systems to lint/format
//! 3. Dispatch the appropriate tool for each system in parallel
//! 4. Aggregate results into a single structured response

use async_trait::async_trait;
use serde_json::Value;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

use crate::subsystems::build::build_manager::BuildManagerMessage;
use spire_core::subsystems::mcp::mcp_client::McpClientMessage;
use crate::subsystems::project::project_query::ProjectQueryMessage;
use spire_core::actors::Actor;
use spire_core::actors::ToolInfo;

// ============================================================================
// ProjectLintMessage
// ============================================================================

/// Messages for the ProjectLint actor.
pub enum ProjectLintMessage {
    /// List the tools provided by this actor.
    ListTools {
        reply_to: oneshot::Sender<Vec<ToolInfo>>,
    },
    /// Handle a tool call.
    CallTool {
        tool: String,
        args: Value,
        reply_to: oneshot::Sender<Result<Value, String>>,
    },
}

// ============================================================================
// ProjectLintActor
// ============================================================================

/// Actor that provides `project/lint` and `project/format` meta-tools.
pub struct ProjectLintActor {
    project_query_tx: mpsc::Sender<ProjectQueryMessage>,
    mcp_client_tx: mpsc::Sender<McpClientMessage>,
    /// Sender to the BuildManagerActor — routes lint/format through the
    /// registered build modules (no hardcoded per-build-system branches).
    build_manager_tx: mpsc::Sender<BuildManagerMessage>,
}

impl ProjectLintActor {
    pub fn new(
        project_query_tx: mpsc::Sender<ProjectQueryMessage>,
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
        build_manager_tx: mpsc::Sender<BuildManagerMessage>,
    ) -> Self {
        Self {
            project_query_tx,
            mcp_client_tx,
            build_manager_tx,
        }
    }

    pub fn tool_definitions() -> Vec<ToolInfo> {
        vec![
            ToolInfo {
                name: "project/lint".to_string(),
                description: "Run linters across all build systems. Calls project/getBuildConfig \
                    to discover build systems, then dispatches the appropriate lint tool \
                    for each system in parallel (e.g. clippy for Cargo). \
                    Use scope=\"all\" for everything, scope=\"Cargo\" for a specific build system, \
                    or scope=\"rust/spire-core\" for a specific subproject path. \
                    Returns aggregated results."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "description": "Lint scope: 'all' (default) for everything, \
                                a build system name like 'Cargo' to filter by type, \
                                or a subpath like 'rust/spire-core' to lint a specific subproject"
                        }
                    }
                }),
            },
            ToolInfo {
                name: "project/format".to_string(),
                description: "Format code across all build systems. Calls project/getBuildConfig \
                    to discover build systems, then dispatches the appropriate format tool \
                    for each system in parallel (e.g. fmt for Cargo). \
                    Use scope=\"all\" for everything, scope=\"Cargo\" for a specific build system, \
                    or scope=\"rust/spire-core\" for a specific subproject path. \
                    Returns aggregated results."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "description": "Format scope: 'all' (default) for everything, \
                                a build system name like 'Cargo' to filter by type, \
                                or a subpath like 'rust/spire-core' to format a specific subproject"
                        }
                    }
                }),
            },
        ]
    }

    // ── Handlers ──────────────────────────────────────────────────────────

    async fn handle_lint(&self, args: &Value) -> Result<Value, String> {
        let start = Instant::now();
        let scope = args.get("scope").and_then(|v| v.as_str());
        info!("project/lint: starting (scope={:?})", scope);

        let systems = self.get_build_config().await?;
        let dispatches = self.resolve_scope(scope, &systems);

        if dispatches.is_empty() {
            return Ok(serde_json::json!({
                "success": true,
                "duration_secs": 0.0,
                "message": "No build systems matched scope",
                "systems": []
            }));
        }

        let mut handles = Vec::new();
        for dispatch in dispatches {
            let mcp_client_tx = self.mcp_client_tx.clone();
            let build_manager_tx = self.build_manager_tx.clone();
            handles.push(tokio::spawn(async move {
                // Lint through the registered build module when available
                // (Cargo → clippy, Meson → clang analyzer, …) — the
                // BuildManager routes by capability.
                let node_family = matches!(
                    dispatch.build_type.as_str(),
                    "npm" | "pnpm" | "yarn"
                );
                let result = if node_family {
                    Self::call_mcp_tool(&mcp_client_tx, "mcp-node", "lint", serde_json::json!({
                        "path": dispatch.path,
                    }))
                    .await
                } else {
                    Self::call_build_module_tool(
                        &build_manager_tx,
                        &dispatch.build_type,
                        "clippy",
                        serde_json::json!({ "path": dispatch.path }),
                    )
                    .await
                };
                (
                    dispatch.build_type,
                    dispatch.path,
                    dispatch.project_name,
                    result,
                )
            }));
        }

        let result = Self::collect_results(handles, start).await;
        info!(
            "project/lint: completed in {:.1}s",
            result["duration_secs"].as_f64().unwrap_or(0.0)
        );
        Ok(result)
    }

    async fn handle_format(&self, args: &Value) -> Result<Value, String> {
        let start = Instant::now();
        let scope = args.get("scope").and_then(|v| v.as_str());
        info!("project/format: starting (scope={:?})", scope);

        let systems = self.get_build_config().await?;
        let dispatches = self.resolve_scope(scope, &systems);

        if dispatches.is_empty() {
            return Ok(serde_json::json!({
                "success": true,
                "duration_secs": 0.0,
                "message": "No build systems matched scope",
                "systems": []
            }));
        }

        let mut handles = Vec::new();
        for dispatch in dispatches {
            let mcp_client_tx = self.mcp_client_tx.clone();
            let build_manager_tx = self.build_manager_tx.clone();
            handles.push(tokio::spawn(async move {
                // Format through the registered build module when available
                // (Cargo → fmt, etc.) — the BuildManager routes by capability.
                let node_family = matches!(
                    dispatch.build_type.as_str(),
                    "npm" | "pnpm" | "yarn"
                );
                let result = if node_family {
                    Self::call_mcp_tool(&mcp_client_tx, "mcp-node", "format", serde_json::json!({
                        "path": dispatch.path,
                    }))
                    .await
                } else {
                    Self::call_build_module_tool(
                        &build_manager_tx,
                        &dispatch.build_type,
                        "fmt",
                        serde_json::json!({ "path": dispatch.path }),
                    )
                    .await
                };
                (
                    dispatch.build_type,
                    dispatch.path,
                    dispatch.project_name,
                    result,
                )
            }));
        }

        let result = Self::collect_results(handles, start).await;
        info!(
            "project/format: completed in {:.1}s",
            result["duration_secs"].as_f64().unwrap_or(0.0)
        );
        Ok(result)
    }

    // ── Shared Helpers ────────────────────────────────────────────────────

    async fn get_build_config(&self) -> Result<Vec<Value>, String> {
        let (tx, rx) = oneshot::channel();
        self.project_query_tx
            .send(ProjectQueryMessage::CallTool {
                tool: "project/getBuildConfig".to_string(),
                args: serde_json::json!({}),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Failed to send to ProjectQueryActor: {}", e))?;

        let config = rx
            .await
            .map_err(|e| format!("ProjectQueryActor response error: {}", e))?;
        match config.get("buildSystems") {
            Some(Value::Array(systems)) => Ok(systems.clone()),
            _ => Ok(Vec::new()),
        }
    }

    fn resolve_scope(&self, scope: Option<&str>, systems: &[Value]) -> Vec<BuildDispatch> {
        let scope = match scope {
            Some(s) if !s.is_empty() && s != "all" => s,
            _ => return self.all_dispatches(systems),
        };

        let by_type: Vec<BuildDispatch> = systems
            .iter()
            .filter(|sys| {
                sys.get("buildType")
                    .and_then(|v| v.as_str())
                    .map(|bt| bt.eq_ignore_ascii_case(scope))
                    .unwrap_or(false)
            })
            .map(|sys| self.system_to_dispatch(sys, None))
            .collect();

        if !by_type.is_empty() {
            return by_type;
        }

        let by_path: Vec<BuildDispatch> = systems
            .iter()
            .filter(|sys| {
                sys.get("path")
                    .and_then(|v| v.as_str())
                    .map(|p| p.contains(scope) || scope.contains(p))
                    .unwrap_or(false)
            })
            .map(|sys| self.system_to_dispatch(sys, None))
            .collect();

        if !by_path.is_empty() {
            return by_path;
        }

        let by_name: Vec<BuildDispatch> = systems
            .iter()
            .filter(|sys| {
                sys.get("projectName")
                    .and_then(|v| v.as_str())
                    .map(|pn| pn.eq_ignore_ascii_case(scope))
                    .unwrap_or(false)
            })
            .map(|sys| self.system_to_dispatch(sys, Some(scope)))
            .collect();

        if !by_name.is_empty() {
            return by_name;
        }

        Vec::new()
    }

    fn all_dispatches(&self, systems: &[Value]) -> Vec<BuildDispatch> {
        systems
            .iter()
            .map(|sys| self.system_to_dispatch(sys, None))
            .collect()
    }

    fn system_to_dispatch(&self, system: &Value, _package: Option<&str>) -> BuildDispatch {
        let build_type = system
            .get("buildType")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = system
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let project_name = system
            .get("projectName")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let path = system
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|p| !p.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.resolve_build_path(&name, &build_type));

        BuildDispatch {
            build_type,
            path,
            project_name,
        }
    }

    fn resolve_build_path(&self, name: &str, _build_type: &str) -> String {
        let path = std::path::Path::new(name);
        if path.extension().is_some() {
            path.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string())
        } else {
            name.to_string()
        }
    }

    /// Dispatch a build-system operation to the registered build module via
    /// the BuildManager's generic `CallModuleTool` message.
    async fn call_build_module_tool(
        build_manager_tx: &mpsc::Sender<BuildManagerMessage>,
        build_type: &str,
        tool_name: &str,
        args: Value,
    ) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        build_manager_tx
            .send(BuildManagerMessage::CallModuleTool {
                build_system: build_type.to_string(),
                tool_name: tool_name.to_string(),
                args,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("BuildManager send error: {}", e))?;
        rx.await
            .map_err(|e| format!("BuildManager response error: {}", e))?
    }

    async fn call_mcp_tool(
        mcp_client_tx: &mpsc::Sender<McpClientMessage>,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        mcp_client_tx
            .send(McpClientMessage::CallTool {
                server_name: server_name.to_string(),
                tool_name: tool_name.to_string(),
                arguments: arguments.as_object().cloned(),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("MCP client send error: {}", e))?;

        match rx.await {
            Ok(Ok(result)) => {
                let text = result
                    .content
                    .first()
                    .and_then(|c| {
                        if let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = c {
                            Some(tc.text.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "{}".to_string());
                serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to parse MCP response: {}", e))
            }
            Ok(Err(e)) => Err(format!("MCP tool call error: {}", e)),
            Err(_) => Err("MCP client response error".to_string()),
        }
    }

    async fn collect_results(
        handles: Vec<
            tokio::task::JoinHandle<(String, String, Option<String>, Result<Value, String>)>,
        >,
        start: Instant,
    ) -> Value {
        let mut system_results = Vec::new();
        let mut overall_success = true;

        for handle in handles {
            match handle.await {
                Ok((build_type, path, project_name, Ok(result))) => {
                    let success = result
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if !success {
                        overall_success = false;
                    }
                    system_results.push(serde_json::json!({
                        "type": build_type,
                        "path": path,
                        "projectName": project_name,
                        "success": success,
                        "details": result,
                    }));
                }
                Ok((build_type, path, project_name, Err(e))) => {
                    overall_success = false;
                    system_results.push(serde_json::json!({
                        "type": build_type,
                        "path": path,
                        "projectName": project_name,
                        "success": false,
                        "error": e,
                    }));
                }
                Err(e) => {
                    overall_success = false;
                    system_results.push(serde_json::json!({
                        "success": false,
                        "error": format!("Task panicked: {}", e),
                    }));
                }
            }
        }

        serde_json::json!({
            "success": overall_success,
            "duration_secs": start.elapsed().as_secs_f64(),
            "systems": system_results,
        })
    }
}

// ============================================================================
// Actor trait implementation
// ============================================================================

#[async_trait]
impl Actor for ProjectLintActor {
    type Message = ProjectLintMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            ProjectLintMessage::ListTools { reply_to } => {
                let _ = reply_to.send(Self::tool_definitions());
            }
            ProjectLintMessage::CallTool {
                tool,
                args,
                reply_to,
            } => {
                let result = match tool.as_str() {
                    "project/lint" => self.handle_lint(&args).await,
                    "project/format" => self.handle_format(&args).await,
                    _ => Err(format!("ProjectLintActor: unknown tool '{}'", tool)),
                };
                let _ = reply_to.send(result);
            }
        }
    }
}

// ============================================================================
// BuildDispatch — resolved build target (duplicated from project_build.rs)
// ============================================================================

struct BuildDispatch {
    build_type: String,
    path: String,
    project_name: Option<String>,
}
