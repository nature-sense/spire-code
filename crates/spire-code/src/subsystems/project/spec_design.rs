// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Spec design actor** — the free-form brainstorm step that precedes spec
//! generation.
//!
//! Interaction model (agreed with the product):
//!
//! - Everything before **Decide** is free-form and changed **through prompts**: a
//!   chat transcript accumulates turns, `Summarize` condenses it into a running
//!   summary (the source of truth), and `PromoteToSpec` compiles the summary
//!   into a `spec.md` document. The user steers with a free-form instruction
//!   ("summarize with techniques X", "add the new findings", "recreate around
//!   storage"). Nothing is marked "decided" during this phase.
//! - **Decide** is a *button*, not a prompt: it freezes the spec and switches to
//!   a deterministic tail — `markdown_to_spec` → `validate` → persist. After
//!   Decide the doc is the accepted contract. **Reopen** always returns to the
//!   free-form phase (the derived AppSpec may not actually meet requirements), so
//!   decide/reopen is cheap and repeatable; every Decide is recorded in
//!   `accepted` for comparison.
//!
//! The LLM is injected as a plain async closure (like [`super::spec_gen`]), so
//! the actor is fully testable with canned scripts.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;

use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::actors::Actor;
use spire_core::models::memory_graph::AttrNode;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;

use super::spec::{validate, AppSpec, SpecIssue, SpecIssueSeverity};
use super::spec_graph;
use super::spec_md;

/// An injected LLM call: prompt text → reply text (mirrors `spec_gen`).
pub type LlmCall = Box<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync,
>;

pub const ROLE_USER: &str = "user";
pub const ROLE_ASSISTANT: &str = "assistant";

/// Whether the design session is still free-form or has been frozen by Decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesignMode {
    /// Prompts drive everything (the default).
    Freeform,
    /// The spec is frozen; only Reopen is allowed.
    Decided,
}

/// One conversation turn (user or assistant) in the design transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignTurn {
    pub role: String,
    pub text: String,
}

/// A condensed document (summary or spec). The summary is the running source of
/// truth during the free-form phase; the spec is the `spec.md` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignArtifact {
    pub version: u32,
    pub content: String,
    pub source_turns: Vec<usize>,
    pub produced_at: chrono::DateTime<chrono::Utc>,
}

/// One deterministic derivation recorded by a Decide press.
#[derive(Debug, Clone, Serialize)]
pub struct AcceptedSpec {
    pub version: u32,
    pub app_spec: AppSpec,
    pub issues: Vec<SpecIssue>,
    pub decided_at: chrono::DateTime<chrono::Utc>,
}

/// A point-in-time view of the design session.
#[derive(Debug, Clone, Serialize)]
pub struct SpecDesignState {
    pub mode: DesignMode,
    pub project_name: String,
    pub goal: String,
    pub summary: Option<DesignArtifact>,
    pub spec: Option<DesignArtifact>,
    pub turn_count: usize,
    pub accepted: Vec<AcceptedSpec>,
    pub latest: Option<AppSpec>,
    pub last_issues: Vec<SpecIssue>,
}

/// Messages routed to the [`SpecDesignActor`]. Each carries its own typed reply
/// channel, following the crate's actor convention.
#[derive(Debug)]
pub enum SpecDesignMessage {
    Start {
        project_name: String,
        goal: String,
        /// false (default) resumes a persisted session; true starts fresh.
        reset: bool,
        reply_to: oneshot::Sender<Result<SpecDesignState, String>>,
    },
    /// A user turn in the free-form brainstorm.
    Reply {
        text: String,
        reply_to: oneshot::Sender<Result<SpecDesignState, String>>,
    },
    /// Mirror any other turn (e.g. the assistant's chat reply) into the
    /// transcript so a later summarize sees the whole conversation.
    AppendTurn {
        role: String,
        text: String,
        reply_to: oneshot::Sender<Result<SpecDesignState, String>>,
    },
    Summarize {
        instruction: String,
        reply_to: oneshot::Sender<Result<DesignArtifact, String>>,
    },
    PromoteToSpec {
        instruction: String,
        reply_to: oneshot::Sender<Result<DesignArtifact, String>>,
    },
    /// Free-form brainstorm turn INSIDE the design session: appends the user
    /// question, asks the LLM (opinionated options + recommendation, grounded
    /// in the running summary), appends the answer, replies with both.
    Ask {
        text: String,
        reply_to: oneshot::Sender<Result<(String, SpecDesignState), String>>,
    },
    Decide {
        reply_to: oneshot::Sender<Result<AppSpec, String>>,
    },
    Reopen {
        reply_to: oneshot::Sender<Result<SpecDesignState, String>>,
    },
    GetState {
        reply_to: oneshot::Sender<SpecDesignState>,
    },
}

/// Free-form design session. Created per project (the wizard starts one when it
/// opens the design step); holds the transcript + the running summary/spec.
pub struct SpecDesignActor {
    project_name: String,
    goal: String,
    llm: LlmCall,
    memory_graph_tx: Option<mpsc::Sender<MemoryGraphMessage>>,
    mode: DesignMode,
    turns: Vec<DesignTurn>,
    /// Turns already folded into the current summary (delta = turns after it).
    summarized_through: usize,
    summary: Option<DesignArtifact>,
    spec: Option<DesignArtifact>,
    accepted: Vec<AcceptedSpec>,
    latest: Option<AppSpec>,
    last_issues: Vec<SpecIssue>,
    /// Session-instance token: child nodes (turns/documents) are named under it
    /// so a fresh `start(reset: true)` simply switches instance instead of
    /// deleting stale nodes. Set at session creation/reset.
    instance: String,
    /// How many turns are already persisted to the graph (append-only writes).
    persisted_through: usize,
}

impl SpecDesignActor {
    /// A fresh, unconfigured session. The RPC entry always calls [`Self::start`]
    /// first, which fixes project + goal; decide/reopen guard on that.
    pub fn new(llm: LlmCall) -> Self {
        Self {
            project_name: String::new(),
            goal: String::new(),
            llm,
            memory_graph_tx: None,
            mode: DesignMode::Freeform,
            turns: Vec::new(),
            summarized_through: 0,
            summary: None,
            spec: None,
            accepted: Vec::new(),
            latest: None,
            last_issues: Vec::new(),
            instance: String::new(),
            persisted_through: 0,
        }
    }

    /// Wire the memory graph so Decide can persist the derived spec.
    pub fn set_memory_graph(&mut self, tx: mpsc::Sender<MemoryGraphMessage>) {
        self.memory_graph_tx = Some(tx);
    }

    /// Node/subtype + name conventions for the persisted free-form session.
    /// Everything hangs off the session node keyed by project name; turns and
    /// documents are named under the session `instance` token so resets never
    /// collide with stale data. Node merges are keyed by (node_type, subtype,
    /// name), making re-persisting idempotent.
    fn session_node_name(&self) -> String {
        self.project_name.clone()
    }

    fn turn_node_name(&self, index: usize) -> String {
        format!("turn.{}.{index}", self.instance)
    }

    fn doc_node_name(&self, kind: &str) -> String {
        format!("{kind}.{}", self.instance)
    }

    /// Persist the free-form session into the project memory graph so a
    /// brainstorm survives restarts and lives beside the decided spec. Written
    /// as plain typed nodes (no whole-session blob): a `design_session` node,
    /// one `design_turn` node per new turn, and `design_document` nodes for the
    /// current summary/spec. Best-effort — a missing graph keeps the session
    /// in-memory.
    async fn persist_session(&self) {
        let Some(mg_tx) = &self.memory_graph_tx else {
            return;
        };
        let props = |p: &[(&str, serde_json::Value)]| -> std::collections::HashMap<String, serde_json::Value> {
            p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
        };
        let now = chrono::Utc::now();
        let session = AttrNode {
            id: uuid::Uuid::new_v4().to_string(),
            node_type: super::spec_persist::MG_NODE_TYPE.to_string(),
            subtype: Some(DS_SESSION.to_string()),
            name: self.session_node_name(),
            description: Some(self.goal.clone()),
            properties: props(&[
                ("goal", serde_json::json!(self.goal)),
                ("mode", serde_json::json!(mode_str(self.mode))),
                (
                    "summarized_through",
                    serde_json::json!(self.summarized_through),
                ),
                ("instance", serde_json::json!(self.instance)),
                ("turn_count", serde_json::json!(self.turns.len())),
            ]),
            embedding_id: None,
            created_at: now,
            updated_at: now,
            version: 1,
        };
        if let Err(e) = merge_node(mg_tx, session).await {
            warn!(
                "[SpecDesign] session node persist failed for '{}': {e}",
                self.project_name
            );
        }
        for index in self.persisted_through.min(self.turns.len())..self.turns.len() {
            let turn = &self.turns[index];
            let node = AttrNode {
                id: uuid::Uuid::new_v4().to_string(),
                node_type: super::spec_persist::MG_NODE_TYPE.to_string(),
                subtype: Some(DS_TURN.to_string()),
                name: self.turn_node_name(index),
                description: None,
                properties: props(&[
                    ("role", serde_json::json!(turn.role)),
                    ("text", serde_json::json!(turn.text)),
                    ("index", serde_json::json!(index)),
                ]),
                embedding_id: None,
                created_at: now,
                updated_at: now,
                version: 1,
            };
            if let Err(e) = merge_node(mg_tx, node).await {
                warn!("[SpecDesign] turn node persist failed: {e}");
            }
        }
        if let Some(summary) = &self.summary {
            persist_doc(mg_tx, &self.doc_node_name("summary"), summary, DS_SUMMARY).await;
        }
        if let Some(spec) = &self.spec {
            persist_doc(mg_tx, &self.doc_node_name("spec"), spec, DS_SPEC).await;
        }
    }

    /// Restore a persisted session from the project graph for `project_name`.
    /// Returns true when a session node existed and was loaded.
    async fn resume_session(&mut self, project_name: &str) -> bool {
        let Some(mg_tx) = &self.memory_graph_tx else {
            return false;
        };
        let Ok(mut found) = query_nodes(
            mg_tx,
            Some(super::spec_persist::MG_NODE_TYPE),
            Some(DS_SESSION),
            Some(project_name),
            1,
        )
        .await
        else {
            return false;
        };
        let Some(session) = found.pop() else {
            return false;
        };
        let Some(instance) = session.str_prop("instance") else {
            return false;
        };
        // Goal hint comes from the caller; everything else from the graph.
        let mode = match session.str_prop("mode").as_deref() {
            Some("decided") => DesignMode::Decided,
            _ => DesignMode::Freeform,
        };
        self.project_name = project_name.to_string();
        self.mode = mode;
        self.instance = instance.clone();
        self.summarized_through = session.u32_prop("summarized_through").unwrap_or(0) as usize;

        // Turns under this instance, ordered by index.
        let Ok(turn_nodes) = query_nodes(
            mg_tx,
            Some(super::spec_persist::MG_NODE_TYPE),
            Some(DS_TURN),
            None,
            100_000,
        )
        .await
        else {
            return true;
        };
        let prefix = format!("turn.{instance}.");
        let mut turns: Vec<(u32, DesignTurn)> = turn_nodes
            .into_iter()
            .filter(|n| n.name.starts_with(&prefix))
            .filter_map(|n| {
                let index = n.u32_prop("index")?;
                let role = n.str_prop("role")?;
                let text = n.str_prop("text")?;
                Some((index, DesignTurn { role, text }))
            })
            .collect();
        turns.sort_by_key(|(i, _)| *i);
        self.turns = turns.into_iter().map(|(_, t)| t).collect();
        self.persisted_through = self.turns.len();

        // Summary + spec documents.
        self.summary = load_doc(mg_tx, &format!("summary.{instance}"), DS_SUMMARY).await;
        self.spec = load_doc(mg_tx, &format!("spec.{instance}"), DS_SPEC).await;
        self.accepted.clear();
        self.latest = None;
        self.last_issues.clear();
        info!(
            "[SpecDesign] resumed persisted session for '{}' ({} turns, instance {})",
            self.project_name,
            self.turns.len(),
            instance
        );
        true
    }

    /// Async entry used by the Start RPC: resume a persisted session unless a
    /// reset was requested.
    pub async fn start_session(
        &mut self,
        project_name: &str,
        goal: &str,
        reset: bool,
    ) -> Result<SpecDesignState, String> {
        if !reset && self.resume_session(project_name).await {
            self.goal = goal.to_string();
            return Ok(self.state());
        }
        let _ = self.start(project_name, goal);
        self.persist_session().await;
        Ok(self.state())
    }

    pub fn state(&self) -> SpecDesignState {
        SpecDesignState {
            mode: self.mode,
            project_name: self.project_name.clone(),
            goal: self.goal.clone(),
            summary: self.summary.clone(),
            spec: self.spec.clone(),
            turn_count: self.turns.len(),
            accepted: self.accepted.clone(),
            latest: self.latest.clone(),
            last_issues: self.last_issues.clone(),
        }
    }

    fn require_session(&self) -> Result<(), String> {
        if self.project_name.is_empty() {
            Err("no spec-design session — call spec-design/start first".to_string())
        } else {
            Ok(())
        }
    }

    fn require_freeform(&self) -> Result<(), String> {
        if self.mode == DesignMode::Decided {
            Err(
                "already decided — the spec is frozen; Reopen to resume the free-form design"
                    .to_string(),
            )
        } else {
            Ok(())
        }
    }

    /// (Re)start the session for a project/goal.
    /// Start a fresh session (the Start RPC uses [`Self::start_session`], which
    /// resumes a persisted one unless reset).
    pub fn start(&mut self, project_name: &str, goal: &str) -> Result<SpecDesignState, String> {
        self.project_name = project_name.to_string();
        self.goal = goal.to_string();
        self.mode = DesignMode::Freeform;
        self.turns.clear();
        self.summarized_through = 0;
        self.summary = None;
        self.spec = None;
        self.accepted.clear();
        self.latest = None;
        self.last_issues.clear();
        self.instance = uuid::Uuid::new_v4().to_string();
        self.persisted_through = 0;
        info!(
            "[SpecDesign] session started for '{}': {}",
            self.project_name, self.goal
        );
        Ok(self.state())
    }

    /// Append a turn to the free-form transcript.
    pub fn append_turn(&mut self, role: &str, text: &str) -> Result<SpecDesignState, String> {
        self.require_freeform()?;
        self.require_session()?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("empty turn".to_string());
        }
        self.turns.push(DesignTurn {
            role: role.to_string(),
            text,
        });
        Ok(self.state())
    }

    /// Free-form condensation of the transcript into the running summary.
    pub async fn summarize(&mut self, instruction: &str) -> Result<DesignArtifact, String> {
        self.require_freeform()?;
        self.require_session()?;
        let instruction = instruction.trim();
        if instruction.is_empty() {
            return Err("summarize needs an instruction".to_string());
        }
        let existing = self.summary.as_ref().map(|a| a.content.clone());
        let (material, source_turns) = if wants_full_material(instruction) {
            (self.turns.clone(), (0..self.turns.len()).collect())
        } else {
            (
                self.turns[self.summarized_through.min(self.turns.len())..].to_vec(),
                (self.summarized_through.min(self.turns.len())..self.turns.len()).collect(),
            )
        };
        let prompt = summarize_prompt(
            &self.project_name,
            &self.goal,
            instruction,
            existing.as_deref(),
            &material,
        );
        let content = self.call_llm(prompt).await?;
        let version = self.summary.as_ref().map(|a| a.version).unwrap_or(0) + 1;
        let artifact = DesignArtifact {
            version,
            content,
            source_turns,
            produced_at: chrono::Utc::now(),
        };
        info!(
            "[SpecDesign] summary v{} for '{}' ({:?} instruction)",
            artifact.version, self.project_name, instruction
        );
        self.summary = Some(artifact.clone());
        self.summarized_through = self.turns.len();
        self.persist_session().await;
        Ok(artifact)
    }

    /// Compile the (mature) summary into the `spec.md` document. Still a prompt:
    /// the user keeps refining via chat until Decide freezes it.
    pub async fn promote(&mut self, instruction: &str) -> Result<DesignArtifact, String> {
        self.require_freeform()?;
        self.require_session()?;
        let instruction = instruction.trim();
        if instruction.is_empty() {
            return Err("promote needs an instruction".to_string());
        }
        // A summary is the source of truth for the spec — create one on demand.
        if self.summary.is_none() {
            self.summarize("Create a first summary of the whole design conversation.")
                .await?;
        }
        let summary = self.summary.clone().expect("summary created above");
        let existing_spec = self.spec.as_ref().map(|a| a.content.clone());
        let context: Vec<DesignTurn> =
            self.turns[self.summarized_through.min(self.turns.len())..].to_vec();
        let prompt = compile_spec_prompt(
            &self.project_name,
            &self.goal,
            instruction,
            &summary.content,
            existing_spec.as_deref(),
            &context,
        );
        let content = self.call_llm(prompt).await?;
        let version = self.spec.as_ref().map(|a| a.version).unwrap_or(0) + 1;
        let artifact = DesignArtifact {
            version,
            content,
            source_turns: (0..self.turns.len()).collect(),
            produced_at: chrono::Utc::now(),
        };
        info!(
            "[SpecDesign] spec v{} for '{}' ({:?} instruction)",
            artifact.version, self.project_name, instruction
        );
        self.spec = Some(artifact.clone());
        self.persist_session().await;
        Ok(artifact)
    }

    /// Free-form brainstorm answer inside the design session (see the `Ask`
    /// message). Appends the user turn FIRST (capture survives an LLM error),
    /// then asks the LLM grounded in the current summary + recent turns,
    /// appends the assistant answer, and returns (answer, state).
    pub async fn ask(&mut self, text: &str) -> Result<(String, SpecDesignState), String> {
        self.require_freeform()?;
        self.require_session()?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("empty question".to_string());
        }
        self.turns.push(DesignTurn {
            role: ROLE_USER.to_string(),
            text: text.clone(),
        });
        let summary = self.summary.as_ref().map(|a| a.content.clone());
        let start = self.turns.len().saturating_sub(10);
        let context = self.turns[start..].to_vec();
        let prompt = brainstorm_prompt(
            &self.project_name,
            &self.goal,
            &text,
            summary.as_deref(),
            &context,
        );
        let answer = self.call_llm(prompt).await?;
        self.turns.push(DesignTurn {
            role: ROLE_ASSISTANT.to_string(),
            text: answer.clone(),
        });
        self.persist_session().await;
        Ok((answer, self.state()))
    }

    /// THE button: freeze the spec and derive the AppSpec deterministically.
    pub async fn decide(&mut self) -> Result<AppSpec, String> {
        self.require_freeform()?;
        self.require_session()?;
        let Some(spec) = self.spec.as_ref() else {
            return Err(format!(
                "nothing to decide for '{}' — promote the summary to a spec first",
                self.project_name
            ));
        };
        let app: AppSpec = spec_md::markdown_to_spec(&spec.content)?;
        let issues = validate(&app);
        let errors: Vec<&SpecIssue> = issues
            .iter()
            .filter(|i| i.severity == SpecIssueSeverity::Error)
            .collect();
        if !errors.is_empty() {
            let detail = errors
                .iter()
                .map(|i| format!("  [{}] {}", i.path, i.message))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(format!(
                "spec for '{}' does not validate — {} error(s):\n{detail}\nReopen and refine via prompts, then Decide again.",
                self.project_name,
                errors.len()
            ));
        }

        // Deterministic tail: persist the derivation (best-effort).
        if let Some(tx) = self.memory_graph_tx.clone() {
            let g = spec_graph::decompose(&app);
            let persisted =
                super::spec_persist::store_spec_graph(&tx, &self.project_name, &self.goal, &g)
                    .await;
            if persisted.is_none() {
                warn!(
                    "[SpecDesign] Decide: spec persisted without an anchor for '{}' (graph unavailable?)",
                    self.project_name
                );
            }
        }

        self.mode = DesignMode::Decided;
        self.last_issues = issues;
        self.latest = Some(app.clone());
        self.accepted.push(AcceptedSpec {
            version: self.accepted.len() as u32 + 1,
            issues: self.last_issues.clone(),
            app_spec: app.clone(),
            decided_at: chrono::Utc::now(),
        });
        self.persist_session().await;
        info!(
            "[SpecDesign] '{}' decided — appspec v{} ({} types, {} actors, {} methods, {} screens)",
            self.project_name,
            self.accepted.len(),
            app.types.len(),
            app.actors.len(),
            app.bridge.len(),
            app.ui.len()
        );
        Ok(app)
    }

    /// Return to the free-form phase. The spec stays; Decide can run again after
    /// further prompt edits (each Decide appends to `accepted`).
    pub async fn reopen(&mut self) -> Result<SpecDesignState, String> {
        self.require_session()?;
        self.mode = DesignMode::Freeform;
        self.persist_session().await;
        info!(
            "[SpecDesign] '{}' reopened for free-form editing",
            self.project_name
        );
        Ok(self.state())
    }

    async fn call_llm(&self, prompt: String) -> Result<String, String> {
        (self.llm)(prompt)
            .await
            .map_err(|e| format!("LLM error: {e}"))
    }
}

/// Graph node subtypes for the persisted free-form design session.
pub const DS_SESSION: &str = "design_session";
pub const DS_TURN: &str = "design_turn";
pub const DS_SUMMARY: &str = "design_summary";
pub const DS_SPEC: &str = "design_spec";

fn mode_str(mode: DesignMode) -> &'static str {
    match mode {
        DesignMode::Freeform => "freeform",
        DesignMode::Decided => "decided",
    }
}

async fn merge_node(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    node: AttrNode,
) -> Result<AttrNode, String> {
    let (t, r) = tokio::sync::oneshot::channel();
    mg_tx
        .send(MemoryGraphMessage::MergeAttrNode { node, reply_to: t })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    r.await
        .map_err(|e| format!("merge reply lost: {e}"))?
        .map_err(|e| format!("merge failed: {e}"))
}

async fn query_nodes(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    node_type: Option<&str>,
    subtype: Option<&str>,
    name: Option<&str>,
    limit: u32,
) -> Result<Vec<AttrNode>, String> {
    let (t, r) = tokio::sync::oneshot::channel();
    mg_tx
        .send(MemoryGraphMessage::QueryAttrNodes {
            node_type: node_type.map(str::to_string),
            subtype: subtype.map(str::to_string),
            name: name.map(str::to_string),
            limit: Some(limit),
            reply_to: t,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    r.await
        .map_err(|e| format!("query reply lost: {e}"))?
        .map_err(|e| format!("query failed: {e}"))
}

/// Upsert one design-document node (summary or spec) for an artifact.
async fn persist_doc(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    name: &str,
    artifact: &DesignArtifact,
    subtype: &str,
) {
    let now = chrono::Utc::now();
    let props: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::from([
            ("version".to_string(), serde_json::json!(artifact.version)),
            ("content".to_string(), serde_json::json!(artifact.content)),
        ]);
    let node = AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: super::spec_persist::MG_NODE_TYPE.to_string(),
        subtype: Some(subtype.to_string()),
        name: name.to_string(),
        description: None,
        properties: props,
        embedding_id: None,
        created_at: now,
        updated_at: now,
        version: 1,
    };
    if let Err(e) = merge_node(mg_tx, node).await {
        warn!("[SpecDesign] document node persist failed: {e}");
    }
}

/// Read a design-document node back as an artifact.
async fn load_doc(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    name: &str,
    subtype: &str,
) -> Option<DesignArtifact> {
    let nodes = query_nodes(
        mg_tx,
        Some(super::spec_persist::MG_NODE_TYPE),
        Some(subtype),
        Some(name),
        1,
    )
    .await
    .ok()?;
    let node = nodes.into_iter().next()?;
    Some(DesignArtifact {
        version: node.u32_prop("version").unwrap_or(1) as u32,
        content: node.str_prop("content").unwrap_or_default(),
        source_turns: Vec::new(),
        produced_at: chrono::Utc::now(),
    })
}

/// Keywords in a summarize instruction that ask for a full-material rewrite
/// rather than a fold of the new turns only.
fn wants_full_material(instruction: &str) -> bool {
    let lower = instruction.to_lowercase();
    [
        "recreate",
        "regenerate",
        "rewrite",
        "start over",
        "from scratch",
        "from the beginning",
        "fresh",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

fn format_turns(turns: &[DesignTurn]) -> String {
    if turns.is_empty() {
        "(no new conversation turns since the summary was last written)".to_string()
    } else {
        turns
            .iter()
            .map(|t| format!("[{}] {}", t.role, t.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Free-form summarize prompt: folds the delta into the running summary unless
/// the instruction asks for a full-material rewrite. NO statuses, NO "decided"
/// markers — the summary is prose; nothing is decided until the Decide button.
fn summarize_prompt(
    project_name: &str,
    goal: &str,
    instruction: &str,
    existing_summary: Option<&str>,
    material: &[DesignTurn],
) -> String {
    let existing = existing_summary.unwrap_or("none yet");
    format!(
        "# Summarize the design conversation\n\nProject: {project_name}\nGoal: {goal}\n\n\
         ## The user's instruction\n{instruction}\n\n\
         ## Current summary (the running source of truth)\n{existing}\n\n\
         ## Conversation turns to fold in\n{}\n\n\
         Follow the instruction above exactly. If it asks to add to or update the\n\
         existing summary, keep every point that still stands and fold in only the\n\
         new information from the conversation turns. If it asks to recreate or\n\
         summarize with a specific lens (techniques, protocols, data format, ...),\n\
         rewrite the summary from the provided material, focused on that lens.\n\
         The summary is freeform prose — any structure you like. No status labels,\n\
         no \"decided\" markers, no forced sections. Nothing is decided yet.\n\
         Do not invent facts; only reflect what was actually discussed.\n\
         Output only the summary text.",
        format_turns(material)
    )
}

/// Free-form promote prompt: compiles the (mature) summary onto the strict
/// spec_md grammar, folding the existing spec forward.
fn compile_spec_prompt(
    project_name: &str,
    goal: &str,
    instruction: &str,
    summary: &str,
    existing_spec: Option<&str>,
    recent_context: &[DesignTurn],
) -> String {
    let existing = existing_spec.unwrap_or("none yet");
    format!(
        "# Turn the design summary into a spec\n\nProject: {project_name}\nGoal: {goal}\n\n\
         ## The user's instruction\n{instruction}\n\n\
         ## Design summary (the running source of truth)\n{summary}\n\n\
         ## Recent conversation (context not yet folded into the summary)\n{}\n\n\
         ## Current spec (fold it forward — keep every section the summary does\n\
         not change)\n{existing}\n\n\
         SPEC GRAMMAR (strict, parseable 1:1):\n\
         # AppSpec: <name>\n\
         **Goal**: <goal>\n\
         ## Data types — records as `| field | type |` tables; enums as `a | b | c`\n\
         ## Graph — ### nodes (`- **name** — desc` then `  - field: type`); ### edges\n\
         ## Backend — ### `Actor` — desc; Handlers: ...; State: ...; Uses: ...\n\
         ## Bridge — ### `method` — desc; `| param | type |` table; Result: <type>\n\
         ## UI — ### id — title; Layout: vstack/hstack/list/text/button(\"x\")->a/\n\
         input(\"p\")@b/spacer/empty; Actions: ...; Bindings: ...; Navigation: ...\n\
         Type expressions: str | int | float | bool | name | list<T> | T? |\n\
         record(f:T;...)  Layout strings are a single line.\n\n\
         Only turn what the summary actually decided into spec content. If the\n\
         summary lacks information a section needs, leave that section out — never\n\
         invent bridge methods, types, or screens. Handlers listed in Backend and\n\
         methods called by UI actions must each exist in ## Bridge for the spec to\n\
         validate.\n\
         Output ONLY the spec markdown.",
        format_turns(recent_context)
    )
}

/// Free-form brainstorm prompt: an opinionated design partner that offers
/// options + trade-offs + a recommendation and ends with one pointed question.
/// Grounded in the running summary (if any) and the recent conversation.
fn brainstorm_prompt(
    project_name: &str,
    goal: &str,
    question: &str,
    summary: Option<&str>,
    context: &[DesignTurn],
) -> String {
    let summary = summary.unwrap_or("(no summary yet — the brainstorm is still open)");
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("Project: {project_name}"));
    parts.push(format!("Goal: {goal}"));
    parts.push(String::new());
    parts.push("## The user's question".to_string());
    parts.push(question.to_string());
    parts.push(String::new());
    parts.push("## Running summary (source of truth so far)".to_string());
    parts.push(summary.to_string());
    parts.push(String::new());
    parts.push("## Recent conversation".to_string());
    parts.push(format_turns(context));
    parts.push(String::new());
    parts.push(
        "You are an opinionated design partner for this project. Answer the question".to_string(),
    );
    parts.push(
        "in the free-form brainstorm. When it involves a choice, offer 2-3 concrete".to_string(),
    );
    parts.push("options with trade-offs and a clear recommendation, then end with one".to_string());
    parts.push("pointed question so the user can react. Nothing is decided yet — no".to_string());
    parts.push("statuses, no forced structure. Do not invent facts outside the".to_string());
    parts.push("conversation.".to_string());
    parts.join("\n")
}

#[async_trait]
impl Actor for SpecDesignActor {
    type Message = SpecDesignMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            SpecDesignMessage::Start {
                project_name,
                goal,
                reset,
                reply_to,
            } => {
                let _ = reply_to.send(self.start_session(&project_name, &goal, reset).await);
            }
            SpecDesignMessage::Reply { text, reply_to } => {
                match self.append_turn(ROLE_USER, &text) {
                    Ok(state) => {
                        self.persist_session().await;
                        let _ = reply_to.send(Ok(state));
                    }
                    Err(e) => {
                        let _ = reply_to.send(Err(e));
                    }
                }
            }
            SpecDesignMessage::AppendTurn {
                role,
                text,
                reply_to,
            } => match self.append_turn(&role, &text) {
                Ok(state) => {
                    self.persist_session().await;
                    let _ = reply_to.send(Ok(state));
                }
                Err(e) => {
                    let _ = reply_to.send(Err(e));
                }
            },
            SpecDesignMessage::Summarize {
                instruction,
                reply_to,
            } => {
                let _ = reply_to.send(self.summarize(&instruction).await);
            }
            SpecDesignMessage::PromoteToSpec {
                instruction,
                reply_to,
            } => {
                let _ = reply_to.send(self.promote(&instruction).await);
            }
            SpecDesignMessage::Ask { text, reply_to } => {
                let _ = reply_to.send(self.ask(&text).await);
            }
            SpecDesignMessage::Decide { reply_to } => {
                let _ = reply_to.send(self.decide().await);
            }
            SpecDesignMessage::Reopen { reply_to } => {
                let _ = reply_to.send(self.reopen().await);
            }
            SpecDesignMessage::GetState { reply_to } => {
                let _ = reply_to.send(self.state());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::super::spec_gen::example_gis_spec;
    use super::super::spec_md;

    /// Canned LLM: returns queued responses (last one repeats) and records
    /// every prompt for assertions.
    fn canned(responses: Vec<String>) -> (LlmCall, Arc<Mutex<Vec<String>>>) {
        let prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let prompts_for_call = prompts.clone();
        let queue: Arc<Mutex<VecDeque<String>>> =
            Arc::new(Mutex::new(responses.into_iter().collect()));
        let queue_for_call = queue.clone();
        let call: LlmCall = Box::new(move |prompt: String| {
            let prompts = prompts_for_call.clone();
            let queue = queue_for_call.clone();
            Box::pin(async move {
                prompts.lock().unwrap().push(prompt);
                let mut q = queue.lock().unwrap();
                let next = q.pop_front().or_else(|| q.back().cloned());
                Ok(next.unwrap_or_default())
            })
        });
        (call, prompts)
    }

    fn actor(responses: Vec<String>) -> (SpecDesignActor, Arc<Mutex<Vec<String>>>) {
        let (llm, prompts) = canned(responses);
        (SpecDesignActor::new(llm), prompts)
    }

    fn example_spec_md() -> String {
        spec_md::spec_to_markdown(&example_gis_spec())
    }

    fn seed_conversation(a: &mut SpecDesignActor) {
        a.append_turn(ROLE_USER, "what is the best canonical GIS format?")
            .unwrap();
        a.append_turn(
            ROLE_ASSISTANT,
            "store features as WKB in SeleneDB; serve GeoJSON at the bridge",
        )
        .unwrap();
    }

    #[test]
    fn transcript_grows_and_state_tracks_turns() {
        let (mut a, _) = actor(vec![]);
        let s = a.start("spire-gis", "view and edit map layers").unwrap();
        assert_eq!(s.mode, DesignMode::Freeform);
        assert_eq!(s.turn_count, 0);
        seed_conversation(&mut a);
        let s = a.state();
        assert_eq!(s.turn_count, 2);
        assert!(
            a.append_turn(ROLE_USER, "  ").is_err(),
            "blank turn rejected"
        );
    }

    #[test]
    fn summarize_folds_only_the_delta_and_bumps_versions() {
        let (mut a, prompts) = actor(vec![
            "v1: store WKB, serve GeoJSON".to_string(),
            "v2: v1 + Parquet for analytics".to_string(),
        ]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);

        let s1 = pollster(a.summarize("summarize with storage techniques")).unwrap();
        assert_eq!(s1.version, 1);
        assert_eq!(s1.content, "v1: store WKB, serve GeoJSON");
        assert_eq!(s1.source_turns, vec![0, 1]);
        let p1 = prompts.lock().unwrap()[0].clone();
        assert!(p1.contains("summarize with storage techniques"));
        assert!(p1.contains("none yet"));
        assert!(p1.contains("[user] what is the best canonical GIS format?"));

        a.append_turn(ROLE_USER, "actually, also evaluate Parquet for analytics")
            .unwrap();
        let s2 = pollster(a.summarize("add the new findings to the summary")).unwrap();
        assert_eq!(s2.version, 2);
        let p2 = prompts.lock().unwrap()[1].clone();
        // Fold: previous summary is present; only the delta turn is material.
        assert!(p2.contains("v1: store WKB, serve GeoJSON"));
        assert!(p2.contains("actually, also evaluate Parquet"));
        assert!(
            !p2.contains("what is the best canonical GIS format?"),
            "add-to must fold only the delta, not the whole transcript"
        );
    }

    #[test]
    fn summarize_recreate_passes_the_full_transcript() {
        let (mut a, prompts) = actor(vec!["fresh summary".to_string()]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let s = pollster(a.summarize("recreate the summary from scratch around storage")).unwrap();
        assert_eq!(s.version, 1);
        let p = prompts.lock().unwrap()[0].clone();
        assert!(
            p.contains("what is the best canonical GIS format?"),
            "recreate must see the full transcript"
        );
    }

    #[test]
    fn promote_auto_summarizes_then_compiles_the_spec() {
        let (mut a, prompts) = actor(vec![
            "decision: WKB in SeleneDB, GeoJSON served".to_string(),
            example_spec_md(),
        ]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let spec = pollster(a.promote("turn this into a spec")).unwrap();
        assert_eq!(spec.version, 1);
        assert_eq!(spec.content, example_spec_md());
        let ps = prompts.lock().unwrap();
        assert_eq!(
            ps.len(),
            2,
            "promote auto-summarizes when no summary exists"
        );
        assert!(ps[1].contains("Project: spire-gis"));
        assert!(ps[1].contains("## Bridge"));
        assert!(ps[1].contains("SPEC GRAMMAR"));
        assert!(ps[1].contains("Current spec"));
        assert!(a.state().summary.is_some());
    }

    #[test]
    fn promote_folds_the_previous_spec_forward() {
        let (mut a, prompts) = actor(vec![
            "summary one".to_string(),
            example_spec_md(),
            "summary one + more".to_string(),
            example_spec_md(),
        ]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        pollster(a.promote("turn this into a spec")).unwrap();
        a.append_turn(ROLE_USER, "add an inspect screen for a selected feature")
            .unwrap();
        let spec2 = pollster(a.promote("update the spec with the new screen")).unwrap();
        assert_eq!(spec2.version, 2);
        let ps = prompts.lock().unwrap();
        assert_eq!(
            ps.len(),
            3,
            "auto-summarize (1) + compile (1) + second compile (1)"
        );
        assert!(
            ps[2].contains("## Current spec") && ps[2].contains("## UI"),
            "second promote carries the existing spec forward"
        );
    }

    #[test]
    fn decide_validates_freezes_and_records_an_accepted_version() {
        let (mut a, _) = actor(vec!["summary text".to_string(), example_spec_md()]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        pollster(a.summarize("summarize the design")).unwrap();
        let spec = pollster(a.promote("turn this into a spec")).unwrap();
        assert!(spec.content.contains("## Bridge"));

        let app = pollster(a.decide()).expect("decide succeeds on a valid spec");
        let expected = spec_md::markdown_to_spec(&example_spec_md()).unwrap();
        assert_eq!(app, expected);
        assert!(app.is_valid());
        let s = a.state();
        assert_eq!(s.mode, DesignMode::Decided);
        assert_eq!(s.accepted.len(), 1);
        assert_eq!(s.accepted[0].version, 1);
        assert!(s.accepted[0].issues.is_empty());
        assert!(s.latest.is_some());

        // Free-form is frozen: prompts are rejected until Reopen.
        assert!(a.append_turn(ROLE_USER, "more ideas").is_err());
        let err = pollster(a.summarize("add")).unwrap_err();
        assert!(err.contains("already decided"));
        assert!(pollster(a.decide()).is_err());
    }

    #[test]
    fn reopen_resumes_editing_and_a_second_decide_appends_a_version() {
        let (mut a, _) = actor(vec![
            "summary text".to_string(),
            example_spec_md(),
            "summary text v2".to_string(),
            example_spec_md(),
        ]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        pollster(a.summarize("summarize")).unwrap();
        pollster(a.promote("to spec")).unwrap();
        let v1 = pollster(a.decide()).unwrap();
        assert_eq!(v1.bridge.len(), 2);

        pollster(a.reopen()).unwrap();
        assert_eq!(a.state().mode, DesignMode::Freeform);
        a.append_turn(
            ROLE_USER,
            "the generated spec missed the ingest actor — fix that",
        )
        .unwrap();
        pollster(a.summarize("add this to the summary")).unwrap();
        pollster(a.promote("regenerate the spec with the fix")).unwrap();
        let v2 = pollster(a.decide()).expect("second decide succeeds");
        assert_eq!(v2, v1, "re-deciding the same doc yields the same appspec");
        let s = a.state();
        assert_eq!(s.accepted.len(), 2);
        assert_eq!(s.accepted[1].version, 2);
    }

    #[test]
    fn decide_requires_a_promoted_spec() {
        let (mut a, _) = actor(vec![]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let err = pollster(a.decide()).unwrap_err();
        assert!(err.contains("promote the summary to a spec first"));
        assert_eq!(a.state().mode, DesignMode::Freeform);
    }

    #[test]
    fn decide_rejects_an_invalid_spec_and_stays_freeform() {
        // A spec whose UI action calls an undefined bridge method.
        let mut bad = example_gis_spec();
        bad.ui[0].actions.push(super::super::spec::UiAction {
            id: "boom".into(),
            description: String::new(),
            bridge: "map/doesNotExist".into(),
        });
        let bad_md = spec_md::spec_to_markdown(&bad);
        let (mut a, _) = actor(vec!["summary".to_string(), bad_md]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        pollster(a.summarize("summarize")).unwrap();
        pollster(a.promote("to spec")).unwrap();
        let err = pollster(a.decide()).unwrap_err();
        assert!(err.contains("does not validate"), "{err}");
        assert!(err.contains("map/doesNotExist"));
        assert_eq!(
            a.state().mode,
            DesignMode::Freeform,
            "invalid spec never freezes"
        );
        assert!(a.state().accepted.is_empty());
        assert!(
            a.state().spec.is_some(),
            "the spec doc survives a failed decide"
        );
    }

    /// Deterministic tail: an actor with a memory graph persists the decided
    /// decomposition (merge + relationship messages flow to the graph double).
    #[test]
    fn decide_persists_the_decomposition_when_a_graph_is_wired() {
        use spire_core::models::memory_graph::{GraphEdge, RelationshipType};
        use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage as MG;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (mg_tx, mut mg_rx) = tokio::sync::mpsc::channel::<MG>(32);
        let merged: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let rels: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let m1 = merged.clone();
        let r1 = rels.clone();
        rt.spawn(async move {
            while let Some(msg) = mg_rx.recv().await {
                match msg {
                    MG::MergeAttrNode { node, reply_to } => {
                        m1.lock()
                            .unwrap()
                            .push(node.subtype.clone().unwrap_or_default());
                        let _ = reply_to.send(Ok(node));
                    }
                    MG::CreateRelationship { rel, reply_to } => {
                        let edge = GraphEdge {
                            id: uuid::Uuid::new_v4().to_string(),
                            edge_type: rel.edge_type.clone(),
                            from_id: rel.from_id.clone(),
                            to_id: rel.to_id.clone(),
                            properties: rel.properties.clone().unwrap_or_default(),
                            created_at: chrono::Utc::now(),
                            weight: None,
                        };
                        if let RelationshipType::Custom(p) = &edge.edge_type {
                            r1.lock().unwrap().push(p.clone());
                        }
                        let _ = reply_to.send(Ok(edge));
                    }
                    _ => {}
                }
            }
        });

        let (mut a, _) = actor(vec!["summary".to_string(), example_spec_md()]);
        a.set_memory_graph(mg_tx);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        rt.block_on(async {
            a.summarize("summarize").await.unwrap();
            a.promote("to spec").await.unwrap();
            a.decide().await.expect("decide persists best-effort");
        });
        let m = merged.lock().unwrap();
        assert!(m.iter().any(|s| s == "appspec"));
        assert!(m.iter().any(|s| s == "spec_method"));
        let r = rels.lock().unwrap();
        assert!(r.iter().any(|p| p == "HAS_ACTOR"));
        assert!(r.iter().any(|p| p == "HAS_METHOD"));
    }

    #[test]
    fn ask_answers_inside_the_session_and_grounds_on_the_summary() {
        let (mut a, prompts) = actor(vec![
            "store WKB in SeleneDB; serve GeoJSON at the bridge".to_string(),
            "storage summary".to_string(),
            "use crate geo for spatial ops".to_string(),
        ]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        // Ask before any summary exists.
        let (answer1, state1) = pollster(a.ask("what is the best canonical GIS format?")).unwrap();
        assert_eq!(
            answer1,
            "store WKB in SeleneDB; serve GeoJSON at the bridge"
        );
        assert_eq!(state1.turn_count, 4); // 2 seeded + question + answer
        let p1 = prompts.lock().unwrap()[0].clone();
        assert!(p1.contains("Project: spire-gis"));
        assert!(p1.contains("The user's question"));
        assert!(p1.contains("what is the best canonical GIS format?"));
        assert!(p1.contains("no summary yet"));

        // Fold the brainstorm into a summary, then ask again — the summary is
        // now in the prompt context, and the assistant answer is captured.
        let s = pollster(a.summarize("summarize the storage decision")).unwrap();
        assert_eq!(s.version, 1);
        let (answer2, state2) = pollster(a.ask("which rust crate for spatial ops?")).unwrap();
        assert_eq!(answer2, "use crate geo for spatial ops");
        let p2 = prompts.lock().unwrap()[2].clone();
        assert!(
            p2.contains("store WKB in SeleneDB; serve GeoJSON at the bridge"),
            "ask must ground on the running summary"
        );
        assert_eq!(
            state2.turn_count, 6,
            "question + answer appended to the transcript"
        );
    }

    #[test]
    fn ask_is_rejected_after_decide_and_user_turns_survive_llm_failure() {
        // Canned LLM that fails on the brainstorm call.
        let llm: LlmCall = Box::new(|_| Box::pin(async move { Err("boom".to_string()) }));
        let mut a = SpecDesignActor::new(llm);
        a.start("spire-gis", "view and edit map layers").unwrap();
        let err = pollster(a.ask("question")).unwrap_err();
        assert!(err.contains("boom"));
        // The user's question was captured even though the LLM failed.
        assert_eq!(a.state().turn_count, 1);
    }

    /// Minimal memory-graph double: MergeAttrNode (dedupe by key) + QueryAttrNodes.
    struct FakeGraph {
        nodes: Arc<Mutex<Vec<AttrNode>>>,
    }

    impl FakeGraph {
        fn spawn(&self, mut rx: mpsc::Receiver<MemoryGraphMessage>) {
            let nodes = self.nodes.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("fake graph runtime");
                rt.block_on(async move {
                    while let Some(msg) = rx.recv().await {
                        match msg {
                            MemoryGraphMessage::MergeAttrNode { node, reply_to } => {
                                let mut list = nodes.lock().unwrap();
                                let key = |n: &AttrNode| {
                                    (n.node_type.clone(), n.subtype.clone(), n.name.clone())
                                };
                                match list.iter().position(|n| key(n) == key(&node)) {
                                    // Real Merge reuses the stored id and upserts content.
                                    Some(i) => {
                                        let id = list[i].id.clone();
                                        let merged = AttrNode { id, ..node };
                                        list[i] = merged.clone();
                                        let _ = reply_to.send(Ok(merged));
                                    }
                                    None => {
                                        list.push(node.clone());
                                        let _ = reply_to.send(Ok(node));
                                    }
                                }
                                drop(list);
                            }
                            MemoryGraphMessage::QueryAttrNodes {
                                node_type,
                                subtype,
                                name,
                                limit,
                                reply_to,
                            } => {
                                let out: Vec<AttrNode> = nodes
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .filter(|n| {
                                        node_type.as_deref().is_none_or(|t| n.node_type == t)
                                    })
                                    .filter(|n| {
                                        subtype
                                            .as_deref()
                                            .is_none_or(|s| n.subtype.as_deref() == Some(s))
                                    })
                                    .filter(|n| name.as_deref().is_none_or(|x| n.name == x))
                                    .take(limit.unwrap_or(u32::MAX) as usize)
                                    .cloned()
                                    .collect();
                                let _ = reply_to.send(Ok(out));
                            }
                            _ => {}
                        }
                    }
                });
            });
        }
    }

    fn fake_graph_pair() -> (mpsc::Sender<MemoryGraphMessage>, FakeGraph) {
        let (tx, rx) = mpsc::channel(64);
        let fake = FakeGraph {
            nodes: Arc::new(Mutex::new(Vec::new())),
        };
        fake.spawn(rx);
        (tx, fake)
    }

    #[test]
    fn design_session_roundtrips_through_the_project_graph() {
        let (tx, fake) = fake_graph_pair();
        let (mut a, _) = actor(vec![
            "v1: store WKB, serve GeoJSON".to_string(),
            example_spec_md(),
        ]);
        a.set_memory_graph(tx.clone());
        pollster(a.start_session("spire-gis", "view and edit map layers", false)).unwrap();
        seed_conversation(&mut a);
        pollster(a.summarize("summarize the design")).unwrap();
        let promoted = pollster(a.promote("to spec")).unwrap();
        assert_eq!(promoted.version, 1);

        let stored = fake.nodes.lock().unwrap();
        assert!(stored
            .iter()
            .any(|n| n.subtype.as_deref() == Some(DS_SESSION)));
        assert!(stored.iter().any(|n| n.subtype.as_deref() == Some(DS_TURN)));
        assert!(stored
            .iter()
            .any(|n| n.subtype.as_deref() == Some(DS_SUMMARY)));
        assert!(stored.iter().any(|n| n.subtype.as_deref() == Some(DS_SPEC)));
        drop(stored);

        // A brand-new actor resumes the brainstorm from the same project graph.
        let (mut b, _) = actor(vec![]);
        b.set_memory_graph(tx.clone());
        let state =
            pollster(b.start_session("spire-gis", "view and edit map layers", false)).unwrap();
        assert_eq!(state.turn_count, 2, "transcript resumed");
        assert_eq!(
            state.summary.as_ref().unwrap().content,
            "v1: store WKB, serve GeoJSON"
        );
        assert_eq!(state.spec.as_ref().unwrap().content, example_spec_md());
        assert_eq!(state.spec.as_ref().unwrap().version, 1);
        assert_eq!(state.mode, DesignMode::Freeform);

        // reset: true starts clean (new instance) — stale nodes stay but are
        // unreachable; a later resume sees the empty session.
        let (mut c, _) = actor(vec![]);
        c.set_memory_graph(tx.clone());
        let state2 = pollster(c.start_session("spire-gis", "fresh goal", true)).unwrap();
        assert_eq!(state2.turn_count, 0);
        assert!(state2.summary.is_none());
        let state3 = pollster(c.start_session("spire-gis", "again", false)).unwrap();
        assert_eq!(state3.turn_count, 0, "resume after reset is empty");
        let _ = tx;
    }

    #[test]
    fn two_projects_keep_independent_sessions_in_the_graph() {
        let (tx, _fake) = fake_graph_pair();
        let (mut a, _) = actor(vec!["alpha summary".to_string()]);
        a.set_memory_graph(tx.clone());
        pollster(a.start_session("spire-alpha", "alpha goal", false)).unwrap();
        a.append_turn(ROLE_USER, "alpha brainstorm").unwrap();
        pollster(a.summarize("summarize")).unwrap();

        let (mut b, _) = actor(vec!["beta summary".to_string()]);
        b.set_memory_graph(tx.clone());
        pollster(b.start_session("spire-beta", "beta goal", false)).unwrap();
        b.append_turn(ROLE_USER, "beta brainstorm").unwrap();
        pollster(b.summarize("summarize")).unwrap();

        let (mut c, _) = actor(vec![]);
        c.set_memory_graph(tx);
        let sa = pollster(c.start_session("spire-alpha", "ignored", false)).unwrap();
        assert_eq!(sa.turn_count, 1);
        assert!(sa.summary.as_ref().unwrap().content.contains("alpha"));
        let sb = pollster(c.start_session("spire-beta", "ignored", false)).unwrap();
        assert_eq!(sb.turn_count, 1);
        assert!(sb.summary.as_ref().unwrap().content.contains("beta"));
    }

    /// Block on a future from a sync test (no global runtime needed).
    fn pollster<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }
}
