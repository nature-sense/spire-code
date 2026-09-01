// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! PlanOrchestratorActor — creates, stores, and executes multi-step execution plans.
//!
//! Plan mode adds a deliberate review-before-execute workflow:
//!   1. User gives a goal → PlanOrchestrator generates a plan (via LLM + graph context)
//!   2. Plan is stored in the graph, presented to user as a plan-list widget
//!   3. User approves/rejects → on approval, steps execute sequentially
//!   4. Each step is dispatched via ToolOrchestrator (reuses StepDefinitions)
//!   5. Failures pause the plan → user can retry or skip
//!
//! All plan state is stored in the graph as Plan and PlanStep nodes.
//!
//! # Modification Scopes
//!
//! Plans can be scoped to either the whole project (Level 1) or a single
//! subproject (Level 2). The scope constrains what the LLM is allowed to
//! modify and what verification steps must be included.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use spire_core::subsystems::chat::chat::ChatMessage;
use spire_core::subsystems::llm::llm::LlmMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::subsystems::tools::tool_orchestrator::ToolOrchestratorMessage;
use spire_core::actors::Actor;
use spire_core::models::memory_graph::PlanStatus;
use spire_core::models::memory_graph::PlanStatusResult;
use spire_core::models::memory_graph::PlanStepData;
use spire_core::models::memory_graph::PlanStepEntry;
use spire_core::models::memory_graph::{AttrNode, NodeUpdate};
use spire_core::transport::socket::TransportMessage;

/// Modification scope — determines what the LLM is allowed to touch.
///
/// - [`ModificationScope::Project`]: project-level (Level 1). The LLM may
///   create/delete subprojects, write build configs, or modify any file.
/// - [`ModificationScope::Subproject`]: subproject-level (Level 2). The LLM
///   may only modify files within the given scope directory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ModificationScope {
    /// Project-level: entire project is in scope.
    Project,
    /// Subproject-level: only files under `path` are in scope.
    Subproject {
        /// Absolute or project-relative path of the subproject directory.
        path: String,
    },
}

/// Messages for the PlanOrchestrator actor.
#[derive(Debug)]
pub enum PlanOrchestratorMessage {
    CreatePlan {
        goal: String,
        intent_name: Option<String>,
        parameters: HashMap<String, String>,
        /// Optional modification scope constraining the LLM's allowed changes.
        scope: Option<ModificationScope>,
        /// Absolute path of the opened project root (from project/open).
        workspace_root: Option<String>,
        reply_to: tokio::sync::oneshot::Sender<Result<PlanStatusResult>>,
    },
    ApprovePlan {
        plan_id: String,
        reply_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
    RejectPlan {
        plan_id: String,
        reason: Option<String>,
        reply_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
    GetPlanStatus {
        plan_id: String,
        reply_to: tokio::sync::oneshot::Sender<Result<PlanStatusResult>>,
    },
    PausePlan {
        plan_id: String,
        reply_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
    ResumePlan {
        plan_id: String,
        reply_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
    RetryStep {
        plan_id: String,
        step_order: u32,
        reply_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
    SkipStep {
        plan_id: String,
        step_order: u32,
        reply_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
}

/// The PlanOrchestrator actor.
pub struct PlanOrchestrator {
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    llm_tx: mpsc::Sender<LlmMessage>,
    tool_orchestrator_tx: mpsc::Sender<ToolOrchestratorMessage>,
    chat_tx: mpsc::Sender<ChatMessage>,
    transport_tx: mpsc::Sender<TransportMessage>,
    max_retries: u32,
    /// In-memory plan registry (keyed by plan_id) for create → approve → execute.
    /// Mutex interior mutability: actor handlers take `&self`.
    plan_cache: std::sync::Mutex<HashMap<String, PlanStatusResult>>,
    step_data_cache: std::sync::Mutex<HashMap<String, Vec<PlanStepData>>>,
    workspace_roots: std::sync::Mutex<HashMap<String, String>>,
}

/// Build an envelope `AttrNode` for a dynamically-typed ("Unknown") node.
///
/// Plans and plan steps are stored with an open subtype rather than a closed
/// typed variant so custom properties survive serialization round-trips.
fn plan_orch_attr_unknown(
    subtype: Option<String>,
    name: String,
    description: Option<String>,
    properties: std::collections::HashMap<String, serde_json::Value>,
) -> AttrNode {
    let now = chrono::Utc::now();
    AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: "Unknown".to_string(),
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

impl PlanOrchestrator {
    pub fn new(
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        llm_tx: mpsc::Sender<LlmMessage>,
        tool_orchestrator_tx: mpsc::Sender<ToolOrchestratorMessage>,
        chat_tx: mpsc::Sender<ChatMessage>,
        transport_tx: mpsc::Sender<TransportMessage>,
    ) -> Self {
        Self {
            memory_graph_tx,
            llm_tx,
            tool_orchestrator_tx,
            chat_tx,
            transport_tx,
            max_retries: 1,
            plan_cache: std::sync::Mutex::new(HashMap::new()),
            step_data_cache: std::sync::Mutex::new(HashMap::new()),
            workspace_roots: std::sync::Mutex::new(HashMap::new()),
        }
    }

    // ── Create Plan ───────────────────────────────────────────────────

    async fn create_plan(
        &self,
        goal: String,
        intent_name: Option<String>,
        _parameters: HashMap<String, String>,
        scope: Option<ModificationScope>,
        workspace_root: Option<String>,
    ) -> Result<PlanStatusResult> {
        info!("PlanOrchestrator: creating plan for goal: {}", goal);

        // 1. Gather context from graph for the LLM prompt
        let context = self.gather_plan_context(workspace_root.as_deref()).await;

        // 2. Generate plan steps via LLM (scope-aware when a scope is given)
        let steps = match scope {
            Some(ModificationScope::Project) => {
                self.generate_plan_steps_scoped(&goal, &context, "project").await?
            }
            Some(ModificationScope::Subproject { ref path }) => {
                let ctx = format!("Scope directory: {}\n{}", path, context);
                self.generate_plan_steps_scoped(&goal, &ctx, "subproject").await?
            }
            None => self.generate_plan_steps(&goal, &context).await?,
        };

        // 3. Store Plan and PlanStep nodes in the graph
        let plan_id = format!(
            "plan_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        // The Plan node is stored with `name = plan_id`, so approve/get-status
        // lookups resolve by the node name. Keep `plan_id` (the name) as the
        // identifier returned to callers — the raw graph UUID is only used
        // internally for relationship creation.
        let _plan_node_id = self
            .store_plan(&plan_id, &goal, &intent_name, &steps)
            .await?;

        let status = PlanStatusResult {
            plan_id: plan_id.clone(),
            goal: goal.clone(),
            status: PlanStatus::Pending,
            intent_name,
            steps: steps
                .iter()
                .enumerate()
                .map(|(i, step)| PlanStepEntry {
                    id: format!("{}-step-{}", plan_id, i + 1),
                    order: (i + 1) as u32,
                    description: step.description.clone(),
                    step_name: step.step_name.clone(),
                    status: PlanStatus::Pending,
                    result: None,
                    error: None,
                })
                .collect(),
            total_steps: steps.len() as u32,
            completed_steps: 0,
            failed_steps: 0,
        };

        // Cache the plan + step data in memory so approve/execute can find
        // them without relying on graph property round-tripping.
        self.plan_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plan_id.clone(), status.clone());
        self.step_data_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plan_id.clone(), steps.clone());
        if let Some(ref root) = workspace_root {
            if !root.is_empty() {
                self.workspace_roots
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(plan_id.clone(), root.clone());
            }
        }

        // 4. Push plan notification to chat
        self.chat_notify(&format!("📋 **Plan:** {}", goal), &status)
            .await;

        info!("PlanOrchestrator: plan created with {} steps", steps.len());
        Ok(status)
    }

    /// Gather project context from the graph for the LLM prompt.
    /// Gracefully handles missing graph data — returns partial context rather than erroring.
    async fn gather_plan_context(&self, workspace_root: Option<&str>) -> String {
        let mut context_parts: Vec<String> = Vec::new();

        // Tell the LLM the absolute project root so it generates arg_template
        // paths relative to the REAL project (not the process CWD).
        if let Some(root) = workspace_root {
            if !root.is_empty() {
                context_parts.push(format!("Project root: {}", root));
            }
        }

        // Query project context (non-fatal if unavailable)
        let (tx, rx) = oneshot::channel();
        if self
            .memory_graph_tx
            .send(MemoryGraphMessage::GetProjectContext { reply_to: tx })
            .await
            .is_ok()
        {
            if let Ok(Ok(snapshot)) = rx.await {
                context_parts.push(format!("Project: {}", snapshot.project.name()));
                context_parts.push(format!("Graph nodes: {}", snapshot.stats.total_nodes));
            }
        }

        // Query available StepDefinitions (non-fatal if unavailable)
        let (tx, rx) = oneshot::channel();
        if self
            .memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("stepDefinition".to_string()),
                subtype: None,
                name: None,
                limit: Some(100),
                reply_to: tx,
            })
            .await
            .is_ok()
        {
            if let Ok(Ok(nodes)) = rx.await {
                let step_names: Vec<String> = nodes
                    .iter()
                    .filter_map(|n| {
                        let cat = n
                            .get("category")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        Some(format!("{} ({})", n.name(), cat))
                    })
                    .collect();
                if !step_names.is_empty() {
                    context_parts.push(format!("Available steps: {}", step_names.join(", ")));
                }
            }
        }

        // Curated list of real routable tools. The LLM must pick step_names
        // from THIS list — invented names (e.g. "create_file") cannot be
        // dispatched by the ToolRouter in standalone mode.
        let curated_tools = [
            "filesystem_read", "filesystem_write", "filesystem_list",
            "filesystem_delete", "filesystem_move", "filesystem_copy",
            "build_analyze", "build_build", "build_test", "build_lint",
            "build_format", "build_fix", "build_list_modules",
            "project/build", "project/test", "project/lint",
        ];
        context_parts.push(format!("Available tools: {}", curated_tools.join(", ")));

        context_parts.join("\n")
    }

    /// Use the LLM to generate a step-by-step plan from the goal.
    async fn generate_plan_steps(&self, goal: &str, context: &str) -> Result<Vec<PlanStepData>> {
        self.generate_plan_steps_scoped(goal, context, "unscoped").await
    }

    /// Scope-aware plan generation. `scope_label` is one of:
    /// - `"project"` — Level 1: full project, may create/delete subprojects,
    ///   write build configs anywhere, add/remove top-level directories.
    /// - `"subproject"` — Level 2: limited to the scope directory. Must not
    ///   modify files outside the given path.
    /// - `"unscoped"` — no modification constraint (current behavior).
    async fn generate_plan_steps_scoped(
        &self,
        goal: &str,
        context: &str,
        scope_label: &str,
    ) -> Result<Vec<PlanStepData>> {
        let scope_instructions = match scope_label {
            "project" => {
                "SCOPE: PROJECT-LEVEL modification. \
                 You may create new subprojects (write meson.build with project(...), \
                 Cargo.toml, etc.), delete subprojects, or modify any file in the project. \
                 When creating a new subproject, ALWAYS include a step to run build_analyze \
                 on the new directory so the project analysis reflects the change."
            }
            "subproject" => {
                "SCOPE: SUBPROJECT-LEVEL modification. \
                 You may ONLY modify files within the scope directory shown below. \
                 Do NOT create or delete files outside this directory. \
                 When modifying the build config, ALWAYS include a step to run build_analyze \
                 on the scope directory so the subproject analysis reflects the change."
            }
            _ => {
                "No modification scope constraint — use general planning rules."
            }
        };

        let system_prompt = format!(
            "You are a planning assistant. Given a goal and available tools, \
            create a step-by-step plan as a JSON array. Each step has: \
            description (string), step_name (from available steps), \
            arg_template (object), depends_on (array of step indices, 1-based), \
            uses_error_context (bool). \
            {scope_instructions} \
            Output ONLY the JSON array, no other text."
        );

        let user_prompt = format!(
            "Goal: {}\n\nAvailable context:\n{}\n\nPlan steps (JSON array):",
            goal, context
        );

        let (tx, rx) = oneshot::channel();
        self.llm_tx
            .send(LlmMessage::Complete {
                prompt: format!("{}\n\n{}", system_prompt, user_prompt),
                role: spire_core::subsystems::llm::llm::LlmModelRole::Planning,
                reply_to: tx,
            })
            .await?;

        let response = rx.await??;

        // LLM responses sometimes arrive wrapped in markdown code fences
        // (```json ... ```) or with surrounding whitespace. Strip those
        // before parsing so the JSON array deserializes cleanly.
        let json_str = response
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        // Parse the JSON array of steps
        let steps: Vec<PlanStepData> = serde_json::from_str(json_str)
            .map_err(|e| anyhow::anyhow!("Failed to parse LLM plan response: {}", e))?;

        if steps.is_empty() {
            return Err(anyhow::anyhow!("LLM returned empty plan"));
        }

        Ok(steps)
    }

    /// Store Plan and PlanStep nodes in the graph.
    async fn store_plan(
        &self,
        plan_id: &str,
        goal: &str,
        intent_name: &Option<String>,
        steps: &[PlanStepData],
    ) -> Result<String> {
        // Store Plan node. We deliberately store with NodeType::Unknown +
        // subtype "plan" (not the typed NodeType::Plan variant) because the
        // typed variants DISCARD custom properties in GraphNode::from(NodeInput)
        // — the "plan" / "plan_step" subtype is required for plan_id / status
        // property round-tripping used by approve/execute lookups.
        let intent_val = match intent_name {
            Some(ref n) => serde_json::Value::String(n.clone()),
            None => serde_json::Value::String(String::new()),
        };
        let mut plan_props: HashMap<String, serde_json::Value> = HashMap::new();
        plan_props.insert("goal".to_string(), serde_json::json!(goal));
        plan_props.insert("status".to_string(), serde_json::json!("pending"));
        plan_props.insert("intent_name".to_string(), intent_val);
        plan_props.insert("total_steps".to_string(), serde_json::json!(steps.len()));
        plan_props.insert("completed_steps".to_string(), serde_json::json!(0));
        plan_props.insert("failed_steps".to_string(), serde_json::json!(0));
        plan_props.insert(
            "max_retries".to_string(),
            serde_json::json!(self.max_retries),
        );

        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::StoreAttrNode {
                node: plan_orch_attr_unknown(
                    Some("plan".to_string()),
                    plan_id.to_string(),
                    Some(goal.to_string()),
                    plan_props,
                ),
                reply_to: tx,
            })
            .await?;
        let plan_node = rx.await??;
        let plan_node_id = plan_node.id().to_string();

        // Store PlanStep nodes (Unknown + subtype "plan_step" for property
        // round-tripping — see note above on typed variants discarding props).
        for (i, step) in steps.iter().enumerate() {
            let step_id = format!("{}-step-{}", plan_id, i + 1);
            let (tx, rx) = oneshot::channel();
            self.memory_graph_tx
                .send(MemoryGraphMessage::StoreAttrNode {
                    node: plan_orch_attr_unknown(
                        Some("plan_step".to_string()),
                        step_id,
                        Some(step.description.clone()),
                        std::collections::HashMap::from([
                            ("plan_id".to_string(), serde_json::json!(plan_id)),
                            ("order".to_string(), serde_json::json!(i + 1)),
                            (
                                "description".to_string(),
                                serde_json::json!(step.description),
                            ),
                            ("step_name".to_string(), serde_json::json!(step.step_name)),
                            ("arg_template".to_string(), step.arg_template.clone()),
                            ("depends_on".to_string(), serde_json::json!(step.depends_on)),
                            (
                                "uses_error_context".to_string(),
                                serde_json::json!(step.uses_error_context),
                            ),
                            ("status".to_string(), serde_json::json!("pending")),
                            (
                                "max_retries".to_string(),
                                serde_json::json!(self.max_retries),
                            ),
                            ("retry_count".to_string(), serde_json::json!(0)),
                        ]),
                    ),
                    reply_to: tx,
                })
                .await?;

            let step_node = rx.await??;

            // Create HasStep relationship: Plan → PlanStep
            let (tx, rx) = oneshot::channel();
            self.memory_graph_tx
                .send(MemoryGraphMessage::CreateRelationship {
                    rel: spire_core::models::memory_graph::RelationshipInput {
                        edge_type: spire_core::models::memory_graph::RelationshipType::Custom(
                            "HAS_STEP".to_string(),
                        ),
                        from_id: plan_node_id.clone(),
                        to_id: step_node.id().to_string(),
                        properties: Some(std::collections::HashMap::from([(
                            "order".to_string(),
                            serde_json::json!(i + 1),
                        )])),
                        weight: None,
                    },
                    reply_to: tx,
                })
                .await?;
            let _ = rx.await?;
        }

        Ok(plan_node_id)
    }

    // ── Approve Plan — Execute Steps ───────────────────────────────────

    async fn approve_plan(&self, plan_id: &str) -> Result<()> {
        info!("PlanOrchestrator: approving plan: {}", plan_id);

        // Update plan status
        self.update_plan_status(plan_id, PlanStatus::Executing)
            .await?;

        // Get all steps for this plan
        let steps = self.get_plan_steps(plan_id).await?;

        // Execute steps sequentially
        self.execute_steps(plan_id, &steps).await?;

        Ok(())
    }

    /// Execute steps sequentially, handling dependencies and failures.
    async fn execute_steps(&self, plan_id: &str, steps: &[PlanStepEntry]) -> Result<()> {
        let mut completed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let _failed: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let _skipped: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let total = steps.len() as u32;

        // Get step data from graph
        let step_data = self.get_step_data(plan_id).await?;

        // Build execution order based on dependencies
        let execution_order = self.resolve_execution_order(&step_data);

        for order in &execution_order {
            let current_order = *order;
            // Map step_data to the step entry
            let idx = (current_order - 1) as usize;
            if idx >= step_data.len() {
                continue;
            }

            // Check dependencies
            let deps = &step_data[idx].depends_on;
            let deps_met = deps.iter().all(|d| completed.contains(d));
            if !deps_met {
                warn!(
                    "PlanOrchestrator: dependencies not met for step {}",
                    current_order
                );
                continue;
            }

            // Mark as running
            self.update_step_status(plan_id, current_order, PlanStatus::Executing, None, None)
                .await?;
            self.emit_plan_widget(plan_id).await?;

            let step_info = &step_data[idx];

            // Flatten the step's arg_template object into String parameters for
            // the ToolOrchestrator (its ExecuteTool accepts HashMap<String, String>).
            let mut params: HashMap<String, String> = HashMap::new();
            if let Some(obj) = step_info.arg_template.as_object() {
                for (k, v) in obj {
                    let val = match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::Bool(b) => b.to_string(),
                        _ => v.to_string(),
                    };
                    params.insert(k.clone(), val);
                }
            }

            // Resolve relative file paths in the step's params against the
            // plan's project root (from project/open) so filesystem tools
            // write into the REAL project, not the process CWD.
            if let Some(root) = self.workspace_roots.lock().unwrap_or_else(|e| e.into_inner()).get(plan_id).cloned() {
                let path_keys = ["path", "src", "destination", "dir", "root", "file_path"];
                for (k, v) in params.iter_mut() {
                    if path_keys.contains(&k.as_str()) && !v.starts_with('/') && !v.is_empty() {
                        *v = format!("{}/{}", root.trim_end_matches('/'), v.trim_start_matches('/'));
                    }
                }
            }

            // Execute via ToolOrchestrator and await the result.
            let (exec_tx, exec_rx) = oneshot::channel();
            info!(
                "PlanOrchestrator: dispatching step {} (tool={}) of plan {}",
                current_order, step_info.step_name, plan_id
            );
            if self
                .tool_orchestrator_tx
                .send(ToolOrchestratorMessage::ExecuteTool {
                    tool_name: step_info.step_name.clone(),
                    parameters: params,
                    reply_to: exec_tx,
                })
                .await
                .is_err()
            {
                // ToolOrchestrator unavailable — record the step as pending
                // rather than killing the approve handler.
                warn!(
                    "PlanOrchestrator: ToolOrchestrator channel closed while dispatching step {}",
                    current_order
                );
                self.update_step_status(
                    plan_id,
                    current_order,
                    PlanStatus::Pending,
                    None,
                    Some("ToolOrchestrator unavailable".to_string()),
                )
                .await
                .ok();
                self.emit_plan_widget(plan_id).await.ok();
                continue;
            }

            match exec_rx.await {
                Ok(Ok(result)) => {
                    info!(
                        "PlanOrchestrator: step {} of plan {} completed: {}",
                        current_order, plan_id, result
                    );
                    completed.insert(current_order);
                    self.update_step_status(
                        plan_id,
                        current_order,
                        PlanStatus::Completed,
                        Some(result),
                        None,
                    )
                    .await?;
                }
                Ok(Err(e)) => {
                    let err_msg = e.to_string();
                    warn!(
                        "PlanOrchestrator: step {} of plan {} failed: {}",
                        current_order, plan_id, err_msg
                    );
                    self.update_step_status(
                        plan_id,
                        current_order,
                        PlanStatus::Failed,
                        None,
                        Some(err_msg.clone()),
                    )
                    .await?;
                    self.emit_plan_widget(plan_id).await?;
                    return Err(anyhow::anyhow!(
                        "Plan step {} failed: {}",
                        current_order,
                        err_msg
                    ));
                }
                Err(e) => {
                    let msg = format!("Plan step {} execution channel lost: {}", current_order, e);
                    warn!("{}", msg);
                    self.update_step_status(
                        plan_id,
                        current_order,
                        PlanStatus::Failed,
                        None,
                        Some(msg.clone()),
                    )
                    .await?;
                    return Err(anyhow::anyhow!("{}", msg));
                }
            }

            self.emit_plan_widget(plan_id).await?;
        }

        // Check if all steps completed
        let all_completed = completed.len() == total as usize;
        if all_completed {
            self.update_plan_status(plan_id, PlanStatus::Completed)
                .await?;
            self.chat_notify(
                "✅ **Plan completed** — all steps succeeded.",
                &PlanStatusResult {
                    plan_id: plan_id.to_string(),
                    goal: String::new(),
                    status: PlanStatus::Completed,
                    intent_name: None,
                    steps: vec![],
                    total_steps: total,
                    completed_steps: completed.len() as u32,
                    failed_steps: 0,
                },
            )
            .await;
        }

        Ok(())
    }

    /// Resolve step execution order respecting dependencies (topological sort).
    fn resolve_execution_order(&self, steps: &[PlanStepData]) -> Vec<u32> {
        let n = steps.len();
        let mut visited = vec![false; n];
        let mut order = Vec::new();

        fn dfs(idx: usize, steps: &[PlanStepData], visited: &mut [bool], order: &mut Vec<u32>) {
            if visited[idx] {
                return;
            }
            visited[idx] = true;
            for dep in &steps[idx].depends_on {
                if *dep > 0 && *dep <= steps.len() as u32 {
                    dfs((dep - 1) as usize, steps, visited, order);
                }
            }
            order.push((idx + 1) as u32);
        }

        for i in 0..n {
            dfs(i, steps, &mut visited, &mut order);
        }

        order
    }

    // ── Plan Control Methods ───────────────────────────────────────────

    async fn reject_plan(&self, plan_id: &str, reason: Option<String>) -> Result<()> {
        info!("PlanOrchestrator: rejecting plan: {}", plan_id);
        self.update_plan_status(plan_id, PlanStatus::Rejected)
            .await?;
        let msg = match reason {
            Some(r) => format!("❌ **Plan rejected** — {}", r),
            None => "❌ **Plan rejected**.".to_string(),
        };
        self.chat_notify(
            &msg,
            &PlanStatusResult {
                plan_id: plan_id.to_string(),
                goal: String::new(),
                status: PlanStatus::Rejected,
                intent_name: None,
                steps: vec![],
                total_steps: 0,
                completed_steps: 0,
                failed_steps: 0,
            },
        )
        .await;
        Ok(())
    }

    async fn get_plan_status(&self, plan_id: &str) -> Result<PlanStatusResult> {
        // Prefer the in-memory cache created during plan creation.
        if let Some(plan) = self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).get(plan_id) {
            return Ok(plan.clone());
        }
        // Fall back to querying the Plan node by name — plans are stored as
        // Unknown nodes with subtype "plan" and name = plan_id.
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("plan".to_string()),
                name: Some(plan_id.to_string()),
                limit: Some(1),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        let attr = nodes
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Plan '{}' not found", plan_id))?;

        let goal = attr
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status_str = attr
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending")
            .to_string();
        let intent_name = attr
            .get("intent_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let total = attr.get("total_steps").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let completed = attr
            .get("completed_steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let failed = attr.get("failed_steps").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let status = match status_str.as_str() {
            "approved" => PlanStatus::Approved,
            "executing" => PlanStatus::Executing,
            "paused" => PlanStatus::Paused,
            "completed" => PlanStatus::Completed,
            "rejected" => PlanStatus::Rejected,
            "failed" => PlanStatus::Failed,
            _ => PlanStatus::Pending,
        };

        // Query plan steps
        let steps = self.get_plan_steps(plan_id).await?;

        Ok(PlanStatusResult {
            plan_id: plan_id.to_string(),
            goal,
            status,
            intent_name,
            steps,
            total_steps: total,
            completed_steps: completed,
            failed_steps: failed,
        })
    }

    /// Query PlanStep nodes for a plan, sorted by order.
    async fn get_plan_steps(&self, plan_id: &str) -> Result<Vec<PlanStepEntry>> {
        // Prefer the in-memory step cache (created during plan creation).
        if let Some(data) = self.step_data_cache.lock().unwrap_or_else(|e| e.into_inner()).get(plan_id) {
            let steps: Vec<PlanStepEntry> = data
                .iter()
                .enumerate()
                .map(|(i, s)| PlanStepEntry {
                    id: format!("{}-step-{}", plan_id, i + 1),
                    order: (i + 1) as u32,
                    description: s.description.clone(),
                    step_name: s.step_name.clone(),
                    status: PlanStatus::Pending,
                    result: None,
                    error: None,
                })
                .collect();
            return Ok(steps);
        }
        // Query by plan_id property
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("plan_step".to_string()),
                name: None,
                limit: Some(100),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        let nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| n.get("plan_id").and_then(|v| v.as_str()) == Some(plan_id))
            .collect();

        let mut steps: Vec<PlanStepEntry> = nodes
            .iter()
            .map(|n| {
                let order = n
                    .get("order")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let status_str = n
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pending");
                let status = match status_str {
                    "approved" => PlanStatus::Approved,
                    "executing" => PlanStatus::Executing,
                    "paused" => PlanStatus::Paused,
                    "completed" => PlanStatus::Completed,
                    "rejected" => PlanStatus::Rejected,
                    "failed" => PlanStatus::Failed,
                    "skipped" => PlanStatus::Skipped,
                    _ => PlanStatus::Pending,
                };
                PlanStepEntry {
                    id: n.id().to_string(),
                    order,
                    description: n
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    step_name: n
                        .get("step_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    status,
                    result: n
                        .get("result")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    error: n
                        .get("error")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                }
            })
            .collect();

        steps.sort_by_key(|a| a.order);
        Ok(steps)
    }

    /// Get PlanStepData from plan steps (for dependency resolution).
    async fn get_step_data(&self, plan_id: &str) -> Result<Vec<PlanStepData>> {
        // Prefer the in-memory step cache (created during plan creation).
        if let Some(data) = self.step_data_cache.lock().unwrap_or_else(|e| e.into_inner()).get(plan_id) {
            return Ok(data.clone());
        }
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("plan_step".to_string()),
                name: None,
                limit: Some(100),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        let nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| n.get("plan_id").and_then(|v| v.as_str()) == Some(plan_id))
            .collect();

        let mut steps: Vec<PlanStepData> = nodes
            .iter()
            .map(|n| PlanStepData {
                description: n
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                step_name: n
                    .get("step_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                arg_template: n
                    .get("arg_template")
                    .cloned()
                    .unwrap_or(serde_json::json!({})),
                depends_on: n
                    .get("depends_on")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_u64().map(|n| n as u32))
                            .collect()
                    })
                    .unwrap_or_default(),
                uses_error_context: n
                    .get("uses_error_context")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
            .collect();

        steps.sort_by(|a, b| {
            // Order by will be determined by depends_on analysis
            a.description.cmp(&b.description)
        });
        Ok(steps)
    }

    // ── State Management Helpers ───────────────────────────────────────

    async fn update_plan_status(&self, plan_id: &str, status: PlanStatus) -> Result<()> {
        // Keep the in-memory cache in sync so approve/execute can read it.
        if let Some(plan) = self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).get_mut(plan_id) {
            plan.status = status.clone();
        }

        // The in-memory cache is the source of truth for approve/execute.
        // If the plan is cached, graph persistence is best-effort — the
        // graph row may be missing if property round-tripping failed.
        if self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).contains_key(plan_id) {
            return Ok(());
        }

        // First query the node by name to get its UUID
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("plan".to_string()),
                name: Some(plan_id.to_string()),
                limit: Some(1),
                reply_to: tx,
            })
            .await
            .ok();

        let nodes = match rx.await {
            Ok(Ok(nodes)) => nodes,
            _ => return Ok(()),
        };
        let Some(node) = nodes.into_iter().next() else {
            return Ok(());
        };

        let status_str = serde_json::json!(status);
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::UpdateNode {
                id: node.id().to_string().to_string(),
                updates: NodeUpdate {
                    node_type: None,
                    subtype: None,
                    name: None,
                    description: None,
                    properties: Some(std::collections::HashMap::from([(
                        "status".to_string(),
                        status_str,
                    )])),
                    embedding_id: None,
                },
                reply_to: tx,
            })
            .await?;
        let _ = rx.await?;
        Ok(())
    }

    async fn update_step_status(
        &self,
        plan_id: &str,
        order: u32,
        status: PlanStatus,
        result: Option<String>,
        error: Option<String>,
    ) -> Result<()> {
        // Best-effort graph persistence — failure must NEVER abort approve/execute.
        let (tx, rx) = oneshot::channel();
        if self
            .memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("plan_step".to_string()),
                name: None,
                limit: Some(100),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return Ok(());
        }
        let nodes = match rx.await {
            Ok(Ok(nodes)) => nodes,
            _ => return Ok(()),
        };
        if let Some(node) = nodes
            .into_iter()
            .find(|n| {
                n.get("plan_id").and_then(|v| v.as_str()) == Some(plan_id)
                    && n.get("order").and_then(|v| v.as_u64()) == Some(order as u64)
            })
        {
            let mut props = std::collections::HashMap::from([(
                "status".to_string(),
                serde_json::json!(status),
            )]);
            if let Some(r) = result {
                props.insert("result".to_string(), serde_json::json!(r));
            }
            if let Some(e) = error {
                props.insert("error".to_string(), serde_json::json!(e));
            }

            let (tx, rx) = oneshot::channel();
            if self
                .memory_graph_tx
                .send(MemoryGraphMessage::UpdateNode {
                    id: node.id().to_string(),
                    updates: NodeUpdate {
                        node_type: None,
                        subtype: None,
                        name: None,
                        description: None,
                        properties: Some(props),
                        embedding_id: None,
                    },
                    reply_to: tx,
                })
                .await
                .is_err()
            {
                return Ok(());
            }
            let _ = rx.await;
        }
        Ok(())
    }

    // ── Widget + Chat Helpers ──────────────────────────────────────────

    /// Emit a plan-list widget update via the transport.
    async fn emit_plan_widget(&self, plan_id: &str) -> Result<()> {
        let status = self.get_plan_status(plan_id).await?;
        let _ = self
            .transport_tx
            .send(TransportMessage::SendNotification {
                method: "event/widget/update".to_string(),
                params: serde_json::json!({
                    "widgetId": format!("plan-{}", plan_id),
                    "widgetType": "plan-list",
                    "state": {
                        "title": format!("📋 Plan: {}", status.goal),
                        "status": status.status,
                        "total_steps": status.total_steps,
                        "completed_steps": status.completed_steps,
                        "failed_steps": status.failed_steps,
                        "steps": status.steps.iter().map(|s| {
                            serde_json::json!({
                                "order": s.order,
                                "description": s.description,
                                "status": s.status,
                                "error": s.error,
                            })
                        }).collect::<Vec<_>>(),
                    }
                }),
            })
            .await;
        Ok(())
    }

    /// Post a chat message with the plan widget.
    async fn chat_notify(&self, content: &str, status: &PlanStatusResult) {
        let widget = serde_json::json!({
            "widgetId": format!("plan-{}", status.plan_id),
            "widgetType": "plan-list",
            "state": {
                "title": format!("📋 Plan: {}", status.goal),
                "status": status.status,
                "total_steps": status.total_steps,
                "completed_steps": status.completed_steps,
                "failed_steps": status.failed_steps,
                "steps": status.steps.iter().map(|s| {
                    serde_json::json!({
                        "order": s.order,
                        "description": s.description,
                        "status": s.status,
                        "error": s.error,
                    })
                }).collect::<Vec<_>>(),
            }
        });

        let (tx, _rx) = oneshot::channel();
        let _ = self
            .chat_tx
            .send(ChatMessage::Append {
                chat_id: "default".to_string(),
                content: content.to_string(),
                role: "assistant".to_string(),
                reply_to: tx,
                widget: Some(widget),
            })
            .await;

        // Push real-time notification
        let _ = self
            .transport_tx
            .send(TransportMessage::SendNotification {
                method: "event/chat/message".to_string(),
                params: serde_json::json!({
                    "chatId": "default",
                    "content": content,
                    "role": "assistant",
                    "widget": {
                        "widgetId": format!("plan-{}", status.plan_id),
                        "widgetType": "plan-list",
                    }
                }),
            })
            .await;
    }
}

#[async_trait]
impl Actor for PlanOrchestrator {
    type Message = PlanOrchestratorMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            PlanOrchestratorMessage::CreatePlan {
                goal,
                intent_name,
                parameters,
                scope,
                workspace_root,
                reply_to,
            } => {
                let result = self
                    .create_plan(goal, intent_name, parameters, scope, workspace_root)
                    .await;
                let _ = reply_to.send(result);
            }
            PlanOrchestratorMessage::ApprovePlan { plan_id, reply_to } => {
                let result = self.approve_plan(&plan_id).await;
                let _ = reply_to.send(result);
            }
            PlanOrchestratorMessage::RejectPlan {
                plan_id,
                reason,
                reply_to,
            } => {
                let result = self.reject_plan(&plan_id, reason).await;
                let _ = reply_to.send(result);
            }
            PlanOrchestratorMessage::GetPlanStatus { plan_id, reply_to } => {
                let result = self.get_plan_status(&plan_id).await;
                let _ = reply_to.send(result);
            }
            PlanOrchestratorMessage::PausePlan { plan_id, reply_to } => {
                let result = self.update_plan_status(&plan_id, PlanStatus::Paused).await;
                let _ = reply_to.send(result);
            }
            PlanOrchestratorMessage::ResumePlan { plan_id, reply_to } => {
                // Re-approve to resume execution
                let result = self.approve_plan(&plan_id).await;
                let _ = reply_to.send(result);
            }
            PlanOrchestratorMessage::RetryStep {
                plan_id,
                step_order,
                reply_to,
            } => {
                info!(
                    "PlanOrchestrator: retrying step {} of plan {}",
                    step_order, plan_id
                );
                self.update_step_status(&plan_id, step_order, PlanStatus::Pending, None, None)
                    .await
                    .ok();
                let result = self.approve_plan(&plan_id).await;
                let _ = reply_to.send(result);
            }
            PlanOrchestratorMessage::SkipStep {
                plan_id,
                step_order,
                reply_to,
            } => {
                info!(
                    "PlanOrchestrator: skipping step {} of plan {}",
                    step_order, plan_id
                );
                self.update_step_status(&plan_id, step_order, PlanStatus::Skipped, None, None)
                    .await
                    .ok();
                let result = self.approve_plan(&plan_id).await;
                let _ = reply_to.send(result);
            }
        }
    }
}