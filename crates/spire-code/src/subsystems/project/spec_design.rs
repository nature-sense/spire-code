// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Spec design actor** — a single free-form conversation that drives the app
//! to an AppSpec.
//!
//! Interaction model (agreed with the product):
//!
//! - The chat transcript accumulates user + assistant turns. The assistant is a
//!   decisive design partner: it proposes one recommended solution, asks a
//!   question only when a choice genuinely matters, and does not loop on
//!   settled ground.
//! - When the model considers the design complete it calls the `submit_appspec`
//!   tool with the full `spec.md`. The coordinator surfaces that hand-off to the
//!   actor as `DesignReply.spec_md`; the actor validates via `markdown_to_spec`
//!   → `validate` and, when valid, runs the deterministic tail: persist the
//!   derivation into the memory graph and mark the session `Decided`. Every
//!   accepted submission is recorded in `accepted`; **Reopen** returns to the
//!   free-form phase so the design can be revised and re-submitted.
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

/// The injected design-model reply: the assistant's free-form text plus an
/// optional AppSpec hand-off (`spec.md` in the strict grammar) the model
/// produced via the `submit_appspec` tool when it considers the design
/// complete.
#[derive(Debug, Clone, Default)]
pub struct DesignReply {
    pub text: String,
    pub spec_md: Option<String>,
}

/// An injected LLM call: a design prompt → a [`DesignReply`].
pub type LlmCall = Box<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<DesignReply, String>> + Send>> + Send + Sync,
>;

/// An injected grounding lookup (docs RAG or web search): query + top_k →
/// formatted result lines. Testable with canned closures; the coordinator
/// supplies the real implementations.
pub type GroundingFn =
    Box<dyn Fn(&str, usize) -> Pin<Box<dyn Future<Output = Vec<String>> + Send>> + Send + Sync>;

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
    /// A user turn appended directly to the transcript (RPC mirror).
    Reply {
        text: String,
        reply_to: oneshot::Sender<Result<SpecDesignState, String>>,
    },
    /// Mirror any other turn (e.g. the assistant's chat reply) into the
    /// transcript so the whole conversation is captured.
    AppendTurn {
        role: String,
        text: String,
        reply_to: oneshot::Sender<Result<SpecDesignState, String>>,
    },
    /// Free-form brainstorm turn INSIDE the design session: appends the user
    /// question, asks the LLM (decisive recommendation, grounded in recent
    /// turns + any requested docs/web references), appends the answer, and
    /// replies with both. The model may finish by submitting the AppSpec via
    /// the `submit_appspec` tool (surfaced as `DesignReply.spec_md`).
    Ask {
        text: String,
        /// Ground the answer in the Spire docs RAG (spire-actor/spire-core).
        docs: bool,
        /// Ground the answer in a web search.
        web: bool,
        reply_to: oneshot::Sender<Result<(String, SpecDesignState), String>>,
    },
    /// Return to the free-form phase after a submit (Revise).
    Reopen {
        reply_to: oneshot::Sender<Result<SpecDesignState, String>>,
    },
    GetState {
        reply_to: oneshot::Sender<SpecDesignState>,
    },
}

/// Free-form design session. Created per project (the wizard starts one when it
/// opens the design step); holds the chat transcript + the submitted AppSpecs.
pub struct SpecDesignActor {
    project_name: String,
    goal: String,
    llm: LlmCall,
    memory_graph_tx: Option<mpsc::Sender<MemoryGraphMessage>>,
    mode: DesignMode,
    turns: Vec<DesignTurn>,
    accepted: Vec<AcceptedSpec>,
    latest: Option<AppSpec>,
    last_issues: Vec<SpecIssue>,
    /// Session-instance token: child nodes (turns/documents) are named under it
    /// so a fresh `start(reset: true)` simply switches instance instead of
    /// deleting stale nodes. Set at session creation/reset.
    instance: String,
    /// How many turns are already persisted to the graph (append-only writes).
    persisted_through: usize,
    /// Optional grounding lookups used only when the Ask flags request them.
    rag_search: Option<GroundingFn>,
    web_search: Option<GroundingFn>,
}

impl SpecDesignActor {
    /// A fresh, unconfigured session. The RPC entry always calls [`Self::start`]
    /// first, which fixes project + goal; submit/reopen guard on that.
    pub fn new(llm: LlmCall) -> Self {
        Self {
            project_name: String::new(),
            goal: String::new(),
            llm,
            memory_graph_tx: None,
            mode: DesignMode::Freeform,
            turns: Vec::new(),
            accepted: Vec::new(),
            latest: None,
            last_issues: Vec::new(),
            instance: String::new(),
            persisted_through: 0,
            rag_search: None,
            web_search: None,
        }
    }

    /// Wire the memory graph so a submitted AppSpec can be persisted.
    pub fn set_memory_graph(&mut self, tx: mpsc::Sender<MemoryGraphMessage>) {
        self.memory_graph_tx = Some(tx);
    }

    /// Optional docs-RAG grounding (Spire docs domain) for Ask turns.
    pub fn set_rag_search(&mut self, f: GroundingFn) {
        self.rag_search = Some(f);
    }

    /// Optional web-search grounding for Ask turns.
    pub fn set_web_search(&mut self, f: GroundingFn) {
        self.web_search = Some(f);
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

    /// Persist the free-form session into the project memory graph so a
    /// brainstorm survives restarts and lives beside the decided spec. Written
    /// as plain typed nodes (no whole-session blob): a `design_session` node and
    /// one `design_turn` node per new turn. Best-effort — a missing graph keeps
    /// the session in-memory.
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

    /// Free-form brainstorm answer inside the design session (see the `Ask`
    /// message). Appends the user turn FIRST (capture survives an LLM error),
    /// then asks the LLM grounded in the recent turns
    /// appends the assistant answer, and returns (answer, state).
    pub async fn ask(
        &mut self,
        text: &str,
        docs: bool,
        web: bool,
    ) -> Result<(String, SpecDesignState), String> {
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
        self.persist_session().await;
        let start = self.turns.len().saturating_sub(10);
        let context = self.turns[start..].to_vec();
        // Optional grounding: docs RAG and/or web search, requested explicitly.
        let mut grounding: Vec<String> = Vec::new();
        if docs {
            if let Some(f) = &self.rag_search {
                let hits = f(&text, 4).await;
                if !hits.is_empty() {
                    let mut block = "## Spire architecture (docs)".to_string();
                    for h in hits {
                        block.push('\n');
                        block.push_str(&h);
                    }
                    grounding.push(block);
                }
            }
        }
        if web {
            if let Some(f) = &self.web_search {
                let hits = f(&text, 4).await;
                if !hits.is_empty() {
                    let mut block = "## Web results".to_string();
                    for h in hits {
                        block.push('\n');
                        block.push_str(&h);
                    }
                    grounding.push(block);
                }
            }
        }
        let prompt = brainstorm_prompt(
            &self.project_name,
            &self.goal,
            &text,
            &context,
            &grounding,
        );
        let reply = self.call_llm(prompt).await?;
        let mut answer = reply.text.clone();
        // The model may finish the design by calling the `submit_appspec` tool.
        if let Some(submitted) = reply.spec_md.as_deref() {
            match self.accept_spec_markdown(submitted).await {
                Ok(app) => {
                    let line = format!(
                        "AppSpec submitted — v{} ({} types, {} actors, {} bridge methods, {} screens). The design is decided; press Revise to change it.",
                        self.accepted.len(),
                        app.types.len(),
                        app.actors.len(),
                        app.bridge.len(),
                        app.ui.len()
                    );
                    answer = if answer.trim().is_empty() {
                        line.clone()
                    } else {
                        format!("{answer}

{line}")
                    };
                }
                Err(e) => {
                    let line = format!("submit_appspec was rejected: {e}");
                    answer = if answer.trim().is_empty() {
                        line.clone()
                    } else {
                        format!("{answer}

{line}")
                    };
                }
            }
        }
        if !answer.trim().is_empty() {
            self.turns.push(DesignTurn {
                role: ROLE_ASSISTANT.to_string(),
                text: answer.clone(),
            });
        }
        self.persist_session().await;
        Ok((answer, self.state()))
    }

    /// Parse + validate a submitted spec.md (the chat `submit_appspec` hand-off)
    /// and, when valid, persist the graph derivation, mark the session decided,
    /// and record an accepted version.
    async fn accept_spec_markdown(&mut self, markdown: &str) -> Result<AppSpec, String> {
        self.require_freeform()?;
        self.require_session()?;
        let app: AppSpec = spec_md::markdown_to_spec(markdown)?;
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
                "spec for '{}' does not validate — {} error(s):\n{detail}\nReopen and refine via prompts, then submit again.",
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
                    "[SpecDesign] spec persisted without an anchor for '{}' (graph unavailable?)",
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
            "[SpecDesign] '{}' accepted — appspec v{} ({} types, {} actors, {} methods, {} screens)",
            self.project_name,
            self.accepted.len(),
            app.types.len(),
            app.actors.len(),
            app.bridge.len(),
            app.ui.len()
        );
        Ok(app)
    }

    /// Return to the free-form phase. The accepted spec stays recorded; the
    /// model can refine the design and submit again (each accepted submission
    /// appends to `accepted`).
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

    async fn call_llm(&self, prompt: String) -> Result<DesignReply, String> {
        (self.llm)(prompt)
            .await
            .map_err(|e| format!("LLM error: {e}"))
    }
}

/// Graph node subtypes for the persisted free-form design session.
pub const DS_SESSION: &str = "design_session";
pub const DS_TURN: &str = "design_turn";

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

fn format_turns(turns: &[DesignTurn]) -> String {
    if turns.is_empty() {
        "(no conversation turns yet)".to_string()
    } else {
        turns
            .iter()
            .map(|t| format!("[{}] {}", t.role, t.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Injected at the top of every design prompt so the model always designs
/// within the Spire stack instead of proposing arbitrary tech (Python/JS/web).
const SPIRE_APP_CONTEXT: &str = "SPIRE APP CONTEXT — always design within this.\n\
A Spire app is a native macOS application with a FIXED, non-negotiable stack:\n\
- UI: SwiftUI (macOS) — declarative screens.\n\
- Core: Rust, built on the actor pattern — isolated actors that own their state\n\
  and exchange typed messages over channels (no shared mutable state).\n\
- Integration: a JSON FFI bridge — actors expose named methods with typed\n\
  parameters and a typed result, called from SwiftUI.\n\
- Specification: the design is captured as a formal AppSpec with these sections:\n\
  * app    — project name + goal\n\
  * types  — shared records/enums referenced by actors, bridge and UI\n\
  * graph  — the memory-graph schema (nodes + edges) actors persist/query\n\
  * actors — the Rust side: each actor lists the bridge methods it handles\n\
  * bridge — the JSON method contract (method + params + result types)\n\
  * ui     — SwiftUI screens whose actions bind to bridge methods\n\
\n\
HARD CONSTRAINTS:\n\
- Design ONLY within the Spire stack: SwiftUI UI + Rust actors + the JSON\n\
  bridge + the AppSpec sections above.\n\
- Do NOT propose other languages or frameworks (Python, JavaScript, web/React,\n\
  Node, Flutter, Electron, or external servers/databases as the primary\n\
  backend).\n\
- The app's backend IS its own Rust actors; persistence is via the memory graph.\n\
- Project/crate naming convention: `spire-<name>`.";

/// Free-form brainstorm prompt: a decisive design partner that proposes ONE
/// recommended solution and asks only when a choice genuinely matters. Grounded
/// in the recent conversation and any requested docs/web references.
fn brainstorm_prompt(
    project_name: &str,
    goal: &str,
    question: &str,
    context: &[DesignTurn],
    grounding: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(SPIRE_APP_CONTEXT.to_string());
    parts.push(format!("Project: {project_name}"));
    parts.push(format!("Goal: {goal}"));
    parts.push(String::new());
    parts.push("## The user's question".to_string());
    parts.push(question.to_string());
    parts.push(String::new());
    parts.push("## Recent conversation".to_string());
    parts.push(format_turns(context));
    if !grounding.is_empty() {
        parts.push(String::new());
        for block in grounding {
            parts.push(block.clone());
        }
    }
    parts.push(String::new());
    parts.push(
        "You are a decisive design partner for a Spire app who knows the platform.".to_string(),
    );
    parts.push("Ground your answer in the supplied architecture/web references when present;".to_string());
    parts.push("otherwise answer from knowledge. Propose ONE concrete recommended solution".to_string());
    parts.push("by default — name the specific types, actors, bridge methods and screens, and".to_string());
    parts.push("reuse the platform primitives above. Keep answers short. Ask a question only".to_string());
    parts.push("when a choice is genuinely consequential and no sensible default exists;".to_string());
    parts.push("ask exactly one, and mark it as the single open question. Do not repeat".to_string());
    parts.push("settled ground or loop on one area. When the design is complete and no open".to_string());
    parts.push("questions remain, call the `submit_appspec` tool with the FULL spec.md in the".to_string());
    parts.push("strict grammar (Data types / Graph / Backend / Bridge / UI). Do NOT submit".to_string());
    parts.push("while questions are still open.".to_string());
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
            SpecDesignMessage::Ask {
                text,
                docs,
                web,
                reply_to,
            } => {
                let _ = reply_to.send(self.ask(&text, docs, web).await);
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
                Ok(DesignReply {
                    text: next.unwrap_or_default(),
                    spec_md: None,
                })
            })
        });
        (call, prompts)
    }

    #[test]
    fn submit_via_tool_marks_decided_and_persists() {
        let (mut a, _) = actor(vec![]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let md = example_spec_md();
        a.llm = Box::new(move |_p: String| {
            let md = md.clone();
            Box::pin(async move {
                Ok(DesignReply {
                    text: "final proposal".to_string(),
                    spec_md: Some(md),
                })
            })
        });
        let (answer, s) = pollster(a.ask("draft the AppSpec now", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Decided);
        assert_eq!(s.accepted.len(), 1);
        assert!(s.latest.is_some());
        assert!(answer.contains("AppSpec submitted"), "{answer}");
    }

    #[test]
    fn invalid_submission_stays_freeform() {
        let (mut a, _) = actor(vec![]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        a.llm = Box::new(move |_p: String| {
            Box::pin(async move {
                Ok(DesignReply {
                    text: String::new(),
                    spec_md: Some("# Not a spec at all\n\nrandom".to_string()),
                })
            })
        });
        let (answer, s) = pollster(a.ask("submit now", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Freeform);
        assert_eq!(s.accepted.len(), 0);
        assert!(answer.contains("rejected"), "{answer}");
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
    fn ask_answers_inside_the_session_and_appends_turns() {
        let (mut a, prompts) = actor(vec![
            "store WKB in SeleneDB; serve GeoJSON at the bridge".to_string(),
            "use crate geo for spatial ops".to_string(),
        ]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        // Ask before any grounding exists.
        let (answer1, state1) =
            pollster(a.ask("what is the best canonical GIS format?", false, false)).unwrap();
        assert_eq!(answer1, "store WKB in SeleneDB; serve GeoJSON at the bridge");
        assert_eq!(state1.turn_count, 4); // 2 seeded + question + answer
        let p1 = prompts.lock().unwrap()[0].clone();
        assert!(p1.contains("Project: spire-gis"));
        assert!(p1.contains("The user's question"));
        assert!(p1.contains("what is the best canonical GIS format?"));
        assert!(p1.contains("[user] what is the best canonical GIS format?"));

        // A second turn carries the conversation forward (recent-context window).
        let (answer2, state2) =
            pollster(a.ask("which rust crate for spatial ops?", false, false)).unwrap();
        assert_eq!(answer2, "use crate geo for spatial ops");
        assert_eq!(state2.turn_count, 6, "question + answer appended again");
        let p2 = prompts.lock().unwrap()[1].clone();
        assert!(
            p2.contains("which rust crate for spatial ops?"),
            "ask must include the latest question"
        );
    }

    /// The deterministic tail must persist the decomposition when a graph is
    /// wired — driven through the chat submit path (merge + relationship
    /// messages flow to the graph double).
    #[test]
    fn submit_persists_the_decomposition_when_a_graph_is_wired() {
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

        let (mut a, _) = actor(vec![]);
        a.set_memory_graph(mg_tx);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let md = example_spec_md();
        a.llm = Box::new(move |_p: String| {
            let md = md.clone();
            Box::pin(async move {
                Ok(DesignReply {
                    text: String::new(),
                    spec_md: Some(md),
                })
            })
        });
        rt.block_on(async {
            a.ask("submit", false, false)
                .await
                .expect("submit persists best-effort");
        });
        let m = merged.lock().unwrap();
        assert!(m.iter().any(|s| s == "appspec"));
        assert!(m.iter().any(|s| s == "spec_method"));
        let r = rels.lock().unwrap();
        assert!(r.iter().any(|p| p == "HAS_ACTOR"));
        assert!(r.iter().any(|p| p == "HAS_METHOD"));
    }

    #[test]
    fn ask_is_rejected_and_user_turns_survive_llm_failure() {
        // Canned LLM that fails on the brainstorm call.
        let llm: LlmCall = Box::new(|_| Box::pin(async move { Err("boom".to_string()) }));
        let mut a = SpecDesignActor::new(llm);
        a.start("spire-gis", "view and edit map layers").unwrap();
        let err = pollster(a.ask("question", false, false)).unwrap_err();
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
        let (mut a, _) = actor(vec![]);
        a.set_memory_graph(tx.clone());
        pollster(a.start_session("spire-gis", "view and edit map layers", false)).unwrap();
        seed_conversation(&mut a);
        // Submit the AppSpec through the chat tool.
        let md = example_spec_md();
        a.llm = Box::new(move |_p: String| {
            let md = md.clone();
            Box::pin(async move {
                Ok(DesignReply {
                    text: "final proposal".to_string(),
                    spec_md: Some(md),
                })
            })
        });
        let (_, submitted) = pollster(a.ask("draft the AppSpec now", false, false)).unwrap();
        assert_eq!(submitted.mode, DesignMode::Decided);

        let stored = fake.nodes.lock().unwrap();
        assert!(stored.iter().any(|n| n.subtype.as_deref() == Some(DS_SESSION)));
        assert!(stored.iter().any(|n| n.subtype.as_deref() == Some(DS_TURN)));
        drop(stored);

        // A brand-new actor resumes the decided session from the same graph.
        let (mut b, _) = actor(vec![]);
        b.set_memory_graph(tx.clone());
        let state =
            pollster(b.start_session("spire-gis", "view and edit map layers", false)).unwrap();
        assert_eq!(state.turn_count, 4, "transcript resumed (2 seeded + ask + answer)");
        assert_eq!(state.mode, DesignMode::Decided, "decided mode survives restart");

        // reset: true starts clean (new instance) — stale nodes stay but are
        // unreachable; a later resume sees the empty session.
        let (mut c, _) = actor(vec![]);
        c.set_memory_graph(tx.clone());
        let state2 = pollster(c.start_session("spire-gis", "fresh goal", true)).unwrap();
        assert_eq!(state2.turn_count, 0);
        assert_eq!(state2.mode, DesignMode::Freeform);
        let state3 = pollster(c.start_session("spire-gis", "again", false)).unwrap();
        assert_eq!(state3.turn_count, 0, "resume after reset is empty");
        let _ = tx;
    }

    #[test]
    fn two_projects_keep_independent_sessions_in_the_graph() {
        let (tx, _fake) = fake_graph_pair();
        let (mut a, _) = actor(vec![]);
        a.set_memory_graph(tx.clone());
        pollster(a.start_session("spire-alpha", "alpha goal", false)).unwrap();
        a.append_turn(ROLE_USER, "alpha brainstorm").unwrap();
        let md = example_spec_md();
        a.llm = Box::new(move |_p: String| {
            let md = md.clone();
            Box::pin(async move {
                Ok(DesignReply {
                    text: "alpha final".to_string(),
                    spec_md: Some(md),
                })
            })
        });
        pollster(a.ask("draft it", false, false)).unwrap();

        let (mut b, _) = actor(vec!["beta answer".to_string()]);
        b.set_memory_graph(tx.clone());
        pollster(b.start_session("spire-beta", "beta goal", false)).unwrap();
        b.append_turn(ROLE_USER, "beta brainstorm").unwrap();
        pollster(b.ask("what format?", false, false)).unwrap();

        // Each project resumes its own transcript + state.
        let (mut c, _) = actor(vec![]);
        c.set_memory_graph(tx);
        let sa = pollster(c.start_session("spire-alpha", "ignored", false)).unwrap();
        assert_eq!(sa.turn_count, 3); // brainstorm + ask question + answer
        assert_eq!(sa.mode, DesignMode::Decided);
        let sb = pollster(c.start_session("spire-beta", "ignored", false)).unwrap();
        assert_eq!(sb.turn_count, 3);
        assert_eq!(sb.mode, DesignMode::Freeform);
    }

    #[test]
    fn ask_grounds_on_docs_and_web_when_requested() {
        let (mut a, prompts) = actor(vec!["answer one".to_string(), "answer two".to_string()]);
        let rag: GroundingFn = Box::new(|_, _| {
            Box::pin(async move { vec!["- Actor trait: handle(&mut self, msg)".to_string()] })
        });
        let web: GroundingFn = Box::new(|_, _| {
            Box::pin(async move { vec!["- GeoJSON vs WKB (wikipedia)".to_string()] })
        });
        a.set_rag_search(rag);
        a.set_web_search(web);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);

        let (_, _) = pollster(a.ask("what is the best canonical GIS format?", true, true)).unwrap();
        let p = prompts.lock().unwrap()[0].clone();
        assert!(
            p.contains("## Spire architecture (docs)"),
            "docs grounding block"
        );
        assert!(p.contains("- Actor trait: handle(&mut self, msg)"));
        assert!(p.contains("## Web results"), "web grounding block");
        assert!(p.contains("- GeoJSON vs WKB (wikipedia)"));

        // No flag -> no grounding sections, and the freeform answer is produced.
        let (answer, _) = pollster(a.ask("summarize the storage idea", false, false)).unwrap();
        assert_eq!(answer, "answer two");
        let p2 = prompts.lock().unwrap()[1].clone();
        assert!(!p2.contains("## Web results"));
        assert!(!p2.contains("## Spire architecture (docs)"));
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


#[cfg(test)]
mod spire_context_tests {
    use super::*;

    #[test]
    fn design_prompts_carry_the_spire_app_context() {
        let b = brainstorm_prompt("spire-gis", "view and edit map layers", "what stack?", &[], &[]);
        assert!(b.contains("SPIRE APP CONTEXT"));
        assert!(b.contains("SwiftUI"));
        assert!(b.contains("actor pattern"));
        assert!(b.contains("Rust"));
    }
}
