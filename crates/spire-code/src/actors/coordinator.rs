// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! CoordinatorActor — main orchestrator that routes JSON-RPC methods to actors.
//!
//! The coordinator receives JSON-RPC requests from the transport layer and
//! dispatches them to the appropriate actor (chat, tools, mcp_client, llm, etc.).

use async_trait::async_trait;
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::actors::system::SystemMessage;
use crate::subsystems::planning::intent_router::{IntentRouterMessage, RouteResult};
use crate::subsystems::planning::plan_orchestrator::PlanOrchestratorMessage;
use crate::subsystems::project::project_query::ProjectQueryMessage;
use spire_core::actors::tool_providers::ToolRouterMessage;
use spire_core::actors::tools::ToolsMessage;
use spire_core::actors::Actor;
use spire_core::models::memory_graph::{McpConfigFile, McpServerConfigEntry};
use spire_core::subsystems::chat::chat::ChatMessage;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::subsystems::llm::llm::LlmMessage;
use spire_core::subsystems::mcp::mcp_client::McpClientMessage;
use spire_core::transport::socket::TransportMessage;

// FFI-inline RPC handlers moved into this single router (see `SetFfiDeps`).
use crate::ffi::{dummy_tx, populate_target_graph, resolve_project_root, serialize_analysis};
use crate::subsystems::project::project_analyzer::ProjectAnalysis;
use crate::subsystems::project::project_analyzer::ProjectAnalyzerMessage;
use crate::subsystems::project::project_build::ProjectBuildMessage;
use crate::subsystems::project::project_creation::ProjectCreationMessage;
use crate::subsystems::project::project_sync::ProjectSyncMessage;
use crate::subsystems::project::spec_design::SpecDesignMessage;
use spire_actor::registry::ServiceRegistry;
use spire_core::actors::rag::RagMessage;
use spire_core::subsystems::tools::file_watcher::{FileChangeNotification, FileWatcherMessage};

/// Messages for the Coordinator actor.
pub enum CoordinatorMessage {
    /// Handle a JSON-RPC request from the extension.
    HandleRequest {
        method: String,
        params: serde_json::Value,
        response_tx: tokio::sync::oneshot::Sender<serde_json::Value>,
    },
    /// Attach the app-only dispatch dependencies so the FFI-inline RPC methods
    /// (`project/open`, `createProject/*`, `rag/*`, …) can be routed here too.
    /// The FFI composition root sends this once at startup, before any request;
    /// the standalone binary never sends it (its extension flow uses the tools/
    /// coordinator methods, so the moved handlers return a clear error there).
    SetFfiDeps {
        registry: Arc<ServiceRegistry>,
        state: Arc<FfiSharedState>,
    },
    /// Shut down the coordinator.
    Shutdown,
}

/// App-only state that lets the coordinator handle the FFI-inline RPC methods
/// so ALL method routing lives in this one router instead of a parallel
/// dispatch path in `ffi.rs::process_json_request`.
pub struct FfiSharedState {
    /// Currently opened project root (set by `project/open`).
    pub project_root: std::sync::Mutex<Option<PathBuf>>,
    /// Latest project analysis (set by `project/open` / `AnalyzeProject`).
    pub analysis: std::sync::Mutex<Option<ProjectAnalysis>>,
    /// File-watcher output channel (`project/open` StartWatching).
    pub watcher_out_tx: mpsc::Sender<FileChangeNotification>,
}

/// The Coordinator actor routes requests to the appropriate sub-actors.
pub struct CoordinatorActor {
    /// Sender for the chat actor.
    chat_tx: mpsc::Sender<ChatMessage>,
    /// Sender for the tools actor.
    tools_tx: mpsc::Sender<ToolsMessage>,
    /// Sender for the MCP client actor.
    mcp_client_tx: mpsc::Sender<McpClientMessage>,
    /// Sender for the LLM actor.
    llm_tx: mpsc::Sender<LlmMessage>,
    /// Sender for the system actor.
    system_tx: mpsc::Sender<SystemMessage>,
    /// Sender for the memory graph actor (knowledge graph + config storage).
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    /// Sender for the project query actor (semantic project queries).
    project_query_tx: mpsc::Sender<ProjectQueryMessage>,
    /// Sender for the intent router actor (routes user queries to matched intents).
    intent_router_tx: mpsc::Sender<IntentRouterMessage>,
    /// Sender for the tool router actor (routes tool calls to appropriate backend).
    tool_router_tx: mpsc::Sender<ToolRouterMessage>,
    /// Sender for the plan orchestrator actor (creates and executes multi-step plans).
    plan_orchestrator_tx: mpsc::Sender<PlanOrchestratorMessage>,
    /// Transport sender for forwarding VSC tool calls / notifications to the extension.
    transport_tx: mpsc::Sender<TransportMessage>,
    /// One free-form AppSpec design session per project (created lazily).
    spec_design_sessions: Arc<Mutex<HashMap<String, mpsc::Sender<SpecDesignMessage>>>>,
    /// App-only dispatch dependencies (registry + shared state), attached via
    /// `SetFfiDeps`. `None` in the standalone binary.
    registry: Option<Arc<ServiceRegistry>>,
    ffi_state: Option<Arc<FfiSharedState>>,
}

impl CoordinatorActor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chat_tx: mpsc::Sender<ChatMessage>,
        tools_tx: mpsc::Sender<ToolsMessage>,
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
        llm_tx: mpsc::Sender<LlmMessage>,
        system_tx: mpsc::Sender<SystemMessage>,
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        project_query_tx: mpsc::Sender<ProjectQueryMessage>,
        intent_router_tx: mpsc::Sender<IntentRouterMessage>,
        tool_router_tx: mpsc::Sender<ToolRouterMessage>,
        plan_orchestrator_tx: mpsc::Sender<PlanOrchestratorMessage>,
        transport_tx: mpsc::Sender<TransportMessage>,
    ) -> Self {
        Self {
            chat_tx,
            tools_tx,
            mcp_client_tx,
            llm_tx,
            system_tx,
            memory_graph_tx,
            project_query_tx,
            intent_router_tx,
            tool_router_tx,
            plan_orchestrator_tx,
            transport_tx,
            spec_design_sessions: Arc::new(Mutex::new(HashMap::new())),
            registry: None,
            ffi_state: None,
        }
    }

    /// HAL fix proposal (file-by-file LLM flow): lint -> whole-file rewrite
    /// prompt -> LLM -> strip fences -> structural syntax check (retry once)
    /// -> return {status, path, proposed_content, issues} for Accept/Reject.
    async fn propose_hal_fix(&self, root: &str, path: &str) -> serde_json::Value {
        let issues =
            crate::build::generic_helpers::hal_doc_lint_file(std::path::Path::new(root), path);
        if issues.is_empty() {
            return serde_json::json!({ "status": "clean", "path": path });
        }
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let mut prompt =
            crate::build::generic_helpers::hal_doc_fix_prompt_whole(path, &content, &issues);
        let mut proposed = String::new();
        // Route through the LLM actor's mailbox (self.llm_tx) so all LLM work
        // shares the single actor-owned config/client. A missing actor returns
        // a clear error; an unconfigured key surfaces as the actor's error.
        let llm_tx = self.llm_tx.clone();
        for _attempt in 0..2 {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            if llm_tx
                .send(crate::actors::LlmMessage::Complete {
                    prompt: prompt.clone(),
                    role: spire_core::subsystems::llm::llm::LlmModelRole::Coding,
                    reply_to: reply_tx,
                })
                .await
                .is_err()
            {
                return serde_json::json!({ "status": "error", "error": "LLM actor unavailable" });
            }
            let text = match reply_rx.await {
                Ok(Ok(t)) => t,
                Ok(Err(e)) => {
                    return serde_json::json!({ "status": "error", "error": e.to_string() })
                }
                Err(e) => {
                    return serde_json::json!({
                        "status": "error",
                        "error": format!("LLM reply lost: {e}")
                    })
                }
            };
            proposed = crate::build::generic_helpers::strip_code_fences(&text);
            let check = crate::build::generic_helpers::cpp_syntax_check(&proposed);
            if check.ok {
                break;
            }
            let hint: Vec<String> = check
                .errors
                .iter()
                .map(|e| format!("line {} col {}: {}", e.line, e.col, e.kind))
                .collect();
            prompt.push_str(&format!(
                "\n\nYour previous attempt had C++ syntax errors: {}. Fix them and return the complete corrected header again.",
                hint.join("; ")
            ));
        }
        serde_json::json!({
            "status": "proposed",
            "path": path,
            "proposed_content": proposed,
            "issues": issues,
        })
    }

    /// Current compiler/linter diagnostics for ONE file, rendered as
    /// `<path>:<line>:<col>: <message>` lines (severity error/warning only).
    async fn compile_diagnostics_for(&self, file: &str) -> Vec<String> {
        let mut out = Vec::new();
        for node in self.diagnostic_nodes().await {
            if node.get("file").and_then(|v| v.as_str()).unwrap_or("") != file {
                continue;
            }
            let sev = node.get("severity").and_then(|v| v.as_str()).unwrap_or("");
            if sev != "error" && sev != "warning" {
                continue;
            }
            let msg = node
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if msg.is_empty() {
                continue;
            }
            let line = node.get("line").and_then(|v| v.as_u64());
            let col = node.get("column").and_then(|v| v.as_u64());
            out.push(match (line, col) {
                (Some(l), Some(c)) => format!("{file}:{l}:{c}: {msg}"),
                (Some(l), None) => format!("{file}:{l}: {msg}"),
                _ => format!("{file}: {msg}"),
            });
        }
        out
    }

    /// Compile-error fix proposal (file-by-file LLM flow): current diagnostics
    /// for ONE file -> whole-file rewrite prompt -> LLM -> strip fences ->
    /// structural syntax check (retry once) -> {status, path, proposed_content,
    /// errors} for Accept/Reject in the UI.
    ///
    /// NOTHING is written here — the caller reviews the proposal and writes it
    /// only on Accept, then rebuilds.
    async fn propose_compile_fix(&self, _root: &str, file: &str) -> serde_json::Value {
        self.propose_compile_fix_for(file, std::path::Path::new(file))
            .await
    }

    /// As [`Self::propose_compile_fix`], but with the on-disk path supplied
    /// separately.
    ///
    /// Compilers report paths relative to the directory the build ran in, so the
    /// autofix loop resolves the real file while the diagnostics stay keyed by the
    /// raw string the compiler printed.
    async fn propose_compile_fix_for(
        &self,
        file: &str,
        path: &std::path::Path,
    ) -> serde_json::Value {
        let lower = file.to_lowercase();
        let is_cpp = [".cpp", ".cc", ".cxx", ".c", ".hpp", ".h", ".hh"]
            .iter()
            .any(|ext| lower.ends_with(ext));
        if !is_cpp {
            return serde_json::json!({
                "status": "error",
                "error": format!("auto-fix is only available for C/C++ sources (got '{file}')")
            });
        }

        let errors = self.compile_diagnostics_for(file).await;
        if errors.is_empty() {
            return serde_json::json!({
                "status": "clean",
                "path": path.to_string_lossy()
            });
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                return serde_json::json!({
                    "status": "error",
                    "error": format!("cannot read {}: {e}", path.display())
                })
            }
        };

        let prompt = crate::build::generic_helpers::compile_fix_prompt(
            &path.to_string_lossy(),
            &content,
            &errors,
        );
        // One shared LLM + structural-check path (see `llm_rewrite`).
        let (proposed, _syntax_ok) = match self.llm_rewrite(prompt).await {
            Ok(result) => result,
            Err(e) => return serde_json::json!({ "status": "error", "error": e }),
        };
        serde_json::json!({
            "status": "proposed",
            "path": path.to_string_lossy(),
            "proposed_content": proposed,
            "errors": errors,
        })
    }

    /// Ask the model for a WHOLE-FILE rewrite of `prompt`, then validate the
    /// result structurally: a tree-sitter parse of the C++ catches truncated or
    /// garbled output before it can ever be written, and one retry feeds the
    /// syntax errors back so the model can correct itself.
    ///
    /// Returns `(proposed_content, passed_structural_check)`. A `false` second
    /// element means the model produced something that does not parse — callers
    /// that write files autonomously must refuse it; the interactive review flow
    /// may still show it.
    async fn llm_rewrite(&self, mut prompt: String) -> Result<(String, bool), String> {
        let llm_tx = self.llm_tx.clone();
        let mut proposed = String::new();
        for _attempt in 0..2 {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            if llm_tx
                .send(crate::actors::LlmMessage::Complete {
                    prompt: prompt.clone(),
                    role: spire_core::subsystems::llm::llm::LlmModelRole::Coding,
                    reply_to: reply_tx,
                })
                .await
                .is_err()
            {
                return Err("LLM actor unavailable".to_string());
            }
            let text = match reply_rx.await {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => return Err(e.to_string()),
                Err(e) => return Err(format!("LLM reply lost: {e}")),
            };
            proposed = crate::build::generic_helpers::strip_code_fences(&text);
            let check = crate::build::generic_helpers::cpp_syntax_check(&proposed);
            if check.ok {
                return Ok((proposed, true));
            }
            let hint: Vec<String> = check
                .errors
                .iter()
                .map(|e| format!("line {} col {}: {}", e.line, e.col, e.kind))
                .collect();
            prompt.push_str(&format!(
                "\n\nYour previous attempt had C++ syntax errors: {}. Fix them and return the complete corrected file again.",
                hint.join("; ")
            ));
        }
        Ok((proposed, false))
    }

    /// Warning-fix proposal for the autonomous safe-warning phase.
    ///
    /// Writes nothing. Unlike the interactive flow this refuses a rewrite that
    /// failed the structural check, because the loop applies it unattended.
    async fn propose_warning_fix_for(
        &self,
        path: &std::path::Path,
        warnings: &[String],
    ) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let prompt = crate::build::generic_helpers::warning_fix_prompt(
            &path.to_string_lossy(),
            &content,
            warnings,
        );
        let (proposed, syntax_ok) = self.llm_rewrite(prompt).await.ok()?;
        (syntax_ok && !proposed.trim().is_empty()).then_some(proposed)
    }

    /// Every Diagnostic node currently recorded for the project.
    ///
    /// The memory graph is per-project, so no path filtering is needed here.
    async fn diagnostic_nodes(&self) -> Vec<spire_core::models::memory_graph::AttrNode> {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = registry
            .get::<MemoryGraphMessage>("memory_graph")
            .unwrap_or_else(dummy_tx)
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Diagnostic".to_string()),
                subtype: None,
                name: None,
                limit: Some(4000),
                reply_to: t,
            })
            .await;
        match r.await {
            Ok(Ok(nodes)) => nodes,
            _ => Vec::new(),
        }
    }

    /// Project the graph's Diagnostic nodes onto the loop's raw view.
    async fn raw_diagnostics(&self) -> Vec<crate::build::autofix::RawDiagnostic> {
        self.diagnostic_nodes()
            .await
            .iter()
            .map(|n| crate::build::autofix::RawDiagnostic {
                build_type: n
                    .get("build_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                severity: n
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                file: n
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                message: n
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
            .collect()
    }

    /// Compile ERROR diagnostics from the **build** run, grouped by file — the
    /// error phase's input. The scoping policy lives in
    /// [`crate::build::autofix::build_errors_from`] so it is unit-tested.
    async fn build_error_diagnostics_by_file(&self) -> crate::build::autofix::ErrorsByFile {
        crate::build::autofix::build_errors_from(&self.raw_diagnostics().await)
    }

    /// WARNING diagnostics grouped by file — the safe-warning phase's input.
    async fn warning_diagnostics_by_file(&self) -> crate::build::autofix::WarningsByFile {
        crate::build::autofix::warnings_from(&self.raw_diagnostics().await)
    }

    /// Analyzer warnings currently recorded (reported, never auto-rewritten).
    async fn warning_count(&self) -> usize {
        self.diagnostic_nodes()
            .await
            .iter()
            .filter(|n| n.get("severity").and_then(|v| v.as_str()) == Some("warning"))
            .count()
    }

    /// Invoke an in-process build tool through the ToolRouter (the same path the
    /// `tools/call` RPC uses).
    async fn call_tool_json(&self, tool: &str, args: serde_json::Value) -> serde_json::Value {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .tool_router_tx
            .send(ToolRouterMessage::CallTool {
                tool_name: tool.to_string(),
                args,
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return serde_json::json!({ "error": "ToolRouter actor not available" });
        }
        match rx.await {
            Ok(Ok(res)) => res,
            Ok(Err(e)) => serde_json::json!({ "error": e }),
            Err(_) => serde_json::json!({ "error": "ToolRouter actor response error" }),
        }
    }

    /// `build/autofix` — the autonomous **Fix & Verify** loop: compile, ask the
    /// model for a fix per error file, apply it, rebuild, keep what helped and
    /// roll back what did not, until the project compiles or the cap is hit.
    ///
    /// Handled here rather than by the build manager because it needs the LLM
    /// actor (which the build manager does not own). Nothing is written until a
    /// compile error actually exists, and every write is compile-verified and
    /// reversible, so this is safe to run unattended.
    async fn handle_build_autofix(&self, args: &serde_json::Value) -> serde_json::Value {
        let path = args
            .get("path")
            .or_else(|| args.get("root"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return serde_json::json!({ "error": "build/autofix needs a project path" });
        }
        let max_rounds = args
            .get("maxRounds")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .clamp(1, 10) as usize;

        // The UI passes the selected subproject's path; the build directories
        // (and therefore the relative compiler paths) live at the project root.
        let project_root = crate::build::autofix::find_project_root(std::path::Path::new(path));
        let bases = crate::build::autofix::diagnostic_bases(&project_root);

        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty())
            .map(|s| s.to_string());
        // A run can arrive with no platform selected. Resolve one, or refuse with
        // something actionable: without it `build` compiles whichever build
        // directory discovery returns first, which may be a different target — the
        // loop would then be "fixing" the wrong platform.
        let platform = match crate::build::autofix::resolve_platform(
            &project_root,
            args.get("platform").and_then(|v| v.as_str()),
            target.as_deref(),
        ) {
            Ok(platform) => platform,
            Err(reason) => {
                tracing::warn!("[COORDINATOR] build/autofix refused: {reason}");
                return serde_json::json!({
                    "success": false,
                    "error": reason,
                    "output": format!("Fix & Verify: {reason}"),
                });
            }
        };

        let driver = CoordinatorAutofix {
            coord: self,
            path: path.to_string(),
            platform: platform.clone(),
            target: target.clone(),
        };

        tracing::info!(
            "[COORDINATOR] build/autofix: path={path} root={} platform={:?} rounds<={max_rounds}",
            project_root.display(),
            platform
        );

        // Compile FIRST so the loop acts on the CURRENT errors rather than on
        // whatever the previous build happened to leave in the graph. If the build
        // cannot even start, stop before spending a single model call.
        let built = self
            .call_tool_json("build_build", driver.build_args())
            .await;
        if let Some(err) = built.get("error").and_then(|v| v.as_str()) {
            return serde_json::json!({
                "success": false,
                "error": err,
                "output": format!("Fix & Verify: the build could not run ({err}); nothing was changed."),
            });
        }

        let mut report = crate::build::autofix::run_autofix(&driver, &bases, max_rounds).await;
        report.platform = platform;

        // Name the built executable: Meson places it at <build-<platform>>/<target>,
        // so report it only when the run ended compiling cleanly AND the file is
        // really there (never a guessed path).
        if report.errors_after == 0 {
            if let (Some(platform), Some(target)) = (report.platform.as_ref(), target.as_ref()) {
                let candidate = project_root.join(format!("build-{platform}")).join(target);
                if candidate.is_file() {
                    report.artifact = Some(candidate.to_string_lossy().to_string());
                }
            }
        }
        tracing::info!(
            "[COORDINATOR] build/autofix done: success={} rounds={} fixed={} reverted={} errors {}→{}",
            report.success,
            report.rounds,
            report.files_fixed.len(),
            report.files_reverted.len(),
            report.errors_before,
            report.errors_after
        );

        let mut value = serde_json::to_value(&report).unwrap_or_else(|_| serde_json::json!({}));
        if let serde_json::Value::Object(ref mut m) = value {
            m.insert("output".to_string(), serde_json::json!(report.summary()));
            m.insert(
                "command".to_string(),
                serde_json::json!("meson compile + LLM fix loop"),
            );
        }
        value
    }

    /// Send a tool event notification to the extension via the transport actor.
    async fn send_tool_event(&self, event: &str, payload: &serde_json::Value) {
        let _ = self
            .transport_tx
            .send(TransportMessage::SendNotification {
                method: format!("event/tool/{}", event),
                params: payload.clone(),
            })
            .await;
    }

    /// Call a VS Code extension tool via the TransportActor.
    async fn call_extension_tool(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.transport_tx
            .send(TransportMessage::CallExtension {
                method: tool_name.to_string(),
                params: args.clone(),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Transport send error: {}", e))?;

        rx.await
            .map_err(|e| format!("Transport response error: {}", e))?
    }
}

/// Bridges the actor-free autofix loop ([`crate::build::autofix`]) to the real
/// LLM and build actors.
struct CoordinatorAutofix<'a> {
    coord: &'a CoordinatorActor,
    /// Path to build, exactly as the UI sent it (may be a subproject).
    path: String,
    platform: Option<String>,
    target: Option<String>,
}

impl CoordinatorAutofix<'_> {
    fn build_args(&self) -> serde_json::Value {
        let mut args = serde_json::json!({ "path": self.path });
        if let Some(platform) = &self.platform {
            args["platform"] = serde_json::json!(platform);
        }
        if let Some(target) = &self.target {
            args["target"] = serde_json::json!(target);
        }
        args
    }
}

#[async_trait]
impl crate::build::autofix::AutofixDriver for CoordinatorAutofix<'_> {
    async fn errors(&self) -> crate::build::autofix::ErrorsByFile {
        self.coord.build_error_diagnostics_by_file().await
    }

    async fn propose(&self, file: &str, path: &std::path::Path) -> Option<String> {
        // Read-only: the loop decides whether the rewrite is written.
        let proposal = self.coord.propose_compile_fix_for(file, path).await;
        if proposal.get("status").and_then(|v| v.as_str()) != Some("proposed") {
            return None;
        }
        proposal
            .get("proposed_content")
            .and_then(|v| v.as_str())
            .filter(|c| !c.trim().is_empty())
            .map(|c| c.to_string())
    }

    async fn warnings(&self) -> crate::build::autofix::WarningsByFile {
        self.coord.warning_diagnostics_by_file().await
    }

    async fn rebuild(&self) -> crate::build::autofix::ErrorsByFile {
        let _ = self
            .coord
            .call_tool_json("build_build", self.build_args())
            .await;
        self.coord.build_error_diagnostics_by_file().await
    }

    async fn propose_warning_fix(
        &self,
        _file: &str,
        path: &std::path::Path,
        warnings: &[String],
    ) -> Option<String> {
        // Only ever called for warnings classified as safe.
        self.coord.propose_warning_fix_for(path, warnings).await
    }

    async fn lint(&self) -> usize {
        let _ = self
            .coord
            .call_tool_json("build_lint", self.build_args())
            .await;
        self.coord.warning_count().await
    }
}

#[async_trait]
impl Actor for CoordinatorActor {
    type Message = CoordinatorMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            CoordinatorMessage::HandleRequest {
                method,
                params,
                response_tx,
            } => {
                tracing::info!(
                    "[COORDINATOR] REQUEST received: method={}, params_keys={:?}",
                    method,
                    params
                        .as_object()
                        .map(|m| m.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default()
                );
                let result = self.route_request(&method, params).await;
                let _ = response_tx.send(result);
            }
            CoordinatorMessage::SetFfiDeps { registry, state } => {
                self.registry = Some(registry);
                self.ffi_state = Some(state);
                tracing::info!("Coordinator: FFI dispatch deps attached");
            }
            CoordinatorMessage::Shutdown => {
                tracing::info!("Coordinator: shutting down");
            }
        }
    }
}

impl CoordinatorActor {
    async fn route_request(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        // project/getBuildTarget is invoked via the `tools/call` JSON-RPC
        // envelope from Swift:
        //   {"method":"tools/call","params":{"tool":"project/getBuildTarget","args":{"name":...}}}
        // Answer directly from the in-memory analysis (authoritative BuildManager
        // result) for both the bare method and the envelope form.
        let is_build_target_call = method == "project/getBuildTarget"
            || (method == "tools/call"
                && params.get("tool").and_then(|v| v.as_str()) == Some("project/getBuildTarget"));
        if is_build_target_call {
            return self.handle_project_get_build_target(method, &params).await;
        }

        // "Fix & Verify" — the autonomous compile → fix → recompile loop. It needs
        // the LLM actor, which the build manager does not own, so the coordinator
        // answers it directly. The UI reaches it through the normal `tools/call`
        // envelope; `build/autofix` also works as a raw method.
        if method == "build/autofix"
            || (method == "tools/call"
                && params.get("tool").and_then(|v| v.as_str()) == Some("build_autofix"))
        {
            let args = if method == "tools/call" {
                params
                    .get("args")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            } else {
                params.clone()
            };
            return self.handle_build_autofix(&args).await;
        }

        // All rag/* RPCs route to the RAG actor (shared dispatch deps).
        if method.starts_with("rag/") {
            return self.handle_rag(method, &params).await;
        }

        match method {
            // ── App-only (FFI) methods — moved from ffi.rs so ALL routing
            // ── lives in this one router ──
            "project/open" => {
                return self.handle_project_open(&params).await;
            }
            "AnalyzeProject" => {
                return self.handle_analyze_project(&params).await;
            }
            "project/buildStatus" => {
                return self.handle_project_build_status(&params).await;
            }
            "project/diagnostics" => {
                return self.handle_project_diagnostics(&params).await;
            }
            "createProject/Plan" => {
                return self.handle_create_project_plan(&params).await;
            }
            "createProject/GeneratePlan" => {
                return self.handle_create_project_generate_plan(&params).await;
            }
            "createProject/Scaffold" => {
                return self.handle_create_project_scaffold(&params).await;
            }
            "createProject/Fill" => {
                return self.handle_create_project_fill(&params).await;
            }
            "createProject/GenerateSpec" => {
                return self.handle_create_project_generate_spec(&params).await;
            }
            "createProject/GenerateCode" => {
                return self.handle_create_project_generate_code(&params).await;
            }
            "createProject/ExecutePlan" => {
                return self.handle_create_project_execute_plan(&params).await;
            }
            "createProject/ExecuteStep" => {
                return self.handle_create_project_execute_step(&params).await;
            }
            // ── Chat methods ──
            "chat/getActive" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::GetActive { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Some(dialog)) => {
                        serde_json::to_value(dialog).unwrap_or(serde_json::Value::Null)
                    }
                    Ok(None) => serde_json::Value::Null,
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/getHistory" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::GetHistory { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(dialogs) => serde_json::to_value(dialogs).unwrap_or(serde_json::json!([])),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/append" => {
                let chat_id = params
                    .get("chatId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let role = params
                    .get("options")
                    .and_then(|o| o.get("role"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant");
                let widget = params.get("options").and_then(|o| o.get("widget")).cloned();
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::Append {
                        chat_id: chat_id.to_string(),
                        content: content.to_string(),
                        role: role.to_string(),
                        widget,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Ok(msg)) => serde_json::to_value(msg).unwrap_or(serde_json::Value::Null),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/clear" => {
                let chat_id = params
                    .get("chatId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::Clear {
                        chat_id: chat_id.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }
            "chat/setTitle" => {
                let chat_id = params
                    .get("chatId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");
                let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .chat_tx
                    .send(ChatMessage::SetTitle {
                        chat_id: chat_id.to_string(),
                        title: title.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Chat actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Chat actor response error"}),
                }
            }

            // ── spec-design methods (free-form AppSpec design step) ──
            "spec-design/start" => {
                return self.handle_spec_design_start(&params).await;
            }
            "spec-design/convert" => {
                return self.handle_spec_design_convert(&params).await;
            }
            "spec-design/accept" => {
                return self.handle_spec_design_accept(&params).await;
            }
            "spec-design/reopen" => {
                return self.handle_spec_design_reopen(&params).await;
            }
            "spec-design/state" => {
                return self.handle_spec_design_state(&params).await;
            }

            // ── Tool methods ──
            "tools/list" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .tools_tx
                    .send(ToolsMessage::ListTools { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Tools actor not available"});
                }
                match rx.await {
                    Ok(tools) => serde_json::to_value(tools).unwrap_or(serde_json::json!([])),
                    Err(_) => serde_json::json!({"error": "Tools actor response error"}),
                }
            }
            "tools/call" => {
                let tool = params.get("tool").and_then(|v| v.as_str()).unwrap_or("");
                let args = params
                    .get("args")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                // Emit tool/start event
                let tool_call_id = format!(
                    "call_direct_{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );
                self.send_tool_event(
                    "start",
                    &serde_json::json!({
                        "tool_name": tool,
                        "args": args,
                        "tool_call_id": tool_call_id,
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    }),
                )
                .await;

                let start = std::time::Instant::now();
                let result = self.call_tool_json(tool, args.clone()).await;
                let duration_ms = start.elapsed().as_millis() as u64;

                if result.get("error").is_some() {
                    self.send_tool_event(
                        "error",
                        &serde_json::json!({
                            "tool_name": tool,
                            "error": result["error"],
                            "duration_ms": duration_ms,
                            "tool_call_id": tool_call_id,
                        }),
                    )
                    .await;
                } else {
                    self.send_tool_event(
                        "result",
                        &serde_json::json!({
                            "tool_name": tool,
                            "result": result,
                            "duration_ms": duration_ms,
                            "tool_call_id": tool_call_id,
                        }),
                    )
                    .await;
                }

                result
            }

            // ── MCP Client methods ──
            "mcp/listServers" | "mcp/servers" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::GetServerDetails { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(details) => serde_json::to_value(details).unwrap_or(serde_json::json!([])),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }

            "mcp/loadConfig" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Memory graph actor not available"});
                }
                match rx.await {
                    Ok(Ok(servers)) => {
                        let count = servers.len();
                        let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers
                            .into_iter()
                            .filter_map(|entry| {
                                let transport = if let Some(url) = entry.url {
                                    spire_core::mcp::client::TransportConfig::Http {
                                        url,
                                        headers: entry.headers.unwrap_or_default(),
                                    }
                                } else {
                                    let command = entry.command?;
                                    spire_core::mcp::client::TransportConfig::Stdio {
                                        command,
                                        args: entry.args,
                                        env: entry.env.unwrap_or_default(),
                                    }
                                };
                                Some(spire_core::mcp::client::McpServerConfig {
                                    name: entry.name,
                                    transport,
                                    autostart: entry.autostart,
                                    build_type: None,
                                })
                            })
                            .collect();
                        let configs = Self::with_device_mcp_configs(configs);

                        let (tx2, rx2) = tokio::sync::oneshot::channel();
                        if self
                            .mcp_client_tx
                            .send(McpClientMessage::LoadConfigFromGraph {
                                servers: configs,
                                reply_to: tx2,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MCP client actor not available"});
                        }
                        let _ = rx2.await;
                        serde_json::json!({"success": true, "serverCount": count})
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "Memory graph actor response error"}),
                }
            }
            "mcp/connectAll" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::ConnectAll { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/connect" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::Connect {
                        server_name: server_name.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/disconnect" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::Disconnect {
                        server_name: server_name.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            // ── Device (on-hardware) servers — one per platform with `device:` ──
            "device/status" | "device/list" => {
                return self.handle_device_status().await;
            }
            "device/connect" => {
                let platform_id = params
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if platform_id.is_empty() {
                    return serde_json::json!({"error": "Missing platform"});
                }
                let server_name = format!("device-{platform_id}");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::Connect {
                        server_name: server_name.clone(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true, "server": server_name}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            // ── Deploy a test binary to a board and run it there ─────────────
            "device/test" => {
                return self.handle_device_test(&params).await;
            }
            "device/deploy" => {
                return self.handle_device_deploy(&params).await;
            }
            "mcp/disconnectAll" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::DisconnectAll { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/listServerTools" | "mcp/getTools" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::GetTools {
                        server_name: server_name.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Some(tools)) => serde_json::to_value(tools).unwrap_or(serde_json::json!([])),
                    Ok(None) => serde_json::json!([]),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/setInternalTools" => {
                let tools: Vec<rust_mcp_sdk::schema::Tool> = params
                    .get("tools")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::SetInternalTools {
                        tools,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }
            "mcp/callTool" => {
                let server_name = params
                    .get("serverName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let tool_name = params
                    .get("toolName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let arguments = params.get("arguments").and_then(|v| v.as_object()).cloned();
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .mcp_client_tx
                    .send(McpClientMessage::CallTool {
                        server_name: server_name.to_string(),
                        tool_name: tool_name.to_string(),
                        arguments,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MCP client actor not available"});
                }
                match rx.await {
                    Ok(Ok(result)) => serde_json::to_value(result)
                        .unwrap_or(serde_json::json!({"error": "serialization error"})),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
                }
            }

            // ── LLM methods ──
            "llm/complete" => {
                tracing::info!("[COORDINATOR] llm/complete called");
                // Extract the prompt (either explicit `prompt` param, or the last user message from `messages`)
                let mut prompt = params
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let prompt_source;
                if prompt.is_empty() {
                    // Fallback: extract the last user message from the messages array
                    prompt_source = "messages fallback";
                    if let Some(messages) = params.get("messages").and_then(|v| v.as_array()) {
                        for msg in messages.iter().rev() {
                            if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                                if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                                    prompt = content.to_string();
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    prompt_source = "explicit prompt param";
                }
                tracing::info!(
                    "[COORDINATOR] PROMPT extracted (source={}): \"{}\"",
                    prompt_source,
                    &prompt.chars().take(200).collect::<String>()
                );

                // Step 0: Route through the IntentRouterActor to determine the handler.
                tracing::info!(
                    "[COORDINATOR] → INTENT_ROUTER: sending RouteQuery (query=\"{}\")",
                    &prompt.chars().take(100).collect::<String>()
                );
                let (intent_tx, intent_rx) = tokio::sync::oneshot::channel();
                if self
                    .intent_router_tx
                    .send(IntentRouterMessage::RouteQuery {
                        query: prompt.to_string(),
                        reply_to: intent_tx,
                    })
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "[COORDINATOR] IntentRouterActor not available, falling through to LLM"
                    );
                } else if let Ok(route_result) = intent_rx.await {
                    tracing::info!("[COORDINATOR] ← INTENT RESULT: {:?}", route_result);
                    match route_result {
                        RouteResult::Build {
                            intent_name,
                            confidence,
                            ref parameters,
                        } => {
                            tracing::info!("[COORDINATOR] INTENT → project/build (intent={}, confidence={}, parameters={:?})", intent_name, confidence, parameters);

                            // Extract scope from the query parameter to pass to the meta build tool
                            let query = parameters.get("query").map(|s| s.as_str()).unwrap_or("");
                            let scope = if query.eq_ignore_ascii_case("build all")
                                || query.eq_ignore_ascii_case("build")
                                || query.is_empty()
                            {
                                None // defaults to "all" in project/build
                            } else {
                                // extract the scope after "build " (None → all)
                                query.strip_prefix("build ").map(|rest| rest.to_string())
                            };

                            let mut build_args = serde_json::Map::new();
                            if let Some(ref s) = scope {
                                build_args.insert(
                                    "scope".to_string(),
                                    serde_json::Value::String(s.clone()),
                                );
                            }
                            if let Some(mode) = parameters.get("mode").map(|s| s.as_str()) {
                                build_args.insert(
                                    "mode".to_string(),
                                    serde_json::Value::String(mode.to_string()),
                                );
                            }

                            tracing::info!("[COORDINATOR] → TOOL_ROUTER: project/build (scope={:?}, args={:?})", scope, build_args);
                            let (build_tx, build_rx) = tokio::sync::oneshot::channel();
                            match self
                                .tool_router_tx
                                .send(ToolRouterMessage::CallTool {
                                    tool_name: "project/build".to_string(),
                                    args: serde_json::Value::Object(build_args),
                                    reply_to: build_tx,
                                })
                                .await
                            {
                                Ok(()) => {
                                    tracing::info!(
                                        "[COORDINATOR] ← TOOL_ROUTER: waiting for build result"
                                    );
                                    return match build_rx.await {
                                        Ok(Ok(result)) => {
                                            // Format build result as a concise text summary instead of raw JSON.
                                            // The detailed build info is already shown via the build-list widget.
                                            let success = result
                                                .get("success")
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            let duration = result
                                                .get("duration_secs")
                                                .and_then(|v| v.as_f64())
                                                .unwrap_or(0.0);
                                            let systems =
                                                result.get("systems").and_then(|v| v.as_array());
                                            let count = systems.map(|a| a.len()).unwrap_or(0);
                                            let summary = if success {
                                                format!("✅ Build completed successfully — {} system(s) in {:.1}s", count, duration)
                                            } else {
                                                "⚠️ Build finished with failures — see build list above for details".to_string()
                                            };
                                            serde_json::json!({"content": summary})
                                        }
                                        Ok(Err(e)) => {
                                            serde_json::json!({"error": format!("Build failed: {}", e)})
                                        }
                                        Err(e) => {
                                            serde_json::json!({"error": format!("Build tool response error: {}", e)})
                                        }
                                    };
                                }
                                Err(e) => {
                                    return serde_json::json!({"error": format!("ToolRouter not available: {}", e)});
                                }
                            }
                        }
                        RouteResult::StateBlocked {
                            intent_name,
                            confidence,
                            ref missing_states,
                        } => {
                            let missing = missing_states.join(", ");
                            tracing::info!("[COORDINATOR] ← INTENT: StateBlocked (intent={}, confidence={}, missing=[{}]) — returning blocked message", intent_name, confidence, missing);
                            return serde_json::json!({
                                "content": format!("⚠️ Cannot run **{}** — required state not ready: **{}**\n\nTry running a project sync first.", intent_name, missing)
                            });
                        }
                        RouteResult::NeedsApproval {
                            intent_name,
                            confidence,
                        } => {
                            tracing::info!("[COORDINATOR] ← INTENT: NeedsApproval (intent={}, confidence={}) — falling to prompt handler", intent_name, confidence);
                        }
                        RouteResult::Plan {
                            intent_name,
                            confidence,
                            ref parameters,
                        } => {
                            tracing::info!("[COORDINATOR] ← INTENT: Plan (intent={}, confidence={}, params={:?}) — dispatching to PlanOrchestrator", intent_name, confidence, parameters);
                            let goal = parameters.get("query").cloned().unwrap_or_else(|| {
                                params
                                    .get("prompt")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string()
                            });
                            // Determine modification scope from parameters, if present:
                            //   scope = "project" → project-level (Level 1)
                            //   scope = "subproject" + scope_path = "<dir>" → subproject-level (Level 2)
                            let scope = parameters
                                .get("scope")
                                .map(|s| s.as_str())
                                .filter(|s| !s.is_empty())
                                .and_then(|s| match s {
                                    "project" => Some(crate::subsystems::planning::plan_orchestrator::ModificationScope::Project),
                                    "subproject" => parameters
                                        .get("scope_path")
                                        .map(|p| crate::subsystems::planning::plan_orchestrator::ModificationScope::Subproject {
                                            path: p.clone(),
                                        }),
                                    _ => None,
                                });
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            if self
                                .plan_orchestrator_tx
                                .send(PlanOrchestratorMessage::CreatePlan {
                                    goal: goal.clone(),
                                    intent_name: Some(intent_name.clone()),
                                    parameters: parameters.clone(),
                                    scope,
                                    workspace_root: None,
                                    reply_to: tx,
                                })
                                .await
                                .is_err()
                            {
                                return serde_json::json!({"error": "PlanOrchestrator not available"});
                            }
                            match rx.await {
                                Ok(Ok(plan)) => {
                                    return serde_json::json!({
                                        "content": format!("📋 **Plan created:** {} — {} steps. Review and approve to begin.", plan.goal, plan.total_steps)
                                    });
                                }
                                Ok(Err(e)) => {
                                    return serde_json::json!({"error": format!("Plan creation failed: {}", e)});
                                }
                                Err(e) => {
                                    return serde_json::json!({"error": format!("PlanOrchestrator response error: {}", e)});
                                }
                            }
                        }
                        RouteResult::Chat => {
                            tracing::info!(
                                "[COORDINATOR] ← INTENT: Chat — proceeding to LLM fall-through"
                            );
                        }
                    }
                }

                // ── Fall through LLM flow ──
                tracing::info!("[COORDINATOR] FALL-THROUGH LLM: gathering chat history and tools");
                let chat_history = {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    if self
                        .chat_tx
                        .send(ChatMessage::GetActive { reply_to: tx })
                        .await
                        .is_err()
                    {
                        tracing::warn!("[COORDINATOR] Chat actor not available for history");
                        None
                    } else {
                        let hist = rx.await.ok().flatten();
                        tracing::info!(
                            "[COORDINATOR] Chat history: {} has {} messages",
                            hist.as_ref().map(|d| d.id.as_str()).unwrap_or("none"),
                            hist.as_ref().map(|d| d.messages.len()).unwrap_or(0)
                        );
                        hist
                    }
                };

                let tools = {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    if self
                        .tools_tx
                        .send(ToolsMessage::ListTools { reply_to: tx })
                        .await
                        .is_err()
                    {
                        tracing::warn!("[COORDINATOR] Tools actor not available");
                        vec![]
                    } else {
                        let t = rx.await.unwrap_or_default();
                        tracing::info!("[COORDINATOR] Tools loaded: {} tools available", t.len());
                        if !t.is_empty() {
                            tracing::info!(
                                "[COORDINATOR] Tool names: {:?}",
                                t.iter().map(|ti| &ti.name).collect::<Vec<_>>()
                            );
                        }
                        t
                    }
                };

                let system_msg = "You are a helpful AI assistant. When you need to use a tool, respond using the native function-calling mechanism (tool_calls) provided by the API — do not describe tool calls in plain text.".to_string();

                let mut messages: Vec<spire_core::subsystems::chat::chat::ChatMessageData> =
                    Vec::new();
                messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                    id: "sys-tools".to_string(),
                    role: "system".to_string(),
                    content: system_msg,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    widget: None,
                });

                if let Some(ref dialog) = chat_history {
                    for msg in &dialog.messages {
                        if msg.role != "system" {
                            messages.push(msg.clone());
                        }
                    }
                }

                let has_user_prompt = messages
                    .last()
                    .map(|m| m.role == "user" && m.content == prompt)
                    .unwrap_or(false);

                if !has_user_prompt {
                    messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                        id: "user-prompt".to_string(),
                        role: "user".to_string(),
                        content: prompt.to_string(),
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        widget: None,
                    });
                }

                tracing::info!("[COORDINATOR] FALL-THROUGH: built messages array with {} msgs (has_user_prompt={}), {} tools",
                    messages.len(), has_user_prompt, tools.len());
                for (i, m) in messages.iter().enumerate() {
                    tracing::info!(
                        "[COORDINATOR]   message[{}]: role={}, id={}, content_len={}",
                        i,
                        m.role,
                        m.id,
                        m.content.len()
                    );
                }

                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(LlmMessage::CompleteWithTools {
                        messages: messages.clone(),
                        tools: tools.clone(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "LLM actor not available"});
                }

                let llm_response = match rx.await {
                    Ok(Ok(content)) => content,
                    Ok(Err(e)) => return serde_json::json!({"error": e.to_string()}),
                    Err(_) => return serde_json::json!({"error": "LLM actor response error"}),
                };

                tracing::info!(
                    "[COORDINATOR] ← LLM first response received (len={} chars)",
                    llm_response.len()
                );
                let final_content = if let Ok(json_msg) =
                    serde_json::from_str::<serde_json::Value>(&llm_response)
                {
                    let has_tc = json_msg
                        .get("tool_calls")
                        .and_then(|t| t.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false);
                    tracing::info!(
                        "[COORDINATOR] LLM response parsed as JSON, has_tool_calls={}",
                        has_tc
                    );
                    if let Some(tool_calls) = json_msg["tool_calls"].as_array() {
                        if !tool_calls.is_empty() {
                            let mut tool_results: Vec<serde_json::Value> = Vec::new();
                            for tc in tool_calls {
                                let function_name =
                                    tc["function"]["name"].as_str().unwrap_or("unknown");
                                let function_args: serde_json::Value = tc["function"]["arguments"]
                                    .as_str()
                                    .and_then(|s| serde_json::from_str(s).ok())
                                    .unwrap_or(serde_json::Value::Null);
                                let tool_call_id = tc["id"].as_str().unwrap_or("call_unknown");
                                tracing::info!(
                                    "Coordinator: executing tool call: {} with args: {:?}",
                                    function_name,
                                    function_args
                                );
                                self.send_tool_event("start", &serde_json::json!({
                                    "tool_name": function_name, "args": function_args, "tool_call_id": tool_call_id, "timestamp": chrono::Utc::now().to_rfc3339(),
                                })).await;
                                let is_vsc_tool = function_name.starts_with("workspace/")
                                    || function_name.starts_with("document/")
                                    || function_name.starts_with("diagnostics/")
                                    || function_name.starts_with("git/")
                                    || function_name.starts_with("symbols/");
                                let is_project_tool = function_name.starts_with("project/");
                                let tool_start = std::time::Instant::now();
                                let tool_result: Result<serde_json::Value, String> = if is_vsc_tool
                                {
                                    self.call_extension_tool(function_name, &function_args)
                                        .await
                                } else if is_project_tool {
                                    let (tx, rx) = tokio::sync::oneshot::channel();
                                    if self
                                        .project_query_tx
                                        .send(ProjectQueryMessage::CallTool {
                                            tool: function_name.to_string(),
                                            args: function_args.clone(),
                                            reply_to: tx,
                                        })
                                        .await
                                        .is_ok()
                                    {
                                        match rx.await {
                                            Ok(result) => Ok(result),
                                            Err(e) => Err(format!(
                                                "ProjectQuery actor response error: {}",
                                                e
                                            )),
                                        }
                                    } else {
                                        Err("ProjectQuery actor not available".to_string())
                                    }
                                } else {
                                    let (tool_tx, tool_rx) = tokio::sync::oneshot::channel();
                                    if self
                                        .mcp_client_tx
                                        .send(McpClientMessage::CallTool {
                                            server_name: String::new(),
                                            tool_name: function_name.to_string(),
                                            arguments: function_args.as_object().cloned(),
                                            reply_to: tool_tx,
                                        })
                                        .await
                                        .is_ok()
                                    {
                                        match tool_rx.await {
                                            Ok(Ok(result)) => Ok(serde_json::to_value(result).unwrap_or(serde_json::json!({"error": "serialization error"}))),
                                            Ok(Err(e)) => Err(e.to_string()),
                                            Err(_) => Err("MCP client response error".to_string()),
                                        }
                                    } else {
                                        Err("MCP client not available".to_string())
                                    }
                                };
                                let tool_duration_ms = tool_start.elapsed().as_millis() as u64;
                                match &tool_result {
                                    Ok(result) => {
                                        self.send_tool_event("result", &serde_json::json!({
                                            "tool_name": function_name, "result": result, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id,
                                        })).await;
                                        tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "result": result}));
                                    }
                                    Err(e) => {
                                        self.send_tool_event("error", &serde_json::json!({
                                            "tool_name": function_name, "error": e, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id,
                                        })).await;
                                        tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "error": e.to_string()}));
                                    }
                                }
                            }
                            let tool_results_text = serde_json::to_string_pretty(&tool_results)
                                .unwrap_or_else(|_| "[]".to_string());
                            messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                                id: "tool-results".to_string(),
                                role: "user".to_string(),
                                content: format!("Tool execution results:\n{}", tool_results_text),
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                widget: None,
                            });
                            let (tx2, rx2) = tokio::sync::oneshot::channel();
                            if self
                                .llm_tx
                                .send(LlmMessage::CompleteWithMessages {
                                    messages,
                                    reply_to: tx2,
                                })
                                .await
                                .is_err()
                            {
                                return serde_json::json!({"error": "LLM actor not available", "tool_results": tool_results});
                            }
                            match rx2.await {
                                Ok(Ok(content)) => content,
                                Ok(Err(e)) => {
                                    return serde_json::json!({"error": e.to_string(), "tool_results": tool_results})
                                }
                                Err(_) => {
                                    return serde_json::json!({"error": "LLM actor response error", "tool_results": tool_results})
                                }
                            }
                        } else {
                            llm_response
                        }
                    } else {
                        json_msg["content"]
                            .as_str()
                            .unwrap_or(&llm_response)
                            .to_string()
                    }
                } else {
                    tracing::info!(
                        "[COORDINATOR] LLM response is NOT valid JSON, checking for XML tool calls"
                    );
                    if let Some(xml_tool_calls) = Self::parse_xml_tool_calls(&llm_response) {
                        tracing::info!(
                            "[COORDINATOR] XML PARSE: detected {} XML-format tool call(s)",
                            xml_tool_calls.len()
                        );
                        let mut tool_results: Vec<serde_json::Value> = Vec::new();
                        for tc in &xml_tool_calls {
                            let function_name =
                                tc["function"]["name"].as_str().unwrap_or("unknown");
                            let function_args: serde_json::Value = tc["function"]["arguments"]
                                .as_str()
                                .and_then(|s| serde_json::from_str(s).ok())
                                .unwrap_or(serde_json::Value::Null);
                            let tool_call_id = tc["id"].as_str().unwrap_or("call_xml_unknown");
                            self.send_tool_event("start", &serde_json::json!({
                                "tool_name": function_name, "args": function_args, "tool_call_id": tool_call_id, "timestamp": chrono::Utc::now().to_rfc3339(),
                            })).await;
                            let is_vsc_tool = function_name.starts_with("workspace/")
                                || function_name.starts_with("document/")
                                || function_name.starts_with("diagnostics/")
                                || function_name.starts_with("git/")
                                || function_name.starts_with("symbols/");
                            let is_project_tool = function_name.starts_with("project/");
                            let tool_start = std::time::Instant::now();
                            let tool_result: Result<serde_json::Value, String> = if is_vsc_tool {
                                self.call_extension_tool(function_name, &function_args)
                                    .await
                            } else if is_project_tool {
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .project_query_tx
                                    .send(ProjectQueryMessage::CallTool {
                                        tool: function_name.to_string(),
                                        args: function_args.clone(),
                                        reply_to: tx,
                                    })
                                    .await
                                    .is_ok()
                                {
                                    match rx.await {
                                        Ok(result) => Ok(result),
                                        Err(e) => {
                                            Err(format!("ProjectQuery actor response error: {}", e))
                                        }
                                    }
                                } else {
                                    Err("ProjectQuery actor not available".to_string())
                                }
                            } else {
                                let (tool_tx, tool_rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::CallTool {
                                        server_name: String::new(),
                                        tool_name: function_name.to_string(),
                                        arguments: function_args.as_object().cloned(),
                                        reply_to: tool_tx,
                                    })
                                    .await
                                    .is_ok()
                                {
                                    match tool_rx.await {
                                        Ok(Ok(result)) => Ok(serde_json::to_value(result)
                                            .unwrap_or(
                                                serde_json::json!({"error": "serialization error"}),
                                            )),
                                        Ok(Err(e)) => Err(e.to_string()),
                                        Err(_) => Err("MCP client response error".to_string()),
                                    }
                                } else {
                                    Err("MCP client not available".to_string())
                                }
                            };
                            let tool_duration_ms = tool_start.elapsed().as_millis() as u64;
                            match &tool_result {
                                Ok(result) => {
                                    self.send_tool_event("result", &serde_json::json!({ "tool_name": function_name, "result": result, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id, })).await;
                                    tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "result": result}));
                                }
                                Err(e) => {
                                    self.send_tool_event("error", &serde_json::json!({ "tool_name": function_name, "error": e, "duration_ms": tool_duration_ms, "tool_call_id": tool_call_id, })).await;
                                    tool_results.push(serde_json::json!({"tool_call_id": tool_call_id, "tool_name": function_name, "error": e.to_string()}));
                                }
                            }
                        }
                        let tool_results_text = serde_json::to_string_pretty(&tool_results)
                            .unwrap_or_else(|_| "[]".to_string());
                        messages.push(spire_core::subsystems::chat::chat::ChatMessageData {
                            id: "tool-results".to_string(),
                            role: "user".to_string(),
                            content: format!("Tool execution results:\n{}", tool_results_text),
                            timestamp: chrono::Utc::now().to_rfc3339(),
                            widget: None,
                        });
                        let (tx2, rx2) = tokio::sync::oneshot::channel();
                        if self
                            .llm_tx
                            .send(LlmMessage::CompleteWithMessages {
                                messages,
                                reply_to: tx2,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "LLM actor not available", "tool_results": tool_results});
                        }
                        match rx2.await {
                            Ok(Ok(content)) => content,
                            Ok(Err(e)) => {
                                return serde_json::json!({"error": e.to_string(), "tool_results": tool_results})
                            }
                            Err(_) => {
                                return serde_json::json!({"error": "LLM actor response error", "tool_results": tool_results})
                            }
                        }
                    } else {
                        llm_response
                    }
                };

                serde_json::json!({"content": final_content})
            }
            "llm/stream" => {
                let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(LlmMessage::Stream {
                        prompt: prompt.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "LLM actor not available"});
                }
                match rx.await {
                    Ok(Ok(mut chunk_rx)) => {
                        let mut full = String::new();
                        while let Some(chunk) = chunk_rx.recv().await {
                            full.push_str(&chunk);
                        }
                        serde_json::json!({"content": full})
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "LLM actor response error"}),
                }
            }
            "llm/updateConfig" => {
                let api_key = params
                    .get("apiKey")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let model = params
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&spire_core::subsystems::llm::llm::LlmConfig::default().model)
                    .to_string();
                let api_url = params
                    .get("apiUrl")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let max_tokens = params
                    .get("maxTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(4096) as u32;
                let coding_max_tokens = params
                    .get("codingMaxTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(
                        spire_core::subsystems::llm::llm::LlmConfig::default().coding_max_tokens
                            as u64,
                    ) as u32;
                let temperature = params
                    .get("temperature")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.7) as f32;
                let strict_mode = params
                    .get("strictMode")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(LlmMessage::UpdateConfig {
                        config: spire_core::subsystems::llm::llm::LlmConfig {
                            api_key,
                            model,
                            api_url,
                            max_tokens,
                            coding_max_tokens,
                            temperature,
                            strict_mode,
                            planning_model: spire_core::subsystems::llm::llm::LlmConfig::default()
                                .planning_model,
                            coding_model: spire_core::subsystems::llm::llm::LlmConfig::default()
                                .coding_model,
                        },
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "LLM actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "LLM actor response error"}),
                }
            }

            // ── Platform methods ──
            "platforms/list" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetPlatforms { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "Memory graph actor not available"});
                }
                match rx.await {
                    Ok(Ok(nodes)) => {
                        // Rebuild the typed Platform view from the generic
                        // registry JSON nodes the knowledge crate returned.
                        let platforms: Vec<crate::platform::Platform> = nodes
                            .iter()
                            .filter_map(crate::actors::platform_codec::platform_json_to_spire)
                            .collect();
                        if !platforms.is_empty() {
                            serde_json::to_value(platforms).unwrap_or(serde_json::json!([]))
                        } else {
                            // The startup phase chain may not have seeded the
                            // graph yet (or a fresh DB cleared it). The YAML
                            // seed is the source of truth for the toolchain —
                            // fall back to reading it directly so the viewer
                            // always shows the registered platforms.
                            let dir = crate::platform::Platform::default_platform_dir();
                            let from_seed =
                                crate::platform::Platform::load_directory(&dir).unwrap_or_default();
                            serde_json::to_value(from_seed).unwrap_or(serde_json::json!([]))
                        }
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => {
                        serde_json::json!({"error": format!("Memory graph response error: {}", e)})
                    }
                }
            }

            // ── System methods ──
            "system/status" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .system_tx
                    .send(SystemMessage::GetStatus { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "System actor not available"});
                }
                match rx.await {
                    Ok(status) => status,
                    Err(_) => serde_json::json!({"error": "System actor response error"}),
                }
            }
            "system/shutdown" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .system_tx
                    .send(SystemMessage::Shutdown { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "System actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "System actor response error"}),
                }
            }
            "system/config/get" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .system_tx
                    .send(SystemMessage::GetConfig {
                        key: key.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "System actor not available"});
                }
                match rx.await {
                    Ok(Some(value)) => serde_json::json!({"value": value}),
                    Ok(None) => serde_json::json!({"value": null}),
                    Err(_) => serde_json::json!({"error": "System actor response error"}),
                }
            }

            // ── Config Storage (via MemoryGraph) ──
            "config/get" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetConfig {
                        key: key.to_string(),
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(Some(value))) => serde_json::json!({"value": value}),
                    Ok(Ok(None)) => serde_json::json!({"value": null}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "config/getAll" => spire_core::config::global_config_json(),
            "config/set" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let value_str = params
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !key.starts_with("deepseek.") && !key.starts_with("tavily.") {
                    return serde_json::json!({"error": "Only deepseek.* and tavily.* keys supported"});
                }
                let new_config =
                    match spire_core::config::set_global_llm_config_key(key, &value_str) {
                        Ok(cfg) => cfg,
                        Err(e) => return serde_json::json!({"error": e}),
                    };
                let (tx_llm, rx_llm) = tokio::sync::oneshot::channel();
                if self
                    .llm_tx
                    .send(crate::actors::LlmMessage::UpdateConfig {
                        config: new_config,
                        reply_to: tx_llm,
                    })
                    .await
                    .is_ok()
                {
                    let _ = rx_llm.await;
                }
                serde_json::json!({"success": true})
            }

            // ── Config Sync (flush WAL) ──
            "config/sync" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::Sync { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"success": true}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }

            // ── MCP Config (stored in MemoryGraph) ──
            "mcp/config/get" => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(servers)) => serde_json::json!({"servers": servers}),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "mcp/config/import" => {
                let servers: Vec<McpServerConfigEntry> = if let Some(config_val) =
                    params.get("config")
                {
                    match serde_json::from_value::<McpConfigFile>(config_val.clone()) {
                        Ok(cfg) => cfg.servers,
                        Err(e) => {
                            return serde_json::json!({"error": format!("Invalid config format: {}", e)});
                        }
                    }
                } else if let Some(config_path) = params.get("path").and_then(|v| v.as_str()) {
                    if config_path.is_empty() {
                        return serde_json::json!({"error": "Missing 'path' parameter"});
                    }
                    let content = match std::fs::read_to_string(config_path) {
                        Ok(c) => c,
                        Err(e) => {
                            return serde_json::json!({"error": format!("Failed to read config file: {}", e)})
                        }
                    };
                    match serde_json::from_str::<McpConfigFile>(&content) {
                        Ok(cfg) => cfg.servers,
                        Err(e) => {
                            return serde_json::json!({"error": format!("Failed to parse config file: {}", e)})
                        }
                    }
                } else {
                    return serde_json::json!({"error": "Missing 'config' or 'path' parameter"});
                };

                let (get_tx, get_rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: get_tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                let existing_servers = match get_rx.await {
                    Ok(Ok(srv)) => srv,
                    Ok(Err(e)) => {
                        return serde_json::json!({"error": format!("Failed to get existing config: {}", e)})
                    }
                    Err(_) => {
                        return serde_json::json!({"error": "MemoryGraph actor response error"})
                    }
                };

                let imported_names: std::collections::HashSet<&str> =
                    servers.iter().map(|s| s.name.as_str()).collect();

                for existing in &existing_servers {
                    if !imported_names.contains(existing.name.as_str()) {
                        tracing::info!(
                            "Coordinator: removing stale MCP server '{}' from import",
                            existing.name
                        );
                        let (del_tx, del_rx) = tokio::sync::oneshot::channel();
                        if self
                            .memory_graph_tx
                            .send(MemoryGraphMessage::SetConfig {
                                key: format!("mcp.server.{}", existing.name),
                                value: serde_json::Value::Null,
                                reply_to: del_tx,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MemoryGraph actor not available"});
                        }
                        if let Err(e) = del_rx.await {
                            tracing::warn!(
                                "Coordinator: failed to delete stale server '{}': {}",
                                existing.name,
                                e
                            );
                        }
                    }
                }

                for server in &servers {
                    let entry_json =
                        serde_json::to_value(server).unwrap_or(serde_json::Value::Null);
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    if self
                        .memory_graph_tx
                        .send(MemoryGraphMessage::SetConfig {
                            key: format!("mcp.server.{}", server.name),
                            value: entry_json,
                            reply_to: tx,
                        })
                        .await
                        .is_err()
                    {
                        return serde_json::json!({"error": "MemoryGraph actor not available"});
                    }
                    if let Err(e) = rx.await {
                        return serde_json::json!({"error": format!("Failed to save server '{}': {}", server.name, e)});
                    }
                }

                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(servers)) => {
                        let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers
                            .into_iter()
                            .filter_map(|entry| {
                                let transport = if let Some(url) = entry.url {
                                    spire_core::mcp::client::TransportConfig::Http {
                                        url, headers: entry.headers.unwrap_or_default(),
                                    }
                                } else if let Some(command) = entry.command {
                                    spire_core::mcp::client::TransportConfig::Stdio {
                                        command, args: entry.args, env: entry.env.unwrap_or_default(),
                                    }
                                } else {
                                    tracing::warn!("Coordinator: MCP server '{}' has no transport config, skipping", entry.name);
                                    return None;
                                };
                                Some(spire_core::mcp::client::McpServerConfig {
                                    name: entry.name, transport, autostart: entry.autostart,
                                build_type: None,
                                })
                            })
                            .collect();

                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .mcp_client_tx
                            .send(McpClientMessage::LoadConfigFromGraph {
                                servers: configs,
                                reply_to: tx,
                            })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "McpClient actor not available"});
                        }
                        let _ = rx.await;

                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .mcp_client_tx
                            .send(McpClientMessage::ConnectAll { reply_to: tx })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "McpClient actor not available"});
                        }
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx).await;

                        serde_json::json!({"success": true})
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "mcp/config/save" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return serde_json::json!({"error": "Missing 'name' parameter"});
                }
                let entry = McpServerConfigEntry {
                    name: name.clone(),
                    command: params
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    args: params
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    env: params.get("env").and_then(|v| v.as_object()).map(|obj| {
                        let mut map = std::collections::HashMap::new();
                        for (k, v) in obj {
                            if let Some(val) = v.as_str() {
                                map.insert(k.clone(), val.to_string());
                            }
                        }
                        map
                    }),
                    url: params
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    headers: params
                        .get("headers")
                        .and_then(|v| v.as_object())
                        .map(|obj| {
                            let mut map = std::collections::HashMap::new();
                            for (k, v) in obj {
                                if let Some(val) = v.as_str() {
                                    map.insert(k.clone(), val.to_string());
                                }
                            }
                            map
                        }),
                    autostart: params
                        .get("autostart")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                };
                let entry_json = serde_json::to_value(entry).unwrap_or(serde_json::Value::Null);
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::SetConfig {
                        key: format!("mcp.server.{}", name),
                        value: entry_json,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .memory_graph_tx
                            .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MemoryGraph actor not available"});
                        }
                        match rx.await {
                            Ok(Ok(servers)) => {
                                let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers.into_iter().filter_map(|entry| {
                                    let transport = if let Some(url) = entry.url { spire_core::mcp::client::TransportConfig::Http { url, headers: entry.headers.unwrap_or_default() } }
                                    else if let Some(command) = entry.command { spire_core::mcp::client::TransportConfig::Stdio { command, args: entry.args, env: entry.env.unwrap_or_default() } }
                                    else { tracing::warn!("Coordinator: MCP server '{}' has no transport config, skipping", entry.name); return None; };
                                    Some(spire_core::mcp::client::McpServerConfig { name: entry.name, transport, autostart: entry.autostart, build_type: None })
                                }).collect();
                                let configs = Self::with_device_mcp_configs(configs);

                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::LoadConfigFromGraph {
                                        servers: configs,
                                        reply_to: tx,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = rx.await;
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::ConnectAll { reply_to: tx })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
                                    .await;
                                serde_json::json!({"success": true})
                            }
                            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                            Err(_) => {
                                serde_json::json!({"error": "MemoryGraph actor response error"})
                            }
                        }
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }
            "mcp/config/delete" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return serde_json::json!({"error": "Missing 'name' parameter"});
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .memory_graph_tx
                    .send(MemoryGraphMessage::SetConfig {
                        key: format!("mcp.server.{}", name),
                        value: serde_json::Value::Null,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "MemoryGraph actor not available"});
                }
                match rx.await {
                    Ok(Ok(())) => {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        if self
                            .memory_graph_tx
                            .send(MemoryGraphMessage::GetMcpConfig { reply_to: tx })
                            .await
                            .is_err()
                        {
                            return serde_json::json!({"error": "MemoryGraph actor not available"});
                        }
                        match rx.await {
                            Ok(Ok(servers)) => {
                                let configs: Vec<spire_core::mcp::client::McpServerConfig> = servers.into_iter().filter_map(|entry| {
                                    let transport = if let Some(url) = entry.url { spire_core::mcp::client::TransportConfig::Http { url, headers: entry.headers.unwrap_or_default() } }
                                    else if let Some(command) = entry.command { spire_core::mcp::client::TransportConfig::Stdio { command, args: entry.args, env: entry.env.unwrap_or_default() } }
                                    else { tracing::warn!("Coordinator: MCP server '{}' has no transport config, skipping", entry.name); return None; };
                                    Some(spire_core::mcp::client::McpServerConfig { name: entry.name, transport, autostart: entry.autostart, build_type: None })
                                }).collect();
                                let configs = Self::with_device_mcp_configs(configs);

                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::LoadConfigFromGraph {
                                        servers: configs,
                                        reply_to: tx,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = rx.await;
                                let (tx, rx) = tokio::sync::oneshot::channel();
                                if self
                                    .mcp_client_tx
                                    .send(McpClientMessage::ConnectAll { reply_to: tx })
                                    .await
                                    .is_err()
                                {
                                    return serde_json::json!({"error": "McpClient actor not available"});
                                }
                                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
                                    .await;
                                serde_json::json!({"success": true})
                            }
                            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                            Err(_) => {
                                serde_json::json!({"error": "MemoryGraph actor response error"})
                            }
                        }
                    }
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(_) => serde_json::json!({"error": "MemoryGraph actor response error"}),
                }
            }

            // ── Ping / Health ──
            "plan/create" => {
                let goal = params
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if goal.is_empty() {
                    return serde_json::json!({"error": "Missing 'goal' parameter"});
                }
                // Project root (from project/open via the shared state). Used so
                // the LLM generates paths under the real project, not the CWD.
                // Prefer an explicit `workspace_root` param; fall back to the
                // FFI-shared project root (the FFI previously injected this).
                let workspace_root = params
                    .get("workspace_root")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        self.ffi_state.as_ref().and_then(|s| {
                            s.project_root
                                .lock()
                                .unwrap()
                                .as_ref()
                                .map(|p| p.to_string_lossy().to_string())
                        })
                    })
                    .unwrap_or_default();
                let scope = params
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("project");
                let scope_val = if scope == "subproject" {
                    params.get("scope_path").and_then(|v| v.as_str()).map(|p| crate::subsystems::planning::plan_orchestrator::ModificationScope::Subproject { path: p.to_string() })
                } else {
                    Some(crate::subsystems::planning::plan_orchestrator::ModificationScope::Project)
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .plan_orchestrator_tx
                    .send(PlanOrchestratorMessage::CreatePlan {
                        goal,
                        intent_name: Some("modification".to_string()),
                        parameters: std::collections::HashMap::new(),
                        scope: scope_val,
                        workspace_root: if workspace_root.is_empty() {
                            None
                        } else {
                            Some(workspace_root)
                        },
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "PlanOrchestrator not available"});
                }
                match rx.await {
                    Ok(Ok(plan)) => serde_json::to_value(plan)
                        .unwrap_or(serde_json::json!({"error": "Serialization error"})),
                    Ok(Err(e)) => {
                        serde_json::json!({"error": format!("Plan creation failed: {}", e)})
                    }
                    Err(e) => {
                        serde_json::json!({"error": format!("PlanOrchestrator response error: {}", e)})
                    }
                }
            }

            "plan/approve" => {
                let plan_id = params
                    .get("plan_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if plan_id.is_empty() {
                    return serde_json::json!({"error": "Missing 'plan_id' parameter"});
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                if self
                    .plan_orchestrator_tx
                    .send(PlanOrchestratorMessage::ApprovePlan {
                        plan_id,
                        reply_to: tx,
                    })
                    .await
                    .is_err()
                {
                    return serde_json::json!({"error": "PlanOrchestrator not available"});
                }
                match rx.await {
                    Ok(Ok(())) => serde_json::json!({"ok": true}),
                    Ok(Err(e)) => {
                        serde_json::json!({"error": format!("Plan approval failed: {}", e)})
                    }
                    Err(e) => {
                        serde_json::json!({"error": format!("PlanOrchestrator response error: {}", e)})
                    }
                }
            }

            "ping" => {
                serde_json::json!({"pong": true})
            }

            "hal/fixPropose" => {
                let root = params.get("root").and_then(|v| v.as_str()).unwrap_or("");
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
                self.propose_hal_fix(root, path).await
            }

            // Compile-error fix proposal for ONE file (driver of the UI's
            // "Fix Errors" review flow). Read-only: writes nothing.
            "build/fixPropose" => {
                let root = params.get("root").and_then(|v| v.as_str()).unwrap_or("");
                let file = params
                    .get("file")
                    .and_then(|v| v.as_str())
                    .or_else(|| params.get("path").and_then(|v| v.as_str()))
                    .unwrap_or("");
                self.propose_compile_fix(root, file).await
            }

            // ── Unknown method ──
            _ => {
                serde_json::json!({"error": format!("Method not found: {}", method)})
            }
        }
    }

    /// Parse XML/Claude-format tool calls from a response content string.
    fn parse_xml_tool_calls(content: &str) -> Option<Vec<serde_json::Value>> {
        if !content.contains("function_calls") {
            return None;
        }

        let invoke_re = Regex::new(
            r#"(?s)<(?:｜DSML｜)?invoke\s+name\s*=\s*"([^"]+)">(.*?)</(?:｜DSML｜)?invoke>"#,
        )
        .ok()?;

        let mut tool_calls = Vec::new();
        let mut call_id_counter = 0u64;

        // Constant pattern — compiled once, outside the per-invoke loop.
        let param_re = Regex::new(
            r#"(?s)<(?:｜DSML｜)?parameter\s+name\s*=\s*"([^"]+)"(?:\s+string\s*=\s*"(true|false)")?\s*>(.*?)</(?:｜DSML｜)?parameter>"#
        ).ok()?;

        for cap in invoke_re.captures_iter(content) {
            let function_name = cap.get(1)?.as_str().to_string();
            let params_body = cap.get(2)?.as_str();

            let mut args = serde_json::Map::new();
            for param_cap in param_re.captures_iter(params_body) {
                let param_name = param_cap.get(1)?.as_str().to_string();
                let param_value = param_cap.get(3)?.as_str().to_string();
                args.insert(param_name, serde_json::json!(param_value));
            }

            call_id_counter += 1;
            tool_calls.push(serde_json::json!({
                "id": format!("call_xml_{}", call_id_counter),
                "type": "function",
                "function": {
                    "name": function_name,
                    "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string()),
                }
            }));
        }

        if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        }
    }
}

// ============================================================================
// FFI-inline RPC handlers — moved from `ffi.rs::process_json_request` so ALL
// method routing lives in this one router. They require the app-only dispatch
// deps attached via `CoordinatorMessage::SetFfiDeps` (registry + shared state).
// ============================================================================

impl CoordinatorActor {
    /// Borrow the app-only dispatch deps (registry + shared state). `None` in
    /// the standalone binary, whose extension flow uses the tools/ methods.
    fn ffi_deps(&self) -> Result<(&Arc<ServiceRegistry>, &Arc<FfiSharedState>), &'static str> {
        match (&self.registry, &self.ffi_state) {
            (Some(r), Some(s)) => Ok((r, s)),
            _ => Err("FFI dispatch deps not attached (standalone binary)"),
        }
    }

    /// Open + analyze a project directory. Sets the shared project root and
    /// analysis, bootstraps per-project graph/MCP/project actors, and returns
    /// the Swift-shaped ProjectInfo.
    async fn handle_project_open(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let root = params
            .get("root")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        let root = match root {
            Some(r) if !r.as_os_str().is_empty() => r,
            _ => return serde_json::json!({"error": "Missing root"}),
        };

        // Guard against opening the user's home directory (or the filesystem
        // root) — scanning them hangs for minutes.
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
        let is_root_dir = root_canon.as_os_str() == std::ffi::OsStr::new("/");
        let is_home = std::env::var("HOME")
            .ok()
            .map(|h| {
                let hp = PathBuf::from(h);
                let hc = hp.canonicalize().unwrap_or(hp);
                root_canon == hc
            })
            .unwrap_or(false);
        if is_root_dir || is_home {
            return serde_json::json!({
                "error": "Please choose a project directory, not your home directory"
            });
        }
        if !root.exists() {
            if let Err(e) = std::fs::create_dir_all(&root) {
                return serde_json::json!({
                    "error": format!("Failed to create project dir {}: {}", root.display(), e)
                });
            }
            tracing::info!("project/open: created {}", root.display());
        }

        // Auto-descend wrapper directories (double-nesting artifact from
        // scaffolding <name> into a folder already named <name>).
        let root = resolve_project_root(&root);
        let data_dir = root.join(".spire").join("data");
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            return serde_json::json!({"error": format!("Failed to create data dir: {}", e)});
        }

        let result: Result<ProjectAnalysis, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<MemoryGraphMessage>("memory_graph")
                .unwrap_or_else(dummy_tx)
                .send(MemoryGraphMessage::Initialize {
                    data_dir: data_dir.clone(),
                    reply_to: t,
                })
                .await;
            if let Err(e) = r.await {
                return Err(format!("MemoryGraph init lost: {}", e));
            }
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<MemoryGraphMessage>("memory_graph")
                .unwrap_or_else(dummy_tx)
                .send(MemoryGraphMessage::InitializeEmbedder {
                    model_path: None,
                    embedder: Some(Arc::new(spire_core::embedder::NoopEmbedder)
                        as Arc<dyn spire_core::models::embedding::Embedder>),
                    reply_to: t,
                })
                .await;
            if let Err(e) = r.await {
                return Err(format!("Embedder init lost: {}", e));
            }

            // ── Bootstrap MCP config into the project graph ──
            // Prefer the project's own config/mcp-config.json; fall back to the
            // bundled global config so fresh/empty projects still get their
            // language MCP servers seeded into the graph.
            let mut config_path = root.join("config").join("mcp-config.json");
            if !config_path.exists() {
                let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .map(|p| p.join("config").join("mcp-config.json"))
                    .unwrap_or_default();
                if bundled.exists() {
                    config_path = bundled;
                }
            }
            if config_path.exists() {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = registry
                    .get::<MemoryGraphMessage>("memory_graph")
                    .unwrap_or_else(dummy_tx)
                    .send(MemoryGraphMessage::BootstrapMcpConfig {
                        config_path: config_path.clone(),
                        reply_to: t,
                    })
                    .await;
                let _ = r.await;
            }
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<MemoryGraphMessage>("memory_graph")
                .unwrap_or_else(dummy_tx)
                .send(MemoryGraphMessage::GetMcpConfig { reply_to: t })
                .await;
            if let Ok(Ok(servers)) = r.await {
                if !servers.is_empty() {
                    use spire_core::mcp::client::{McpServerConfig, TransportConfig};
                    let configs: Vec<McpServerConfig> = servers
                        .into_iter()
                        .filter_map(|entry| {
                            let transport = if let Some(url) = entry.url {
                                TransportConfig::Http {
                                    url,
                                    headers: entry.headers.unwrap_or_default(),
                                }
                            } else {
                                let cmd = entry.command?;
                                TransportConfig::Stdio {
                                    command: cmd,
                                    args: entry.args,
                                    env: entry.env.unwrap_or_default(),
                                }
                            };
                            Some(McpServerConfig {
                                name: entry.name,
                                transport,
                                autostart: entry.autostart,
                                build_type: None,
                            })
                        })
                        .collect();
                    let configs = Self::with_device_mcp_configs(configs);

                    if !configs.is_empty() {
                        let (t, _r) = tokio::sync::oneshot::channel();
                        let _ = self
                            .mcp_client_tx
                            .send(McpClientMessage::LoadConfigFromGraph {
                                servers: configs,
                                reply_to: t,
                            })
                            .await;
                        let (t, r) = tokio::sync::oneshot::channel();
                        let _ = self
                            .mcp_client_tx
                            .send(McpClientMessage::ConnectAll { reply_to: t })
                            .await;
                        // Wait for the servers to actually connect before
                        // continuing — avoids a race where plan generation
                        // starts before MCP tools are available.
                        let _ = r.await;
                    }
                }
            }

            // ── Bootstrap per-project actors ──
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectQueryMessage>("project.query")
                .unwrap_or_else(dummy_tx)
                .send(ProjectQueryMessage::Initialize {
                    memory_graph_tx: registry
                        .get::<MemoryGraphMessage>("memory_graph")
                        .unwrap_or_else(dummy_tx)
                        .clone(),
                    project_root: root.clone(),
                    reply_to: t,
                })
                .await;
            let _ = r.await;
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectSyncMessage>("project.sync")
                .unwrap_or_else(dummy_tx)
                .send(ProjectSyncMessage::Bootstrap {
                    project_root: root.clone(),
                    reply_to: t,
                })
                .await;
            let _ = r.await;
            let _ = registry
                .get::<FileWatcherMessage>("tools.watcher")
                .unwrap_or_else(dummy_tx)
                .send(FileWatcherMessage::StopWatching)
                .await;
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<FileWatcherMessage>("tools.watcher")
                .unwrap_or_else(dummy_tx)
                .send(FileWatcherMessage::StartWatching {
                    root: root.clone(),
                    output: ffi_state.watcher_out_tx.clone(),
                    reply_to: t,
                })
                .await;
            let _ = r.await;
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectAnalyzerMessage>("project.analyzer")
                .unwrap_or_else(dummy_tx)
                .send(ProjectAnalyzerMessage::Analyze {
                    project_root: root.clone(),
                    reply_to: t,
                })
                .await;
            match r.await {
                Ok(Ok(a)) => Ok(a),
                Ok(Err(e)) => Err(format!("Analysis: {}", e)),
                Err(e) => Err(format!("Analysis lost: {}", e)),
            }
        }
        .await;

        match result {
            Ok(analysis) => {
                *ffi_state.analysis.lock().unwrap() = Some(analysis.clone());
                *ffi_state.project_root.lock().unwrap() = Some(root.clone());
                // Keep ProjectBuildActor's root in sync so relative build paths
                // resolve against the newly-opened project.
                let _ = registry
                    .get::<ProjectBuildMessage>("project.build")
                    .unwrap_or_else(dummy_tx)
                    .send(ProjectBuildMessage::SetProjectRoot { root: root.clone() })
                    .await;
                // Populate first-class target nodes so the graph can be queried
                // via project/getBuildTarget (deps/platform/files).
                let _ = populate_target_graph(registry, &analysis.build_systems).await;
                // Boards are deliberately NOT reached here. Only one board tends
                // to be up (the one being developed), and the MCP client's
                // mailbox is serial, so dialing the others would queue-delay the
                // live one for as long as each dead board takes to time out.
                // Connecting is an explicit act — `device/connect` from the
                // Device group, or `ensure_device_connected` on the first
                // `device/test` / `device/deploy`.
                serialize_analysis(&analysis)
            }
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `device/status` — every platform that declares a board, whether Spire is
    /// connected to it right now, and where artifacts deploy to.
    ///
    /// Tokens are never echoed back, only whether one is configured.
    async fn handle_device_status(&self) -> serde_json::Value {
        let online: Vec<String> = {
            let (tx, rx) = tokio::sync::oneshot::channel();
            if self
                .mcp_client_tx
                .send(McpClientMessage::GetServerDetails { reply_to: tx })
                .await
                .is_ok()
            {
                rx.await
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|detail| {
                        detail
                            .properties
                            .get("status")
                            .and_then(|status| status.as_str())
                            == Some("online")
                    })
                    .map(|detail| detail.name)
                    .collect()
            } else {
                Vec::new()
            }
        };

        let devices: Vec<serde_json::Value> = crate::platform::Platform::device_platforms()
            .iter()
            .filter_map(|platform| {
                let device = platform.device.as_ref()?;
                let mcp = device.mcp.as_ref();
                let name = platform.device_server_name()?;
                Some(serde_json::json!({
                    "platform": platform.id,
                    "platform_name": platform.name,
                    "server": name,
                    "url": mcp.map(|mcp| mcp.url.clone()),
                    "upload_endpoint": mcp
                        .and_then(|mcp| crate::device::upload_endpoint(&mcp.url).ok()),
                    "has_token": mcp
                        .and_then(|mcp| mcp.token.as_deref())
                        .map(|token| !token.trim().is_empty())
                        .unwrap_or(false),
                    "deploy_dest": device.deploy.as_ref().map(|deploy| deploy.dest.clone()),
                    "connected": online.iter().any(|online_name| online_name == &name),
                }))
            })
            .collect();

        serde_json::json!({ "devices": devices })
    }

    /// Resolve, connect and upload for a `device/*` request.
    ///
    /// The shared front half of `device/test` and `device/deploy`: both name a
    /// `platform` and a `path` to a cross-built binary, both ship that binary to
    /// the same upload endpoint, and only then do they differ in which tool they
    /// call on the board. Returning the staged facts as one object keeps the two
    /// handlers honest about sharing this — and on failure the returned object is
    /// the error to hand straight back to the caller.
    async fn stage_device_artifact(
        &self,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let platform_id = params
            .get("platform")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if platform_id.is_empty() {
            return Err(serde_json::json!({"error": "Missing platform"}));
        }

        let path_param = params
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if path_param.is_empty() {
            return Err(serde_json::json!({
                "error": "Missing path (the test binary to deploy, relative to the project root)"
            }));
        }

        let Some(platform) = crate::platform::Platform::from_registry(&platform_id) else {
            return Err(serde_json::json!({
                "error": format!("platform '{platform_id}' is not in the registry (~/.spire/platforms)")
            }));
        };
        let Some(mcp) = platform.device.as_ref().and_then(|device| device.mcp.as_ref()) else {
            return Err(serde_json::json!({
                "error": format!("platform '{platform_id}' declares no device.mcp endpoint — see ~/.spire/platforms/{platform_id}.yaml")
            }));
        };

        // Resolve the binary against the project root when there is one, then
        // fall back to the path as given (an absolute path works either way).
        let root = self
            .ffi_deps()
            .ok()
            .and_then(|(_, state)| state.project_root.lock().unwrap().clone());
        let candidate = match root.as_ref() {
            Some(root) => root.join(&path_param),
            None => PathBuf::from(&path_param),
        };
        let binary = if candidate.is_file() {
            candidate
        } else {
            PathBuf::from(&path_param)
        };
        if !binary.is_file() {
            return Err(serde_json::json!({
                "error": format!(
                    "test binary not found: {} — build it for {platform_id} first",
                    binary.display()
                )
            }));
        }

        let bytes = match std::fs::read(&binary) {
            Ok(bytes) => bytes,
            Err(err) => {
                return Err(
                    serde_json::json!({"error": format!("read {}: {err}", binary.display())}),
                )
            }
        };
        let name = params
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|name| !name.trim().is_empty())
            .or_else(|| {
                binary
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| format!("{platform_id}-tests"));

        let server_name = format!("device-{platform_id}");
        if let Err(err) = self.ensure_device_connected(&server_name).await {
            return Err(serde_json::json!({ "error": err }));
        }

        let url = match crate::device::upload_url(&mcp.url, &name) {
            Ok(url) => url,
            Err(err) => return Err(serde_json::json!({ "error": err })),
        };
        if let Err(err) =
            crate::device::upload_binary(&url, mcp.token.as_deref(), &name, &bytes).await
        {
            return Err(serde_json::json!({ "error": err }));
        }

        Ok(serde_json::json!({
            "platform": platform_id,
            "server": server_name,
            "binary": binary.display().to_string(),
            "upload": { "url": url, "name": name, "bytes": bytes.len() },
            // Where this board wants production binaries installed, when the
            // registry says (`device.deploy.dest`) — `device/deploy` uses it.
            "deploy_dest": platform
                .device
                .as_ref()
                .and_then(|device| device.deploy.as_ref())
                .map(|deploy| deploy.dest.clone()),
        }))
    }


    /// `device/test` — deploy a cross-built test binary to a platform's board and
    /// run it there.
    ///
    /// The build itself stays with the build tools (`project/build`, the
    /// meson/cargo modules); this picks the artifact up, pushes it to the board's
    /// upload endpoint, calls the board's `run_test` tool and returns the board's
    /// result verbatim (exit code, streams, duration) so a caller — or the M4 fix
    /// loop — can act on it.
    async fn handle_device_test(&self, params: &serde_json::Value) -> serde_json::Value {
        let staged = match self.stage_device_artifact(params).await {
            Ok(staged) => staged,
            Err(err) => return err,
        };
        let server_name = staged["server"].as_str().unwrap_or_default().to_string();
        let name = staged["upload"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // Ask the board to run it.
        let mut arguments = serde_json::Map::new();
        arguments.insert("path".to_string(), serde_json::json!(name));
        if let Some(args) = params.get("args") {
            arguments.insert("args".to_string(), args.clone());
        }
        if let Some(timeout) = params.get("timeout_secs") {
            arguments.insert("timeout_secs".to_string(), timeout.clone());
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .mcp_client_tx
            .send(McpClientMessage::CallTool {
                server_name: server_name.clone(),
                tool_name: "run_test".to_string(),
                arguments: Some(arguments),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return serde_json::json!({"error": "MCP client actor not available"});
        }

        match rx.await {
            Ok(Ok(result)) => {
                let output = result
                    .content
                    .iter()
                    .filter_map(|content| content.as_text_content().ok())
                    .map(|content| content.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                let passed = result.is_error != Some(true);
                serde_json::json!({
                    "platform": staged["platform"],
                    "server": server_name,
                    "binary": staged["binary"],
                    "upload": staged["upload"],
                    "passed": passed,
                    "result": result.structured_content,
                    "output": output,
                })
            }
            Ok(Err(err)) => {
                serde_json::json!({"error": format!("run_test on '{server_name}' failed: {err}")})
            }
            Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
        }
    }

    /// `device/deploy` — install a cross-built production binary on a board.
    ///
    /// The same front half as `device/test` (resolve → connect → upload), then
    /// the board's `deploy` tool instead of `run_test`: the artifact is copied
    /// to a destination the board keeps, replacing any previous copy. That is
    /// what makes this "deploy" rather than "test" — `run_test` runs an artifact
    /// out of the scratch work directory and reports.
    ///
    /// The destination comes from the request (`dest`), else from the platform's
    /// `device.deploy.dest` in the registry — which is what gives that field a
    /// purpose. Starting and supervising what was deployed is trap control (M5).
    async fn handle_device_deploy(&self, params: &serde_json::Value) -> serde_json::Value {
        let staged = match self.stage_device_artifact(params).await {
            Ok(staged) => staged,
            Err(err) => return err,
        };

        let dest = params
            .get("dest")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|dest| !dest.is_empty())
            .map(str::to_string)
            .or_else(|| {
                staged["deploy_dest"]
                    .as_str()
                    .map(str::trim)
                    .filter(|dest| !dest.is_empty())
                    .map(str::to_string)
            });
        let Some(dest) = dest else {
            return serde_json::json!({
                "error": format!(
                    "no deploy destination: pass \"dest\", or set device.deploy.dest for platform '{}' in ~/.spire/platforms",
                    staged["platform"].as_str().unwrap_or_default()
                )
            });
        };

        let server_name = staged["server"].as_str().unwrap_or_default().to_string();
        let mut arguments = serde_json::Map::new();
        arguments.insert(
            "path".to_string(),
            serde_json::json!(staged["upload"]["name"].as_str().unwrap_or_default()),
        );
        arguments.insert("dest".to_string(), serde_json::json!(dest));
        if let Some(install_as) = params.get("install_as") {
            arguments.insert("name".to_string(), install_as.clone());
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .mcp_client_tx
            .send(McpClientMessage::CallTool {
                server_name: server_name.clone(),
                tool_name: "deploy".to_string(),
                arguments: Some(arguments),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return serde_json::json!({"error": "MCP client actor not available"});
        }

        match rx.await {
            Ok(Ok(result)) => {
                let output = result
                    .content
                    .iter()
                    .filter_map(|content| content.as_text_content().ok())
                    .map(|content| content.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::json!({
                    "platform": staged["platform"],
                    "server": server_name,
                    "binary": staged["binary"],
                    "upload": staged["upload"],
                    "dest": dest,
                    "deployed": result.is_error != Some(true),
                    "result": result.structured_content,
                    "output": output,
                })
            }
            Ok(Err(err)) => {
                serde_json::json!({"error": format!("deploy on '{server_name}' failed: {err}")})
            }
            Err(_) => serde_json::json!({"error": "MCP client actor response error"}),
        }
    }

    /// MCP configs for every platform that declares a device endpoint.
    ///
    /// Registering is cheap and side-effect free: these configs carry
    /// `autostart: false`, so the MCP client's `ConnectAll` skips them and a
    /// powered-off board can never delay startup. Reaching a board is a
    /// deliberate act — `device/connect`, or the first `device/test`.
    fn device_mcp_configs() -> Vec<spire_core::mcp::client::McpServerConfig> {
        crate::platform::Platform::device_platforms()
            .iter()
            .filter_map(|platform| platform.device_mcp_config())
            .collect()
    }

    /// Append the device MCP servers to a config set loaded from the graph.
    ///
    /// `LoadConfigFromGraph` replaces the client's entire config set, so every
    /// path that reloads from the graph has to re-add the device servers —
    /// otherwise the boards silently vanish from the server list.
    fn with_device_mcp_configs(
        mut configs: Vec<spire_core::mcp::client::McpServerConfig>,
    ) -> Vec<spire_core::mcp::client::McpServerConfig> {
        for config in Self::device_mcp_configs() {
            if !configs.iter().any(|existing| existing.name == config.name) {
                configs.push(config);
            }
        }
        configs
    }

    // NOTE: there was a `connect_devices_in_background` here, dialing every
    // board a project builds for on project open. It was removed once connecting
    // became an explicit UI action: with one target under development at a time,
    // the other boards are predictably absent, and the MCP client's serial
    // mailbox meant those timeouts delayed the live board's first request. Use
    // `device/connect` (the Device group's button) or `device/test`, which
    // connects on demand through `ensure_device_connected`.

    /// Make sure a device server is connected before talking to it.
    ///
    /// Connecting is explicit now (`device/connect` from the Device group), so a
    /// board may well be offline here — or the MCP client reloaded its config and
    /// dropped it — hence the check-and-connect on demand.
    async fn ensure_device_connected(&self, server_name: &str) -> Result<(), String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .mcp_client_tx
            .send(McpClientMessage::GetServerDetails { reply_to: tx })
            .await
            .is_err()
        {
            return Err("MCP client actor not available".to_string());
        }

        let online = rx.await.unwrap_or_default().into_iter().any(|detail| {
            detail.name == server_name
                && detail
                    .properties
                    .get("status")
                    .and_then(|status| status.as_str())
                    == Some("online")
        });
        if online {
            return Ok(());
        }

        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .mcp_client_tx
            .send(McpClientMessage::Connect {
                server_name: server_name.to_string(),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            return Err("MCP client actor not available".to_string());
        }

        match rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(format!("board '{server_name}' is not reachable: {err}")),
            Err(_) => Err("MCP client actor response error".to_string()),
        }
    }

    /// Re-run a fresh project analysis (always a disk scan, never cached).
    async fn handle_analyze_project(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let custom_root = params
            .get("root")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);
        if let Some(root) = custom_root {
            // Resolve wrapper folders (same auto-descend as project/open) so a
            // refresh/analyze on the OUTER path never re-points the project root.
            let resolved = resolve_project_root(&root);
            let analysis: Option<ProjectAnalysis> = async {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = registry
                    .get::<ProjectAnalyzerMessage>("project.analyzer")
                    .unwrap_or_else(dummy_tx)
                    .send(ProjectAnalyzerMessage::Analyze {
                        project_root: resolved.clone(),
                        reply_to: t,
                    })
                    .await;
                r.await.ok().and_then(|r| r.ok())
            }
            .await;
            match analysis {
                Some(a) => {
                    // Keep the shared project root in sync so subsequent relative
                    // file reads resolve against the REAL project root.
                    *ffi_state.project_root.lock().unwrap() = Some(resolved.clone());
                    // Re-point ProjectBuildActor (it resolves relative build paths).
                    let _ = registry
                        .get::<ProjectBuildMessage>("project.build")
                        .unwrap_or_else(dummy_tx)
                        .send(ProjectBuildMessage::SetProjectRoot {
                            root: resolved.clone(),
                        })
                        .await;
                    return serialize_analysis(&a);
                }
                None => {
                    return serde_json::json!({
                        "error": format!("Failed to analyze project at {}", resolved.display())
                    });
                }
            }
        }

        // Return real analysis if available, otherwise an actionable error.
        if let Some(ref analysis) = *ffi_state.analysis.lock().unwrap() {
            return serialize_analysis(analysis);
        }
        serde_json::json!({
            "error": "No project opened; call project/open with a root directory"
        })
    }

    /// Fetch the last persisted build status for a directory (+ optional target)
    /// from the graph config key `build.last.<path>[.<target>]`.
    async fn handle_project_build_status(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let status_path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status_target = params
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // `kind` selects which action's status to read: build (default) / lint /
        // test / clean — each is stored under `<kind>.last.<path>[.<target>]`.
        let status_kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "build".to_string());
        let key = if status_target.trim().is_empty() {
            format!("{}.last.{}", status_kind, status_path)
        } else {
            format!("{}.last.{}.{}", status_kind, status_path, status_target)
        };

        let (t, r) = tokio::sync::oneshot::channel();
        let _ = registry
            .get::<MemoryGraphMessage>("memory_graph")
            .unwrap_or_else(dummy_tx)
            .send(MemoryGraphMessage::GetConfig { key, reply_to: t })
            .await;
        match r.await {
            Ok(Ok(Some(value))) => value,
            _ => serde_json::Value::Null,
        }
    }

    /// Query the knowledge graph for Diagnostic nodes and return them as a JSON
    /// array, optionally filtered by subproject directory path.
    async fn handle_project_diagnostics(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let filter_path = params
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_default();

        let (t, r) = tokio::sync::oneshot::channel();
        let _ = registry
            .get::<MemoryGraphMessage>("memory_graph")
            .unwrap_or_else(dummy_tx)
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Diagnostic".to_string()),
                subtype: None,
                name: None,
                limit: Some(2000),
                reply_to: t,
            })
            .await;
        let result: Vec<serde_json::Value> = match r.await {
            Ok(Ok(nodes)) => {
                let mut filtered: Vec<serde_json::Value> = Vec::new();
                let mut all_build: Vec<serde_json::Value> = Vec::new();
                for node in nodes {
                    let message = node
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let file = node
                        .get("file")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let line = node.get("line").and_then(|v| v.as_u64()).map(|n| n as u32);
                    let column = node
                        .get("column")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as u32);
                    let severity = node
                        .get("severity")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let build_type = node
                        .get("build_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let build_run_id = node
                        .get("build_run_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if severity != "warning" && severity != "error" {
                        continue;
                    }
                    if message.trim().is_empty() {
                        continue;
                    }
                    let f = file.clone().unwrap_or_default();
                    let entry = serde_json::json!({
                        "severity": severity,
                        "file": file,
                        "line": line,
                        "column": column,
                        "message": message,
                        "buildType": build_type,
                        "buildRunId": build_run_id,
                    });
                    if build_type == "build" {
                        all_build.push(entry);
                        continue;
                    }
                    if filter_path.is_empty() || f.is_empty() || f.starts_with(&filter_path) {
                        filtered.push(entry);
                    }
                }
                if filtered.is_empty() && !all_build.is_empty() && !filter_path.is_empty() {
                    // Subproject has no diagnostics of its own yet — show the
                    // shared build's warnings so the pane isn't misleading.
                    filtered = all_build;
                }
                filtered
            }
            _ => Vec::new(),
        };
        serde_json::to_value(result).unwrap_or(serde_json::Value::Null)
    }

    /// Answer `project/getBuildTarget` (bare method or the `tools/call`
    /// envelope from Swift) directly from the in-memory analysis — the
    /// authoritative BuildManager result — so target-scoped data is always
    /// available the moment analysis completes.
    async fn handle_project_get_build_target(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (_registry, ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let (target_name, _is_envelope) = if method == "tools/call" {
            let name = params
                .get("args")
                .and_then(|a| a.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (name, true)
        } else {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (name, false)
        };

        let Some(analysis) = ffi_state.analysis.lock().unwrap().as_ref().cloned() else {
            return serde_json::json!({"error": "No project analyzed yet"});
        };

        // Find the BuildMetadata whose targets contain the requested name.
        let Some(meta) = analysis
            .build_systems
            .iter()
            .find(|bs| bs.targets.iter().any(|t| t.name == target_name))
        else {
            return serde_json::json!({
                "error": format!("Build target not found: {}", target_name)
            });
        };

        // Use the analyzer's parsed source files for this target (the exact set
        // compiled for it) instead of guessing directories.
        let target = meta.targets.iter().find(|t| t.name == target_name);
        let declared_sources: Vec<String> =
            target.map(|t| t.source_files.to_vec()).unwrap_or_default();

        let mut files: Vec<serde_json::Value> = Vec::new();
        if !declared_sources.is_empty() {
            let scope_dir = target_name
                .strip_prefix("ai-trap-")
                .map(|s| s.to_string())
                .unwrap_or_default();
            for s in &declared_sources {
                let path = if s.starts_with("src/") || s.starts_with("include/") {
                    format!("toolkit/{s}")
                } else if !scope_dir.is_empty() {
                    format!("{scope_dir}/{s}")
                } else {
                    s.clone()
                };
                files.push(serde_json::json!({
                    "path": path,
                    "role": "source",
                    "language": "C++",
                }));
            }
        } else {
            // Fallback when source_files weren't parsed — scope to the platform dir.
            let scope_dir = target_name
                .strip_prefix("ai-trap-")
                .map(|s| s.to_string())
                .unwrap_or_default();
            if scope_dir.is_empty() {
                crate::ffi::collect_tree_files(&analysis.file_tree, &mut files);
            } else if let Some(dir_node) =
                crate::ffi::find_tree_dir(&analysis.file_tree, &scope_dir)
            {
                crate::ffi::collect_tree_files(dir_node, &mut files);
            }
        }

        // Dependencies are STRICTLY per-target — the selected target's own list,
        // parsed from its meson.build section. There is deliberately NO fallback
        // to the flat metadata-level list: that list is the union of every
        // platform's `dependency()` calls, so falling back made one platform's
        // pane show all platforms' libraries.
        let deps: Vec<serde_json::Value> = target
            .map(|t| t.dependencies.as_slice())
            .unwrap_or(&[])
            .iter()
            .map(|d| {
                serde_json::json!({
                    "name": d.name,
                    "version": d.version_req,
                })
            })
            .collect();

        serde_json::json!({
            "name": target_name,
            "kind": meta.targets.iter().find(|t| t.name == target_name)
                .and_then(|t| t.kind.first()).cloned().unwrap_or_default(),
            "configFile": meta.config_files.first().cloned().unwrap_or_default(),
            // The SELECTED target's own platform — not every platform in the
            // project (which is what made the pane list all of them).
            "platform": target.map(|t| vec![t.platform.clone()]).unwrap_or_default(),
            "dependencies": deps,
            "files": files,
        })
    }

    /// `createProject/Plan` — single-round-trip describe-the-project flow:
    /// compute the in-memory structural contract (no disk writes) + LLM plan,
    /// returning both `{plan, spec}` for UI approval.
    /// Read the wizard's `structure`/`embedded` params (shared by all
    /// createProject/* handlers). `structure` defaults to None (→ Native).
    fn params_structure_embedded(
        params: &serde_json::Value,
    ) -> (Option<spire_core::build_types::ProjectStructure>, bool) {
        let structure = params
            .get("structure")
            .and_then(|v| v.as_str())
            .map(spire_core::build_types::ProjectStructure::from_str);
        let embedded = params
            .get("embedded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        (structure, embedded)
    }

    async fn handle_create_project_plan(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("Rust")
            .to_string();
        let platforms: Vec<String> = params
            .get("platforms")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let (structure, embedded) = Self::params_structure_embedded(params);

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::PlanScaffold {
                    goal,
                    root_dir: PathBuf::from(root_dir),
                    project_name,
                    language,
                    platforms,
                    structure,
                    embedded,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(res)) => serde_json::json!({
                "plan": res.plan,
                "spec": res.spec,
            }),
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/GeneratePlan` — LLM-decomposed plan for a new project.
    async fn handle_create_project_generate_plan(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("Rust")
            .to_string();
        let platforms: Vec<String> = params
            .get("platforms")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let (structure, embedded) = Self::params_structure_embedded(params);

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::GeneratePlan {
                    goal,
                    root_dir: PathBuf::from(root_dir),
                    language,
                    platforms,
                    structure,
                    embedded,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(plan)) => {
                serde_json::to_value(plan).unwrap_or(serde_json::json!({"error": "serialize"}))
            }
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/Scaffold` — materialize the structural scaffold offline.
    async fn handle_create_project_scaffold(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or("Rust")
            .to_string();
        let platforms: Vec<String> = params
            .get("platforms")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| p.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let (structure, embedded) = Self::params_structure_embedded(params);

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::ScaffoldProject {
                    project_name,
                    root_dir: PathBuf::from(root_dir),
                    language,
                    platforms,
                    structure,
                    embedded,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(spec)) => {
                serde_json::to_value(spec).unwrap_or(serde_json::json!({"error": "serialize"}))
            }
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/Fill` — constrained LLM fill of a materialized scaffold.
    async fn handle_create_project_fill(&self, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let spec: crate::subsystems::build::build_manager::ScaffoldSpec = params
            .get("spec")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::FillProject {
                    goal,
                    root_dir: PathBuf::from(root_dir),
                    spec,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(plan)) => {
                serde_json::to_value(plan).unwrap_or(serde_json::json!({"error": "serialize"}))
            }
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/GenerateSpec` — SpireApp requirements pass: derive a
    /// VALIDATED AppSpec JSON contract from the goal (self-healed against
    /// `spec::validate`). Writes nothing to disk; the spec drives the later
    /// fill/codegen phase.
    async fn handle_create_project_generate_spec(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::GenerateAppSpec {
                    project_name,
                    goal,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {e}"))
        }
        .await;
        match result {
            Ok(Ok(spec)) => {
                serde_json::to_value(spec).unwrap_or(serde_json::json!({"error": "serialize"}))
            }
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/GenerateCode` — deterministic skeleton steps from a
    /// VALIDATED AppSpec (types/actors/FFI dispatch + Swift wrappers/screens,
    /// bridge-derived routing). Returns the `write_source_file` steps; the
    /// caller executes them via `createProject/ExecutePlan`.
    async fn handle_create_project_generate_code(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let project_name = params
            .get("projectName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let spec: crate::subsystems::project::spec::AppSpec = match params
            .get("spec")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
        {
            Some(s) => s,
            None => return serde_json::json!({"error": "missing or invalid 'spec'"}),
        };

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::GenerateCode {
                    project_name,
                    spec,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {e}"))
        }
        .await;
        match result {
            Ok(Ok(steps)) => {
                serde_json::to_value(steps).unwrap_or(serde_json::json!({"error": "serialize"}))
            }
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/ExecutePlan` — execute the entire plan sequentially.
    async fn handle_create_project_execute_plan(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let steps: Vec<crate::subsystems::project::project_creation::CreationStep> = params
            .get("steps")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::ExecutePlan {
                    root_dir: PathBuf::from(root_dir),
                    steps,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(results)) => {
                serde_json::to_value(results).unwrap_or(serde_json::json!({"error": "serialize"}))
            }
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `createProject/ExecuteStep` — execute a single creation step.
    async fn handle_create_project_execute_step(
        &self,
        params: &serde_json::Value,
    ) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let root_dir = params
            .get("rootDir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let step: crate::subsystems::project::project_creation::CreationStep = params
            .get("step")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(crate::subsystems::project::project_creation::CreationStep {
                id: String::new(),
                step_type: crate::subsystems::project::project_creation::CreationStepType::Build,
                description: String::new(),
                status: crate::subsystems::project::project_creation::StepStatus::Pending,
                parameters: serde_json::json!({}),
                result: None,
            });

        let result: Result<_, String> = async {
            let (t, r) = tokio::sync::oneshot::channel();
            let _ = registry
                .get::<ProjectCreationMessage>("project_creation")
                .unwrap_or_else(dummy_tx)
                .send(ProjectCreationMessage::ExecuteStep {
                    root_dir: PathBuf::from(root_dir),
                    step,
                    reply_to: t,
                })
                .await;
            r.await.map_err(|e| format!("lost: {}", e))
        }
        .await;
        match result {
            Ok(Ok(res)) => {
                serde_json::to_value(res).unwrap_or(serde_json::json!({"error": "serialize"}))
            }
            Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
            Err(e) => serde_json::json!({"error": e}),
        }
    }

    /// `rag/*` RPCs — semantic search, interface lookup, domain/source listing,
    /// ingest. Routed to the RAG actor (which owns the default domain).
    async fn handle_rag(&self, method: &str, params: &serde_json::Value) -> serde_json::Value {
        let (registry, _ffi_state) = match self.ffi_deps() {
            Ok(d) => d,
            Err(e) => return serde_json::json!({"error": e}),
        };

        let rag_tx = match registry.get::<RagMessage>("rag") {
            Some(tx) => tx.clone(),
            None => return serde_json::json!({"error": "RAG subsystem not available"}),
        };

        match method {
            "rag/search" => {
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let top_k = params.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx
                    .send(RagMessage::Query {
                        domain,
                        query,
                        top_k,
                        reply_to: t,
                    })
                    .await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/find-interfaces" => {
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let top_k = params.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx
                    .send(RagMessage::FindInterfaces {
                        domain,
                        query,
                        top_k,
                        reply_to: t,
                    })
                    .await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/list-domains" => {
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx.send(RagMessage::ListDomains { reply_to: t }).await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/install-bundle-manifests" => {
                // Ship the application-wisdom corpora (spire-actor + spire-core
                // docs) into the KnowledgeStore's manifest scan dirs so RagView
                // discovers them and its existing Ingest buttons build the
                // corpus. Portable: sources fetch from GitHub (cached).
                let dir = spire_core::config::knowledge_dir();
                let bundles: [(&str, &str); 2] = [
                    (
                        "spire-core",
                        include_str!("../../resources/rag-ingest/spire-core.ingest.yaml"),
                    ),
                    (
                        "spire-actor",
                        include_str!("../../resources/rag-ingest/spire-actor.ingest.yaml"),
                    ),
                ];
                let mut installed: Vec<String> = Vec::new();
                for (name, content) in bundles {
                    let target = dir.join(name).join("ingest.yaml");
                    if let Some(parent) = target.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            return serde_json::json!({ "error": format!("create dir: {e}") });
                        }
                    }
                    if let Err(e) = std::fs::write(&target, content) {
                        return serde_json::json!({"error": format!("write {name}: {e}") });
                    }
                    installed.push(name.to_string());
                }
                serde_json::json!({ "success": true, "installed": installed })
            }
            "rag/list-manifests" => {
                let project_root = params
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default();
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx
                    .send(RagMessage::ListManifests {
                        project_root,
                        reply_to: t,
                    })
                    .await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/set-domain" => {
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let domain = if domain.is_empty() {
                    None
                } else {
                    Some(domain)
                };
                // The RagActor owns the default domain (mailbox-serialized).
                let _ = rag_tx.send(RagMessage::SetDefaultDomain { domain }).await;
                serde_json::json!({"ok": true})
            }
            "rag/list-sources" => {
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx
                    .send(RagMessage::ListSources {
                        domain,
                        reply_to: t,
                    })
                    .await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/ingest-graph-config" => {
                let manifest_path = params
                    .get("manifest_path")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default();
                let project_root = params
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .filter(|p| !p.as_os_str().is_empty());
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx
                    .send(RagMessage::IngestGraphConfig {
                        manifest_path,
                        project_root,
                        reply_to: t,
                    })
                    .await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            "rag/reingest-graph-config" => {
                let manifest_path = params
                    .get("manifest_path")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default();
                let project_root = params
                    .get("project_root")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .filter(|p| !p.as_os_str().is_empty());
                let (t, r) = tokio::sync::oneshot::channel();
                let _ = rag_tx
                    .send(RagMessage::ReingestGraphConfig {
                        manifest_path,
                        project_root,
                        reply_to: t,
                    })
                    .await;
                match r.await {
                    Ok(Ok(v)) => serde_json::to_value(v).unwrap_or_default(),
                    Ok(Err(e)) => serde_json::json!({"error": e.to_string()}),
                    Err(e) => serde_json::json!({"error": format!("lost: {}", e)}),
                }
            }
            _ => serde_json::json!({"error": "unknown rag method"}),
        }
    }
}

impl CoordinatorActor {
    // ── spec-design RPC: per-project free-form design sessions ────────────
    // One SpecDesignActor per project (so several apps can be designed in
    // parallel); each is created lazily on first use and persists its
    // brainstorm to <config>/design/<project>.json.

    /// Lazily create / look up the design-session sender for a project.
    async fn spec_design_tx(
        &self,
        project_name: &str,
    ) -> Result<tokio::sync::mpsc::Sender<SpecDesignMessage>, String> {
        let map = self.spec_design_sessions.clone();
        if let Some(tx) = map
            .lock()
            .map(|g| g.get(project_name).cloned())
            .unwrap_or(None)
        {
            return Ok(tx);
        }

        let llm: crate::subsystems::project::spec_design::LlmCall = {
            let llm_tx = self.llm_tx.clone();
            Box::new(move |prompt: String| {
                let llm_tx = llm_tx.clone();
                Box::pin(async move {
                    let (t, r) = tokio::sync::oneshot::channel();
                    if llm_tx
                        .send(crate::actors::LlmMessage::Complete {
                            prompt,
                            // Free-form conversion — prose output, NOT structured
                            // JSON. The Planning role forces json_object mode, which
                            // DeepSeek rejects (400) for non-JSON prompts.
                            role: spire_core::subsystems::llm::llm::LlmModelRole::Freeform,
                            reply_to: t,
                        })
                        .await
                        .is_err()
                    {
                        return Err("LLM actor unavailable".to_string());
                    }
                    match r.await {
                        Ok(Ok(text)) => Ok(text),
                        Ok(Err(e)) => Err(format!("LLM error: {e}")),
                        Err(e) => Err(format!("LLM reply lost: {e}")),
                    }
                })
            })
        };

        let mut actor = crate::subsystems::project::spec_design::SpecDesignActor::new(llm);
        actor.set_memory_graph(self.memory_graph_tx.clone());
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let _handle = spire_core::actors::Actor::spawn(actor, rx);
        if let Ok(mut g) = map.lock() {
            g.insert(project_name.to_string(), tx.clone());
        }
        Ok(tx)
    }

    fn spec_design_project(&self, params: &serde_json::Value) -> Result<String, serde_json::Value> {
        params
            .get("projectName")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| serde_json::json!({ "error": "missing 'projectName'" }))
    }

    async fn handle_spec_design_start(&self, params: &serde_json::Value) -> serde_json::Value {
        let project_name = match self.spec_design_project(params) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let reset = params
            .get("reset")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tx = match self.spec_design_tx(&project_name).await {
            Ok(tx) => tx,
            Err(e) => return serde_json::json!({ "error": e }),
        };
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = tx
            .send(SpecDesignMessage::Start {
                project_name,
                goal,
                reset,
                reply_to: t,
            })
            .await;
        match r.await {
            Ok(Ok(state)) => serde_json::to_value(state)
                .unwrap_or_else(|_| serde_json::json!({ "error": "serialize state" })),
            Ok(Err(e)) => serde_json::json!({ "error": e }),
            Err(e) => serde_json::json!({ "error": format!("lost: {e}") }),
        }
    }

    async fn handle_spec_design_convert(&self, params: &serde_json::Value) -> serde_json::Value {
        let project_name = match self.spec_design_project(params) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let spec_text = params
            .get("spec_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tx = match self.spec_design_tx(&project_name).await {
            Ok(tx) => tx,
            Err(e) => return serde_json::json!({ "error": e }),
        };
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = tx
            .send(SpecDesignMessage::Convert {
                spec_text,
                reply_to: t,
            })
            .await;
        match r.await {
            Ok(Ok(outcome)) => serde_json::to_value(outcome)
                .unwrap_or_else(|_| serde_json::json!({ "error": "serialize outcome" })),
            Ok(Err(e)) => serde_json::json!({ "error": e }),
            Err(e) => serde_json::json!({ "error": format!("lost: {e}") }),
        }
    }

    async fn handle_spec_design_accept(&self, params: &serde_json::Value) -> serde_json::Value {
        let project_name = match self.spec_design_project(params) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let tx = match self.spec_design_tx(&project_name).await {
            Ok(tx) => tx,
            Err(e) => return serde_json::json!({ "error": e }),
        };
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = tx.send(SpecDesignMessage::Accept { reply_to: t }).await;
        match r.await {
            Ok(Ok(spec)) => serde_json::to_value(spec)
                .unwrap_or_else(|_| serde_json::json!({ "error": "serialize spec" })),
            Ok(Err(e)) => serde_json::json!({ "error": e }),
            Err(e) => serde_json::json!({ "error": format!("lost: {e}") }),
        }
    }

    async fn handle_spec_design_reopen(&self, params: &serde_json::Value) -> serde_json::Value {
        let project_name = match self.spec_design_project(params) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let tx = match self.spec_design_tx(&project_name).await {
            Ok(tx) => tx,
            Err(e) => return serde_json::json!({ "error": e }),
        };
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = tx.send(SpecDesignMessage::Reopen { reply_to: t }).await;
        match r.await {
            Ok(Ok(state)) => serde_json::to_value(state)
                .unwrap_or_else(|_| serde_json::json!({ "error": "serialize state" })),
            Ok(Err(e)) => serde_json::json!({ "error": e }),
            Err(e) => serde_json::json!({ "error": format!("lost: {e}") }),
        }
    }

    async fn handle_spec_design_state(&self, params: &serde_json::Value) -> serde_json::Value {
        let project_name = match self.spec_design_project(params) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let tx = match self.spec_design_tx(&project_name).await {
            Ok(tx) => tx,
            Err(e) => return serde_json::json!({ "error": e }),
        };
        let (t, r) = tokio::sync::oneshot::channel();
        let _ = tx.send(SpecDesignMessage::GetState { reply_to: t }).await;
        match r.await {
            Ok(state) => serde_json::to_value(state)
                .unwrap_or_else(|_| serde_json::json!({ "error": "serialize state" })),
            Err(e) => serde_json::json!({ "error": format!("lost: {e}") }),
        }
    }
}
