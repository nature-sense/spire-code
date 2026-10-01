// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! ProjectCreationActor — scaffolds new projects and plans changes to existing ones.
//!
//! Unified pipeline for "create" and "plan":
//!   1. LLM decomposes a natural-language goal into structured steps
//!   2. Steps execute via FilesystemModule (dirs/config/source) + crates-io MCP (deps)
//!   3. After each step, the project is re-analyzed so the LLM's context stays current
//!   4. BuildModule validates generated source (AST parse) and builds/tests

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use super::spec::AppSpec;
use crate::actors::{
    Actor, BuildManagerMessage, LlmMessage, McpClientMessage, ProjectAnalyzerMessage,
};
use crate::build::{BuildOptions, TestOptions};
use spire_core::models::memory_graph::AttrNode;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;

// ── Step types ──────────────────────────────────────────────────────────────

/// A discrete step in a project creation/planning pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreationStep {
    pub id: String,
    #[serde(rename = "stepType")]
    pub step_type: CreationStepType,
    pub description: String,
    pub status: StepStatus,
    /// Arbitrary parameters: file path, content, dependency name, etc.
    pub parameters: serde_json::Value,
    /// Result message after execution (error / output).
    pub result: Option<String>,
}

/// Type of a creation step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CreationStepType {
    CreateDirectory,
    WriteBuildConfig,
    AddDependency,
    WriteSourceFile,
    ParseAndValidate,
    Build,
    Test,
    /// Call an arbitrary MCP tool by server_name + tool_name (LLM-driven,
    /// language-module-agnostic). Never hardcodes a specific MCP server.
    ToolCall,
    /// Write + validate the approved HAL contract header (`hal/api/*.hpp`).
    /// Stage 0 gate: the header must summarize as a genuine abstract class
    /// (public pure-virtual methods + virtual destructor) or the step FAILS
    /// before anything touches disk.
    WriteHalContract,
    /// Write a per-platform HAL implementation source file
    /// (`hal/implementations/<plat>/*`), subject to the same fill-root guard
    /// as WriteSourceFile.
    WriteHalImplementation,
}

/// Execution status of a step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StepStatus {
    #[default]
    Pending,
    Executing,
    Completed,
    Failed,
}

/// Result of generating a plan.
#[derive(Debug, Clone, Serialize)]
pub struct PlanGenerationResult {
    pub goal: String,
    pub language: String,
    pub root_dir: String,
    pub steps: Vec<CreationStep>,
    /// True when the plan is the deterministic template fallback (the LLM
    /// timed out, returned unparseable JSON, or was not configured). The UI
    /// surfaces this distinctly so a 2-step stub is never mistaken for a real
    /// implementation plan.
    #[serde(default)]
    pub is_template: bool,
    /// Why the LLM was not used, when `is_template` is true. Distinguishes
    /// "LLM unavailable/unconfigured" from "the LLM answered but the response
    /// could not be parsed" — previously both collapsed into a misleading
    /// "LLM unavailable" message in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// Result of executing one step.
#[derive(Debug, Clone, Serialize)]
pub struct StepExecutionResult {
    pub step_id: String,
    pub success: bool,
    pub message: String,
}

/// Result of `PlanScaffold`: the in-memory structural contract (ScaffoldSpec)
/// computed WITHOUT writing any files, plus the LLM plan that implements its
/// goal inside that contract. The UI shows the plan for approval; on OK it
/// calls `ScaffoldProject` (materializes) then `ExecutePlan`. On reject
/// nothing has touched disk.
#[derive(Debug, Clone, Serialize)]
pub struct PlanScaffoldResult {
    pub plan: PlanGenerationResult,
    pub spec: crate::subsystems::build::build_manager::ScaffoldSpec,
}

/// Map a step-type string to its enum (shared by both plan shapes).
fn parse_step_type(s: &str) -> Option<CreationStepType> {
    Some(match s {
        "create_directory" => CreationStepType::CreateDirectory,
        "write_build_config" => CreationStepType::WriteBuildConfig,
        "add_dependency" | "declare_dependencies" => CreationStepType::AddDependency,
        "write_source_file" => CreationStepType::WriteSourceFile,
        "parse_and_validate" => CreationStepType::ParseAndValidate,
        "build" => CreationStepType::Build,
        "test" => CreationStepType::Test,
        "tool_call" => CreationStepType::ToolCall,
        "write_hal_contract" => CreationStepType::WriteHalContract,
        "write_hal_implementation" => CreationStepType::WriteHalImplementation,
        _ => return None,
    })
}

/// Normalize one LLM step JSON value into a CreationStep.
///
/// Accepts the two shapes the model actually emits:
///   1. `{"step_type": "write_source_file", "description": "...", "parameters": {...}}`
///   2. `{"write_source_file": {"path": "...", "content": "..."}}`
///      (single-key object whose key IS the step type and whose value is the
///      parameters — this is the schema `deepseek-chat` returned verbatim).
///
/// Backfill sensible defaults for LLM steps that omit required fields —
/// otherwise a `write_source_file` step with no `content` created an EMPTY
/// file and a `path`-less step targeted nowhere.
fn backfill_step_params(
    step_type: &CreationStepType,
    mut params: serde_json::Value,
) -> serde_json::Value {
    if let serde_json::Value::Object(map) = &mut params {
        match step_type {
            CreationStepType::WriteSourceFile => {
                map.entry("path")
                    .or_insert_with(|| serde_json::json!("src/main.rs"));
                map.entry("content")
                    .or_insert_with(|| serde_json::json!("// TODO: implement\n"));
            }
            CreationStepType::WriteBuildConfig => {
                map.entry("path")
                    .or_insert_with(|| serde_json::json!("Cargo.toml"));
            }
            _ => {}
        }
    }
    params
}

/// Human-readable step description derived from the parameters so the review
/// shows WHAT is added (files + dependencies), not a bare step-type name.
fn step_description(step_type: &CreationStepType, params: &serde_json::Value) -> String {
    match step_type {
        CreationStepType::WriteBuildConfig
        | CreationStepType::WriteSourceFile
        | CreationStepType::WriteHalImplementation
        | CreationStepType::WriteHalContract => {
            let path = params
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("(path)");
            let kind = match step_type {
                CreationStepType::WriteHalContract => "HAL contract",
                CreationStepType::WriteHalImplementation => "HAL implementation",
                _ => "file",
            };
            format!("Write {kind} {path}")
        }
        CreationStepType::CreateDirectory => {
            let path = params
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("(dir)");
            format!("Create directory {path}")
        }
        CreationStepType::AddDependency => {
            if let Some(deps) = params.get("dependencies").and_then(|v| v.as_array()) {
                let names: Vec<String> = deps
                    .iter()
                    .map(|d| {
                        let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let ver = d
                            .get("version")
                            .and_then(|v| v.as_str())
                            .filter(|v| !v.is_empty())
                            .unwrap_or("*");
                        format!("{name}@{ver}")
                    })
                    .collect();
                if names.is_empty() {
                    "Add dependencies".to_string()
                } else {
                    format!("Add {}", names.join(", "))
                }
            } else {
                "Add dependencies".to_string()
            }
        }
        _ => format!("{:?}", step_type).to_lowercase(),
    }
}

fn step_from_value(index: usize, v: &serde_json::Value) -> Option<CreationStep> {
    // Resolve the step-type string from ANY alias the model has emitted —
    // verified across live runs: `step`, `action`, `step_type`, `stepType`, and `type` — the
    // last one added after a live run of the application-creation loop, where the model wrote
    // `{"type": "write_source_file", "path": …, "content": …}` for all eight of its steps and
    // every one of them was discarded: an entire plan rejected for its key's *name*.
    // Parameters likewise from `arguments` or `parameters`, or — for that flat shape — from the
    // step object itself, which is where a model that writes `type` puts them.
    let type_key = v
        .get("step")
        .or_else(|| v.get("action"))
        .or_else(|| v.get("step_type"))
        .or_else(|| v.get("stepType"))
        .or_else(|| v.get("type"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    if let Some(s) = type_key {
        if let Some(step_type) = parse_step_type(&s) {
            let raw = v
                .get("arguments")
                .or_else(|| v.get("parameters"))
                .cloned()
                .unwrap_or_else(|| v.clone());
            let params = backfill_step_params(&step_type, raw);
            let description = v
                .get("description")
                .and_then(|x| x.as_str())
                .filter(|d| !d.trim().is_empty())
                .map(|d| d.to_string())
                .unwrap_or_else(|| step_description(&step_type, &params));
            return Some(CreationStep {
                id: format!("fill-{}", index + 1),
                step_type,
                description,
                status: StepStatus::Pending,
                parameters: params,
                result: None,
            });
        }
    }
    // Shape 2: single-key object — key is the step type, value is parameters.
    if let Some(obj) = v.as_object() {
        if obj.len() == 1 {
            if let Some((key, params)) = obj.iter().next() {
                if let Some(step_type) = parse_step_type(key) {
                    let params = backfill_step_params(&step_type, params.clone());
                    let description = params
                        .get("description")
                        .and_then(|x| x.as_str())
                        .filter(|d| !d.trim().is_empty())
                        .map(|d| d.to_string())
                        .unwrap_or_else(|| step_description(&step_type, &params));
                    return Some(CreationStep {
                        id: format!("fill-{}", index + 1),
                        step_type,
                        description,
                        status: StepStatus::Pending,
                        parameters: params,
                        result: None,
                    });
                }
            }
        }
    }
    None
}

/// Parse an LLM fill response into ordered steps, or None if it contains no
/// usable steps. Handles ```json fences with a simple trim, and is tolerant of
/// three top-level shapes the model actually emits:
///   1. a JSON array of steps;
///   2. an object wrapping the array under `steps` or `plan`;
///   3. a single step object (shape-2, key = step type).
///
/// Robustness (verified against LIVE DeepSeek V4 bodies, 2026-08-17): the model
/// sometimes returns a LARGE array (10k+ chars, many write_source_file steps
/// with embedded code) where ONE malformed element would previously reject the
/// whole reply and drop to the template. Recovery is layered:
///   1. Full-document parse (the common case, incl. ```json fences).
///   2. If the full parse fails, extract the first `[` … last `]` span and
///      attempt each top-level element independently, keeping the VALID steps
///      and dropping only the broken ones.
///
/// Verified: the exact 13k-char fenced shape from the live API parses on the
/// first path; the recovery path is a fallback for the occasional bad element.
fn parse_fill_steps(text: &str) -> Option<Vec<CreationStep>> {
    let cleaned = text
        .trim()
        .trim_start_matches('\u{feff}') // UTF-8 BOM
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Parse, remembering which slice succeeded so the recovery path can reuse it.
    let json_str = cleaned;
    let v = match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(v) => v,
        Err(_) => return parse_fill_steps_recover(json_str),
    };

    step_array_from_value(v)
}

/// Shared conversion from a parsed value into ordered steps (or None).
fn step_array_from_value(v: serde_json::Value) -> Option<Vec<CreationStep>> {
    // Shape 2: object wrapping the array under "steps" / "plan".
    let v = if let Some(obj) = v.as_object() {
        if let Some(arr) = obj
            .get("steps")
            .or_else(|| obj.get("plan"))
            .and_then(|x| x.as_array())
        {
            serde_json::Value::Array(arr.clone())
        } else if obj.len() == 1 {
            // Shape 3: single step object ({"write_source_file": {...}}).
            v
        } else {
            return None;
        }
    } else {
        v
    };

    let arr: Vec<&serde_json::Value> = if let Some(a) = v.as_array() {
        a.iter().collect()
    } else if v.as_object().map(|o| o.len() == 1).unwrap_or(false) {
        vec![&v]
    } else {
        return None;
    };

    let steps: Vec<CreationStep> = arr
        .iter()
        .enumerate()
        .filter_map(|(i, item)| step_from_value(i, item))
        .collect();
    if steps.is_empty() {
        None
    } else {
        Some(steps)
    }
}

/// Recovery path when the full document JSON parse fails: extract the first
/// `[` … last `]` span, then split into candidate top-level elements and keep
/// only the ones that parse. Assumes each element is an independent
/// `{ "step_type": { ... } }` object; the common failure mode is one bad
/// element (e.g. truncated or with an unescaped sequence) amidst an otherwise
/// valid array.
/// Add dependencies to a Cargo.toml manifest for the FILL plan.
///
/// The LLM's `declare_dependencies` step provides:
///   { "path": "<root>/Cargo.toml", "dependencies": [{"name":"tokio","version":"1"}, ...] }
/// This inserts `name = "version"` into the appropriate section:
///   [workspace.dependencies] for a workspace-root manifest,
///   [dependencies] otherwise.
/// A line-based editor is used so no `toml` crate dependency is required, and
/// existing entries are never duplicated. Returns the number added.
fn add_dependencies_to_manifest(
    manifest: &std::path::Path,
    deps: &[serde_json::Value],
) -> Result<usize, String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| format!("read {}: {e}", manifest.display()))?;
    let is_workspace_root = text.contains("[workspace]") || text.contains("[workspace.package]");
    let section = if is_workspace_root {
        "[workspace.dependencies]"
    } else {
        "[dependencies]"
    };

    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    // Locate the section header.
    let ix = lines
        .iter()
        .position(|l| l.trim() == section)
        .ok_or_else(|| format!("no {} section in {}", section, manifest.display()))?;
    // Determine where the section ends (next [x] header).
    let end = lines[ix + 1..]
        .iter()
        .position(|l| l.trim().starts_with('['))
        .map(|p| ix + 1 + p)
        .unwrap_or(lines.len());

    let mut added = 0usize;
    for d in deps {
        let name = d
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        // Skip if already present as `name = ...` in this section.
        let already = lines[ix + 1..end]
            .iter()
            .any(|l| l.trim_start().starts_with(&format!("{name} =")));
        if already {
            continue;
        }
        let version = d
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("*")
            .trim();
        let version = if version.is_empty() { "*" } else { version };
        lines.insert(end, format!("{name} = \"{version}\""));
        added += 1;
    }

    if added > 0 {
        let out = lines.join("\n") + "\n";
        std::fs::write(manifest, out).map_err(|e| format!("write {}: {e}", manifest.display()))?;
    }
    Ok(added)
}

fn parse_fill_steps_recover(text: &str) -> Option<Vec<CreationStep>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end <= start {
        return None;
    }
    let inner = &text[start + 1..end];
    let mut steps = Vec::new();
    let mut remaining = inner;
    let mut guard = 0;
    while let Some(rel) = remaining.find('{') {
        guard += 1;
        if guard > 256 {
            break;
        }
        // Find the matching closing brace, respecting nested braces and
        // string literals so embedded code in `content` fields is not split.
        let mut depth = 0i32;
        let mut in_str = false;
        let mut esc = false;
        let mut close = None;
        for (i, c) in remaining[rel..].char_indices() {
            if in_str {
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    in_str = false;
                }
            } else {
                match c {
                    '"' => in_str = true,
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(rel + i + c.len_utf8());
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        let Some(close) = close else { break };
        let candidate = &remaining[rel..close];
        remaining = &remaining[close..];
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(candidate) {
            let idx = steps.len();
            if let Some(step) = step_from_value(idx, &parsed) {
                steps.push(step);
            }
        }
        // Skip any gap (commas / whitespace) up to the next '{'.
        if let Some(next) = remaining.find('{') {
            remaining = &remaining[next..];
        } else {
            break;
        }
    }
    if steps.is_empty() {
        None
    } else {
        Some(steps)
    }
}

// ── Messages ────────────────────────────────────────────────────────────────

/// Messages for the ProjectCreation actor.
#[derive(Debug)]
pub enum ProjectCreationMessage {
    /// Generate a step-by-step plan for creating a new project.
    GeneratePlan {
        goal: String,
        root_dir: PathBuf,
        language: String,
        /// Optional cross-platform targets (registry ids, e.g. `["rpi5"]`).
        /// Empty/default = `["host"]` (single-target scaffold).
        platforms: Vec<String>,
        /// Optional structural shape ("spire_app" etc.) chosen in the wizard.
        /// Defaults to Native.
        structure: Option<spire_core::build_types::ProjectStructure>,
        /// Where the container an `EmbeddedApp` depends on lives — see
        /// `ScaffoldProject`. The plan **is** the application's scaffold, so it is required here
        /// too, not only when writing.
        embedded_root: Option<PathBuf>,
        /// True for embedded projects (cross-compiled targets only — no host).
        embedded: bool,
        reply_to: oneshot::Sender<Result<PlanGenerationResult>>,
    },
    /// Phase 1 of the two-phase creation flow: scaffold the project structure
    /// offline (NO LLM). Writes the module's `scaffold_layout` files to disk,
    /// reports which paths are structural (locked) vs fillable, then
    /// `AnalyzeProject` persists the graph. The returned `ScaffoldSpec` is
    /// what the LLM's `FillProject` phase respects.
    ScaffoldProject {
        project_name: String,
        root_dir: PathBuf,
        language: String,
        platforms: Vec<String>,
        /// Optional structural shape ("spire_app" etc.) chosen in the wizard.
        structure: Option<spire_core::build_types::ProjectStructure>,
        /// Where the container an `EmbeddedApp` depends on lives.
        ///
        /// Set only for that structure, and **required** by it: an application's whole dependency
        /// graph is read out of that directory, so a request without it is refused by name rather
        /// than scaffolded against nothing.
        embedded_root: Option<PathBuf>,
        /// True for embedded projects (cross-compiled targets only — no host).
        embedded: bool,
        /// The **framework** the application was designed in, when the design phase ran: it is
        /// written into the application's `CMakeLists.txt`, so the fill phase, a person and another
        /// tool all read the same choice back instead of inferring one.
        application: Option<crate::build::application_spec::ApplicationSpec>,
        reply_to: oneshot::Sender<Result<crate::subsystems::build::build_manager::ScaffoldSpec>>,
    },
    /// Phase 2 of the two-phase creation flow (LLM, constrained): fill the
    /// materialized scaffold from a natural-language goal. The LLM may only
    /// write files under the spec's fill roots, create subdirectories under
    /// existing leaves, and declare dependencies via `declare_dependencies`.
    /// Structural files are rejected by the guard in `execute_step`.
    FillProject {
        goal: String,
        root_dir: PathBuf,
        spec: crate::subsystems::build::build_manager::ScaffoldSpec,
        /// The library this project is built against, when the caller named one — read for its
        /// `SPIRE.md`, the same way `ScaffoldProject` reads it for the plan. `None` for a project
        /// whose caller has not said.
        library_root: Option<PathBuf>,
        reply_to: oneshot::Sender<Result<PlanGenerationResult>>,
    },
    /// AppSpec requirements pass (SpireApp, LLM): derive a VALIDATED AppSpec
    /// JSON contract for the goal — the bridge is the single source of truth
    /// the later fill/codegen phase implements. Nothing is written to disk;
    /// the spec is self-healed against `validate()` before it is returned.
    GenerateAppSpec {
        project_name: String,
        goal: String,
        reply_to: oneshot::Sender<Result<AppSpec>>,
    },
    /// AppSpec codegen (deterministic, no LLM): derive `write_source_file`
    /// skeleton steps from a VALIDATED AppSpec — serde types + actor skeletons
    /// + FFI dispatch (routing derived from `handlers`) on the Rust side, and
    ///   typed bridge wrappers + screen skeletons on the Swift side. Nothing is
    ///   written here; the caller executes the returned plan.
    GenerateCode {
        project_name: String,
        spec: AppSpec,
        reply_to: oneshot::Sender<Result<Vec<CreationStep>>>,
    },
    /// Plan a NEW project WITHOUT writing anything: compute the in-memory
    /// structural contract (ScaffoldSpec) from the build module, have the LLM
    /// propose implementation steps inside it, and return both `{plan, spec}`.
    /// Nothing touches disk until the user confirms — the UI then calls
    /// `ScaffoldProject` + `ExecutePlan`.
    PlanScaffold {
        goal: String,
        root_dir: PathBuf,
        project_name: String,
        language: String,
        platforms: Vec<String>,
        /// Optional structural shape ("spire_app" etc.) chosen in the wizard.
        structure: Option<spire_core::build_types::ProjectStructure>,
        /// Where the container an `EmbeddedApp` depends on lives — see
        /// `ScaffoldProject`. The *plan* needs it for the same reason the scaffold does: the spec
        /// it describes is read from that directory.
        embedded_root: Option<PathBuf>,
        /// True for embedded projects (cross-compiled targets only — no host).
        embedded: bool,
        /// The **framework** an application was designed in — see `ScaffoldProject`. The plan is
        /// asked for *before* anything is written, so this is the only place it can come from: a
        /// tree that does not exist yet cannot state it.
        application: Option<crate::build::application_spec::ApplicationSpec>,
        reply_to: oneshot::Sender<Result<PlanScaffoldResult>>,
    },
    /// Execute the entire plan sequentially.
    ExecutePlan {
        root_dir: PathBuf,
        steps: Vec<CreationStep>,
        reply_to: oneshot::Sender<Result<Vec<StepExecutionResult>>>,
    },
    /// Execute a single step and return the result.
    ExecuteStep {
        root_dir: PathBuf,
        step: CreationStep,
        reply_to: oneshot::Sender<Result<StepExecutionResult>>,
    },
}

// ── Actor ───────────────────────────────────────────────────────────────────

/// Creates projects and plans changes by orchestrating filesystem + MCP + build modules.
pub struct ProjectCreationActor {
    filesystem_tx: mpsc::Sender<spire_core::modules::FilesystemMessage>,
    build_manager_tx: mpsc::Sender<BuildManagerMessage>,
    mcp_client_tx: mpsc::Sender<McpClientMessage>,
    project_analyzer_tx: Option<mpsc::Sender<ProjectAnalyzerMessage>>,
    llm_tx: Option<mpsc::Sender<LlmMessage>>,
    /// Memory-graph sender (the graph is the system's single source of truth).
    /// When wired, the AppSpec requirements pass persists the validated spec
    /// as an `appspec` node so it can be linked to its implementation later.
    memory_graph_tx: Option<mpsc::Sender<MemoryGraphMessage>>,
    /// The active fill-phase structural contract (set by FillProject). The
    /// execution guard consults it to deny LLM writes to structural files.
    active_spec: Option<crate::subsystems::build::build_manager::ScaffoldSpec>,
}

impl ProjectCreationActor {
    pub fn new(
        filesystem_tx: mpsc::Sender<spire_core::modules::FilesystemMessage>,
        build_manager_tx: mpsc::Sender<BuildManagerMessage>,
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
    ) -> Self {
        Self {
            filesystem_tx,
            build_manager_tx,
            mcp_client_tx,
            project_analyzer_tx: None,
            llm_tx: None,
            memory_graph_tx: None,
            active_spec: None,
        }
    }

    /// Generate a fill plan: the LLM proposes implementation steps confined to
    /// the scaffold's fill roots.
    ///
    /// ATOMIC + FATAL: the LLM planning step is REQUIRED. On any genuine LLM
    /// failure (no API key, transport error, HTTP error, empty/unparseable
    /// response) this returns `Err` — the caller must NOT scaffold anything.
    /// No deterministic template fallback is permitted: a new project is only
    /// written once a real plan exists.
    async fn generate_fill_plan(
        &self,
        goal: &str,
        root_dir: &Path,
        spec: &crate::subsystems::build::build_manager::ScaffoldSpec,
        // The **library** an ESP-IDF application is built against, when the caller named one. Its
        // `SPIRE.md` is the project's architecture, and this prompt is the only place a model can
        // read it.
        library_root: Option<&Path>,
        // The **framework** an ESP-IDF application was designed in, when the caller knows it. `None`
        // is the ordinary case here — the caller is planning, so the tree does not exist yet — and it
        // is then read back from the application's own `CMakeLists.txt`, which is where the choice
        // lives once the tree does.
        application: Option<crate::build::application_spec::ApplicationSpec>,
    ) -> Result<PlanGenerationResult, String> {
        let project_name = root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "my_project".to_string());

        // **The tree says what it is.** Every application-specific block below — the framework, the
        // reviewed composition, the composition rules, the library's own headers — is gated on
        // `structure == IdfApplication`, and `structure` is a field a caller can drop: a spec built by
        // hand without it deserializes as `native`, because the field carries `#[serde(default)]`.
        //
        // That is not hypothetical, and it cost two live runs: the wizard's `scaffoldSpecJSON` omitted it,
        // so a fill asked for `actors` was handed the goal and the generic rules and **nothing else** — no
        // framework, no composition, no API — and wrote a bare FreeRTOS poll loop with its own sensor
        // classes. Both the composition and its JSON record are written *by* the application scaffold, so
        // a tree carrying either **is** an application whatever the field says: the file is the fact, the
        // field the claim.
        let planned_structure = match spec.structure {
            spire_core::build_types::ProjectStructure::IdfApplication => spec.structure,
            _ if root_dir
                .join(crate::build::idf_projects::COMPOSITION_FILE)
                .is_file()
                || root_dir
                    .join(crate::build::idf_projects::APPLICATION_FILE)
                    .is_file() =>
            {
                spire_core::build_types::ProjectStructure::IdfApplication
            }
            other => other,
        };

        // ATOMIC: the LLM planning step is REQUIRED. No configured LLM →
        // fatal error; nothing may ever be scaffolded without a real plan.
        let llm_tx = match &self.llm_tx {
            Some(tx) => tx.clone(),
            None => {
                return Err(
                    "LLM unavailable — the project creator is not connected to the LLM service (wiring, not a missing API key)."
                        .to_string(),
                )
            }
        };

        // For multi-platform workspaces the dependency section is the
        // workspace-root manifest ONLY (dependency_sections = ["Cargo.toml"]),
        // so the LLM declares each dependency ONCE — never duplicated into
        // per-member manifests.
        let structure = format!(
            "Structural files (DO NOT modify): {}\nFill roots (you may write files + create subdirs here): {}\nDependencies (declare each dependency ONCE via declare_dependencies, targeting this manifest): {}\nPlatforms: {:?}\n",
            spec.structural_files.join(", "),
            spec.fill_roots.join(", "),
            spec.dependency_sections.join(", "),
            spec.platform_targets
        );
        // The single manifest `declare_dependencies` must target (for a
        // multi-platform Cargo workspace this is the root Cargo.toml).
        let dep_section = spec
            .dependency_sections
            .first()
            .cloned()
            .unwrap_or_else(|| "Cargo.toml".to_string());
        // SpireApp projects get the curated framework API surface so the LLM
        // builds on spire-actor/spire-core instead of inventing APIs.
        let framework_hints =
            if spec.structure == spire_core::build_types::ProjectStructure::SpireApp {
                crate::build::generic_helpers::spire_framework_hints()
            } else {
                String::new()
            };
        // The **library's** hints, and the whole reason `SPIRE.md` is written down.
        //
        // An architecture is not in the code — it is in the file the library's author filled in — so
        // a plan written without it is a plan that invents one. Each type reads exactly one file:
        // an **application** reads the library it named, and a **library** reads its own, which is
        // there only when it is being filled after it was created. A brand-new library has written
        // down nothing yet, and that is not a gap to paper over — it is the truth about it.
        //
        // Read from disk rather than from the scaffolded tree, because at plan time the tree does
        // not exist: the plan is asked for *before* anything is written.
        let hints_root = match planned_structure {
            spire_core::build_types::ProjectStructure::IdfLibrary => Some(root_dir),
            spire_core::build_types::ProjectStructure::IdfApplication => library_root,
            _ => None,
        };
        let library_hints = hints_root
            .and_then(crate::build::idf_projects::library_hints)
            .map(|hints| {
                format!(
                    "LIBRARY HINTS — how the library this project builds on says it is meant to be\n\
                     used. Its author wrote this down, it is the architecture, and it is not in the\n\
                     code. Follow it.\n\n{hints}\n"
                )
            })
            .unwrap_or_default();
        // The **decomposition** the design phase produced, which is what stops the model choosing a
        // framework again *and* inventing a composition. It matters only for an application: a library
        // builds against nothing and states neither, which is exactly what the library scaffold
        // asserts.
        //
        // The decomposition handed in arrives **already resolved against the tree**: the caller asks
        // `idf_projects::design_for_scaffold`, which owns the rule that a tree carrying a design decides
        // and a copy a caller still holds does not override it. So the only question left here is the
        // arm below — the leg with nothing handed in at all.
        //
        // A tree that states a framework this does not know, or whose composition does not parse, is
        // *refused* rather than planned around: a composition written under a guess is a wasted round
        // trip at best.
        let designed = match planned_structure {
            spire_core::build_types::ProjectStructure::IdfApplication => match application {
                Some(application) => Some(application),
                // Nothing handed in: read the one the application carries, which is where the design
                // lives once the tree exists — the fill leg after a scaffold, with no caller needing
                // to remember it. **`composition.spire` is what is read**, and the JSON record is
                // brought back into agreement with it on the way (see
                // `read_application_and_sync_record`): the file a person edits decides, and the record
                // never goes on describing a design that was replaced.
                //
                // This leg is about to write the tree, so settling the record here is a write it was
                // going to do anyway — which is why it is the *settling* reader and not the plain one.
                //
                // A composition that is there and does not parse is **refused** here rather than
                // planned around, which is the rule the framework check above already follows. Falling
                // back to the JSON record in its place would plan the round trip from a design the
                // file no longer states, and nothing downstream would say so.
                None => crate::build::idf_projects::read_application_and_sync_record(root_dir)?,
            },
            _ => None,
        };
        // The framework: from the decomposition when there is one, and otherwise from the line the
        // application states — so an application made before the design phase runs is still told what
        // it says about itself.
        let framework = match designed.as_ref().map(|application| application.framework) {
            Some(framework) => Some(framework),
            None if planned_structure
                == spire_core::build_types::ProjectStructure::IdfApplication =>
            {
                match std::fs::read_to_string(root_dir.join("CMakeLists.txt")) {
                    Ok(contents) => crate::build::application_spec::declared_framework(&contents)?,
                    // No tree yet, or no file to read: nothing is stated, and the library's rule for
                    // choosing applies.
                    Err(_) => None,
                }
            }
            None => None,
        };
        let framework_block = match planned_structure {
            spire_core::build_types::ProjectStructure::IdfApplication => {
                crate::build::idf_projects::framework_prompt_block(framework)
            }
            _ => String::new(),
        };
        // The composition itself: the components, the units, the wiring and the board facts that were
        // reviewed. Without it the model writes a *different* application from the one that was
        // agreed, and nothing in the round trip says so.
        let composition_block = match &designed {
            Some(application)
                if planned_structure
                    == spire_core::build_types::ProjectStructure::IdfApplication =>
            {
                crate::build::idf_projects::composition_block(application)
            }
            _ => String::new(),
        };
        // The interfaces the composition has to call. A design says which components exist; only the
        // headers say what to call on them, and a live run wrote a plausible API that did not exist.
        let component_apis = match (&designed, library_root) {
            (Some(application), Some(root))
                if planned_structure
                    == spire_core::build_types::ProjectStructure::IdfApplication =>
            {
                crate::build::idf_projects::component_apis(root, application)
            }
            _ => String::new(),
        };
        // The composition is the **architecture, not advice**, and this is the failure it guards
        // against: a live run was handed the reviewed `actors` decomposition, the framework's idiom and
        // the library's own headers, and answered with one flat FreeRTOS poll loop plus its own
        // `Sps30`/`Sht20` classes. So the two things it did are named as prohibitions. Only an
        // application has a composition, so this is empty for everything else.
        let composition_rules = if composition_block.is_empty() {
            String::new()
        } else {
            "THE COMPOSITION IS THE ARCHITECTURE — implement it, do not approximate it:\n\
             - Write every unit above with the **framework's own idiom** (see FRAMEWORK). For `actors` \
             that is `spire::Actor<Message>` with `on_message`, spawned on a `spire::Scheduler` — \
             **not** a bare FreeRTOS `while (true)` task, and **not** an `xTaskCreate` per unit.\n\
             - The design's components belong to the **library**: include them and use them — a \
             component named `name` publishes `#include <name.hpp>`. Do **not** declare your own class \
             in `main/` for a component the design names: a second copy of the same driver is free to \
             disagree with the first.\n\
             - **Use only the methods the headers declare.** A component whose header offers only what a stub offers — `probe()`, `run()` — has **no read API yet**: write the `TODO` where the call belongs, exactly as the bus opening is left open, and **do not invent** `read(...)`, `push(...)` or any other method. A call the library does not declare is a file that does not compile, and the headers above are the whole API.\n"
                .to_string()
        };
        let prompt = format!(
            r#"You are filling an already-scaffolded {bs} project. The structure is LOCKED.
Write a JSON object of the form {{"steps": [...]}} where each step is one of
(write_source_file / create_directory / declare_dependencies / build / test / parse_and_validate / tool_call).
A step is {{"type": "<one of those>", ...its own fields}} — a `write_source_file` carries "path" and
"content", a `create_directory` carries "path". For example:

  {{"steps": [
    {{"type": "write_source_file", "path": "{source_dir}/main.cpp", "content": "// the composition goes here\n"}},
    {{"type": "build"}}
  ]}}

The step's fields sit BESIDE "type", not wrapped in another object.
to implement the goal INSIDE the existing skeleton.

STRUCTURE CONTRACT:
{structure}

{framework_block}
{composition_block}
{composition_rules}
{component_apis}
{framework_hints}
{library_hints}

RULES:
- You may write/modify files ONLY under the fill roots and create subdirectories beneath them.
- You may NOT write structural files.
- Dependencies may ONLY be added via declare_dependencies (server build/{bs_lower}, tool declare_dependencies, arguments: {{"path": "<root>/{dep_section}", "dependencies": [{{"name": "...", "version": "..."}}]}}).
- Declare each dependency exactly ONCE, against the single dependency manifest above — do NOT add the same dependency to multiple manifests.
- Respond with ONLY the JSON object {{"steps": [...]}} — no other text.
- CONCISION: keep the plan SMALL (at most ~8 steps). If the goal needs files, limit each write_source_file to its ESSENTIAL content: write each use/import line EXACTLY ONCE and NEVER repeat any block of lines.
- NEVER loop: if part of a file's source repeats a previous block verbatim, stop and emit it only once — a truncated or looping response is a hard failure.
- Keep each content string under ~1500 characters.

Project: name={project_name}, root={root}, goal={goal}
"#,
            bs = spec.build_system,
            bs_lower = spec.build_system.to_lowercase(),
            root = root_dir.display(),
            framework_block = framework_block,
            composition_block = composition_block,
            composition_rules = composition_rules,
            component_apis = component_apis,
            framework_hints = framework_hints,
            library_hints = library_hints,
            source_dir = spec
                .fill_roots
                .first()
                .map(String::as_str)
                .unwrap_or("main"),
        );
        info!(
            "[ProjectCreation] Fill: requesting LLM fill plan for {} (spec bs={}, platforms={:?})",
            project_name, spec.build_system, spec.platform_targets
        );

        // NO artificial client-side timeout. The LlmActor's HTTP client owns a
        // 120s transport timeout that fires only on a genuine transport
        // failure (connect/reset/body interruption) — never because a working
        // completion is merely slow. Verified live (2026-08-18): deepseek-chat
        // returns the JSON plan in ~7s; the earlier 90s deadline here is what
        // aborted otherwise-fine responses and caused the false "LLM
        // unavailable" fallback.
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = llm_tx
            .send(LlmMessage::Complete {
                prompt: prompt.clone(), // kept for the truncation retry below
                role: spire_core::subsystems::llm::llm::LlmModelRole::Planning,
                reply_to: t,
            })
            .await;

        // ATOMIC + FATAL: every failure path returns Err, so the caller writes
        // nothing to disk unless a real plan was produced.
        match r.await {
            Ok(Ok(response)) => {
                info!(
                    "[ProjectCreation] Fill: LLM responded — {} chars",
                    response.len()
                );
                // DIAGNOSTIC (temporary): log the FULL raw LLM plan JSON and
                // dump it to a file so the exact plan (and any duplicate /
                // detail-less steps) can be inspected instead of guessed at.
                tracing::info!(
                    "[ProjectCreation] Fill RAW PLAN JSON ({} chars):\n{}",
                    response.len(),
                    response
                );
                if let Some(log_dir) = spire_core::config::config_dir()
                    .join("logs")
                    .parent()
                    .map(|p| p.to_path_buf())
                {
                    let _ = std::fs::create_dir_all(&log_dir);
                    let _ = std::fs::write(log_dir.join("plan-raw.json"), &response);
                }
                match parse_fill_steps(&response) {
                    Some(parsed) => {
                        // DIAGNOSTIC: log every parsed step (id/type/desc/params).
                        for (i, s) in parsed.iter().enumerate() {
                            tracing::info!(
                                "[ProjectCreation] PARSED STEP [{}] id={} type={:?} desc={:?} params={}",
                                i,
                                s.id,
                                s.step_type,
                                s.description,
                                s.parameters
                            );
                        }
                        info!("[ProjectCreation] Fill: parsed {} steps", parsed.len());
                        Ok(PlanGenerationResult {
                            goal: goal.to_string(),
                            language: spec.build_system.clone(),
                            root_dir: root_dir.to_string_lossy().to_string(),
                            steps: parsed,
                            is_template: false,
                            fallback_reason: None,
                        })
                    }
                    None => {
                        warn!(
                            "[ProjectCreation] Fill: LLM response contained no usable steps ({})",
                            response.len()
                        );
                        // TEMPORARY DIAGNOSTIC: dump the exact rejected body so
                        // the real (non-deterministic) model output that broke
                        // parsing can be inspected and covered by a fixture —
                        // instead of guessing another shape. Removed once the
                        // structured-output fix is verified.
                        let _ = std::fs::create_dir_all(std::env::temp_dir().join("spire-fill"));
                        let dump_path = std::env::temp_dir()
                            .join("spire-fill")
                            .join("rejected-body.json");
                        let _ = std::fs::write(&dump_path, &response);
                        warn!(
                            "[ProjectCreation] Fill: rejected LLM body written to {} ({} bytes)",
                            dump_path.display(),
                            response.len()
                        );

                        // TRUNCATION / DEGENERATION RETRY (verified cause 2026-08-18):
                        // a repetitive-degeneration loop (same `use rmcp::…` lines
                        // emitted ~200×) hit the token ceiling, leaving a 14291-char
                        // body cut off mid-token → invalid JSON. Retry ONCE with a
                        // compaction instruction (smaller plan, short content, no
                        // repeated lines). If the second response also fails to parse,
                        // fail ATOMICALLY — no template, nothing scaffolded.
                        warn!(
                            "[ProjectCreation] Fill: retrying once with compaction (previous {} chars unusable)",
                            response.len()
                        );
                        let retry_prompt = format!(
                            "{prompt}\n\nIMPORTANT: your previous answer was truncated or unusable. \
Reply with a much SMALLER plan: at most 5 steps, each content string under 600 characters, \
and NEVER repeat any line or block."
                        );
                        let (t2, r2) = tokio::sync::oneshot::channel();
                        let _ = llm_tx
                            .send(LlmMessage::Complete {
                                prompt: retry_prompt,
                                role: spire_core::subsystems::llm::llm::LlmModelRole::Planning,
                                reply_to: t2,
                            })
                            .await;
                        if let Ok(Ok(retry_resp)) = r2.await {
                            if let Some(parsed2) = parse_fill_steps(&retry_resp) {
                                info!(
                                    "[ProjectCreation] Fill: retry parsed {} steps ({} chars)",
                                    parsed2.len(),
                                    retry_resp.len()
                                );
                                return Ok(PlanGenerationResult {
                                    goal: goal.to_string(),
                                    language: spec.build_system.clone(),
                                    root_dir: root_dir.to_string_lossy().to_string(),
                                    steps: parsed2,
                                    is_template: false,
                                    fallback_reason: Some(
                                        "First response was truncated/unusable; retried with a smaller plan"
                                            .to_string(),
                                    ),
                                });
                            }
                        }
                        Err(
                            "LLM produced a truncated/unusable plan (even after a compaction retry). \
                             Nothing was scaffolded."
                                .to_string(),
                        )
                    }
                }
            }
            Ok(Err(e)) => Err(format!("LLM planning failed: {}", e)),
            Err(_) => Err("LLM reply channel dropped while planning".to_string()),
        }
    }

    /// Resolve a build-system label from a language/user input string.
    fn build_system_for_language(language: &str) -> &'static str {
        match language.to_lowercase().as_str() {
            "swift" => "SwiftPM",
            "python" => "Python",
            "javascript" | "typescript" | "node" => "npm",
            "go" => "Go",
            "c++" | "cpp" | "c" | "meson" => "Meson",
            _ => "Cargo",
        }
    }

    /// The build-system label a scaffold reports: the **structure** decides first, because a project
    /// type can say what a language cannot — an ESP-IDF library and an ESP-IDF application are both
    /// "ESP-IDF", and neither is "C++" — and the language is the fallback.
    fn build_system_for(
        structure: spire_core::build_types::ProjectStructure,
        language: &str,
    ) -> &'static str {
        use spire_core::build_types::ProjectStructure;
        match structure {
            ProjectStructure::IdfLibrary | ProjectStructure::IdfApplication => "ESP-IDF",
            _ => Self::build_system_for_language(language),
        }
    }

    /// The four facts a scaffold request cannot be answered without, resolved from the request
    /// itself: **which config file** the build module is found by, the **library** an application is
    /// built against, the **build-system label** the spec reports, and whether the project is
    /// **embedded**.
    ///
    /// One function because this was two. The in-memory spec and the scaffold each mapped a *language*
    /// to a build file, and adding the ESP-IDF project types to only one of them is how a project
    /// ends up with `structure: "idf_library"` and `build_system: "Meson"` — which is exactly what
    /// happened, and how this was found. The next shape to be added now has one place to go.
    ///
    /// The ESP-IDF types are chosen by **structure**, not by language: their config file is
    /// `sdkconfig.defaults`, the one file in an IDF tree that is IDF's alone. `language: "cpp"` would
    /// otherwise route them to the Meson module, which has never heard of them.
    fn scaffold_routing(
        language: &str,
        structure: Option<spire_core::build_types::ProjectStructure>,
        embedded_root: Option<&Path>,
        embedded: bool,
    ) -> (&'static str, Option<String>, &'static str, bool) {
        use spire_core::build_types::ProjectStructure;
        match structure {
            // A component library is built against nothing, and is cross-compiled by definition.
            Some(ProjectStructure::IdfLibrary) => ("sdkconfig.defaults", None, "ESP-IDF", true),
            Some(ProjectStructure::IdfApplication) => (
                "sdkconfig.defaults",
                // "The project this one depends on" is the same fact the build module calls the
                // `library`, under the name the wizard already had for it.
                embedded_root.map(|root| root.to_string_lossy().to_string()),
                "ESP-IDF",
                true,
            ),
            _ => (
                match language.to_lowercase().as_str() {
                    "swift" => "Package.swift",
                    "python" => "pyproject.toml",
                    "javascript" | "typescript" | "node" => "package.json",
                    "go" => "go.mod",
                    "c++" | "cpp" | "c" | "meson" => "meson.build",
                    _ => "Cargo.toml",
                },
                None,
                Self::build_system_for_language(language),
                embedded,
            ),
        }
    }

    /// Return the `.gitignore` body for a build system. Every scaffolded
    /// project also ignores `.spire/` (project-local graph DB + logs) so it is
    /// never committed. Keyed on the build system (not a coarse language
    /// string): e.g. Cargo ignores `/target` only, so a legitimate `build/`
    /// source dir is never hidden.
    fn gitignore_for_build_system(build_system: &str) -> String {
        let body = match build_system.to_lowercase().as_str() {
            "cargo" => "/target/\n",
            "meson" => "build/\nbuilddir/\n_build/\nmeson-private/\nmeson-logs/\n",
            "python" => "__pycache__/\n*.egg-info/\ndist/\nbuild/\n.venv/\n",
            "npm" => "node_modules/\ndist/\n",
            "go" => "bin/\n",
            "swiftpm" => ".build/\n",
            "maven" => "target/\n",
            "gradle" => "build/\n.gradle/\n",
            "cmake" => "build/\nCMakeCache.txt\nCMakeFiles/\n",
            "make" => "*.o\n*.a\n*.so\n",
            _ => "",
        };
        format!("{body}\n# Spire project-local state\n.spire/\n")
    }

    /// Ensure `<root>` is a git repository with a language `.gitignore` and a
    /// committed scaffold baseline so LLM fill changes can be reviewed via
    /// `git diff` and rolled back to this commit. Non-fatal: errors are
    /// returned; the caller still succeeds with the scaffold.
    async fn ensure_scaffold_git(root: &PathBuf, build_system: &str) -> Result<String, String> {
        async fn run(root: &PathBuf, args: &[&str]) -> Result<String, String> {
            let out = tokio::process::Command::new("git")
                .current_dir(root)
                .args(args)
                .output()
                .await
                .map_err(|e| format!("git {} failed: {e}", args.join(" ")))?;
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() {
                Ok(text)
            } else {
                Err(text)
            }
        }

        // 1. Ensure a repo (idempotent — already-initialized dirs are untouched).
        if run(root, &["rev-parse", "--git-dir"]).await.is_err() {
            run(root, &["init", "-q"])
                .await
                .map_err(|e| format!("git init: {e}"))?;
        }

        // 2. Write/refresh the build-system .gitignore (always ignores .spire/).
        let _ = std::fs::write(
            root.join(".gitignore"),
            Self::gitignore_for_build_system(build_system),
        );

        // 3. Stage everything and commit the pristine scaffold baseline.
        run(root, &["add", "-A"])
            .await
            .map_err(|e| format!("git add: {e}"))?;
        let has_staged = run(root, &["diff", "--cached", "--quiet"]).await.is_err();
        if has_staged {
            if run(root, &["config", "user.email"]).await.is_err() {
                let _ = run(root, &["config", "user.email", "spire@localhost"]).await;
                let _ = run(root, &["config", "user.name", "Spire"]).await;
            }
            run(
                root,
                &["commit", "-q", "-m", "chore: scaffold project structure"],
            )
            .await
            .map_err(|e| format!("git commit: {e}"))?;
        }
        Ok("git initialized; scaffold baseline committed".to_string())
    }

    /// Resolve the structural spec for a new project without writing anything.
    // The arguments are the creation request's own fields plus the two the wizard adds: splitting
    // them into a struct would move the same list one line up, and every caller already has them.
    #[allow(clippy::too_many_arguments)]
    async fn scaffold_spec_in_memory(
        &self,
        project_name: &str,
        language: &str,
        platforms: &[String],
        structure: Option<spire_core::build_types::ProjectStructure>,
        // Where the library an **application** is built against lives.
        //
        // `None` for every other structure, and for an application whose caller has not named one —
        // which is a legitimate start ("No library named" is written into its build file) rather
        // than something to guess at: an app that invents its own dependency is a build failure
        // several steps later, in a place that names neither the app nor the library.
        embedded_root: Option<&Path>,
        embedded: bool,
        // The **design** this spec is resolved from, already decided by the caller — the framework the
        // design phase chose and the composition that goes with it, which is what the application's own
        // `CMakeLists.txt` states and the fill phase reads back.
        //
        // Resolved by the caller rather than here, and through the one resolver,
        // [`idf_projects::design_for_scaffold`](crate::build::idf_projects::design_for_scaffold): the
        // design a **plan** is built against and the design a **scaffold** writes have to be one design,
        // and they are asked for separately — so the rule that decides it lives in one place and both
        // legs call it. Nothing here reads the tree, which is what this function's promise is: the plan
        // phase has to be able to run before the tree exists.
        application: Option<crate::build::application_spec::ApplicationSpec>,
    ) -> Result<crate::subsystems::build::build_manager::ScaffoldSpec, String> {
        let (build_file, library, _build_system, embedded) =
            Self::scaffold_routing(language, structure, embedded_root, embedded);
        let (t, r) = oneshot::channel();
        self.build_manager_tx
            .send(BuildManagerMessage::ScaffoldBuildConfig {
                project_name: project_name.to_string(),
                goal: String::new(),
                build_file: build_file.to_string(),
                platforms: platforms.to_vec(),
                structure,
                embedded,
                library,
                application,
                reply_to: t,
            })
            .await
            .map_err(|e| e.to_string())?;
        let out = r
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;

        Ok(spec_from_scaffold_output(out, language))
    }

    pub fn set_project_analyzer(&mut self, tx: mpsc::Sender<ProjectAnalyzerMessage>) {
        self.project_analyzer_tx = Some(tx);
    }

    pub fn set_llm(&mut self, tx: mpsc::Sender<LlmMessage>) {
        self.llm_tx = Some(tx);
    }

    /// Wire the memory graph so the AppSpec requirements pass can persist the
    /// validated spec as a graph node (upsert keeps one stable node per app).
    pub fn set_memory_graph(&mut self, tx: mpsc::Sender<MemoryGraphMessage>) {
        self.memory_graph_tx = Some(tx);
    }

    /// Persist a validated AppSpec in the memory graph (best-effort — the
    /// spec is already returned to the caller; failures are logged, not fatal).
    /// Persist a validated AppSpec in the memory graph (best-effort — the spec
    /// is already returned to the caller; failures are logged, not fatal).
    /// Returns the stored node id when the upsert succeeded, so codegen can
    /// link generated artifacts to it.
    /// Persist a validated AppSpec in the memory graph (best-effort — the spec
    /// is already returned to the caller; failures are logged, not fatal).
    /// Stage 4: the spec is stored in its graph-native form
    /// ([`super::spec_persist`]) — an `appspec` anchor (name == project, with
    /// a rendered `spec_md` review copy) plus one node per decomposed spec
    /// node and one relationship per spec edge. The whole-JSON `spec` blob
    /// property is gone; the graph IS the spec.
    async fn store_app_spec_in_graph(
        &self,
        project_name: &str,
        goal: &str,
        spec: &AppSpec,
    ) -> Option<String> {
        let Some(mg_tx) = &self.memory_graph_tx else {
            return None;
        };
        let g = super::spec_graph::decompose(spec);
        super::spec_persist::store_spec_graph(mg_tx, project_name, goal, &g).await
    }

    /// Find the appspec node id for a project (upserted by the requirements
    /// pass; one stable node per (appspec, name)).
    async fn appspec_node_id(&self, project_name: &str) -> Option<String> {
        let Some(mg_tx) = &self.memory_graph_tx else {
            return None;
        };
        let (t, r) = tokio::sync::oneshot::channel();
        if mg_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Unknown".to_string()),
                subtype: Some("appspec".to_string()),
                name: Some(project_name.to_string()),
                limit: Some(1),
                reply_to: t,
            })
            .await
            .is_err()
        {
            return None;
        }
        match r.await {
            Ok(Ok(mut nodes)) => nodes.pop().map(|n| n.id),
            _ => None,
        }
    }

    /// Record codegen artifacts in the graph and link each to the project's
    /// appspec node (`GENERATED_FROM`), so the spec ↔ implementation trace is
    /// queryable. Best-effort: when the appspec node is missing (codegen ran
    /// before the requirements pass) or the graph is unwired, this is skipped.
    async fn store_generated_artifacts(&self, project_name: &str, spec: &AppSpec) {
        let Some(mg_tx) = &self.memory_graph_tx else {
            return;
        };
        let Some(appspec_id) = self.appspec_node_id(project_name).await else {
            warn!(
                "[ProjectCreation] Codegen artifacts NOT linked: no appspec node for '{}' \
                 (run createProject/GenerateSpec first)",
                project_name
            );
            return;
        };

        let files = crate::subsystems::project::spec_codegen::generated_files(spec, project_name);
        for f in &files {
            let now = chrono::Utc::now();
            let node = AttrNode {
                id: uuid::Uuid::new_v4().to_string(),
                node_type: "Unknown".to_string(),
                subtype: Some("generated_file".to_string()),
                name: f.path.clone(),
                description: Some(format!("codegen artifact for {project_name}")),
                properties: std::collections::HashMap::from([
                    ("path".to_string(), serde_json::json!(f.path)),
                    ("spec".to_string(), serde_json::json!(project_name)),
                ]),
                embedding_id: None,
                created_at: now,
                updated_at: now,
                version: 1,
            };
            let (t, r) = tokio::sync::oneshot::channel();
            if mg_tx
                .send(MemoryGraphMessage::MergeAttrNode { node, reply_to: t })
                .await
                .is_err()
            {
                warn!("[ProjectCreation] codegen artifact store: graph unavailable");
                return;
            }
            match r.await {
                Ok(Ok(stored)) => {
                    let edge_from = stored.id;
                    let (et, er) = tokio::sync::oneshot::channel();
                    let _ = mg_tx
                        .send(MemoryGraphMessage::CreateRelationship {
                            rel: spire_core::models::memory_graph::RelationshipInput {
                                edge_type:
                                    spire_core::models::memory_graph::RelationshipType::Custom(
                                        "GENERATED_FROM".to_string(),
                                    ),
                                from_id: edge_from,
                                to_id: appspec_id.clone(),
                                properties: None,
                                weight: None,
                            },
                            reply_to: et,
                        })
                        .await;
                    match er.await {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => warn!("[ProjectCreation] codegen link failed: {e}"),
                        Err(_) => {}
                    }
                }
                Ok(Err(e)) => warn!("[ProjectCreation] codegen artifact store failed: {e}"),
                Err(e) => warn!("[ProjectCreation] codegen artifact reply lost: {e}"),
            }
        }
    }

    /// AppSpec requirements pass (SpireApp, LLM): derive a VALIDATED AppSpec
    /// JSON contract from the goal (self-healed against `validate()`). Nothing
    /// is written to disk — the later fill/codegen phase consumes the spec.
    async fn generate_app_spec(&self, project_name: &str, goal: &str) -> Result<AppSpec, String> {
        let llm_tx = match &self.llm_tx {
            Some(tx) => tx.clone(),
            None => {
                return Err(
                    "LLM unavailable — the project creator is not connected to the LLM service (wiring, not a missing API key)."
                        .to_string(),
                )
            }
        };
        let hints = crate::build::generic_helpers::spire_framework_hints();
        let call = |prompt: String| {
            let tx = llm_tx.clone();
            async move {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = tx
                    .send(LlmMessage::Complete {
                        prompt,
                        role: spire_core::subsystems::llm::llm::LlmModelRole::Planning,
                        reply_to: t,
                    })
                    .await;
                match r.await {
                    Ok(Ok(resp)) => Ok(resp),
                    Ok(Err(e)) => Err(format!("LLM actor error: {e}")),
                    Err(e) => Err(format!("LLM response lost: {e}")),
                }
            }
        };
        let spec = super::spec_gen::generate_app_spec(project_name, goal, &hints, call)
            .await
            .map_err(|e| e.to_string())?;
        // The graph is Spire's single source of truth: persist the validated
        // spec (upsert by name) so later codegen can link artifacts to it.
        self.store_app_spec_in_graph(project_name, goal, &spec)
            .await;
        Ok(spec)
    }

    // ── Plan generation ───────────────────────────────────────────────────

    /// Try LLM-driven plan generation first, then fall back to the deterministic
    /// template if the LLM is unavailable or returns unparseable JSON.
    async fn generate_plan_async(
        &self,
        goal: &str,
        // A path rather than a `PathBuf`: it is read, and passed on to the planners, which take a
        // directory to plan against.
        root_dir: &Path,
        language: &str,
        platforms: &[String],
        structure: Option<spire_core::build_types::ProjectStructure>,
    ) -> PlanGenerationResult {
        // SpireApp: deterministic monorepo scaffold — the structure itself is
        // fixed (Cargo workspace + SwiftUI), so the plan is the scaffold's own
        // file writes plus a parse+build gate. No LLM needed for this phase.
        if structure == Some(spire_core::build_types::ProjectStructure::SpireApp) {
            return self
                .spire_app_template_plan(goal, root_dir, language, platforms)
                .await;
        }
        if let Some(llm_tx) = &self.llm_tx {
            let project_name = root_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "my_project".to_string());

            let build_file = match language.to_lowercase().as_str() {
                "swift" => "Package.swift",
                "python" => "pyproject.toml",
                "javascript" | "typescript" | "node" => "package.json",
                "go" => "go.mod",
                _ => "Cargo.toml",
            };

            let source_dir = match language.to_lowercase().as_str() {
                "swift" => "Sources",
                "python" => "src",
                "javascript" | "typescript" | "node" => "src",
                "go" => "",
                _ => "src",
            };

            let source_file = match language.to_lowercase().as_str() {
                "swift" => "main.swift",
                "python" => "main.py",
                "javascript" | "typescript" | "node" => "index.js",
                "go" => "main.go",
                _ => "main.rs",
            };

            let source_path = if source_dir.is_empty() {
                source_file.to_string()
            } else {
                format!("{}/{}", source_dir, source_file)
            };

            // Discover the connected MCP server tools so the LLM can issue
            // generic tool_call steps (modular — never hardcoded to a server).
            let mut tool_catalog = String::from("No external MCP tools connected.\n");
            let (tc, rc) = tokio::sync::oneshot::channel();
            let _ = self
                .mcp_client_tx
                .send(McpClientMessage::GetConnectedServersWithTools { reply_to: tc })
                .await;
            if let Ok(connected) = rc.await {
                if !connected.is_empty() {
                    tool_catalog.clear();
                    for (server, tools) in &connected {
                        tool_catalog.push_str(&format!("**{}**\n", server));
                        for tool in tools {
                            tool_catalog.push_str(&format!("- {}\n", tool.name));
                        }
                    }
                }
            }

            // Also expose in-process language-module tools (e.g. crates.io
            // REST tools in CargoBuildModule) so the LLM can invoke them
            // directly via tool_call with server_name "build/...".
            let (lt, lr) = tokio::sync::oneshot::channel();
            let _ = self
                .build_manager_tx
                .send(BuildManagerMessage::ListTools { reply_to: lt })
                .await;
            if let Ok(tools) = lr.await {
                if !tools.is_empty() {
                    tool_catalog.push_str("**build (in-process modules)**\n");
                    for t in &tools {
                        tool_catalog.push_str(&format!("- {}\n", t.name));
                    }
                }
            }

            let system_prompt = format!(
                r#"You are the Spire project creation planner. Given a project goal, language, and project name,
generate a JSON array of steps to scaffold a new project. Use only these step types:
create_directory, write_build_config, add_dependency, write_source_file, tool_call, parse_and_validate, build, test.

IMPORTANT RULES:
- Respond with ONLY a JSON array. No markdown fencing, no commentary.
- Step fields: {{"id": "step-N", "step_type": "...", "description": "...", "parameters": {{...}}}}
- The first step (if a source dir exists) should create the source directory ({source_dir}/).
- The second step should write the build config file ({build_file}).
- Use tool_call steps (parameters: server_name, tool_name, arguments) to query external MCP servers for dependency info.
- Include a write_source_file step for {source_path}.
- Include a final build step.

Available MCP tools:
{tool_catalog}

Project:
- Language: {language}
- Project name: {project_name}
- Goal: {goal}
- Build file: {build_file}
- Source dir: {source_dir} (empty = files at root)
- Source file: {source_path}
"#,
                tool_catalog = tool_catalog,
            );

            info!(
                "[ProjectCreation] LLM plan prompt ({} chars): {}",
                system_prompt.len(),
                system_prompt.chars().take(300).collect::<String>()
            );

            let (t, r) = tokio::sync::oneshot::channel();
            let _ = llm_tx
                .send(LlmMessage::Complete {
                    prompt: system_prompt,
                    role: spire_core::subsystems::llm::llm::LlmModelRole::Planning,
                    reply_to: t,
                })
                .await;

            // Bound the LLM wait — if the model is slow/unconfigured/misconfigured,
            // fall back to the template plan instead of hanging the UI.
            //
            // tokio::time::timeout(fut).await = Result<T, Elapsed>
            // r = oneshot<Result<String, ActorError>> → r.await = Result<Result<String, ActorError>, RecvError>
            // timeout(fut).await: Result<T, Elapsed> where
            //   T = r.await: Result<Result<String, ActorError>, RecvError>
            // So the outer Ok yields a Result<Result<...>, ...>; flatten both layers.
            let llm_reply = tokio::time::timeout(std::time::Duration::from_secs(120), r).await;
            let response_str: Option<String> = match llm_reply {
                Ok(inner) => inner.ok().and_then(|inner2| inner2.ok()),
                Err(_elapsed) => {
                    warn!(
                        "[ProjectCreation] LLM plan response timed out - falling back to template"
                    );
                    None
                }
            };

            if let Some(response) = response_str {
                info!(
                    "[ProjectCreation] LLM plan response ({} chars): {}",
                    response.len(),
                    response.chars().take(500).collect::<String>()
                );
                // Try to parse the LLM response as a JSON array of steps.
                let trimmed = response.trim();
                let json_str = trimmed
                    .trim_start_matches("```json")
                    .trim_start_matches("```")
                    .trim_end_matches("```")
                    .trim();

                if let Ok(steps) = serde_json::from_str::<Vec<serde_json::Value>>(json_str) {
                    let parsed_steps: Vec<CreationStep> = steps
                        .into_iter()
                        .enumerate()
                        .filter_map(|(i, v)| {
                            let step_type_str = v
                                .get("step_type")
                                .or_else(|| v.get("stepType"))
                                .and_then(|s| s.as_str())?;
                            let step_type = match step_type_str {
                                "create_directory" => CreationStepType::CreateDirectory,
                                "write_build_config" => CreationStepType::WriteBuildConfig,
                                "add_dependency" => CreationStepType::AddDependency,
                                "write_source_file" => CreationStepType::WriteSourceFile,
                                "parse_and_validate" => CreationStepType::ParseAndValidate,
                                "build" => CreationStepType::Build,
                                "test" => CreationStepType::Test,
                                "tool_call" => CreationStepType::ToolCall,
                                _ => return None,
                            };
                            let id = v
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&format!("step-{}", i + 1))
                                .to_string();
                            let description = v
                                .get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let parameters = v
                                .get("parameters")
                                .cloned()
                                .unwrap_or(serde_json::json!({}));
                            Some(CreationStep {
                                id,
                                step_type,
                                description,
                                status: StepStatus::Pending,
                                parameters,
                                result: None,
                            })
                        })
                        .collect();

                    if !parsed_steps.is_empty() {
                        // Fill in defaults the LLM often omits: the target path
                        // (build file / source file) and a source-content placeholder.
                        let filled: Vec<CreationStep> = parsed_steps
                            .into_iter()
                            .map(|mut step| {
                                match step.step_type {
                                    CreationStepType::WriteBuildConfig => {
                                        if step.parameters.get("path").is_none() {
                                            step.parameters["path"] = serde_json::json!(build_file);
                                        }
                                    }
                                    CreationStepType::WriteSourceFile => {
                                        if step.parameters.get("path").is_none() {
                                            step.parameters["path"] =
                                                serde_json::json!(source_path);
                                        }
                                        if step.parameters.get("content").is_none() {
                                            step.parameters["content"] =
                                                serde_json::json!("// LLM-generated source");
                                        }
                                    }
                                    _ => {}
                                }
                                step
                            })
                            .collect();
                        info!(
                            "[ProjectCreation] LLM generated {} plan steps",
                            filled.len()
                        );
                        return PlanGenerationResult {
                            goal: goal.to_string(),
                            language: language.to_string(),
                            root_dir: root_dir.to_string_lossy().to_string(),
                            steps: filled,
                            is_template: false,
                            fallback_reason: None,
                        };
                    }
                }
                warn!("[ProjectCreation] LLM plan response unparseable — falling back to template");
            }
        }
        self.generate_plan(goal, root_dir, language, platforms)
    }

    /// Deterministic SpireApp plan: scaffold the monorepo via the build module's
    /// `scaffold_layout`, then emit one step per file (structural → build-config
    /// write, fillable → source write) plus a parse+build gate. Never calls the
    /// LLM — the fill phase (`FillProject`) fills the scaffold from the goal.
    async fn spire_app_template_plan(
        &self,
        goal: &str,
        // A path rather than a `PathBuf`: it is only ever read — for the project's name, and by the
        // plan that is built from the spec below.
        root_dir: &Path,
        language: &str,
        platforms: &[String],
    ) -> PlanGenerationResult {
        let project_name = root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "spire-app".to_string());
        let spec = self
            .scaffold_spec_in_memory(
                &project_name,
                language,
                platforms,
                Some(spire_core::build_types::ProjectStructure::SpireApp),
                // A Spire app is host-only: it depends on no HAL project.
                None,
                false,
                // Nor is it an ESP-IDF application, so it states no framework.
                None,
            )
            .await
            .unwrap_or_else(|e| {
                warn!("[ProjectCreation] SpireApp scaffold spec failed: {e}");
                crate::subsystems::build::build_manager::ScaffoldSpec {
                    structural_files: vec!["Cargo.toml".to_string()],
                    fill_roots: vec!["crates".to_string(), "ui".to_string()],
                    dependency_sections: vec!["Cargo.toml".to_string()],
                    platform_targets: vec!["host".to_string()],
                    build_system: "Cargo".to_string(),
                    files: vec![],
                    structure: spire_core::build_types::ProjectStructure::SpireApp,
                    embedded: false,
                    // A Spire app states no design and scaffolds nothing over one.
                    design_warning: None,
                }
            });

        scaffold_plan_from_spec(
            goal,
            root_dir,
            language,
            spec,
            "SpireApp structure — deterministic monorepo scaffold",
        )
    }

    /// Generate a plan for a new project. In v1 this is a deterministic
    /// template-based expansion; later it will call the LLM.
    fn generate_plan(
        &self,
        goal: &str,
        root_dir: &Path,
        language: &str,
        _platforms: &[String],
    ) -> PlanGenerationResult {
        let project_name = root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "my_project".to_string());

        let (build_file, source_dir, source_file) = match language.to_lowercase().as_str() {
            "swift" => ("Package.swift", "Sources", "main.swift"),
            "python" => ("pyproject.toml", "src", "main.py"),
            "javascript" | "typescript" | "node" => ("package.json", "src", "index.js"),
            "go" => ("go.mod", "", "main.go"),
            _ => ("Cargo.toml", "src", "main.rs"), // Rust default
        };

        let mut steps = Vec::new();

        // 1. Create the source directory
        if !source_dir.is_empty() {
            steps.push(CreationStep {
                id: "step-1".into(),
                step_type: CreationStepType::CreateDirectory,
                description: format!("Create the {} source directory", source_dir),
                status: StepStatus::Pending,
                parameters: serde_json::json!({ "path": format!("{}/", source_dir) }),
                result: None,
            });
        }

        // 2. Write the build config — delegate to the language module's owned
        // scaffold (single source of truth in spire-modules). Fall back to the
        // legacy local generator only if the module has no scaffold.
        let build_config = self.generate_build_config(language, &project_name, goal);
        steps.push(CreationStep {
            id: if source_dir.is_empty() {
                "step-1".into()
            } else {
                "step-2".into()
            },
            step_type: CreationStepType::WriteBuildConfig,
            description: format!("Write {} project configuration", build_file),
            status: StepStatus::Pending,
            parameters: serde_json::json!({
                "path": build_file,
                "content": build_config
            }),
            result: None,
        });

        // 3. Add dependencies (via crates-io MCP — placeholder: the LLM fills these)
        steps.push(CreationStep {
            id: if source_dir.is_empty() {
                "step-2".into()
            } else {
                "step-3".into()
            },
            step_type: CreationStepType::AddDependency,
            description: "Discover and add project dependencies via crates-io MCP".into(),
            status: StepStatus::Pending,
            parameters: serde_json::json!({
                "goal": goal,
                "language": language
            }),
            result: None,
        });

        // 4. Write the main source file
        let source_path = if source_dir.is_empty() {
            source_file.to_string()
        } else {
            format!("{}/{}", source_dir, source_file)
        };
        steps.push(CreationStep {
            id: if source_dir.is_empty() {
                "step-3".into()
            } else {
                "step-4".into()
            },
            step_type: CreationStepType::WriteSourceFile,
            description: format!("Write the main source file {}", source_path),
            status: StepStatus::Pending,
            parameters: serde_json::json!({
                "path": source_path,
                "content": "# TODO: LLM-generated source\n".to_string()
            }),
            result: None,
        });

        // 5. Parse and validate
        steps.push(CreationStep {
            id: if source_dir.is_empty() {
                "step-4".into()
            } else {
                "step-5".into()
            },
            step_type: CreationStepType::ParseAndValidate,
            description: "Parse source files to validate syntax via AST modules".into(),
            status: StepStatus::Pending,
            parameters: serde_json::json!({
                "paths": [source_path]
            }),
            result: None,
        });

        // 6. Build
        steps.push(CreationStep {
            id: if source_dir.is_empty() {
                "step-5".into()
            } else {
                "step-6".into()
            },
            step_type: CreationStepType::Build,
            description: "Build the project to verify it compiles".into(),
            status: StepStatus::Pending,
            parameters: serde_json::json!({}),
            result: None,
        });

        PlanGenerationResult {
            goal: goal.to_string(),
            language: language.to_string(),
            root_dir: root_dir.to_string_lossy().to_string(),
            steps,
            is_template: true, // deterministic template fallback (GeneratePlan)
            fallback_reason: Some("LLM unavailable — using deterministic template".to_string()),
        }
    }

    /// Generate a minimal build config for the given language.
    fn generate_build_config(&self, language: &str, project_name: &str, goal: &str) -> String {
        match language.to_lowercase().as_str() {
            "swift" => format!(
                "// swift-tools-version: 5.10\nimport PackageDescription\n\nlet package = Package(\n    name: \"{}\",\n    targets: [\n        .executableTarget(\n            name: \"{}\",\n            path: \"Sources\"\n        )\n    ]\n)\n",
                project_name, project_name
            ),
            "python" => format!(
                "[project]\nname = \"{}\"\nversion = \"0.1.0\"\ndescription = \"{}\"\nrequires-python = \">=3.11\"\ndependencies = []\n\n[build-system]\nrequires = [\"setuptools>=68\"]\nbuild-backend = \"setuptools.build_meta\"\n",
                project_name, goal
            ),
            "javascript" | "typescript" | "node" => format!(
                "{{\n  \"name\": \"{}\",\n  \"version\": \"0.1.0\",\n  \"description\": \"{}\",\n  \"main\": \"src/index.js\",\n  \"scripts\": {{\n    \"start\": \"node src/index.js\",\n    \"build\": \"npm run tsx -- src/index.ts\"\n  }},\n  \"dependencies\": {{}}\n}}\n",
                project_name, goal
            ),
            "go" => format!(
                "module {}\n\ngo 1.22\n",
                project_name
            ),
            _ => format!(
                "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"{}\"\n\n[dependencies]\n",
                project_name, goal
            ),
        }
    }

    // ── Step execution ─────────────────────────────────────────────────────

    async fn execute_step(
        &self,
        root_dir: &PathBuf,
        step: &CreationStep,
    ) -> Result<StepExecutionResult, String> {
        // Resolve a plan path to an absolute file target.
        //
        // The LLM emits ABSOLUTE paths (e.g.
        // `/Users/steve/naturesense/ai-traps-mcp/core/src/lib.rs`). The old
        // `root_dir.join(p.trim_start_matches('/'))` turned that into
        // `…/ai-traps-mcp/Users/steve/naturesense/…` — a phantom nested path —
        // so the plan's file content never reached the real file
        // (verified: plan-raw showed `pub fn add(...)` while the on-disk file
        // kept the scaffold stub). Fix: use the absolute path directly when
        // `p` is absolute; otherwise join against `root_dir` as before.
        let full_path = |p: &str| {
            let path = std::path::Path::new(p);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root_dir.join(p)
            }
        };

        // Normalize a plan path relative to `root_dir` for the structural guard.
        // The LLM emits ABSOLUTE paths, but `trim_start_matches('/')` produced
        // `Users/steve/…/src/main.rs` — which never matched the fill root `src`,
        // so every absolute-path write was rejected and the plan's content never
        // landed on disk (verified: Cargo.toml kept only rmcp+tokio; main.rs
        // stayed the scaffold stub). Strip the root prefix first.
        let normalize_rel = |p: &str| -> String {
            std::path::Path::new(p)
                .strip_prefix(root_dir)
                .map(|r| r.to_string_lossy().trim_start_matches('/').to_string())
                .unwrap_or_else(|_| p.trim_start_matches('/').to_string())
        };

        let result = match step.step_type {
            CreationStepType::CreateDirectory => {
                let path = step
                    .parameters
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // STRUCTURAL GUARD: creating a NEW top-level directory under the
                // scaffold root (but outside a fill root) would alter structure.
                // The LLM may create subdirectories ONLY under an existing fill root.
                if let Some(spec) = &self.active_spec {
                    let rel = normalize_rel(&path).trim_end_matches('/').to_string();
                    let is_under_root = spec.fill_roots.iter().any(|r| {
                        rel.as_str() == r.as_str()
                            || rel.starts_with(&format!("{}/", r.trim_end_matches('/')))
                    });
                    if !is_under_root
                        && !spec
                            .structural_files
                            .iter()
                            .any(|s| s.starts_with(&format!("{}/", rel)))
                    {
                        return Ok(StepExecutionResult {
                            step_id: step.id.clone(),
                            success: false,
                            message: format!(
                                "Structural guard: cannot create directory '{}' — only under fill roots {:?}",
                                path, spec.fill_roots
                            ),
                        });
                    }
                }
                info!("[ProjectCreation] Creating directory: {}", path);
                // Use FilesystemModule::CallTool to create the directory
                let full_dir = full_path(&path).to_string_lossy().to_string();
                info!(
                    "[ProjectCreation] TOOL call filesystem_create_directory path={}",
                    full_dir
                );
                let (t, r) = tokio::sync::oneshot::channel();
                self.filesystem_tx
                    .send(spire_core::modules::FilesystemMessage::CallTool {
                        tool_name: "filesystem_create_directory".to_string(),
                        args: serde_json::json!({ "path": full_dir }),
                        reply_to: t,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                let resp = r.await.map_err(|e| e.to_string())?;
                // Check response for errors
                if let Some(err) = resp.get("Err").and_then(|v| v.as_str()) {
                    Ok((
                        format!("Failed to create directory {}: {}", path, err),
                        false,
                    ))
                } else {
                    Ok((format!("Created directory {}", path), true))
                }
            }

            // HAL ENTRY POINT (Stage 0 gate): write + validate the approved
            // contract header ONLY. The contract is the binding interface, so
            // it must be human-approved: `summarize_hal_header` rejects the
            // write (nothing touches disk) when the header is not a genuine
            // abstract class with public pure-virtual methods (canonical shape
            // includes `virtual ~ClassName() = default;`).
            CreationStepType::WriteHalContract => {
                let path = step
                    .parameters
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = step
                    .parameters
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let summary = match crate::build::generic_helpers::summarize_hal_header(&content) {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(StepExecutionResult {
                            step_id: step.id.clone(),
                            success: false,
                            message: format!(
                                "HAL contract rejected: {e}. The contract must be an abstract class with public pure-virtual methods (include `virtual ~X();`) — nothing was written to {}.",
                                path
                            ),
                        });
                    }
                };
                // Structural guard: a contract header is a locked, structural
                // file (hal/api/*) — the wizard writes it once via this step.
                let full_write = full_path(&path);
                let _ = &full_write;
                info!(
                    "[ProjectCreation] HAL contract validated ({}): {}",
                    path, summary
                );
                let full_write_path = full_path(&path).to_string_lossy().to_string();
                let (t, r) = tokio::sync::oneshot::channel();
                self.filesystem_tx
                    .send(spire_core::modules::FilesystemMessage::WriteFile {
                        path: PathBuf::from(full_write_path.clone()),
                        content: content.clone(),
                        reply_to: t,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                match r.await {
                    Ok(Ok(_)) => Ok((
                        format!("Wrote validated HAL contract {} ({})", path, summary),
                        true,
                    )),
                    Ok(Err(e)) => Ok((format!("Failed to write {}: {}", path, e), false)),
                    Err(e) => Ok((format!("Write response lost: {}", e), false)),
                }
            }

            CreationStepType::WriteBuildConfig
            | CreationStepType::WriteSourceFile
            | CreationStepType::WriteHalImplementation => {
                let path = step
                    .parameters
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // STRUCTURAL GUARD: the LLM may write source files ONLY under a
                // fill root (or create new subdirs beneath it). Writing
                // structural files (Cargo.toml, meson.build, meson_options.txt,
                // .cargo/config.toml, member Cargo.tomls, build.rs) is denied —
                // the LLM touches build-configs only via declare_dependencies.
                // HAL implementation files live under hal/implementations/<plat>
                // (a fill root), so they pass the same guard as source files.
                if let Some(spec) = &self.active_spec {
                    let rel = normalize_rel(&path);
                    let structural_hit = spec.structural_files.iter().any(|s| {
                        s.as_str() == rel.as_str()
                            || rel.starts_with(&format!("{}/", s.trim_end_matches('/')))
                    });
                    let under_fill_root = spec.fill_roots.iter().any(|r| {
                        let r = r.trim_end_matches('/');
                        rel == r || rel.starts_with(&format!("{}/", r))
                    });
                    if structural_hit {
                        return Ok(StepExecutionResult {
                            step_id: step.id.clone(),
                            success: false,
                            message: format!(
                                "Structural guard: '{}' is a locked structural file (build config / structure). Declare dependencies via declare_dependencies instead.",
                                path
                            ),
                        });
                    }
                    if !under_fill_root && !spec.fill_roots.is_empty() {
                        return Ok(StepExecutionResult {
                            step_id: step.id.clone(),
                            success: false,
                            message: format!(
                                "Structural guard: '{}' is not under a fill root {:?} — you may only modify files inside the scaffolded source tree.",
                                path, spec.fill_roots
                            ),
                        });
                    }
                }
                let content = step
                    .parameters
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                info!(
                    "[ProjectCreation] TOOL call filesystem_write path={} content_len={}",
                    path,
                    content.len()
                );
                let full_write_path = full_path(&path).to_string_lossy().to_string();
                let (t, r) = tokio::sync::oneshot::channel();
                self.filesystem_tx
                    .send(spire_core::modules::FilesystemMessage::WriteFile {
                        path: PathBuf::from(full_write_path.clone()),
                        content: content.clone(),
                        reply_to: t,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                match r.await {
                    Ok(Ok(_)) => Ok((format!("Wrote {}", path), true)),
                    Ok(Err(e)) => Ok((format!("Failed to write {}: {}", path, e), false)),
                    Err(e) => Ok((format!("Write response lost: {}", e), false)),
                }
            }

            CreationStepType::ToolCall => {
                // Generic MCP tool invocation driven by the plan. Parameters:
                //   server_name: String  (e.g. "cratesio-mcp")
                //   tool_name:   String  (e.g. "search_crates")
                //   arguments:   object  (tool args)
                let server_name = step
                    .parameters
                    .get("server_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = step
                    .parameters
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = step
                    .parameters
                    .get("arguments")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();

                if server_name.is_empty() || tool_name.is_empty() {
                    Ok((
                        "ToolCall step missing server_name or tool_name".to_string(),
                        false,
                    ))
                } else {
                    // Enforce per-language whitelist: look up the server's
                    // declared allowed_tools from the build modules. If the
                    // module declares a non-empty whitelist, the requested
                    // tool MUST be in it.
                    let (lt, lr) = tokio::sync::oneshot::channel();
                    let _ = self
                        .build_manager_tx
                        .send(BuildManagerMessage::ListModules { reply_to: lt })
                        .await;
                    let mut allowed_here: Vec<String> = Vec::new();
                    if let Ok(modules) = lr.await {
                        for cap in modules.iter() {
                            for dep in cap.mcp_servers.iter() {
                                if dep.name == server_name {
                                    if dep.allowed_tools.is_empty() {
                                        // Empty whitelist = legacy server, allow all.
                                        allowed_here.clear();
                                        allowed_here.push("*".to_string());
                                    } else {
                                        allowed_here.extend(dep.allowed_tools.clone());
                                    }
                                }
                            }
                        }
                    }
                    // The built-in "spire" pseudo-server hosts Spire's own
                    // internal tools (system/status, project/query, etc.) —
                    // always trusted, never subject to the external whitelist.
                    let is_allowed = server_name == "spire"
                        || allowed_here.contains(&"*".to_string())
                        || allowed_here.contains(&tool_name);
                    if !is_allowed {
                        Ok((
                            format!(
                                "Tool {}/{} is not on the whitelist for this language module",
                                server_name, tool_name
                            ),
                            false,
                        ))
                    } else {
                        info!(
                            "[ProjectCreation] TOOL call {} @ {}",
                            tool_name, server_name
                        );
                        if server_name.starts_with("build/") {
                            // In-process language-module tool (e.g. build/cargo ->
                            // CargoBuildModule). No external MCP process involved.
                            let (t, r) = tokio::sync::oneshot::channel();
                            let _ = self
                                .build_manager_tx
                                .send(BuildManagerMessage::CallTool {
                                    tool_name: tool_name.clone(),
                                    args: serde_json::Value::Object(arguments),
                                    reply_to: t,
                                })
                                .await;
                            match r.await {
                                Ok(res) => {
                                    let is_error = res
                                        .get("result")
                                        .and_then(|r| r.get("isError"))
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    let text = res
                                        .get("result")
                                        .and_then(|r| r.get("content"))
                                        .and_then(|c| c.as_array())
                                        .and_then(|a| a.first())
                                        .and_then(|x| x.get("text"))
                                        .and_then(|t| t.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    if is_error {
                                        Ok((
                                            format!(
                                                "Tool {}/{} returned error: {}",
                                                server_name, tool_name, text
                                            ),
                                            false,
                                        ))
                                    } else {
                                        Ok((text, true))
                                    }
                                }
                                Err(e) => Ok((format!("tool call lost: {}", e), false)),
                            }
                        } else {
                            // External MCP server tool.
                            let (t, r) = tokio::sync::oneshot::channel();
                            let _ = self
                                .mcp_client_tx
                                .send(McpClientMessage::CallTool {
                                    server_name: server_name.clone(),
                                    tool_name: tool_name.clone(),
                                    arguments: Some(arguments),
                                    reply_to: t,
                                })
                                .await;
                            match r.await {
                                Ok(Ok(result)) => {
                                    let text = result
                                        .content
                                        .iter()
                                        .filter_map(|c| {
                                            if let rust_mcp_sdk::schema::ContentBlock::TextContent(
                                                tc,
                                            ) = c
                                            {
                                                Some(tc.text.clone())
                                            } else {
                                                None
                                            }
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    if result.is_error.unwrap_or(false) {
                                        Ok((
                                            format!(
                                                "Tool {}/{} returned error: {}",
                                                server_name, tool_name, text
                                            ),
                                            false,
                                        ))
                                    } else {
                                        Ok((text, true))
                                    }
                                }
                                _ => Ok((
                                    format!(
                                        "MCP tool {}/{} unavailable (server not connected)",
                                        server_name, tool_name
                                    ),
                                    false,
                                )),
                            }
                        }
                    }
                }
            }

            CreationStepType::AddDependency => {
                // The FILL plan declares explicit dependencies:
                //   { "path": "<root>/Cargo.toml",
                //     "dependencies": [{"name": "tokio", "version": "1"}, ...] }
                // Write each into the declared manifest (workspace-root →
                // [workspace.dependencies], single crate → [dependencies]) so
                // the plan's dependencies actually land in Cargo.toml — no more
                // discarding them (previously only a crates-io search note was
                // produced, so [dependencies] stayed empty).
                let deps = step
                    .parameters
                    .get("dependencies")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let dep_path = step
                    .parameters
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if !deps.is_empty() && dep_path.is_some() {
                    let manifest = full_path(dep_path.as_deref().unwrap());
                    match add_dependencies_to_manifest(&manifest, &deps) {
                        Ok(added) => Ok((
                            format!("Added {} dependency(s) to {}", added, manifest.display()),
                            true,
                        )),
                        Err(e) => Ok((
                            format!(
                                "Failed to add dependencies to {}: {}",
                                manifest.display(),
                                e
                            ),
                            false,
                        )),
                    }
                } else {
                    // Query the crates-io MCP server (rust-docs-mcp) for dependency
                    // suggestions matching the goal. The server exposes tools under
                    // the "crates-io" prefix (e.g. crates-io/search).
                    let goal = step
                        .parameters
                        .get("goal")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let language = step
                        .parameters
                        .get("language")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Rust")
                        .to_string();

                    let (t, r) = tokio::sync::oneshot::channel();
                    let mut args = serde_json::Map::new();
                    if !goal.is_empty() {
                        args.insert("query".to_string(), serde_json::json!(goal));
                    }
                    info!(
                        "[ProjectCreation] TOOL call crates-io/search query={}",
                        goal
                    );
                    let _ = self
                        .mcp_client_tx
                        .send(McpClientMessage::CallTool {
                            server_name: "crates-io".to_string(),
                            tool_name: "search".to_string(),
                            arguments: Some(args),
                            reply_to: t,
                        })
                        .await;

                    info!("[ProjectCreation] crates-io search returned result");
                    match r.await {
                        Ok(Ok(result)) => {
                            // Extract the tool result text from the MCP response.
                            let text = result
                                .content
                                .iter()
                                .filter_map(|c| {
                                    if let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = c {
                                        Some(tc.text.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            if text.is_empty() {
                                Ok(("Crates-io MCP returned no suggestions".to_string(), true))
                            } else {
                                Ok((
                                    format!(
                                        "Discovered dependencies for {} via crates-io MCP:\n{}",
                                        language, text
                                    ),
                                    true,
                                ))
                            }
                        }
                        _ => {
                            // MCP not connected yet — informational response only.
                            Ok((
                            format!(
                                "Dependency discovery via crates-io MCP deferred (server may not be connected) for {}",
                                language
                            ),
                            true,
                        ))
                        }
                    }
                }
            }

            CreationStepType::ParseAndValidate => {
                let paths = step
                    .parameters
                    .get("paths")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut results = Vec::new();
                let mut any_errors = false;
                for p in paths {
                    if let Some(path) = p.as_str() {
                        let full = full_path(path);
                        // Ask BuildManager to parse the source file (routes to the
                        // correct build module — tree-sitter for Rust/JS/Python,
                        // regex parser for Swift).
                        info!(
                            "[ProjectCreation] TOOL call build.parse_and_store file={}",
                            full.display()
                        );
                        let (t, r) = tokio::sync::oneshot::channel();
                        self.build_manager_tx
                            .send(BuildManagerMessage::ParseAndStoreSourceFile {
                                file_path: full.clone(),
                                reply_to: t,
                            })
                            .await
                            .map_err(|e| e.to_string())?;
                        match r.await {
                            Ok(Ok(summary)) => {
                                if summary.skipped {
                                    results.push(format!("{} — already parsed (no change)", path));
                                } else {
                                    results.push(format!(
                                        "{} — parsed: {} nodes, {} edges",
                                        path, summary.nodes_written, summary.edges_written
                                    ));
                                }
                            }
                            Ok(Err(e)) => {
                                any_errors = true;
                                results.push(format!("{} — PARSE ERROR: {}", path, e));
                            }
                            Err(e) => {
                                any_errors = true;
                                results.push(format!("{} — parse response lost: {}", path, e));
                            }
                        }
                    }
                }
                Ok((
                    if results.is_empty() {
                        "No files to validate".to_string()
                    } else {
                        results.join("\n")
                    },
                    !any_errors,
                ))
            }

            CreationStepType::Build => {
                info!(
                    "[ProjectCreation] TOOL call build.build path={}",
                    root_dir.display()
                );
                let (t, r) = tokio::sync::oneshot::channel();
                self.build_manager_tx
                    .send(BuildManagerMessage::BuildProject {
                        path: root_dir.clone(),
                        opts: BuildOptions {
                            target: None,
                            mode: "debug".to_string(),
                            platform: None,
                            package: None,
                        },
                        reply_to: t,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                match r.await {
                    Ok(Ok(output)) => {
                        if output.success {
                            Ok((format!("Build OK ({:.2}s)", output.duration_secs), true))
                        } else {
                            Ok((format!("Build FAILED:\n{}", output.output), false))
                        }
                    }
                    Ok(Err(e)) => Ok((format!("Build error: {}", e), false)),
                    Err(e) => Ok((format!("Build response lost: {}", e), false)),
                }
            }

            CreationStepType::Test => {
                let (t, r) = tokio::sync::oneshot::channel();
                self.build_manager_tx
                    .send(BuildManagerMessage::TestProject {
                        path: root_dir.clone(),
                        opts: TestOptions { filter: None },
                        reply_to: t,
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                match r.await {
                    Ok(Ok(output)) => {
                        if output.success {
                            Ok((format!("Tests passed ({:.2}s)", output.duration_secs), true))
                        } else {
                            Ok((format!("Tests FAILED:\n{}", output.output), false))
                        }
                    }
                    _ => Ok(("Test execution not available".to_string(), false)),
                }
            }
        };

        match result {
            Ok((message, success)) => Ok(StepExecutionResult {
                step_id: step.id.clone(),
                success,
                message,
            }),
            Err(e) => Ok(StepExecutionResult {
                step_id: step.id.clone(),
                success: false,
                message: e,
            }),
        }
    }
}

// ── Actor impl ──────────────────────────────────────────────────────────────

#[async_trait]
impl Actor for ProjectCreationActor {
    type Message = ProjectCreationMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            ProjectCreationMessage::GeneratePlan {
                goal,
                root_dir,
                language,
                platforms,
                structure,
                // Both are read by the *scaffold-based* plan (`PlanScaffold`), which is the one that
                // fills a real project: this generator writes a generic skeleton from a goal and has
                // nowhere to put either — an ESP-IDF project's library is substituted into its
                // `CMakeLists.txt` by the scaffold, and its hints are read by `generate_fill_plan`.
                embedded_root: _embedded_root,
                embedded: _embedded,
                reply_to,
            } => {
                // Empty platforms => single-target ("host") scaffold.
                let platforms = if platforms.is_empty() {
                    vec!["host".to_string()]
                } else {
                    platforms
                };
                let plan = self
                    .generate_plan_async(&goal, &root_dir, &language, &platforms, structure)
                    .await;
                info!(
                    "[ProjectCreation] PLAN GENERATED: language={}, root_dir={}, steps={}",
                    plan.language,
                    plan.root_dir,
                    plan.steps.len()
                );
                for (i, step) in plan.steps.iter().enumerate() {
                    info!(
                        "[ProjectCreation]   step[{}] type={:?} desc=\"{}\" params={}",
                        i, step.step_type, step.description, step.parameters
                    );
                }
                let _ = reply_to.send(Ok(plan));
            }

            ProjectCreationMessage::ScaffoldProject {
                project_name,
                root_dir,
                language,
                platforms,
                structure,
                embedded_root,
                embedded,
                application,
                reply_to,
            } => {
                let platforms = if platforms.is_empty() {
                    vec!["host".to_string()]
                } else {
                    platforms
                };
                let result: Result<crate::subsystems::build::build_manager::ScaffoldSpec, String> =
                    async {
                        let (build_file, library, _build_system, embedded) =
                            ProjectCreationActor::scaffold_routing(
                                &language,
                                structure,
                                embedded_root.as_deref(),
                                embedded,
                            );
                        // **The design this tree is written from**, resolved the same way the plan was:
                        // the tree's when it already carries one — it is the file a person edits, and the
                        // application's `REQUIRES` and its record are stated from it — and the caller's
                        // only when the tree has none. See `idf_projects::design_for_scaffold`; the one
                        // resolver, so what was planned and what is written cannot disagree.
                        //
                        // This leg is about to write, so the read is the plain one and nothing is settled
                        // here: the scaffold writes the record itself, from this same design, through
                        // `application_scaffold`.
                        let design = crate::build::idf_projects::design_for_scaffold(
                            &root_dir,
                            structure,
                            application,
                        )?;
                        // The design, and — when the tree already stated one — the sentence the
                        // wizard shows. Every statement below (`REQUIRES`, the record, the
                        // composition) is written from `design.spec`, so a caller whose copy was
                        // dropped is told here rather than left to discover it in the tree.
                        let application = design.spec;
                        let (t, r) = oneshot::channel();
                        self.build_manager_tx
                            .send(BuildManagerMessage::ScaffoldBuildConfig {
                                project_name: project_name.clone(),
                                goal: String::new(),
                                build_file: build_file.to_string(),
                                platforms: platforms.clone(),
                                structure,
                                embedded,
                                library,
                                application,
                                reply_to: t,
                            })
                            .await
                            .map_err(|e| e.to_string())?;
                        let out = r
                            .await
                            .map_err(|e| e.to_string())?
                            .map_err(|e| e.to_string())?;

                        let root = root_dir.clone();
                        // The structure decides where the language cannot: an ESP-IDF library and
                        // an ESP-IDF application are both "ESP-IDF", and neither of them is "C++".
                        let build_system =
                            ProjectCreationActor::build_system_for(out.structure, &language);
                        let mut spec = crate::subsystems::build::build_manager::ScaffoldSpec {
                            structural_files: Vec::new(),
                            fill_roots: out.fill_roots.clone(),
                            dependency_sections: out.dependency_sections.clone(),
                            platform_targets: out.platform_targets.clone(),
                            build_system: build_system.to_string(),
                            files: out.files.clone(),
                            structure: out.structure,
                            embedded: out.embedded,
                            // When the tree already stated a design, this is the sentence the
                            // wizard shows: the design just reviewed was not the one used.
                            design_warning: design.dropped,
                        };
                        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
                        materialize_scaffold_files(&root, &out.files, &mut spec)?;
                        // Legacy single-file scaffold fallback.
                        if out.files.is_empty() {
                            let p = root.join(&out.build_file);
                            std::fs::create_dir_all(p.parent().unwrap_or(&root))
                                .map_err(|e| e.to_string())?;
                            std::fs::write(&p, &out.build_content).map_err(|e| e.to_string())?;
                            spec.structural_files.push(out.build_file.clone());
                            let src = root.join(&out.source_dir);
                            std::fs::create_dir_all(&src).map_err(|e| e.to_string())?;
                            let sp = src.join(&out.source_file);
                            std::fs::write(&sp, &out.source_content).map_err(|e| e.to_string())?;
                            if spec.fill_roots.is_empty() {
                                spec.fill_roots = vec![out.source_dir.clone()];
                            }
                        }

                        // Mandate git: init + .gitignore + initial commit so the
                        // LLM's fill changes can be diffed and rolled back to this
                        // pristine scaffold baseline. Non-fatal — scaffolding still
                        // succeeds when git is unavailable.
                        if let Err(ge) =
                            ProjectCreationActor::ensure_scaffold_git(&root, build_system).await
                        {
                            warn!("[ProjectCreation] git scaffold baseline skipped: {ge}");
                        }

                        // Persist the scaffolded structure in the graph.
                        let (at, ar) = oneshot::channel();
                        self.build_manager_tx
                            .send(BuildManagerMessage::AnalyzeProject {
                                path: root,
                                config_file: None,
                                reply_to: at,
                            })
                            .await
                            .map_err(|e| e.to_string())?;
                        let _ = ar.await.map_err(|e| e.to_string())?;
                        Ok(spec)
                    }
                    .await;
                // The actor's reply type is anyhow::Result — map the String error.
                let _ = reply_to.send(result.map_err(anyhow::Error::msg));
            }

            ProjectCreationMessage::PlanScaffold {
                goal,
                root_dir,
                project_name,
                language,
                platforms,
                structure,
                embedded_root,
                embedded,
                application,
                reply_to,
            } => {
                let platforms = if platforms.is_empty() {
                    vec!["host".to_string()]
                } else {
                    platforms
                };
                let result: Result<PlanScaffoldResult, String> = async {
                    // **The design this creation works from**, resolved once for both legs: the tree's
                    // when it already carries one and the caller's otherwise — see
                    // `design_for_scaffold`, which owns that rule so that what the plan is built against
                    // and what the scaffold writes are one design and not two.
                    //
                    // Read **without writing**: this is the planning phase, and nothing goes on disk
                    // until the caller confirms.
                    let design = crate::build::idf_projects::design_for_scaffold(
                        &root_dir,
                        structure,
                        application,
                    )?;
                    // In-memory contract only — NO disk writes until confirm.
                    let mut spec = self
                        .scaffold_spec_in_memory(
                            &project_name,
                            &language,
                            &platforms,
                            structure,
                            // The HAL directory the wizard's picker supplied: an application's spec
                            // is read from it, so a plan for one without it is refused by name.
                            embedded_root.as_deref(),
                            embedded,
                            design.spec.clone(),
                        )
                        .await?;
                    // A plan is a scaffold in waiting, so it carries the same report the write does:
                    // a person told at the plan what will not be applied cannot be surprised by the
                    // tree — and the plan is what the sheet's log reads first.
                    spec.design_warning = design.dropped;
                    self.active_spec = Some(spec.clone());
                    let plan = self
                        .generate_fill_plan(
                            &goal,
                            &root_dir,
                            &spec,
                            embedded_root.as_deref(),
                            design.spec,
                        )
                        .await?;
                    Ok(PlanScaffoldResult { plan, spec })
                }
                .await;
                let _ = reply_to.send(result.map_err(anyhow::Error::msg));
            }

            ProjectCreationMessage::FillProject {
                goal,
                root_dir,
                spec,
                library_root,
                reply_to,
            } => {
                self.active_spec = Some(spec.clone());
                // The framework is not asked of the caller here: the application states it in its own
                // `CMakeLists.txt` by now, and `generate_fill_plan` reads it back from there.
                let plan = self
                    .generate_fill_plan(&goal, &root_dir, &spec, library_root.as_deref(), None)
                    .await;
                let _ = reply_to.send(plan.map_err(anyhow::Error::msg));
            }

            ProjectCreationMessage::GenerateAppSpec {
                project_name,
                goal,
                reply_to,
            } => {
                info!(
                    "[ProjectCreation] GenerateAppSpec: deriving validated AppSpec for '{}' (goal: {})",
                    project_name, goal
                );
                let spec = self.generate_app_spec(&project_name, &goal).await;
                let _ = reply_to.send(spec.map_err(anyhow::Error::msg));
            }

            ProjectCreationMessage::GenerateCode {
                project_name,
                spec,
                reply_to,
            } => {
                // Record + link the codegen artifacts to the project's appspec
                // node first (best-effort), then return the executable steps.
                self.store_generated_artifacts(&project_name, &spec).await;
                let steps =
                    crate::subsystems::project::spec_codegen::codegen_steps(&spec, &project_name);
                info!(
                    "[ProjectCreation] GenerateCode: {} skeleton steps for '{}'",
                    steps.len(),
                    project_name
                );
                let _ = reply_to.send(Ok(steps));
            }

            ProjectCreationMessage::ExecutePlan {
                root_dir,
                steps,
                reply_to,
            } => {
                let mut results = Vec::new();
                for step in &steps {
                    info!(
                        "[ProjectCreation] EXECUTING step[{}] type={:?} desc=\"{}\"",
                        step.id, step.step_type, step.description
                    );
                    let r = self.execute_step(&root_dir, step).await;
                    let res = r.unwrap_or(StepExecutionResult {
                        step_id: step.id.clone(),
                        success: false,
                        message: "Unknown execution error".to_string(),
                    });
                    info!(
                        "[ProjectCreation]   -> finished success={} msg=\"{}\"",
                        res.success, res.message
                    );
                    results.push(res);
                }
                let _ = reply_to.send(Ok(results));
            }

            ProjectCreationMessage::ExecuteStep {
                root_dir,
                step,
                reply_to,
            } => {
                let r = self.execute_step(&root_dir, &step).await;
                let _ = reply_to.send(r.map_err(anyhow::Error::msg));
            }
        }
    }
}

/// Turn a scaffold spec's files into a creation plan: one write step per file (structural →
/// build-config, fillable → source), then a parse and a **host** build.
///
/// Shared by the two structures whose shape is fixed before the goal is read — SpireApp and the
/// embedded HAL — so they cannot drift in what "scaffold and gate this" means: whatever the spec
/// contains is written, then checked. Neither asks the LLM for a plan, because for both of them the
/// structure *is* the answer.
///
/// `reason` lands in `fallback_reason`, which the wizard surfaces as "this is a template, not a
/// generated plan" — honest, since no model was involved.
/// A build module's [`crate::build::ScaffoldOutput`] as the `ScaffoldSpec` the creation flow
/// passes around.
///
/// Factored out because there are two producers now: the build module (every language layout) and
/// the embedded-application emitter, which is not a layout — see `scaffold_spec_in_memory`.
fn spec_from_scaffold_output(
    out: crate::build::ScaffoldOutput,
    language: &str,
) -> crate::subsystems::build::build_manager::ScaffoldSpec {
    let build_system = ProjectCreationActor::build_system_for(out.structure, language);
    let mut spec = crate::subsystems::build::build_manager::ScaffoldSpec {
        structural_files: Vec::new(),
        fill_roots: out.fill_roots.clone(),
        dependency_sections: out.dependency_sections.clone(),
        platform_targets: out.platform_targets.clone(),
        build_system: build_system.to_string(),
        files: out.files.clone(),
        structure: out.structure,
        embedded: out.embedded,
        // Nothing was resolved here: this is the shape of a scaffold, not a resolved design.
        design_warning: None,
    };
    for f in &out.files {
        if f.structural {
            spec.structural_files.push(f.path.clone());
        }
    }
    // Legacy single-file scaffold fallback (same shape as ScaffoldProject).
    if out.files.is_empty() {
        spec.structural_files.push(out.build_file.clone());
        if spec.fill_roots.is_empty() {
            spec.fill_roots = vec![out.source_dir.clone()];
        }
    }
    spec
}

fn scaffold_plan_from_spec(
    goal: &str,
    root_dir: &std::path::Path,
    language: &str,
    spec: crate::subsystems::build::build_manager::ScaffoldSpec,
    reason: &str,
) -> PlanGenerationResult {
    let mut steps: Vec<CreationStep> = Vec::new();
    for (i, f) in spec.files.iter().enumerate() {
        let step_type = if f.structural {
            CreationStepType::WriteBuildConfig
        } else {
            CreationStepType::WriteSourceFile
        };
        steps.push(CreationStep {
            id: format!("scaffold-{}", i + 1),
            step_type,
            description: format!("Write {}", f.path),
            status: StepStatus::Pending,
            parameters: serde_json::json!({ "path": f.path, "content": f.content }),
            result: None,
        });
    }
    let rs_paths: Vec<String> = spec
        .files
        .iter()
        .filter(|f| f.path.ends_with(".rs"))
        .map(|f| f.path.clone())
        .collect();
    steps.push(CreationStep {
        id: format!("scaffold-{}", steps.len() + 1),
        step_type: CreationStepType::ParseAndValidate,
        description: "Parse source files to validate syntax via AST modules".into(),
        status: StepStatus::Pending,
        parameters: serde_json::json!({ "paths": rs_paths }),
        result: None,
    });
    steps.push(CreationStep {
        id: format!("scaffold-{}", steps.len() + 1),
        step_type: CreationStepType::Build,
        description: "Build the workspace to verify the scaffold compiles".into(),
        status: StepStatus::Pending,
        parameters: serde_json::json!({}),
        result: None,
    });

    PlanGenerationResult {
        goal: goal.to_string(),
        language: language.to_string(),
        root_dir: root_dir.to_string_lossy().to_string(),
        steps,
        is_template: true,
        fallback_reason: Some(reason.to_string()),
    }
}

/// Materialize a scaffold's files under `root`, **leaving a composition that is already there alone**.
///
/// Everything the scaffold emits is the tool's to write: the build files, the record, the stub the fill
/// phase replaces. [`COMPOSITION_FILE`](crate::build::idf_projects::COMPOSITION_FILE) is the exception,
/// and it is a file of a different kind: it is the one a person edits, the one that can carry comments,
/// and — since the design was resolved *from it* (`idf_projects::design_for_scaffold`) — rendering it
/// again could only lose something. So a composition that is already there is kept exactly as it is,
/// byte for byte, and the record beside it is written from the design that was read out of it, which is
/// what keeps the two stating the same thing.
///
/// Every structural path is recorded whether or not it was written: locked is a fact about the file, not
/// about what this loop happened to do to it just now — and a path that is on disk and not recorded is a
/// file the fill phase would then be free to overwrite.
fn materialize_scaffold_files(
    root: &Path,
    files: &[crate::build::ScaffoldFile],
    spec: &mut crate::subsystems::build::build_manager::ScaffoldSpec,
) -> Result<(), String> {
    for file in files {
        let path = root.join(&file.path);
        if file.structural {
            spec.structural_files.push(file.path.clone());
        }
        if file.path == crate::build::idf_projects::COMPOSITION_FILE && path.is_file() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, &file.content).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_fs() -> mpsc::Sender<spire_core::modules::FilesystemMessage> {
        mpsc::channel(4).0
    }

    fn dummy_bm() -> mpsc::Sender<BuildManagerMessage> {
        mpsc::channel(4).0
    }

    fn dummy_mcp() -> mpsc::Sender<McpClientMessage> {
        mpsc::channel(4).0
    }

    /// The shared builder behind the two fixed-shape structures: every file the spec carries
    /// becomes a write step (structural → build-config, the rest → source), and the plan ends with
    /// a parse and a build gate. No LLM is involved, which is why `is_template` is true and the
    /// reason is the caller's.
    #[test]
    fn a_scaffold_spec_becomes_its_write_steps_and_a_gate() {
        use crate::build::ScaffoldFile;
        use crate::subsystems::build::build_manager::ScaffoldSpec;

        let spec = ScaffoldSpec {
            structural_files: vec!["Cargo.toml".to_string()],
            fill_roots: vec!["crates".to_string()],
            dependency_sections: vec!["Cargo.toml".to_string()],
            platform_targets: vec!["esp32c6".to_string()],
            build_system: "Cargo".to_string(),
            files: vec![
                ScaffoldFile {
                    path: "Cargo.toml".to_string(),
                    content: "[workspace]".to_string(),
                    structural: true,
                    fill_role: None,
                },
                ScaffoldFile {
                    path: "crates/demo-hal/src/lib.rs".to_string(),
                    content: "#![no_std]".to_string(),
                    structural: false,
                    fill_role: None,
                },
            ],
            structure: spire_core::build_types::ProjectStructure::Embedded,
            embedded: true,
            design_warning: None,
        };

        let plan = scaffold_plan_from_spec(
            "blink a light",
            &PathBuf::from("/tmp/demo"),
            "Rust",
            spec,
            "embedded-HAL structure — deterministic workspace scaffold",
        );

        assert!(
            plan.is_template,
            "no model was asked for this plan: {plan:?}"
        );
        assert_eq!(
            plan.fallback_reason.as_deref(),
            Some("embedded-HAL structure — deterministic workspace scaffold")
        );
        assert_eq!(plan.root_dir, "/tmp/demo");
        let kinds: Vec<&CreationStepType> = plan.steps.iter().map(|s| &s.step_type).collect();
        assert_eq!(
            kinds,
            vec![
                &CreationStepType::WriteBuildConfig,
                &CreationStepType::WriteSourceFile,
                &CreationStepType::ParseAndValidate,
                &CreationStepType::Build,
            ],
            "{:?}",
            plan.steps
        );
        assert_eq!(plan.steps[0].parameters["path"], "Cargo.toml");
        assert_eq!(
            plan.steps[2].parameters["paths"],
            serde_json::json!(["crates/demo-hal/src/lib.rs"]),
            "the parse gate checks the Rust the scaffold wrote: {:?}",
            plan.steps[2].parameters
        );
    }

    /// The library's `SPIRE.md` reaches the model — and so do the **decomposition** the design phase
    /// produced and the framework inside it.
    ///
    /// The first is the whole reason the file is written down, and both of its failure modes are
    /// silent: hints that never arrive are an architecture nobody follows, and hints taken from the
    /// wrong project are someone else's architecture — which is worse. The others are the same failure
    /// one level up: the hints describe *both* frameworks and how to choose, so without the decision
    /// the model chooses again and invents a different composition, differently, per run.
    #[test]
    fn the_librarys_hints_and_the_design_reach_the_fill_prompt() {
        use crate::build::application_spec::{examples, ApplicationFramework, ApplicationSpec};
        use crate::build::idf_projects::{APPLICATION_FILE, HINTS_FILE};
        use crate::subsystems::build::build_manager::ScaffoldSpec;
        use std::sync::{Arc, Mutex};

        /// Ask for one plan and hand back the prompt it was asked with, under a root of the caller's
        /// choosing — so the read-back of a design the tree carries can be exercised rather than
        /// assumed. The reply is a plan the caller can parse, so a failure here is about the prompt
        /// and not the response.
        fn prompt_for(
            spec: &ScaffoldSpec,
            library_root: Option<&std::path::Path>,
            application: Option<ApplicationSpec>,
            root: &std::path::Path,
        ) -> String {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let (llm_tx, mut llm_rx) = mpsc::channel::<LlmMessage>(4);
            let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let captured = seen.clone();
            rt.spawn(async move {
                while let Some(msg) = llm_rx.recv().await {
                    if let LlmMessage::Complete {
                        prompt, reply_to, ..
                    } = msg
                    {
                        captured.lock().unwrap().push(prompt);
                        let _ = reply_to.send(Ok(
                            r#"[{ "write_source_file": { "path": "main/sensor.cpp", "content": "// read" } }]"#
                                .to_string(),
                        ));
                    }
                }
            });

            let mut actor = ProjectCreationActor::new(dummy_fs(), dummy_bm(), dummy_mcp());
            actor.set_llm(llm_tx);
            rt.block_on(actor.generate_fill_plan(
                "read a PM2.5 sensor",
                root,
                spec,
                library_root,
                application,
            ))
            .expect("a parseable plan comes back");

            let prompts = seen.lock().unwrap();
            assert_eq!(prompts.len(), 1, "one planning round trip");
            prompts[0].clone()
        }

        // The library the application is built against, and what it said about itself.
        let library = tempfile::tempdir().unwrap();
        let said =
            "# sensors\n\n## How to use it\n\nCall `sensors::begin()` once, before any read.\n";
        std::fs::write(library.path().join(HINTS_FILE), said).unwrap();
        // Its **headers**, written by hand: what a composition has to call is the library's own API, and
        // this is the half of the prompt a model cannot invent. `component_names` counts a component
        // directory only when it has a manifest, which is the rule IDF applies too.
        for (path, body) in [
            (
                "components/sps30/CMakeLists.txt",
                "idf_component_register(INCLUDE_DIRS include)\n",
            ),
            (
                "components/sps30/include/sps30.hpp",
                "namespace sps30 {\nclass Sps30 {\npublic:\n    explicit Sps30(BusHandle device);\n};\n}\n",
            ),
            (
                "components/actors/include/actor.hpp",
                "namespace spire {\ntemplate <typename Message>\nclass Actor {\nprotected:\n    void on_message(const Message& message);\n};\n}\n",
            ),
        ] {
            let file = library.path().join(path);
            std::fs::create_dir_all(file.parent().expect("a parent")).unwrap();
            std::fs::write(&file, body).unwrap();
        }

        let spec = ScaffoldSpec {
            structural_files: vec!["CMakeLists.txt".to_string()],
            fill_roots: vec!["main".to_string()],
            dependency_sections: Vec::new(),
            platform_targets: vec!["esp32s3".to_string()],
            build_system: "ESP-IDF".to_string(),
            files: Vec::new(),
            structure: spire_core::build_types::ProjectStructure::IdfApplication,
            embedded: true,
            design_warning: None,
        };
        // A root with no tree in it yet: planning happens before anything is written.
        let unborn = tempfile::tempdir().unwrap();

        let named = prompt_for(&spec, Some(library.path()), None, unborn.path());
        assert!(
            named.contains("Call `sensors::begin()` once, before any read."),
            "the library's hints are in the prompt:\n{named}"
        );
        assert!(named.contains("LIBRARY HINTS"), "{named}");

        // An application that named no library is told nothing — not the hints of whatever happens
        // to sit at its own root, which is not a library.
        let unnamed = prompt_for(&spec, None, None, unborn.path());
        assert!(
            !unnamed.contains("LIBRARY HINTS"),
            "no library named, so no hints:\n{unnamed}"
        );

        // The decomposition the design phase produced, which is what stops the model choosing a
        // framework again *and* inventing a composition.
        let app_one = crate::build::application_spec::parse_spec(examples::PM25_METER)
            .expect("the worked example parses");
        let actors = prompt_for(&spec, None, Some(app_one.clone()), unborn.path());
        assert!(actors.contains("FRAMEWORK: `actors`"), "{actors}");
        assert!(actors.contains("not choose again"), "{actors}");
        assert!(
            actors.contains("spire::Scheduler"),
            "and what the choice means, in the idiom that matters:\n{actors}"
        );
        // **What an edge carries.** The compiler, not a test, is what found this: the model wrote an
        // actor holding `ActorRef<TouchEvent>` because `TouchEvent` was its own message, and spawned it
        // with a ref to an actor that takes `Reading`. `a -> b` carries `b`'s message — stated here
        // because it is the difference between a composition that compiles and one that does not.
        assert!(
            actors.contains("edge carries the receiver's message, not the sender's")
                && actors.contains("`spire::ActorRef<b::Message>`"),
            "{actors}"
        );
        // And *where a component lives*, which the next build asked for: a driver's handle does not exist
        // until `init()` opens the bus, and a non-copyable component cannot be kept as a plain member or
        // assigned — `sps30_ = sps30::Sps30(handle);` was the error.
        assert!(
            actors.contains("driver's argument usually does not exist until `init()`")
                && actors.contains("std::optional<sps30::Sps30>")
                && actors.contains("non-copyable"),
            "{actors}"
        );
        // The composition itself: the units, what they use, how they are wired and where the devices
        // are. Without this the model writes a different application from the reviewed one.
        for expected in [
            "THE DESIGN — reviewed and approved",
            "`sps30` — driver on i2c: PM1/2.5/4/10 readings (to be written in the library)",
            "`moving_average` — library: a rolling window (already in the library)",
            "`sampler` — actor on `Tick`",
            "uses sps30, sht20",
            "sends to air_quality",
            "Wiring: sampler -> air_quality; air_quality -> view",
            "`sps30` on i2c at 0x69",
        ] {
            assert!(
                actors.contains(expected),
                "the composition block is missing {expected:?}:
{actors}"
            );
        }

        // And the two prohibitions the composition rules carry — the two things a live run did without
        // them: it wrote one flat FreeRTOS poll loop, and its own `class Sps30` in `main/`.
        assert!(
            actors.contains("not** a bare FreeRTOS `while (true)` task")
                && actors.contains("not** an `xTaskCreate` per unit"),
            "the composition is implemented with the framework, not approximated by a loop:\n{actors}"
        );
        assert!(
            actors.contains("Do **not** declare your own class")
                && actors.contains("`#include <name.hpp>`"),
            "the library's components are include-only, never re-declared:\n{actors}"
        );
        // **And what a stub can be called.** A component whose header offers `probe()` and nothing else
        // has no read API; a fill that invented `read(...)` produced `sampler.cpp` that could not compile,
        // and the header it was shown was the whole API. The rule names the move — leave the `TODO` —
        // rather than only forbidding the invention.
        assert!(
            actors.contains("Use only the methods the headers declare")
                && actors.contains("**do not invent**")
                && actors.contains("no read API yet"),
            "a stub component has nothing to call, and the composition must leave it open:\n{actors}"
        );

        // **And the API the composition has to call.** Telling a model which components exist is not
        // telling it what to call on them, and it cannot write an API it has never seen: a live run was
        // handed the reviewed composition and the prose idiom with **no headers at all**, and answered
        // with a bare FreeRTOS `while (true)` loop and its own sensor classes rather than `spire::Actor`
        // on a `spire::Scheduler`. The headers arrive only when a design *and* a library are both
        // present — which is why the fill is given the library as `embeddedRoot` — so it is pinned here
        // rather than left to a caller to remember.
        let with_library = prompt_for(
            &spec,
            Some(library.path()),
            Some(app_one.clone()),
            unborn.path(),
        );
        assert!(
            with_library.contains("#include <actor.hpp>") && with_library.contains("spire"),
            "the framework's own headers are in the prompt:\n{with_library}"
        );
        assert!(
            with_library.contains("#include <sps30.hpp>")
                && with_library.contains("explicit Sps30(BusHandle device)"),
            "and the components the design names, by the path they are included by:\n{with_library}"
        );
        assert!(
            !actors.contains("#include <actor.hpp>"),
            "with no library named there is nothing to read, and nothing is invented:\n{actors}"
        );

        // A ramen application gets its own idiom and its own units.
        let trap = prompt_for(
            &spec,
            None,
            Some(crate::build::application_spec::parse_spec(examples::INSECT_TRAP).unwrap()),
            unborn.path(),
        );
        assert!(trap.contains("FRAMEWORK: `ramen`"), "{trap}");
        assert!(
            trap.contains("synchronous and inline") && trap.contains("never a cycle"),
            "the ramen idiom, including the DAG rule:
{trap}"
        );
        assert!(
            trap.contains("`capture` — stage: pulls nothing")
                && trap.contains("`classifier` — stage: pulls detections, pushes verdicts"),
            "the stages, as they were reviewed:
{trap}"
        );

        // Nothing named and nothing stated: the library's rule applies, and the prompt says where the
        // answer belongs rather than leaving the model to invent a record of it.
        let none = prompt_for(&spec, None, None, unborn.path());
        assert!(none.contains("this application states none"), "{none}");
        assert!(
            !none.contains("THE DESIGN"),
            "with no design there is nothing to follow, and nothing is invented:\n{none}"
        );
        assert!(
            !none.contains("THE COMPOSITION IS THE ARCHITECTURE"),
            "and with nothing to compose there is no composition to insist on:\n{none}"
        );

        // Once the tree exists, what it carries is read back — the caller does not have to remember
        // the design, which is what makes the fill leg after a scaffold work unattended.
        let built = tempfile::tempdir().unwrap();
        std::fs::write(
            built.path().join(APPLICATION_FILE),
            format!("{}\n", serde_json::to_string_pretty(&app_one).unwrap()),
        )
        .unwrap();
        let from_disk = prompt_for(&spec, None, None, built.path());
        assert!(
            from_disk.contains("FRAMEWORK: `actors`")
                && from_disk.contains("`sps30` on i2c at 0x69"),
            "the application's own file is the source once it exists:
{from_disk}"
        );

        // **And the field a caller can drop.** `structure` arrives from the caller, and a spec built
        // without it deserializes as `native` — which is exactly what the wizard's hand-built spec did.
        // Every application block above is gated on that one word, so a fill asked for `actors` was handed
        // the goal and the generic rules and nothing else, and wrote a flat FreeRTOS loop with its own
        // sensor classes. The tree's own `SPIRE.application.json` is written *by* the application scaffold,
        // so it decides: the file is the fact, the field the claim.
        let mut dropped = spec.clone();
        dropped.structure = spire_core::build_types::ProjectStructure::Native;
        let from_a_dropped_field = prompt_for(&dropped, Some(library.path()), None, built.path());
        assert!(
            from_a_dropped_field.contains("FRAMEWORK: `actors`")
                && from_a_dropped_field.contains("THE DESIGN — reviewed and approved")
                && from_a_dropped_field.contains("THE COMPOSITION IS THE ARCHITECTURE")
                && from_a_dropped_field.contains("#include <actor.hpp>"),
            "a tree carrying a design is an application whatever the spec's field says:\n\
             {from_a_dropped_field}"
        );
        // With no tree to say it, the field is all there is: a native project stays native and is not
        // handed a composition it does not have.
        let native_with_nothing = prompt_for(&dropped, None, None, unborn.path());
        assert!(
            !native_with_nothing.contains("THE DESIGN")
                && !native_with_nothing.contains("THE COMPOSITION IS THE ARCHITECTURE"),
            "nothing in the tree and `native` in the field is still not an application:\n\
             {native_with_nothing}"
        );

        // An application made before the design phase exists states no decomposition — only the
        // framework line — and that alone is still followed.
        let stated = tempfile::tempdir().unwrap();
        std::fs::write(
            stated.path().join("CMakeLists.txt"),
            format!(
                "project(pm25-meter)\n{}\n",
                ApplicationFramework::Ramen.marker_line()
            ),
        )
        .unwrap();
        let from_line = prompt_for(&spec, None, None, stated.path());
        assert!(
            from_line.contains("FRAMEWORK: `ramen`"),
            "the stated framework is read back even with no decomposition:
{from_line}"
        );
        assert!(
            !from_line.contains("THE DESIGN"),
            "and no design is invented for it:
{from_line}"
        );

        // A **library** is told nothing about frameworks: it is built against nothing, states none of
        // its own, and a prompt that offered it one would be inventing a decision it never makes.
        let library_spec = ScaffoldSpec {
            structure: spire_core::build_types::ProjectStructure::IdfLibrary,
            ..spec
        };
        let library_prompt = prompt_for(&library_spec, None, Some(app_one), unborn.path());
        assert!(
            !library_prompt.contains("FRAMEWORK:") && !library_prompt.contains("THE DESIGN"),
            "a library is offered no framework and no application's design:
{library_prompt}"
        );
    }

    /// The AppSpec requirements pass drives the real LlmMessage channel: a
    /// stub responder answers `Complete` with a valid GIS AppSpec JSON.
    #[test]
    fn generate_app_spec_roundtrips_a_valid_spec_over_the_llm_channel() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (llm_tx, mut llm_rx) = mpsc::channel::<LlmMessage>(4);
        let reply = serde_json::json!({
            "app": { "name": "spire-gis", "goal": "view and edit map layers" },
            "types": [],
            "actors": [{
                "name": "MapActor", "description": "", "handlers": ["map/listLayers"],
                "state": [], "uses": []
            }],
            "bridge": [{
                "method": "map/listLayers", "description": "", "params": [],
                "result": { "kind": "list", "of": { "kind": "str" } }
            }],
            "ui": [{
                "id": "map", "title": "Map",
                "actions": [{ "id": "load", "description": "", "bridge": "map/listLayers" }]
            }]
        });
        rt.spawn(async move {
            while let Some(msg) = llm_rx.recv().await {
                if let LlmMessage::Complete { reply_to, .. } = msg {
                    let _ = reply_to.send(Ok(reply.to_string()));
                }
            }
        });
        let mut actor = ProjectCreationActor::new(dummy_fs(), dummy_bm(), dummy_mcp());
        actor.set_llm(llm_tx);
        let spec = rt
            .block_on(actor.generate_app_spec("spire-gis", "view and edit map layers"))
            .expect("valid spec must round-trip through the LLM message path");
        assert_eq!(spec.app.name, "spire-gis");
        assert_eq!(spec.bridge[0].method, "map/listLayers");
        assert!(spec.is_valid());
    }

    /// The validated AppSpec is persisted to the memory graph as a
    /// `MergeAttrNode` upsert (node_type=Unknown, subtype=appspec) — the graph
    /// is Spire's single source of truth and later codegen links to this node.
    #[test]
    fn generate_app_spec_persists_the_spec_into_the_graph() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (llm_tx, mut llm_rx) = mpsc::channel::<LlmMessage>(4);
        let (mg_tx, mut mg_rx) =
            mpsc::channel::<spire_core::subsystems::graph::memory_graph::MemoryGraphMessage>(4);
        let reply = serde_json::json!({
            "app": { "name": "spire-gis", "goal": "view and edit map layers" },
            "types": [],
            "actors": [{
                "name": "MapActor", "description": "", "handlers": ["map/listLayers"],
                "state": [], "uses": []
            }],
            "bridge": [{
                "method": "map/listLayers", "description": "", "params": [],
                "result": { "kind": "list", "of": { "kind": "str" } }
            }],
            "ui": [{
                "id": "map", "title": "Map",
                "actions": [{ "id": "load", "description": "", "bridge": "map/listLayers" }]
            }]
        });
        let stored: std::sync::Arc<std::sync::Mutex<Vec<AttrNode>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stored2 = stored.clone();
        rt.spawn(async move {
            while let Some(msg) = llm_rx.recv().await {
                if let LlmMessage::Complete { reply_to, .. } = msg {
                    let _ = reply_to.send(Ok(reply.to_string()));
                }
            }
        });
        rt.spawn(async move {
            while let Some(msg) = mg_rx.recv().await {
                if let MemoryGraphMessage::MergeAttrNode { node, reply_to } = msg {
                    stored2.lock().unwrap().push(node.clone());
                    let _ = reply_to.send(Ok(node));
                }
            }
        });
        let mut actor = ProjectCreationActor::new(dummy_fs(), dummy_bm(), dummy_mcp());
        actor.set_llm(llm_tx);
        actor.set_memory_graph(mg_tx);
        let spec = rt
            .block_on(actor.generate_app_spec("spire-gis", "view and edit map layers"))
            .expect("valid spec must be produced");
        assert!(spec.is_valid());

        // Stage 4: the DECOMPOSITION is what lands in the graph — an appspec
        // anchor (no whole-JSON `spec` blob property) + one AttrNode per spec
        // node, plus one Custom relationship per spec edge.
        let g = crate::subsystems::project::spec_graph::decompose(&spec);
        let stored = stored.lock().unwrap();
        assert_eq!(
            stored.len(),
            g.nodes.len(),
            "one upsert per decomposed node after the full pass"
        );
        let anchor = stored
            .iter()
            .find(|n| n.subtype.as_deref() == Some("appspec"))
            .expect("appspec anchor must be stored");
        assert_eq!(anchor.node_type, "Unknown");
        assert_eq!(anchor.name, "spire-gis");
        assert!(
            !anchor.properties.contains_key("spec"),
            "no whole-JSON spec blob property remains"
        );
        assert!(anchor.properties.contains_key("spec_md"));
        for disc in ["spec_actor", "spec_method", "spec_screen"] {
            assert!(
                stored.iter().any(|n| n.subtype.as_deref() == Some(disc)),
                "decomposition must contain {disc} nodes"
            );
        }
    }

    /// Codegen artifacts are recorded as `generated_file` nodes and linked to
    /// the project's appspec node with a GENERATED_FROM edge — the graph keeps
    /// the spec ↔ implementation trace (single source of truth).
    #[test]
    fn generate_code_links_artifacts_to_the_appspec_node() {
        use spire_core::models::memory_graph::{GraphEdge, RelationshipType};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (llm_tx, mut llm_rx) = mpsc::channel::<LlmMessage>(4);
        let (mg_tx, mut mg_rx) = mpsc::channel::<MemoryGraphMessage>(4);
        let reply = serde_json::json!({
            "app": { "name": "spire-gis", "goal": "view and edit map layers" },
            "types": [],
            "actors": [{
                "name": "MapActor", "description": "", "handlers": ["map/listLayers"],
                "state": [], "uses": []
            }],
            "bridge": [{
                "method": "map/listLayers", "description": "", "params": [],
                "result": { "kind": "list", "of": { "kind": "str" } }
            }],
            "ui": [{
                "id": "map", "title": "Map",
                "actions": [{ "id": "load", "description": "", "bridge": "map/listLayers" }]
            }]
        });
        let nodes: std::sync::Arc<std::sync::Mutex<Vec<AttrNode>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let edges: std::sync::Arc<std::sync::Mutex<Vec<GraphEdge>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let nodes2 = nodes.clone();
        let edges2 = edges.clone();
        rt.spawn(async move {
            while let Some(msg) = llm_rx.recv().await {
                if let LlmMessage::Complete { reply_to, .. } = msg {
                    let _ = reply_to.send(Ok(reply.to_string()));
                }
            }
        });
        rt.spawn(async move {
            while let Some(msg) = mg_rx.recv().await {
                match msg {
                    MemoryGraphMessage::MergeAttrNode { node, reply_to } => {
                        nodes2.lock().unwrap().push(node.clone());
                        let _ = reply_to.send(Ok(node));
                    }
                    MemoryGraphMessage::QueryAttrNodes {
                        node_type,
                        subtype,
                        name,
                        limit,
                        reply_to,
                    } => {
                        let out: Vec<AttrNode> = nodes2
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|n| match &node_type {
                                Some(t) => n.node_type == *t,
                                None => true,
                            })
                            .filter(|n| match &subtype {
                                Some(s) => n.subtype.as_deref() == Some(s.as_str()),
                                None => true,
                            })
                            .filter(|n| match &name {
                                Some(x) => n.name == *x,
                                None => true,
                            })
                            .cloned()
                            .collect();
                        let out = match limit {
                            Some(l) => out.into_iter().take(l as usize).collect(),
                            None => out,
                        };
                        let _ = reply_to.send(Ok(out));
                    }
                    MemoryGraphMessage::CreateRelationship { rel, reply_to } => {
                        let edge = GraphEdge {
                            id: uuid::Uuid::new_v4().to_string(),
                            edge_type: rel.edge_type.clone(),
                            from_id: rel.from_id.clone(),
                            to_id: rel.to_id.clone(),
                            properties: rel.properties.clone().unwrap_or_default(),
                            created_at: chrono::Utc::now(),
                            weight: rel.weight,
                        };
                        edges2.lock().unwrap().push(edge.clone());
                        let _ = reply_to.send(Ok(edge));
                    }
                    _ => {}
                }
            }
        });
        let mut actor = ProjectCreationActor::new(dummy_fs(), dummy_bm(), dummy_mcp());
        actor.set_llm(llm_tx);
        actor.set_memory_graph(mg_tx);
        let spec = rt
            .block_on(actor.generate_app_spec("spire-gis", "view and edit map layers"))
            .expect("requirements pass must produce a spec");

        // Codegen for that spec records + links the skeleton artifacts.
        rt.block_on(actor.store_generated_artifacts("spire-gis", &spec));

        let appspec_id = nodes
            .lock()
            .unwrap()
            .iter()
            .find(|n| n.subtype.as_deref() == Some("appspec"))
            .expect("appspec node must exist")
            .id
            .clone();
        let file_nodes = nodes
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.subtype.as_deref() == Some("generated_file"))
            .count();
        assert_eq!(
            file_nodes, 5,
            "every generated skeleton file must be recorded as a node"
        );
        let edges = edges.lock().unwrap();
        let generated_from: Vec<&GraphEdge> = edges
            .iter()
            .filter(|e| {
                matches!(
                    &e.edge_type,
                    RelationshipType::Custom(p) if p == "GENERATED_FROM"
                )
            })
            .collect();
        assert_eq!(
            generated_from.len(),
            5,
            "one GENERATED_FROM edge per artifact (spec edges also present)"
        );
        assert!(
            edges
                .iter()
                .any(|e| matches!(&e.edge_type, RelationshipType::Custom(p) if p == "HAS_ACTOR")),
            "the decomposition's spec edges are stored alongside codegen links"
        );
        for e in generated_from {
            assert_eq!(e.to_id, appspec_id, "every artifact points at the spec");
        }
    }

    #[test]
    fn plan_generation_creates_ordered_steps() {
        let actor = ProjectCreationActor::new(dummy_fs(), dummy_bm(), dummy_mcp());
        let root = PathBuf::from("/tmp/test-project");
        let plan = actor.generate_plan("A CLI tool that converts CSV to JSON", &root, "Rust", &[]);

        assert_eq!(plan.steps.len(), 6);
        assert_eq!(plan.steps[0].step_type, CreationStepType::CreateDirectory);
        assert_eq!(plan.steps[1].step_type, CreationStepType::WriteBuildConfig);
        assert_eq!(plan.steps[2].step_type, CreationStepType::AddDependency);
        assert_eq!(plan.steps[3].step_type, CreationStepType::WriteSourceFile);
        assert_eq!(plan.steps[4].step_type, CreationStepType::ParseAndValidate);
        assert_eq!(plan.steps[5].step_type, CreationStepType::Build);
    }

    /// Regression test against the ACTUAL shape `deepseek-chat` returned
    /// (verified via live API): array of single-key objects where the key is
    /// the step type and the value is the parameters object.
    #[test]
    fn parse_fill_steps_handles_single_key_shape() {
        let json = r#"```json
[
  { "declare_dependencies": { "path": "Cargo.toml", "dependencies": [{"name": "tokio", "version": "1"}] } },
  { "write_source_file": { "path": "src/main.rs", "content": "fn main() {}" } },
  { "build": {} }
]
```"#;
        let steps = parse_fill_steps(json).expect("should parse shape-2 steps");
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].step_type, CreationStepType::AddDependency);
        assert_eq!(steps[0].parameters["path"], "Cargo.toml");
        assert_eq!(steps[1].step_type, CreationStepType::WriteSourceFile);
        assert_eq!(steps[1].parameters["content"], "fn main() {}");
        assert_eq!(steps[2].step_type, CreationStepType::Build);
    }

    /// Recovery-path test: a LARGE array where ONE element is malformed (e.g.
    /// truncated content with an unescaped sequence) must NOT discard the whole
    /// response — the valid steps are kept, the broken one dropped.
    #[test]
    fn parse_fill_steps_recovers_from_one_bad_element() {
        // Deliberately malformed: `content` contains an unescaped `"` inside
        // the string, so the full-document JSON parse fails for this element.
        let json = r#"```json
[
  { "declare_dependencies": { "path": "Cargo.toml", "dependencies": [{"name": "tokio", "version": "1"}] } },
  { "write_source_file": { "path": "core/src/lib.rs", "content": "let x = "broken";" } },
  { "build": {} }
]
```"#;
        let steps = parse_fill_steps(json).expect("should recover valid steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_type, CreationStepType::AddDependency);
        // Indices renumber after the broken element is dropped.
        assert_eq!(steps[1].step_type, CreationStepType::Build);
    }

    /// Regression test from a LIVE DeepSeek V4 response (captured via curl with
    /// the real fill-plan prompt, 2026-08-17): plain `content` field containing
    /// a ```json-fenced array of shape-2 steps. This is the exact shape the
    /// fill path must accept.
    #[test]
    fn parse_fill_steps_handles_live_v4_fenced_array() {
        let json = r#"```json
[
  {
    "declare_dependencies": {
      "path": "/Users/steve/naturesense/ai-traps-mcp/Cargo.toml",
      "dependencies": [
        { "name": "rmcp", "version": "0.1" },
        { "name": "tokio", "version": "1", "features": ["full"] },
        { "name": "serde", "version": "1", "features": ["derive"] },
        { "name": "serde_json", "version": "1" },
        { "name": "tracing", "version": "0.1" },
        { "name": "tracing-subscriber", "version": "0.3" }
      ]
    }
  },
  {
    "create_directory": {
      "path": "/Users/steve/naturesense/ai-traps-mcp/core/src"
    }
  },
  {
    "write_source_file": {
      "path": "/Users/steve/naturesense/ai-traps-mcp/core/src/lib.rs",
      "content": "// core/src/lib.rs\npub fn hello() {}\n"
    }
  }
]
```"#;
        let steps = parse_fill_steps(json).expect("live V4 fenced array must parse");
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].step_type, CreationStepType::AddDependency);
        let deps = steps[0].parameters["dependencies"].as_array().unwrap();
        assert_eq!(deps.len(), 6);
        assert_eq!(steps[1].step_type, CreationStepType::CreateDirectory);
        assert_eq!(steps[2].step_type, CreationStepType::WriteSourceFile);
    }

    /// Regression test from the LIVE deterministic-json response (captured
    /// 2026-08-18 after temperature=0 + response_format json_object): outer
    /// `{"steps": [...]}` envelope whose steps use the model's actual inner
    /// schema `{ "action": ..., "arguments": {...} }`. This exact body was
    /// previously rejected ("no usable steps") and is now accepted.
    #[test]
    fn parse_fill_steps_handles_action_arguments_shape() {
        let json = r#"{
  "steps": [
    {
      "action": "declare_dependencies",
      "arguments": {
        "path": "/Users/steve/naturesense/ai-traps-mcp/Cargo.toml",
        "dependencies": [
          { "name": "rmcp", "version": "0.1" },
          { "name": "tokio", "version": "1" }
        ]
      }
    },
    {
      "action": "create_directory",
      "arguments": { "path": "/Users/steve/naturesense/ai-traps-mcp/core/src" }
    },
    {
      "action": "write_source_file",
      "arguments": {
        "path": "/Users/steve/naturesense/ai-traps-mcp/core/src/lib.rs",
        "content": "pub fn hello() -> &'static str {\n    \"Hello from core!\"\n}\n"
      }
    },
    {
      "action": "build",
      "arguments": { "path": "/Users/steve/naturesense/ai-traps-mcp" }
    },
    {
      "action": "parse_and_validate",
      "arguments": { "path": "/Users/steve/naturesense/ai-traps-mcp" }
    }
  ]
}"#;
        let steps = parse_fill_steps(json).expect("action/arguments body must parse");
        assert_eq!(steps.len(), 5);
        assert_eq!(steps[0].step_type, CreationStepType::AddDependency);
        assert_eq!(
            steps[0].parameters["path"],
            "/Users/steve/naturesense/ai-traps-mcp/Cargo.toml"
        );
        assert_eq!(steps[1].step_type, CreationStepType::CreateDirectory);
        assert_eq!(
            steps[1].parameters["path"],
            "/Users/steve/naturesense/ai-traps-mcp/core/src"
        );
        assert_eq!(steps[2].step_type, CreationStepType::WriteSourceFile);
        assert_eq!(
            steps[2].parameters["content"],
            "pub fn hello() -> &'static str {\n    \"Hello from core!\"\n}\n"
        );
        assert_eq!(steps[3].step_type, CreationStepType::Build);
        assert_eq!(steps[4].step_type, CreationStepType::ParseAndValidate);
    }

    /// The shape a **live** run of the application-creation loop produced (2026-09-26): `type` as the
    /// step-type key, with the step's own fields *beside* it rather than wrapped. Every one of that
    /// plan's eight steps was discarded — an entire plan rejected for its key's **name** — which is the
    /// failure mode this family of tests exists for, and the reason a live run is worth more than any
    /// number of scripted ones.
    #[test]
    fn parse_fill_steps_handles_the_flat_type_shape() {
        let text = r##"{"steps": [
            {"type": "create_directory", "path": "main/components"},
            {"type": "write_source_file", "path": "main/main.cpp",
             "content": "#include <cstdio>\nint main() { return 0; }\n"},
            {"type": "build"}
        ]}"##;
        let steps = parse_fill_steps(text).expect("the shape a live run produced parses");
        assert_eq!(steps.len(), 3, "{steps:?}");
        assert_eq!(steps[0].step_type, CreationStepType::CreateDirectory);
        assert_eq!(steps[0].parameters["path"], "main/components");
        assert_eq!(steps[1].step_type, CreationStepType::WriteSourceFile);
        assert_eq!(steps[1].parameters["path"], "main/main.cpp");
        assert!(
            steps[1].parameters["content"]
                .as_str()
                .is_some_and(|content| content.contains("int main")),
            "the source beside `type` is the step's content: {:?}",
            steps[1].parameters
        );
        assert_eq!(steps[2].step_type, CreationStepType::Build);
    }

    /// Regression test from the SECOND live deterministic-json response
    /// (captured 2026-08-18): the model used `"step"` as the step-type key
    /// (previous run used `"action"`) with `"arguments"` for params, and a
    /// multi-line Rust source in `content`. The alias normalization in
    /// `step_from_value` must accept both.
    #[test]
    fn parse_fill_steps_handles_step_key_shape() {
        let json = r#"{
  "steps": [
    {
      "step": "declare_dependencies",
      "arguments": {
        "path": "/Users/steve/naturesense/ai-traps-mcp/Cargo.toml",
        "dependencies": [
          { "name": "rmcp", "version": "0.1" },
          { "name": "tokio", "version": "1" }
        ]
      }
    },
    {
      "step": "write_source_file",
      "arguments": {
        "path": "src/main.rs",
        "content": "use rmcp::Error;\nfn main() -> Result<(), Error> { Ok(()) }\n"
      }
    },
    {
      "step": "build",
      "arguments": { "path": "/Users/steve/naturesense/ai-traps-mcp" }
    },
    {
      "step": "parse_and_validate",
      "arguments": { "path": "/Users/steve/naturesense/ai-traps-mcp" }
    }
  ]
}"#;
        let steps = parse_fill_steps(json).expect("step-key body must parse");
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0].step_type, CreationStepType::AddDependency);
        assert_eq!(
            steps[0].parameters["path"],
            "/Users/steve/naturesense/ai-traps-mcp/Cargo.toml"
        );
        assert_eq!(steps[1].step_type, CreationStepType::WriteSourceFile);
        assert_eq!(
            steps[1].parameters["content"],
            "use rmcp::Error;\nfn main() -> Result<(), Error> { Ok(()) }\n"
        );
        assert_eq!(steps[2].step_type, CreationStepType::Build);
        assert_eq!(steps[3].step_type, CreationStepType::ParseAndValidate);
    }

    /// Regression test for the Observed failure: the LLM returned 15435 chars
    /// but the reply was wrapped (an object containing a `steps` array), which
    /// parse_fill_steps rejected and the plan fell back to the 2-step template.
    #[test]
    fn parse_fill_steps_handles_wrapped_steps_object() {
        let json = r#"
{
  "steps": [
    { "declare_dependencies": { "path": "Cargo.toml", "dependencies": [{"name": "tokio", "version": "1"}] } },
    { "write_source_file": { "path": "src/main.rs" } }
  ]
}
"#;
        let steps = parse_fill_steps(json).expect("should unwrap the steps object");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_type, CreationStepType::AddDependency);
        // Backfill ensures a path/content default even when the model omitted content.
        assert_eq!(steps[1].step_type, CreationStepType::WriteSourceFile);
        assert_eq!(steps[1].parameters["content"], "// TODO: implement\n");
    }

    #[test]
    fn gitignore_for_build_system_covers_scaffold_and_spire() {
        let cargo = ProjectCreationActor::gitignore_for_build_system("Cargo");
        assert!(cargo.contains("/target/"));
        assert!(cargo.contains(".spire/"));
        // Cargo must NOT ignore `build/` globally — a real source dir can exist.
        assert!(!cargo.contains("\nbuild/\n"));

        let meson = ProjectCreationActor::gitignore_for_build_system("Meson");
        assert!(meson.contains("meson-private/"));
        assert!(meson.contains(".spire/"));

        let python = ProjectCreationActor::gitignore_for_build_system("Python");
        assert!(python.contains("__pycache__/"));
        assert!(python.contains(".spire/"));
    }

    /// Regression: the LLM emits ABSOLUTE paths into fill plans. The structural
    /// guard must normalize them against root_dir BEFORE matching fill roots —
    /// otherwise every absolute-path write (e.g. /…/src/main.rs) is rejected
    /// as "not under a fill root" and the plan's content never lands on disk.
    /// Phase B: the WriteHalContract step is the Stage 0 gate. A canonical
    /// abstract-class header passes validation and proceeds to the filesystem
    /// write; a non-abstract header (no public pure-virtual methods) is
    /// rejected BEFORE any disk write with a clear "rejected" message.
    /// (The dummy filesystem channel has no receiver, so the write stage's
    /// `send` errors — but that path is reached only when validation passed.)
    #[test]
    fn write_hal_contract_step_validates_before_write() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // LIVE dummy filesystem receiver: reply `Err` to any WriteFile so the
        // write stage's `r.await` resolves promptly (a bare sender with no
        // receiver would block forever in execute_step's `r.await`).
        let (fs_tx, mut fs_rx) = mpsc::channel::<spire_core::modules::FilesystemMessage>(4);
        let _fs_task = rt.spawn(async move {
            while let Some(msg) = fs_rx.recv().await {
                if let spire_core::modules::FilesystemMessage::WriteFile { reply_to, .. } = msg {
                    let _ = reply_to.send(Err("dummy fs unavailable".to_string()));
                }
            }
        });
        let actor = ProjectCreationActor::new(fs_tx, dummy_bm(), dummy_mcp());
        let root = PathBuf::from("/tmp/hal-contract-gate");

        // Valid canonical contract (virtual destructor + pure-virtual methods).
        let valid_header = r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#;
        let ok_step = CreationStep {
            id: "hal-1".to_string(),
            step_type: CreationStepType::WriteHalContract,
            description: "Write HAL contract hal/api/camera_hal.hpp".to_string(),
            status: StepStatus::Pending,
            parameters: serde_json::json!({
                "path": "hal/api/camera_hal.hpp",
                "content": valid_header
            }),
            result: None,
        };
        let res = rt.block_on(actor.execute_step(&root, &ok_step)).unwrap();
        assert!(
            !res.message.contains("rejected"),
            "valid contract must pass validation (reaches the write stage): {}",
            res.message
        );

        // Invalid: no pure-virtual methods → rejected before any disk write.
        let bad_step = CreationStep {
            id: "hal-2".to_string(),
            step_type: CreationStepType::WriteHalContract,
            description: "Write HAL contract hal/api/not_abstract.hpp".to_string(),
            status: StepStatus::Pending,
            parameters: serde_json::json!({
                "path": "hal/api/not_abstract.hpp",
                "content": "class NotAbstract {\npublic:\n    void do_thing();\n};\n"
            }),
            result: None,
        };
        let res = rt.block_on(actor.execute_step(&root, &bad_step)).unwrap();
        assert!(
            res.message.contains("HAL contract rejected"),
            "non-abstract header must be rejected with the contract message: {}",
            res.message
        );
    }

    /// Phase B: the LLM step parser accepts the two new step types so the
    /// wizard's plan JSON (`write_hal_contract` / `write_hal_implementation`)
    /// round-trips through the same path as every other step.
    #[test]
    fn parse_step_type_accepts_hal_step_names() {
        assert_eq!(
            parse_step_type("write_hal_contract"),
            Some(CreationStepType::WriteHalContract)
        );
        assert_eq!(
            parse_step_type("write_hal_implementation"),
            Some(CreationStepType::WriteHalImplementation)
        );
    }

    #[test]
    fn execute_step_accepts_absolute_path_under_fill_root() {
        let root = PathBuf::from("/tmp/fill-abs-test");
        let _ = std::fs::create_dir_all(root.join("src"));
        // A LIVE dummy filesystem receiver: keep the channel open and reply
        // `Err` to any WriteFile message so the step completes as a failed
        // write (success=false) — never a guard rejection and never a hard
        // "channel closed" error that would abort the whole plan.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (fs_tx, mut fs_rx) = mpsc::channel::<spire_core::modules::FilesystemMessage>(4);
        let _fs_task = rt.spawn(async move {
            while let Some(msg) = fs_rx.recv().await {
                if let spire_core::modules::FilesystemMessage::WriteFile { reply_to, .. } = msg {
                    let _ = reply_to.send(Err("dummy fs unavailable".to_string()));
                }
            }
        });
        let mut actor = ProjectCreationActor::new(fs_tx, dummy_bm(), dummy_mcp());
        actor.active_spec = Some(crate::subsystems::build::build_manager::ScaffoldSpec {
            structural_files: vec!["Cargo.toml".to_string()],
            fill_roots: vec!["src".to_string()],
            dependency_sections: vec!["Cargo.toml".to_string()],
            platform_targets: vec![],
            build_system: "Cargo".to_string(),
            files: vec![],
            structure: spire_core::build_types::ProjectStructure::default(),
            embedded: false,
            design_warning: None,
        });
        let step = CreationStep {
            id: "fill-1".to_string(),
            step_type: CreationStepType::WriteSourceFile,
            description: "Write main.rs".to_string(),
            status: StepStatus::Pending,
            parameters: serde_json::json!({
                // Absolute path under root_dir/src → must pass the guard.
                "path": root.join("src/main.rs").to_string_lossy().to_string(),
                "content": "fn main() {}\n"
            }),
            result: None,
        };
        // The live dummy receiver replies Err to any WriteFile, so execution
        // completes as a failed write (success=false) — never a guard rejection
        // and never a hard "channel closed" error that aborts the whole plan.
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(actor.execute_step(&root, &step));
        let res = result.unwrap();
        assert!(
            res.success || !res.message.contains("fill root"),
            "absolute src/main.rs write must not be guard-rejected: {}",
            res.message
        );
    }

    #[test]
    fn build_config_for_rust_has_cargo_sections() {
        let actor = ProjectCreationActor::new(dummy_fs(), dummy_bm(), dummy_mcp());
        let config = actor.generate_build_config("Rust", "my_cli", "CSV to JSON");
        assert!(config.contains("[package]"));
        assert!(config.contains("name = \"my_cli\""));
        assert!(config.contains("[dependencies]"));
    }

    #[test]
    fn build_config_for_swift_has_package_description() {
        let actor = ProjectCreationActor::new(dummy_fs(), dummy_bm(), dummy_mcp());
        let config = actor.generate_build_config("Swift", "my_app", "iOS app");
        assert!(config.contains("// swift-tools-version"));
        assert!(config.contains("PackageDescription"));
        assert!(config.contains("name: \"my_app\""));
    }

    /// **A scaffold does not render over a composition that is already there**, and everything it does
    /// write is stated from the design that file carries.
    ///
    /// This is the write half of the resolution rule (see `idf_projects::design_for_scaffold`), and both
    /// failures it closes are silent: a scaffold that wrote `REQUIRES` from nothing left an application
    /// whose own `CMakeLists.txt` did not name the components its composition includes — a build error at
    /// the `#include`, naming neither file; and one that wrote `composition.spire` from the design its
    /// caller still held discarded the comments a person had left in it, which is the one thing a file has
    /// that a record cannot.
    #[test]
    fn a_scaffold_leaves_an_authored_composition_alone_and_states_the_record_from_it() {
        use crate::build::application_spec::{examples, parse_spec};
        use crate::build::idf_projects::{
            application_scaffold, design_for_scaffold, render_composition, APPLICATION_FILE,
            COMPOSITION_FILE,
        };

        let authored = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let stale = parse_spec(examples::INSECT_TRAP).expect("the other worked example parses");
        let root = tempfile::tempdir().unwrap();

        // The file a person edited: the reviewed composition, plus a comment that is theirs and can only
        // live in the file.
        let on_disk = format!(
            "{}\n# `moving_average` is a library we already have; the two drivers are stubs to fill.\n",
            render_composition(&authored).unwrap()
        );
        std::fs::write(root.path().join(COMPOSITION_FILE), &on_disk).unwrap();

        // A caller that still holds the design it used to — it resolves to the tree's, which is the point.
        let resolved = design_for_scaffold(
            root.path(),
            Some(spire_core::build_types::ProjectStructure::IdfApplication),
            Some(stale),
        )
        .expect("the tree's design resolves");
        assert_eq!(resolved.spec.as_ref(), Some(&authored));

        let out = application_scaffold(
            "pm25-meter",
            &[],
            "../sensor-library",
            resolved.spec.as_ref(),
        )
        .expect("the application scaffolds");
        // As `ScaffoldProject` starts it: nothing recorded, so what is locked is what this write decided.
        let mut spec = spec_from_scaffold_output(out.clone(), "C++");
        // …and the report the leg threads onto it, from the same resolution — so the wizard's banner
        // has something to show on exactly this run, the one where the reviewed design was dropped.
        spec.design_warning = resolved.dropped;
        spec.structural_files.clear();
        materialize_scaffold_files(root.path(), &out.files, &mut spec)
            .expect("the scaffold is written");

        // The composition is the person's, byte for byte — comment and all.
        assert_eq!(
            std::fs::read_to_string(root.path().join(COMPOSITION_FILE)).unwrap(),
            on_disk,
            "a scaffold does not render over the file it read the design from"
        );
        assert!(
            spec.structural_files.iter().any(|p| p == COMPOSITION_FILE),
            "and it is still locked, whether or not this run wrote it: {:?}",
            spec.structural_files
        );

        // The record, by contrast, is the tool's: written, and stating the composition the file holds
        // rather than the design the caller had.
        assert_eq!(
            parse_spec(&std::fs::read_to_string(root.path().join(APPLICATION_FILE)).unwrap())
                .expect("the record parses"),
            authored,
            "the record follows the file it was read from"
        );

        // And the application's own `CMakeLists.txt` names the components the composition names — the
        // build failure this whole resolution exists to prevent. The framework first, then the
        // composition's own (`messages` and one component per actor), then the library's.
        let cmake = std::fs::read_to_string(root.path().join("main/CMakeLists.txt")).unwrap();
        assert!(
            cmake.contains(
                "REQUIRES actors toolkit messages sampler air_quality view touch power sps30 sht20 moving_average"
            ),
            "the requirements are stated from the design the tree carries: {cmake}"
        );

        // And the **report**: this run dropped the caller's design, so the spec the wizard reads
        // carries the sentence for it. Silence here is the failure the report exists to prevent — a
        // sheet that reviewed one decomposition and a tree built from another, saying nothing.
        let told = spec
            .design_warning
            .expect("the drop is reported on the spec the wizard reads");
        assert!(
            told.contains(COMPOSITION_FILE),
            "and the report names the file that decides, so it is actionable: {told}"
        );
    }

    /// A tree with **no** composition gets one written: the file's first writing, not a rewrite over
    /// something — which is what keeps the scaffold's `REQUIRES`, its record and its composition one
    /// design rather than three statements about it.
    #[test]
    fn a_scaffold_writes_the_composition_it_was_designed_from_when_there_is_none() {
        use crate::build::application_spec::{examples, parse_composition, parse_spec};
        use crate::build::idf_projects::{
            application_scaffold, design_for_scaffold, COMPOSITION_FILE,
        };

        let design = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let root = tempfile::tempdir().unwrap();
        let resolved = design_for_scaffold(
            root.path(),
            Some(spire_core::build_types::ProjectStructure::IdfApplication),
            Some(design.clone()),
        )
        .expect("the caller's design resolves");
        assert_eq!(
            resolved.spec.as_ref(),
            Some(&design),
            "there is no file to prefer"
        );
        assert_eq!(
            resolved.dropped, None,
            "and the ordinary case reports nothing: this is a project being created for the first time"
        );

        let out = application_scaffold(
            "pm25-meter",
            &[],
            "../sensor-library",
            resolved.spec.as_ref(),
        )
        .expect("the application scaffolds");
        let mut spec = spec_from_scaffold_output(out.clone(), "C++");
        spec.structural_files.clear();
        materialize_scaffold_files(root.path(), &out.files, &mut spec)
            .expect("the scaffold is written");

        assert_eq!(
            parse_composition(
                &std::fs::read_to_string(root.path().join(COMPOSITION_FILE)).unwrap()
            )
            .expect("the file it wrote is readable by the reader it is for"),
            design,
            "the composition is written when there is none, and it is the reviewed design"
        );
    }
}
