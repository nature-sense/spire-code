// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! BuildOrchestrator actor — orchestrates the build-fix loop lifecycle.
//!
//! This is the central state machine for the build-fix cycle. It:
//! 1. Creates a build session node in the graph
//! 2. Dispatches the actual build via ToolRouter → ProjectBuildActor
//! 3. On success: transitions state, no fix needed
//! 4. On failure: stores errors, runs ErrorAnalyzer, returns FixPlan
//! 5. On fix apply: executes fix steps, auto-rebuilds (scoped), loops
//! 6. Guards against infinite loops with max_iterations
//!
//! All graph access goes through `MemoryGraphMessage` — no direct `GraphDb`
//! reference. The MemoryGraphActor internally translates these into GQL queries.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::subsystems::planning::error_analyzer::ErrorAnalyzerMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::subsystems::tools::tool_orchestrator::ToolOrchestratorMessage;
use spire_core::actors::tool_providers::ToolRouterMessage;
use spire_core::actors::Actor;
use spire_core::models::memory_graph::{
    BuildContext, BuildError, BuildResult, BuildStartResult, FixPlan,
    NodeUpdate, StreamOp, StreamOpResult, SystemBuildResult, TransactionRequest,
};

/// Build an envelope `AttrNode` for a node stored by the build orchestrator
/// (build sessions and state transitions).
fn bo_attr_unknown(
    node_type: &str,
    subtype: Option<String>,
    name: String,
    description: Option<String>,
    properties: HashMap<String, serde_json::Value>,
) -> spire_core::models::memory_graph::AttrNode {
    let now = chrono::Utc::now();
    spire_core::models::memory_graph::AttrNode {
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

/// Messages for the BuildOrchestrator actor.
#[derive(Debug)]
pub enum BuildOrchestratorMessage {
    /// Start a new build. Internally dispatches the actual build via ToolRouter,
    /// transitions state, and returns the full result (including FixPlan if failed).
    StartBuild {
        parameters: BuildContext,
        reply_to: tokio::sync::oneshot::Sender<Result<BuildStartResult>>,
    },
    /// Apply a specific fix strategy, then automatically rebuild.
    ApplyFix {
        strategy_name: String,
        /// Optional: target a specific build system (e.g. "Cargo") for scoped rebuild
        target_system: Option<String>,
        reply_to: tokio::sync::oneshot::Sender<Result<BuildStartResult>>,
    },
    /// Rollback to a previous build step.
    RollbackBuild {
        step: String,
        reply_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
}

/// The BuildOrchestrator actor.
pub struct BuildOrchestrator {
    /// Sender to the MemoryGraphActor for all graph queries.
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    /// Sender to the ErrorAnalyzer actor.
    error_analyzer_tx: mpsc::Sender<ErrorAnalyzerMessage>,
    /// Sender to the ToolOrchestrator actor.
    tool_orchestrator_tx: mpsc::Sender<ToolOrchestratorMessage>,
    /// Sender to the ToolRouter actor (for dispatching builds).
    tool_router_tx: mpsc::Sender<ToolRouterMessage>,
    /// Maximum fix iterations before surrendering.
    max_iterations: u32,
    /// Current iteration count for the active build session.
    iteration_count: u32,
    /// Current build context (persisted across fix→rebuild cycles).
    current_context: Option<BuildContext>,
    /// Current build run ID (persisted across fix→rebuild cycles).
    current_build_run_id: Option<String>,
}

impl BuildOrchestrator {
    pub fn new(
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        error_analyzer_tx: mpsc::Sender<ErrorAnalyzerMessage>,
        tool_orchestrator_tx: mpsc::Sender<ToolOrchestratorMessage>,
        tool_router_tx: mpsc::Sender<ToolRouterMessage>,
    ) -> Self {
        Self {
            memory_graph_tx,
            error_analyzer_tx,
            tool_orchestrator_tx,
            tool_router_tx,
            max_iterations: 5,
            iteration_count: 0,
            current_context: None,
            current_build_run_id: None,
        }
    }

    // ── Transaction Stream Helpers ──────────────────────────────────────

    /// Open a transaction stream via the MemoryGraphActor.
    async fn open_txn_stream(&self) -> Result<mpsc::Sender<TransactionRequest>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::OpenTransactionStream { reply_to: tx })
            .await
            .map_err(|e| anyhow::anyhow!("MemoryGraph channel closed: {}", e))?;
        rx.await
            .map_err(|e| anyhow::anyhow!("OpenTransactionStream response error: {}", e))
    }

    /// Send a single operation through a transaction stream and await its result.
    async fn send_stream_op(
        stream_tx: &mpsc::Sender<TransactionRequest>,
        op: StreamOp,
    ) -> Result<StreamOpResult> {
        let (reply_to, rx) = tokio::sync::oneshot::channel();
        stream_tx
            .send(TransactionRequest {
                operation: op,
                reply_to,
            })
            .await
            .map_err(|e| anyhow::anyhow!("Transaction stream closed: {}", e))?;
        rx.await
            .map_err(|e| anyhow::anyhow!("Transaction stream response error: {}", e))?
            .map_err(|e| anyhow::anyhow!("Transaction op failed: {}", e))
    }

    /// Commit a transaction stream.
    async fn commit_stream(stream_tx: mpsc::Sender<TransactionRequest>) -> Result<()> {
        let result = Self::send_stream_op(&stream_tx, StreamOp::Commit).await?;
        match result {
            StreamOpResult::RawGql(_) => Ok(()),
            _ => Err(anyhow::anyhow!(
                "Expected RawGql on commit, got {:?}",
                result
            )),
        }
    }

    // ── State Machine Helpers ──────────────────────────────────────────

    /// Transition a state node to active/inactive.
    async fn transition_state(&self, state_name: &str, active: bool) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("BuildState".to_string()),
                subtype: None,
                name: Some(state_name.to_string()),
                limit: Some(1),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        if let Some(node) = nodes.into_iter().next() {
            let stream_tx = self.open_txn_stream().await?;
            Self::send_stream_op(
                &stream_tx,
                StreamOp::UpdateNode {
                    id: node.id().to_string(),
                    updates: NodeUpdate {
                        node_type: None,
                        subtype: None,
                        name: None,
                        description: None,
                        properties: Some({
                            let mut map = HashMap::new();
                            map.insert("active".to_string(), serde_json::Value::Bool(active));
                            map
                        }),
                        embedding_id: None,
                    },
                },
            )
            .await?;
            Self::commit_stream(stream_tx).await?;
        }
        Ok(())
    }

    // ── Main Build Lifecycle ───────────────────────────────────────────

    /// Start a new build session, dispatch the actual build, and handle the result.
    async fn start_build(&mut self, parameters: BuildContext) -> Result<BuildStartResult> {
        info!("BuildOrchestrator: starting build");

        // Generate a build run ID for this cycle
        let build_run_id = format!(
            "build_run_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let build_id = format!(
            "build_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );

        // Store the build session node in the graph
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::StoreAttrNode {
                node: bo_attr_unknown(
                    "Standard",
                    Some("build_session".to_string()),
                    build_id.clone(),
                    Some(format!("Build session for {}", parameters.project_root)),
                    {
                        let mut map = HashMap::new();
                        map.insert(
                            "build_system".to_string(),
                            serde_json::Value::String(parameters.build_system.clone()),
                        );
                        map.insert(
                            "project_root".to_string(),
                            serde_json::Value::String(parameters.project_root.clone()),
                        );
                        map.insert(
                            "status".to_string(),
                            serde_json::Value::String("building".to_string()),
                        );
                        map.insert(
                            "build_run_id".to_string(),
                            serde_json::Value::String(build_run_id.clone()),
                        );
                        map.insert(
                            "iteration_count".to_string(),
                            serde_json::Value::Number(serde_json::Number::from(
                                self.iteration_count,
                            )),
                        );
                        map
                    },
                ),
                reply_to: tx,
            })
            .await?;

        let _ = rx.await??;

        // Store context for potential fix cycles
        self.current_context = Some(parameters.clone());
        self.current_build_run_id = Some(build_run_id.clone());

        // Reset iteration count if starting fresh (not a fix→rebuild cycle)
        if self.iteration_count == 0 {
            self.iteration_count = 0;
        }

        // ── Dispatch the actual build via ToolRouter ──
        // This calls ToolRouter → project/build → ProjectBuildActor → MCP build tools
        let build_result = self.dispatch_build(&parameters).await?;

        if build_result.success {
            // ── Build succeeded ──
            info!("BuildOrchestrator: build completed successfully");

            // Update session node
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.memory_graph_tx
                .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Standard".to_string()),
                subtype: Some("build_session".to_string()),
                name: Some(build_id.clone()),
                limit: Some(1),
                reply_to: tx,
            })
                .await?;

            if let Some(session) = rx.await??.into_iter().next() {
                let stream_tx = self.open_txn_stream().await?;
                Self::send_stream_op(
                    &stream_tx,
                    StreamOp::UpdateNode {
                        id: session.id().to_string(),
                        updates: NodeUpdate {
                            node_type: None,
                            subtype: None,
                            name: None,
                            description: None,
                            properties: Some({
                                let mut map = HashMap::new();
                                map.insert(
                                    "status".to_string(),
                                    serde_json::Value::String("completed".to_string()),
                                );
                                map
                            }),
                            embedding_id: None,
                        },
                    },
                )
                .await?;
                Self::commit_stream(stream_tx).await?;
            }

            // Transition state machine
            self.transition_state("project_synced", true).await.ok();
            self.transition_state("build_completed", true).await.ok();
            self.transition_state("build_failed", false).await.ok();

            // Reset iteration count on successful build
            self.iteration_count = 0;

            Ok(BuildStartResult {
                build_id,
                success: true,
                build_result: build_result.clone(),
                fix_plan: None,
                iteration_count: 0,
                max_iterations: self.max_iterations,
            })
        } else {
            // ── Build failed ──
            info!("BuildOrchestrator: build failed, analyzing errors");

            // Update session node to failed
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.memory_graph_tx
                .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Standard".to_string()),
                subtype: Some("build_session".to_string()),
                name: Some(build_id.clone()),
                limit: Some(1),
                reply_to: tx,
            })
                .await?;

            if let Some(session) = rx.await??.into_iter().next() {
                let stream_tx = self.open_txn_stream().await?;
                Self::send_stream_op(
                    &stream_tx,
                    StreamOp::UpdateNode {
                        id: session.id().to_string(),
                        updates: NodeUpdate {
                            node_type: None,
                            subtype: None,
                            name: None,
                            description: None,
                            properties: Some({
                                let mut map = HashMap::new();
                                map.insert(
                                    "status".to_string(),
                                    serde_json::Value::String("failed".to_string()),
                                );
                                map
                            }),
                            embedding_id: None,
                        },
                    },
                )
                .await?;
                Self::commit_stream(stream_tx).await?;
            }

            // Transition state machine
            self.transition_state("build_completed", false).await.ok();
            self.transition_state("build_failed", true).await.ok();

            // Run ErrorAnalyzer on the failed system results
            let fix_plan = self.analyze_errors(&build_result).await?;

            Ok(BuildStartResult {
                build_id,
                success: false,
                build_result: build_result.clone(),
                fix_plan: Some(fix_plan),
                iteration_count: self.iteration_count,
                max_iterations: self.max_iterations,
            })
        }
    }

    /// Dispatch the actual build via ToolRouter → ProjectBuildActor.
    async fn dispatch_build(&self, parameters: &BuildContext) -> Result<BuildResult> {
        let mut args = serde_json::Map::new();

        // Build parameters from context
        if let Some(ref target) = parameters.target {
            args.insert(
                "scope".to_string(),
                serde_json::Value::String(target.clone()),
            );
        }

        // Add mode from environment if present
        if let Some(mode) = parameters.environment.get("mode") {
            args.insert("mode".to_string(), serde_json::Value::String(mode.clone()));
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tool_router_tx
            .send(ToolRouterMessage::CallTool {
                tool_name: "project/build".to_string(),
                args: serde_json::Value::Object(args),
                reply_to: tx,
            })
            .await
            .map_err(|e| anyhow::anyhow!("ToolRouter channel closed: {}", e))?;

        let result = rx
            .await
            .map_err(|e| anyhow::anyhow!("ToolRouter response error: {}", e))?
            .map_err(|e| anyhow::anyhow!("Build dispatch failed: {}", e))?;

        // Parse the JSON result from ProjectBuildActor into our BuildResult type
        let success = result
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let duration_secs = result
            .get("duration_secs")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        // Extract per-system results
        let systems = result
            .get("systems")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut system_results = Vec::new();

        for sys in &systems {
            let build_type = sys
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let path = sys
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let project_name = sys
                .get("projectName")
                .and_then(|v| v.as_str())
                .unwrap_or(&build_type)
                .to_string();
            let sys_success = sys
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let exit_code = sys
                .get("details")
                .and_then(|d| d.get("exit_code"))
                .and_then(|v| v.as_i64())
                .map(|c| c as i32);

            // Extract errors and warnings from the build result
            let details = sys.get("details");
            let errors = Self::parse_build_diagnostics(details, "errors", &build_type);
            let warnings = Self::parse_build_diagnostics(details, "warnings", &build_type);

            system_results.push(SystemBuildResult {
                build_type,
                path,
                project_name,
                success: sys_success,
                errors,
                warnings,
                exit_code,
                duration_ms: details
                    .and_then(|d| d.get("duration_ms"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            });
        }

        Ok(BuildResult {
            success,
            system_results,
            build_run_id: self.current_build_run_id.clone().unwrap_or_default(),
            duration_secs,
        })
    }

    /// Parse error/warning diagnostics from a build system's result details.
    fn parse_build_diagnostics(
        details: Option<&serde_json::Value>,
        key: &str,
        build_type: &str,
    ) -> Vec<BuildError> {
        let Some(details) = details else {
            return vec![];
        };
        let Some(items) = details.get(key).and_then(|v| v.as_array()) else {
            return vec![];
        };

        items
            .iter()
            .map(|item| BuildError {
                error_text: item
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                error_type: item
                    .get("error_type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                file: item
                    .get("file")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                line: item.get("line").and_then(|v| v.as_u64()).map(|n| n as u32),
                column: item
                    .get("column")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32),
                exit_code: None,
                build_type: Some(build_type.to_string()),
                diagnostic_node_id: item
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                file_node_id: None,
            })
            .collect()
    }

    /// Run ErrorAnalyzer on the failed system results.
    async fn analyze_errors(&self, build_result: &BuildResult) -> Result<FixPlan> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.error_analyzer_tx
            .send(ErrorAnalyzerMessage::AnalyzeErrors {
                system_results: build_result.system_results.clone(),
                build_run_id: build_result.build_run_id.clone(),
                reply_to: tx,
            })
            .await?;

        let fix_plan = rx.await??;
        Ok(fix_plan)
    }

    /// Apply a specific fix strategy, then automatically rebuild.
    async fn apply_fix(
        &mut self,
        strategy_name: &str,
        target_system: Option<&str>,
    ) -> Result<BuildStartResult> {
        info!(
            "BuildOrchestrator: applying fix strategy: {}",
            strategy_name
        );

        // Check iteration limit
        self.iteration_count += 1;
        if self.iteration_count > self.max_iterations {
            return Err(anyhow::anyhow!(
                "Maximum fix iterations ({}) reached. Manual intervention required.",
                self.max_iterations
            ));
        }

        // Step 1: Look up the fix strategy node in the graph
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("FixStrategy".to_string()),
                subtype: None,
                name: Some(strategy_name.to_string()),
                limit: Some(1),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        let execution_steps: Vec<String> = if let Some(node) = nodes.into_iter().next() {
            node.get("steps")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            warn!(
                "BuildOrchestrator: fix strategy '{}' not found in graph",
                strategy_name
            );
            return Err(anyhow::anyhow!(
                "Fix strategy '{}' not found",
                strategy_name
            ));
        };

        if execution_steps.is_empty() {
            warn!(
                "BuildOrchestrator: fix strategy '{}' has no execution steps",
                strategy_name
            );
        }

        // Step 2: Execute fix steps via ToolOrchestrator
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tool_orchestrator_tx
            .send(ToolOrchestratorMessage::ExecuteToolChain {
                tools: execution_steps,
                parameters: HashMap::new(),
                reply_to: tx,
            })
            .await?;

        let _ = rx.await??;

        // Step 3: Transition state to fix_applied
        self.transition_state("fix_applied", true).await.ok();

        // Step 4: Auto-rebuild (scoped to the target system if specified)
        info!("BuildOrchestrator: fix applied, auto-rebuilding");

        let mut rebuild_context = self.current_context.clone().unwrap_or(BuildContext {
            project_root: ".".to_string(),
            build_system: "unknown".to_string(),
            target: None,
            environment: HashMap::new(),
        });

        // If a specific target system was fixed, scope the rebuild
        if let Some(system) = target_system {
            rebuild_context.target = Some(system.to_string());
        }

        // Recursive call: this will re-run start_build which uses the current
        // iteration_count to track how many fix cycles we've done
        self.start_build(rebuild_context).await
    }

    /// Rollback to a previous build step.
    async fn rollback_build(&self, step: &str) -> Result<()> {
        info!("BuildOrchestrator: rolling back to step: {}", step);

        // Execute rollback via ToolOrchestrator
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tool_orchestrator_tx
            .send(ToolOrchestratorMessage::ExecuteTool {
                tool_name: format!("rollback_{}", step),
                parameters: HashMap::new(),
                reply_to: tx,
            })
            .await?;

        let _ = rx.await??;
        Ok(())
    }
}

#[async_trait]
impl Actor for BuildOrchestrator {
    type Message = BuildOrchestratorMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildOrchestratorMessage::StartBuild {
                parameters,
                reply_to,
            } => {
                let result = self.start_build(parameters).await;
                let _ = reply_to.send(result);
            }
            BuildOrchestratorMessage::ApplyFix {
                strategy_name,
                target_system,
                reply_to,
            } => {
                let result = self
                    .apply_fix(&strategy_name, target_system.as_deref())
                    .await;
                let _ = reply_to.send(result);
            }
            BuildOrchestratorMessage::RollbackBuild { step, reply_to } => {
                let result = self.rollback_build(&step).await;
                let _ = reply_to.send(result);
            }
        }
    }
}
