// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! ProjectInstallActor — meta-tool for installing dependencies and adding packages.
//!
//! Provides `project/install` and `project/add_dependency` which:
//! 1. Call `project/getBuildConfig` to discover all build systems
//! 2. Resolve the scope parameter to determine which systems to act on
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
// ProjectInstallMessage
// ============================================================================

/// Messages for the ProjectInstall actor.
pub enum ProjectInstallMessage {
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
// ProjectInstallActor
// ============================================================================

/// Actor that provides `project/install` and `project/add_dependency` meta-tools.
pub struct ProjectInstallActor {
    project_query_tx: mpsc::Sender<ProjectQueryMessage>,
    mcp_client_tx: mpsc::Sender<McpClientMessage>,
    /// Sender to the BuildManagerActor — routes add_dependency through the
    /// registered build modules (no hardcoded per-build-system branches).
    build_manager_tx: mpsc::Sender<BuildManagerMessage>,
}

impl ProjectInstallActor {
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
                name: "project/install".to_string(),
                description: "Install dependencies across all build systems. Calls \
                    project/getBuildConfig to discover build systems, then dispatches \
                    the appropriate install tool for each system in parallel \
                    (e.g. npm install for Node projects). \
                    Use scope=\"all\" for everything, scope=\"npm\" for a specific build system, \
                    or scope=\"path/to/project\" for a specific subproject. \
                    Returns aggregated results."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "description": "Install scope: 'all' (default) for everything, \
                                a build system name like 'npm' to filter by type, \
                                or a subpath like 'ts/spire-extension' to install a specific subproject"
                        }
                    }
                }),
            },
            ToolInfo {
                name: "project/add_dependency".to_string(),
                description: "Add a dependency to a project. Calls project/getBuildConfig \
                    to discover build systems, then dispatches the appropriate add_dependency \
                    tool for each matching system. \
                    Use scope to target a specific build system or subproject. \
                    The 'crate' or 'package' parameter specifies the dependency name. \
                    Returns aggregated results."
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "scope": {
                            "type": "string",
                            "description": "Target scope: 'all' (default) for everything, \
                                a build system name like 'Cargo' or 'npm' to filter by type, \
                                or a subpath like 'rust/spire-core' to target a specific subproject"
                        },
                        "crate": {
                            "type": "string",
                            "description": "Crate name (for Cargo projects)"
                        },
                        "package": {
                            "type": "string",
                            "description": "Package name (for npm/pnpm/yarn projects)"
                        },
                        "version": {
                            "type": "string",
                            "description": "Optional version constraint (e.g. \"^1.0.0\" or \"0.5\")"
                        },
                        "features": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional features to enable (for Cargo projects)"
                        }
                    }
                }),
            },
        ]
    }

    // ── Handlers ──────────────────────────────────────────────────────────

    async fn handle_install(&self, args: &Value) -> Result<Value, String> {
        let start = Instant::now();
        let scope = args.get("scope").and_then(|v| v.as_str());
        info!("project/install: starting (scope={:?})", scope);

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
            handles.push(tokio::spawn(async move {
                let result = match dispatch.build_type.as_str() {
                    "npm" | "pnpm" | "yarn" => {
                        let args = serde_json::json!({
                            "path": dispatch.path,
                            "package_manager": dispatch.build_type,
                        });
                        Self::call_mcp_tool(&mcp_client_tx, "mcp-node", "install", args).await
                    }
                    other => Ok(serde_json::json!({
                        "type": other,
                        "path": dispatch.path,
                        "success": false,
                        "skipped": true,
                        "message": format!("Unsupported build system: {}. Skipping.", other),
                    })),
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
            "project/install: completed in {:.1}s",
            result["duration_secs"].as_f64().unwrap_or(0.0)
        );
        Ok(result)
    }

    async fn handle_add_dependency(&self, args: &Value) -> Result<Value, String> {
        let start = Instant::now();
        let scope = args.get("scope").and_then(|v| v.as_str());
        info!("project/add_dependency: starting (scope={:?})", scope);

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
            let args = args.clone();
            handles.push(tokio::spawn(async move {
                let node_family = matches!(
                    dispatch.build_type.as_str(),
                    "npm" | "pnpm" | "yarn"
                );
                let result = if node_family {
                    let mut mcp_args = serde_json::Map::new();
                    mcp_args.insert("path".into(), Value::String(dispatch.path.clone()));
                    mcp_args.insert(
                        "package_manager".into(),
                        Value::String(dispatch.build_type.clone()),
                    );
                    if let Some(pkg) = args.get("package").and_then(|v| v.as_str()) {
                        mcp_args.insert("package".into(), Value::String(pkg.to_string()));
                    }
                    if let Some(version) = args.get("version").and_then(|v| v.as_str()) {
                        mcp_args.insert("version".into(), Value::String(version.to_string()));
                    }
                    Self::call_mcp_tool(
                        &mcp_client_tx,
                        "mcp-node",
                        "add_dependency",
                        Value::Object(mcp_args),
                    )
                    .await
                } else {
                    let mut mod_args = serde_json::Map::new();
                    mod_args.insert("path".into(), Value::String(dispatch.path.clone()));
                    if let Some(crate_name) = args.get("crate").and_then(|v| v.as_str()) {
                        mod_args.insert("crate".into(), Value::String(crate_name.to_string()));
                    }
                    if let Some(version) = args.get("version").and_then(|v| v.as_str()) {
                        mod_args.insert("version".into(), Value::String(version.to_string()));
                    }
                    if let Some(features) = args.get("features").and_then(|v| v.as_array()) {
                        let feats: Vec<String> = features
                            .iter()
                            .filter_map(|f| f.as_str().map(|s| s.to_string()))
                            .collect();
                        if !feats.is_empty() {
                            mod_args.insert("features".into(), serde_json::json!(feats));
                        }
                    }
                    Self::call_build_module_tool(
                        &build_manager_tx,
                        &dispatch.build_type,
                        "add_dependency",
                        Value::Object(mod_args),
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
            "project/add_dependency: completed in {:.1}s",
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
impl Actor for ProjectInstallActor {
    type Message = ProjectInstallMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            ProjectInstallMessage::ListTools { reply_to } => {
                let _ = reply_to.send(Self::tool_definitions());
            }
            ProjectInstallMessage::CallTool {
                tool,
                args,
                reply_to,
            } => {
                let result = match tool.as_str() {
                    "project/install" => self.handle_install(&args).await,
                    "project/add_dependency" => self.handle_add_dependency(&args).await,
                    _ => Err(format!("ProjectInstallActor: unknown tool '{}'", tool)),
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
