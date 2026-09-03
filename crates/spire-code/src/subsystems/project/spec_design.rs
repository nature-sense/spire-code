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

use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;

use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::actors::Actor;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignMode {
    /// Prompts drive everything (the default).
    Freeform,
    /// The spec is frozen; only Reopen is allowed.
    Decided,
}

/// One conversation turn (user or assistant) in the design transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignTurn {
    pub role: String,
    pub text: String,
}

/// A condensed document (summary or spec). The summary is the running source of
/// truth during the free-form phase; the spec is the `spec.md` document.
#[derive(Debug, Clone, PartialEq)]
pub struct DesignArtifact {
    pub version: u32,
    pub content: String,
    pub source_turns: Vec<usize>,
    pub produced_at: chrono::DateTime<chrono::Utc>,
}

/// One deterministic derivation recorded by a Decide press.
#[derive(Debug, Clone)]
pub struct AcceptedSpec {
    pub version: u32,
    pub app_spec: AppSpec,
    pub issues: Vec<SpecIssue>,
    pub decided_at: chrono::DateTime<chrono::Utc>,
}

/// A point-in-time view of the design session.
#[derive(Debug, Clone)]
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
}

impl SpecDesignActor {
    pub fn new(project_name: impl Into<String>, goal: impl Into<String>, llm: LlmCall) -> Self {
        Self {
            project_name: project_name.into(),
            goal: goal.into(),
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
        }
    }

    /// Wire the memory graph so Decide can persist the derived spec.
    pub fn set_memory_graph(&mut self, tx: mpsc::Sender<MemoryGraphMessage>) {
        self.memory_graph_tx = Some(tx);
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
        info!(
            "[SpecDesign] session started for '{}': {}",
            self.project_name, self.goal
        );
        Ok(self.state())
    }

    /// Append a turn to the free-form transcript.
    pub fn append_turn(&mut self, role: &str, text: &str) -> Result<SpecDesignState, String> {
        self.require_freeform()?;
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
        Ok(artifact)
    }

    /// Compile the (mature) summary into the `spec.md` document. Still a prompt:
    /// the user keeps refining via chat until Decide freezes it.
    pub async fn promote(&mut self, instruction: &str) -> Result<DesignArtifact, String> {
        self.require_freeform()?;
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
        Ok(artifact)
    }

    /// THE button: freeze the spec and derive the AppSpec deterministically.
    pub async fn decide(&mut self) -> Result<AppSpec, String> {
        self.require_freeform()?;
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
    pub fn reopen(&mut self) -> Result<SpecDesignState, String> {
        self.mode = DesignMode::Freeform;
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

#[async_trait]
impl Actor for SpecDesignActor {
    type Message = SpecDesignMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            SpecDesignMessage::Start {
                project_name,
                goal,
                reply_to,
            } => {
                let _ = reply_to.send(self.start(&project_name, &goal));
            }
            SpecDesignMessage::Reply { text, reply_to } => {
                let _ = reply_to.send(self.append_turn(ROLE_USER, &text));
            }
            SpecDesignMessage::AppendTurn {
                role,
                text,
                reply_to,
            } => {
                let _ = reply_to.send(self.append_turn(&role, &text));
            }
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
            SpecDesignMessage::Decide { reply_to } => {
                let _ = reply_to.send(self.decide().await);
            }
            SpecDesignMessage::Reopen { reply_to } => {
                let _ = reply_to.send(self.reopen());
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
        (
            SpecDesignActor::new("spire-gis", "view and edit map layers", llm),
            prompts,
        )
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

        a.reopen().unwrap();
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

    /// Block on a future from a sync test (no global runtime needed).
    fn pollster<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }
}
