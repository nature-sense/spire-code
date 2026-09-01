// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! BuildManagerActor — neutral entry point to the build system.
//!
//! Maintains a router (config filename → module sender) that it builds by
//! querying each static build module's `DescribeCapabilities`. It is the
//! single point of access for all build/analysis actions:
//!
//! - `AnalyzeProject` routes to a module, produces `BuildMetadata`, and
//!   persists it in the knowledge graph (the actor's state).
//! - `BuildProject`/`TestProject` fetch the stored analysis from the graph
//!   and pass it to the module in the request message (modules stay stateless).
//! - `GetAnalysis` reads the graph without touching any module.

use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};

use crate::build::{
    AstParseResult, BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability, ParseSummary,
    TestOptions,
};
use spire_core::subsystems::llm::llm::{LlmMessage, LlmModelRole};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::Actor;
use spire_core::models::memory_graph::{
    AttrNode, NodeUpdate, RelationshipInput, RelationshipType, StreamOp,
    StreamOpResult, TransactionRequest,
};

/// Build an envelope `AttrNode` for a node stored through a transaction stream
/// (diagnostics, source files, and AST nodes are all open-model node kinds).
fn bm_attr_unknown(
    node_type: &str,
    subtype: Option<String>,
    name: String,
    description: Option<String>,
    properties: HashMap<String, serde_json::Value>,
) -> AttrNode {
    let now = chrono::Utc::now();
    AttrNode {
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

use spire_core::build_types::BuildMetadata;
use uuid::Uuid;

/// A materialized scaffold's structural contract (returned by
/// `ProjectCreationMessage::ScaffoldProject`). It tells the fill phase (LLM)
/// exactly which paths are locked (structural), which source roots it may
/// write files + create subdirs under, which build-config dependency sections
/// may be edited ONLY through `declare_dependencies`, and the target platforms.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ScaffoldSpec {
    /// Paths (relative to project root) that are structural/locked — the LLM
    /// may NOT write, rename, or delete these.
    pub structural_files: Vec<String>,
    /// Source roots the LLM may add/modify files under (and create
    /// subdirectories beneath them).
    pub fill_roots: Vec<String>,
    /// Build-config dependency sections editable ONLY via the module's
    /// `declare_dependencies` tool.
    pub dependency_sections: Vec<String>,
    /// Registry ids the scaffold targets.
    pub platform_targets: Vec<String>,
    /// The module's build-system label (e.g. "Cargo", "Meson") — used to route
    /// `declare_dependencies` via `CallModuleTool`.
    pub build_system: String,
    /// The emitted scaffold files (build configs + source stubs) for the UI's
    /// structure preview. Ignored by the fill-phase guard (the guard uses
    /// `structural_files`/`fill_roots`).
    #[serde(default)]
    pub files: Vec<crate::build::ScaffoldFile>,
    /// Structural shape the scaffold was emitted for (Hal / SingleSource /
    /// Native). The UI uses it to label the HAL contract and per-platform
    /// implementations; the fill guard uses `fill_roots` regardless.
    #[serde(default, skip_serializing_if = "is_native_structure")]
    pub structure: spire_core::build_types::ProjectStructure,
    /// True when the project is embedded (cross-compiled targets only — never
    /// a host build target). Wiring: the wizard sets this for embedded projects.
    #[serde(default)]
    pub embedded: bool,
}

fn is_native_structure(s: &spire_core::build_types::ProjectStructure) -> bool {
    *s == spire_core::build_types::ProjectStructure::Native
}

/// Everything the semantic Stage-1 generation derives from the contract +
/// registry platform: the validated summary, the concrete impl class name, the
/// clean deterministic declaration header, the module-pair file names and the
/// final implementation prompt.
struct HalImplContext {
    summary: String,
    class_name: String,
    impl_header: String,
    prompt: String,
    impl_dir: PathBuf,
    cpp_name: String,
    hpp_name: String,
}

/// Resolve the contract header + the registry platform record and build the
/// SEMANTIC module-pair implementation prompt (`generate_hal_impl_prompt_pair`)
/// for one interface × platform. Shared by `hal_build_impl_prompt` (prompt
/// preview) and `hal_generate_impl` (LLM run) so both steps always agree on
/// the prompt, concrete class name and module-pair file names.
fn hal_impl_generation_context(
    root: &str,
    interface: &str,
    platform: &str,
    library_hints: Option<&str>,
) -> Result<HalImplContext, String> {
    use crate::build::generic_helpers::{
        datatype_docs_to_prompt_text, extract_cpp_base_classes, extract_contract_methods_cpp,
        generate_hal_impl_prompt_pair, generate_hal_module_header_clean, hal_docs_to_prompt_text,
        hal_platform_library_hints, parse_hal_docs, resolve_semantic_hal_impl_names, summarize_hal_header,
    };
    // Contract (binding gate) — an invalid header never reaches the LLM.
    let header = std::path::Path::new(root)
        .join("hal")
        .join("api")
        .join(format!("{interface}.hpp"));
    let content = std::fs::read_to_string(&header)
        .map_err(|e| format!("read {}: {e}", header.display()))?;
    let summary = summarize_hal_header(&content)
        .map_err(|e| format!("contract {} invalid: {e}", header.display()))?;
    let classes = extract_contract_methods_cpp(&content);
    let methods: Vec<_> = classes.iter().flat_map(|(_, ms)| ms.clone()).collect();
    if methods.is_empty() {
        return Err(format!("contract {} has no pure-virtual methods", header.display()));
    }
    // Concrete base: prefer the HalModule-derived class, else the first
    // abstract class (simplified contracts without the hal::HalModule base).
    let base_class = extract_cpp_base_classes(&content)
        .into_iter()
        .find(|(_, bases)| bases.iter().any(|b| b == "HalModule"))
        .map(|(name, _)| name)
        .or_else(|| classes.first().map(|(name, _)| name.clone()))
        .unwrap_or_else(|| interface.to_string());
    // Platform registry record → hardware profile.
    let plat = spire_core::build_types::Platform::from_registry(platform)
        .ok_or_else(|| format!("platform '{platform}' not in registry (~/.spire/platforms)"))?;
    // Module-pair naming: semantic mode REUSES `<iface>_<plat>` when the name
    // is free or only holds a scaffold stub (so Fix replaces stubs instead of
    // piling up `_2` siblings); real implementations are never overwritten.
    let impl_dir = std::path::Path::new(root)
        .join("hal")
        .join("implementations")
        .join(platform);
    let (class_name, cpp_name, hpp_name) = resolve_semantic_hal_impl_names(interface, platform, &impl_dir);
    // Clean deterministic declaration header (no SPIRE-HAL-STUB sentinel).
    let impl_header =
        generate_hal_module_header_clean(interface, &class_name, &base_class, &methods, platform);
    let hints = library_hints
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| hal_platform_library_hints(platform));
    let hardware_profile = format!(
        "id: {}\nname: {}\nos: {}\ncpu_family: {}\ncpu: {}\ntarget_triple: {}\ncross compiler: {}\nsysroot: {}",
        plat.id, plat.name, plat.os,
        plat.architecture.cpu_family, plat.architecture.cpu, plat.architecture.target_triple,
        plat.toolchain.c, plat.sysroot.root,
    );
    // Structured docs (deterministic, from the repo itself).
    let contract_docs = hal_docs_to_prompt_text(&parse_hal_docs(&content));
    let datatype_docs = datatype_docs_to_prompt_text(std::path::Path::new(root));
    let prompt = generate_hal_impl_prompt_pair(
        &summary, interface, &class_name, &base_class,
        &plat.id, &plat.name, &hardware_profile, &hints,
        &format!("{interface}-{platform}"), &impl_header,
        &contract_docs, &datatype_docs, "",
    );
    Ok(HalImplContext {
        summary,
        class_name,
        impl_header,
        prompt,
        impl_dir,
        cpp_name,
        hpp_name,
    })
}

/// Run the LLM (role Coding) against the module-pair prompt with one
/// fence-strip + structural syntax-check retry. Returns `(cleaned source,
/// syntax verdict)` — the verdict is "ok" when the source parsed cleanly.
async fn hal_llm_generate_source(
    llm_tx: &Option<mpsc::Sender<LlmMessage>>,
    prompt: String,
) -> Result<(String, String), String> {
    let mut prompt = prompt;
    let mut src = String::new();
    let mut syntax = String::new();
    for attempt in 0..3u32 {
        let (t, r) = oneshot::channel();
        let _ = llm_tx
            .as_ref()
            .expect("llm_tx is checked by callers before invoking")
            .send(LlmMessage::Complete {
                prompt: prompt.clone(),
                role: LlmModelRole::Coding,
                reply_to: t,
            })
            .await;
        let text = match r.await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                let msg = e.to_string();
                // Truncation: the model ran out of output tokens. Retry ONCE
                // with a concise-code instruction instead of failing outright.
                if msg.contains("truncated") && attempt == 0 {
                    prompt.push_str(
                        "\n\nYour previous response was truncated. Write a MORE CONCISE implementation: minimal comments, no explanations, compact code — stay within the output token limit.",
                    );
                    continue;
                }
                return Err(format!("LLM failed: {msg}"));
            }
            Err(_) => return Err("LLM reply lost".to_string()),
        };
        src = crate::build::generic_helpers::strip_code_fences(&text);
        let check = crate::build::generic_helpers::cpp_syntax_check(&src);
        if check.ok {
            break;
        }
        let hints: Vec<String> = check
            .errors
            .iter()
            .map(|e| format!("line {} col {}: {}", e.line, e.col, e.kind))
            .collect();
        if attempt < 2 {
            prompt.push_str(&format!(
                "\n\nYour previous attempt had C++ syntax errors:\n{}\nFix them and return the complete corrected .cpp again (no fences).",
                hints.join("; ")
            ));
        } else {
            syntax = format!("syntax check failed after retry: {}", hints.join("; "));
        }
    }
    Ok((src, syntax))
}

/// Messages for the BuildManager actor.
pub enum BuildManagerMessage {
    /// Register a module's router entries from its capability.
    AddModule {
        capability: ModuleCapability,
        module_tx: mpsc::Sender<BuildModuleMessage>,
    },
    /// Analyze a project → produce + store BuildMetadata.
    AnalyzeProject {
        path: PathBuf,
        reply_to: oneshot::Sender<Result<BuildMetadata, String>>,
    },
    /// Build a project using stored analysis.
    BuildProject {
        path: PathBuf,
        opts: BuildOptions,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Test a project using stored analysis.
    TestProject {
        path: PathBuf,
        opts: TestOptions,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Get stored analysis without touching a module.
    GetAnalysis {
        path: String,
        reply_to: oneshot::Sender<Option<BuildMetadata>>,
    },
    /// List all registered modules and their capabilities.
    ListModules {
        reply_to: oneshot::Sender<Vec<ModuleCapability>>,
    },
    /// LLM tool invocation: routes build/* tools to the appropriate handler.
    CallTool {
        tool_name: String,
        args: serde_json::Value,
        reply_to: oneshot::Sender<serde_json::Value>,
    },
    /// Generic module tool invocation by build-system label (e.g. "Cargo").
    /// Used by the legacy `project_*` actors so ALL dispatch flows through
    /// the registered build modules — no hardcoded string branches.
    CallModuleTool {
        build_system: String,
        tool_name: String,
        args: serde_json::Value,
        reply_to: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// List this manager's unified build tools (actor-based discovery).
    ListTools {
        reply_to: oneshot::Sender<Vec<spire_core::actors::ToolInfo>>,
    },
    /// Parse a source file (routed by extension) and persist the AST into the
    /// graph via a transaction stream. Single-writer rule: only this manager
    /// writes AST nodes/edges.
    ParseAndStoreSourceFile {
        file_path: PathBuf,
        reply_to: oneshot::Sender<Result<ParseSummary, String>>,
    },
    ScaffoldBuildConfig {
        project_name: String,
        goal: String,
        build_file: String,
        /// Optional cross-platform targets (registry ids, e.g. `["rpi5"]`).
        /// Passed through to the build module's scaffold.
        platforms: Vec<String>,
        /// `"native" | "single_source" | "hal"` — forwarded to the build
        /// module's `scaffold_layout`. Defaults to Native for legacy callers.
        structure: Option<spire_core::build_types::ProjectStructure>,
        /// True for embedded projects (cross-compiled targets only — no host).
        embedded: bool,
        reply_to: oneshot::Sender<Result<crate::build::ScaffoldOutput, String>>,
    },
    /// Attach the UI broadcast sender for streaming build events.
    SetEventTx {
        event_tx: tokio::sync::broadcast::Sender<String>,
    },
    /// Attach the LLM sender (Stage-1 HAL implementation generation via the
    /// constrained `generate_hal_impl_prompt`).
    SetLlm {
        llm_tx: mpsc::Sender<LlmMessage>,
    },
}

/// The BuildManager actor — routes build/analysis requests to static modules.
pub struct BuildManagerActor {
    /// Router: config filename → module sender.
    router: HashMap<String, mpsc::Sender<BuildModuleMessage>>,
    /// Router: source file extension → module sender (for AST parsing).
    extension_router: HashMap<String, mpsc::Sender<BuildModuleMessage>>,
    /// Capabilities of all registered modules.
    capabilities: Vec<ModuleCapability>,
    /// Sender to the MemoryGraph actor for analysis state.
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    /// Optional broadcast sender for pushing BuildEvents to the UI event stream.
    event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    /// Shared, pollable buffer of recent build events (for incremental UI output).
    build_event_buffer: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    /// Notifier signaled whenever a new build event is pushed, so the UI can
    /// wake a waiter instead of polling on a timer.
    build_notify: std::sync::Arc<tokio::sync::Notify>,
    /// Optional LLM sender (Stage-1 implementation generation).
    llm_tx: Option<mpsc::Sender<LlmMessage>>,
}

impl BuildManagerActor {
    pub fn new(
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        build_event_buffer: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        build_notify: std::sync::Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            router: HashMap::new(),
            extension_router: HashMap::new(),
            capabilities: Vec::new(),
            memory_graph_tx,
            event_tx: None,
            build_event_buffer,
            build_notify,
            llm_tx: None,
        }
    }

    /// Attach the LLM sender (Stage-1 HAL implementation generation).
    pub fn set_llm(&mut self, llm_tx: mpsc::Sender<LlmMessage>) {
        self.llm_tx = Some(llm_tx);
    }

    /// Drain (remove + return) all accumulated build events. Used by the FFI to
    /// stream incremental output to the UI while a tool is still running.
    pub fn drain_build_events(&self) -> Vec<serde_json::Value> {
        let mut buf = self.build_event_buffer.lock().unwrap();
        std::mem::take(&mut *buf)
    }


    /// Attach the UI event broadcast sender so streaming build events can be pushed.
    pub fn set_event_tx(&mut self, event_tx: tokio::sync::broadcast::Sender<String>) {
        self.event_tx = Some(event_tx);
    }

    /// Register a module. Each config file maps to the module's sender, and
    /// each source extension maps to the module's sender for AST parsing.
    fn add_module(
        &mut self,
        capability: ModuleCapability,
        module_tx: mpsc::Sender<BuildModuleMessage>,
    ) {
        tracing::info!(
            "BuildManager: registering module '{}' with configs {:?}, extensions {:?}",
            capability.name, capability.config_files, capability.source_extensions
        );
        for config in &capability.config_files {
            self.router.insert(config.clone(), module_tx.clone());
        }
        for ext in &capability.source_extensions {
            self.extension_router.insert(ext.clone(), module_tx.clone());
        }
        self.capabilities.push(capability);
        tracing::info!(
            "BuildManager: router now has {} config files: {:?}",
            self.router.len(),
            self.router.keys().collect::<Vec<_>>()
        );
    }

    /// Find which registered config file applies to a path.
    /// For a file path, use its basename. For a directory, look for a known
    /// config file inside it.
    fn find_config_file(&self, path: &Path) -> Option<String> {
        if path.is_file() {
            let name = path.file_name()?.to_str()?.to_string();
            return self.router.contains_key(&name).then_some(name);
        }
        for config in self.router.keys() {
            if path.join(config).exists() {
                return Some(config.clone());
            }
        }
        None
    }

    /// Persist analysis metadata in the graph keyed by the path.
    async fn store_analysis(&self, path: &str, metadata: &BuildMetadata) -> Result<(), String> {
        let key = format!("build.analysis.{}", path);
        let value = serde_json::to_value(metadata)
            .map_err(|e| format!("Failed to serialize BuildMetadata: {e}"))?;
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::SetConfig {
                key,
                value,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("MemoryGraph channel closed: {e}"))?;
        rx.await
            .map_err(|e| format!("MemoryGraph response lost: {e}"))?
            .map_err(|e| format!("MemoryGraph store failed: {e}"))?;
        Ok(())
    }

    /// Fetch stored analysis metadata from the graph by path key.
    async fn get_analysis(&self, path: &str) -> Option<BuildMetadata> {
        let key = format!("build.analysis.{}", path);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .memory_graph_tx
            .send(MemoryGraphMessage::GetConfig { key, reply_to: tx })
            .await;
        match rx.await {
            Ok(Ok(Some(value))) => serde_json::from_value(value).ok(),
            _ => None,
        }
    }

    /// Send a single operation through a transaction stream and await its result.
    async fn send_stream_op(
        stream: &mpsc::Sender<TransactionRequest>,
        op: StreamOp,
    ) -> Result<StreamOpResult, String> {
        let (tx, rx) = oneshot::channel();
        stream
            .send(TransactionRequest {
                operation: op,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Transaction stream closed: {e}"))?;
        rx.await
            .map_err(|e| format!("Transaction stream response lost: {e}"))?
            .map_err(|e| format!("Stream op failed: {e}"))
    }

    /// Find an existing SourceFile node by its file path.
    async fn find_source_file(
        &self,
        path: &str,
    ) -> Option<spire_core::models::memory_graph::AttrNode> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("SourceFile".to_string()),
                subtype: None,
                name: None,
                limit: Some(100),
                reply_to: tx,
            })
            .await;
        match rx.await {
            Ok(Ok(nodes)) => nodes.into_iter().find(|n| {
                n.name() == path
                    || n.get("path").and_then(|v| v.as_str()) == Some(path)
            }),
            _ => None,
        }
    }

    /// Persist a run's BuildEvents as Diagnostic graph nodes linked to their
    /// SourceFile nodes via HasDiagnostic edges. Each run is tagged with a
    /// build_run_id so stale diagnostics can be cleaned up later.
    async fn ingest_diagnostics(
        &self,
        events: &[serde_json::Value],
        build_type: &str,
    ) -> Result<(), String> {
        // Collect events that carry a file path — these become Diagnostic nodes.
        let diag_events: Vec<&serde_json::Value> = events
            .iter()
            .filter(|ev| {
                ev.get("file").and_then(|f| f.as_str()).is_some()
                    && ev.get("level").and_then(|l| l.as_str()).is_some()
            })
            .collect();
        if diag_events.is_empty() {
            return Ok(());
        }

        let run_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::OpenTransactionStream { reply_to: tx })
            .await
            .map_err(|e| format!("MemoryGraph channel closed: {e}"))?;
        let stream = rx
            .await
            .map_err(|e| format!("MemoryGraph response lost: {e}"))?;

        for ev in diag_events {
            let file = ev["file"].as_str().unwrap_or("").to_string();
            let level = ev["level"].as_str().unwrap_or("info").to_string();
            let message = ev["message"]
                .as_str()
                .unwrap_or_else(|| ev["line"].as_str().unwrap_or(""))
                .to_string();
            let line = ev["line_number"].as_u64().map(|v| v as u32);
            let mut props = HashMap::new();
            props.insert("message".to_string(), serde_json::Value::String(message.clone()));
            props.insert("file".to_string(), serde_json::Value::String(file.clone()));
            if let Some(line) = line {
                props.insert("line".to_string(), serde_json::json!(line));
            }
            props.insert("severity".to_string(), serde_json::Value::String(level));
            props.insert("build_type".to_string(), serde_json::Value::String(build_type.to_string()));
            props.insert("build_run_id".to_string(), serde_json::Value::String(run_id.clone()));

            // Stable name so MergeNode upserts by (Diagnostic, name).
            let name = format!("{}:{}:{}", file, line.map(|l| l.to_string()).unwrap_or_default(), message);
            let merge_result = Self::send_stream_op(
                &stream,
                StreamOp::MergeNode(bm_attr_unknown(
                    "Diagnostic",
                    Some(build_type.to_string()),
                    name,
                    Some(message.clone()),
                    props.clone(),
                )),
            )
            .await?;

            // Link SourceFile --HasDiagnostic--> Diagnostic using the merged
            // node's stable id (kept across upserts).
            let diag_id = match merge_result {
                StreamOpResult::NodeStored(n) | StreamOpResult::NodeUpdated(n) => n.id().to_string(),
                _ => continue,
            };
            if let Some(source_node) = self.find_source_file(&file).await {
                let _ = Self::send_stream_op(
                    &stream,
                    StreamOp::MergeRelationship(RelationshipInput {
                        edge_type: RelationshipType::HasDiagnostic,
                        from_id: source_node.id().to_string(),
                        to_id: diag_id,
                        properties: None,
                        weight: None,
                    }),
                )
                .await;
            }
        }

        Self::send_stream_op(&stream, StreamOp::Commit).await?;
        Ok(())
    }

    /// Parse a source file via the extension router and persist the AST to the
    /// graph in a single transaction stream (UPSERT semantics via MergeNode /
    /// MergeRelationship). Skips the write when the content hash is unchanged.
    async fn parse_and_store_source_file(&self, path: &Path) -> Result<ParseSummary, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| format!("No file extension for {}", path.display()))?
            .to_string();
        let module_tx = self
            .extension_router
            .get(&ext)
            .cloned()
            .ok_or_else(|| format!("No parser registered for .{}", ext))?;

        // 1. Module parses the file (stateless — never touches the graph).
        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::ParseSourceFile {
                file_path: path.to_path_buf(),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {e}"))?;
        let result: AstParseResult = rx
            .await
            .map_err(|e| format!("Module response lost: {e}"))??;

        let path_str = path.to_string_lossy().to_string();

        // 2. Incremental re-parse: skip if the content hash is unchanged.
        if let Some(existing) = self.find_source_file(&path_str).await {
            if existing
                .get("content_hash")
                .and_then(|v| v.as_str())
                == Some(result.content_hash.as_str())
            {
                return Ok(ParseSummary {
                    nodes_written: 0,
                    edges_written: 0,
                    skipped: true,
                });
            }
        }

        // 3. Open a transaction stream and persist the AST.
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::OpenTransactionStream { reply_to: tx })
            .await
            .map_err(|e| format!("MemoryGraph channel closed: {e}"))?;
        let stream = rx
            .await
            .map_err(|e| format!("MemoryGraph response lost: {e}"))?;

        // SourceFile node — reuses the node created by ProjectSyncActor during
        // the bootstrap scan (unified file representation), or creates one if
        // no file-tree node exists yet. The AST `ast_child` edges below link
        // this SourceFile node (the file-tree entity) to its parsed children.
        let existing_source = self.find_source_file(&path_str).await;
        let source_file_id = if let Some(existing) = &existing_source {
            // Update metadata (content_hash, last_parsed, has_errors) in place.
            let mut source_props = HashMap::new();
            source_props.insert(
                "content_hash".to_string(),
                serde_json::Value::String(result.content_hash.clone()),
            );
            source_props.insert(
                "last_parsed".to_string(),
                serde_json::Value::String(Utc::now().to_rfc3339()),
            );
            source_props.insert(
                "has_errors".to_string(),
                serde_json::Value::Bool(result.has_errors),
            );
            Self::send_stream_op(
                &stream,
                StreamOp::UpdateNode {
                    id: existing.id().to_string(),
                    updates: NodeUpdate {
                        node_type: None,
                        subtype: None,
                        name: None,
                        description: None,
                        properties: Some(source_props),
                        embedding_id: None,
                    },
                },
            )
            .await?;
            existing.id().to_string()
        } else {
            // No file-tree node yet — create a fresh SourceFile (UPSERT).
            let mut source_props = HashMap::new();
            source_props.insert(
                "path".to_string(),
                serde_json::Value::String(path_str.clone()),
            );
            source_props.insert(
                "language".to_string(),
                serde_json::Value::String(result.language.clone()),
            );
            source_props.insert(
                "content_hash".to_string(),
                serde_json::Value::String(result.content_hash.clone()),
            );
            source_props.insert(
                "last_parsed".to_string(),
                serde_json::Value::String(Utc::now().to_rfc3339()),
            );
            source_props.insert(
                "has_errors".to_string(),
                serde_json::Value::Bool(result.has_errors),
            );
            let op_result = Self::send_stream_op(
                &stream,
                StreamOp::MergeNode(bm_attr_unknown(
                    "SourceFile",
                    None,
                    path_str.clone(),
                    Some(format!("Source file: {}", path_str)),
                    source_props,
                )),
            )
            .await?;
            match op_result {
                StreamOpResult::NodeStored(n) | StreamOpResult::NodeUpdated(n) => {
                    n.id().to_string()
                }
                _ => return Err("MergeNode did not return a node".to_string()),
            }
        };
        let mut nodes_written = 1usize;
        let mut edges_written = 0usize;

        // AST nodes (UPSERT by (type, name)).
        // Map each index in result.nodes to its stored node id (None → skipped).
        let mut stored_ids: Vec<Option<String>> = Vec::with_capacity(result.nodes.len());
        let mut top_level_ids: Vec<String> = Vec::new();
        for node in &result.nodes {
            let node_type = match node.node_type.as_str() {
                "function" | "method" => "AstFunction",
                "class" => "AstClass",
                "import" => "AstImport",
                "variable" => "AstVariable",
                _ => {
                    // Blocks and other structural nodes are not stored as
                    // first-class graph nodes; their edges are skipped too.
                    stored_ids.push(None);
                    continue;
                }
            };

            let mut props = HashMap::new();
            props.insert(
                "kind".to_string(),
                serde_json::Value::String(node.node_type.clone()),
            );
            props.insert(
                "text".to_string(),
                serde_json::Value::String(node.text.clone()),
            );
            props.insert(
                "start_line".to_string(),
                serde_json::Value::Number(node.start_line.into()),
            );
            props.insert(
                "start_col".to_string(),
                serde_json::Value::Number(node.start_col.into()),
            );
            props.insert(
                "end_line".to_string(),
                serde_json::Value::Number(node.end_line.into()),
            );
            props.insert(
                "end_col".to_string(),
                serde_json::Value::Number(node.end_col.into()),
            );
            props.insert(
                "depth".to_string(),
                serde_json::Value::Number(node.depth.into()),
            );
            props.insert(
                "file_path".to_string(),
                serde_json::Value::String(path_str.clone()),
            );
            props.insert(
                "language".to_string(),
                serde_json::Value::String(result.language.clone()),
            );
            props.insert(
                "is_public".to_string(),
                serde_json::Value::Bool(node.is_public),
            );
            props.insert(
                "is_async".to_string(),
                serde_json::Value::Bool(node.is_async),
            );
            if let Some(sig) = &node.signature {
                props.insert(
                    "signature".to_string(),
                    serde_json::Value::String(sig.clone()),
                );
            }
            if let Some(rt) = &node.return_type {
                props.insert(
                    "return_type".to_string(),
                    serde_json::Value::String(rt.clone()),
                );
            }

            let name = node.name.clone().unwrap_or_else(|| {
                format!("{}@{}:{}", node.node_type, node.start_line, node.start_col)
            });
            let description = if node.text.is_empty() {
                None
            } else {
                Some(node.text.chars().take(200).collect())
            };

            let op_result = Self::send_stream_op(
                &stream,
                StreamOp::MergeNode(bm_attr_unknown(
                    node_type,
                    None,
                    name.clone(),
                    description,
                    props,
                )),
            )
            .await?;
            let id = match op_result {
                StreamOpResult::NodeStored(n) | StreamOpResult::NodeUpdated(n) => {
                    n.id().to_string()
                }
                _ => return Err("MergeNode did not return a node".to_string()),
            };
            if node.depth == 0 {
                top_level_ids.push(id.clone());
            }
            stored_ids.push(Some(id));
            nodes_written += 1;
        }

        // Link the owning SourceFile node to its top-level AST nodes
        // (the file tree and AST graph are unified — one node per file).
        for (order, child_id) in top_level_ids.iter().enumerate() {
            let mut props = HashMap::new();
            props.insert(
                "order".to_string(),
                serde_json::Value::Number((order as u32).into()),
            );
            Self::send_stream_op(
                &stream,
                StreamOp::MergeRelationship(RelationshipInput {
                    edge_type: RelationshipType::Custom("ast_child".to_string()),
                    from_id: source_file_id.clone(),
                    to_id: child_id.clone(),
                    properties: Some(props),
                    weight: None,
                }),
            )
            .await?;
            edges_written += 1;
        }

        // AST edges (UPSERT by (edge label, from_id, to_id)).
        for edge in &result.edges {
            let from_id = match stored_ids.get(edge.from_index) {
                Some(Some(id)) => id.clone(),
                _ => continue,
            };
            let to_id = match stored_ids.get(edge.to_index) {
                Some(Some(id)) => id.clone(),
                _ => continue,
            };
            let edge_type = match edge.edge_type.as_str() {
                "child" => "ast_child",
                "calls" => "ast_calls",
                "imports" => "ast_imports",
                "references" => "ast_references",
                _ => continue,
            };

            let mut props = HashMap::new();
            if let Some(order) = edge.order {
                props.insert("order".to_string(), serde_json::Value::Number(order.into()));
            }
            if let Some(field) = &edge.field {
                props.insert(
                    "field".to_string(),
                    serde_json::Value::String(field.clone()),
                );
            }

            Self::send_stream_op(
                &stream,
                StreamOp::MergeRelationship(RelationshipInput {
                    edge_type: RelationshipType::Custom(edge_type.to_string()),
                    from_id,
                    to_id,
                    properties: Some(props),
                    weight: None,
                }),
            )
            .await?;
            edges_written += 1;
        }

        // 4. Commit the transaction.
        Self::send_stream_op(&stream, StreamOp::Commit).await?;

        Ok(ParseSummary {
            nodes_written,
            edges_written,
            skipped: false,
        })
    }

    /// Analyze: route to the matching module, then store result in graph.
    async fn analyze_project(&self, path: &Path) -> Result<BuildMetadata, String> {
        let config = self
            .find_config_file(path)
            .ok_or_else(|| format!("No known build config found for {}", path.display()))?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Analyze {
                path: path.to_path_buf(),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        let metadata = rx
            .await
            .map_err(|e| format!("Module response lost: {}", e))??;

        // Persist as the manager's state (single writer via MemoryGraphActor).
        // This is best-effort: a persistence failure must not discard a valid
        // analysis result. The metadata is already correct; it just won't be
        // available via GetAnalysis until a later successful persist.
        let path_str = path.to_string_lossy().to_string();
        if let Err(e) = self.store_analysis(&path_str, &metadata).await {
            tracing::warn!(
                "BuildManager: failed to persist analysis for {}: {}",
                path_str, e
            );
        }

        Ok(metadata)
    }

    /// Build: fetch stored analysis, route to module with the analysis in the
    /// message (the module stays stateless). Batch — returns only BuildOutput.
    async fn build_project(&self, path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
        let path_str = path.to_string_lossy().to_string();
        let metadata = self.get_analysis(&path_str).await.ok_or_else(|| {
            format!(
                "No stored analysis for {}; run AnalyzeProject first",
                path_str
            )
        })?;

        let config = metadata
            .config_files
            .first()
            .cloned()
            .ok_or_else(|| "Stored analysis has no config file".to_string())?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let build_spec = Self::resolve_build_spec(&metadata, opts);
        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Build {
                path: path.to_path_buf(),
                metadata,
                opts: opts.clone(),
                build_spec,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        rx.await
            .map_err(|e| format!("Module response lost: {}", e))?
    }

    /// Streaming build that collects every per-line BuildEvent and returns them
    /// alongside the final BuildOutput. The UI receives ALL lines reliably (no
    /// broadcast-channel overflow).
    async fn build_project_with_events(
        &self,
        path: &Path,
        opts: &BuildOptions,
    ) -> Result<(BuildOutput, Vec<serde_json::Value>), String> {
        let path_str = path.to_string_lossy().to_string();
        let metadata = self.get_analysis(&path_str).await.ok_or_else(|| {
            format!(
                "No stored analysis for {}; run AnalyzeProject first",
                path_str
            )
        })?;

        let config = metadata
            .config_files
            .first()
            .cloned()
            .ok_or_else(|| "Stored analysis has no config file".to_string())?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        let (build_event_tx, mut build_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::build::BuildEvent>();
        let events_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let shared_buf = self.build_event_buffer.clone();
        let notify = self.build_notify.clone();
        {
            let events_buf = events_buf.clone();
            tokio::spawn(async move {
                while let Some(ev) = build_event_rx.recv().await {
                    let json = serde_json::json!({
                        "line": ev.line,
                        "level": ev.level,
                        "target": ev.target,
                        "file": ev.file,
                        "line_number": ev.line_number,
                        "message": ev.message,
                        "detail": ev.detail,
                    });
                    events_buf.lock().unwrap().push(json.clone());
                    // Incremental: push into the shared pollable buffer too.
                    shared_buf.lock().unwrap().push(json);
        // Wake a waiting FFI listener (async push, no polling).
        notify.notify_one();
        tracing::info!("build forwarder: pushed 1 event + notified (line={})", ev.line);
                }
            });
        }
        let build_spec = Self::resolve_build_spec(&metadata, opts);
        module_tx
            .send(BuildModuleMessage::BuildStreaming {
                path: path.to_path_buf(),
                metadata,
                opts: opts.clone(),
                build_spec,
                event_tx: build_event_tx,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        let output = rx.await.map_err(|e| format!("Module response lost: {}", e))?;
        let events = events_buf.lock().unwrap().clone();
        output.map(|o| (o, events))
    }

    /// Streaming lint that pushes every line into the shared pollable buffer.
    async fn lint_project_streaming(
        &self,
        path: &Path,
        platform: Option<String>,
    ) -> Result<(BuildOutput, Vec<serde_json::Value>), String> {
        let path_str = path.to_string_lossy().to_string();
        let metadata = self.get_analysis(&path_str).await.ok_or_else(|| {
            format!(
                "No stored analysis for {}; run AnalyzeProject first",
                path_str
            )
        })?;

        let config = metadata
            .config_files
            .first()
            .cloned()
            .ok_or_else(|| "Stored analysis has no config file".to_string())?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        let (build_event_tx, mut build_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::build::BuildEvent>();
        let events_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let shared_buf = self.build_event_buffer.clone();
        let notify = self.build_notify.clone();
        {
            let events_buf = events_buf.clone();
            tokio::spawn(async move {
                while let Some(ev) = build_event_rx.recv().await {
                    let json = serde_json::json!({
                        "line": ev.line,
                        "level": ev.level,
                        "target": ev.target,
                        "file": ev.file,
                        "line_number": ev.line_number,
                        "message": ev.message,
                        "detail": ev.detail,
                    });
                    events_buf.lock().unwrap().push(json.clone());
                    shared_buf.lock().unwrap().push(json);
        // Wake a waiting FFI listener (async push, no polling).
        notify.notify_one();
                }
            });
        }
        module_tx
            .send(BuildModuleMessage::LintStreaming {
                path: path.to_path_buf(),
                metadata,
                platform: platform.clone(),
                event_tx: build_event_tx,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        let output = rx.await.map_err(|e| format!("Module response lost: {}", e))?;
        let events = events_buf.lock().unwrap().clone();
        output.map(|o| (o, events))
    }

    /// Streaming fix that pushes every line into the shared pollable buffer.
    async fn fix_project_streaming(
        &self,
        path: &Path,
    ) -> Result<(BuildOutput, Vec<serde_json::Value>), String> {
        let path_str = path.to_string_lossy().to_string();
        let metadata = self.get_analysis(&path_str).await.ok_or_else(|| {
            format!(
                "No stored analysis for {}; run AnalyzeProject first",
                path_str
            )
        })?;

        let config = metadata
            .config_files
            .first()
            .cloned()
            .ok_or_else(|| "Stored analysis has no config file".to_string())?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        let (build_event_tx, mut build_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::build::BuildEvent>();
        let events_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let shared_buf = self.build_event_buffer.clone();
        let notify = self.build_notify.clone();
        {
            let events_buf = events_buf.clone();
            tokio::spawn(async move {
                while let Some(ev) = build_event_rx.recv().await {
                    let json = serde_json::json!({
                        "line": ev.line,
                        "level": ev.level,
                        "target": ev.target,
                        "file": ev.file,
                        "line_number": ev.line_number,
                        "message": ev.message,
                        "detail": ev.detail,
                    });
                    events_buf.lock().unwrap().push(json.clone());
                    shared_buf.lock().unwrap().push(json);
        // Wake a waiting FFI listener (async push, no polling).
        notify.notify_one();
                }
            });
        }
        module_tx
            .send(BuildModuleMessage::FixStreaming {
                path: path.to_path_buf(),
                metadata,
                event_tx: build_event_tx,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        let output = rx.await.map_err(|e| format!("Module response lost: {}", e))?;
        let events = events_buf.lock().unwrap().clone();
        output.map(|o| (o, events))
    }

    /// Test: same pattern as build.
    async fn test_project(&self, path: &Path, opts: &TestOptions) -> Result<BuildOutput, String> {
        let path_str = path.to_string_lossy().to_string();
        let metadata = self.get_analysis(&path_str).await.ok_or_else(|| {
            format!(
                "No stored analysis for {}; run AnalyzeProject first",
                path_str
            )
        })?;

        let config = metadata
            .config_files
            .first()
            .cloned()
            .ok_or_else(|| "Stored analysis has no config file".to_string())?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Test {
                path: path.to_path_buf(),
                metadata,
                opts: opts.clone(),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        rx.await
            .map_err(|e| format!("Module response lost: {}", e))?
    }

    /// Unified LLM tool entry point: routes build/* tools to handlers.
    /// Clean: same pattern as build — route to module via a proper message.
    async fn clean_project(&self, path: &Path) -> Result<BuildOutput, String> {
        let metadata = self
            .get_analysis(path.to_string_lossy().as_ref())
            .await
            .ok_or_else(|| {
                format!(
                    "No stored analysis for {}; run AnalyzeProject first",
                    path.display()
                )
            })?;

        let config = metadata
            .config_files
            .first()
            .cloned()
            .ok_or_else(|| "Stored analysis has no config file".to_string())?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Clean {
                path: path.to_path_buf(),
                metadata,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        rx.await
            .map_err(|e| format!("Module response lost: {}", e))?
    }

/// Parse clang/clang++ analyzer output into structured lint events:
/// `file:line:col: error|warning: message` (and `file:line:col: fatal error: …`).
fn parse_clang_output(output: &str) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    // Clang emits diagnostics as:  /path/file.cpp:12:5: error: message text
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Split at the first ": " that precedes error/warning/fatal.
        let lower = line.to_lowercase();
        let markers = ["error:", "warning:", "fatal error:"];
        let Some(marker) = markers.iter().find(|m| lower.contains(**m)) else {
            continue;
        };
        // Find the position of the message.
        let Some(msg_idx) = lower.find(marker) else { continue };
        let path_part = &line[..msg_idx];
        let msg = line[msg_idx + marker.len()..].trim().to_string();
        // path_part = "/abs/file.cpp:12:5: "
        let mut segs = path_part.rsplitn(3, ':');
        let col = segs.next().and_then(|c| c.trim().parse::<u64>().ok());
        let line_no = segs.next().and_then(|l| l.trim().parse::<u64>().ok());
        let file = segs.next().map(|f| f.trim().to_string()).unwrap_or_default();
        let severity = if marker.starts_with("error") { "error" } else { "warning" };
        events.push(serde_json::json!({
            "file": file,
            "level": severity,
            "line": msg.clone(),
            "message": msg,
            "line_number": line_no,
            "column": col,
        }));
    }
    events
}


    /// Format: same pattern as build — route to module via a proper message.
    async fn format_project(&self, path: &Path) -> Result<BuildOutput, String> {
        let metadata = self
            .get_analysis(path.to_string_lossy().as_ref())
            .await
            .ok_or_else(|| {
                format!(
                    "No stored analysis for {}; run AnalyzeProject first",
                    path.display()
                )
            })?;

        let config = metadata
            .config_files
            .first()
            .cloned()
            .ok_or_else(|| "Stored analysis has no config file".to_string())?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Format {
                path: path.to_path_buf(),
                metadata,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        rx.await
            .map_err(|e| format!("Module response lost: {}", e))?
    }

    /// Scaffold a new project by routing to the build module that owns the
    /// requested build system (e.g. "Cargo" → Cargo.toml, "Python" → pyproject.toml,
    /// "Meson" → meson.build). Templates live 1:1 with each build module.
    async fn scaffold_build_config(
        &self,
        project_name: &str,
        build_system: &str,
        goal: &str,
        platforms: &[String],
        structure: spire_core::build_types::ProjectStructure,
        embedded: bool,
    ) -> Result<crate::build::ScaffoldOutput, String> {
        // Map the build-system label to the config file the module owns.
        let config = self
            .capabilities
            .iter()
            .find(|cap| {
                cap.build_system.eq_ignore_ascii_case(build_system)
                    || cap.language.eq_ignore_ascii_case(build_system)
            })
            .and_then(|cap| cap.config_files.first().cloned())
            .ok_or_else(|| {
                format!("No build module registered for build system '{}'", build_system)
            })?;

        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::ScaffoldBuildConfig {
                project_name: project_name.to_string(),
                goal: goal.to_string(),
                platforms: platforms.to_vec(),
                structure,
                embedded,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        rx.await
            .map_err(|e| format!("Module response lost: {}", e))?
    }

    /// Find the registered module capability for a build-system label.
    /// Matches `build_system` or `language` case-insensitively (e.g. "Cargo",
    /// "Rust", "npm", "JavaScript", "Meson").
    fn module_for_build_system(&self, bt: &str) -> Option<&ModuleCapability> {
        self.capabilities.iter().find(|cap| {
            cap.build_system.eq_ignore_ascii_case(bt) || cap.language.eq_ignore_ascii_case(bt)
        })
    }

    /// Dispatch a generic tool to the module that owns `build_system` by
    /// wrapping `BuildModuleMessage::CallTool`. Unwraps the module's
    /// MCP-style reply envelope (`{ result: { content, isError } }`) into a
    /// plain JSON value so callers (project_* actors) get the same shape
    /// they previously received from MCP servers.
    async fn call_module_tool(
        &self,
        build_system: &str,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let config = self
            .module_for_build_system(build_system)
            .and_then(|cap| cap.config_files.first().cloned())
            .ok_or_else(|| {
                format!(
                    "No build module registered for build system '{}'",
                    build_system
                )
            })?;
        let module_tx = self
            .router
            .get(&config)
            .ok_or_else(|| format!("No module registered for {}", config))?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::CallTool {
                tool_name: tool_name.to_string(),
                args,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        let reply = rx
            .await
            .map_err(|e| format!("Module response lost: {}", e))?;

        // Modules reply with an MCP-shaped envelope; some (node/meson stubs)
        // reply with a bare `{ "error": "…" }` instead.
        if let Some(err_text) = reply
            .get("error")
            .and_then(|v| v.as_str())
            .or_else(|| {
                reply
                    .get("result")
                    .and_then(|r| r.get("isError"))
                    .and_then(|ie| ie.as_bool())
                    .filter(|b| *b)
                    .and_then(|_| {
                        reply["result"]["content"][0]
                            .get("text")
                            .and_then(|t| t.as_str())
                    })
            })
        {
            return Err(err_text.to_string());
        }

        let text = reply["result"]["content"][0]
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("{}");
        serde_json::from_str(text)
            .map_err(|e| format!("Failed to parse module tool response: {}", e))
    }

    /// Semantic Stage-1 (LLM half, PLAN only): build the context, run the LLM
    /// (fence-strip + syntax retry) and return the PROPOSED module pair
    /// (deterministic clean header + LLM .cpp) WITHOUT writing anything. The
    /// UI previews this for approval, then calls `hal_generate_impl_apply`.
    async fn hal_generate_plan(
        &self,
        root: &str,
        interface: &str,
        platform: &str,
        library_hints: Option<&str>,
    ) -> serde_json::Value {
        let ctx = match hal_impl_generation_context(root, interface, platform, library_hints) {
            Ok(ctx) => ctx,
            Err(e) => return serde_json::json!({ "error": e }),
        };
        let (source, syntax) =
            match hal_llm_generate_source(&self.llm_tx, ctx.prompt.clone()).await {
                Ok(pair) => pair,
                Err(e) => return serde_json::json!({ "error": e }),
            };
        let hpp_path = ctx.impl_dir.join(&ctx.hpp_name);
        let cpp_path = ctx.impl_dir.join(&ctx.cpp_name);
        serde_json::json!({
            "interface": interface,
            "platform": platform,
            "class_name": ctx.class_name,
            "hpp_path": hpp_path.to_string_lossy().to_string(),
            "cpp_path": cpp_path.to_string_lossy().to_string(),
            "header": ctx.impl_header,
            "source": source,
            "prompt": ctx.prompt,
            "syntax": if syntax.is_empty() { "ok".to_string() } else { syntax },
        })
    }

    /// Semantic Stage-1 (LLM half, APPLY): write an APPROVED module pair
    /// (header + source from the plan), remove stale stubs for the interface,
    /// wire hal/meson.build and run the meson compile gate. No LLM call here —
    /// the approved content is passed in by the UI.
    async fn hal_generate_apply(
        &self,
        root: &str,
        interface: &str,
        platform: &str,
        args: &serde_json::Value,
    ) -> serde_json::Value {
        let class_name = args.get("class_name").and_then(|v| v.as_str()).unwrap_or_default();
        let hpp_path = args.get("hpp_path").and_then(|v| v.as_str()).unwrap_or_default();
        let cpp_path = args.get("cpp_path").and_then(|v| v.as_str()).unwrap_or_default();
        let header = args.get("header").and_then(|v| v.as_str()).unwrap_or_default();
        let source = args.get("source").and_then(|v| v.as_str()).unwrap_or_default();
        if hpp_path.is_empty() || cpp_path.is_empty() || header.is_empty() || source.is_empty() {
            return serde_json::json!({ "error": "hal_generate_impl_apply: 'hpp_path', 'cpp_path', 'header' and 'source' are required" });
        }
        // Write the approved module pair.
        if let Some(parent) = std::path::Path::new(hpp_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return serde_json::json!({ "error": format!("create {}: {e}", parent.display()) });
            }
        }
        if let Err(e) = std::fs::write(hpp_path, header).and_then(|_| std::fs::write(cpp_path, source)) {
            return serde_json::json!({ "error": format!("write pair: {e}") });
        }
        // Remove stale stubs so coverage never ORs their sentinel.
        let impl_dir = std::path::Path::new(root)
            .join("hal").join("implementations").join(platform);
        let removed_stubs =
            crate::build::generic_helpers::remove_stale_hal_stubs(&impl_dir, interface);
        // Idempotent meson wiring.
        let meson_path = std::path::Path::new(root).join("hal").join("meson.build");
        if let Ok(existing) = std::fs::read_to_string(&meson_path) {
            let cpp_file = std::path::Path::new(cpp_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let updated = crate::build::generic_helpers::hal_meson_upsert_sources(
                &existing,
                platform,
                &[interface.to_string()],
                &[cpp_file],
            );
            if updated != existing {
                let _ = std::fs::write(&meson_path, updated);
            }
        }
        // Build gate.
        let gate = format!("meson compile -C build-{platform} {interface}-{platform}");
        let gate_status = match crate::build::generic_helpers::run_cmd(
            std::path::Path::new(root), "meson",
            &["compile", "-C", &format!("build-{platform}"), &format!("{interface}-{platform}")],
        ).await {
            Ok(o) if o.success => "build passed".to_string(),
            Ok(o) => format!("build FAILED:\n{}", o.output.lines().take(20).collect::<Vec<_>>().join("\n")),
            Err(e) => format!("build gate unavailable: {e}"),
        };
        serde_json::json!({
            "interface": interface,
            "platform": platform,
            "class_name": class_name,
            "written": vec![hpp_path.to_string(), cpp_path.to_string()],
            "removed_stubs": removed_stubs,
            "gate": gate,
            "gate_status": gate_status,
        })
    }

    async fn call_tool(&self, tool_name: &str, args: serde_json::Value) -> serde_json::Value {
        match tool_name {
            "build_analyze" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match self.analyze_project(Path::new(path)).await {
                    Ok(md) => serde_json::to_value(md)
                        .unwrap_or(serde_json::json!({"error": "serialize"})),
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_build" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let mode = args.get("mode").and_then(|v| v.as_str()).unwrap_or("");
                let opts = BuildOptions {
                    mode: mode.to_string(),
                    package: args.get("package").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    platform: args.get("platform").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    target: args.get("target").and_then(|v| v.as_str()).map(|s| s.to_string()),
                };
                match self.build_project_with_events(Path::new(path), &opts).await {
                    Ok((o, mut events)) => {
                        // Persist warnings/errors as Diagnostic graph nodes so the
                        // Build tab can show them after the run. Meson/ninja put
                        // compiler warnings in o.output as plain text (the streamed
                        // events only carry module "finished" markers), so parse
                        // them into diagnostic events when none were streamed.
                        let has_file_diags =
                            events.iter().any(|e| e.get("file").and_then(|f| f.as_str()).is_some());
                        if !has_file_diags {
                            events.append(&mut Self::parse_clang_output(&o.output));
                        }
                        let _ = self.ingest_diagnostics(&events, "build").await;
                        // Persist the build status (success/duration + raw output)
                        // for the Build detail tab header, keyed PER TARGET so
                        // building rock3c and rpi5 store separate results.
                        let _ = self
                            .store_build_status_with_output(path, opts.target.as_deref(), o.success, o.duration_secs, &o.output)
                            .await;
                        let mut val = serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"}));
                        if let serde_json::Value::Object(ref mut m) = val {
                            m.insert("buildEvents".to_string(), serde_json::json!(events));
                        }
                        val
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_test" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let filter = args
                    .get("filter")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let opts = TestOptions { filter };
                match self.test_project(Path::new(path), &opts).await {
                    Ok(o) => {
                        // Persist test status for the Build detail tab header.
                        let _ = self
                            .store_build_status(path, None, o.success, o.duration_secs)
                            .await;
                        serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"}))
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_clean" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
                match self.clean_project(Path::new(path)).await {
                    Ok(o) => serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"})),
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_lint" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
                tracing::info!("build_lint: path={:?} exists={}", path, std::path::Path::new(path).exists());
                let platform = args.get("platform").and_then(|v| v.as_str()).map(|s| s.to_string());
                match self.lint_project_streaming(Path::new(path), platform).await {
                    Ok((o, mut events)) => {
                        tracing::info!("build_lint OK: success={} out_len={} events={}", o.success, o.output.len(), events.len());
                        // Meson lint returns clang analyzer output as plain text in
                        // `o.output` (the streaming events are usually empty), so
                        // parse the clang diagnostic lines into structured events.
                        // That lets ingest_diagnostics persist graph nodes and the
                        // UI's lint/diagnostics panel show real findings.
                        if events.is_empty() {
                            events.append(&mut Self::parse_clang_output(&o.output));
                            tracing::info!("build_lint: parsed {} diagnostics from output", events.len());
                        }
                        // Persist lint findings as Diagnostic graph nodes so the
                        // Build tab shows them after the lint run.
                        let _ = self.ingest_diagnostics(&events, "lint").await;
                        let _ = self
                            .store_build_status(path, None, o.success, o.duration_secs)
                            .await;
                        let mut val = serde_json::to_value(&o).unwrap_or(serde_json::json!({"error": "serialize"}));
                        if let serde_json::Value::Object(ref mut m) = val {
                            m.insert("buildEvents".to_string(), serde_json::json!(events));
                        }
                        val
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_format" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
                match self.format_project(Path::new(path)).await {
                    Ok(o) => serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"})),
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_fix" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
                match self.fix_project_streaming(Path::new(path)).await {
                    Ok((o, events)) => {
                        let mut val = serde_json::to_value(&o).unwrap_or(serde_json::json!({"error": "serialize"}));
                        if let serde_json::Value::Object(ref mut m) = val {
                            m.insert("buildEvents".to_string(), serde_json::json!(events.clone()));
                        }
                        // Persist post-fix diagnostics (usually no remaining events).
                        let _ = self.ingest_diagnostics(&events, "fix").await;
                        val
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_scaffold" => {
                let project_name = args.get("project_name").and_then(|v| v.as_str()).unwrap_or_default();
                let build_system = args.get("build_system").and_then(|v| v.as_str()).unwrap_or_default();
                let goal = args.get("goal").and_then(|v| v.as_str()).unwrap_or_default();
                // Empty platforms => single-target ("host") scaffold.
                let platforms: Vec<String> = args
                    .get("platforms")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|p| p.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_else(|| vec!["host".to_string()]);
                match self.scaffold_build_config(project_name, build_system, goal, &platforms, spire_core::build_types::ProjectStructure::Native, false).await {
                    Ok(out) => serde_json::to_value(out).unwrap_or_else(|_| serde_json::json!({ "error": "scaffold serialization" })),
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_list_modules" => {
                serde_json::to_value(&self.capabilities).unwrap_or(serde_json::json!([]))
            }
            "build_dependency_docs" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                let version = args.get("version").and_then(|v| v.as_str()).unwrap_or("");
                // Route to the module that owns the requested language, falling
                // back to Cargo (Rust) which is the primary supported runtime.
                let lang = args.get("language").and_then(|v| v.as_str()).unwrap_or("Rust");
                let module_tx = self
                    .capabilities
                    .iter()
                    .find(|c| c.language.eq_ignore_ascii_case(lang) || c.build_system.eq_ignore_ascii_case(lang))
                    .and_then(|c| self.router.get(c.config_files.first().map(|s| s.as_str()).unwrap_or("")))
                    .cloned()
                    .or_else(|| self.router.get("Cargo.toml").cloned());
                match module_tx {
                    Some(tx) => {
                        let (t, r) = oneshot::channel();
                        let _ = tx
                            .send(crate::build::BuildModuleMessage::CallTool {
                                tool_name: "get_dependency_docs".to_string(),
                                args: serde_json::json!({ "name": name, "version": version }),
                                reply_to: t,
                            })
                            .await;
                        match r.await {
                            Ok(v) => v,
                            Err(e) => serde_json::json!({ "error": format!("dependency docs response lost: {}", e) }),
                        }
                    }
                    None => serde_json::json!({ "error": "no build module available for dependency docs" }),
                }
            }
            // ── HAL contract helpers (Phase A) ─────────────────────────
            // Exposed through tools/call so the Swift wizard can validate a
            // proposed abstract-class header, generate per-target placeholder
            // implementations, and diff contract versions — all against the
            // deterministic contract tooling in spire-modules.
            "hal_validate_contract" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if content.is_empty() {
                    serde_json::json!({ "error": "hal_validate_contract: 'content' (header source) is required" })
                } else {
                    match crate::build::generic_helpers::summarize_hal_header(content) {
                        Ok(summary) => serde_json::json!({ "valid": true, "summary": summary }),
                        Err(e) => serde_json::json!({ "valid": false, "error": e }),
                    }
                }
            }
            // Phase E step 2 — "Approve & Write": validate then persist the
            // contract header to `hal/api/<name>.hpp`. Same Stage-0 gate as
            // `hal_validate_contract` — an invalid header never touches disk.
            "hal_write_contract" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let filename = args
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("camera_hal.hpp");
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if root.is_empty() || content.is_empty() {
                    serde_json::json!({ "error": "hal_write_contract: 'root' and 'content' are required" })
                } else {
                    match crate::build::generic_helpers::summarize_hal_header(content) {
                        Err(e) => serde_json::json!({ "valid": false, "error": e }),
                        Ok(summary) => {
                            // Sanitize the filename: keep the stem, force .hpp.
                            let stem = std::path::Path::new(filename)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("camera_hal");
                            let safe_name = format!("{stem}.hpp");
                            let api_dir = std::path::Path::new(root).join("hal").join("api");
                            let target = api_dir.join(&safe_name);
                            let write_result = std::fs::create_dir_all(&api_dir)
                                .and_then(|_| std::fs::write(&target, content));
                            match write_result {
                                Ok(()) => serde_json::json!({
                                    "valid": true,
                                    "summary": summary,
                                    "written": target.to_string_lossy().to_string(),
                                }),
                                Err(e) => serde_json::json!({
                                    "valid": false,
                                    "error": format!("failed to write {}: {e}", target.display()),
                                }),
                            }
                        }
                    }
                }
            }

            // Step 4 (deterministic half): resolve the contract header + the
            // registry platform record and build the SEMANTIC module-pair prompt
            // via hal_impl_generation_context (contract + structured docs +
            // hardware profile + library hints + clean impl header + meson
            // build gate). The LLM runs against THIS prompt.
            "hal_build_impl_prompt" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                let interface = args.get("interface").and_then(|v| v.as_str()).unwrap_or_default();
                let platform = args.get("platform").and_then(|v| v.as_str()).unwrap_or_default();
                if root.is_empty() || interface.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_build_impl_prompt: 'root', 'interface' and 'platform' are required" })
                } else {
                    match hal_impl_generation_context(
                        root,
                        interface,
                        platform,
                        args.get("library_hints").and_then(|v| v.as_str()),
                    ) {
                        Ok(ctx) => serde_json::json!({
                            "interface": interface,
                            "platform": platform,
                            "class_name": ctx.class_name,
                            "header": ctx.impl_header,
                            "prompt": ctx.prompt,
                            "summary": ctx.summary,
                        }),
                        Err(e) => serde_json::json!({ "error": e }),
                    }
                }
            }

            // Step 4 (LLM half, one-shot): plan → apply in one step.
            "hal_generate_impl" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                let interface = args.get("interface").and_then(|v| v.as_str()).unwrap_or_default();
                let platform = args.get("platform").and_then(|v| v.as_str()).unwrap_or_default();
                if root.is_empty() || interface.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_generate_impl: 'root', 'interface' and 'platform' are required" })
                } else if self.llm_tx.is_none() {
                    serde_json::json!({ "error": "hal_generate_impl: LLM not configured — set your API key in Settings" })
                } else {
                    let plan = self.hal_generate_plan(
                        root, interface, platform,
                        args.get("library_hints").and_then(|v| v.as_str()),
                    ).await;
                    if plan.get("error").is_some() {
                        plan
                    } else {
                        let syntax = plan.get("syntax").cloned().unwrap_or(serde_json::json!("ok"));
                        let mut result = self.hal_generate_apply(root, interface, platform, &plan).await;
                        if let serde_json::Value::Object(ref mut m) = result {
                            m.insert("syntax".to_string(), syntax);
                        }
                        result
                    }
                }
            }

            // Step 4 (LLM half, PLAN): preview the PROPOSED module pair (clean
            // header + LLM .cpp) before any write. The UI shows it for approval
            // then calls `hal_generate_impl_apply`.
            "hal_generate_impl_plan" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                let interface = args.get("interface").and_then(|v| v.as_str()).unwrap_or_default();
                let platform = args.get("platform").and_then(|v| v.as_str()).unwrap_or_default();
                if root.is_empty() || interface.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_generate_impl_plan: 'root', 'interface' and 'platform' are required" })
                } else if self.llm_tx.is_none() {
                    serde_json::json!({ "error": "hal_generate_impl_plan: LLM not configured — set your API key in Settings" })
                } else {
                    self.hal_generate_plan(
                        root, interface, platform,
                        args.get("library_hints").and_then(|v| v.as_str()),
                    ).await
                }
            }
            // Step 4 (LLM half, APPLY): write an APPROVED module pair (header +
            // source), remove stale stubs, wire meson and run the compile gate.
            "hal_generate_impl_apply" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                let interface = args.get("interface").and_then(|v| v.as_str()).unwrap_or_default();
                let platform = args.get("platform").and_then(|v| v.as_str()).unwrap_or_default();
                if root.is_empty() || interface.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_generate_impl_apply: 'root', 'interface' and 'platform' are required" })
                } else {
                    self.hal_generate_apply(root, interface, platform, &args).await
                }
            }

            "hal_generate_placeholder" => {
                let summary = args
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if summary.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_generate_placeholder: 'summary' and 'platform' are required" })
                } else {
                    // header_stem from the first class line's header name if
                    // not provided (defaults to the class name, lowercased).
                    let header_stem = args
                        .get("header_stem")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            crate::build::generic_helpers::parse_hal_contract_summary(summary)
                                .first()
                                .map(|(name, _)| name.to_lowercase())
                        })
                        .unwrap_or_else(|| "hal".to_string());
                    let classes = crate::build::generic_helpers::parse_hal_contract_summary(summary);
                    if classes.is_empty() {
                        serde_json::json!({ "error": "hal_generate_placeholder: summary has no contract methods" })
                    } else {
                        let (class_name, methods) = &classes[0];
                        let source = crate::build::generic_helpers::generate_hal_placeholder_source(
                            &header_stem, class_name, methods, platform,
                        );
                        serde_json::json!({ "class_name": class_name, "source": source })
                    }
                }
            }
            // Phase D — "add target" action: for every stored contract header
            // (hal/api/*.hpp), generate a per-platform placeholder implementation
            // (hal/implementations/<plat>/<stem>_stub.cpp) via the existing
            // generator, emit the hal/meson.build `hal_impl_<plat>_sources`
            // wiring for the new platform, and re-run analysis so the
            // `missing_implementation` queue reflects the new target.
            "hal_add_target" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_add_target: 'root' (project dir) and 'platform' are required" })
                } else {
                    let api_dir = std::path::Path::new(root).join("hal").join("api");
                    let impl_dir = std::path::Path::new(root)
                        .join("hal").join("implementations").join(platform);
                    let Ok(entries) = std::fs::read_dir(&api_dir) else {
                        return serde_json::json!({ "error": format!("no hal/api found under {}", root) });
                    };
                    // Contract headers → header stem + its summary + class name.
                    let mut interfaces: Vec<(String, String, String)> = Vec::new();
                    for e in entries.flatten() {
                        let ep = e.path();
                        let Some(ext) = ep.extension().and_then(|x| x.to_str()) else { continue };
                        if ext != "hpp" {
                            continue;
                        }
                        let Ok(content) = std::fs::read_to_string(&ep) else { continue };
                        let Ok(summary) =
                            crate::build::generic_helpers::summarize_hal_header(&content)
                        else { continue };
                        let Some(stem) = ep.file_stem().and_then(|s| s.to_str()) else { continue };
                        let class_name = crate::build::generic_helpers::parse_hal_contract_summary(&summary)
                            .first()
                            .map(|(name, _)| name.clone())
                            .unwrap_or_else(|| stem.to_string());
                        interfaces.push((stem.to_string(), summary, class_name));
                    }
                    if interfaces.is_empty() {
                        serde_json::json!({ "error": format!("no valid HAL contract headers under {}", api_dir.display()) })
                    } else {
                        // Write one placeholder per interface.
                        let _ = std::fs::create_dir_all(&impl_dir);
                        let mut written: Vec<String> = Vec::new();
                        let mut failures: Vec<String> = Vec::new();
                        for (stem, summary, class_name) in &interfaces {
                            let source = crate::build::generic_helpers::generate_hal_placeholder_source(
                                stem, class_name,
                                &crate::build::generic_helpers::parse_hal_contract_summary(summary)[0].1,
                                platform,
                            );
                            let target = impl_dir.join(format!("{stem}_stub.cpp"));
                            match std::fs::write(&target, source) {
                                Ok(()) => written.push(target.display().to_string()),
                                Err(e) => failures.push(format!("{}: {}", target.display(), e)),
                            }
                        }
                        // hal/meson.build wiring: append the new platform's files(...) list.
                        let meson_path = std::path::Path::new(root).join("hal").join("meson.build");
                        let section = crate::build::generic_helpers::hal_meson_var_section(
                            platform,
                            &interfaces.iter().map(|(s, _, _)| s.clone()).collect::<Vec<_>>(),
                        );
                        let mut meson_status = "wired".to_string();
                        if let Ok(existing) = std::fs::read_to_string(&meson_path) {
                            let _ = std::fs::write(&meson_path, format!("{existing}\n{section}"));
                        } else {
                            let _ = std::fs::create_dir_all(meson_path.parent().unwrap());
                            match std::fs::write(&meson_path, &section) {
                                Ok(()) => {}
                                Err(e) => meson_status = format!("meson write failed: {e}"),
                            }
                        }
                        // Re-analyze so the missing-implementation queue reflects
                        // the new target (the analyzer lists placeholders as
                        // implementations — real ones fill them via Stage 1).
                        let analysis_status;
                        match self.analyze_project(std::path::Path::new(root)).await {
                            Ok(_) => analysis_status = "re-analyzed (queued impls ready)".to_string(),
                            Err(e) => analysis_status = format!("re-analyze failed: {e}"),
                        }
                        serde_json::json!({
                            "platform": platform,
                            "interfaces": interfaces.iter().map(|(s, _, _)| s.clone()).collect::<Vec<_>>(),
                            "placeholders_written": written,
                            "failures": failures,
                            "meson": meson_status,
                            "analysis": analysis_status,
                        })
                    }
                }
            }

            // Project-level "add platform": scaffold a FULL new platform target
            // into an existing HAL project. Deterministic + offline (no LLM):
            //   • <plat>/meson.build + <plat>/main.cpp templated from an existing
            //     non-host platform (so the new target mirrors the project's real
            //     build wiring — compiled against toolkit_sources + hal sources)
            //   • hal/implementations/<plat>/<stem>_stub.cpp per contract interface,
            //     each carrying the SPIRE-HAL-STUB sentinel + #pragma message so the
            //     coverage/fill queue surfaces it as "needs implementation"
            //   • hal/meson.build   → append hal_impl_<plat>_sources
            //   • root meson.build  → append subdir('<plat>')
            //   • meson_options.txt → append <plat> to "Valid values"
            //   • re-analyze so the new domain + build target appear
            "hal_add_platform" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_add_platform: 'root' (project dir) and 'platform' (registry id) are required" })
                } else {
                    let root_path = std::path::Path::new(root);
                    let root_meson = root_path.join("meson.build");
                    let Ok(root_content) = std::fs::read_to_string(&root_meson) else {
                        return serde_json::json!({ "error": format!("no meson.build found under {}", root) });
                    };

                    // 1. Project name + existing platform subdirs (template =
                    // first non-host subdir, preserving its real build wiring).
                    let project_name = regex::Regex::new(r#"project\s*\(\s*['"]([^'"]+)['"]"#)
                        .unwrap()
                        .captures(&root_content)
                        .and_then(|c| c.get(1))
                        .map(|m| m.as_str().to_string())
                        .unwrap_or_else(|| "app".to_string());
                    let subdir_re = regex::Regex::new(r#"subdir\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
                    let existing: Vec<String> = subdir_re
                        .captures_iter(&root_content)
                        .filter_map(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
                        .filter(|s| !s.is_empty() && !s.starts_with(".."))
                        .collect();
                    let template = existing.iter().find(|p| *p != "toolkit" && *p != "hal" && *p != "host" && *p != "subprojects").cloned();
                    let Some(template) = template else {
                        return serde_json::json!({ "error": "hal_add_platform: no existing platform subdir to use as a template" });
                    };
                    if existing.iter().any(|p| p == platform) || root_path.join(platform).exists()
                        || root_path.join("hal/implementations").join(platform).exists()
                    {
                        return serde_json::json!({ "error": format!("platform '{platform}' is already present in this project") });
                    }
                    if spire_core::build_types::Platform::from_registry(platform).is_none() {
                        return serde_json::json!({ "error": format!("platform '{platform}' not in registry (~/.spire/platforms)") });
                    }

                    // 2. Contract headers → (stem, summary, class_name).
                    let api_dir = root_path.join("hal/api");
                    let mut interfaces: Vec<(String, String, String)> = Vec::new();
                    if api_dir.is_dir() {
                        if let Ok(entries) = std::fs::read_dir(&api_dir) {
                            for e in entries.flatten() {
                                let ep = e.path();
                                let Some(ext) = ep.extension().and_then(|x| x.to_str()) else { continue };
                                if ext != "hpp" && ext != "h" {
                                    continue;
                                }
                                let Ok(content) = std::fs::read_to_string(&ep) else { continue };
                                let Ok(summary) = crate::build::generic_helpers::summarize_hal_header(&content)
                                else { continue };
                                let Some(stem) = ep.file_stem().and_then(|s| s.to_str()) else { continue };
                                let class_name = crate::build::generic_helpers::parse_hal_contract_summary(&summary)
                                    .first()
                                    .map(|(name, _)| name.clone())
                                    .unwrap_or_else(|| stem.to_string());
                                interfaces.push((stem.to_string(), summary, class_name));
                            }
                        }
                    }

                    // 3. Placeholder stubs (SPIRE-HAL-STUB sentinel + #pragma).
                    let impl_dir = root_path.join("hal/implementations").join(platform);
                    let _ = std::fs::create_dir_all(&impl_dir);
                    let mut written: Vec<String> = Vec::new();
                    let mut failures: Vec<String> = Vec::new();
                    for (stem, summary, class_name) in &interfaces {
                        let methods = &crate::build::generic_helpers::parse_hal_contract_summary(summary)[0].1;
                        let src = crate::build::generic_helpers::generate_hal_placeholder_source(
                            stem, class_name, methods, platform,
                        );
                        let target = impl_dir.join(format!("{stem}_stub.cpp"));
                        match std::fs::write(&target, src) {
                            Ok(()) => written.push(target.display().to_string()),
                            Err(e) => failures.push(format!("{}: {}", target.display(), e)),
                        }
                    }

                    // 4. hal/meson.build wiring.
                    let meson_path = root_path.join("hal/meson.build");
                    let mut meson_status = "wired".to_string();
                    let stems: Vec<String> = interfaces.iter().map(|(s, _, _)| s.clone()).collect();
                    let section = crate::build::generic_helpers::hal_meson_var_section(platform, &stems);
                    if let Ok(existing) = std::fs::read_to_string(&meson_path) {
                        if !existing.contains(&format!("hal_impl_{platform}_sources")) {
                            let _ = std::fs::write(&meson_path, format!("{existing}\n{section}"));
                        }
                    } else {
                        let _ = std::fs::create_dir_all(meson_path.parent().unwrap());
                        match std::fs::write(&meson_path, &section) {
                            Ok(()) => {}
                            Err(e) => meson_status = format!("meson write failed: {e}"),
                        }
                    }

                    // 5. Root meson.build: subdir('<plat>').
                    let mut root_status = "wired".to_string();
                    if !root_content.contains(&format!("subdir('{platform}')")) {
                        let mut updated = root_content.clone();
                        updated.push_str(&format!("\nsubdir('{platform}')\n"));
                        if let Err(e) = std::fs::write(&root_meson, &updated) {
                            root_status = format!("root meson write failed: {e}");
                        }
                    }

                    // 6. meson_options.txt: add <plat> to "Valid values".
                    let mut options_status = "wired".to_string();
                    let options_path = root_path.join("meson_options.txt");
                    if let Ok(opts) = std::fs::read_to_string(&options_path) {
                        let re = regex::Regex::new(r"(?i)(Valid values\s*:\s*)([A-Za-z0-9_ ,\-]+)").unwrap();
                        if let Some(caps) = re.captures(&opts) {
                            let mut values: Vec<String> = caps.get(2).unwrap().as_str()
                                .split(',')
                                .map(|t| t.trim().to_string())
                                .filter(|t| !t.is_empty())
                                .collect();
                            if !values.contains(&platform.to_string()) {
                                values.push(platform.to_string());
                                let new_line = format!("{}{}", &caps[1], values.join(", "));
                                let updated = re.replace(&opts, new_line.as_str()).to_string();
                                if let Err(e) = std::fs::write(&options_path, &updated) {
                                    options_status = format!("options write failed: {e}");
                                }
                            }
                        }
                    }

                    // 7. <plat>/meson.build + <plat>/main.cpp templated from the
                    // template platform (substitute the platform id; all shared
                    // toolkit/hal variables are inherited at meson scope).
                    let plat_dir = root_path.join(platform);
                    let _ = std::fs::create_dir_all(&plat_dir);
                    let mut plat_status = "wired".to_string();
                    let template_meson = root_path.join(&template).join("meson.build");
                    if let Ok(tmpl) = std::fs::read_to_string(&template_meson) {
                        let templated = tmpl.replace(&template, platform);
                        if let Err(e) = std::fs::write(plat_dir.join("meson.build"), &templated) {
                            plat_status = format!("platform meson write failed: {e}");
                        }
                    } else {
                        let minimal = format!(
                            "cpp = meson.get_compiler('cpp')\n\
                             core_deps = []\n\
                             platform_deps = []\n\
                             executable('{project_name}-{platform}',\n\
                               'main.cpp',\n\
                               dependencies: core_deps + platform_deps)\n"
                        );
                        if let Err(e) = std::fs::write(plat_dir.join("meson.build"), &minimal) {
                            plat_status = format!("platform meson write failed: {e}");
                        }
                    }
                    let main_cpp = format!(
                        "#include <iostream>\nint main() {{\n    std::cout << \"Hello from {project_name}-{platform}!\" << std::endl;\n    return 0;\n}}\n"
                    );
                    if let Err(e) = std::fs::write(plat_dir.join("main.cpp"), &main_cpp) {
                        plat_status = format!("platform main write failed: {e}");
                    }

                    // 8. Re-analyze so the new domain/build target + fill queue
                    // reflect the new platform.
                    let analysis_status;
                    match self.analyze_project(root_path).await {
                        Ok(_) => analysis_status = "re-analyzed (platform added)".to_string(),
                        Err(e) => analysis_status = format!("re-analyze failed: {e}"),
                    }

                    serde_json::json!({
                        "platform": platform,
                        "templated_from": template,
                        "interfaces": interfaces.iter().map(|(s, _, _)| s.clone()).collect::<Vec<_>>(),
                        "stubs_written": written,
                        "failures": failures,
                        "hal_meson": meson_status,
                        "root_meson": root_status,
                        "options": options_status,
                        "platform_wiring": plat_status,
                        "analysis": analysis_status,
                        "needs_fill": interfaces.iter().map(|(s, _, _)| format!("{s}: SPIRE-HAL-STUB pending")).collect::<Vec<_>>(),
                    })
                }
            }

            // Step 3 — missing-implementation queue: compute AST coverage fresh
            // from disk (contract pure-virtual method set vs each platform's
            // out-of-class definitions). Returns the per-interface platform
            // lists (backward-compatible `missing` shape) PLUS the per-perf
            // platform/interface function gaps for the UI + LLM fill action.
            "hal_missing_impls" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_missing_impls: 'root' (project dir) is required" })
                } else {
                    let coverage = crate::build::generic_helpers::hal_platform_coverage_map(
                        std::path::Path::new(root),
                    );
                    let (_by_platform, by_interface) =
                        crate::build::generic_helpers::flatten_hal_coverage(&coverage);
                    // Encode per-platform × interface gaps: {implemented, missing, drifted}.
                    let mut platforms: std::collections::BTreeMap<
                        String,
                        std::collections::BTreeMap<String, serde_json::Value>,
                    > = std::collections::BTreeMap::new();
                    for (plat, ifaces) in &coverage {
                        let mut m: std::collections::BTreeMap<String, serde_json::Value> =
                            std::collections::BTreeMap::new();
                        for (iface, cov) in ifaces {
                            let kind = if cov.implemented {
                                "implemented"
                            } else if cov.has_impl {
                                "partial"
                            } else {
                                "none"
                            };
                            let missing_sigs: Vec<serde_json::Value> = cov
                                .missing_sigs
                                .iter()
                                .map(|m| {
                                    serde_json::json!({
                                        "name": m.name,
                                        "return_type": m.return_type,
                                        "params": m.params,
                                    })
                                })
                                .collect();
                            m.insert(
                                iface.clone(),
                                serde_json::json!({
                                    "implemented": cov.implemented,
                                    "has_impl": cov.has_impl,
                                    "is_stub": cov.is_stub,
                                    "kind": kind,
                                    "missing": cov.missing,
                                    "missing_sigs": missing_sigs,
                                    "drifted": cov.drifted,
                                }),
                            );
                        }
                        platforms.insert(plat.clone(), m);
                    }
                    serde_json::json!({
                        "missing": by_interface,
                        "platforms": platforms,
                    })
                }
            }

            // Plan-then-apply HAL gap filling (plan is read-only; apply
            // executes an approved plan).
            "hal_fill_plan" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let interfaces: Vec<String> = args
                    .get("interfaces")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_fill_plan: \"root\" required" })
                } else {
                    crate::actors::hal_fill::plan(std::path::Path::new(&root), &platform, &interfaces)
                }
            }

            "hal_fill_apply" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let plan = args.get("plan").cloned();
                if root.is_empty() || plan.is_none() {
                    serde_json::json!({ "error": "hal_fill_apply: \"root\" and \"plan\" required" })
                } else {
                    let analyze = async {
                        let r = self
                            .analyze_project(std::path::Path::new(&root))
                            .await;
                        r.map(|_| ())
                    };
                    crate::actors::hal_fill::apply(
                        std::path::Path::new(&root),
                        plan.as_ref().unwrap(),
                        Box::pin(analyze),
                    ).await
                }
            }

            "hal_diff_contracts" => {
                let old_summary = args
                    .get("old_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new_summary = args
                    .get("new_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if old_summary.is_empty() || new_summary.is_empty() {
                    serde_json::json!({ "error": "hal_diff_contracts: 'old_summary' and 'new_summary' are required" })
                } else {
                    let change = crate::build::generic_helpers::diff_hal_contracts(old_summary, new_summary);
                    serde_json::json!({
                        "added": change.added,
                        "removed": change.removed,
                        "changed": change.changed,
                    })
                }
            }
            "cpp_syntax_check" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
                match std::fs::read_to_string(path) {
                    Ok(content) => serde_json::to_value(
                        crate::build::generic_helpers::cpp_syntax_check(&content)
                    ).unwrap_or(serde_json::json!({"error": "cpp_syntax_check serialization"})),
                    Err(e) => serde_json::json!({"error": format!("read {}: {e}", path)}),
                }
            }
            "hal_fix_prompt" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
                let issues = crate::build::generic_helpers::hal_doc_lint_file(
                    std::path::Path::new(root), path);
                let content = std::fs::read_to_string(path).unwrap_or_default();
                serde_json::json!({
                    "issues": issues,
                    "prompt": crate::build::generic_helpers::hal_doc_fix_prompt_whole(path, &content, &issues),
                })
            }
            "hal_state" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                serde_json::to_value(
                    crate::build::generic_helpers::compute_hal_state(std::path::Path::new(root))
                ).unwrap_or(serde_json::json!({"error": "hal_state serialization"}))
            }
            "hal_doc_lint" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                serde_json::to_value(
                    crate::build::generic_helpers::hal_doc_lint(std::path::Path::new(root))
                ).unwrap_or(serde_json::json!({"error": "hal_doc_lint serialization"}))
            }
            "hal_docs" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                let r = std::path::Path::new(root);
                serde_json::to_value(crate::build::generic_helpers::hal_report(r))
                    .unwrap_or(serde_json::json!({"error": "hal_docs serialization"}))
            }
            "hal_verify" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                let r = std::path::Path::new(root);
                serde_json::to_value(crate::build::generic_helpers::hal_verify(r))
                    .unwrap_or(serde_json::json!({"error": "hal_verify serialization"}))
            }
            "hal_sanity_check" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_sanity_check: 'root' is required" })
                } else {
                    let report = crate::build::hal_migration::hal_sanity_check(std::path::Path::new(root));
                    serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({ "error": "report serialization" }))
                }
            }
            "hal_migrate_plan" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_migrate_plan: 'root' is required" })
                } else {
                    match crate::build::hal_migration::migrate_hal_plan(std::path::Path::new(root)) {
                        Ok(plan) => serde_json::to_value(plan).unwrap_or_else(|_| serde_json::json!({ "error": "plan serialization" })),
                        Err(e) => serde_json::json!({ "error": e }),
                    }
                }
            }
            "hal_migrate_apply" => {
                let root = args.get("root").and_then(|v| v.as_str()).unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_migrate_apply: 'root' is required" })
                } else {
                    let plan_value = args.get("plan").cloned().unwrap_or(serde_json::Value::Null);
                    let plan: crate::build::hal_migration::HalMigrationPlan = match serde_json::from_value(plan_value) {
                        Ok(p) => p,
                        Err(e) => return serde_json::json!({ "error": format!("invalid plan: {e}") }),
                    };
                    match crate::build::hal_migration::migrate_hal_apply(std::path::Path::new(root), &plan) {
                        Ok(res) => serde_json::to_value(res).unwrap_or_else(|_| serde_json::json!({ "error": "result serialization" })),
                        Err(e) => serde_json::json!({ "error": e }),
                    }
                }
            }
            other => serde_json::json!({ "error": format!("Unknown build tool: {other}") }),
        }
    }

    /// Persist the latest build/test/lint status under a per-target graph
    /// config key `build.last.<path>.<target>` (or `build.last.<path>` when no
    /// target was selected) so the Build detail tab can show
    /// "Last build succeeded/failed" + duration per platform. Builds run per
    /// platform (rpi5/rock3c), so results MUST NOT overwrite each other.
    async fn store_build_status(
        &self,
        path: &str,
        target: Option<&str>,
        success: bool,
        duration_secs: f64,
    ) -> Result<(), String> {
        self.store_build_status_with_output(path, target, success, duration_secs, "")
            .await
    }

    async fn store_build_status_with_output(
        &self,
        path: &str,
        target: Option<&str>,
        success: bool,
        duration_secs: f64,
        output: &str,
    ) -> Result<(), String> {
        let key = match target {
            Some(t) if !t.trim().is_empty() => format!("build.last.{}.{}", path, t),
            _ => format!("build.last.{}", path),
        };
        let value = serde_json::json!({
            "path": path,
            "success": success,
            "duration_secs": duration_secs,
            "timestamp": Utc::now().to_rfc3339(),
            "output": output,
        });
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::SetConfig {
                key,
                value,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("MemoryGraph channel closed: {e}"))?;
        rx.await
            .map_err(|e| format!("MemoryGraph response lost: {e}"))?
            .map_err(|e| format!("MemoryGraph store failed: {e}"))?;
        Ok(())
    }

    /// Resolve the normalized `build_spec` for the selected build target from
    /// stored analysis. When the caller selected a concrete target (name or
    /// platform), the matching `BuildTarget.build_spec` is returned so the
    /// module executes it directly; otherwise `None` (existing per-tool logic).
    fn resolve_build_spec(metadata: &BuildMetadata, opts: &BuildOptions) -> Option<spire_core::build_types::BuildSpec> {
        let target_matches = |t: &spire_core::build_types::BuildTarget| {
            if let Some(tname) = &opts.target {
                if !tname.is_empty() && t.name == *tname {
                    return true;
                }
            }
            if let Some(plat) = &opts.platform {
                if !plat.is_empty() && t.platform == *plat {
                    return true;
                }
            }
            false
        };
        metadata
            .targets
            .iter()
            .find(|t| target_matches(t))
            .and_then(|t| t.build_spec.clone())
    }


    /// The unified build tools exposed to the LLM.
    fn list_tools() -> Vec<spire_core::actors::ToolInfo> {
        vec![
            spire_core::actors::ToolInfo {
                name: "build_analyze".to_string(),
                description: "Analyze a project directory using its detected build system (Cargo, npm, Maven, CMake, etc.) and return structured metadata.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string", "description": "Project directory path" } },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_build".to_string(),
                description: "Build a project directory using its detected build system. Requires prior build_analyze.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project directory path" },
                        "mode": { "type": "string", "description": "Build mode: debug (default) or release" },
                        "package": { "type": "string", "description": "Optional workspace member/package name (Cargo: --package)" },
                        "platform": { "type": "string", "description": "Optional cross-platform target (e.g. host/rpi5) selecting build-<platform> Meson dir" },
                        "target": { "type": "string", "description": "Optional specific build target (e.g. Meson executable name like 'myapp-rpi') passed to meson compile -C <dir> <target>" }
                    },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_test".to_string(),
                description: "Run tests for a project directory using its detected build system. Requires prior build_analyze.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project directory path" },
                        "filter": { "type": "string", "description": "Optional test name filter" }
                    },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_scaffold".to_string(),
                description: "Generate a minimal build-config + source stub for a new project via the registered build module (templates live with each language module). Returns build_file, build_content, source_file, source_content; caller writes the files.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project_name": { "type": "string", "description": "Project/package name" },
                        "build_system": { "type": "string", "description": "Build system label or language, e.g. Cargo, Python, SwiftPM, Meson" },
                        "goal": { "type": "string", "description": "Natural-language goal for the scaffold" }
                    },
                    "required": ["project_name", "build_system"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_list_modules".to_string(),
                description: "List the registered build modules and their capabilities (config files, languages).".to_string(),
                input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            },
            spire_core::actors::ToolInfo {
                name: "build_fix".to_string(),
                description: "Apply auto-fixes for warnings/errors (cargo fix --allow-dirty) via the language module.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project directory path" },
                        "language": { "type": "string", "description": "Language/module to route to" }
                    },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_dependency_docs".to_string(),
                description: "Fetch documentation (Markdown) for a dependency package via the language module (crates.io for Rust, npm registry, PyPI, etc.).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Dependency package name" },
                        "version": { "type": "string", "description": "Optional package version" },
                        "language": { "type": "string", "description": "Language/module to route to (e.g. Rust, JavaScript, Swift)" }
                    },
                    "required": ["name"]
                }),
            },
        ]
    }
}

#[async_trait]
impl Actor for BuildManagerActor {
    type Message = BuildManagerMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildManagerMessage::AddModule {
                capability,
                module_tx,
            } => {
                self.add_module(capability, module_tx);
            }

            BuildManagerMessage::AnalyzeProject { path, reply_to } => {
                let result = self.analyze_project(&path).await;
                let _ = reply_to.send(result);
            }

            BuildManagerMessage::BuildProject {
                path,
                opts,
                reply_to,
            } => {
                let result = self.build_project(&path, &opts).await;
                let _ = reply_to.send(result);
            }

            BuildManagerMessage::TestProject {
                path,
                opts,
                reply_to,
            } => {
                let result = self.test_project(&path, &opts).await;
                let _ = reply_to.send(result);
            }

            BuildManagerMessage::GetAnalysis { path, reply_to } => {
                let result = self.get_analysis(&path).await;
                let _ = reply_to.send(result);
            }

            BuildManagerMessage::ListModules { reply_to } => {
                let _ = reply_to.send(self.capabilities.clone());
            }

            BuildManagerMessage::CallTool {
                tool_name,
                args,
                reply_to,
            } => {
                let result = self.call_tool(&tool_name, args).await;
                let _ = reply_to.send(result);
            }

            BuildManagerMessage::CallModuleTool {
                build_system,
                tool_name,
                args,
                reply_to,
            } => {
                let result = self.call_module_tool(&build_system, &tool_name, args).await;
                let _ = reply_to.send(result);
            }

            BuildManagerMessage::SetEventTx { event_tx } => {
                self.event_tx = Some(event_tx);
            }

            BuildManagerMessage::SetLlm { llm_tx } => {
                self.llm_tx = Some(llm_tx);
            }


            BuildManagerMessage::ScaffoldBuildConfig {
                project_name,
                goal,
                build_file,
                platforms,
                structure,
                embedded,
                reply_to,
            } => {
                if let Some(tx) = self.router.get(&build_file).cloned() {
                    let (t, r) = oneshot::channel();
                    let _ = tx
                        .send(crate::build::BuildModuleMessage::ScaffoldBuildConfig {
                            project_name,
                            goal,
                            platforms,
                            structure: structure
                                .unwrap_or(spire_core::build_types::ProjectStructure::Native),
                            embedded,
                            reply_to: t,
                        })
                        .await;
                    match r.await {
                        Ok(result) => { let _ = reply_to.send(result); }
                        Err(e) => { let _ = reply_to.send(Err(format!("scaffold response lost: {}", e))); }
                    }
                } else {
                    let _ = reply_to.send(Err(format!(
                        "no build module owns config file '{}'",
                        build_file
                    )));
                }
            }

            BuildManagerMessage::ListTools { reply_to } => {
                let _ = reply_to.send(Self::list_tools());
            }

            BuildManagerMessage::ParseAndStoreSourceFile {
                file_path,
                reply_to,
            } => {
                let result = self.parse_and_store_source_file(&file_path).await;
                let _ = reply_to.send(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::CargoBuildModule;
    use crate::Actor;
    
    use spire_actor::ServiceRegistry;
    use std::sync::Arc;

    /// Serializes tests that mutate the PROCESS-GLOBAL `SPIRE_PLATFORM_DIR`
    /// env var (the platform registry seed). Cargo runs tests in parallel, so
    /// two registry-dependent tests must never set it concurrently — the last
    /// writer would break the other's `Platform::from_registry` lookup.
    /// Shared across the crate via `crate::PLATFORM_DIR_TEST_LOCK` so the
    /// cargo/meson scaffold tests (readers) serialize against these writers.

    /// Sets `SPIRE_PLATFORM_DIR` for a fixture and restores the ambient value
    /// on drop — a stale var pointing at a deleted fixture dir would break any
    /// registry-reading test that runs later in this process.
    struct SpirePlatformDirGuard {
        previous: Option<String>,
    }
    impl SpirePlatformDirGuard {
        fn set(dir: impl AsRef<std::path::Path>) -> Self {
            let previous = std::env::var("SPIRE_PLATFORM_DIR").ok();
            std::env::set_var("SPIRE_PLATFORM_DIR", dir.as_ref());
            Self { previous }
        }
    }
    impl Drop for SpirePlatformDirGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(prev) => std::env::set_var("SPIRE_PLATFORM_DIR", prev),
                None => std::env::remove_var("SPIRE_PLATFORM_DIR"),
            }
        }
    }

    #[test]
    fn find_config_file_detects_known_names() {
        let mut manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        manager.add_module(
            ModuleCapability {
                name: "cargo".to_string(),
                config_files: vec!["Cargo.toml".to_string()],
                build_system: "Cargo".to_string(),
                language: "Rust".to_string(),
                source_extensions: vec!["rs".to_string()],
            mcp_servers: vec![],
            },
            mpsc::channel(1).0,
        );

        // Direct file path
        assert_eq!(
            manager.find_config_file(Path::new("Cargo.toml")),
            Some("Cargo.toml".to_string())
        );
        // Unknown file
        assert!(manager.find_config_file(Path::new("README.md")).is_none());
    }

    #[tokio::test]
    async fn analyze_routes_to_cargo_module() {
        let system = spire_actor::ActorSystem::new();
        let _registry = Arc::new(ServiceRegistry::new());
        let (mg_tx, _mg_rx) = mpsc::channel(1);

        // Spawn the cargo module + build manager.
        let (cargo_tx, _cargo_handle) = {
            let (tx, rx) = mpsc::channel::<BuildModuleMessage>(8);
            CargoBuildModule::new().spawn(rx);
            (tx, ())
        };
        let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(
            mg_tx,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        // Register the module (as the FFI bootstrap would).
        let (t, r) = oneshot::channel();
        cargo_tx
            .send(BuildModuleMessage::DescribeCapabilities { reply_to: t })
            .await
            .unwrap();
        let cap = r.await.unwrap();
        bm_tx
            .send(BuildManagerMessage::AddModule {
                capability: cap,
                module_tx: cargo_tx,
            })
            .await
            .unwrap();

        // List modules — should contain the cargo module.
        let (t, r) = oneshot::channel();
        bm_tx
            .send(BuildManagerMessage::ListModules { reply_to: t })
            .await
            .unwrap();
        let modules = r.await.unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].name, "cargo");
    }

    #[tokio::test]
    async fn extension_router_registers_source_extensions() {
        let mut manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        manager.add_module(
            ModuleCapability {
                name: "cargo".to_string(),
                config_files: vec!["Cargo.toml".to_string()],
                build_system: "Cargo".to_string(),
                language: "Rust".to_string(),
                source_extensions: vec!["rs".to_string()],
            mcp_servers: vec![],
            },
            mpsc::channel(1).0,
        );

        assert!(manager.extension_router.contains_key("rs"));
        assert!(!manager.extension_router.contains_key("py"));
    }

    /// Phase A: HAL contract JSON tools must return the validated summary,
    /// placeholder source (for "add target"), and contract diff over the
    /// same `call_tool`/`tools/call` surface the Swift wizard uses.
    #[tokio::test]
    async fn hal_contract_tools_validate_generate_and_diff() {
        let manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );

        // 1. Validate a valid abstract-class contract. Canonical HAL contracts
        // declare a virtual destructor (`virtual ~X() = default;`) — the
        // extractor requires it (a header without one is an edge case).
        let header = r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#;
        let valid = manager
            .call_tool("hal_validate_contract", serde_json::json!({ "content": header }))
            .await;
        assert_eq!(valid["valid"], serde_json::json!(true), "valid: {valid}");
        let summary = valid["summary"].as_str().expect("summary field");
        assert!(summary.contains("CameraHAL"), "summary: {summary}");
        assert!(summary.contains("start"), "summary must list start(): {summary}");

        // 2. Reject a non-abstract header.
        let bad = manager
            .call_tool(
                "hal_validate_contract",
                serde_json::json!({ "content": "class NotAbstract { public: void do_thing(); };" }),
            )
            .await;
        assert_eq!(bad["valid"], serde_json::json!(false), "bad: {bad}");
        assert!(bad["error"].as_str().is_some(), "error field");

        // 3. Generate a per-platform placeholder from the summary.
        let placeholder = manager
            .call_tool(
                "hal_generate_placeholder",
                serde_json::json!({ "summary": summary, "platform": "rpi5", "header_stem": "camera_hal" }),
            )
            .await;
        let source = placeholder["source"].as_str().expect("source");
        assert!(source.contains("CameraHAL::start"), "placeholder: {placeholder}\nsource: {source}");
        assert!(source.contains("/* TODO: implement for rpi5 */"), "placeholder: {placeholder}\nsource: {source}");

        // 4. Diff two contract summaries → added + changed.
        let diff = manager
            .call_tool(
                "hal_diff_contracts",
                serde_json::json!({
                    "old_summary": "CameraHAL: bool start() = 0; std::uint32_t capture(int timeout_ms) = 0",
                    "new_summary": "CameraHAL: bool start() = 0; bool teardown() = 0; std::uint32_t capture(int timeout_ms, int mode) = 0",
                }),
            )
            .await;
        assert_eq!(diff["added"][0], serde_json::json!("teardown"), "diff: {diff}");
        assert!(
            diff["changed"].as_array().map(|a| a.iter().any(|p| p[0] == "capture")).unwrap_or(false),
            "diff must flag capture sig change: {diff}"
        );
    }

    /// Phase E step 2: `hal_write_contract` must validate-then-persist the
    /// approved header (Stage-0 gate — invalid contract never touches disk).
    #[tokio::test]
    async fn hal_write_contract_persists_valid_header_only() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        let manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );

        // Valid canonical contract → written to hal/api/camera_hal.hpp.
        let header = r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#;
        let result = manager
            .call_tool(
                "hal_write_contract",
                serde_json::json!({
                    "root": root.to_string_lossy().to_string(),
                    "filename": "camera_hal",
                    "content": header,
                }),
            )
            .await;
        assert_eq!(result["valid"], serde_json::json!(true), "result: {result}");
        let written = root.join("hal/api/camera_hal.hpp");
        assert!(written.exists(), "contract must be persisted");
        assert_eq!(std::fs::read_to_string(&written).unwrap(), header);
        assert!(result["summary"].as_str().unwrap().contains("CameraHAL"));

        // Non-abstract header → rejected, nothing written.
        let bad = manager
            .call_tool(
                "hal_write_contract",
                serde_json::json!({
                    "root": root.to_string_lossy().to_string(),
                    "filename": "not_abstract",
                    "content": "class NotAbstract { public: void do_thing(); };",
                }),
            )
            .await;
        assert_eq!(bad["valid"], serde_json::json!(false), "bad: {bad}");
        assert!(!root.join("hal/api/not_abstract.hpp").exists());
    }

    /// Step 4 (deterministic half): `hal_build_impl_prompt` must resolve the
    /// contract header + a registry platform record and produce the Stage-1
    /// constrained prompt (contract + hardware profile + meson build gate).
    /// Uses the real rpi5.yaml seed via a fixture SPIRE_PLATFORM_DIR.
    #[tokio::test]
    async fn hal_build_impl_prompt_resolves_contract_and_registry_platform() {
        use tempfile::tempdir;

        // Serialize against the OTHER test that mutates the process-global
        // SPIRE_PLATFORM_DIR env var (they must never interleave).
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();

        // Fixture platform registry mirroring the real seed layout.
        let reg = tempdir().unwrap();
        let seed = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".spire").join("platforms").join("rpi5.yaml");
        if !seed.exists() {
            // Skip silently when the seed is absent (fresh environments).
            return;
        }
        std::fs::create_dir_all(reg.path().join("platforms")).unwrap();
        std::fs::copy(&seed, reg.path().join("platforms/rpi5.yaml")).unwrap();
        let _restore = SpirePlatformDirGuard::set(reg.path().join("platforms"));

        // Project with the approved contract header.
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#,
        )
        .unwrap();

        let manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        let result = manager
            .call_tool(
                "hal_build_impl_prompt",
                serde_json::json!({
                    "root": root.to_string_lossy().to_string(),
                    "interface": "camera_hal",
                    "platform": "rpi5",
                }),
            )
            .await;

        let prompt = result["prompt"].as_str().expect("prompt");
        assert!(prompt.contains("CameraHAL"), "must embed the contract class: {prompt}");
        assert!(prompt.contains("CameraHalRpi5"), "must embed the concrete impl class: {prompt}");
        assert!(result["class_name"].as_str() == Some("CameraHalRpi5"), "class_name: {result:?}");
        assert!(
            prompt.contains("meson compile -C build-rpi5 camera_hal-rpi5"),
            "must embed the per-target gate: {prompt}"
        );
        // The deterministic clean header must declare the concrete derived
        // class (module pair) and must NOT carry the pending-stub sentinel.
        let header = result["header"].as_str().expect("header");
        assert!(
            header.contains("class CameraHalRpi5 : public CameraHAL"),
            "header must declare the derived pair: {header}"
        );
        assert!(
            !header.contains(crate::build::generic_helpers::SPIRE_HAL_STUB_SENTINEL),
            "clean header must not carry the stub sentinel: {header}"
        );
    }

    /// Step 4 (LLM half, unconfigured branch): `hal_generate_impl` must reject
    /// cleanly when the LLM sender is not attached (fresh BuildManagerActor) —
    /// no panic, no partial writes, a clear "LLM not configured" error.
    #[tokio::test]
    async fn hal_generate_impl_requires_configured_llm() {
        let manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        let result = manager
            .call_tool(
                "hal_generate_impl",
                serde_json::json!({
                    "root": "/tmp/hal",
                    "interface": "camera_hal",
                    "platform": "rpi5",
                }),
            )
            .await;
        let err = result["error"].as_str().expect("error field");
        assert!(
            err.contains("LLM not configured"),
            "must reject when LLM is unconfigured: {err}"
        );
    }

    /// Step 3: `hal_missing_impls` must return an empty queue without stored
    /// analysis (no panic), and the `missing_implementation` message format it
    /// parses is already pinned by the Meson analyzer's container-layout test
    /// ("HAL interface {stem} has no implementation for platform {plat}").
    #[tokio::test]
    async fn hal_missing_impls_returns_empty_queue_without_analysis() {
        let manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        let result = manager
            .call_tool(
                "hal_missing_impls",
                serde_json::json!({ "root": "/tmp/does-not-exist" }),
            )
            .await;
        // No stored analysis → empty map, no error, no panic.
        assert_eq!(result["missing"], serde_json::json!({}), "result: {result}");
    }

    /// Phase D round-trip: `hal_add_target` must discover the contract header
    /// (hal/api/camera_hal.hpp), write a per-platform placeholder
    /// (hal/implementations/rpi5/camera_hal_stub.cpp), and emit the
    /// hal/meson.build `hal_impl_rpi5_sources` wiring — the "add target"
    /// path the wizard triggers. (The final re-analyze step is best-effort;
    /// the assertion focuses on the deterministic file+wiring payload.)
    #[tokio::test]
    async fn hal_add_target_generates_placeholders_and_meson_wiring() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#,
        )
        .unwrap();

        let manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        let result = manager
            .call_tool(
                "hal_add_target",
                serde_json::json!({
                    "root": root.to_string_lossy().to_string(),
                    "platform": "rpi5",
                }),
            )
            .await;

        // Placeholder written for the discovered interface.
        let placeholder = root.join("hal/implementations/rpi5/camera_hal_stub.cpp");
        assert!(
            placeholder.exists() && !result["error"].is_string(),
            "placeholder must exist, result: {result}"
        );
        let src = std::fs::read_to_string(&placeholder).unwrap();
        assert!(src.contains("#include \"camera_hal.hpp\""), "src: {src}");
        assert!(src.contains("CameraHAL::start"), "src: {src}");
        assert!(src.contains("/* TODO: implement for rpi5 */"), "src: {src}");

        // Meson wiring for the new platform's source list.
        let meson = std::fs::read_to_string(root.join("hal/meson.build")).unwrap();
        assert!(
            meson.contains("hal_impl_rpi5_sources = files(") && meson.contains("'implementations/rpi5/camera_hal_stub.cpp'"),
            "meson wiring: {meson}"
        );
    }

    /// `hal_fill_apply` must generate stubs directly from the contract AST even
    /// when methods have MULTI-LINE parameter lists (e.g. ai-traps
    /// `video_scaler.hpp`: `scale_nv12(...)` wraps onto a second line). The
    /// old `summarize_hal_header` → `parse_hal_contract_summary` string
    /// round-trip was line-based and produced "video_scaler: no classes".
    #[tokio::test]
    async fn hal_fill_apply_handles_multiline_contract_params() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        // Mirrors hal/api/video_scaler.hpp exactly: multi-line params.
        std::fs::write(
            root.join("hal/api/video_scaler.hpp"),
            "struct IVideoScaler {\npublic:\n    virtual ~IVideoScaler() = default;\n    virtual bool scale_nv12(int src_fd, int src_w, int src_h,\n                            int dst_fd, int dst_w, int dst_h) = 0;\n};\n",
        )
        .unwrap();
        std::fs::write(root.join("hal/meson.build"), "hal_impl_rpi5_sources = files()\n").unwrap();

        // Plan → apply (none → whole-class stub).
        let plan = crate::actors::hal_fill::plan(root, "rpi5", &[]);
        let items = plan["plan"].as_array().expect("plan items");
        assert_eq!(items.len(), 1, "one video_scaler item: {plan}");
        let analyze = async { Ok(()) };
        let result = crate::actors::hal_fill::apply(
            root,
            &serde_json::json!(items),
            Box::pin(analyze),
        )
        .await;
        assert!(
            result["failures"].as_array().unwrap().is_empty(),
            "no 'no classes' failure: {result}"
        );

        // The module-pair definition for a NEW class ("none") lands at
        // `video_scaler_rpi5.cpp` (the concrete derived class, not the contract
        // abstract name), plus its `.hpp` declaration — written atomically.
        let stub = std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.cpp")).unwrap();
        assert!(stub.contains("SPIRE-HAL-STUB"), "sentinel: {stub}");
        assert!(stub.contains("#pragma message("), "pragma: {stub}");
        assert!(stub.contains("VideoScalerRpi5::scale_nv12("), "stub body: {stub}");
        for tok in ["src_fd", "src_w", "src_h", "dst_fd", "dst_w", "dst_h"] {
            assert!(stub.contains(tok), "missing param {tok}: {stub}");
        }
        // The concrete declaration header exists, derived from the contract
        // base (`IVideoScaler`) and declaring the same class name.
        let header = std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.hpp")).unwrap();
        assert!(header.contains("class VideoScalerRpi5"), "header: {header}");
        assert!(header.contains("IVideoScaler"), "derives contract base: {header}");
        assert!(header.contains("SPIRE-HAL-STUB"), "pair sentinel: {header}");
    }

    /// `hal_fill_apply` must write a stub that carries the SPIRE-HAL-STUB
    /// sentinel + `#pragma message` for BOTH fill kinds: `none` (whole new
    /// class via the placeholder generator) and `partial` (existing class,
    /// only the missing methods). The coverage/fill queue then reports the
    /// file as "needs implementation" instead of "implemented".
    #[tokio::test]
    async fn hal_fill_apply_stubs_carry_spire_hal_stub_for_none_and_partial() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        // Contract (CameraHAL: start + capture) + a partial existing impl
        // that already provides `start` so the missing set is `capture` only.
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            "class CameraHAL {\npublic:\n    virtual ~CameraHAL() = default;\n    virtual bool start() = 0;\n    virtual std::uint32_t capture(int timeout_ms) = 0;\n};\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/api/video_scaler.hpp"),
            "class VideoScaler {\npublic:\n    virtual ~VideoScaler() = default;\n    virtual bool resize(int w, int h) = 0;\n};\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/implementations/rpi5/camera_hal_rpi5.cpp"),
            "bool CameraHalRpi5::start() { return true; }\n",
        )
        .unwrap();
        std::fs::write(root.join("hal/meson.build"), "hal_impl_rpi5_sources = files()\n").unwrap();

        // Plan: rpi5 → camera_hal (partial, missing capture) + video_scaler
        // (none → whole-class stub).
        let plan = crate::actors::hal_fill::plan(root, "rpi5", &[]);
        let items = plan["plan"].as_array().expect("plan items");
        assert_eq!(items.len(), 2, "one partial + one none: {plan}");

        let analyze = async { Ok(()) };
        let result = crate::actors::hal_fill::apply(
            root,
            &serde_json::json!(items),
            Box::pin(analyze),
        )
        .await;
        assert!(result["failures"].as_array().unwrap().is_empty(), "{result}");

        // Partial gap stub: sentinel + pragma + the missing method body only.
        let gap = std::fs::read_to_string(root.join("hal/implementations/rpi5/camera_hal_gap.cpp")).unwrap();
        assert!(gap.contains("SPIRE-HAL-STUB"), "partial sentinel: {gap}");
        assert!(gap.contains("#pragma message("), "partial pragma: {gap}");
        assert!(gap.contains("CameraHAL::capture(int timeout_ms)"), "partial body: {gap}");
        assert!(!gap.contains("CameraHAL::start()"), "partial must not re-add start: {gap}");

        // Whole-class module pair: the definition (sentinel + pragma + every
        // contract method, using the CONCRETE derived class name from the new
        // naming scheme) plus the `.hpp` declaration — written atomically.
        let stub = std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.cpp")).unwrap();
        assert!(stub.contains("SPIRE-HAL-STUB"), "none sentinel: {stub}");
        assert!(stub.contains("#pragma message("), "none pragma: {stub}");
        assert!(stub.contains("VideoScalerRpi5::resize(int w, int h)"), "none body: {stub}");
        let header = std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.hpp")).unwrap();
        assert!(header.contains("class VideoScalerRpi5"), "none header: {header}");
        assert!(header.contains("VideoScaler"), "derives contract base: {header}");

        // The coverage/fill queue must report BOTH as still needing
        // implementation (the sentinel marks them as stubs, not implemented).
        let cov = crate::build::generic_helpers::hal_interface_coverage(
            &[
                crate::build::generic_helpers::HalContractMethod {
                    name: "start".into(), return_type: "bool".into(), params: "".into(),
                },
                crate::build::generic_helpers::HalContractMethod {
                    name: "capture".into(), return_type: "std::uint32_t".into(), params: "int timeout_ms".into(),
                },
            ],
            "camera_hal",
            &root.join("hal/implementations/rpi5"),
        );
        assert!(!cov.implemented, "partial stub must still be unimplemented: {cov:?}");
    }

    /// Project-level "add platform" round-trip: `hal_add_platform` must scaffold
    /// the FULL new platform surface into an existing HAL project — <plat>/
    /// (meson.build + main.cpp), per-contract `SPIRE-HAL-STUB` placeholders,
    /// hal/meson.build wiring, root subdir('<plat>'), and meson_options.txt —
    /// then (best-effort) re-analyze. Uses the real registry seed rpi5.yaml via
    /// a fixture SPIRE_PLATFORM_DIR.
    #[tokio::test]
    async fn hal_add_platform_scaffolds_full_platform_surface() {
        use tempfile::tempdir;

        // Serialize against the OTHER test that mutates the process-global
        // SPIRE_PLATFORM_DIR env var (they must never interleave).
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();

        // Fixture platform registry mirroring the real seed layout.
        let reg = tempdir().unwrap();
        let seed = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".spire").join("platforms").join("rpi5.yaml");
        if !seed.exists() {
            // Skip silently when the seed is absent (fresh environments).
            return;
        }
        std::fs::create_dir_all(reg.path().join("platforms")).unwrap();
        std::fs::copy(&seed, reg.path().join("platforms/rpi5.yaml")).unwrap();
        let _restore = SpirePlatformDirGuard::set(reg.path().join("platforms"));

        // Fake existing HAL project: root meson.build with subdir('rpi5'),
        // contract header, hal/meson.build wiring, template rpi5/meson.build,
        // meson_options.txt with "Valid values: host, rpi5".
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("meson.build"),
            "project('ai-traps', ['c', 'cpp'])\nsubdir('hal')\nsubdir('toolkit')\nsubdir('rpi5')\n",
        )
        .unwrap();
        std::fs::write(
            root.join("meson_options.txt"),
            "option('platform', type: 'string', value: 'host',\n  description: 'Target platform. Valid values: host, rpi5')\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::write(
            root.join("hal/meson.build"),
            "hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_rpi5.cpp')\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("rpi5")).unwrap();
        std::fs::write(
            root.join("rpi5/meson.build"),
            r#"cpp = meson.get_compiler('cpp')
rpi5_hal_sources = hal_impl_rpi5_sources
executable('ai-trap-rpi5', 'main.cpp' + rpi5_hal_sources, dependencies: core_deps + platform_deps)
"#,
        )
        .unwrap();
        std::fs::write(root.join("rpi5/main.cpp"), "int main() { return 0; }\n").unwrap();

        let manager = BuildManagerActor::new(
            mpsc::channel(1).0,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        );
        let result = manager
            .call_tool(
                "hal_add_platform",
                serde_json::json!({
                    "root": root.to_string_lossy().to_string(),
                    "platform": "rpi5",
                }),
            )
            .await;
        // rpi5 is already present → rejected before any write.
        assert!(
            result["error"].as_str().map(|e| e.contains("already present")).unwrap_or(false),
            "must reject an already-present platform, result: {result}"
        );

        // Add rock3c — a SECOND registry platform so the scaffolding path
        // (not the reject path) is exercised. The YAML must declare its own
        // `id: rock3c` (a copy of rpi5.yaml would still parse id=rpi5).
        std::fs::write(
            reg.path().join("platforms/rock3c.yaml"),
            "id: rock3c\nname: Rock 3C\nos: linux\narchitecture:\n  cpu_family: aarch64\n  cpu: armv8-a\n  endian: little\n  target_triple: aarch64-linux-gnu\ntoolchain:\n  c: clang\n  cpp: clang++\n  ar: llvm-ar\n  strip: llvm-strip\nsysroot:\n  root: /tmp/rock3c-sysroot\n  lib_dirs:\n    - ${SYSROOT}/usr/lib/aarch64-linux-gnu\n",
        )
        .unwrap();
        let result = manager
            .call_tool(
                "hal_add_platform",
                serde_json::json!({
                    "root": root.to_string_lossy().to_string(),
                    "platform": "rock3c",
                }),
            )
            .await;
        assert!(
            !result["error"].is_string(),
            "rock3c add must succeed, result: {result}"
        );
        assert_eq!(result["interfaces"][0], serde_json::json!("camera_hal"), "{result}");

        // 1. Placeholder stub with the SPIRE-HAL-STUB sentinel + #pragma.
        let stub = root.join("hal/implementations/rock3c/camera_hal_stub.cpp");
        assert!(stub.exists(), "stub must exist: {result}");
        let src = std::fs::read_to_string(&stub).unwrap();
        assert!(src.contains("SPIRE-HAL-STUB"), "sentinel: {src}");
        assert!(src.contains("#pragma message("), "pragma: {src}");

        // 2. hal/meson.build wiring.
        let hal_meson = std::fs::read_to_string(root.join("hal/meson.build")).unwrap();
        assert!(
            hal_meson.contains("hal_impl_rock3c_sources = files(")
                && hal_meson.contains("'implementations/rock3c/camera_hal_stub.cpp'"),
            "hal meson wiring: {hal_meson}"
        );

        // 3. Root meson.build subdir('rock3c').
        let root_meson = std::fs::read_to_string(root.join("meson.build")).unwrap();
        assert!(root_meson.contains("subdir('rock3c')"), "root meson: {root_meson}");

        // 4. meson_options.txt Valid values include rock3c.
        let opts = std::fs::read_to_string(root.join("meson_options.txt")).unwrap();
        assert!(
            opts.to_lowercase().contains("valid values: host, rpi5, rock3c"),
            "options: {opts}"
        );

        // 5. <plat>/meson.build templated from rpi5 (id substituted).
        let plat_meson = std::fs::read_to_string(root.join("rock3c/meson.build")).unwrap();
        assert!(plat_meson.contains("hal_impl_rock3c_sources"), "plat meson: {plat_meson}");

        // 6. <plat>/main.cpp.
        assert!(
            std::fs::read_to_string(root.join("rock3c/main.cpp")).unwrap().contains("Hello from ai-traps-rock3c"),
            "main.cpp missing"
        );
    }

    #[tokio::test]
    async fn parse_source_file_routes_to_cargo_module() {
        use tempfile::tempdir;
        use std::io::Write;

        let dir = tempdir().unwrap();
        let file_path = dir.path().join("main.rs");
        let mut f = std::fs::File::create(&file_path).unwrap();
        f.write_all(
            b"use std::collections::HashMap;\n\npub fn main() {\n    let mut map = HashMap::new();\n    map.insert(\"hello\", 1);\n}\n",
        )
        .unwrap();
        f.flush().unwrap();

        let system = spire_actor::ActorSystem::new();
        let _registry = Arc::new(ServiceRegistry::new());

        // This test exercises the module-level parsing pipeline: spawn the
        // cargo module via the spire-modules Actor trait and verify
        // ParseSourceFile returns structured AST nodes. (Graph persistence is
        // tested via the FFI / integration layer where a real MemoryGraphActor
        // is available.)
        let (mg_tx, _mg_rx) = mpsc::channel(8);

        let (cargo_tx, _cargo_handle) = {
            let (tx, rx) = mpsc::channel::<BuildModuleMessage>(8);
            CargoBuildModule::new().spawn(rx);
            (tx, ())
        };
        let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(
            mg_tx,
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            std::sync::Arc::new(tokio::sync::Notify::new()),
        ));

        // Register the module.
        let (t, r) = oneshot::channel();
        cargo_tx
            .send(BuildModuleMessage::DescribeCapabilities { reply_to: t })
            .await
            .unwrap();
        let cap = r.await.unwrap();
        bm_tx
            .send(BuildManagerMessage::AddModule {
                capability: cap,
                module_tx: cargo_tx.clone(),
            })
            .await
            .unwrap();

        // Call ParseSourceFile directly on the module and verify AST nodes.
        let (t, r) = oneshot::channel();
        cargo_tx
            .send(BuildModuleMessage::ParseSourceFile {
                file_path: file_path.clone(),
                reply_to: t,
            })
            .await
            .unwrap();
        let parse_result = r.await.unwrap().unwrap();
        assert_eq!(parse_result.language, "Rust");
        assert!(!parse_result.nodes.is_empty(), "Parser produced no nodes");
        assert!(
            parse_result.nodes.iter().any(|n| n.node_type == "function"),
            "Expected at least one function node"
        );
        assert!(
            parse_result.nodes.iter().any(|n| n.node_type == "import"),
            "Expected at least one import node"
        );

        // Verify content_hash is a SHA-256 hex string (64 chars).
        assert_eq!(parse_result.content_hash.len(), 64);
    }
}
