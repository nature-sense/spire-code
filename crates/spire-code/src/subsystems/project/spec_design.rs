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

/// One open design question the model raised, with the answer it recommends so
/// the user can accept it (or pick an alternative) instead of typing an answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesignQuestion {
    /// Which AppSpec section the question belongs to: one of "types", "graph",
    /// "backend", "bridge" or "ui" ("" when unknown). Used to surface coverage.
    #[serde(default)]
    pub section: String,
    pub question: String,
    /// The answer the model recommends (what "Use recommendation" fills in).
    pub recommendation: String,
    /// Optional alternative answers the user can pick instead.
    #[serde(default)]
    pub options: Vec<String>,
}

/// The injected design-model reply: the assistant's free-form text plus the
/// optional tool hand-offs:
/// - `outline` — the model's current draft design outline (markdown, one
///   section per AppSpec section) via the `set_outline` tool, kept current every
///   turn so the session always shows the design skeleton;
/// - `open_questions` — the model's current open-question list (replace
///   semantics) maintained via the `set_open_questions` tool. Submission is
///   refused while the list is non-empty;
/// - `spec_md` — the full AppSpec (`spec.md` in the strict grammar) produced via
///   the `submit_appspec` tool when the model considers the design complete.
#[derive(Debug, Clone, Default)]
pub struct DesignReply {
    pub text: String,
    pub outline: Option<String>,
    pub spec_md: Option<String>,
    pub open_questions: Option<Vec<DesignQuestion>>,
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
    /// The model's current draft design outline (markdown, one section per
    /// AppSpec section), kept current via the `set_outline` tool.
    pub outline: Option<String>,
    /// Design questions the assistant raised that are still unanswered, each
    /// with a recommended answer the user can accept. The assistant maintains
    /// the list via the `set_open_questions` tool and must clear it before the
    /// AppSpec can be submitted.
    pub open_questions: Vec<DesignQuestion>,
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
    /// Current draft design outline (model-maintained via `set_outline`).
    outline: Option<String>,
    /// Current open design questions (question + recommended answer),
    /// model-maintained via `set_open_questions`.
    open_questions: Vec<DesignQuestion>,
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
            outline: None,
            open_questions: Vec::new(),
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
                ("outline", serde_json::json!(self.outline)),
                (
                    "open_questions",
                    serde_json::to_value(&self.open_questions)
                        .unwrap_or_else(|_| serde_json::json!([])),
                ),
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
        self.outline = session
            .properties
            .get("outline")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        self.open_questions = session
            .properties
            .get("open_questions")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

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
            outline: self.outline.clone(),
            open_questions: self.open_questions.clone(),
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
        self.outline = None;
        self.open_questions.clear();
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
            self.outline.as_deref(),
            &self.open_questions,
        );
        let reply = self.call_llm(prompt).await?;
        let model_text = reply.text.clone();
        let mut answer = model_text.clone();
        let outline_sent = reply.outline.is_some();
        let spec_sent = reply.spec_md.is_some();
        // The model keeps its draft outline current via the `set_outline` tool.
        if let Some(o) = reply.outline {
            let trimmed = o.trim().to_string();
            if !trimmed.is_empty() {
                self.outline = Some(trimmed);
            }
        }
        // The model keeps its open-question list current via the
        // `set_open_questions` tool (replace semantics: the full list each time,
        // [] once nothing remains unanswered). Surface the list in the chat so
        // the user sees what still blocks a submission.
        if let Some(open) = reply.open_questions {
            self.open_questions = open
                .into_iter()
                .map(|q| DesignQuestion {
                    section: q.section.trim().to_string(),
                    question: q.question.trim().to_string(),
                    recommendation: q.recommendation.trim().to_string(),
                    options: q
                        .options
                        .into_iter()
                        .map(|o| o.trim().to_string())
                        .filter(|o| !o.is_empty())
                        .collect(),
                })
                .filter(|q| !q.question.is_empty())
                .collect();
            if !self.open_questions.is_empty() {
                // Numbered list so MarkdownText renders each question on its own
                // line instead of one merged paragraph.
                let qs = self
                    .open_questions
                    .iter()
                    .enumerate()
                    .map(|(i, q)| format!("{}. {}", i + 1, q.question))
                    .collect::<Vec<_>>()
                    .join("
");
                let line = format!(
                    "Open questions still to resolve before the AppSpec can be submitted:
{qs}

Accept a recommended answer on the right, or answer in the chat."
                );
                answer = if answer.trim().is_empty() {
                    line.clone()
                } else {
                    format!("{answer}

{line}")
                };
            }
        }
        // The model may finish the design by calling the `submit_appspec` tool.
        if let Some(submitted) = reply.spec_md.as_deref() {
            if !self.open_questions.is_empty() {
                // Deterministic gate: never accept a submission while the model
                // still tracks open questions — surface them so the user can
                // answer and the model can re-submit.
                let qs = self
                    .open_questions
                    .iter()
                    .enumerate()
                    .map(|(i, q)| format!("{}. {}", i + 1, q.question))
                    .collect::<Vec<_>>()
                    .join("
");
                let line = format!(
                    "submit_appspec was rejected — {} open question(s) must be answered first:
{qs}

Answer each (or pick its recommended answer on the right), then ask it to submit again.",
                    self.open_questions.len()
                );
                answer = if answer.trim().is_empty() {
                    line.clone()
                } else {
                    format!("{answer}\n\n{line}")
                };
            } else {
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
                            format!("{answer}\n\n{line}")
                        };
                    }
                    Err(e) => {
                        let line = format!("submit_appspec was rejected: {e}");
                        answer = if answer.trim().is_empty() {
                            line.clone()
                        } else {
                            format!("{answer}\n\n{line}")
                        };
                    }
                }
            }
        }
        // Tool-only replies (e.g. just a concept update) carry no prose — never
        // return an empty answer that looks like the LLM failed. Drive progress:
        // point at the next open question or, once everything is settled, invite
        // a submission.
        if answer.trim().is_empty() {
            let mut lines: Vec<String> = Vec::new();
            if outline_sent {
                lines.push("Concept updated.".to_string());
            }
            if let Some(first) = self.open_questions.first() {
                let next = if first.recommendation.trim().is_empty() {
                    first.question.clone()
                } else {
                    format!("{} — I recommend: {}", first.question, first.recommendation)
                };
                lines.push(format!("Next up — 1. {next}"));
                if self.open_questions.len() > 1 {
                    let rest = self.open_questions[1..]
                        .iter()
                        .enumerate()
                        .map(|(i, q)| format!("{}. {}", i + 2, q.question))
                        .collect::<Vec<_>>()
                        .join("  ");
                    lines.push(format!("Also still to decide: {rest}"));
                }
                lines.push(
                    "Answer it here or accept the recommendation on the right.".to_string(),
                );
            } else {
                if lines.is_empty() {
                    lines.push("All questions are settled.".to_string());
                }
                lines.push(
                    "I'll prepare the AppSpec — say submit when you are ready.".to_string(),
                );
            }
            answer = lines.join(" ");
        }

        // Deterministic finalize: the model reliably updates the concept and
        // question list but rarely calls submit_appspec itself. When the user
        // asks to submit (or the model says it is submitting) and no spec_md
        // arrived, drive a focused submit pass so "continue" actually produces
        // the AppSpec. A user command overrides the question gate; a model claim
        // only counts once the open-question list is empty.
        if !spec_sent
            && (submit_intent(&text, self.open_questions.is_empty())
                || (self.open_questions.is_empty() && model_signals_submit(&model_text)))
        {
            let prompt = submit_spec_prompt(
                &self.project_name,
                &self.goal,
                self.outline.as_deref(),
            );
            let outcome = match self.call_llm(prompt).await {
                Ok(sr) => {
                    let candidate = sr.spec_md.or_else(|| {
                        if sr.text.trim().is_empty() {
                            None
                        } else {
                            Some(sr.text)
                        }
                    });
                    match candidate {
                        Some(md) => match self.accept_spec_markdown(&md).await {
                            Ok(app) => format!(
                                "AppSpec submitted — v{} ({} types, {} actors, {} bridge methods, {} screens). The design is decided; press Revise to change it.",
                                self.accepted.len(),
                                app.types.len(),
                                app.actors.len(),
                                app.bridge.len(),
                                app.ui.len()
                            ),
                            Err(e) => format!(
                                "submit did not go through — the spec did not validate:
{e}"
                            ),
                        },
                        None => "submit produced no spec — say submit again.".to_string(),
                    }
                }
                Err(e) => format!("submit pass failed: {e}"),
            };
            answer = outcome;
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

/// Names of the two tools the design model may call (kept in one place so the
/// coordinator advertises exactly what [`parse_design_reply`] understands).
pub const SUBMIT_APPSPEC_TOOL: &str = "submit_appspec";
pub const SET_OPEN_QUESTIONS_TOOL: &str = "set_open_questions";
pub const SET_OUTLINE_TOOL: &str = "set_outline";

/// The strict spec.md grammar the design model must produce for
/// `submit_appspec` (1:1 parseable by the AppSpec parser). Injected into the
/// tool schema so the model sees it exactly when it composes the submission.
pub const SPEC_MD_GRAMMAR: &str = r#"# AppSpec: <name>
**Goal**: <goal>

## Data types
### `Name` (record)
| field | type |
|-------|------|
| <field> | <type> |
### `Name` (enum)
A | B | C

## Graph
### nodes
- **NodeName** — <description>
  - <field>: <type>
### edges
| name | from | to | description |
|------|------|----|-------------|
| <edge> | <FromNode> | <ToNode> | <desc> |

## Backend
### `ActorName` — <description>
Handlers: <bridge method 1>, <bridge method 2>
State: <field>: <type>
Uses: <dependency>

## Bridge
### `method` — <description>
| param | type |
|-------|------|
| <param> | <type> |
Result: <type>

## UI
### <screenId> — Screen Title
Layout: vstack(text("..."), list(<item>), button("Save")->save, input("Name")@name, spacer)
Actions: <actionId>("label")-><bridgeMethod>   (or <actionId>-><bridgeMethod> when no label)
Bindings: <field><-<bridgeMethod>
Navigation: <actionId>-><screenId>

Type expressions: str | int | float | bool | <Name> | list<T> | T? | record(f:T;...)

Only include a section when the conversation actually decided it — never invent
content. Every UI action/binding/layout button must reference a ## Bridge method
and every bridge method must appear in exactly one actor's Handlers."#;

/// Parse the raw LLM reply (plain text, or the native/synthetic JSON assistant
/// message with `tool_calls`) into a [`DesignReply`]. Understands the
/// `submit_appspec` and `set_open_questions` tools.
pub fn parse_design_reply(raw: &str) -> DesignReply {
    let mut reply = DesignReply::default();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        // Normal text reply (no tool calls): the whole payload is the answer.
        reply.text = raw.to_string();
        return reply;
    };
    if let Some(c) = v["content"].as_str() {
        reply.text = c.to_string();
    }
    let Some(calls) = v["tool_calls"].as_array() else {
        return reply;
    };
    for tc in calls {
        let Some(name) = tc["function"]["name"].as_str() else {
            continue;
        };
        let Some(args) = tc["function"]["arguments"].as_str().and_then(|a| {
            serde_json::from_str::<serde_json::Value>(a).ok()
        }) else {
            continue;
        };
        match name {
            SUBMIT_APPSPEC_TOOL => {
                reply.spec_md = args
                    .get("spec_md")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
            }
            SET_OUTLINE_TOOL => {
                reply.outline = args
                    .get("outline_md")
                    .and_then(|m| m.as_str())
                    .map(str::to_string);
            }
            SET_OPEN_QUESTIONS_TOOL => {
                reply.open_questions = args.get("questions").and_then(|q| q.as_array()).map(
                    |qs| {
                        qs.iter()
                            .filter_map(|x| {
                                if let Some(s) = x.as_str() {
                                    // Back-compat: a bare string is a question
                                    // without a recommendation.
                                    return Some(DesignQuestion {
                                        section: String::new(),
                                        question: s.to_string(),
                                        recommendation: String::new(),
                                        options: Vec::new(),
                                    });
                                }
                                let obj = x.as_object()?;
                                let section = obj
                                    .get("section")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let question = obj.get("question")?.as_str()?.to_string();
                                let recommendation = obj
                                    .get("recommendation")
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let options = obj
                                    .get("options")
                                    .and_then(|o| o.as_array())
                                    .map(|o| {
                                        o.iter()
                                            .filter_map(|v| v.as_str())
                                            .map(str::to_string)
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                Some(DesignQuestion {
                                    section,
                                    question,
                                    recommendation,
                                    options,
                                })
                            })
                            .collect()
                    },
                );
            }
            _ => {}
        }
    }
    reply
}

/// The tools advertised to the design model: finalize the AppSpec and keep the
/// open-question list current.
pub fn design_tools() -> Vec<spire_core::actors::messages::ToolInfo> {
    let submit = spire_core::actors::messages::ToolInfo {
        name: SUBMIT_APPSPEC_TOOL.to_string(),
        description: "Finalize the AppSpec for this project. Call ONLY when the design is\n            complete AND the open-question list is empty — never after a single\n            underspecified goal. When in doubt, ask a clarifying question instead.\n            Pass the FULL spec.md in the strict grammar below."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "spec_md": {
                    "type": "string",
                    "description": format!(
                        "The full spec.md document in this strict grammar:\n{SPEC_MD_GRAMMAR}"
                    ),
                }
            },
            "required": ["spec_md"]
        }),
    };
    let questions = spire_core::actors::messages::ToolInfo {
        name: SET_OPEN_QUESTIONS_TOOL.to_string(),
        description: "Replace the design session's open-question list — what must still be
            answered before an AppSpec can be submitted. Each entry is a question
            plus the answer you recommend (and optional alternatives) so the user
            can accept one with a single click. Call it whenever the set changes,
            INCLUDING to clear it (pass an empty list once every question is
            resolved). Keep each question concrete and answerable."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "section": { "type": "string", "description": "Which AppSpec section this question covers: types | graph | backend | bridge | ui." },
                            "question": { "type": "string", "description": "One concrete, answerable question." },
                            "recommendation": { "type": "string", "description": "The answer you recommend — the user can accept it with one click." },
                            "options": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional alternative answers the user can pick instead."
                            }
                        },
                        "required": ["question", "recommendation"]
                    },
                    "description": "The complete list of open questions ([] when none remain)."
                }
            },
            "required": ["questions"]
        }),
    };
    let outline = spire_core::actors::messages::ToolInfo {
        name: SET_OUTLINE_TOOL.to_string(),
        description: "Replace the session's concept draft — the basic assignment of the app
            across its core modules (UI / Graph / Backend / Bridge, plus shared data
            types), in markdown. Emit it every turn so the session always shows the
            current concept; give every part a concrete name and a sensible default,
            and mark only genuine forks with `(to decide)`."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "outline_md": {
                    "type": "string",
                    "description": "The current concept/outline in markdown, organized by UI / Graph / Backend / Bridge / types.",
                }
            },
            "required": ["outline_md"]
        }),
    };
    vec![submit, outline, questions]
}

/// The required AppSpec sections the model must cover while drafting the
/// outline and its questions (the `section` tags on [`DesignQuestion`]).
const DESIGN_SECTIONS: [&str; 5] = ["types", "graph", "backend", "bridge", "ui"];

/// Which required sections currently have NO open question — surfaced to the
/// model (and testable) so a section like the graph schema is never silently
/// dropped.
fn coverage_note(open: &[DesignQuestion]) -> Option<String> {
    use std::collections::HashSet;
    let covered: HashSet<&str> = open
        .iter()
        .map(|q| q.section.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let missing: Vec<&str> = DESIGN_SECTIONS
        .iter()
        .copied()
        .filter(|s| !covered.contains(s))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let covered_list = if covered.is_empty() {
        "(none yet)".to_string()
    } else {
        covered.iter().map(|s| *s).collect::<Vec<_>>().join(", ")
    };
    Some(format!(
        "## Coverage — your open questions so far touch: {covered_list}.\nNo open question yet for: {missing}. If those sections still need decisions, add questions for them.",
        covered_list = covered_list,
        missing = missing.join(", ")
    ))
}

/// Does this user message ask the design to be finalized? Strong words always
/// trigger submission; weak/affirmative words only do when no questions remain
/// (otherwise they just continue the Q&A).
fn submit_intent(text: &str, questions_empty: bool) -> bool {
    let lower = text.to_lowercase();
    let strong = ["submit", "finalize"]
        .iter()
        .any(|k| lower.contains(k))
        || (["generate", "create", "draft", "write"]
            .iter()
            .any(|k| lower.contains(k))
            && lower.contains("appspec"));
    let weak = [
        "continue", "go ahead", "go", "proceed", "yes", "ok", "okay", "done", "ready", "send",
    ]
    .iter()
    .any(|k| lower.contains(k));
    strong || (questions_empty && weak)
}

/// The model narrated a submission ("sending the AppSpec", ...) without actually
/// calling the submit tool — treat that as a request to finalize deterministically.
fn model_signals_submit(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["submitting", "sending", "ready to submit", "will submit", "finaliz", "submit_appspec"]
        .iter()
        .any(|k| lower.contains(k))
}

/// Focused prompt for the deterministic finalize pass: convert the settled
/// concept into the full strict-grammar spec.md, nothing else.
fn submit_spec_prompt(project_name: &str, goal: &str, outline: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(SPIRE_APP_CONTEXT.to_string());
    parts.push(format!("Project: {project_name}"));
    parts.push(format!("Goal: {goal}"));
    parts.push(String::new());
    parts.push("The design is complete — do NOT ask questions. Produce ONLY the full".to_string());
    parts.push("spec.md document for this project in the strict grammar below. Output".to_string());
    parts.push("the spec.md content directly as your reply text (no commentary), or call".to_string());
    parts.push("the `submit_appspec` tool with it.".to_string());
    if let Some(o) = outline.filter(|o| !o.trim().is_empty()) {
        parts.push(String::new());
        parts.push("## Current concept (source of truth — convert it faithfully)".to_string());
        parts.push(o.trim().to_string());
    }
    parts.push(String::new());
    parts.push("SPEC GRAMMAR:".to_string());
    parts.push(SPEC_MD_GRAMMAR.to_string());
    parts.join("
")
}

/// Free-form brainstorm prompt: an outline-first workflow. The user's message is
/// ALWAYS an incomplete list of functional requirements; the model drafts the
/// design outline (via `set_outline`), tracks every gap as a question (via
/// `set_open_questions`, each tagged with its section and a recommended answer),
/// asks one question at a time, and only submits once the outline has no gaps.
fn brainstorm_prompt(
    project_name: &str,
    goal: &str,
    question: &str,
    context: &[DesignTurn],
    grounding: &[String],
    outline: Option<&str>,
    open: &[DesignQuestion],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(SPIRE_APP_CONTEXT.to_string());
    parts.push(format!("Project: {project_name}"));
    parts.push(format!("Goal: {goal}"));
    parts.push(String::new());
    parts.push("## The user's message".to_string());
    parts.push(question.to_string());
    parts.push(String::new());
    if let Some(o) = outline.filter(|o| !o.trim().is_empty()) {
        parts.push("## Design outline (current draft — keep it in sync)".to_string());
        parts.push(o.trim().to_string());
        parts.push(String::new());
    }
    parts.push("## Recent conversation".to_string());
    parts.push(format_turns(context));
    if !grounding.is_empty() {
        parts.push(String::new());
        for block in grounding {
            parts.push(block.clone());
        }
    }
    if let Some(note) = coverage_note(open) {
        parts.push(String::new());
        parts.push(note);
    }
    parts.push(String::new());
    parts.push(
        "You are a decisive design partner for a Spire app who knows the platform.".to_string(),
    );
    parts.push(String::new());
    parts.push("WORKFLOW — the user's message is ALWAYS a vague, incomplete list of functional".to_string());
    parts.push("requirements. Your job is to turn it into a concrete Spire app. Be creative:".to_string());
    parts.push("invent a basic concept and assign every responsibility to one of the app's".to_string());
    parts.push("core modules — UI (SwiftUI screens), backend (Rust actors), graph (the".to_string());
    parts.push("memory-graph schema), bridge (the JSON contract between UI and actors).".to_string());
    parts.push(String::new());
    parts.push("CONCEPT (be creative, decide by default). With every reply, first make sure a".to_string());
    parts.push("basic concept exists that assigns the requirement across the modules. Give".to_string());
    parts.push("every part a concrete name and a sensible default — do NOT wait to be asked.".to_string());
    parts.push("For a requirement like \"a UI to view GIS data stored in the memory graph\",".to_string());
    parts.push("invent: a `MapScreen` (UI) that lists/views `Feature` nodes; a `Feature`".to_string());
    parts.push("graph node (with fields) plus edges to its source; a `FeatureStore` actor".to_string());
    parts.push("that owns them; and `list_features`/`get_feature` bridge methods the screen".to_string());
    parts.push("calls. Fill in whatever the requirements imply — only mark something `(to".to_string());
    parts.push("decide)` when the requirement is genuinely silent on a real fork that".to_string());
    parts.push("changes the design.".to_string());
    parts.push(String::new());
    parts.push("OUTLINE. Keep the concept as the current draft via the `set_outline` tool:".to_string());
    parts.push("markdown organized by UI / Graph / Backend / Bridge (plus shared Data".to_string());
    parts.push("types), listing the concrete items you have decided and marking items that".to_string());
    parts.push("still genuinely need a decision with `(to decide)`.".to_string());
    parts.push(String::new());
    parts.push("QUESTIONS. For the few items marked `(to decide)`, keep the open-question".to_string());
    parts.push("list current via `set_open_questions` (replace semantics). Every entry must".to_string());
    parts.push("name its `section` (types | graph | backend | bridge | ui), include the".to_string());
    parts.push("answer you recommend, and add 2-3 `options` only when a real choice".to_string());
    parts.push("exists. Ask exactly ONE of them in the chat — the most consequential — and".to_string());
    parts.push("let the rest live in the list. ALWAYS write that question in your reply".to_string());
    parts.push("text — never reply with only a tool call and no prose.".to_string());
    parts.push(String::new());
    parts.push("CONVERGE. When a question is answered, fold the decision into the concept,".to_string());
    parts.push("REMOVE that question from the list and update it via `set_open_questions`".to_string());
    parts.push("(pass [] once none remain). Then ask the NEXT question in your reply".to_string());
    parts.push("text. Never re-ask a settled question and never stop mid-design: each".to_string());
    parts.push("reply either asks the next question or says you are ready to submit. Do".to_string());
    parts.push("not invent requirements the user never stated.".to_string());
    parts.push(String::new());
    parts.push("SUBMIT. Only when every module of the concept is decided AND the open-".to_string());
    parts.push("question list is empty, call `submit_appspec` with the FULL spec.md in the".to_string());
    parts.push("strict grammar described on that tool. Do not submit after the first".to_string());
    parts.push("message unless you have genuinely filled every module.".to_string());
    parts.push(String::new());
    parts.push("COMPLETENESS CHECKLIST — the concept must concretely pin down each module:".to_string());
    parts.push("  - app: project name + a crisp goal".to_string());
    parts.push("  - types: the shared records/enums actors, bridge and UI reference".to_string());
    parts.push("  - graph: the node types (with fields) that store the domain data, and the edges".to_string());
    parts.push("  - backend: the actors, their state, and which bridge methods each handles".to_string());
    parts.push("  - bridge: every method with its params and Result type".to_string());
    parts.push("  - ui: each screen, what it shows/does, and its actions/bindings to bridge".to_string());
    parts.push("Every bridge method must be handled by exactly one actor; every UI action".to_string());
    parts.push("must call a bridge method. Decide what you can, ask only about real forks.".to_string());
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
                    outline: None,
                    text: next.unwrap_or_default(),
                    spec_md: None,
                    open_questions: None,
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
                    outline: None,
                    text: "final proposal".to_string(),
                    spec_md: Some(md),
                    open_questions: None,
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
                    outline: None,
                    text: String::new(),
                    spec_md: Some("# Not a spec at all\n\nrandom".to_string()),
                    open_questions: None,
                })
            })
        });
        let (answer, s) = pollster(a.ask("submit now", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Freeform);
        assert_eq!(s.accepted.len(), 0);
        assert!(answer.contains("rejected"), "{answer}");
    }

    #[test]
    fn parse_design_reply_understands_text_and_tools() {
        // Plain text reply.
        let t = parse_design_reply("just some prose");
        assert_eq!(t.text, "just some prose");
        assert!(t.spec_md.is_none() && t.open_questions.is_none());

        // JSON assistant message with a submit_appspec tool call.
        let json = serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "x",
                "type": "function",
                "function": { "name": "submit_appspec", "arguments": "{\"spec_md\":\"# AppSpec: X\\n\"}" }
            }]
        });
        let r = parse_design_reply(&json.to_string());
        assert_eq!(r.text, "");
        assert_eq!(r.spec_md.as_deref(), Some("# AppSpec: X\n"));

        // set_open_questions call alongside prose.
        let json2 = serde_json::json!({
            "content": "two questions for you",
            "tool_calls": [{
                "function": { "name": "set_open_questions", "arguments": "{\"questions\":[\"a\",\" b \"]}" }
            }]
        });
        let r2 = parse_design_reply(&json2.to_string());
        assert_eq!(r2.text, "two questions for you");
        assert_eq!(
            r2.open_questions,
            Some(vec![
                DesignQuestion {
                    section: String::new(),
                    question: "a".to_string(),
                    recommendation: String::new(),
                    options: Vec::new(),
                },
                DesignQuestion {
                    section: String::new(),
                    question: " b ".to_string(),
                    recommendation: String::new(),
                    options: Vec::new(),
                },
            ])
        );

        // Object-form entries (question + recommendation + options).
        let args4 = serde_json::json!({
            "questions": [
                {
                    "question": "Format?",
                    "recommendation": "WKB",
                    "options": ["GeoJSON", "Parquet"],
                }
            ]
        })
        .to_string();
        let json4 = serde_json::json!({
            "content": null,
            "tool_calls": [{
                "function": { "name": "set_open_questions", "arguments": args4 }
            }]
        });
        let r4 = parse_design_reply(&json4.to_string());
        let qs4 = r4.open_questions.expect("parsed object questions");
        assert_eq!(qs4.len(), 1);
        assert_eq!(qs4[0].question, "Format?");
        assert_eq!(qs4[0].recommendation, "WKB");
        assert_eq!(qs4[0].options, vec!["GeoJSON", "Parquet"]);

        // Both tools in one message.
        let json3 = serde_json::json!({
            "content": null,
            "tool_calls": [
                { "function": { "name": "set_open_questions", "arguments": "{\"questions\":[]}" } },
                { "function": { "name": "submit_appspec", "arguments": "{\"spec_md\":\"# A\\n\"}" } }
            ]
        });
        let r3 = parse_design_reply(&json3.to_string());
        assert_eq!(r3.open_questions, Some(vec![]));
        assert_eq!(r3.spec_md.as_deref(), Some("# A\n"));
    }

    #[test]
    fn design_tools_carry_the_submit_and_questions_tools() {
        let tools = design_tools();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name, SUBMIT_APPSPEC_TOOL);
        assert_eq!(tools[1].name, SET_OUTLINE_TOOL);
        assert_eq!(tools[2].name, SET_OPEN_QUESTIONS_TOOL);
        // The grammar is embedded in the submit tool so the model can compose a
        // parseable spec.md.
        let schema = serde_json::to_string(&tools[0].input_schema).unwrap();
        assert!(schema.contains("## Data types"));
        assert!(schema.contains("## Bridge"));
        assert!(schema.contains("## UI"));
    }

    #[test]
    fn submit_is_refused_while_open_questions_remain() {
        let (mut a, _) = actor(vec![]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let md = example_spec_md();
        a.llm = Box::new(move |_p: String| {
            let md = md.clone();
            Box::pin(async move {
                Ok(DesignReply {
                    outline: None,
                    text: String::new(),
                    spec_md: Some(md),
                    open_questions: Some(vec![DesignQuestion {
                        section: String::new(),
                        question: "vector or raster?".to_string(),
                        recommendation: "vector".to_string(),
                        options: Vec::new(),
                    }]),
                })
            })
        });
        let (answer, s) = pollster(a.ask("draft it now", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Freeform, "gate holds while questions remain");
        assert!(s.accepted.is_empty());
        assert!(answer.contains("rejected"), "{answer}");
        assert!(answer.contains("vector or raster?"), "{answer}");
    }

    #[test]
    fn cleared_open_questions_unblock_submission() {
        let (mut a, _) = actor(vec![]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let md = example_spec_md();
        a.llm = Box::new(move |_p: String| {
            let md = md.clone();
            Box::pin(async move {
                Ok(DesignReply {
                    outline: None,
                    text: "done".to_string(),
                    spec_md: Some(md),
                    open_questions: Some(vec![]),
                })
            })
        });
        let (answer, s) = pollster(a.ask("submit", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Decided);
        assert_eq!(s.accepted.len(), 1);
        assert!(answer.contains("AppSpec submitted"), "{answer}");
    }

    #[test]
    fn open_questions_persist_and_resume() {
        let (tx, _fake) = fake_graph_pair();
        let (mut a, _) = actor(vec![]);
        a.set_memory_graph(tx.clone());
        pollster(a.start_session("spire-gis", "view and edit map layers", false)).unwrap();
        a.llm = Box::new(|_p: String| {
            Box::pin(async move {
                Ok(DesignReply {
                    outline: None,
                    text: "ok".to_string(),
                    spec_md: None,
                    open_questions: Some(vec![
                        DesignQuestion { section: String::new(), question: "units?".to_string(), recommendation: "meters".to_string(), options: Vec::new() },
                        DesignQuestion { section: String::new(), question: "projection?".to_string(), recommendation: "EPSG:3857".to_string(), options: Vec::new() },
                    ]),
                })
            })
        });
        let (_, s1) = pollster(a.ask("what format?", false, false)).unwrap();
        assert_eq!(
            s1.open_questions,
            vec![
                DesignQuestion {
                    section: String::new(),
                    question: "units?".to_string(),
                    recommendation: "meters".to_string(),
                    options: Vec::new(),
                },
                DesignQuestion {
                    section: String::new(),
                    question: "projection?".to_string(),
                    recommendation: "EPSG:3857".to_string(),
                    options: Vec::new(),
                },
            ]
        );

        // A brand-new actor resumes the same list from the graph.
        let (mut b, _) = actor(vec![]);
        b.set_memory_graph(tx);
        let s2 = pollster(b.start_session("spire-gis", "again", false)).unwrap();
        assert_eq!(
            s2.open_questions,
            vec![
                DesignQuestion {
                    section: String::new(),
                    question: "units?".to_string(),
                    recommendation: "meters".to_string(),
                    options: Vec::new(),
                },
                DesignQuestion {
                    section: String::new(),
                    question: "projection?".to_string(),
                    recommendation: "EPSG:3857".to_string(),
                    options: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn coverage_note_lists_sections_still_missing() {
        // Nothing covered yet -> all five missing.
        let none = coverage_note(&[]).expect("empty list gets a reminder");
        assert!(none.contains("graph"), "{none}");
        assert!(none.contains("bridge"), "{none}");
        assert!(none.contains("ui"), "{none}");

        // Partial coverage -> only the gap is named.
        let open = vec![DesignQuestion {
            section: "types".to_string(),
            question: "what units?".to_string(),
            recommendation: "meters".to_string(),
            options: vec![],
        }];
        let partial = coverage_note(&open).expect("partial coverage gets a reminder");
        assert!(partial.contains("types"));
        assert!(partial.contains("graph"), "{partial}");
        assert!(!partial.contains("ui,") || partial.contains("ui"), "{partial}");

        // Every required section covered -> no reminder.
        let full: Vec<DesignQuestion> = ["types", "graph", "backend", "bridge", "ui"]
            .iter()
            .map(|s| DesignQuestion {
                section: s.to_string(),
                question: "q".to_string(),
                recommendation: "r".to_string(),
                options: vec![],
            })
            .collect();
        assert!(coverage_note(&full).is_none());
    }

    #[test]
    fn outline_parses_and_persists_across_restart() {
        // set_outline tool call is parsed.
        let args = serde_json::json!({
            "outline_md": "## Data types\n- (to decide) MapFeature\n\n## Graph\n- (to decide) nodes",
        })
        .to_string();
        let json = serde_json::json!({
            "content": null,
            "tool_calls": [{
                "function": { "name": "set_outline", "arguments": args }
            }]
        });
        let r = parse_design_reply(&json.to_string());
        assert!(r.outline.as_deref().unwrap_or("").contains("## Graph"));

        // Persist + resume.
        let (tx, _fake) = fake_graph_pair();
        let (mut a, _) = actor(vec![]);
        a.set_memory_graph(tx.clone());
        pollster(a.start_session("spire-gis", "view and edit map layers", false)).unwrap();
        a.llm = Box::new(|_p: String| {
            Box::pin(async move {
                Ok(DesignReply {
                    outline: Some("## Graph\n- nodes (to decide)".to_string()),
                    text: "drafting".to_string(),
                    spec_md: None,
                    open_questions: None,
                })
            })
        });
        let (_, s1) = pollster(a.ask("what should this do?", false, false)).unwrap();
        assert_eq!(s1.outline.as_deref(), Some("## Graph\n- nodes (to decide)"));

        let (mut b, _) = actor(vec![]);
        b.set_memory_graph(tx);
        let s2 = pollster(b.start_session("spire-gis", "again", false)).unwrap();
        assert_eq!(s2.outline.as_deref(), Some("## Graph\n- nodes (to decide)"));
    }

    #[test]
    fn tool_only_reply_still_returns_a_meaningful_answer() {
        let (mut a, _) = actor(vec![]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        a.llm = Box::new(|_p: String| {
            Box::pin(async move {
                Ok(DesignReply {
                    text: String::new(),
                    outline: Some("## UI
- MapScreen".to_string()),
                    spec_md: None,
                    open_questions: None,
                })
            })
        });
        let (answer, s) = pollster(a.ask("draft it", false, false)).unwrap();
        assert!(!answer.trim().is_empty(), "tool-only reply must not come back empty");
        assert!(answer.contains("Concept updated"), "{answer}");
        assert!(answer.contains("prepare the AppSpec"), "{answer}");
        assert_eq!(s.outline.as_deref(), Some("## UI
- MapScreen"));
    }

    #[test]
    fn silent_follow_up_surfaces_the_next_question() {
        let (mut a, _) = actor(vec![]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let calls = Arc::new(Mutex::new(0usize));
        let c1 = calls.clone();
        a.llm = Box::new(move |_p: String| {
            let calls = c1.clone();
            Box::pin(async move {
                let n = {
                    let mut g = calls.lock().unwrap();
                    *g += 1;
                    *g
                };
                if n == 1 {
                    Ok(DesignReply {
                        text: "asking about the domain".to_string(),
                        outline: None,
                        spec_md: None,
                        open_questions: Some(vec![
                            DesignQuestion {
                                section: "types".to_string(),
                                question: "what units?".to_string(),
                                recommendation: "meters".to_string(),
                                options: vec![],
                            },
                            DesignQuestion {
                                section: "graph".to_string(),
                                question: "which nodes?".to_string(),
                                recommendation: "feature nodes".to_string(),
                                options: vec![],
                            },
                        ]),
                    })
                } else {
                    // Tool-only follow-up: concept update, no prose.
                    Ok(DesignReply {
                        text: String::new(),
                        outline: Some("## UI
- MapScreen".to_string()),
                        spec_md: None,
                        open_questions: None,
                    })
                }
            })
        });
        let (_, s1) = pollster(a.ask("what should this do?", false, false)).unwrap();
        assert_eq!(s1.open_questions.len(), 2);
        let (answer2, _) = pollster(a.ask("use meters", false, false)).unwrap();
        assert!(!answer2.trim().is_empty(), "follow-up must not be empty");
        assert!(answer2.contains("Next up"), "{answer2}");
        assert!(answer2.contains("what units?"), "{answer2}");
        assert!(answer2.contains("meters"), "{answer2}");
    }

    #[test]
    fn submit_intent_detection_is_sensible() {
        assert!(submit_intent("submit", true));
        assert!(submit_intent("please finalize", false));
        assert!(submit_intent("go ahead and create the appspec", false));
        assert!(submit_intent("continue", true));
        assert!(!submit_intent("continue", false), "weak words only finalize when empty");
        assert!(!submit_intent("what projection do you recommend?", false));
    }

    #[test]
    fn continue_command_runs_the_deterministic_submit_pass() {
        // Canned LLM: first call answers normally, the submit pass converts the
        // settled concept into the spec.md (returned as plain text).
        let (mut a, prompts) = actor(vec!["design ready".to_string(), example_spec_md()]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let (answer, s) = pollster(a.ask("continue", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Decided);
        assert_eq!(s.accepted.len(), 1);
        assert!(answer.contains("AppSpec submitted"), "{answer}");
        // The submit pass used a dedicated prompt.
        let prompts = prompts.lock().unwrap();
        assert!(prompts[1].contains("SPEC GRAMMAR"), "{}", prompts[1]);
        assert!(prompts[1].contains("do NOT ask questions"));
    }

    #[test]
    fn submit_pass_surfaces_validation_errors_and_stays_freeform() {
        let (mut a, _) = actor(vec!["ready".to_string(), "# Not a spec at all

random".to_string()]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        let (answer, s) = pollster(a.ask("submit", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Freeform);
        assert!(s.accepted.is_empty());
        assert!(answer.contains("did not validate"), "{answer}");
    }

    #[test]
    fn submit_command_still_respects_open_questions() {
        // With open questions the weak "continue" does not trigger the submit
        // pass (the model's answer is returned as-is).
        let (mut a, prompts) = actor(vec!["what about the projection?".to_string()]);
        a.start("spire-gis", "view and edit map layers").unwrap();
        seed_conversation(&mut a);
        a.open_questions.push(DesignQuestion {
            section: "graph".to_string(),
            question: "which projection?".to_string(),
            recommendation: "EPSG:3857".to_string(),
            options: vec![],
        });
        let (answer, s) = pollster(a.ask("continue", false, false)).unwrap();
        assert_eq!(s.mode, DesignMode::Freeform);
        assert_eq!(answer, "what about the projection?");
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "no submit pass while questions remain");
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
        assert!(p1.contains("## The user's message"));
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
                    outline: None,
                    text: String::new(),
                    spec_md: Some(md),
                    open_questions: None,
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
                    outline: None,
                    text: "final proposal".to_string(),
                    spec_md: Some(md),
                    open_questions: None,
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
                    outline: None,
                    text: "alpha final".to_string(),
                    spec_md: Some(md),
                    open_questions: None,
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
        let b = brainstorm_prompt("spire-gis", "view and edit map layers", "what stack?", &[], &[], None, &[]);
        assert!(b.contains("SPIRE APP CONTEXT"));
        assert!(b.contains("SwiftUI"));
        assert!(b.contains("actor pattern"));
        assert!(b.contains("Rust"));
        // Concept-first workflow + coverage + no early submit.
        assert!(b.contains("WORKFLOW"));
        assert!(b.contains("CONCEPT"));
        assert!(b.contains("be creative"));
        assert!(b.contains("core modules"));
        assert!(b.contains("set_outline"));
        assert!(b.contains("set_open_questions"));
        assert!(b.contains("submit_appspec"));
        assert!(b.contains("COMPLETENESS CHECKLIST"));
        // The strict grammar the submit tool advertises is complete.
        assert!(SPEC_MD_GRAMMAR.contains("## Data types"));
        assert!(SPEC_MD_GRAMMAR.contains("## Graph"));
        assert!(SPEC_MD_GRAMMAR.contains("## Backend"));
        assert!(SPEC_MD_GRAMMAR.contains("## Bridge"));
        assert!(SPEC_MD_GRAMMAR.contains("## UI"));
    }
}
