// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Build modules — static child actors that own language/build-system logic.
//!
//! Each build module is a long-lived `ChildActor` spawned once at startup.
//! The `BuildManagerActor` maintains a router (config filename → module sender)
//! and is the single point of access for all build/analysis actions.
//!
//! Design:
//! - Modules are **static** (spawned once, queried via `DescribeCapabilities`)
//! - `Analyze` produces analysis from scratch; `Build`/`Test` receive the
//!   previously-computed analysis in the message (no module-internal state)
//! - Analysis is persisted in the knowledge graph by the manager, not held
//!   by the modules themselves

use std::path::PathBuf;
use tokio::sync::oneshot;

use spire_core::build_types::{BuildMetadata, BuildSpec};
use serde::{Deserialize, Serialize};

pub mod cargo;
pub use cargo::CargoBuildModule;
pub mod node;
pub use node::NodeBuildModule;
pub mod swift;
pub use swift::SwiftBuildModule;
pub mod generic_helpers;
pub mod ast_parser;
pub use ast_parser::{
    javascript_language_config, parse_with_tree_sitter, python_language_config,
    rust_language_config,
};
pub use ast_parser::LanguageConfig;

pub mod python;
pub use python::PythonBuildModule;
pub mod go;
pub use go::GoBuildModule;
pub mod maven;
pub use maven::MavenBuildModule;
pub mod gradle;
pub use gradle::GradleBuildModule;
pub mod cmake;
pub use cmake::CmakeBuildModule;
pub mod make;
pub use make::MakeBuildModule;
pub mod ruby;
pub use ruby::RubyBuildModule;
pub mod meson;
pub use meson::MesonBuildModule;
pub mod hal_migration;

/// Shared message protocol implemented by every build module.
#[derive(Debug)]
pub enum BuildModuleMessage {
    /// Describe this module's capabilities (config files, language, build system).
    DescribeCapabilities {
        reply_to: oneshot::Sender<ModuleCapability>,
    },
    /// Parse a source file into AST nodes/edges. The result is returned to the
    /// caller; the build module never touches the graph (single-writer rule:
    /// BuildManager persists the result).
    ParseSourceFile {
        file_path: PathBuf,
        reply_to: oneshot::Sender<Result<AstParseResult, String>>,
    },
    /// Produce analysis from scratch for a project directory.
    Analyze {
        path: PathBuf,
        reply_to: oneshot::Sender<Result<BuildMetadata, String>>,
    },
    /// Build a project using previously-computed analysis.
    Build {
        path: PathBuf,
        metadata: BuildMetadata,
        opts: BuildOptions,
        /// Normalized invocation for this target (command + args + env) from the
        /// selected `BuildTarget.build_spec`. When present the module executes it
        /// directly; when absent the module falls back to its per-tool logic.
        build_spec: Option<BuildSpec>,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Build a project while streaming per-line events (e.g. "Compiling serde").
    BuildStreaming {
        path: PathBuf,
        metadata: BuildMetadata,
        opts: BuildOptions,
        /// Same semantics as `Build::build_spec`.
        build_spec: Option<BuildSpec>,
        event_tx: tokio::sync::mpsc::UnboundedSender<BuildEvent>,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Lint while streaming per-line events.
    LintStreaming {
        path: PathBuf,
        metadata: BuildMetadata,
        /// Optional cross-platform target (e.g. "rpi5") selecting which build
        /// dir's compile_commands.json the linter should use.
        platform: Option<String>,
        event_tx: tokio::sync::mpsc::UnboundedSender<BuildEvent>,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Auto-fix while streaming per-line events.
    FixStreaming {
        path: PathBuf,
        metadata: BuildMetadata,
        event_tx: tokio::sync::mpsc::UnboundedSender<BuildEvent>,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Run tests using previously-computed analysis.
    Test {
        path: PathBuf,
        metadata: BuildMetadata,
        opts: TestOptions,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Run the project's clean command (e.g. cargo clean).
    Clean {
        path: PathBuf,
        metadata: BuildMetadata,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Run the project's linter (e.g. cargo clippy).
    Lint {
        path: PathBuf,
        metadata: BuildMetadata,
        /// Optional cross-platform target (e.g. "rpi5") selecting which build
        /// dir's compile_commands.json the linter should use.
        platform: Option<String>,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Run the project's formatter check (e.g. cargo fmt --check).
    Format {
        path: PathBuf,
        metadata: BuildMetadata,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Apply auto-fixes for warnings/errors (e.g. cargo fix --allow-dirty).
    Fix {
        path: PathBuf,
        metadata: BuildMetadata,
        reply_to: oneshot::Sender<Result<BuildOutput, String>>,
    },
    /// Invoke an LLM-facing tool generically (JSON args/result).
    CallTool {
        tool_name: String,
        args: serde_json::Value,
        reply_to: oneshot::Sender<serde_json::Value>,
    },
    /// Generate a minimal build-config + source scaffold for a new project.
    /// Language modules own their templates (polyglot modularity). `platforms`
    /// holds the requested cross-compilation targets (registry ids, e.g.
    /// `["host"]`, `["rpi5","rock3c"]`). Modules that support multi-platform
    /// layouts render per-platform config/source files; other modules ignore it.
    ScaffoldBuildConfig {
        project_name: String,
        goal: String,
        platforms: Vec<String>,
        /// Structural shape (Native / SingleSource / Hal). `Hal` emits the
        /// `hal/` container scaffold; other modules/toolchains ignore it.
        structure: spire_core::build_types::ProjectStructure,
        /// True when the project is embedded (cross-compiled targets only).
        /// Modules may use it to exclude a host build target or unlock
        /// per-platform HAL scaffolding.
        embedded: bool,
        reply_to: oneshot::Sender<Result<ScaffoldOutput, String>>,
    },
}

/// A single file in a scaffolded project (multi-file layouts).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScaffoldFile {
    /// Path relative to the project root, e.g. "crates/core/Cargo.toml".
    pub path: String,
    /// File content.
    pub content: String,
    /// Structural files (build configs, workspace wiring, meson_options.txt,
    /// .cargo/config.toml) are immutable to the LLM's raw file writes.
    /// Source stubs are fillable. Default = structural (conservative).
    #[serde(default = "default_true")]
    pub structural: bool,
    /// Source role of a FILLABLE file so the contract distinguishes a HAL
    /// implementation from plain app source. Empty = not fillable / structural.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_role: Option<spire_core::build_types::SourceRole>,
}

/// Minimal scaffold generated by a language module for a new project.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScaffoldOutput {
    /// Build config filename, e.g. "Cargo.toml".
    pub build_file: String,
    /// Build config content.
    pub build_content: String,
    /// Source directory (empty = files at root).
    pub source_dir: String,
    /// Source file, e.g. "src/main.rs".
    pub source_file: String,
    /// Source file content.
    pub source_content: String,
    /// Full multi-file layout for multi-target/multi-platform scaffolds.
    /// Empty = consumers use the legacy single-file fields above.
    #[serde(default)]
    pub files: Vec<ScaffoldFile>,
    /// Register ids the scaffold targets (echoed back for consumers/UI).
    #[serde(default)]
    pub platform_targets: Vec<String>,
    /// Directories under which the LLM may create/modify files during the
    /// fill phase. Everything else is structural (locked). Empty = no fill
    /// allowed (all structural).
    #[serde(default)]
    pub fill_roots: Vec<String>,
    /// Build-config dependency sections the LLM may edit ONLY through the
    /// module's `declare_dependencies` tool (e.g. leaf `[dependencies]`,
    /// Meson `platform_deps`). Never written directly.
    #[serde(default)]
    pub dependency_sections: Vec<String>,
    /// Structural shape the scaffold was emitted for (Native / SingleSource /
    /// Hal). Consumers use it to set fill expectations and preview labels.
    #[serde(default, skip_serializing_if = "is_native_structure")]
    pub structure: spire_core::build_types::ProjectStructure,
    /// True when the project is embedded (cross-compiled targets only — never
    /// a host build target). The wizard sets this for embedded projects.
    #[serde(default)]
    pub embedded: bool,
}

fn is_native_structure(s: &spire_core::build_types::ProjectStructure) -> bool {
    *s == spire_core::build_types::ProjectStructure::Native
}

/// A streaming build event emitted per output line (e.g. "Compiling X").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildEvent {
    /// Raw output line(s) — for block-level warnings/errors this contains the
    /// full multi-line block text joined with newlines.
    pub line: String,
    /// "compiling", "warning", "error", "finished", "info"
    pub level: String,
    /// Parsed crate/file name when the line is "Compiling X Y".
    #[serde(default)]
    pub target: Option<String>,
    /// File path parsed from "  --> path:line:col" inside a warning/error block.
    #[serde(default)]
    pub file: Option<String>,
    /// Line number parsed from "  --> path:line:col".
    #[serde(default)]
    pub line_number: Option<u32>,
    /// The message text (first line of a warning/error block, prefix stripped).
    #[serde(default)]
    pub message: Option<String>,
    /// Multi-line detail/context (the rest of the block after the message).
    #[serde(default)]
    pub detail: Option<String>,
}

/// An MCP server that a build module requires for full functionality.
///
/// Modules declare these so the BuildManager / FFI bootstrap can provision
/// the server automatically (install + connect) instead of requiring a
/// static entry in `config/mcp-config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDependency {
    /// Logical name (e.g. "crates-io", "npmjs").
    pub name: String,
    /// Cargo crate or npm package to install (e.g. "crates-mcp").
    #[serde(default)]
    pub package: String,
    /// Shell command to (re)install the server (e.g. "cargo install crates-mcp").
    #[serde(default)]
    pub install_command: String,
    /// Client path — if empty, `package` is assumed to be on PATH.
    #[serde(default)]
    pub command: String,
    /// CLI arguments passed to the server on launch.
    #[serde(default)]
    pub args: Vec<String>,
    /// Whether to auto-start the server at bootstrap.
    #[serde(default = "default_true")]
    pub autostart: bool,
    /// Build system this server serves (e.g. "Cargo", "npm"). When present,
    /// the server's tools are only exposed if the project has a matching
    /// BuildSystem node.
    #[serde(default)]
    pub build_type: Option<String>,
    /// Which plan step types are allowed to use tools from this server.
    /// Empty = tools are available to any step.
    #[serde(default)]
    pub allowed_for_steps: Vec<String>,
    /// Specific tool names whitelisted for the current language module.
    /// Empty = all tools from this server are allowed (not recommended for
    /// untrusted servers); non-empty = ONLY these tools may be called.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

fn default_true() -> bool { true }

/// Declared by each module at startup so the manager can build its router.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleCapability {
    /// Module name (e.g. "cargo", "node").
    pub name: String,
    /// Config files this module handles (e.g. ["Cargo.toml"]).
    pub config_files: Vec<String>,
    /// Build system label (e.g. "Cargo", "npm", "Python").
    pub build_system: String,
    /// Primary language (e.g. "Rust", "JavaScript", "Swift").
    pub language: String,
    /// Source file extensions this module can parse (e.g. ["rs"] or ["js", "ts"]).
    /// Used by BuildManager to build the AST-parsing extension router.
    #[serde(default)]
    pub source_extensions: Vec<String>,
    /// MCP servers this module depends on. BuildManager/FFI bootstrap
    /// aggregates these and provisions them for the McpClientActor.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerDependency>,
}

// ============================================================================
// AST Parsing Types
// ============================================================================

/// Output of source file parsing — returned by the module, persisted by BuildManager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstParseResult {
    pub file_path: String,
    pub language: String,
    /// SHA-256 of file content (used for incremental re-parse).
    pub content_hash: String,
    pub has_errors: bool,
    pub nodes: Vec<AstNodeData>,
    pub edges: Vec<AstEdgeData>,
    /// Annotated doc comments (structured `@tags` + prose) parsed from the
    /// source. For C/C++ HAL headers this is the output of `parse_hal_docs`,
    /// so the persistent AST carries the same self-describing docs as the
    /// HAL graph/viewer.
    #[serde(default)]
    pub docs: Vec<crate::build::generic_helpers::HalDoc>,
}

/// A single AST node extracted from a source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstNodeData {
    /// "function", "class", "import", "variable", "block", ...
    pub node_type: String,
    pub name: Option<String>,
    /// Raw source text for the node.
    pub text: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    /// Nesting depth in the AST.
    pub depth: u32,
    pub is_public: bool,
    pub is_async: bool,
    pub signature: Option<String>,
    pub return_type: Option<String>,
    /// Indices into the `nodes` vec (children of this node).
    pub children: Vec<usize>,
}

/// A directed edge between two nodes in `AstParseResult::nodes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstEdgeData {
    /// Index into `nodes` (source).
    pub from_index: usize,
    /// Index into `nodes` (target).
    pub to_index: usize,
    /// "child", "calls", "imports", "references".
    pub edge_type: String,
    /// Child ordering / call ordering.
    pub order: Option<u32>,
    /// Structural field name (e.g. "body", "condition").
    pub field: Option<String>,
}

/// Result of persisting a parsed file into the graph.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParseSummary {
    pub nodes_written: usize,
    pub edges_written: usize,
    /// True if the content hash matched an existing SourceFile node → no-op.
    pub skipped: bool,
}

/// Options controlling a build.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuildOptions {
    /// Build profile: "debug" (default) or "release".
    pub mode: String,
    /// Optional workspace member / package name to target. Generically
    /// interpreted by each build module (Cargo: `--package <name>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Optional cross-platform target (e.g. "host" or "rpi5" for Meson
    /// projects with a `platform` option). The build module uses this to
    /// select the platform-specific build dir (`build-native` vs `build-rpi5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Optional specific build target within the project (e.g. Meson
    /// `executable('myapp-rpi', ...)` target name passed to
    /// `meson compile -C <dir> <target>`). When present, only that target
    /// (and its dependencies) is built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// Options controlling a test run.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TestOptions {
    /// Optional test filter (e.g. test name prefix).
    pub filter: Option<String>,
}

/// Structured output from a build/test execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuildOutput {
    pub success: bool,
    pub command: String,
    pub duration_secs: f64,
    pub output: String,
    pub exit_code: Option<i32>,
}

// ============================================================================
// Module Registry (for discovery)
/// Map a build-config filename to the build-system label of the module that
/// owns it. Falls back to "Unknown" for unregistered configs.
pub fn build_system_for_config(filename: &str) -> &'static str {
    match filename {
        "Cargo.toml" => "Cargo",
        "package.json" | "pnpm-workspace.yaml" => "npm",
        "Package.swift" => "SwiftPM",
        "pyproject.toml" | "setup.py" | "setup.cfg" => "Python",
        "go.mod" => "Go",
        "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" => "Gradle",
        "pom.xml" => "Maven",
        "CMakeLists.txt" => "CMake",
        "Makefile" | "makefile" => "Make",
        "meson.build" => "Meson",
        "Gemfile" | "Rakefile" | "*.gemspec" => "Ruby",
        _ => "Unknown",
    }
}
