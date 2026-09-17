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
use spire_core::actors::Actor;
use spire_core::models::memory_graph::{
    AttrNode, NodeUpdate, RelationshipInput, RelationshipType, StreamOp, StreamOpResult,
    TransactionRequest,
};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::subsystems::llm::llm::{LlmMessage, LlmModelRole};

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
        datatype_docs_to_prompt_text, extract_contract_methods_cpp, extract_cpp_base_classes,
        generate_hal_impl_prompt_pair, generate_hal_module_header_clean, hal_docs_to_prompt_text,
        hal_platform_library_hints, parse_hal_docs, resolve_semantic_hal_impl_names,
        summarize_hal_header,
    };
    // Contract (binding gate) — an invalid header never reaches the LLM.
    let header = std::path::Path::new(root)
        .join("hal")
        .join("api")
        .join(format!("{interface}.hpp"));
    let content =
        std::fs::read_to_string(&header).map_err(|e| format!("read {}: {e}", header.display()))?;
    let summary = summarize_hal_header(&content)
        .map_err(|e| format!("contract {} invalid: {e}", header.display()))?;
    let classes = extract_contract_methods_cpp(&content);
    let methods: Vec<_> = classes.iter().flat_map(|(_, ms)| ms.clone()).collect();
    if methods.is_empty() {
        return Err(format!(
            "contract {} has no pure-virtual methods",
            header.display()
        ));
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
    let plat = crate::platform::Platform::from_registry(platform)
        .ok_or_else(|| format!("platform '{platform}' not in registry (~/.spire/platforms)"))?;
    // Module-pair naming: semantic mode REUSES `<iface>_<plat>` when the name
    // is free or only holds a scaffold stub (so Fix replaces stubs instead of
    // piling up `_2` siblings); real implementations are never overwritten.
    let impl_dir = std::path::Path::new(root)
        .join("hal")
        .join("implementations")
        .join(platform);
    let (class_name, cpp_name, hpp_name) =
        resolve_semantic_hal_impl_names(interface, platform, &impl_dir);
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
        &summary,
        interface,
        &class_name,
        &base_class,
        &plat.id,
        &plat.name,
        &hardware_profile,
        &hints,
        &format!("{interface}-{platform}"),
        &impl_header,
        &contract_docs,
        &datatype_docs,
        "",
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

/// Ensure a generated implementation `.cpp` includes its OWN declaration header
/// (`<stem>_<platform>.hpp`).
///
/// The module pair splits the declaration (deterministic `.hpp`, where the
/// concrete class is declared) from the implementation (LLM `.cpp`). Models
/// routinely open the `.cpp` with the *contract* header only, leaving the
/// concrete class undeclared — the file then fails to compile with
/// `use of undeclared identifier '<Class>'`. Prepending the impl-header include
/// is deterministic and model-independent, and idempotent.
fn ensure_impl_header_include(source: &str, hpp_name: &str) -> String {
    if hpp_name.is_empty() {
        return source.to_string();
    }
    let needle = format!("#include \"{hpp_name}\"");
    if source.contains(&needle) {
        source.to_string()
    } else {
        format!("{needle}\n\n{source}")
    }
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
    /// Register a module as the handler for a **platform `os`** (e.g. `"esp-idf"`), in
    /// preference to whichever module owns the config file.
    ///
    /// Separate from `AddModule` on purpose. The config router assumes one config file maps to
    /// exactly one module, and that breaks for a platform-specific build system: an ESP32
    /// project is *also* a `Cargo.toml` project, so a module claiming that file would silently
    /// **replace** the cargo module and send every Rust project in Spire down the ESP32 path.
    /// Declaring the platform routes on what actually differs instead.
    ///
    /// A module registered this way claims **no** config files.
    AddPlatformModule {
        os: String,
        /// What the module declared it can do — carried here because it decides whether a
        /// `flash` request is routed to it or refused (see `PlatformModule`).
        capability: ModuleCapability,
        module_tx: mpsc::Sender<BuildModuleMessage>,
    },
    /// Analyze a project → produce + store BuildMetadata.
    AnalyzeProject {
        path: PathBuf,
        /// The specific config file to analyze (e.g. "Cargo.toml") when the
        /// directory holds several build configs (e.g. a Cargo workspace root
        /// that also carries a Makefile). `None` → the manager detects a single
        /// config in the directory (see `find_config_file`).
        config_file: Option<String>,
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
    SetLlm { llm_tx: mpsc::Sender<LlmMessage> },
    /// Attach the actor-owned build-event log (drain-safe while a streaming
    /// build/lint/fix occupies this actor's mailbox).
    SetEventLog {
        event_log_tx: mpsc::Sender<BuildEventLogMessage>,
    },
}

/// The BuildManager actor — routes build/analysis requests to static modules.
pub struct BuildManagerActor {
    /// Router: config filename → module sender.
    router: HashMap<String, mpsc::Sender<BuildModuleMessage>>,
    /// Router: source file extension → module sender (for AST parsing).
    extension_router: HashMap<String, mpsc::Sender<BuildModuleMessage>>,
    /// Router: platform `os` → module sender, for builds whose selected platform names one.
    ///
    /// Consulted *before* [`Self::router`]: a platform-specific module wins over the module
    /// that owns the config file, because the platform is what the invocation actually
    /// depends on (an ESP32 build is still a `Cargo.toml` project).
    ///
    /// The module's capability is stored with its sender: it is what gates the one operation
    /// only a platform module can perform (`flash`), and a missing capability would have to be
    /// re-asked for on every request.
    platform_router: HashMap<String, PlatformModule>,
    /// Capabilities of all registered modules.
    capabilities: Vec<ModuleCapability>,
    /// Sender to the MemoryGraph actor for analysis state.
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
    /// Optional broadcast sender for pushing BuildEvents to the UI event stream.
    event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    /// Actor-owned build-event log sender (set via `SetEventLog`). Streaming
    /// forwarders push incremental lines to this log actor instead of a shared
    /// `Arc<Mutex<Vec>>`, so the FFI can drain while builds/lints/fixes occupy
    /// this actor's mailbox.
    event_log_tx: Option<mpsc::Sender<BuildEventLogMessage>>,
    /// Optional LLM sender (Stage-1 implementation generation).
    llm_tx: Option<mpsc::Sender<LlmMessage>>,
}

/// Which registered module should handle a build.
///
/// The decision is its own type so it can be asserted in a test and named in a log, rather
/// than being an implicit `.or_else()` a reader has to infer.
#[derive(Debug, PartialEq, Eq)]
enum BuildRoute {
    /// A module that declared itself the handler for this platform `os`.
    Platform(String),
    /// The module that owns the config file — the pre-existing behaviour.
    Config(String),
}

/// A module registered by the platform `os` it serves, with the capability that gates it.
///
/// One value rather than two maps: a platform module is only usable together with what it
/// declared it can do, so they are registered and looked up as a unit.
struct PlatformModule {
    capability: ModuleCapability,
    module_tx: mpsc::Sender<BuildModuleMessage>,
}

/// The `os` of a registered platform, or `None` when the id is unknown.
///
/// A free function so the routing decision does not depend on the actor, which is what makes
/// `route_for` testable against a temporary platform directory.
fn platform_os_of(platform_id: &str) -> Option<String> {
    crate::platform::Platform::from_registry(platform_id).map(|p| p.os)
}

impl BuildManagerActor {
    pub fn new(memory_graph_tx: mpsc::Sender<MemoryGraphMessage>) -> Self {
        Self {
            router: HashMap::new(),
            extension_router: HashMap::new(),
            platform_router: HashMap::new(),
            capabilities: Vec::new(),
            memory_graph_tx,
            event_tx: None,
            event_log_tx: None,
            llm_tx: None,
        }
    }

    /// Attach the LLM sender (Stage-1 HAL implementation generation).
    pub fn set_llm(&mut self, llm_tx: mpsc::Sender<LlmMessage>) {
        self.llm_tx = Some(llm_tx);
    }

    /// Attach the actor-owned build-event log.
    pub fn set_event_log(&mut self, event_log_tx: mpsc::Sender<BuildEventLogMessage>) {
        self.event_log_tx = Some(event_log_tx);
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
            capability.name,
            capability.config_files,
            capability.source_extensions
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

    /// Register a platform-specific module (see `AddPlatformModule`).
    fn add_platform_module(
        &mut self,
        os: String,
        capability: ModuleCapability,
        module_tx: mpsc::Sender<BuildModuleMessage>,
    ) {
        tracing::info!(
            "BuildManager: module '{}' registered for platform os '{}'",
            capability.name,
            os
        );
        self.platform_router.insert(
            os,
            PlatformModule {
                capability,
                module_tx,
            },
        );
    }

    /// Which registered module should handle a build of `config` under `platform`.
    ///
    /// The decision is its own type so it can be asserted in a test and named in a log,
    /// rather than being an implicit `.or_else()` someone has to infer.
    fn route_for(&self, config: &str, platform: Option<&str>) -> BuildRoute {
        if let Some(os) = platform.and_then(platform_os_of) {
            if self.platform_router.contains_key(&os) {
                return BuildRoute::Platform(os);
            }
        }
        BuildRoute::Config(config.to_string())
    }

    fn sender_for(&self, route: &BuildRoute) -> Option<&mpsc::Sender<BuildModuleMessage>> {
        match route {
            BuildRoute::Platform(os) => self.platform_router.get(os).map(|m| &m.module_tx),
            BuildRoute::Config(config) => self.router.get(config),
        }
    }

    /// The module a build-like request for `config` should go to, honouring platform routing.
    ///
    /// **The single decision point.** Nine call sites used to repeat this lookup and its error
    /// message by hand, which is how a routing change ends up applied to eight of them — and
    /// the shadowing bug this replaced was exactly a site that had not been updated.
    ///
    /// `platform` is `None` on the paths that have not been threaded yet: they keep the config
    /// router's behaviour exactly as it was, and upgrading one is now a one-line change here
    /// rather than an edit at every call site.
    fn module_tx_for(
        &self,
        config: &str,
        platform: Option<&str>,
    ) -> Result<&mpsc::Sender<BuildModuleMessage>, String> {
        let route = self.route_for(config, platform);
        self.sender_for(&route).ok_or_else(|| match &route {
            BuildRoute::Platform(os) => format!("No module registered for platform '{}'", os),
            BuildRoute::Config(config) => format!("No module registered for {}", config),
        })
    }

    /// The route a `flash` request takes, refused unless the module that would handle it
    /// declares `supports_flash`.
    ///
    /// Flash is the one operation only a *platform* module can perform — the chip and the USB
    /// tool are platform facts, and no config file implies them. So a project whose platform
    /// routes to its config file's owner is refused here by name (a Raspberry Pi is still
    /// Cargo), rather than handed to a module whose answer would arrive as a lost channel.
    fn route_for_flash(&self, config: &str, platform: &str) -> Result<BuildRoute, String> {
        let route = self.route_for(config, Some(platform));
        match &route {
            BuildRoute::Platform(os) => {
                let module = self
                    .platform_router
                    .get(os)
                    .ok_or_else(|| format!("No module registered for platform '{}'", os))?;
                if !module.capability.supports_flash {
                    return Err(format!(
                        "flash is not supported for build system {}",
                        module.capability.build_system
                    ));
                }
                Ok(route)
            }
            // No platform module claimed this platform's `os`, so nothing here flashes. Naming
            // the `os` is what tells the user whether the id was wrong or the board simply has
            // no flash step.
            BuildRoute::Config(_) => Err(match platform_os_of(platform) {
                Some(os) => format!(
                    "flash is not supported for platform '{}' (no module flashes os '{}')",
                    platform, os
                ),
                None => format!("unknown platform '{}'", platform),
            }),
        }
    }

    /// Find which registered config file applies to a path.
    /// For a file path, use its basename. For a directory, look for a known
    /// config file inside it.
    /// Deterministic config-file priority so a directory holding several
    /// build configs (e.g. a Cargo workspace root that also carries a
    /// Makefile wrapper) resolves to the same primary system every time.
    const CONFIG_PRIORITY: &[&str] = &[
        "Cargo.toml",
        "Package.swift",
        "meson.build",
        "package.json",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "CMakeLists.txt",
        "Makefile",
        "go.mod",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
    ];

    fn find_config_file(&self, path: &Path) -> Option<String> {
        if path.is_file() {
            let name = path.file_name()?.to_str()?.to_string();
            return self.router.contains_key(&name).then_some(name);
        }
        // Deterministic priority first…
        for config in Self::CONFIG_PRIORITY {
            if self.router.contains_key(*config) && path.join(config).exists() {
                return Some(config.to_string());
            }
        }
        // …then any remaining registered config in stable sorted order.
        let mut keys: Vec<&String> = self.router.keys().collect();
        keys.sort();
        for config in keys {
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
            Ok(Ok(nodes)) => nodes
                .into_iter()
                .find(|n| n.name() == path || n.get("path").and_then(|v| v.as_str()) == Some(path)),
            _ => None,
        }
    }

    /// The scope root diagnostics are recorded under: the nearest ancestor of
    /// `path` that looks like a project (`.git` / `.spire`), else `path` itself.
    ///
    /// The UI passes a SUBPROJECT path to Build/Lint/Verify, while diagnostics are
    /// stored with absolute file paths under the project root — so matching the
    /// raw argument would leave the previous run's diagnostics behind and the
    /// Build tab would keep showing errors that no longer reproduce.
    fn diagnostics_scope_root(path: &Path) -> PathBuf {
        let mut dir = path.to_path_buf();
        loop {
            if dir.join(".git").exists() || dir.join(".spire").exists() {
                return dir;
            }
            if !dir.pop() {
                return path.to_path_buf();
            }
        }
    }

    /// Delete the previous run's Diagnostic nodes of `build_type` for
    /// `project_root`, inside the caller's open transaction stream.
    ///
    /// Diagnostics are merged by a stable `file:line:message` name, so a finding
    /// that no longer reproduces would otherwise stay in the graph forever and
    /// keep showing up in the Build tab. That is exactly how the bogus
    /// cross-header errors from pre-fix lint runs outlived the fix (the panel
    /// reads `project/diagnostics`, which has no "latest run" notion of its own).
    ///
    /// Membership is decided by `diagnostic_in_project`, which accepts the
    /// build-dir-relative paths compilers actually print — see its note.
    async fn clear_previous_diagnostics(
        &self,
        stream: &mpsc::Sender<TransactionRequest>,
        build_type: &str,
        project_root: &Path,
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Diagnostic".to_string()),
                subtype: None,
                name: None,
                limit: Some(5000),
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("MemoryGraph channel closed: {e}"))?;
        let nodes = rx
            .await
            .map_err(|e| format!("MemoryGraph response lost: {e}"))?
            .map_err(|e| format!("Diagnostic query failed: {e}"))?;

        let root = Self::diagnostics_scope_root(project_root)
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        for node in nodes {
            // Only this project's diagnostics of this kind — never another
            // project's, and never another run kind (build vs lint vs fix).
            let same_kind = node.get("build_type").and_then(|v| v.as_str()) == Some(build_type);
            let in_project = node
                .get("file")
                .and_then(|v| v.as_str())
                .map(|file| Self::diagnostic_in_project(file, &root))
                .unwrap_or(false);
            if same_kind && in_project {
                Self::send_stream_op(stream, StreamOp::DeleteNode(node.id().to_string())).await?;
            }
        }
        Ok(())
    }

    /// Persist a run's BuildEvents as Diagnostic graph nodes linked to their
    /// SourceFile nodes via HasDiagnostic edges. Each run is tagged with a
    /// build_run_id.
    ///
    /// A run SUPERSEDES the previous run of the same kind for this project (see
    /// [`Self::clear_previous_diagnostics`]), so the Build tab always reflects
    /// the latest run rather than an accumulation of every past one.
    async fn ingest_diagnostics(
        &self,
        events: &[serde_json::Value],
        build_type: &str,
        project_root: &Path,
    ) -> Result<(), String> {
        // Collect events that carry a file path — these become Diagnostic nodes.
        let diag_events: Vec<&serde_json::Value> = events
            .iter()
            .filter(|ev| {
                ev.get("file").and_then(|f| f.as_str()).is_some()
                    && ev.get("level").and_then(|l| l.as_str()).is_some()
            })
            .collect();

        let run_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::OpenTransactionStream { reply_to: tx })
            .await
            .map_err(|e| format!("MemoryGraph channel closed: {e}"))?;
        let stream = rx
            .await
            .map_err(|e| format!("MemoryGraph response lost: {e}"))?;

        // Supersede the previous run of this kind — deliberately BEFORE the
        // "nothing to ingest" return, so a run that is now clean clears the
        // findings it used to report instead of leaving them on screen.
        self.clear_previous_diagnostics(&stream, build_type, project_root)
            .await?;
        if diag_events.is_empty() {
            Self::send_stream_op(&stream, StreamOp::Commit).await?;
            return Ok(());
        }

        for ev in diag_events {
            let file = ev["file"].as_str().unwrap_or("").to_string();
            let level = ev["level"].as_str().unwrap_or("info").to_string();
            let message = ev["message"]
                .as_str()
                .unwrap_or_else(|| ev["line"].as_str().unwrap_or(""))
                .to_string();
            let line = ev["line_number"].as_u64().map(|v| v as u32);
            let mut props = HashMap::new();
            props.insert(
                "message".to_string(),
                serde_json::Value::String(message.clone()),
            );
            props.insert("file".to_string(), serde_json::Value::String(file.clone()));
            if let Some(line) = line {
                props.insert("line".to_string(), serde_json::json!(line));
            }
            props.insert("severity".to_string(), serde_json::Value::String(level));
            props.insert(
                "build_type".to_string(),
                serde_json::Value::String(build_type.to_string()),
            );
            props.insert(
                "build_run_id".to_string(),
                serde_json::Value::String(run_id.clone()),
            );

            // Stable name so MergeNode upserts by (Diagnostic, name).
            let name = format!(
                "{}:{}:{}",
                file,
                line.map(|l| l.to_string()).unwrap_or_default(),
                message
            );
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
                StreamOpResult::NodeStored(n) | StreamOpResult::NodeUpdated(n) => {
                    n.id().to_string()
                }
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
            if existing.get("content_hash").and_then(|v| v.as_str())
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
    async fn analyze_project(
        &self,
        path: &Path,
        config_file: Option<&str>,
    ) -> Result<BuildMetadata, String> {
        // Prefer the explicitly requested config file (the discovery already
        // knows which one it found); fall back to directory detection.
        let config = match config_file {
            Some(cf) if self.router.contains_key(cf) => cf.to_string(),
            _ => self
                .find_config_file(path)
                .ok_or_else(|| format!("No known build config found for {}", path.display()))?,
        };
        // Through the same single decision point as `build_project`. Platform threading for
        // this path is a one-line change here once its `opts` is plumbed.
        let module_tx = self.module_tx_for(&config, None)?;

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
                path_str,
                e
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
        // A platform-specific module wins over the config file's owner: the platform is what
        // the invocation actually depends on, and an ESP32 build is still a Cargo.toml project.
        let module_tx = self.module_tx_for(&config, opts.platform.as_deref())?;

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
        // The platform decides the module here too — `build_build` is the path a UI Build click
        // and a `build_build` tool call share, so leaving `platform` out of the lookup sent an
        // ESP32 build to the cargo module while the batch `build_project` above sent it to the
        // platform module. Two paths, two answers, for the same request.
        let module_tx = self.module_tx_for(&config, opts.platform.as_deref())?;

        let (tx, rx) = oneshot::channel();
        let (build_event_tx, mut build_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::build::BuildEvent>();
        let events_buf =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let event_log = self.event_log_tx.clone();
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
                    // Incremental: forward to the actor-owned build-event log
                    // (the FFI drains it while this actor's mailbox is busy).
                    if let Some(event_log_tx) = event_log.as_ref() {
                        let _ = event_log_tx.send(BuildEventLogMessage::Record(json)).await;
                    }
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
        let output = rx
            .await
            .map_err(|e| format!("Module response lost: {}", e))?;
        let events = events_buf.lock().unwrap().clone();
        output.map(|o| (o, events))
    }

    /// Reject an operation the registered module declares unsupported.
    ///
    /// Modules set `supports_*` in their `ModuleCapability`; the modules without
    /// a clean/lint/format/fix implementation leave them false, so the manager
    /// surfaces a clear error instead of forwarding to a module that would reply
    /// "not implemented".
    fn check_capability(
        &self,
        config: &str,
        op: &str,
        supported: impl Fn(&ModuleCapability) -> bool,
    ) -> Result<(), String> {
        if let Some(cap) = self
            .capabilities
            .iter()
            .find(|c| c.config_files.iter().any(|f| f == config))
        {
            if !supported(cap) {
                return Err(format!(
                    "{} is not supported for build system {}",
                    op, cap.build_system
                ));
            }
        }
        Ok(())
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
        self.check_capability(&config, "lint", |c| c.supports_lint)?;
        let module_tx = self.module_tx_for(&config, None)?;

        let (tx, rx) = oneshot::channel();
        let (build_event_tx, mut build_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::build::BuildEvent>();
        let events_buf =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let event_log = self.event_log_tx.clone();
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
                    // Incremental: forward to the actor-owned build-event log
                    // (the FFI drains it while this actor's mailbox is busy).
                    if let Some(event_log_tx) = event_log.as_ref() {
                        let _ = event_log_tx.send(BuildEventLogMessage::Record(json)).await;
                    }
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
        let output = rx
            .await
            .map_err(|e| format!("Module response lost: {}", e))?;
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
        self.check_capability(&config, "fix", |c| c.supports_fix)?;
        let module_tx = self.module_tx_for(&config, None)?;

        let (tx, rx) = oneshot::channel();
        let (build_event_tx, mut build_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::build::BuildEvent>();
        let events_buf =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let event_log = self.event_log_tx.clone();
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
                    // Incremental: forward to the actor-owned build-event log
                    // (the FFI drains it while this actor's mailbox is busy).
                    if let Some(event_log_tx) = event_log.as_ref() {
                        let _ = event_log_tx.send(BuildEventLogMessage::Record(json)).await;
                    }
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
        let output = rx
            .await
            .map_err(|e| format!("Module response lost: {}", e))?;
        let events = events_buf.lock().unwrap().clone();
        output.map(|o| (o, events))
    }

    /// Test: same pattern as build.
    async fn test_project(
        &self,
        path: &Path,
        opts: &TestOptions,
        platform: Option<String>,
    ) -> Result<BuildOutput, String> {
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
        let module_tx = self.module_tx_for(&config, None)?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Test {
                path: path.to_path_buf(),
                metadata,
                opts: opts.clone(),
                platform,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        rx.await
            .map_err(|e| format!("Module response lost: {}", e))?
    }

    /// Unified LLM tool entry point: routes build/* tools to handlers.
    /// Clean: same pattern as build — route to module via a proper message.
    async fn clean_project(
        &self,
        path: &Path,
        platform: Option<String>,
    ) -> Result<BuildOutput, String> {
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
        self.check_capability(&config, "clean", |c| c.supports_clean)?;
        let module_tx = self.module_tx_for(&config, None)?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Clean {
                path: path.to_path_buf(),
                metadata,
                platform,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Module channel closed: {}", e))?;
        rx.await
            .map_err(|e| format!("Module response lost: {}", e))?
    }

    /// Flash the built artifact for `opts.platform` onto the board — the last leg of
    /// contract → drift → fill → cross-build → **flash** → run.
    ///
    /// A platform is mandatory here, in a way it is not for a build: it names the chip the
    /// tool must talk to, and unlike a build there is no default that is safe — the host is
    /// not a board. Both the refusal and the route follow from it, so they are checked before
    /// the message is sent (`route_for_flash`) rather than after it fails to answer.
    /// Build each filled backend through its platform's module, and repair once when it fails.
    ///
    /// The fill's gate is structural: it decides whether an answer is the file it claims to be.
    /// Whether that file *builds* is a question only `cargo` answers, and the answer is worth a
    /// second call — a wrong-but-plausible vendor API is the failure mode the gate cannot see
    /// (`PinDriver<'d, MODE>` written with two generics, `gpio::Output` where the type is
    /// `FunctionSio<SioOutput>`), and the compiler's own words are what fix it.
    ///
    /// Verification is **skipped, never failed**, when Spire cannot build here: no stored analysis
    /// (nothing routes), no platform id on the item, or no module for that platform's `os`. The
    /// note says which, because "written" and "verified to build" are different claims and the
    /// result must not blur them.
    async fn verify_embedded_hal_fills(
        &self,
        root: &Path,
        plan: &serde_json::Value,
        result: &mut serde_json::Value,
    ) {
        let Some(llm_tx) = self.llm_tx.clone() else {
            self.note_build_verification(result, "skipped: no LLM to repair with");
            return;
        };
        let path_str = root.to_string_lossy().to_string();
        // Analyze on demand when the graph has nothing: the verification needs the config file and
        // the metadata, and requiring a *prior* `build_analyze` in the same process was a hidden
        // ordering requirement — the store is best-effort, so a caller that did analyze could still
        // arrive here with nothing to read (found by driving it: `build_analyze` succeeded and the
        // lookup still missed). Analyzing here is cheap and makes the tool self-contained.
        let metadata = match self.get_analysis(&path_str).await {
            Some(metadata) => metadata,
            None => match self.analyze_project(root, None).await {
                Ok(metadata) => metadata,
                Err(e) => {
                    self.note_build_verification(
                        result,
                        &format!("skipped: could not analyze the project to build it: {e}"),
                    );
                    return;
                }
            },
        };
        let Some(config) = metadata.config_files.first().cloned() else {
            self.note_build_verification(result, "skipped: the analysis names no config file");
            return;
        };

        // The tool accepts the plan either as the whole object or as just its `plan` array (that is
        // what the UI hands back), so read it the same way the apply does.
        let items = match plan {
            serde_json::Value::Array(items) => items.clone(),
            other => other
                .get("plan")
                .and_then(|p| p.as_array())
                .cloned()
                .unwrap_or_default(),
        };
        let applied: Vec<String> = result
            .get("applied")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|entry| {
                        entry.get("file").and_then(|f| f.as_str()).map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut outcomes: Vec<serde_json::Value> = Vec::new();
        for item in &items {
            let file = item
                .get("file")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !applied.contains(&file) {
                continue;
            }
            let Some(platform) = item.get("platform").and_then(|v| v.as_str()) else {
                outcomes.push(serde_json::json!({
                    "file": file,
                    "built": serde_json::Value::Null,
                    "note": "no platform id on the item — cannot choose a build",
                }));
                continue;
            };
            let package = item
                .get("crate")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            outcomes.push(
                self.build_one_backend(
                    root, &file, item, platform, &package, &config, &metadata, &llm_tx,
                )
                .await,
            );
        }

        if !outcomes.is_empty() {
            if let Some(obj) = result.as_object_mut() {
                obj.insert(
                    "build_verification".into(),
                    serde_json::Value::Array(outcomes),
                );
            }
        }
    }

    /// The last `n` lines of build output.
    ///
    /// `cargo` prints the interesting part **last** (the errors, then the summary), and a failed
    /// build for a crate with a big dependency tree can carry thousands of "Compiling …" lines
    /// before them — so the tail is the part worth reporting, and the part worth handing to a model.
    fn tail_lines(output: &str, n: usize) -> String {
        let lines: Vec<&str> = output.lines().collect();
        let start = lines.len().saturating_sub(n);
        lines[start..].join("\n")
    }

    fn note_build_verification(&self, result: &mut serde_json::Value, note: &str) {
        if let Some(obj) = result.as_object_mut() {
            obj.insert(
                "build_verification".into(),
                serde_json::Value::String(note.to_string()),
            );
        }
    }

    /// One backend, on the **verify spine**: build it, and on failure hand the compiler's errors to
    /// the model and rebuild — bounded (see `MAX_REPAIR_ROUNDS`).
    ///
    /// Returns the outcome as JSON, because "written", "repaired then built" and "still does not
    /// build" are different things and the caller's result has to say which happened. `rounds` says
    /// how many repairs it took, and `built: null` still means *nothing compiled it*.
    #[allow(clippy::too_many_arguments)]
    async fn build_one_backend(
        &self,
        root: &Path,
        file: &str,
        item: &serde_json::Value,
        platform: &str,
        package: &str,
        config: &str,
        metadata: &BuildMetadata,
        llm_tx: &mpsc::Sender<LlmMessage>,
    ) -> serde_json::Value {
        let artifact = FillArtifact {
            manager: self,
            root: root.to_path_buf(),
            file: file.to_string(),
            item: item.clone(),
            platform: platform.to_string(),
            package: package.to_string(),
            config: config.to_string(),
            metadata: metadata.clone(),
            llm_tx: llm_tx.clone(),
        };
        let outcome =
            crate::build::verify_spine::verify_generated(&artifact, MAX_REPAIR_ROUNDS).await;
        let mut json = outcome.to_json();
        // The tail, not the whole build log: `cargo` says the interesting part last, and a failed
        // build for a crate with a big dependency tree is mostly "Compiling …".
        if let Some(errors) = json.get("errors").and_then(|e| e.as_str()) {
            let kept = Self::tail_lines(errors, 40);
            json["errors"] = serde_json::json!(kept);
        }
        json["file"] = serde_json::json!(file);
        json
    }

    /// One build attempt for a backend crate, through the platform module that owns its `os`.
    ///
    /// The same message the UI's build action sends, with `package` naming the backend and
    /// `platform` choosing the toolchain — so a fill is verified by the *product's* build path
    /// rather than by a second implementation of it in a test.
    async fn build_backend_crate(
        &self,
        root: &Path,
        config: &str,
        metadata: &BuildMetadata,
        platform: &str,
        package: &str,
    ) -> Result<BuildOutput, String> {
        match self.route_for(config, Some(platform)) {
            BuildRoute::Platform(_) => {}
            // No platform module owns this platform's `os`, so nothing here knows how to build for
            // it — say so rather than sending the message to a module that would refuse it.
            BuildRoute::Config(_) => {
                return Err(format!(
                    "platform '{platform}' does not route to a platform module"
                ))
            }
        }
        let module_tx = self.module_tx_for(config, Some(platform))?;
        let opts = BuildOptions {
            mode: "debug".to_string(),
            package: Some(package.to_string()),
            platform: Some(platform.to_string()),
            target: None,
        };
        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Build {
                path: root.to_path_buf(),
                metadata: metadata.clone(),
                opts,
                build_spec: None,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("module channel closed: {e}"))?;
        rx.await.map_err(|e| format!("module response lost: {e}"))?
    }

    /// Check a just-written contract with a compiler, and attach what happened.
    ///
    /// The second generator on the **verify spine** (the fill leg was the first), and the reason the
    /// spine exists: the shape — gate, build, report — is the same, only the artifact differs. Two
    /// differences from the fill leg are deliberate:
    ///
    /// - **The gate is real here.** The fill leg gates before writing and says so; a contract is
    ///   written first and gated after, by re-reading the file that landed and parsing it — which is
    ///   the only way to catch a write that did not survive the filesystem.
    /// - **No repair rounds.** The source is the user's, typed in the sheet: there is no generator to
    ///   hand errors back to, so `max_rounds = 0` (build once, report) is the honest configuration
    ///   rather than a special case.
    ///
    /// A project Spire cannot build from (no manifest, no cargo module) is reported as `not_built`,
    /// never as a failed contract — the same distinction the spine makes everywhere.
    async fn verify_written_contract(&self, root: &Path, result: &mut serde_json::Value) {
        let Some(written) = result
            .get("written")
            .and_then(|value| value.as_str())
            .map(PathBuf::from)
        else {
            return;
        };
        // `crates/<crate>/src/hal/<stem>.rs` → the crate to build. Read from the path that was
        // written rather than re-deriving the project's naming rule a second time.
        let crate_name = written
            .strip_prefix(root.join("crates"))
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|first| first.as_os_str().to_string_lossy().to_string());
        let Some(crate_name) = crate_name else {
            result["host_build"] = serde_json::json!({
                "built": serde_json::Value::Null,
                "not_built": "the written path is not inside crates/",
            });
            return;
        };

        let artifact = ContractArtifact {
            manager: self,
            root: root.to_path_buf(),
            file: written.clone(),
            crate_name,
        };
        let outcome = crate::build::verify_spine::verify_generated(&artifact, 0).await;
        let mut json = outcome.to_json();
        if let Some(errors) = json.get("errors").and_then(|e| e.as_str()) {
            let kept = Self::tail_lines(errors, 40);
            json["errors"] = serde_json::json!(kept);
        }
        result["host_build"] = json;
    }

    /// Check a just-written contract with a compiler, and attach what happened.
    async fn flash_project(
        &self,
        path: &Path,
        opts: &BuildOptions,
        artifact: Option<PathBuf>,
        port: Option<PathBuf>,
    ) -> Result<BuildOutput, String> {
        let platform_id = opts
            .platform
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| "flashing needs a platform, e.g. \"esp32c6\"".to_string())?;

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

        self.route_for_flash(&config, platform_id)?;
        let module_tx = self.module_tx_for(&config, Some(platform_id))?;

        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Flash {
                path: path.to_path_buf(),
                metadata,
                opts: opts.clone(),
                artifact,
                port,
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
            // Longest marker first: "fatal error:" CONTAINS "error:", so matching
            // "error:" first would leave the "fatal " prefix in the path part and
            // swallow the line number — turning `../x.cpp:41:10: fatal error: …` into
            // the unresolvable file `../x.cpp:41`, which is why missing-header errors
            // could never be located (and so were silently skipped by the fix loop).
            let markers = ["fatal error:", "error:", "warning:"];
            let Some(marker) = markers.iter().find(|m| lower.contains(**m)) else {
                continue;
            };
            // Find the position of the message.
            let Some(msg_idx) = lower.find(marker) else {
                continue;
            };
            let path_part = &line[..msg_idx];
            let msg = line[msg_idx + marker.len()..].trim().to_string();
            // path_part is "<file>:<line>:<col>: " (column optional). Drop the
            // trailing separator FIRST, then read the numbers off the right — splitting
            // straight away glued the line number onto the file name, so every build
            // diagnostic pointed at a path that cannot exist (`../x.cpp:41`) and the
            // fix loop skipped all of them.
            let mut loc = path_part.trim_end();
            if let Some(stripped) = loc.strip_suffix(':') {
                loc = stripped.trim_end();
            }
            let (head, tail) = loc.rsplit_once(':').unwrap_or(("", loc));
            let (file, line_no, col) = match head.rsplit_once(':') {
                // "<file>:<line>:<col>" — two numbers, so the pair is line:column.
                Some((path, line)) if line.trim().parse::<u64>().is_ok() => (
                    path.trim().to_string(),
                    line.trim().parse::<u64>().ok(),
                    tail.trim().parse::<u64>().ok(),
                ),
                // "<file>:<line>" — a single number is the line.
                _ => (
                    head.trim().to_string(),
                    tail.trim().parse::<u64>().ok(),
                    None,
                ),
            };
            // "fatal error:" is an error, not a warning.
            let severity = if marker.contains("error") {
                "error"
            } else {
                "warning"
            };
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

    /// True when a diagnostic's recorded `file` belongs to `project_root`.
    ///
    /// Compilers print paths the way they were invoked, and Meson/ninja invoke them
    /// RELATIVE to the build directory (`../app/main.cpp`), so a relative path is
    /// this project's by definition — the memory graph is per-project. An absolute
    /// path must still sit under the project root, so a shared graph can never have
    /// another project's diagnostics deleted by mistake.
    ///
    /// Getting this wrong is what let ~137 stale BUILD errors (recorded with relative
    /// paths by an earlier platform-less build) survive every supersede and be
    /// re-reported by "Fix & Verify" as if the current rpi5 build produced them.
    fn diagnostic_in_project(file: &str, project_root: &str) -> bool {
        let file = file.trim();
        if file.is_empty() {
            return false;
        }
        if file.starts_with('/') {
            file.starts_with(project_root.trim_end_matches('/'))
        } else {
            true
        }
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
        self.check_capability(&config, "format", |c| c.supports_format)?;
        let module_tx = self.module_tx_for(&config, None)?;

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
                format!(
                    "No build module registered for build system '{}'",
                    build_system
                )
            })?;

        let module_tx = self.module_tx_for(&config, None)?;

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
        let module_tx = self.module_tx_for(&config, None)?;

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
        if let Some(err_text) = reply.get("error").and_then(|v| v.as_str()).or_else(|| {
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
        }) {
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
        let (source, syntax) = match hal_llm_generate_source(&self.llm_tx, ctx.prompt.clone()).await
        {
            Ok(pair) => pair,
            Err(e) => return serde_json::json!({ "error": e }),
        };
        // Guarantee the `.cpp` includes its own declaration header.
        let source = ensure_impl_header_include(&source, &ctx.hpp_name);
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
        let class_name = args
            .get("class_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let hpp_path = args
            .get("hpp_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let cpp_path = args
            .get("cpp_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let header = args
            .get("header")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let source = args
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if hpp_path.is_empty() || cpp_path.is_empty() || header.is_empty() || source.is_empty() {
            return serde_json::json!({ "error": "hal_generate_impl_apply: 'hpp_path', 'cpp_path', 'header' and 'source' are required" });
        }
        // Guarantee the `.cpp` includes its own declaration header (the UI may
        // pass through a source that predates the include normalization).
        let hpp_name = std::path::Path::new(hpp_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let source = ensure_impl_header_include(source, hpp_name);
        // Write the approved module pair.
        if let Some(parent) = std::path::Path::new(hpp_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return serde_json::json!({ "error": format!("create {}: {e}", parent.display()) });
            }
        }
        if let Err(e) =
            std::fs::write(hpp_path, header).and_then(|_| std::fs::write(cpp_path, source))
        {
            return serde_json::json!({ "error": format!("write pair: {e}") });
        }
        // Remove stale stubs so coverage never ORs their sentinel.
        let impl_dir = std::path::Path::new(root)
            .join("hal")
            .join("implementations")
            .join(platform);
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
            std::path::Path::new(root),
            "meson",
            &[
                "compile",
                "-C",
                &format!("build-{platform}"),
                &format!("{interface}-{platform}"),
            ],
        )
        .await
        {
            Ok(o) if o.success => "build passed".to_string(),
            Ok(o) => format!(
                "build FAILED:\n{}",
                o.output.lines().take(20).collect::<Vec<_>>().join("\n")
            ),
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
                match self.analyze_project(Path::new(path), None).await {
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
                    package: args
                        .get("package")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    platform: args
                        .get("platform")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    target: args
                        .get("target")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                };
                match self.build_project_with_events(Path::new(path), &opts).await {
                    Ok((o, mut events)) => {
                        // Persist warnings/errors as Diagnostic graph nodes so the
                        // Build tab can show them after the run. Meson/ninja put
                        // compiler warnings in o.output as plain text (the streamed
                        // events only carry module "finished" markers), so parse
                        // them into diagnostic events when none were streamed.
                        let has_file_diags = events
                            .iter()
                            .any(|e| e.get("file").and_then(|f| f.as_str()).is_some());
                        if !has_file_diags {
                            events.append(&mut Self::parse_clang_output(&o.output));
                        }
                        let _ = self
                            .ingest_diagnostics(&events, "build", Path::new(path))
                            .await;
                        // Persist the build status PER TARGET (success/duration +
                        // raw output) so building rock3c and rpi5 store separate
                        // results under the same key the platform list reads back.
                        let status_target = self
                            .resolve_status_target(
                                path,
                                opts.target.as_deref(),
                                opts.platform.as_deref(),
                            )
                            .await;
                        let _ = self
                            .store_action_status_with_output(
                                "build",
                                path,
                                status_target.as_deref(),
                                o.success,
                                o.duration_secs,
                                &o.output,
                            )
                            .await;
                        let mut val = serde_json::to_value(o)
                            .unwrap_or(serde_json::json!({"error": "serialize"}));
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
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let opts = TestOptions { filter };
                let status_target = self
                    .resolve_status_target(
                        path,
                        args.get("target").and_then(|v| v.as_str()),
                        platform.as_deref(),
                    )
                    .await;
                match self.test_project(Path::new(path), &opts, platform).await {
                    Ok(o) => {
                        // Record the test result PER TARGET as well, so the
                        // platform list shows the last test — not just builds.
                        let _ = self
                            .store_action_status(
                                "test",
                                path,
                                status_target.as_deref(),
                                o.success,
                                o.duration_secs,
                            )
                            .await;
                        serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"}))
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_flash" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let opts = BuildOptions {
                    mode: args
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    package: args
                        .get("package")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    platform: args
                        .get("platform")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    target: None,
                };
                let artifact = args
                    .get("artifact")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(PathBuf::from);
                let port = args
                    .get("port")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from);
                let status_target = self
                    .resolve_status_target(path, None, opts.platform.as_deref())
                    .await;
                match self
                    .flash_project(Path::new(path), &opts, artifact, port)
                    .await
                {
                    Ok(o) => {
                        // Recorded per target like build/test, so the platform list can show
                        // "last flashed" alongside "last built" — the two facts that decide
                        // whether the chip is running what is on disk.
                        let _ = self
                            .store_action_status_with_output(
                                "flash",
                                path,
                                status_target.as_deref(),
                                o.success,
                                o.duration_secs,
                                &o.output,
                            )
                            .await;
                        serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"}))
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_clean" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let status_target = self
                    .resolve_status_target(
                        path,
                        args.get("target").and_then(|v| v.as_str()),
                        platform.as_deref(),
                    )
                    .await;
                match self.clean_project(Path::new(path), platform).await {
                    Ok(o) => {
                        // Clean is a per-platform action too (`meson compile
                        // --clean -C build-<platform>`), so record it per target.
                        let _ = self
                            .store_action_status(
                                "clean",
                                path,
                                status_target.as_deref(),
                                o.success,
                                o.duration_secs,
                            )
                            .await;
                        serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"}))
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_lint" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                tracing::info!(
                    "build_lint: path={:?} exists={}",
                    path,
                    std::path::Path::new(path).exists()
                );
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let status_target = self
                    .resolve_status_target(
                        path,
                        args.get("target").and_then(|v| v.as_str()),
                        platform.as_deref(),
                    )
                    .await;
                match self.lint_project_streaming(Path::new(path), platform).await {
                    Ok((o, mut events)) => {
                        tracing::info!(
                            "build_lint OK: success={} out_len={} events={}",
                            o.success,
                            o.output.len(),
                            events.len()
                        );
                        // Meson lint returns clang analyzer output as plain text in
                        // `o.output` (the streaming events are usually empty), so
                        // parse the clang diagnostic lines into structured events.
                        // That lets ingest_diagnostics persist graph nodes and the
                        // UI's lint/diagnostics panel show real findings.
                        if events.is_empty() {
                            events.append(&mut Self::parse_clang_output(&o.output));
                            tracing::info!(
                                "build_lint: parsed {} diagnostics from output",
                                events.len()
                            );
                        }
                        // Persist lint findings as Diagnostic graph nodes so the
                        // Build tab shows them after the lint run.
                        let _ = self
                            .ingest_diagnostics(&events, "lint", Path::new(path))
                            .await;
                        let _ = self
                            .store_action_status(
                                "lint",
                                path,
                                status_target.as_deref(),
                                o.success,
                                o.duration_secs,
                            )
                            .await;
                        let mut val = serde_json::to_value(&o)
                            .unwrap_or(serde_json::json!({"error": "serialize"}));
                        if let serde_json::Value::Object(ref mut m) = val {
                            m.insert("buildEvents".to_string(), serde_json::json!(events));
                        }
                        val
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_verify" => {
                // Build, then (only if the build compiled) lint — the two
                // checks a developer runs together. Both results are persisted
                // per target under their own kind so the platform list shows
                // them independently.
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let opts = BuildOptions {
                    mode: args
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    package: args
                        .get("package")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    platform: args
                        .get("platform")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    target: args
                        .get("target")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                };
                let status_target = self
                    .resolve_status_target(path, opts.target.as_deref(), opts.platform.as_deref())
                    .await;

                // ── 1. Build ─────────────────────────────────────────────────
                let (build_out, mut build_events) =
                    match self.build_project_with_events(Path::new(path), &opts).await {
                        Ok(v) => v,
                        Err(e) => return serde_json::json!({ "error": e }),
                    };
                let has_file_diags = build_events
                    .iter()
                    .any(|e| e.get("file").and_then(|f| f.as_str()).is_some());
                if !has_file_diags {
                    build_events.append(&mut Self::parse_clang_output(&build_out.output));
                }
                let _ = self
                    .ingest_diagnostics(&build_events, "build", Path::new(path))
                    .await;
                let _ = self
                    .store_action_status_with_output(
                        "build",
                        path,
                        status_target.as_deref(),
                        build_out.success,
                        build_out.duration_secs,
                        &build_out.output,
                    )
                    .await;

                // ── 2. Lint (only meaningful once the code compiles) ─────────
                let mut lint_out: Option<BuildOutput> = None;
                let mut lint_events: Vec<serde_json::Value> = Vec::new();
                if build_out.success {
                    if let Ok((lo, mut le)) = self
                        .lint_project_streaming(Path::new(path), opts.platform.clone())
                        .await
                    {
                        if le.is_empty() {
                            le.append(&mut Self::parse_clang_output(&lo.output));
                        }
                        let _ = self.ingest_diagnostics(&le, "lint", Path::new(path)).await;
                        let _ = self
                            .store_action_status(
                                "lint",
                                path,
                                status_target.as_deref(),
                                lo.success,
                                lo.duration_secs,
                            )
                            .await;
                        lint_events = le;
                        lint_out = Some(lo);
                    }
                }

                let success =
                    build_out.success && lint_out.as_ref().map(|l| l.success).unwrap_or(true);
                let mut sections = vec![format!(
                    "=== Build ({}) ===\n{}",
                    if build_out.success { "ok" } else { "FAILED" },
                    build_out.output
                )];
                match &lint_out {
                    Some(lo) => sections.push(format!(
                        "=== Lint ({}) ===\n{}",
                        if lo.success { "ok" } else { "FAILED" },
                        lo.output
                    )),
                    None => sections.push(
                        "=== Lint ===\nskipped — the build failed, so there is no compile \
                         database to analyse"
                            .to_string(),
                    ),
                }
                let mut events = build_events;
                events.extend(lint_events);
                serde_json::json!({
                    "success": success,
                    "output": sections.join("\n\n"),
                    "command": "meson compile + lint",
                    "duration_secs": build_out.duration_secs
                        + lint_out.as_ref().map(|l| l.duration_secs).unwrap_or(0.0),
                    "exit_code": if success { 0 } else { 1 },
                    "buildEvents": events,
                    "steps": [
                        { "name": "build", "success": build_out.success },
                        { "name": "lint", "success": lint_out.as_ref().map(|l| l.success) },
                    ],
                })
            }
            "build_format" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match self.format_project(Path::new(path)).await {
                    Ok(o) => {
                        serde_json::to_value(o).unwrap_or(serde_json::json!({"error": "serialize"}))
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_fix" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match self.fix_project_streaming(Path::new(path)).await {
                    Ok((o, events)) => {
                        let mut val = serde_json::to_value(&o)
                            .unwrap_or(serde_json::json!({"error": "serialize"}));
                        if let serde_json::Value::Object(ref mut m) = val {
                            m.insert("buildEvents".to_string(), serde_json::json!(events.clone()));
                        }
                        // Persist post-fix diagnostics (usually no remaining events).
                        let _ = self
                            .ingest_diagnostics(&events, "fix", Path::new(path))
                            .await;
                        val
                    }
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_scaffold" => {
                let project_name = args
                    .get("project_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let build_system = args
                    .get("build_system")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let goal = args
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
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
                match self
                    .scaffold_build_config(
                        project_name,
                        build_system,
                        goal,
                        &platforms,
                        spire_core::build_types::ProjectStructure::Native,
                        false,
                    )
                    .await
                {
                    Ok(out) => serde_json::to_value(out).unwrap_or_else(
                        |_| serde_json::json!({ "error": "scaffold serialization" }),
                    ),
                    Err(e) => serde_json::json!({ "error": e }),
                }
            }
            "build_list_modules" => {
                serde_json::to_value(&self.capabilities).unwrap_or(serde_json::json!([]))
            }
            "build_dependency_docs" => {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let version = args.get("version").and_then(|v| v.as_str()).unwrap_or("");
                // Route to the module that owns the requested language, falling
                // back to Cargo (Rust) which is the primary supported runtime.
                let lang = args
                    .get("language")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Rust");
                let module_tx = self
                    .capabilities
                    .iter()
                    .find(|c| {
                        c.language.eq_ignore_ascii_case(lang)
                            || c.build_system.eq_ignore_ascii_case(lang)
                    })
                    .and_then(|c| {
                        self.router
                            .get(c.config_files.first().map(|s| s.as_str()).unwrap_or(""))
                    })
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
                            Err(e) => {
                                serde_json::json!({ "error": format!("dependency docs response lost: {}", e) })
                            }
                        }
                    }
                    None => {
                        serde_json::json!({ "error": "no build module available for dependency docs" })
                    }
                }
            }
            // ── HAL contract helpers (Phase A) ─────────────────────────
            // Exposed through tools/call so the Swift wizard can validate a
            // proposed abstract-class header, generate per-target placeholder
            // implementations, and diff contract versions — all against the
            // deterministic contract tooling in spire-modules.
            "hal_validate_contract" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
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
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
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
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let interface = args
                    .get("interface")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
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
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let interface = args
                    .get("interface")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() || interface.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_generate_impl: 'root', 'interface' and 'platform' are required" })
                } else if self.llm_tx.is_none() {
                    serde_json::json!({ "error": "hal_generate_impl: LLM unavailable — the build manager is not connected to the LLM service (wiring, not a missing API key)" })
                } else {
                    let plan = self
                        .hal_generate_plan(
                            root,
                            interface,
                            platform,
                            args.get("library_hints").and_then(|v| v.as_str()),
                        )
                        .await;
                    if plan.get("error").is_some() {
                        plan
                    } else {
                        let syntax = plan
                            .get("syntax")
                            .cloned()
                            .unwrap_or(serde_json::json!("ok"));
                        let mut result = self
                            .hal_generate_apply(root, interface, platform, &plan)
                            .await;
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
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let interface = args
                    .get("interface")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() || interface.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_generate_impl_plan: 'root', 'interface' and 'platform' are required" })
                } else if self.llm_tx.is_none() {
                    serde_json::json!({ "error": "hal_generate_impl_plan: LLM unavailable — the build manager is not connected to the LLM service (wiring, not a missing API key)" })
                } else {
                    self.hal_generate_plan(
                        root,
                        interface,
                        platform,
                        args.get("library_hints").and_then(|v| v.as_str()),
                    )
                    .await
                }
            }
            // Step 4 (LLM half, APPLY): write an APPROVED module pair (header +
            // source), remove stale stubs, wire meson and run the compile gate.
            "hal_generate_impl_apply" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let interface = args
                    .get("interface")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() || interface.is_empty() || platform.is_empty() {
                    serde_json::json!({ "error": "hal_generate_impl_apply: 'root', 'interface' and 'platform' are required" })
                } else {
                    self.hal_generate_apply(root, interface, platform, &args)
                        .await
                }
            }

            "hal_generate_placeholder" => {
                let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                let platform = args.get("platform").and_then(|v| v.as_str()).unwrap_or("");
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
                    let classes =
                        crate::build::generic_helpers::parse_hal_contract_summary(summary);
                    if classes.is_empty() {
                        serde_json::json!({ "error": "hal_generate_placeholder: summary has no contract methods" })
                    } else {
                        let (class_name, methods) = &classes[0];
                        let source = crate::build::generic_helpers::generate_hal_placeholder_source(
                            &header_stem,
                            class_name,
                            methods,
                            platform,
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
                        .join("hal")
                        .join("implementations")
                        .join(platform);
                    let Ok(entries) = std::fs::read_dir(&api_dir) else {
                        return serde_json::json!({ "error": format!("no hal/api found under {}", root) });
                    };
                    // Contract headers → header stem + its summary + class name.
                    let mut interfaces: Vec<(String, String, String)> = Vec::new();
                    for e in entries.flatten() {
                        let ep = e.path();
                        let Some(ext) = ep.extension().and_then(|x| x.to_str()) else {
                            continue;
                        };
                        if ext != "hpp" {
                            continue;
                        }
                        let Ok(content) = std::fs::read_to_string(&ep) else {
                            continue;
                        };
                        let Ok(summary) =
                            crate::build::generic_helpers::summarize_hal_header(&content)
                        else {
                            continue;
                        };
                        let Some(stem) = ep.file_stem().and_then(|s| s.to_str()) else {
                            continue;
                        };
                        let class_name =
                            crate::build::generic_helpers::parse_hal_contract_summary(&summary)
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
                            let parsed =
                                crate::build::generic_helpers::parse_hal_contract_summary(summary);
                            let Some((_, methods)) = parsed.first() else {
                                failures.push(format!("{stem}: no contract methods parsed"));
                                continue;
                            };
                            let source =
                                crate::build::generic_helpers::generate_hal_placeholder_source(
                                    stem, class_name, methods, platform,
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
                            &interfaces
                                .iter()
                                .map(|(s, _, _)| s.clone())
                                .collect::<Vec<_>>(),
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

                        let analysis_status =
                            match self.analyze_project(std::path::Path::new(root), None).await {
                                Ok(_) => "re-analyzed (queued impls ready)".to_string(),
                                Err(e) => format!("re-analyze failed: {e}"),
                            };
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
                    let template = existing
                        .iter()
                        .find(|p| {
                            *p != "toolkit" && *p != "hal" && *p != "host" && *p != "subprojects"
                        })
                        .cloned();
                    let Some(template) = template else {
                        return serde_json::json!({ "error": "hal_add_platform: no existing platform subdir found (add one platform first)" });
                    };
                    if existing.iter().any(|p| p == platform)
                        || root_path.join(platform).exists()
                        || root_path
                            .join("hal/implementations")
                            .join(platform)
                            .exists()
                    {
                        return serde_json::json!({ "error": format!("platform '{platform}' is already present in this project") });
                    }
                    let Some(platform_rec) = crate::platform::Platform::from_registry(platform)
                    else {
                        return serde_json::json!({ "error": format!("platform '{platform}' not in registry (~/.spire/platforms)") });
                    };

                    // 2. Contract headers → (stem, summary, class_name).
                    let api_dir = root_path.join("hal/api");
                    let mut interfaces: Vec<(String, String, String)> = Vec::new();
                    if api_dir.is_dir() {
                        if let Ok(entries) = std::fs::read_dir(&api_dir) {
                            for e in entries.flatten() {
                                let ep = e.path();
                                let Some(ext) = ep.extension().and_then(|x| x.to_str()) else {
                                    continue;
                                };
                                if ext != "hpp" && ext != "h" {
                                    continue;
                                }
                                let Ok(content) = std::fs::read_to_string(&ep) else {
                                    continue;
                                };
                                let Ok(summary) =
                                    crate::build::generic_helpers::summarize_hal_header(&content)
                                else {
                                    continue;
                                };
                                let Some(stem) = ep.file_stem().and_then(|s| s.to_str()) else {
                                    continue;
                                };
                                let class_name =
                                    crate::build::generic_helpers::parse_hal_contract_summary(
                                        &summary,
                                    )
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
                        let parsed =
                            crate::build::generic_helpers::parse_hal_contract_summary(summary);
                        let Some((_, methods)) = parsed.first() else {
                            failures.push(format!("{stem}: no contract methods parsed"));
                            continue;
                        };
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
                    let section =
                        crate::build::generic_helpers::hal_meson_var_section(platform, &stems);
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

                    // 5. Root meson.build: gated subdir('<plat>'). The platform
                    // subdir must be included ONLY when -Dplatform=<plat>, so
                    // append a guarded block — an unconditional subdir('x') would
                    // pull the target into every other platform's build.
                    let mut root_status = "wired".to_string();
                    if !root_content.contains(&format!("subdir('{platform}')")) {
                        let mut updated = root_content.clone();
                        updated.push_str(&format!(
                            "\nif platform == '{platform}'\n  subdir('{platform}')\nendif\n"
                        ));
                        if let Err(e) = std::fs::write(&root_meson, &updated) {
                            root_status = format!("root meson write failed: {e}");
                        }
                    }

                    // 6. meson_options.txt: add <plat> to "Valid values".
                    let mut options_status = "wired".to_string();
                    let options_path = root_path.join("meson_options.txt");
                    if let Ok(opts) = std::fs::read_to_string(&options_path) {
                        let re = regex::Regex::new(r"(?i)(Valid values\s*:\s*)([A-Za-z0-9_ ,\-]+)")
                            .unwrap();
                        if let Some(caps) = re.captures(&opts) {
                            let mut values: Vec<String> = caps
                                .get(2)
                                .unwrap()
                                .as_str()
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

                    // 7. <plat>/meson.build + <plat>/main.cpp.
                    //
                    // Generate a CLEAN skeleton rather than cloning the template
                    // platform's meson.build: a blind id-substituted copy drags in
                    // the template's platform-specific external deps (e.g. rpi5's
                    // libcamera / tensorflow-lite / edgetpu), its -DHAVE_<TEMPLATE>
                    // define, sysroot paths and app sources — all wrong for a new
                    // platform. Platform-specific deps are left EMPTY with a TODO;
                    // hal_add_platform must not guess a new platform's libraries.
                    let plat_dir = root_path.join(platform);
                    let _ = std::fs::create_dir_all(&plat_dir);
                    let mut plat_status = "wired".to_string();
                    let plat_name = platform_rec.name.clone();
                    let plat_upper = platform.to_uppercase();
                    // Per-platform hardware facts from the registry, surfaced in the
                    // meson skeleton so the deps TODO names the real libraries.
                    let lib_hints: String =
                        crate::build::generic_helpers::hal_platform_library_hints(platform)
                            .lines()
                            .map(|l| format!("#   {l}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                    let platform_meson = format!(
                        r#"# ------------------------------------------------------------------------------
# {plat_name} ({platform}) PLATFORM TARGET
#
# Included from the ROOT meson.build via a subdir('{platform}') gated on
# -Dplatform={platform}. Compiles the shared toolkit + HAL sources with the
# {platform} cross toolchain. No project() call here (the root one is the only
# project()).
#
# Scaffolded by hal_add_platform. The platform-specific deps are LEFT EMPTY on
# purpose: a new platform's libraries are not knowable here. Fill in
# `platform_deps` below with the real {platform} dependencies.
#
#   meson setup build-{platform} --cross-file {platform}/{platform}-cross.txt -Dplatform={platform}
#   meson compile -C build-{platform}
# ------------------------------------------------------------------------------

fs  = import('fs')
cpp = meson.get_compiler('cpp')

platform = get_option('platform')

# --- Core dependencies (shared across platforms; optional via pkg-config) ---
nlohmann_json_dep = dependency('nlohmann_json', required: false)
yaml_cpp_dep      = dependency('yaml-cpp', required: false)
sqlite3_dep       = dependency('sqlite3', required: false)
jpeg_dep          = dependency('libjpeg', required: false)
turbojpeg_dep     = dependency('libturbojpeg', required: false)
png_dep           = dependency('libpng', required: false)
systemd_dep       = dependency('libsystemd', required: false)

core_deps = []
foreach d : [nlohmann_json_dep, yaml_cpp_dep, sqlite3_dep, jpeg_dep,
             turbojpeg_dep, png_dep]
  if d.found()
    core_deps += d
  endif
endforeach

# --- Shared toolkit sources (inherited from subdir('toolkit') in root) ---
all_sources = toolkit_sources

# WiFi provisioning requires systemd (BlueZ D-Bus GATT) - Linux only.
if systemd_dep.found()
  all_sources += wifi_provisioning_source
  core_deps += systemd_dep
endif

# --- {platform} HAL sources (centralized in hal/meson.build) ---
{platform}_hal_sources = hal_impl_{platform}_sources

# --- Application sources ---
# The generic app (main + the ONE platform-specific HAL binding) is shared
# across platforms; only ../app/platform_hal_{platform}.cpp differs.
app_sources = files(
  '../app/main.cpp',
  '../app/platform_hal_{platform}.cpp',
)

# --- Platform-specific dependencies ({platform}) ---
# TODO({platform}): add {platform}-specific external libraries (accelerator / ISP
# / media / camera) and append them to platform_deps. Registry hints:
{lib_hints}
platform_deps = []
add_project_arguments('-DHAVE_{plat_upper}', language: 'cpp')

# --- Include paths (toolkit (from root subdir) + {platform}) ---
inc = include_directories(
  '..',
  '.',
  '../hal/api',
  '../hal/implementations',
)

# --- Target: the {project_name}-{platform} binary ---
executable('{project_name}-{platform}',
  app_sources + {platform}_hal_sources + all_sources,
  include_directories : [toolkit_inc, inc],
  dependencies : core_deps + platform_deps,
  install : true,
  install_dir : '/usr/local/bin')
"#
                    );
                    if let Err(e) = std::fs::write(plat_dir.join("meson.build"), &platform_meson) {
                        plat_status = format!("platform meson write failed: {e}");
                    }
                    // 7a. The per-platform HAL binding + the concrete aggregate
                    // HAL. The generic app/main.cpp is shared across platforms, so
                    // there is NO per-platform main.cpp: we write the ONE binding
                    // (app/platform_hal_<plat>.cpp) plus the aggregate that owns
                    // the platform's components (hal/implementations/<plat>/
                    // ai_trap_hal_<plat>.{hpp,cpp}).
                    let suffix = crate::build::generic_helpers::platform_class_suffix(platform);
                    let agg_class = format!("AiTrapHal{suffix}");

                    let app_dir = root_path.join("app");
                    let _ = std::fs::create_dir_all(&app_dir);
                    let platform_hal = crate::build::generic_helpers::generate_platform_hal_source(
                        platform, &agg_class,
                    );
                    if let Err(e) = std::fs::write(
                        app_dir.join(format!("platform_hal_{platform}.cpp")),
                        &platform_hal,
                    ) {
                        plat_status = format!("platform_hal write failed: {e}");
                    }

                    // The aggregate's accessors mirror the project's AiTrapHal
                    // contract, parsed from disk, so it matches whatever the app
                    // declares (it is not hardcoded to camera/inference/...).
                    let mut accessors: Vec<(String, String)> = Vec::new();
                    if let Ok(txt) =
                        std::fs::read_to_string(root_path.join("hal/api/ai_trap_hal.hpp"))
                    {
                        for (cls, methods) in
                            crate::build::generic_helpers::extract_contract_methods_cpp(&txt)
                        {
                            if cls != "AiTrapHal" {
                                continue;
                            }
                            for m in methods {
                                if m.name.starts_with('~') {
                                    continue;
                                }
                                accessors.push((m.return_type, m.name));
                            }
                            break;
                        }
                    }
                    let agg_hpp = crate::build::generic_helpers::generate_aggregate_hal_header(
                        platform, &agg_class, &accessors,
                    );
                    let agg_cpp = crate::build::generic_helpers::generate_aggregate_hal_source(
                        platform, &agg_class, &accessors,
                    );
                    if let Err(e) = std::fs::write(
                        impl_dir.join(format!("ai_trap_hal_{platform}.hpp")),
                        &agg_hpp,
                    ) {
                        plat_status = format!("aggregate header write failed: {e}");
                    }
                    if let Err(e) = std::fs::write(
                        impl_dir.join(format!("ai_trap_hal_{platform}.cpp")),
                        &agg_cpp,
                    ) {
                        plat_status = format!("aggregate source write failed: {e}");
                    }

                    // 7b. <plat>/<plat>-cross.txt — generated from the platform
                    // registry record so a freshly added platform can actually be
                    // cross-compiled (the meson wiring alone can't build).
                    let mut cross_status =
                        "not generated (platform has no Linux cross file)".to_string();
                    if let Some(cross) = platform_rec.meson_cross_file() {
                        let header = format!(
                            "# {plat_name} ({platform}) Meson cross file — generated from ~/.spire/platforms/{platform}.yaml\n\n"
                        );
                        let cross_path = plat_dir.join(format!("{platform}-cross.txt"));
                        match std::fs::write(&cross_path, format!("{header}{cross}")) {
                            Ok(()) => cross_status = "wired".to_string(),
                            Err(e) => cross_status = format!("cross file write failed: {e}"),
                        }
                    }

                    // 8. Re-analyze so the new domain/build target + fill queue
                    // reflect the new platform.

                    let analysis_status = match self.analyze_project(root_path, None).await {
                        Ok(_) => "re-analyzed (platform added)".to_string(),
                        Err(e) => format!("re-analyze failed: {e}"),
                    };

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
                        "platform_binding": format!("app/platform_hal_{platform}.cpp"),
                        "aggregate": format!("hal/implementations/{platform}/ai_trap_hal_{platform}.cpp"),
                        "cross_file": cross_status,
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
                    let mut coverage = crate::build::generic_helpers::hal_platform_coverage_map(
                        std::path::Path::new(root),
                    );
                    // Rust HAL contracts are recognised beside the C++ ones: `hal/api/*.rs`
                    // is the same convention with a different language, so the two maps MERGE
                    // rather than one replacing the other — and a project mid-migration
                    // legitimately has both.
                    for (plat, ifaces) in
                        crate::build::hal_rust_contract::rust_platform_coverage_map(
                            std::path::Path::new(root),
                        )
                    {
                        coverage.entry(plat).or_default().extend(ifaces);
                    }
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
                            // `stub` is checked before `partial`: a placeholder that declares every
                            // method is neither implemented nor a genuine partial — it is a
                            // generated stub, and the queue has to say which so a reader can tell
                            // "nothing written yet" from "some of it is written".
                            let kind = if cov.is_stub {
                                "stub"
                            } else if cov.implemented {
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
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_fill_plan: \"root\" required" })
                } else {
                    crate::actors::hal_fill::plan(
                        std::path::Path::new(&root),
                        &platform,
                        &interfaces,
                    )
                }
            }

            "hal_fill_apply" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let plan = args.get("plan").cloned();
                match plan {
                    Some(plan) if !root.is_empty() => {
                        let analyze = async {
                            let r = self
                                .analyze_project(std::path::Path::new(&root), None)
                                .await;
                            r.map(|_| ())
                        };
                        crate::actors::hal_fill::apply(
                            std::path::Path::new(&root),
                            &plan,
                            Box::pin(analyze),
                        )
                        .await
                    }
                    _ => serde_json::json!({
                        "error": "hal_fill_apply: \"root\" and \"plan\" required"
                    }),
                }
            }

            // Rust embedded-HAL gap fill: the work items for each backend crate, with the
            // constrained prompt. Reads the same Rust coverage measure `hal_missing_impls`
            // merges, so the plan and the UI's indicators can never disagree.
            "embedded_hal_fill_plan" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args.get("platform").and_then(|v| v.as_str());
                if root.is_empty() {
                    serde_json::json!({ "error": "embedded_hal_fill_plan: \"root\" required" })
                } else {
                    crate::build::embedded_hal_fill::plan(std::path::Path::new(root), platform)
                }
            }

            "embedded_hal_fill_apply" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match (root.is_empty(), args.get("plan")) {
                    (false, Some(plan)) => {
                        // No re-analysis afterwards, unlike the C++ apply: that one *creates* files
                        // (a declaration/definition pair plus meson wiring) and has to re-analyze
                        // for them to exist. This rewrites one existing backend file, and the
                        // coverage the UI reads is recomputed from disk on every call — so there is
                        // nothing stale to refresh, and a full parse pass would be a cost with no
                        // effect.
                        let path = std::path::Path::new(root);
                        let mut result =
                            crate::build::embedded_hal_fill::apply(path, plan, &self.llm_tx).await;
                        // Then the only check that can say whether the file *builds*: the fill's own
                        // gate is structural, so the compiler gets the last word — and one repair
                        // round with the compiler's errors, because a wrong-but-plausible API guess
                        // is the failure mode that gate cannot see.
                        self.verify_embedded_hal_fills(path, plan, &mut result)
                            .await;
                        result
                    }
                    _ => serde_json::json!({
                        "error": "embedded_hal_fill_apply: \"root\" and \"plan\" required"
                    }),
                }
            }

            // Authoring a Rust contract: validate, then write it *and wire it* — a contract file
            // nothing declares is invisible to the drift measure, which is the failure this pair
            // exists to prevent (`hal_validate_contract`/`hal_write_contract` for the Rust layout).
            "embedded_hal_validate_contract" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    serde_json::json!({ "error": "embedded_hal_validate_contract: 'content' (Rust source) is required" })
                } else {
                    match crate::build::embedded_hal_contract::validate_contract(content) {
                        Ok(summary) => summary,
                        Err(e) => serde_json::json!({ "valid": false, "error": e }),
                    }
                }
            }

            "embedded_hal_write_contract" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let filename = args
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if root.is_empty() || filename.is_empty() || content.is_empty() {
                    serde_json::json!({
                        "error": "embedded_hal_write_contract: 'root', 'filename' and 'content' are required"
                    })
                } else {
                    match crate::build::embedded_hal_contract::write_contract(
                        std::path::Path::new(root),
                        filename,
                        content,
                    ) {
                        Ok(mut result) => {
                            // The write validated the *submitted* text; this checks the file that
                            // landed, with a compiler. The verify spine runs with no repair rounds
                            // here on purpose: the source is the user's own, typed in the sheet, so
                            // there is no generator to hand errors back to — the caller gets the
                            // compiler's words and decides.
                            self.verify_written_contract(std::path::Path::new(root), &mut result)
                                .await;
                            result
                        }
                        Err(e) => serde_json::json!({ "valid": false, "error": e }),
                    }
                }
            }

            // Adding a board family to an existing project: the backend crate comes from the
            // scaffold's own emitter, plus the workspace member line that makes it exist.
            "embedded_hal_add_platform" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let platform = args
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() || platform.is_empty() {
                    serde_json::json!({
                        "error": "embedded_hal_add_platform: 'root' (project dir) and 'platform' (registry id) are required"
                    })
                } else {
                    match crate::build::embedded_hal_contract::add_platform(
                        std::path::Path::new(root),
                        platform,
                    ) {
                        Ok(result) => result,
                        Err(e) => serde_json::json!({ "error": e }),
                    }
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
                    let change =
                        crate::build::generic_helpers::diff_hal_contracts(old_summary, new_summary);
                    serde_json::json!({
                        "added": change.added,
                        "removed": change.removed,
                        "changed": change.changed,
                    })
                }
            }
            "cpp_syntax_check" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match std::fs::read_to_string(path) {
                    Ok(content) => serde_json::to_value(
                        crate::build::generic_helpers::cpp_syntax_check(&content),
                    )
                    .unwrap_or(serde_json::json!({"error": "cpp_syntax_check serialization"})),
                    Err(e) => serde_json::json!({"error": format!("read {}: {e}", path)}),
                }
            }
            "hal_fix_prompt" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let issues = crate::build::generic_helpers::hal_doc_lint_file(
                    std::path::Path::new(root),
                    path,
                );
                let content = std::fs::read_to_string(path).unwrap_or_default();
                serde_json::json!({
                    "issues": issues,
                    "prompt": crate::build::generic_helpers::hal_doc_fix_prompt_whole(path, &content, &issues),
                })
            }
            "hal_state" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                serde_json::to_value(crate::build::generic_helpers::compute_hal_state(
                    std::path::Path::new(root),
                ))
                .unwrap_or(serde_json::json!({"error": "hal_state serialization"}))
            }
            "hal_doc_lint" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                serde_json::to_value(crate::build::generic_helpers::hal_doc_lint(
                    std::path::Path::new(root),
                ))
                .unwrap_or(serde_json::json!({"error": "hal_doc_lint serialization"}))
            }
            "hal_docs" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let r = std::path::Path::new(root);
                serde_json::to_value(crate::build::generic_helpers::hal_report(r))
                    .unwrap_or(serde_json::json!({"error": "hal_docs serialization"}))
            }
            "hal_verify" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let r = std::path::Path::new(root);
                serde_json::to_value(crate::build::generic_helpers::hal_verify(r))
                    .unwrap_or(serde_json::json!({"error": "hal_verify serialization"}))
            }
            "hal_sanity_check" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_sanity_check: 'root' is required" })
                } else {
                    let report =
                        crate::build::hal_migration::hal_sanity_check(std::path::Path::new(root));
                    serde_json::to_value(report)
                        .unwrap_or_else(|_| serde_json::json!({ "error": "report serialization" }))
                }
            }
            "hal_migrate_plan" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_migrate_plan: 'root' is required" })
                } else {
                    match crate::build::hal_migration::migrate_hal_plan(std::path::Path::new(root))
                    {
                        Ok(plan) => serde_json::to_value(plan).unwrap_or_else(
                            |_| serde_json::json!({ "error": "plan serialization" }),
                        ),
                        Err(e) => serde_json::json!({ "error": e }),
                    }
                }
            }
            "hal_migrate_apply" => {
                let root = args
                    .get("root")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if root.is_empty() {
                    serde_json::json!({ "error": "hal_migrate_apply: 'root' is required" })
                } else {
                    let plan_value = args.get("plan").cloned().unwrap_or(serde_json::Value::Null);
                    let plan: crate::build::hal_migration::HalMigrationPlan =
                        match serde_json::from_value(plan_value) {
                            Ok(p) => p,
                            Err(e) => {
                                return serde_json::json!({ "error": format!("invalid plan: {e}") })
                            }
                        };
                    match crate::build::hal_migration::migrate_hal_apply(
                        std::path::Path::new(root),
                        &plan,
                    ) {
                        Ok(res) => serde_json::to_value(res).unwrap_or_else(
                            |_| serde_json::json!({ "error": "result serialization" }),
                        ),
                        Err(e) => serde_json::json!({ "error": e }),
                    }
                }
            }
            other => serde_json::json!({ "error": format!("Unknown build tool: {other}") }),
        }
    }

    /// Persist the latest per-platform action status under a graph config key
    /// `<kind>.last.<path>.<target>` (or `<kind>.last.<path>` when no target was
    /// selected) so the platform list / Build tab can show "last <kind>
    /// succeeded/failed" + duration PER PLATFORM. `kind` is one of
    /// "build" | "lint" | "test" | "clean"; each action runs per platform
    /// (rpi5/rock3c), so results MUST NOT overwrite each other.
    async fn store_action_status(
        &self,
        kind: &str,
        path: &str,
        target: Option<&str>,
        success: bool,
        duration_secs: f64,
    ) -> Result<(), String> {
        self.store_action_status_with_output(kind, path, target, success, duration_secs, "")
            .await
    }

    async fn store_action_status_with_output(
        &self,
        kind: &str,
        path: &str,
        target: Option<&str>,
        success: bool,
        duration_secs: f64,
        output: &str,
    ) -> Result<(), String> {
        let key = match target {
            Some(t) if !t.trim().is_empty() => format!("{kind}.last.{path}.{t}"),
            _ => format!("{kind}.last.{path}"),
        };
        let value = serde_json::json!({
            "path": path,
            "kind": kind,
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

    /// Resolve the target name an action actually ran for, so per-platform
    /// status lands under the SAME per-target key the UI reads back:
    ///   • the explicit `target` argument, when the caller selected a target;
    ///   • otherwise the target matching the requested `platform` (a platform /
    ///     domain selection still builds one specific executable).
    async fn resolve_status_target(
        &self,
        path: &str,
        target: Option<&str>,
        platform: Option<&str>,
    ) -> Option<String> {
        if let Some(t) = target.filter(|t| !t.trim().is_empty()) {
            return Some(t.to_string());
        }
        let plat = platform.filter(|p| !p.trim().is_empty())?;
        let md = self.get_analysis(path).await?;
        md.targets
            .iter()
            .find(|t| t.platform == plat)
            .map(|t| t.name.clone())
    }

    /// Resolve the normalized `build_spec` for the selected build target from
    /// stored analysis. When the caller selected a concrete target (name or
    /// platform), the matching `BuildTarget.build_spec` is returned so the
    /// module executes it directly; otherwise `None` (existing per-tool logic).
    fn resolve_build_spec(
        metadata: &BuildMetadata,
        opts: &BuildOptions,
    ) -> Option<spire_core::build_types::BuildSpec> {
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
        let mut tools = vec![
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
                        "filter": { "type": "string", "description": "Optional test name filter" },
                        "platform": { "type": "string", "description": "Optional cross-platform target (e.g. host/rpi5) selecting which build-<platform> Meson dir to test in" }
                    },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_verify".to_string(),
                description: "Build a project and, when it compiles, lint it — the combined check a developer runs together. Requires prior build_analyze.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project directory path" },
                        "platform": { "type": "string", "description": "Optional cross-platform target (e.g. host/rpi5) selecting the build-<platform> Meson dir" },
                        "target": { "type": "string", "description": "Optional specific build target (e.g. Meson executable name)" }
                    },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_flash".to_string(),
                description: "Flash a built artifact onto an embedded board over USB (esp-idf: espflash) and report the result. Requires prior build_analyze and a successful build for that platform.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project root directory" },
                        "platform": { "type": "string", "description": "Platform registry id of the board to flash (e.g. esp32c6) — required, because it names the chip the tool talks to" },
                        "mode": { "type": "string", "description": "Build profile whose artifact is flashed: debug (default) or release" },
                        "package": { "type": "string", "description": "Optional package/workspace member whose binary is flashed" },
                        "artifact": { "type": "string", "description": "Optional path to the binary to flash; defaults to the one the build wrote for that platform" },
                        "port": { "type": "string", "description": "Optional serial port of the board (e.g. /dev/cu.usbserial-1234); defaults to the single USB serial device" }
                    },
                    "required": ["path", "platform"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_clean".to_string(),
                description: "Clean a project directory using its detected build system (removes build artifacts, keeps the configured build dir). Requires prior build_analyze.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project directory path" },
                        "platform": { "type": "string", "description": "Optional cross-platform target (e.g. host/rpi5) pinning the build-<platform> Meson dir to clean" }
                    },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_lint".to_string(),
                description: "Lint/analyze a project directory using its detected build system and return structured diagnostics. Requires prior build_analyze.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project directory path" },
                        "platform": { "type": "string", "description": "Optional cross-platform target (e.g. host/rpi5) selecting which build-<platform> Meson dir's compile database to analyze" }
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
                name: "build_format".to_string(),
                description: "Run the project's formatter (e.g. clang-format) via the language module.".to_string(),
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
                name: "modify_code".to_string(),
                description: "Change existing code from a prompt: the model picks the files in scope, rewrites them, and a change is kept only if the project still builds and the tests that can run still pass — otherwise it is rolled back byte-for-byte. Target tests run only when a board is connected, and the result says which layer verification reached.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project or subproject directory path" },
                        "prompt": { "type": "string", "description": "What to change, in the user's own words" },
                        "platform": { "type": "string", "description": "Cross-platform target (e.g. rpi5) the change is verified against" },
                        "scope": { "type": "array", "items": { "type": "string" }, "description": "Files the model may consider; defaults to the project's sources" }
                    },
                    "required": ["path", "prompt"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "modify_contract".to_string(),
                description: "Resolve the HAL contract cascade for a platform: find the interfaces that are missing or drifted, generate their implementations, and keep a round only when the drift fell without breaking the build. Compile errors left in consumers are what Fix & Verify is for.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project directory path" },
                        "platform": { "type": "string", "description": "Platform whose implementations are generated (required)" },
                        "target": { "type": "string", "description": "Specific build target within the project" }
                    },
                    "required": ["path"]
                }),
            },
            spire_core::actors::ToolInfo {
                name: "build_autofix".to_string(),
                description: "Fix & Verify pipeline: compile, fix compile errors with LLM rewrites, lint, fix safely-fixable warnings (dead stores/unused code), and re-verify — keeping only edits that measurably help and rolling back any that do not, until the project builds cleanly or the round cap is reached. Every write is compile-verified; warnings that need judgement are reported, never rewritten.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Project or subproject directory path" },
                        "platform": { "type": "string", "description": "Cross-platform target (e.g. rpi5) whose build directory the fixes are verified against" },
                        "target": { "type": "string", "description": "Specific build target within the project" }
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
        ];
        tools.extend(Self::hal_tool_definitions());
        tools
    }

    /// HAL / build-helper tools dispatched by [`Self::call_tool`].
    ///
    /// These MUST be advertised here: `build_default_registry` registers exactly
    /// the tools `ListTools` returns, so any tool handled by `call_tool` but
    /// omitted from this list is unreachable through `tools/call` — it falls
    /// through to the MCP catch-all and fails with "no tool found <name>".
    fn hal_tool_definitions() -> Vec<spire_core::actors::ToolInfo> {
        fn t(
            name: &str,
            description: &str,
            properties: serde_json::Value,
            required: &[&str],
        ) -> spire_core::actors::ToolInfo {
            spire_core::actors::ToolInfo {
                name: name.to_string(),
                description: description.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": properties,
                    "required": required,
                }),
            }
        }

        let root = serde_json::json!({ "root": { "type": "string", "description": "Project root directory" } });
        let root_plat = serde_json::json!({
            "root": { "type": "string", "description": "Project root directory" },
            "platform": { "type": "string", "description": "Platform registry id (e.g. rpi5)" }
        });
        let root_iface_plat = serde_json::json!({
            "root": { "type": "string" },
            "interface": { "type": "string", "description": "HAL interface / contract stem" },
            "platform": { "type": "string" }
        });

        vec![
            // Project checks + migration.
            t("hal_sanity_check", "HAL project structural sanity check (layout + missing components).", root.clone(), &["root"]),
            t("hal_migrate_plan", "Plan a HAL layout migration (legacy <-> canonical).", root.clone(), &["root"]),
            t("hal_migrate_apply", "Apply a HAL migration plan produced by hal_migrate_plan.",
              serde_json::json!({ "root": { "type": "string" }, "plan": { "type": "object" } }), &["root", "plan"]),
            t("hal_state", "Compute HAL state (contracts, platforms, coverage gaps).", root.clone(), &["root"]),
            t("hal_docs", "Generate the HAL documentation report.", root.clone(), &["root"]),
            t("hal_verify", "Verify every contract is implemented on every platform.", root.clone(), &["root"]),
            t("hal_doc_lint", "Lint HAL contract doc comments across a project.", root.clone(), &["root"]),
            t("hal_fix_prompt", "Build a doc-fix prompt for one HAL header file.",
              serde_json::json!({ "root": { "type": "string" }, "path": { "type": "string", "description": "Header file path" } }),
              &["root", "path"]),
            // Contract authoring.
            t("hal_validate_contract", "Validate a HAL contract header (C++ source) and summarize its methods.",
              serde_json::json!({ "content": { "type": "string", "description": "C++ header source" } }), &["content"]),
            t("hal_write_contract", "Write a validated HAL contract header into the project.",
              serde_json::json!({ "root": { "type": "string" }, "content": { "type": "string" } }), &["root", "content"]),
            // LLM Stage-1 implementation generation.
            t("hal_build_impl_prompt", "Build the constrained Stage-1 implementation prompt for one interface on one platform.",
              root_iface_plat.clone(), &["root", "interface", "platform"]),
            t("hal_generate_impl", "Generate a platform implementation for one HAL interface (LLM Stage 1).",
              root_iface_plat.clone(), &["root", "interface", "platform"]),
            t("hal_generate_impl_plan", "Plan (read-only) LLM Stage-1 implementation generation for one interface.",
              root_iface_plat.clone(), &["root", "interface", "platform"]),
            t("hal_generate_impl_apply", "Apply a previously generated Stage-1 implementation (write files).",
              root_iface_plat.clone(), &["root", "interface", "platform"]),
            t("hal_generate_placeholder", "Generate a platform placeholder stub from a contract summary.",
              serde_json::json!({ "summary": { "type": "string" }, "platform": { "type": "string" } }), &["summary", "platform"]),
            // Platform scaffolding.
            t("hal_add_target", "Add one platform target to a HAL project (meson dir + placeholder stubs).",
              root_plat.clone(), &["root", "platform"]),
            t("hal_add_platform", "Scaffold a FULL new platform target into an existing HAL project (dir, stubs, meson wiring, re-analyze).",
              root_plat.clone(), &["root", "platform"]),
            t("hal_missing_impls", "List the HAL implementations missing on a platform.", root.clone(), &["root"]),
            // Gap fill.
            t("hal_fill_plan", "Plan (read-only) the HAL gap-fill work items for a platform.", root_plat.clone(), &["root"]),
            t("hal_fill_apply", "Apply a HAL gap-fill plan (write the concrete implementation files).",
              serde_json::json!({ "root": { "type": "string" }, "plan": { "type": "array" } }), &["root", "plan"]),
            t("embedded_hal_fill_plan", "Plan (read-only) the Rust embedded-HAL fill work: one item per backend file, with the constrained prompt.",
              serde_json::json!({
                  "root": { "type": "string" },
                  "platform": { "type": "string", "description": "Platform registry id supplying the hardware profile + hints (e.g. esp32c6); defaults to the first record of the backend's family" }
              }),
              &["root"]),
            t("embedded_hal_fill_apply", "Generate and write a Rust embedded-HAL fill plan (one model call per backend file; the result is gated and the project re-measured).",
              serde_json::json!({
                  "root": { "type": "string" },
                  "plan": { "description": "The embedded_hal_fill_plan result, or its `plan` array" }
              }),
              &["root", "plan"]),
            t("embedded_hal_validate_contract", "Validate a Rust embedded-HAL contract's source before it is written (parses, declares at least one implementable trait, no implementation).",
              serde_json::json!({ "content": { "type": "string" } }),
              &["content"]),
            t("embedded_hal_write_contract", "Validate and write a Rust embedded-HAL contract into the contract crate, declaring and re-exporting its module so the drift measure can see it.",
              serde_json::json!({
                  "root": { "type": "string" },
                  "filename": { "type": "string", "description": "e.g. `sensor.rs` or `sensor` — the module name in the contract crate" },
                  "content": { "type": "string" }
              }),
              &["root", "filename", "content"]),
            t("embedded_hal_add_platform", "Add a board family to an existing Rust embedded-HAL project: a backend crate (from the scaffold's own emitter) and its workspace member.",
              serde_json::json!({
                  "root": { "type": "string" },
                  "platform": { "type": "string", "description": "Platform registry id (e.g. esp32c6); its `family` decides the crate" }
              }),
              &["root", "platform"]),
            t("hal_diff_contracts", "Diff two HAL contract summaries (added/removed/changed methods).",
              serde_json::json!({ "old_summary": { "type": "object" }, "new_summary": { "type": "object" } }),
              &["old_summary", "new_summary"]),
            // Helpers.
            t("cpp_syntax_check", "Run a lightweight C++ syntax check on a file.",
              serde_json::json!({ "path": { "type": "string" } }), &["path"]),
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

            BuildManagerMessage::AddPlatformModule {
                os,
                capability,
                module_tx,
            } => {
                self.add_platform_module(os, capability, module_tx);
            }

            BuildManagerMessage::AnalyzeProject {
                path,
                config_file,
                reply_to,
            } => {
                let result = self.analyze_project(&path, config_file.as_deref()).await;
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
                // Legacy manager-level path (no platform context); the
                // platform-aware entry point is the `build_test` tool.
                let result = self.test_project(&path, &opts, None).await;
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
                // Isolate tool panics: a panic would otherwise unwind this actor
                // task and drop `reply_to`, surfacing to the caller as
                // "backend response error: channel closed" AND killing the
                // BuildManager for the rest of the session (every later build/
                // hal call then fails too). Catch it, log it, and answer with an
                // error instead.
                let result = match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                    self.call_tool(&tool_name, args),
                ))
                .await
                {
                    Ok(v) => v,
                    Err(payload) => {
                        let msg = payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_string())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown panic".to_string());
                        tracing::error!("BuildManager: tool '{tool_name}' panicked: {msg}");
                        serde_json::json!({ "error": format!("tool '{tool_name}' panicked: {msg}") })
                    }
                };
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

            BuildManagerMessage::SetEventLog { event_log_tx } => {
                self.event_log_tx = Some(event_log_tx);
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
                        Ok(result) => {
                            let _ = reply_to.send(result);
                        }
                        Err(e) => {
                            let _ = reply_to.send(Err(format!("scaffold response lost: {}", e)));
                        }
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

// ============================================================================
// BuildEventLogActor — owns the incremental build-event log the FFI drains.
// ============================================================================

/// Messages for the actor-owned build-event log.
pub enum BuildEventLogMessage {
    /// Append one serialized build event.
    Record(serde_json::Value),
    /// Drain (remove + return) every accumulated event.
    Drain {
        reply_to: tokio::sync::oneshot::Sender<Vec<serde_json::Value>>,
    },
}

/// Owns the accumulated build events that the Swift UI streams incrementally.
///
/// Replaces the previous shared `Arc<Mutex<Vec<Value>>>` + `Notify` that the
/// streaming forwarders and the FFI both reached into directly. The
/// BuildManagerActor is occupied for the whole duration of a streaming
/// build/lint/fix, so event delivery cannot go through its mailbox; this small
/// dedicated actor serializes appends and drains without a `Mutex` shared with
/// the FFI (the FFI only holds this actor's sender + a `Notify`).
pub struct BuildEventLogActor {
    pending: Vec<serde_json::Value>,
    notify: std::sync::Arc<tokio::sync::Notify>,
}

impl BuildEventLogActor {
    pub fn new(notify: std::sync::Arc<tokio::sync::Notify>) -> Self {
        Self {
            pending: Vec::new(),
            notify,
        }
    }
}

#[async_trait]
impl Actor for BuildEventLogActor {
    type Message = BuildEventLogMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildEventLogMessage::Record(json) => {
                self.pending.push(json);
                self.notify.notify_one();
            }
            BuildEventLogMessage::Drain { reply_to } => {
                let _ = reply_to.send(std::mem::take(&mut self.pending));
            }
        }
    }
}

/// How many repair rounds a filled backend gets before its compiler errors are simply reported.
///
/// A cost ceiling, not an aspiration: each round is one model call. One round was measurably not
/// enough — a live run fixed the GPIO type and then needed one more import — and three covers the
/// observed second-order cases while bounding a model that keeps answering the same way. A leftover
/// failure is reported with the compiler's words attached, which is what a fourth round would have
/// given it anyway.
const MAX_REPAIR_ROUNDS: u32 = 3;

/// A filled backend, seen by the verify spine.
///
/// The two facts the spine needs that the fill leg alone knows: which platform module can build this
/// crate, and that a build which *cannot run* (no module for that `os`, a closed channel, a missing
/// toolchain) is a setup problem rather than a compiler verdict. It is reported as such and never
/// handed to the model — no answer fixes an environment.
struct FillArtifact<'a> {
    manager: &'a BuildManagerActor,
    root: PathBuf,
    file: String,
    item: serde_json::Value,
    platform: String,
    package: String,
    config: String,
    metadata: BuildMetadata,
    llm_tx: mpsc::Sender<LlmMessage>,
}

#[async_trait::async_trait]
impl crate::build::verify_spine::GeneratedArtifact for FillArtifact<'_> {
    fn describe(&self) -> String {
        self.file.clone()
    }

    /// Nothing to gate: the fill leg already gated this answer (every pending trait implemented by
    /// name, no `unimplemented!()` left) *before* writing it, and re-checking the same rules on the
    /// same file would be a second implementation of that gate rather than a second opinion.
    fn gate(&self) -> Result<(), String> {
        Ok(())
    }

    async fn build(&self) -> Result<(), crate::build::verify_spine::BuildFailure> {
        use crate::build::verify_spine::BuildFailure;
        match self
            .manager
            .build_backend_crate(
                &self.root,
                &self.config,
                &self.metadata,
                &self.platform,
                &self.package,
            )
            .await
        {
            Ok(output) if output.success => Ok(()),
            // The crate was built and rejected: the compiler's words are the repair's input.
            Ok(output) => Err(BuildFailure::Compiler(output.output)),
            // Not built at all — nothing for a model to act on.
            Err(setup) => Err(BuildFailure::Setup(setup)),
        }
    }

    async fn repair(&self, errors: &str) -> Result<(), String> {
        crate::build::embedded_hal_fill::repair(&self.root, &self.item, errors, &self.llm_tx)
            .await
            .map(|_| ())
    }
}

/// A contract the user has just authored, seen by the verify spine.
///
/// The second implementation of the trait (the fill leg was the first), and the reason the spine is a
/// trait rather than a pair of closures: the *shape* is identical while almost every detail differs —
/// the artifact is a contract, the compiler is a host `cargo`, and there is nothing to repair.
struct ContractArtifact<'a> {
    manager: &'a BuildManagerActor,
    root: PathBuf,
    file: PathBuf,
    /// The contract crate to build, e.g. `weather-hal`.
    crate_name: String,
}

#[async_trait::async_trait]
impl crate::build::verify_spine::GeneratedArtifact for ContractArtifact<'_> {
    fn describe(&self) -> String {
        self.file.display().to_string()
    }

    /// Re-read the file that landed and parse it.
    ///
    /// The write validated the *submitted* text; this is the one check that says the file on disk is
    /// the file that was validated. Cheap, and it is what the drift measure's own parser would say.
    fn gate(&self) -> Result<(), String> {
        let content = std::fs::read_to_string(&self.file)
            .map_err(|e| format!("cannot re-read the written file: {e}"))?;
        let syntax = crate::build::hal_rust_contract::rust_syntax_check(&content);
        if syntax.ok {
            return Ok(());
        }
        let first = syntax
            .errors
            .first()
            .map(|e| format!("line {}:{}: {}", e.line, e.col, e.context))
            .unwrap_or_else(|| "unknown position".to_string());
        Err(format!("the written file does not parse ({first})"))
    }

    /// Build the contract crate **on the host**.
    ///
    /// No platform: the contract crate is `no_std` but dependency-free and host-testable by design
    /// (that is the point of the seam), so this needs no cross toolchain and no vendor SDK — which is
    /// exactly why an authored contract can be verified everywhere, unlike a backend.
    async fn build(&self) -> Result<(), crate::build::verify_spine::BuildFailure> {
        use crate::build::verify_spine::BuildFailure;
        let path_str = self.root.to_string_lossy().to_string();
        let metadata = match self.manager.get_analysis(&path_str).await {
            Some(metadata) => metadata,
            None => match self.manager.analyze_project(&self.root, None).await {
                Ok(metadata) => metadata,
                Err(e) => return Err(BuildFailure::Setup(format!("could not analyse: {e}"))),
            },
        };
        let Some(config) = metadata.config_files.first().cloned() else {
            return Err(BuildFailure::Setup(
                "the analysis names no config file".to_string(),
            ));
        };
        let module_tx = self
            .manager
            .module_tx_for(&config, None)
            .map_err(BuildFailure::Setup)?;
        let opts = BuildOptions {
            mode: "debug".to_string(),
            package: Some(self.crate_name.clone()),
            platform: None,
            target: None,
        };
        let (tx, rx) = oneshot::channel();
        module_tx
            .send(BuildModuleMessage::Build {
                path: self.root.clone(),
                metadata,
                opts,
                build_spec: None,
                reply_to: tx,
            })
            .await
            .map_err(|e| BuildFailure::Setup(format!("module channel closed: {e}")))?;
        match rx.await {
            Ok(Ok(output)) if output.success => Ok(()),
            Ok(Ok(output)) => Err(BuildFailure::Compiler(output.output)),
            Ok(Err(e)) => Err(BuildFailure::Setup(e)),
            Err(e) => Err(BuildFailure::Setup(format!("module response lost: {e}"))),
        }
    }

    /// Never reached: the caller runs the spine with no repair rounds, because the source is the
    /// user's own. Present because the trait asks, and honest about why.
    async fn repair(&self, _errors: &str) -> Result<(), String> {
        Err("an authored contract is not repaired by a model — fix the source".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::generic_helpers::hal_platform_library_hints;
    use crate::build::CargoBuildModule;
    use crate::Actor;

    use spire_actor::ServiceRegistry;
    use std::sync::Arc;

    #[test]
    fn ensure_impl_header_include_prepends_and_is_idempotent() {
        let model_output = "#include \"hal/api/h264_encoder.hpp\"\n\nnamespace hal { int x; }";
        let out = ensure_impl_header_include(model_output, "h264_encoder_a7s.hpp");
        assert!(
            out.starts_with(
                "#include \"h264_encoder_a7s.hpp\"\n\n#include \"hal/api/h264_encoder.hpp\""
            ),
            "must prepend the impl header: {out}"
        );
        assert_eq!(
            ensure_impl_header_include(&out, "h264_encoder_a7s.hpp"),
            out,
            "must be idempotent"
        );
        assert_eq!(ensure_impl_header_include(model_output, ""), model_output);
    }

    /// Verify is invoked with a SUBPROJECT path, but diagnostics are stored under
    /// the project root — the scope root must be resolved by walking up to the
    /// project marker, or the previous run's diagnostics are never superseded and
    /// the Build tab keeps showing errors that no longer reproduce.
    #[test]
    fn diagnostics_scope_root_walks_up_to_the_project_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("app");
        std::fs::create_dir_all(&sub).unwrap();

        assert_eq!(
            BuildManagerActor::diagnostics_scope_root(&sub),
            root,
            "a subproject path must resolve to the project root"
        );
        assert_eq!(
            BuildManagerActor::diagnostics_scope_root(&root),
            root,
            "the root resolves to itself"
        );
        // No marker anywhere → fall back to the path we were given (so a match
        // can still be attempted rather than silently scoping to nothing).
        let loose = tmp.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(BuildManagerActor::diagnostics_scope_root(&loose), loose);
    }

    /// Diagnostics recorded by a BUILD carry the compiler's verbatim path, and
    /// ninja makes it relative to the build dir (`../app/main.cpp`). Those MUST be
    /// treated as this project's: otherwise the supersede never clears them and
    /// stale build errors get re-reported by "Fix & Verify" run after run.
    #[test]
    fn diagnostic_in_project_accepts_build_dir_relative_paths() {
        let root = "/Users/me/ai-traps";
        // Relative (ninja/clang): this project's by construction.
        assert!(BuildManagerActor::diagnostic_in_project(
            "../app/main.cpp",
            root
        ));
        assert!(BuildManagerActor::diagnostic_in_project(
            "../hal/implementations/rock3c/camera_hal_rk.cpp",
            root
        ));
        // Absolute under the project: ours.
        assert!(BuildManagerActor::diagnostic_in_project(
            "/Users/me/ai-traps/build-rpi5/../app/main.cpp",
            root
        ));
        assert!(BuildManagerActor::diagnostic_in_project(
            "/Users/me/ai-traps/app/main.cpp",
            root
        ));
        // Absolute elsewhere: never deleted from a (possibly shared) graph.
        assert!(!BuildManagerActor::diagnostic_in_project(
            "/Users/me/other-project/main.cpp",
            root
        ));
        assert!(!BuildManagerActor::diagnostic_in_project(
            "/usr/include/stdio.h",
            root
        ));
        // A trailing slash on the root must not matter.
        assert!(BuildManagerActor::diagnostic_in_project(
            "/Users/me/ai-traps/x.cpp",
            "/Users/me/ai-traps/"
        ));
        // Empty paths are never a match.
        assert!(!BuildManagerActor::diagnostic_in_project("", root));
        assert!(!BuildManagerActor::diagnostic_in_project("   ", root));
    }

    /// A `fatal error:` line must yield a CLEAN path: a missing header is the most
    /// common cross-build failure, and a file recorded as `../x.cpp:41` (line
    /// number glued on) can never be located — so the fix loop silently skipped
    /// exactly the errors it was built to repair.
    #[test]
    fn fatal_error_lines_parse_to_a_resolvable_relative_path() {
        let output = "ninja: Entering directory `/proj/build-rpi5'\n\
                      [1/3] Compiling C++ object rock3c/libfoo.a.p/x.cpp.o\n\
                      ../hal/implementations/rock3c/camera_hal_rk.cpp:41:10: fatal error: 'rockchip/rk_mpi.h' file not found\n\
                      1 error generated.\n";
        let events = BuildManagerActor::parse_clang_output(output);
        assert_eq!(events.len(), 1, "one diagnostic: {events:?}");

        let event = &events[0];
        assert_eq!(
            event["file"], "../hal/implementations/rock3c/camera_hal_rk.cpp",
            "the path must not carry the line number: {event:?}"
        );
        assert_eq!(event["line_number"], 41);
        assert_eq!(event["column"], 10);
        assert_eq!(
            event["level"], "error",
            "a fatal error is an error, never a warning"
        );
        assert!(
            BuildManagerActor::diagnostic_in_project(
                event["file"].as_str().unwrap_or_default(),
                "/proj"
            ),
            "the recorded path must count as this project's, or the supersede skips it"
        );

        // The plain "error:" form must parse identically.
        let plain =
            BuildManagerActor::parse_clang_output("../app/main.cpp:27:5: error: no member\n");
        assert_eq!(plain.len(), 1, "{plain:?}");
        assert_eq!(plain[0]["file"], "../app/main.cpp");
        assert_eq!(plain[0]["line_number"], 27);
        assert_eq!(plain[0]["column"], 5);
        assert_eq!(plain[0]["level"], "error");
    }

    // Serializes tests that mutate the PROCESS-GLOBAL `SPIRE_PLATFORM_DIR`
    // env var (the platform registry seed). Cargo runs tests in parallel, so
    // two registry-dependent tests must never set it concurrently — the last
    // writer would break the other's `Platform::from_registry` lookup.
    // Shared across the crate via `crate::PLATFORM_DIR_TEST_LOCK` so the
    // cargo/meson scaffold tests (readers) serialize against these writers.

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

    /// The capability the ESP32 module advertises: it claims **no** config file (routing is by
    /// platform) and it is the one module that flashes.
    fn esp_module_capability() -> ModuleCapability {
        ModuleCapability {
            name: "esp-idf".to_string(),
            config_files: Vec::new(),
            build_system: "Cargo (esp-idf)".to_string(),
            language: "Rust".to_string(),
            source_extensions: vec!["rs".to_string()],
            mcp_servers: Vec::new(),
            supports_flash: true,
            supports_clean: false,
            supports_lint: false,
            supports_format: false,
            supports_fix: false,
        }
    }

    /// The capability a platform module advertises: it claims **no** config file (routing is by
    /// platform) and it is one of the modules that flashes. The rp2040 module's, as it registers
    /// itself — the two platform modules must not shadow each other.
    fn rp2040_module_capability() -> ModuleCapability {
        ModuleCapability {
            name: "rp2040".to_string(),
            config_files: Vec::new(),
            build_system: "Cargo (rp2040)".to_string(),
            language: "Rust".to_string(),
            source_extensions: vec!["rs".to_string()],
            mcp_servers: Vec::new(),
            supports_flash: true,
            supports_clean: false,
            supports_lint: false,
            supports_format: false,
            supports_fix: false,
        }
    }

    /// The capability of a config-file module: owns `Cargo.toml`, flashes nothing.
    fn cargo_capability() -> ModuleCapability {
        ModuleCapability {
            name: "cargo".to_string(),
            config_files: vec!["Cargo.toml".to_string()],
            build_system: "Cargo".to_string(),
            language: "Rust".to_string(),
            source_extensions: vec!["rs".to_string()],
            mcp_servers: Vec::new(),
            supports_clean: true,
            supports_lint: true,
            supports_format: true,
            supports_fix: true,
            supports_flash: false,
        }
    }

    /// The point of platform routing: an ESP32 build must reach the platform module, and
    /// **every other project must keep reaching its config module**.
    ///
    /// The second half is the one that matters. A routing change that quietly claimed
    /// ordinary Rust projects would be far worse than having no routing at all — that is the
    /// failure this whole mechanism exists to avoid, so it is asserted, not assumed.
    #[test]
    fn a_build_routes_by_platform_only_when_a_platform_module_exists() {
        let _guard = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("platform dir");
        std::fs::write(
            dir.path().join("esp32c6.yaml"),
            "id: esp32c6\nname: ESP32-C6\nos: esp-idf\narchitecture:\n  \
             cpu_family: riscv\n  cpu: esp32c6\n  endian: little\n  \
             target_triple: riscv32imac-esp-espidf\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rp2040.yaml"),
            "id: rp2040\nname: Raspberry Pi Pico\nos: rp2040\nfamily: rp2040\narchitecture:\n  \
             cpu_family: arm\n  cpu: rp2040\n  endian: little\n  \
             target_triple: thumbv6m-none-eabi\nrust:\n  target: thumbv6m-none-eabi\n  \
             idf_target: RP2040\n  flash: picotool\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rpi5.yaml"),
            "id: rpi5\nname: Raspberry Pi 5\nos: linux\narchitecture:\n  \
             cpu_family: aarch64\n  cpu: armv8-a\n  endian: little\n  \
             target_triple: aarch64-linux-gnu\n",
        )
        .unwrap();
        let _env = SpirePlatformDirGuard::set(dir.path());

        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        // The platform modules (as the ESP32 and rp2040 ones register themselves) — and note
        // they claim no config file, which is what makes the next registration possible.
        manager.add_platform_module(
            "esp-idf".to_string(),
            esp_module_capability(),
            mpsc::channel(1).0,
        );
        manager.add_platform_module(
            "rp2040".to_string(),
            rp2040_module_capability(),
            mpsc::channel(1).0,
        );
        manager.add_module(
            ModuleCapability {
                name: "cargo".to_string(),
                config_files: vec!["Cargo.toml".to_string()],
                build_system: "Cargo".to_string(),
                language: "Rust".to_string(),
                source_extensions: vec!["rs".to_string()],
                mcp_servers: Vec::new(),
                supports_clean: true,
                supports_lint: true,
                supports_format: true,
                supports_fix: true,
                supports_flash: false,
            },
            mpsc::channel(1).0,
        );

        // The esp-idf case: the platform module wins even though cargo owns the config file.
        assert_eq!(
            manager.route_for("Cargo.toml", Some("esp32c6")),
            BuildRoute::Platform("esp-idf".to_string())
        );

        // And the second platform module is reached just as directly: routing is by `os`, so
        // neither platform can shadow the other however many there are.
        assert_eq!(
            manager.route_for("Cargo.toml", Some("rp2040")),
            BuildRoute::Platform("rp2040".to_string())
        );

        // Everywhere else, nothing changes — including for a Linux *cross* target, which is
        // also a Cargo project with a platform set.
        assert_eq!(
            manager.route_for("Cargo.toml", Some("rpi5")),
            BuildRoute::Config("Cargo.toml".to_string())
        );
        assert_eq!(
            manager.route_for("Cargo.toml", None),
            BuildRoute::Config("Cargo.toml".to_string())
        );
        // An unknown platform id must not swallow the route either.
        assert_eq!(
            manager.route_for("Cargo.toml", Some("no-such-board")),
            BuildRoute::Config("Cargo.toml".to_string())
        );
    }

    /// `flash` is gated by capability *and* by platform, and each refusal names a different
    /// gap — wrong id, no flash step for that os, or a module that does not flash.
    ///
    /// It matters that these are refusals and not errors from the module: a module that cannot
    /// flash has no reply to send, so routing to it would surface as a lost channel, which
    /// reads as a crash rather than as "this board has no USB flash step".
    #[test]
    fn flash_routes_only_to_a_module_that_declares_it() {
        let _guard = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("platform dir");
        std::fs::write(
            dir.path().join("esp32c6.yaml"),
            "id: esp32c6\nname: ESP32-C6\nos: esp-idf\narchitecture:\n  \
             cpu_family: riscv\n  cpu: esp32c6\n  endian: little\n  \
             target_triple: riscv32imac-esp-espidf\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rp2040.yaml"),
            "id: rp2040\nname: Raspberry Pi Pico\nos: rp2040\nfamily: rp2040\narchitecture:\n  \
             cpu_family: arm\n  cpu: rp2040\n  endian: little\n  \
             target_triple: thumbv6m-none-eabi\nrust:\n  target: thumbv6m-none-eabi\n  \
             idf_target: RP2040\n  flash: picotool\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rpi5.yaml"),
            "id: rpi5\nname: Raspberry Pi 5\nos: linux\narchitecture:\n  \
             cpu_family: aarch64\n  cpu: armv8-a\n  endian: little\n  \
             target_triple: aarch64-linux-gnu\n",
        )
        .unwrap();
        let _env = SpirePlatformDirGuard::set(dir.path());

        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        manager.add_platform_module(
            "esp-idf".to_string(),
            esp_module_capability(),
            mpsc::channel(1).0,
        );
        manager.add_platform_module(
            "rp2040".to_string(),
            rp2040_module_capability(),
            mpsc::channel(1).0,
        );
        manager.add_module(cargo_capability(), mpsc::channel(1).0);

        // A flash-capable platform module is routed exactly as a build would be.
        assert_eq!(
            manager.route_for_flash("Cargo.toml", "esp32c6"),
            Ok(BuildRoute::Platform("esp-idf".to_string()))
        );
        assert_eq!(
            manager.route_for_flash("Cargo.toml", "rp2040"),
            Ok(BuildRoute::Platform("rp2040".to_string()))
        );

        // A board with no flash step: the message names both the platform and its os, so a
        // reader can tell a typo'd id from "this board is not flashed over USB".
        let err = manager
            .route_for_flash("Cargo.toml", "rpi5")
            .expect_err("a linux platform has no flash step");
        assert!(err.contains("rpi5") && err.contains("linux"), "{err}");

        // An id that is not in the registry: the same wording `run_esp_flash` uses, so the
        // message does not depend on how far the request got.
        let err = manager
            .route_for_flash("Cargo.toml", "no-such-board")
            .expect_err("an unknown platform cannot be flashed");
        assert!(err.contains("unknown platform"), "{err}");

        // Same platform, but the module says it does not flash: refused before routing.
        let mut no_flash = BuildManagerActor::new(mpsc::channel(1).0);
        let mut cap = esp_module_capability();
        cap.supports_flash = false;
        no_flash.add_platform_module("esp-idf".to_string(), cap, mpsc::channel(1).0);
        no_flash.add_module(cargo_capability(), mpsc::channel(1).0);
        let err = no_flash
            .route_for_flash("Cargo.toml", "esp32c6")
            .expect_err("a module without flash must be refused up front");
        assert!(err.contains("not supported for build system"), "{err}");
    }

    /// `build_build` — the **streaming** path a UI Build click and a `build_build` tool call
    /// share — must honour the platform when routing, exactly as the batch path does.
    ///
    /// Regression test for a bug found on hardware: the streaming path looked the module up
    /// with `platform: None`, so an ESP32 build went to the **cargo** module while the batch
    /// `build_project` sent the same request to the esp module. Two paths, two answers — and
    /// the wrong one is a host `cargo build` that "succeeds" without producing firmware.
    ///
    /// Both modules are fakes that stamp a distinct `command`, so the assertion is about *which
    /// module ran*, not about a build. The stored analysis is faked at the channel boundary
    /// (`GetConfig`), which is what keeps this a unit test: no MemoryGraph, no toolchain, no
    /// board.
    #[tokio::test]
    async fn build_build_routes_by_platform_not_to_the_config_owners_module() {
        let _guard = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = tempfile::tempdir().unwrap();
        std::fs::write(
            reg.path().join("esp32c6.yaml"),
            "id: esp32c6\nname: ESP32-C6\nos: esp-idf\narchitecture:\n  \
             cpu_family: riscv\n  cpu: esp32c6\n  endian: little\n  \
             target_triple: riscv32imac-esp-espidf\nrust:\n  \
             target: riscv32imac-esp-espidf\n  idf_target: esp32c6\n  flash: espflash\n",
        )
        .unwrap();
        let _env = SpirePlatformDirGuard::set(reg.path());

        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().to_string();

        // `get_analysis` asks the graph for the manager's own key; answer it with the analysis
        // an `AnalyzeProject` would have stored, and swallow anything else.
        let analysis_key = format!("build.analysis.{root}");
        let stored = serde_json::to_value(BuildMetadata {
            config_files: vec!["Cargo.toml".to_string()],
            ..Default::default()
        })
        .unwrap();
        let (mg_tx, mut mg_rx) = mpsc::channel::<MemoryGraphMessage>(8);
        tokio::spawn(async move {
            while let Some(msg) = mg_rx.recv().await {
                match msg {
                    MemoryGraphMessage::GetConfig { key, reply_to } => {
                        let value = (key == analysis_key).then(|| stored.clone());
                        let _ = reply_to.send(Ok(value));
                    }
                    MemoryGraphMessage::SetConfig { reply_to, .. } => {
                        let _ = reply_to.send(Ok(()));
                    }
                    _ => {}
                }
            }
        });

        let mut manager = BuildManagerActor::new(mg_tx);

        // The platform module — and the command it stamps is what proves it was chosen.
        let (esp_tx, mut esp_rx) = mpsc::channel::<BuildModuleMessage>(8);
        tokio::spawn(async move {
            while let Some(msg) = esp_rx.recv().await {
                if let BuildModuleMessage::BuildStreaming { reply_to, .. } = msg {
                    let _ = reply_to.send(Ok(BuildOutput {
                        success: true,
                        command: "esp-build".to_string(),
                        ..Default::default()
                    }));
                }
            }
        });
        manager.add_platform_module("esp-idf".to_string(), esp_module_capability(), esp_tx);

        // The module that owns `Cargo.toml`: registered, and must NOT be the one asked.
        let (cargo_tx, mut cargo_rx) = mpsc::channel::<BuildModuleMessage>(8);
        tokio::spawn(async move {
            while let Some(msg) = cargo_rx.recv().await {
                if let BuildModuleMessage::BuildStreaming { reply_to, .. } = msg {
                    let _ = reply_to.send(Ok(BuildOutput {
                        success: true,
                        command: "cargo-build".to_string(),
                        ..Default::default()
                    }));
                }
            }
        });
        manager.add_module(cargo_capability(), cargo_tx);

        let opts = BuildOptions {
            platform: Some("esp32c6".to_string()),
            mode: "release".to_string(),
            ..Default::default()
        };
        let (output, _events) = manager
            .build_project_with_events(std::path::Path::new(&root), &opts)
            .await
            .expect("the routed module answers");

        assert_eq!(
            output.command, "esp-build",
            "the esp-idf module must build for the esp platform, not the Cargo.toml owner"
        );
    }

    /// Per-platform HAL library hints must come from the registry YAML
    /// (`library_hints:`) instead of a hardcoded map, with a generic fallback
    /// when the platform YAML is absent or carries no hint.
    #[test]
    fn hal_library_hints_come_from_yaml_and_fall_back() {
        use tempfile::tempdir;

        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let reg = tempdir().unwrap();
        let dir = reg.path().join("platforms");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("fakeboard.yaml"),
            "id: fakeboard\nname: Fake Board\nlibrary_hints: |\n  Fake SDK (fakesdk.h), DMA via ioctl\n",
        )
        .unwrap();
        // A platform YAML without a `library_hints` key must still parse.
        std::fs::write(dir.join("plain.yaml"), "id: plain\nname: Plain\n").unwrap();

        let _restore = SpirePlatformDirGuard::set(&dir);

        let hint = hal_platform_library_hints("fakeboard");
        assert!(
            hint.contains("fakesdk.h"),
            "yaml library_hints must be used: {hint}"
        );
        assert!(
            !hint.contains("standard SDK and drivers"),
            "must not fall back when a hint is present: {hint}"
        );

        let plain = hal_platform_library_hints("plain");
        assert!(
            plain.contains("standard SDK and drivers"),
            "a platform without the key must fall back: {plain}"
        );

        let missing = hal_platform_library_hints("nosuchboard");
        assert!(
            missing.contains("standard SDK and drivers"),
            "an absent platform yaml must fall back: {missing}"
        );
    }

    /// Every action the UI's action row can invoke MUST be advertised by
    /// `list_tools()`: `build_default_registry` registers exactly the tools
    /// `ListTools` returns, so a tool handled by `call_tool` but missing there
    /// is silently unreachable through `tools/call` (it falls through to the
    /// MCP catch-all and fails instantly, on every target).
    ///
    /// Regression guard for build_clean / build_lint / build_format.
    #[test]
    fn ui_build_actions_are_registered_tools() {
        let names: Vec<String> = BuildManagerActor::list_tools()
            .into_iter()
            .map(|t| t.name)
            .collect();
        for name in [
            "build_analyze",
            "build_build",
            "build_verify",
            "build_test",
            "build_flash",
            "build_clean",
            "build_lint",
            "build_format",
            "build_fix",
        ] {
            assert!(
                names.iter().any(|n| n == name),
                "tool '{name}' is handled by call_tool but missing from list_tools(), \
                 so tools/call cannot reach it. Registered: {names:?}"
            );
        }
    }

    #[test]
    fn find_config_file_detects_known_names() {
        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        manager.add_module(
            ModuleCapability {
                name: "cargo".to_string(),
                config_files: vec!["Cargo.toml".to_string()],
                build_system: "Cargo".to_string(),
                language: "Rust".to_string(),
                source_extensions: vec!["rs".to_string()],
                supports_clean: true,
                supports_lint: true,
                supports_format: true,
                supports_fix: true,
                supports_flash: false,
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

    #[test]
    fn find_config_file_prefers_cargo_over_makefile_deterministically() {
        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        let add = |manager: &mut BuildManagerActor, name: &str, config: &str| {
            manager.add_module(
                ModuleCapability {
                    name: name.to_string(),
                    config_files: vec![config.to_string()],
                    build_system: name.to_string(),
                    language: "x".to_string(),
                    source_extensions: Vec::new(),
                    supports_clean: false,
                    supports_lint: false,
                    supports_format: false,
                    supports_fix: false,
                    supports_flash: false,
                    mcp_servers: vec![],
                },
                mpsc::channel(1).0,
            );
        };
        add(&mut manager, "cargo", "Cargo.toml");
        add(&mut manager, "make", "Makefile");

        let tmp = tempfile::tempdir().unwrap();
        // A Cargo workspace root that also carries a Makefile wrapper.
        std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(tmp.path().join("Makefile"), "build:\n").unwrap();

        // Deterministic: the primary build config (Cargo) wins every time.
        for _ in 0..5 {
            assert_eq!(
                manager.find_config_file(tmp.path()),
                Some("Cargo.toml".to_string())
            );
        }
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
        let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(mg_tx));

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
        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        manager.add_module(
            ModuleCapability {
                name: "cargo".to_string(),
                config_files: vec!["Cargo.toml".to_string()],
                build_system: "Cargo".to_string(),
                language: "Rust".to_string(),
                source_extensions: vec!["rs".to_string()],
                supports_clean: true,
                supports_lint: true,
                supports_format: true,
                supports_fix: true,
                supports_flash: false,
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
        let manager = BuildManagerActor::new(mpsc::channel(1).0);

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
            .call_tool(
                "hal_validate_contract",
                serde_json::json!({ "content": header }),
            )
            .await;
        assert_eq!(valid["valid"], serde_json::json!(true), "valid: {valid}");
        let summary = valid["summary"].as_str().expect("summary field");
        assert!(summary.contains("CameraHAL"), "summary: {summary}");
        assert!(
            summary.contains("start"),
            "summary must list start(): {summary}"
        );

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
        assert!(
            source.contains("CameraHAL::start"),
            "placeholder: {placeholder}\nsource: {source}"
        );
        assert!(
            source.contains("/* TODO: implement for rpi5 */"),
            "placeholder: {placeholder}\nsource: {source}"
        );

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
        assert_eq!(
            diff["added"][0],
            serde_json::json!("teardown"),
            "diff: {diff}"
        );
        assert!(
            diff["changed"]
                .as_array()
                .map(|a| a.iter().any(|p| p[0] == "capture"))
                .unwrap_or(false),
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
        let manager = BuildManagerActor::new(mpsc::channel(1).0);

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
    // The guard deliberately spans the whole test body to serialize the
    // process-global env mutation below; it is released when the test ends.
    #[allow(clippy::await_holding_lock)]
    async fn hal_build_impl_prompt_resolves_contract_and_registry_platform() {
        use tempfile::tempdir;

        // Serialize against the OTHER test that mutates the process-global
        // SPIRE_PLATFORM_DIR env var (they must never interleave).
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Fixture platform registry mirroring the real seed layout.
        let reg = tempdir().unwrap();
        let seed = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".spire")
            .join("platforms")
            .join("rpi5.yaml");
        if !seed.exists() {
            // Skip silently when the seed is absent (fresh environments).
            return;
        }
        std::fs::create_dir_all(reg.path().join("platforms")).unwrap();
        std::fs::copy(&seed, reg.path().join("platforms/rpi5.yaml")).unwrap();
        let _restore = SpirePlatformDirGuard::set(reg.path().join("platforms"));

        // The Stage-1 prompt must carry the platform's `library_hints:` — this
        // is how rpi5 tells the model it has a Coral Edge TPU (without it the
        // model defaults to CPU-only inference). Inject a marker when the seed
        // lacks one so the assertion is deterministic in any environment.
        let copied = reg.path().join("platforms/rpi5.yaml");
        let mut yaml = std::fs::read_to_string(&copied).unwrap();
        if !yaml.contains("library_hints:") {
            yaml.push_str("\nlibrary_hints: Fixture hint - FIXTURE-NPU-SDK.\n");
            std::fs::write(&copied, &yaml).unwrap();
        }
        let expected_hint = serde_yaml::from_str::<serde_yaml::Value>(&yaml)
            .expect("fixture yaml parses")
            .get("library_hints")
            .and_then(|v| v.as_str())
            .expect("fixture declares library_hints")
            .trim()
            .to_string();

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

        let manager = BuildManagerActor::new(mpsc::channel(1).0);
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
        assert!(
            prompt.contains("CameraHAL"),
            "must embed the contract class: {prompt}"
        );
        assert!(
            prompt.contains("CameraHalRpi5"),
            "must embed the concrete impl class: {prompt}"
        );
        assert!(
            result["class_name"].as_str() == Some("CameraHalRpi5"),
            "class_name: {result:?}"
        );
        assert!(
            prompt.contains("meson compile -C build-rpi5 camera_hal-rpi5"),
            "must embed the per-target gate: {prompt}"
        );
        // The registry `library_hints` must reach the prompt — this is the hook
        // that tells the model about the rpi5's Coral Edge TPU (and stops it
        // defaulting to CPU-only inference).
        let probe: String = expected_hint
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            prompt.contains(&probe),
            "prompt must embed the platform library_hints ({probe:?}): {prompt}"
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
    /// no panic, no partial writes, a clear "LLM unavailable" error.
    #[tokio::test]
    async fn hal_generate_impl_requires_configured_llm() {
        let manager = BuildManagerActor::new(mpsc::channel(1).0);
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
            err.contains("LLM unavailable"),
            "must reject when LLM is unconfigured: {err}"
        );
    }

    /// A model that always answers with `answer`, so the fill leg can be exercised without a
    /// provider: what is under test is the plumbing around the answer, not the answer.
    fn answering_llm(answer: &'static str) -> mpsc::Sender<LlmMessage> {
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let LlmMessage::Complete { reply_to, .. } = message {
                    let _ = reply_to.send(Ok(answer.to_string()));
                }
            }
        });
        tx
    }

    /// A scaffolded project in miniature: the contract's `led` trait and a backend whose body is
    /// still the placeholder.
    fn rust_hal_project(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("crates/demo-hal/src/hal")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/led.rs"),
            "/// A single binary output.\npub trait Led {\n    fn set(&mut self, on: bool);\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/demo-hal-esp32/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/lib.rs"),
            "use demo_hal::hal::Led;\n\npub struct GpioLed;\n\n\
             impl Led for GpioLed {\n    fn set(&mut self, _on: bool) {\n        \
             unimplemented!(\"GpioLed::set\")\n    }\n}\n",
        )
        .unwrap();
    }

    /// **The fill leg end to end, without a board and without a provider**: plan → generate → gate
    /// → write → re-measure. The verdict comes from the same coverage map the UI reads, so this
    /// asserts the loop a user actually runs rather than the file write alone.
    #[tokio::test]
    async fn embedded_hal_fill_apply_writes_what_the_gate_accepts() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        rust_hal_project(root);
        let backend = root.join("crates/demo-hal-esp32/src/lib.rs");

        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        manager.set_llm(answering_llm(
            "Sure — here it is:\n\n```rust\nuse demo_hal::hal::Led;\n\npub struct GpioLed;\n\n\
             impl Led for GpioLed {\n    fn set(&mut self, on: bool) {\n        let _ = on;\n    }\n}\n```\n",
        ));

        let plan = manager
            .call_tool(
                "embedded_hal_fill_plan",
                serde_json::json!({ "root": root.to_string_lossy() }),
            )
            .await;
        let items = plan["plan"].as_array().expect("plan items");
        assert_eq!(items.len(), 1, "{plan}");
        assert_eq!(items[0]["pending"][0]["status"], "stub", "{plan}");
        assert!(
            items[0]["prompt"].as_str().unwrap().contains("esp-idf-hal"),
            "the prompt names the vendor API: {plan}"
        );

        let applied = manager
            .call_tool(
                "embedded_hal_fill_apply",
                serde_json::json!({
                    "root": root.to_string_lossy(),
                    "plan": plan,
                }),
            )
            .await;
        assert_eq!(applied["failures"], serde_json::json!([]), "{applied}");
        assert_eq!(
            applied["applied"][0]["interfaces_done"],
            serde_json::json!(["led"]),
            "{applied}"
        );
        assert_eq!(
            applied["applied"][0]["interfaces_still_pending"],
            serde_json::json!([]),
            "{applied}"
        );
        // The build verification is *skipped* here, and says why: this fixture has no manifest at the
        // root, so nothing can be analyzed — let alone built — from it. "Written" and "verified to
        // build" are different claims, and a skip always names its reason rather than implying a
        // check happened.
        let note = applied["build_verification"].as_str().unwrap_or_default();
        assert!(
            note.starts_with("skipped:") && note.contains("could not analyze"),
            "an unbuildable project is reported as such, with the reason: {applied}"
        );

        // The fence came off, the body landed, the placeholder is gone, and the rest of the file
        // — the import and the struct — is what it was.
        let written = std::fs::read_to_string(&backend).unwrap();
        assert!(written.contains("let _ = on;"), "{written}");
        assert!(!written.contains("unimplemented!"), "{written}");
        assert!(written.contains("use demo_hal::hal::Led;"), "{written}");
        assert!(written.contains("pub struct GpioLed;"), "{written}");

        // And the indicator agrees: the interface is no longer in the missing queue.
        let coverage = manager
            .call_tool(
                "hal_missing_impls",
                serde_json::json!({ "root": root.to_string_lossy() }),
            )
            .await;
        assert_eq!(
            coverage["platforms"]["esp32"]["led"]["implemented"],
            serde_json::json!(true),
            "{coverage}"
        );
        assert_eq!(
            coverage["platforms"]["esp32"]["led"]["is_stub"],
            serde_json::json!(false),
            "{coverage}"
        );
    }

    /// The gate is what stands between a plausible answer and the file: an answer that keeps the
    /// placeholder is reported and **nothing is written**. A file that looks finished but is not is
    /// the worst outcome the placeholder convention exists to prevent.
    #[tokio::test]
    async fn embedded_hal_fill_apply_refuses_an_answer_that_keeps_the_placeholder() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        rust_hal_project(root);
        let backend = root.join("crates/demo-hal-esp32/src/lib.rs");
        let before = std::fs::read_to_string(&backend).unwrap();

        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        // Parses as Rust, declares the impl, provides the method — and still does nothing.
        manager.set_llm(answering_llm(
            "```rust\nimpl Led for GpioLed {\n    fn set(&mut self, _on: bool) {\n        \
             unimplemented!(\"GpioLed::set\")\n    }\n}\n```",
        ));

        let plan = manager
            .call_tool(
                "embedded_hal_fill_plan",
                serde_json::json!({ "root": root.to_string_lossy() }),
            )
            .await;
        let applied = manager
            .call_tool(
                "embedded_hal_fill_apply",
                serde_json::json!({ "root": root.to_string_lossy(), "plan": plan }),
            )
            .await;

        assert_eq!(applied["applied"], serde_json::json!([]), "{applied}");
        let reason = applied["failures"][0]["reason"].as_str().expect("a reason");
        assert!(reason.contains("unimplemented!()"), "{reason}");
        assert!(
            reason.contains("Led"),
            "the refusal names the trait: {reason}"
        );
        assert_eq!(
            std::fs::read_to_string(&backend).unwrap(),
            before,
            "the file is untouched"
        );
    }

    /// Step 3: `hal_missing_impls` must return an empty queue without stored
    /// analysis (no panic), and the `missing_implementation` message format it
    /// parses is already pinned by the Meson analyzer's container-layout test
    /// ("HAL interface {stem} has no implementation for platform {plat}").
    #[tokio::test]
    async fn hal_missing_impls_returns_empty_queue_without_analysis() {
        let manager = BuildManagerActor::new(mpsc::channel(1).0);
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

        let manager = BuildManagerActor::new(mpsc::channel(1).0);
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
            meson.contains("hal_impl_rpi5_sources = files(")
                && meson.contains("'implementations/rpi5/camera_hal_stub.cpp'"),
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
        std::fs::write(
            root.join("hal/meson.build"),
            "hal_impl_rpi5_sources = files()\n",
        )
        .unwrap();

        // Plan → apply (none → whole-class stub).
        let plan = crate::actors::hal_fill::plan(root, "rpi5", &[]);
        let items = plan["plan"].as_array().expect("plan items");
        assert_eq!(items.len(), 1, "one video_scaler item: {plan}");
        let analyze = async { Ok(()) };
        let result =
            crate::actors::hal_fill::apply(root, &serde_json::json!(items), Box::pin(analyze))
                .await;
        assert!(
            result["failures"].as_array().unwrap().is_empty(),
            "no 'no classes' failure: {result}"
        );

        // The module-pair definition for a NEW class ("none") lands at
        // `video_scaler_rpi5.cpp` (the concrete derived class, not the contract
        // abstract name), plus its `.hpp` declaration — written atomically.
        let stub =
            std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.cpp"))
                .unwrap();
        assert!(stub.contains("SPIRE-HAL-STUB"), "sentinel: {stub}");
        assert!(stub.contains("#pragma message("), "pragma: {stub}");
        assert!(
            stub.contains("VideoScalerRpi5::scale_nv12("),
            "stub body: {stub}"
        );
        for tok in ["src_fd", "src_w", "src_h", "dst_fd", "dst_w", "dst_h"] {
            assert!(stub.contains(tok), "missing param {tok}: {stub}");
        }
        // The concrete declaration header exists, derived from the contract
        // base (`IVideoScaler`) and declaring the same class name.
        let header =
            std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.hpp"))
                .unwrap();
        assert!(header.contains("class VideoScalerRpi5"), "header: {header}");
        assert!(
            header.contains("IVideoScaler"),
            "derives contract base: {header}"
        );
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
        std::fs::write(
            root.join("hal/meson.build"),
            "hal_impl_rpi5_sources = files()\n",
        )
        .unwrap();

        // Plan: rpi5 → camera_hal (partial, missing capture) + video_scaler
        // (none → whole-class stub).
        let plan = crate::actors::hal_fill::plan(root, "rpi5", &[]);
        let items = plan["plan"].as_array().expect("plan items");
        assert_eq!(items.len(), 2, "one partial + one none: {plan}");

        let analyze = async { Ok(()) };
        let result =
            crate::actors::hal_fill::apply(root, &serde_json::json!(items), Box::pin(analyze))
                .await;
        assert!(
            result["failures"].as_array().unwrap().is_empty(),
            "{result}"
        );

        // Partial gap stub: sentinel + pragma + the missing method body only.
        let gap = std::fs::read_to_string(root.join("hal/implementations/rpi5/camera_hal_gap.cpp"))
            .unwrap();
        assert!(gap.contains("SPIRE-HAL-STUB"), "partial sentinel: {gap}");
        assert!(gap.contains("#pragma message("), "partial pragma: {gap}");
        assert!(
            gap.contains("CameraHAL::capture(int timeout_ms)"),
            "partial body: {gap}"
        );
        assert!(
            !gap.contains("CameraHAL::start()"),
            "partial must not re-add start: {gap}"
        );

        // Whole-class module pair: the definition (sentinel + pragma + every
        // contract method, using the CONCRETE derived class name from the new
        // naming scheme) plus the `.hpp` declaration — written atomically.
        let stub =
            std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.cpp"))
                .unwrap();
        assert!(stub.contains("SPIRE-HAL-STUB"), "none sentinel: {stub}");
        assert!(stub.contains("#pragma message("), "none pragma: {stub}");
        assert!(
            stub.contains("VideoScalerRpi5::resize(int w, int h)"),
            "none body: {stub}"
        );
        let header =
            std::fs::read_to_string(root.join("hal/implementations/rpi5/video_scaler_rpi5.hpp"))
                .unwrap();
        assert!(
            header.contains("class VideoScalerRpi5"),
            "none header: {header}"
        );
        assert!(
            header.contains("VideoScaler"),
            "derives contract base: {header}"
        );

        // The coverage/fill queue must report BOTH as still needing
        // implementation (the sentinel marks them as stubs, not implemented).
        let cov = crate::build::generic_helpers::hal_interface_coverage(
            &[
                crate::build::generic_helpers::HalContractMethod {
                    name: "start".into(),
                    return_type: "bool".into(),
                    params: "".into(),
                },
                crate::build::generic_helpers::HalContractMethod {
                    name: "capture".into(),
                    return_type: "std::uint32_t".into(),
                    params: "int timeout_ms".into(),
                },
            ],
            "camera_hal",
            &root.join("hal/implementations/rpi5"),
        );
        assert!(
            !cov.implemented,
            "partial stub must still be unimplemented: {cov:?}"
        );
    }

    /// Project-level "add platform" round-trip: `hal_add_platform` must scaffold
    /// the FULL new platform surface into an existing HAL project — <plat>/
    /// (meson.build + main.cpp), per-contract `SPIRE-HAL-STUB` placeholders,
    /// hal/meson.build wiring, root subdir('<plat>'), and meson_options.txt —
    /// then (best-effort) re-analyze. Uses the real registry seed rpi5.yaml via
    /// a fixture SPIRE_PLATFORM_DIR.
    #[tokio::test]
    // The guard deliberately spans the whole test body to serialize the
    // process-global env mutation below; it is released when the test ends.
    #[allow(clippy::await_holding_lock)]
    async fn hal_add_platform_scaffolds_full_platform_surface() {
        use tempfile::tempdir;

        // Serialize against the OTHER test that mutates the process-global
        // SPIRE_PLATFORM_DIR env var (they must never interleave).
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Fixture platform registry mirroring the real seed layout.
        let reg = tempdir().unwrap();
        let seed = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".spire")
            .join("platforms")
            .join("rpi5.yaml");
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
        // The template carries RPi5-specific external deps + a -DHAVE_RPI5 define;
        // hal_add_platform must NOT copy these into the new platform.
        std::fs::write(
            root.join("rpi5/meson.build"),
            r#"cpp = meson.get_compiler('cpp')
rpi5_hal_sources = hal_impl_rpi5_sources
platform_deps = []
if platform == 'rpi5'
  add_project_arguments('-DHAVE_RPI5', language: 'cpp')
  libcamera_dep = dependency('libcamera', required: true)
  tflite_dep    = cpp.find_library('tensorflow-lite', dirs: ['/opt/cross/sysroot/rpi5/usr/lib/aarch64-linux-gnu'], required: true)
  edgetpu_dep   = cpp.find_library('edgetpu', dirs: ['/opt/cross/sysroot/rpi5/usr/lib/aarch64-linux-gnu'], required: true)
  platform_deps += [libcamera_dep, tflite_dep, edgetpu_dep]
endif
executable('ai-trap-rpi5', 'main.cpp' + rpi5_hal_sources, dependencies: core_deps + platform_deps)
"#,
        )
        .unwrap();
        std::fs::write(root.join("rpi5/main.cpp"), "int main() { return 0; }\n").unwrap();

        let manager = BuildManagerActor::new(mpsc::channel(1).0);
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
            result["error"]
                .as_str()
                .map(|e| e.contains("already present"))
                .unwrap_or(false),
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
        assert_eq!(
            result["interfaces"][0],
            serde_json::json!("camera_hal"),
            "{result}"
        );

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
        assert!(
            root_meson.contains("subdir('rock3c')"),
            "root meson: {root_meson}"
        );

        // 4. meson_options.txt Valid values include rock3c.
        let opts = std::fs::read_to_string(root.join("meson_options.txt")).unwrap();
        assert!(
            opts.to_lowercase()
                .contains("valid values: host, rpi5, rock3c"),
            "options: {opts}"
        );

        // 5. <plat>/meson.build: a CLEAN skeleton — the template's RPi5-specific
        // external deps and -DHAVE_RPI5 must NOT be copied in.
        let plat_meson = std::fs::read_to_string(root.join("rock3c/meson.build")).unwrap();
        assert!(
            plat_meson.contains("hal_impl_rock3c_sources"),
            "plat meson: {plat_meson}"
        );
        for leaked in [
            "libcamera",
            "tensorflow-lite",
            "edgetpu",
            "HAVE_RPI5",
            "/opt/cross/sysroot/rpi5",
        ] {
            assert!(
                !plat_meson.contains(leaked),
                "template-specific '{leaked}' leaked into the new platform meson.build:\n{plat_meson}"
            );
        }
        assert!(
            plat_meson.contains("-DHAVE_ROCK3C"),
            "correct uppercase define missing: {plat_meson}"
        );
        assert!(
            plat_meson.contains("TODO(rock3c)"),
            "platform-deps TODO missing: {plat_meson}"
        );
        // Root meson.build must GATE the new subdir on -Dplatform (an
        // unconditional subdir() would build the target for every platform).
        assert!(
            root_meson.contains("if platform == 'rock3c'")
                && root_meson.contains("subdir('rock3c')"),
            "root meson must gate subdir('rock3c'): {root_meson}"
        );

        // 5b. <plat>-cross.txt generated from the platform registry record.
        let cross = std::fs::read_to_string(root.join("rock3c/rock3c-cross.txt")).unwrap();
        assert!(
            cross.contains("-target") && cross.contains("--sysroot=/tmp/rock3c-sysroot"),
            "cross file must be generated from the registry: {cross}"
        );

        // 6. The NEW architecture: there is NO per-platform main.cpp. Instead the
        // ONE binding (app/platform_hal_<plat>.cpp) + the concrete aggregate HAL.
        assert!(
            !root.join("rock3c/main.cpp").exists(),
            "must not scaffold a per-platform main.cpp (the app has one generic main)"
        );
        let ph = std::fs::read_to_string(root.join("app/platform_hal_rock3c.cpp")).unwrap();
        assert!(ph.contains("create_platform_hal"), "platform binding: {ph}");
        assert!(
            ph.contains("AiTrapHalRock3c"),
            "must construct the aggregate: {ph}"
        );

        let agg_hpp =
            std::fs::read_to_string(root.join("hal/implementations/rock3c/ai_trap_hal_rock3c.hpp"))
                .unwrap();
        assert!(
            agg_hpp.contains("class AiTrapHalRock3c : public AiTrapHal"),
            "aggregate header: {agg_hpp}"
        );
        assert!(
            agg_hpp.contains("SPIRE-HAL-STUB"),
            "aggregate sentinel: {agg_hpp}"
        );
        let agg_cpp =
            std::fs::read_to_string(root.join("hal/implementations/rock3c/ai_trap_hal_rock3c.cpp"))
                .unwrap();
        assert!(
            agg_cpp.contains("AiTrapHalRock3c::AiTrapHalRock3c()"),
            "aggregate source: {agg_cpp}"
        );

        // The platform meson compiles the SHARED generic app, not a local main.
        assert!(
            plat_meson.contains("../app/main.cpp"),
            "plat meson: {plat_meson}"
        );
        assert!(
            plat_meson.contains("../app/platform_hal_rock3c.cpp"),
            "plat meson: {plat_meson}"
        );

        // The aggregate is wired into the platform's HAL sources.
        assert!(
            hal_meson.contains("ai_trap_hal_rock3c.cpp"),
            "hal meson must link the aggregate: {hal_meson}"
        );
    }

    #[tokio::test]
    async fn parse_source_file_routes_to_cargo_module() {
        use std::io::Write;
        use tempfile::tempdir;

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
        let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(mg_tx));

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

    /// A contract the user authored, through the same spine the fill leg uses.
    ///
    /// Two levels, both cheap because the fixture is a project Spire cannot build from: the write
    /// still lands (and is wired), and the host build is reported as **not built** with a reason
    /// rather than as a broken contract. The build itself needs a real cargo project; what is pinned
    /// here is that the verification runs, that its three-valued `built` is honest, and that a file
    /// which does not parse is refused by the gate before any build is attempted.
    #[tokio::test]
    async fn a_written_contract_is_verified_and_says_what_it_could_not_check() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        // A contract crate with a home for the write, but no workspace manifest: nothing to build
        // from, which is exactly the "cannot check" case.
        std::fs::create_dir_all(root.join("crates/demo-hal/src/hal")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/mod.rs"),
            "pub mod led;\n\npub use led::Led;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/led.rs"),
            "pub trait Led {\n    fn set(&mut self, on: bool);\n}\n",
        )
        .unwrap();

        let mut manager = BuildManagerActor::new(mpsc::channel(1).0);
        manager.set_llm(answering_llm("unused"));

        let written = manager
            .call_tool(
                "embedded_hal_write_contract",
                serde_json::json!({
                    "root": root.to_string_lossy(),
                    "filename": "sensor.rs",
                    "content": "pub trait Sensor {\n    fn read(&mut self) -> i32;\n}\n",
                }),
            )
            .await;
        assert_eq!(written["valid"], serde_json::json!(true), "{written}");
        assert_eq!(written["wired"], serde_json::json!(true), "{written}");
        let host_build = &written["host_build"];
        assert_eq!(
            host_build["built"],
            serde_json::Value::Null,
            "no compiler ran, and the result says so rather than claiming a pass: {written}"
        );
        let not_built = host_build["not_built"].as_str().unwrap_or_default();
        assert!(
            not_built.contains("could not analyse"),
            "and names the reason the check could not run: {written}"
        );

        // The layer that refuses bad *input* is the write's own validation, before anything lands —
        // the gate on the spine re-reads what is already there, so this is where a parse error is
        // caught first.
        let broken = "pub trait Sensor {\n    fn read(&mut self) -> i32\n}\n";
        let refused_write = manager
            .call_tool(
                "embedded_hal_write_contract",
                serde_json::json!({
                    "root": root.to_string_lossy(),
                    "filename": "broken.rs",
                    "content": broken,
                }),
            )
            .await;
        assert_eq!(
            refused_write["valid"],
            serde_json::json!(false),
            "{refused_write}"
        );
        assert!(
            refused_write["error"]
                .as_str()
                .unwrap_or_default()
                .contains("does not parse"),
            "{refused_write}"
        );
        assert!(
            !root.join("crates/demo-hal/src/hal/broken.rs").exists(),
            "an invalid contract never touches disk"
        );
    }

    /// The spine's **gate**, on the contract artifact: it re-reads the file that landed, so a file
    /// that changed under us (or was never written) is refused *before* a build is attempted.
    ///
    /// Unreachable through the tool — `write_contract` validates the submitted text first, which is
    /// why it is tested here directly rather than by trying to sneak a broken file past the write.
    #[tokio::test]
    async fn the_contract_gate_re_reads_what_is_on_disk() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        let broken = root.join("hal.rs");
        std::fs::write(
            &broken,
            "pub trait Sensor {\n    fn read(&mut self) -> i32\n}\n",
        )
        .unwrap();

        let manager = BuildManagerActor::new(mpsc::channel(1).0);
        let artifact = ContractArtifact {
            manager: &manager,
            root: root.to_path_buf(),
            file: broken,
            crate_name: "demo-hal".to_string(),
        };
        let outcome = crate::build::verify_spine::verify_generated(&artifact, 0).await;
        assert_eq!(outcome.built, None, "no compiler ran: {outcome:?}");
        let refused = outcome.refused.unwrap_or_default();
        assert!(refused.contains("does not parse"), "{refused}");
        assert!(
            refused.contains("hal.rs"),
            "the gate names the file: {refused}"
        );

        // A missing file is a refusal too, not a panic.
        let artifact = ContractArtifact {
            manager: &manager,
            root: root.to_path_buf(),
            file: root.join("gone.rs"),
            crate_name: "demo-hal".to_string(),
        };
        let outcome = crate::build::verify_spine::verify_generated(&artifact, 0).await;
        let refused = outcome.refused.clone().unwrap_or_default();
        assert!(refused.contains("cannot re-read"), "{refused:?}");
    }
}
