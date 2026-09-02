// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! ProjectBuildActor — orchestrates multi-system project builds.
//!
//! This actor provides the `project/build` meta-tool that:
//! 1. Calls `project/getBuildConfig` to discover all build systems in the project
//! 2. Resolves the scope parameter to determine which build systems/subprojects to build
//! 3. Dispatches the appropriate build tool for each system in parallel
//! 4. Aggregates results into a single structured response
//!
//! # Scope Resolution
//!
//! The `scope` parameter supports three modes:
//! - `"all"` (default) — build all detected build systems
//! - `"Cargo"` / `"npm"` / `"pnpm"` — build only systems matching that build type
//! - `"rust/spire-core"` — build only the system whose path contains this subpath
//!
//! Supported build systems:
//!   - Any registered build module (Cargo, Meson, Python, SwiftPM, …) → BuildManager
//!   - npm/pnpm/yarn → mcp-node/build

use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::info;

use crate::subsystems::build::build_manager::BuildManagerMessage;
use spire_core::subsystems::chat::chat::ChatMessage;
use spire_core::subsystems::mcp::mcp_client::McpClientMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::progress::{ProgressMessage, ProgressStatus, ProgressUpdate};
use crate::subsystems::project::project_query::ProjectQueryMessage;
use spire_core::actors::Actor;
use spire_core::actors::ToolInfo;
use spire_core::models::memory_graph::{RelationshipInput, RelationshipType};
use spire_core::transport::socket::TransportMessage;

// ============================================================================
// BuildDispatch — resolved build target
// ============================================================================

/// A single resolved build target after scope filtering.
struct BuildDispatch {
    /// Build system type (e.g. "Cargo", "npm", "pnpm").
    build_type: String,
    /// Project root path for this build system.
    path: String,
    /// Logical project name (e.g. "spire-core", "spire-extension").
    project_name: Option<String>,
    /// Optional package/subproject name for single-package builds.
    /// For Cargo: `-p <package>`. For pnpm: `--filter=<package>`.
    package: Option<String>,
}

// ============================================================================
// ProjectBuildMessage
// ============================================================================

/// Messages for the ProjectBuild actor.
pub enum ProjectBuildMessage {
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
    /// Set the project root (the FFI opens projects dynamically; the root is
    /// re-pointed on every `project/open` / `AnalyzeProject`).
    SetProjectRoot {
        root: PathBuf,
    },
}

// ============================================================================
// ProjectBuildActor
// ============================================================================

/// Actor that orchestrates multi-system project builds.
///
/// This actor sits between the ToolRouterActor and the MCP client + project
/// query actors. It does NOT own a graph connection directly — it queries
/// the ProjectQueryActor for build config, then dispatches to MCP build tools.
pub struct ProjectBuildActor {
    /// Sender to the ProjectQueryActor (for project/getBuildConfig).
    project_query_tx: mpsc::Sender<ProjectQueryMessage>,
    /// Sender to the McpClientActor (for dispatching MCP build calls).
    mcp_client_tx: mpsc::Sender<McpClientMessage>,
    /// Sender to the ProgressActor (for publishing progress updates).
    progress_tx: mpsc::Sender<ProgressMessage>,
    /// Sender to the ChatActor (for posting build notifications to chat).
    chat_tx: mpsc::Sender<ChatMessage>,
    /// Sender to the TransportActor (for pushing real-time chat notifications to the webview).
    transport_tx: mpsc::Sender<TransportMessage>,
    /// Sender to the MemoryGraphActor (for persisting diagnostics as graph nodes).
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    /// Sender to the BuildManagerActor — routes build operations to the
    /// registered build modules (no hardcoded per-build-system branches).
    build_manager_tx: mpsc::Sender<BuildManagerMessage>,
    /// Absolute path to the project root. Used to resolve relative build paths
    /// to absolute paths before dispatching to build tools. `None` until a
    /// project is opened (set via `SetProjectRoot`).
    project_root: Option<PathBuf>,
}

impl ProjectBuildActor {
    pub fn new(
        project_query_tx: mpsc::Sender<ProjectQueryMessage>,
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
        progress_tx: mpsc::Sender<ProgressMessage>,
        chat_tx: mpsc::Sender<ChatMessage>,
        transport_tx: mpsc::Sender<TransportMessage>,
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        build_manager_tx: mpsc::Sender<BuildManagerMessage>,
        project_root: PathBuf,
    ) -> Self {
        Self {
            project_query_tx,
            mcp_client_tx,
            progress_tx,
            chat_tx,
            transport_tx,
            memory_graph_tx,
            build_manager_tx,
            project_root: Some(project_root),
        }
    }

    /// Return the tool definitions for this actor.
    pub fn tool_definitions() -> Vec<ToolInfo> {
        vec![ToolInfo {
            name: "project/build".to_string(),
            description: "Build the project. Calls project/getBuildConfig to discover \
                    all build systems (Cargo, npm, pnpm, yarn, etc.), then dispatches the \
                    appropriate build tool for each system in parallel. \
                    Use scope=\"all\" for everything, scope=\"Cargo\" for a specific build \
                    system, or scope=\"rust/spire-core\" for a specific subproject path. \
                    Returns aggregated results with per-system success/failure."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["debug", "release"],
                        "description": "Build mode (default: debug)"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Build scope: 'all' (default) for everything, \
                            a build system name like 'Cargo' or 'npm' to filter by type, \
                            or a subpath like 'rust/spire-core' to build a specific subproject"
                    },
                    "clean": {
                        "type": "boolean",
                        "description": "Clean before building (default: false)"
                    }
                }
            }),
        }]
    }

    // ── Widget Update Helper ────────────────────────────────────────────────

    /// Build the full builds array from the shared state and emit a widget update
    /// notification to the webview. Used by spawned build tasks to report progress.
    async fn emit_widget_update(
        transport_tx: mpsc::Sender<TransportMessage>,
        widget_id: String,
        states: &[Option<serde_json::Value>],
    ) {
        let builds: Vec<serde_json::Value> =
            states.iter().filter_map(|s| s.as_ref().cloned()).collect();
        let _ = transport_tx
            .send(TransportMessage::SendNotification {
                method: "event/widget/update".to_string(),
                params: serde_json::json!({
                    "widgetId": widget_id,
                    "state": {
                        "builds": builds,
                    }
                }),
            })
            .await;
    }

    // ── Progress & Chat Helpers ────────────────────────────────────────────

    /// Publish a progress update.
    async fn publish_progress(
        &self,
        task_id: &str,
        message: &str,
        percent: f64,
        status: ProgressStatus,
    ) {
        let _ = self
            .progress_tx
            .send(ProgressMessage::Publish {
                update: ProgressUpdate {
                    task_id: task_id.to_string(),
                    message: message.to_string(),
                    percent,
                    status,
                    metadata: None,
                },
            })
            .await;
    }

    /// Append a message to the default chat dialog and push a real-time
    /// notification to the webview via the transport.
    ///
    /// When `widget` is `Some(...)`, the message will be rendered as an
    /// interactive widget (e.g. `build-list`) in the chat dialog.
    async fn chat_notify(&self, content: &str, role: &str, widget: Option<serde_json::Value>) {
        // Persist the message in the ChatActor
        let (tx, _rx) = oneshot::channel();
        let _ = self
            .chat_tx
            .send(ChatMessage::Append {
                chat_id: "default".to_string(),
                content: content.to_string(),
                role: role.to_string(),
                reply_to: tx,
                widget: widget.clone(),
            })
            .await;

        // Build the notification params — include widget if present
        let mut params = serde_json::json!({
            "chatId": "default",
            "content": content,
            "role": role,
        });
        if let Some(ref w) = widget {
            params["widget"] = w.clone();
        }

        // Push a real-time event/chat/message notification to the webview
        let _ = self
            .transport_tx
            .send(TransportMessage::SendNotification {
                method: "event/chat/message".to_string(),
                params,
            })
            .await;
    }

    // ── Orchestration ──────────────────────────────────────────────────────

    /// Handle the `project/build` tool call.
    async fn handle_build(&self, args: &Value) -> Result<Value, String> {
        // The FFI spawns this actor with an empty root and re-points it on every
        // project/open via SetProjectRoot — an empty/unset root is not buildable.
        let root_set = self
            .project_root
            .as_ref()
            .map(|p| !p.as_os_str().is_empty())
            .unwrap_or(false);
        if !root_set {
            return Err("No project root set — open a project before building".to_string());
        }
        let start = Instant::now();
        let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("debug");
        let scope = args.get("scope").and_then(|v| v.as_str());
        let clean = args.get("clean").and_then(|v| v.as_bool()).unwrap_or(false);

        info!(
            "project/build: starting build (mode={}, scope={:?}, clean={})",
            mode, scope, clean
        );

        // Build a unique widget ID so we can update the build-list widget in-place
        let build_widget_id = format!("build-{}", uuid::Uuid::new_v4());

        // Notify: build starting (plain text, no widget yet — we'll emit the widget
        // once we know which build systems we're building)
        self.publish_progress("build", "Starting build...", 0.0, ProgressStatus::Running)
            .await;

        // Step 1: Get build config from ProjectQueryActor
        let build_config = self.get_build_config().await?;

        // Step 2: Parse build systems from the config
        let systems = match build_config.get("buildSystems") {
            Some(Value::Array(systems)) => systems.clone(),
            _ => {
                self.publish_progress(
                    "build",
                    "No build systems found",
                    100.0,
                    ProgressStatus::Completed,
                )
                .await;
                self.chat_notify(
                    "✅ **Build complete** — no build systems found in project configuration.",
                    "assistant",
                    None,
                )
                .await;
                return Ok(serde_json::json!({
                    "success": true,
                    "duration_secs": 0.0,
                    "message": "No build systems found in project configuration",
                    "systems": []
                }));
            }
        };

        if systems.is_empty() {
            self.publish_progress(
                "build",
                "No build systems found",
                100.0,
                ProgressStatus::Completed,
            )
            .await;
            self.chat_notify(
                "✅ **Build complete** — no build systems found in project configuration.",
                "assistant",
                None,
            )
            .await;
            return Ok(serde_json::json!({
                "success": true,
                "duration_secs": 0.0,
                "message": "No build systems found in project configuration",
                "systems": []
            }));
        }

        // Step 3: Resolve scope — filter build systems based on scope parameter
        let dispatches = self.resolve_scope(scope, &systems);

        if dispatches.is_empty() {
            self.publish_progress(
                "build",
                "No matching build systems",
                100.0,
                ProgressStatus::Completed,
            )
            .await;
            self.chat_notify(
                &format!(
                    "⚠️ **Build skipped** — no build systems matched scope '{:?}'.",
                    scope
                ),
                "assistant",
                None,
            )
            .await;
            return Ok(serde_json::json!({
                "success": false,
                "duration_secs": 0.0,
                "message": format!("No build systems matched scope '{:?}'", scope),
                "systems": []
            }));
        }

        // Notify: found build systems — update the build-list widget with initial entries
        let system_names: Vec<String> = dispatches
            .iter()
            .map(|d| d.project_name.as_deref().unwrap_or(&d.path).to_string())
            .collect();
        self.publish_progress(
            "build",
            &format!("Building {} system(s)...", system_names.len()),
            5.0,
            ProgressStatus::Running,
        )
        .await;

        // Build the initial build-list state with all systems as "pending"
        let initial_build_items: Vec<serde_json::Value> = dispatches
            .iter()
            .map(|d| {
                let name = d.project_name.as_deref().unwrap_or(&d.path).to_string();
                serde_json::json!({
                    "name": name,
                    "type": d.build_type,
                    "status": "pending",
                    "duration_ms": null,
                    "log": null,
                })
            })
            .collect();
        let build_widget = serde_json::json!({
            "widgetId": build_widget_id,
            "widgetType": "build-list",
            "state": {
                "title": format!("Building {} system(s)...", system_names.len()),
                "builds": initial_build_items,
            }
        });
        self.chat_notify(
            &format!(
                "🔨 Building **{}** system(s): {}",
                system_names.len(),
                system_names.join(", ")
            ),
            "assistant",
            Some(build_widget),
        )
        .await;

        // Step 4: Dispatch builds in parallel
        //
        // We use a shared `build_states` vector (protected by Arc<Mutex>) so that
        // each spawned task can update its own slot and then emit a full
        // `event/widget/update` notification with the complete builds array.
        // This ensures the build-list widget shows real-time status transitions
        // (pending → running → success/error) for every build system.
        let num_builds = dispatches.len();
        let build_states: Arc<Mutex<Vec<Option<serde_json::Value>>>> =
            Arc::new(Mutex::new(vec![None; num_builds]));
        let mut handles = Vec::new();
        let transport_tx = self.transport_tx.clone();
        let build_widget_id_clone = build_widget_id.clone();

        for (i, dispatch) in dispatches.into_iter().enumerate() {
            let mcp_client_tx = self.mcp_client_tx.clone();
            let build_manager_tx = self.build_manager_tx.clone();
            let memory_graph_tx = self.memory_graph_tx.clone();
            let progress_tx = self.progress_tx.clone();
            let transport_tx = transport_tx.clone();
            let build_widget_id = build_widget_id_clone.clone();
            let build_states = build_states.clone();
            let mode = mode.to_string();
            let clean = clean;
            let project_name = dispatch
                .project_name
                .clone()
                .unwrap_or_else(|| dispatch.path.clone());
            let build_type = dispatch.build_type.clone();

            handles.push(tokio::spawn(async move {
                // Notify: starting individual build — update shared state and emit widget update
                {
                    let mut states = build_states.lock().await;
                    states[i] = Some(serde_json::json!({
                        "name": project_name,
                        "type": build_type,
                        "status": "running",
                        "duration_ms": null,
                        "log": null,
                    }));
                    ProjectBuildActor::emit_widget_update(
                        transport_tx.clone(),
                        build_widget_id.clone(),
                        &states,
                    )
                    .await;
                }

                let _ = progress_tx
                    .send(ProgressMessage::Publish {
                        update: ProgressUpdate {
                            task_id: format!("build-{}", i),
                            message: format!("Building {}...", project_name),
                            percent: 0.0,
                            status: ProgressStatus::Running,
                            metadata: None,
                        },
                    })
                    .await;

                let build_run_id = uuid::Uuid::new_v4().to_string();
                let build_start = Instant::now();
                // Dispatch by capability: ANY build type with a registered
                // module (Cargo, Meson, Python, SwiftPM, …) goes through the
                // BuildManager's generic CallModuleTool. Only Node-family
                // build types keep their legacy MCP-server path. There are no
                // hardcoded per-build-system branches beyond that.
                let node_family = matches!(
                    dispatch.build_type.as_str(),
                    "npm" | "pnpm" | "yarn"
                );
                let result = if node_family {
                    Self::build_node(
                        mcp_client_tx,
                        &dispatch.path,
                        &mode,
                        &dispatch.build_type,
                        dispatch.package.as_deref(),
                        clean,
                    )
                    .await
                } else {
                    let mut args = serde_json::Map::new();
                    args.insert("path".to_string(), Value::String(dispatch.path.clone()));
                    args.insert("mode".to_string(), Value::String(mode.clone()));
                    if let Some(pkg) = dispatch.package.as_deref() {
                        if !pkg.is_empty() {
                            args.insert("package".to_string(), Value::String(pkg.to_string()));
                        }
                    }
                    if clean {
                        let _ = Self::call_build_module_tool(
                            &build_manager_tx,
                            &dispatch.build_type,
                            "clean",
                            serde_json::json!({ "path": dispatch.path }),
                        )
                        .await;
                    }
                    Self::call_build_module_tool(
                        &build_manager_tx,
                        &dispatch.build_type,
                        "build",
                        Value::Object(args),
                    )
                    .await
                };
                let build_duration = build_start.elapsed().as_millis() as u64;

                // Store diagnostics (warnings/errors) from the build result into the graph
                if let Ok(ref build_result) = result {
                    Self::store_diagnostics(
                        &memory_graph_tx,
                        &build_type,
                        build_result,
                        &build_run_id,
                    )
                    .await;
                }

                // Notify: individual build complete — update shared state and emit widget update
                let success = result
                    .as_ref()
                    .ok()
                    .and_then(|r| r.get("success").and_then(|v| v.as_bool()))
                    .unwrap_or(false);
                let status = if success { "success" } else { "error" };
                // Extract log: first try "output" field, then "error" field (for failed builds
                // where the MCP server returns Ok with {"success":false,"error":"..."}),
                // then fall back to the outer Err variant.
                let log = result
                    .as_ref()
                    .ok()
                    .and_then(|r| r.get("output").and_then(|v| v.as_str()))
                    .or_else(|| {
                        result
                            .as_ref()
                            .ok()
                            .and_then(|r| r.get("error").and_then(|v| v.as_str()))
                    })
                    .or_else(|| result.as_ref().err().map(|e| e.as_str()))
                    .map(|s| s.to_string());

                {
                    let mut states = build_states.lock().await;
                    states[i] = Some(serde_json::json!({
                        "name": project_name,
                        "type": build_type,
                        "status": status,
                        "duration_ms": build_duration,
                        "log": log,
                    }));
                    ProjectBuildActor::emit_widget_update(
                        transport_tx.clone(),
                        build_widget_id.clone(),
                        &states,
                    )
                    .await;
                }

                let _ = progress_tx
                    .send(ProgressMessage::Publish {
                        update: ProgressUpdate {
                            task_id: format!("build-{}", i),
                            message: format!(
                                "{}: {}",
                                project_name,
                                if success { "✅ success" } else { "❌ failed" }
                            ),
                            percent: if success { 100.0 } else { 100.0 },
                            status: if success {
                                ProgressStatus::Completed
                            } else {
                                ProgressStatus::Failed
                            },
                            metadata: None,
                        },
                    })
                    .await;

                (
                    dispatch.build_type,
                    dispatch.path,
                    dispatch.project_name,
                    result,
                )
            }));
        }

        // Step 5: Collect results
        let mut system_results = Vec::new();
        let mut overall_success = true;
        let mut success_count = 0;
        let mut fail_count = 0;

        for handle in handles {
            match handle.await {
                Ok((build_type, path, project_name, Ok(result))) => {
                    let success = result
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if success {
                        success_count += 1;
                    } else {
                        fail_count += 1;
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
                    fail_count += 1;
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
                    fail_count += 1;
                    overall_success = false;
                    system_results.push(serde_json::json!({
                        "success": false,
                        "error": format!("Build task panicked: {}", e),
                    }));
                }
            }
        }

        let duration = start.elapsed().as_secs_f64();
        info!(
            "project/build: completed in {:.1}s (success={})",
            duration, overall_success
        );

        // Notify: build complete
        self.publish_progress("build", "Build complete", 100.0, ProgressStatus::Completed)
            .await;

        // Build the updated build-list state with real results
        let updated_builds: Vec<serde_json::Value> = system_results
            .iter()
            .map(|sys| {
                let success = sys
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let status = if success { "success" } else { "error" };
                let name = sys
                    .get("projectName")
                    .and_then(|v| v.as_str())
                    .or_else(|| sys.get("path").and_then(|v| v.as_str()))
                    .unwrap_or("unknown")
                    .to_string();
                let build_type = sys.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let duration_ms = sys
                    .get("details")
                    .and_then(|d| d.get("duration_ms"))
                    .and_then(|v| v.as_u64())
                    .or_else(|| {
                        sys.get("details")
                            .and_then(|d| d.get("duration_secs"))
                            .and_then(|v| v.as_f64())
                            .map(|s| (s * 1000.0) as u64)
                    });
                let log = sys
                    .get("details")
                    .and_then(|d| d.get("output"))
                    .and_then(|v| v.as_str())
                    .or_else(|| sys.get("error").and_then(|v| v.as_str()))
                    .map(|s| s.to_string());
                serde_json::json!({
                    "name": name,
                    "type": build_type,
                    "status": status,
                    "duration_ms": duration_ms,
                    "log": log,
                })
            })
            .collect();

        let summary_title = if overall_success {
            format!(
                "✅ Build complete — {} system(s) in {:.1}s",
                success_count, duration
            )
        } else {
            format!(
                "⚠️ Build finished — {} succeeded, {} failed in {:.1}s",
                success_count, fail_count, duration
            )
        };

        // Send widget update notification to update the build-list in-place
        let _ = self
            .transport_tx
            .send(TransportMessage::SendNotification {
                method: "event/widget/update".to_string(),
                params: serde_json::json!({
                    "widgetId": build_widget_id,
                    "state": {
                        "title": summary_title,
                        "builds": updated_builds,
                    }
                }),
            })
            .await;

        // Also update the widget state in the ChatActor so it persists across reloads
        let (update_tx, _update_rx) = oneshot::channel();
        let _ = self
            .chat_tx
            .send(ChatMessage::UpdateWidget {
                widget_id: build_widget_id.clone(),
                state: serde_json::json!({
                    "title": summary_title,
                    "builds": updated_builds,
                }),
                reply_to: update_tx,
            })
            .await;

        Ok(serde_json::json!({
            "success": overall_success,
            "duration_secs": duration,
            "systems": system_results,
        }))
    }

    // ── Scope Resolution ───────────────────────────────────────────────────

    /// Resolve the scope parameter into a list of build dispatches.
    ///
    /// # Resolution Rules
    ///
    /// | scope value | Behavior |
    /// |---|---|
    /// | `None` or `"all"` | Return all build systems unfiltered |
    /// | Build system name (e.g. `"Cargo"`, `"npm"`) | Match against `buildType` field |
    /// | Subpath (e.g. `"rust/spire-core"`) | Match against `path` field (prefix match) |
    /// | Project name (e.g. `"spire-core"`) | Match against `projectName` field |
    fn resolve_scope(&self, scope: Option<&str>, systems: &[Value]) -> Vec<BuildDispatch> {
        let scope = match scope {
            Some(s) if !s.is_empty() && s != "all" => s,
            _ => return self.all_dispatches(systems),
        };

        // Try matching as a build system type first
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

        // Try matching as a subpath (prefix match on the path field)
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

        // Try matching as a project name
        let by_name: Vec<BuildDispatch> = systems
            .iter()
            .filter(|sys| {
                sys.get("projectName")
                    .and_then(|v| v.as_str())
                    .map(|pn| pn.eq_ignore_ascii_case(scope))
                    .unwrap_or(false)
            })
            .map(|sys| {
                // When matching by project name, set the package scope
                self.system_to_dispatch(sys, Some(scope))
            })
            .collect();

        if !by_name.is_empty() {
            return by_name;
        }

        // No match — return empty
        Vec::new()
    }

    /// Convert all build systems to dispatches (no filtering).
    fn all_dispatches(&self, systems: &[Value]) -> Vec<BuildDispatch> {
        systems
            .iter()
            .map(|sys| self.system_to_dispatch(sys, None))
            .collect()
    }

    /// Convert a single build system config entry to a BuildDispatch.
    ///
    /// The `path` from the build config is typically relative (e.g. "rust/spire-core").
    /// This method resolves it to an absolute path using `self.project_root`.
    fn system_to_dispatch(&self, system: &Value, package: Option<&str>) -> BuildDispatch {
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

        let raw_path = system
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|p| !p.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.resolve_build_path(&name, &build_type));

        // Resolve relative paths to absolute using the project root
        let path = self.resolve_absolute_path(&raw_path);

        BuildDispatch {
            build_type,
            path,
            project_name,
            package: package.map(|s| s.to_string()),
        }
    }

    // ── Build Config Query ─────────────────────────────────────────────────

    /// Call `project/getBuildConfig` on the ProjectQueryActor.
    async fn get_build_config(&self) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.project_query_tx
            .send(ProjectQueryMessage::CallTool {
                tool: "project/getBuildConfig".to_string(),
                args: serde_json::json!({}),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Failed to send to ProjectQueryActor: {}", e))?;

        rx.await
            .map_err(|e| format!("ProjectQueryActor response error: {}", e))
    }

    // ── Build Dispatchers ──────────────────────────────────────────────────

    /// Dispatch a build-system operation to the registered build module via
    /// the BuildManager's generic `CallModuleTool` message. Modules resolve
    /// by capability (`build_system`/`language`), so no hardcoded "Cargo"
    /// branches are needed — Meson, Python, SwiftPM, etc. all dispatch here.
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

    /// Dispatch a Node.js build via mcp-node/build.
    ///
    /// When `package` is set, passes it as `--filter=<package>` for pnpm
    /// or `--workspace=<package>` for npm/yarn workspaces.
    async fn build_node(
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
        path: &str,
        _mode: &str,
        package_manager: &str,
        package: Option<&str>,
        clean: bool,
    ) -> Result<Value, String> {
        let mut args = serde_json::Map::new();
        args.insert("path".to_string(), Value::String(path.to_string()));
        args.insert(
            "package_manager".to_string(),
            Value::String(package_manager.to_string()),
        );

        if let Some(pkg) = package {
            args.insert("package".to_string(), Value::String(pkg.to_string()));
        }

        if clean {
            let _ = Self::call_mcp_tool(
                &mcp_client_tx,
                "mcp-node",
                "install",
                serde_json::json!({
                    "path": path,
                    "package_manager": package_manager,
                }),
            )
            .await;
        }

        Self::call_mcp_tool(&mcp_client_tx, "mcp-node", "build", Value::Object(args)).await
    }

    // ── MCP Tool Call Helper ───────────────────────────────────────────────

    /// Call a tool on a specific MCP server via the McpClientActor.
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
                let content = result.content;
                let text = content
                    .first()
                    .and_then(|c| {
                        if let rust_mcp_sdk::schema::ContentBlock::TextContent(text_content) = c {
                            Some(text_content.text.clone())
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
    /// Store diagnostics (warnings and errors) from a build result into the graph.
    ///
    /// Parses the `warnings` and `errors` arrays from the MCP build response and
    /// stores each as a `Diagnostic`-type graph node linked to its file node
    /// via a `HasDiagnostic` relationship.
    async fn store_diagnostics(
        memory_graph_tx: &mpsc::Sender<MemoryGraphMessage>,
        build_type: &str,
        build_result: &Value,
        build_run_id: &str,
    ) {
        Self::store_diagnostics_inner(
            memory_graph_tx,
            build_type,
            build_result,
            build_run_id,
            "warnings",
            "warning",
        )
        .await;
        Self::store_diagnostics_inner(
            memory_graph_tx,
            build_type,
            build_result,
            build_run_id,
            "errors",
            "error",
        )
        .await;
    }

    /// Process a single diagnostic array (warnings or errors) and store each as a graph node.
    async fn store_diagnostics_inner(
        memory_graph_tx: &mpsc::Sender<MemoryGraphMessage>,
        build_type: &str,
        build_result: &Value,
        build_run_id: &str,
        key: &str,
        severity: &str,
    ) {
        let Some(items) = build_result.get(key).and_then(|v| v.as_array()) else {
            return;
        };

        for item in items {
            let message = item
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let file = item
                .get("file")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let line = item.get("line").and_then(|v| v.as_u64()).map(|n| n as u32);
            let column = item
                .get("column")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);

            // Store diagnostic node
            let diag_name = format!(
                "{}-{}-{}",
                severity,
                file.as_deref().unwrap_or("unknown"),
                build_run_id
            );
            let diagnostic_node = {
                let now = chrono::Utc::now();
                spire_core::models::memory_graph::AttrNode {
                    id: uuid::Uuid::new_v4().to_string(),
                    node_type: "Diagnostic".to_string(),
                    subtype: Some(severity.to_string()),
                    name: diag_name,
                    description: Some(message.clone()),
                    properties: [("message", &message), ("severity", &severity.to_string())]
                        .iter()
                        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                        .chain(file.as_ref().map(|f| {
                            ("file".to_string(), serde_json::Value::String(f.clone()))
                        }))
                        .chain(
                            line.map(|n| {
                                ("line".to_string(), serde_json::Value::Number(n.into()))
                            }),
                        )
                        .chain(column.map(|n| {
                            ("column".to_string(), serde_json::Value::Number(n.into()))
                        }))
                        .chain(std::iter::once((
                            "build_type".to_string(),
                            serde_json::Value::String(build_type.to_string()),
                        )))
                        .chain(std::iter::once((
                            "build_run_id".to_string(),
                            serde_json::Value::String(build_run_id.to_string()),
                        )))
                        .collect(),
                    embedding_id: None,
                    created_at: now,
                    updated_at: now,
                    version: 1,
                }
            };

            let (store_tx, store_rx) = oneshot::channel();
            let _ = memory_graph_tx
                .send(MemoryGraphMessage::StoreAttrNode {
                    node: diagnostic_node,
                    reply_to: store_tx,
                })
                .await;

            if let Ok(Ok(stored_node)) = store_rx.await {
                // If the diagnostic has a file reference, create a HasDiagnostic relationship
                if let Some(ref file_path) = file {
                    // Query for the file node by name
                    let (query_tx, query_rx) = oneshot::channel();
                    let _ = memory_graph_tx
                        .send(MemoryGraphMessage::QueryAttrNodes {
                            node_type: Some("Unknown".to_string()),
                            subtype: Some("File".to_string()),
                            name: Some(file_path.clone()),
                            limit: Some(1),
                            reply_to: query_tx,
                        })
                        .await;

                    if let Ok(Ok(nodes)) = query_rx.await {
                        if let Some(file_node) = nodes.first() {
                            let _ = Self::create_diagnostic_relationship(
                                memory_graph_tx,
                                file_node.id(),
                                stored_node.id(),
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }

    /// Create a `HasDiagnostic` relationship from a file node to a diagnostic node.
    async fn create_diagnostic_relationship(
        memory_graph_tx: &mpsc::Sender<MemoryGraphMessage>,
        file_id: &str,
        diagnostic_id: &str,
    ) -> Result<(), String> {
        let (rel_tx, rel_rx) = oneshot::channel();
        memory_graph_tx
            .send(MemoryGraphMessage::CreateRelationship {
                rel: RelationshipInput {
                    edge_type: RelationshipType::HasDiagnostic,
                    from_id: file_id.to_string(),
                    to_id: diagnostic_id.to_string(),
                    properties: None,
                    weight: None,
                },
                reply_to: rel_tx,
            })
            .await
            .map_err(|e| format!("Failed to send create relationship: {}", e))?;

        let _ = rel_rx
            .await
            .map_err(|e| format!("Create relationship response error: {}", e))?;
        Ok(())
    }

    // ── Path Resolution ────────────────────────────────────────────────────

    /// Resolve the project root path from a build system name.
    ///
    /// The build system node's "name" field typically contains the path to the
    /// build file (e.g., "rust/Cargo.toml"). We strip the filename to get the
    /// directory, or use the project root if it's a top-level build file.
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

    /// Resolve a potentially relative path to an absolute path.
    ///
    /// If the path is already absolute, return it as-is.
    /// If it's relative, join it with `self.project_root`.
    fn resolve_absolute_path(&self, path: &str) -> String {
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            path.to_string()
        } else {
            match self.project_root.as_ref() {
                Some(root) => root.join(p).to_string_lossy().to_string(),
                // No project open yet — leave relative; `handle_build` guards.
                None => path.to_string(),
            }
        }
    }
}

// ============================================================================
// Actor trait implementation
// ============================================================================

#[async_trait]
impl Actor for ProjectBuildActor {
    type Message = ProjectBuildMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            ProjectBuildMessage::ListTools { reply_to } => {
                let _ = reply_to.send(Self::tool_definitions());
            }

            ProjectBuildMessage::CallTool {
                tool,
                args,
                reply_to,
            } => {
                let result = match tool.as_str() {
                    "project/build" => self.handle_build(&args).await,
                    _ => Err(format!("ProjectBuildActor: unknown tool '{}'", tool)),
                };
                let _ = reply_to.send(result);
            }

            ProjectBuildMessage::SetProjectRoot { root } => {
                self.project_root = Some(root);
            }
        }
    }
}
