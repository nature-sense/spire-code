// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! ProjectQueryActor — high-level semantic project query tools for LLM understanding.
//!
//! This actor provides a set of tools that sit on top of the knowledge graph,
//! giving the LLM rich semantic understanding of the project structure, build
//! systems, languages, dependencies, and architecture.
//!
//! # Tools
//!
//! | Tool | Description |
//! |------|-------------|
//! | `project/getOverview` | High-level project summary |
//! | `project/getFileTree` | Directory/file tree with semantic annotations |
//! | `project/getFileDetails` | Detailed metadata about a specific file |
//! | `project/searchFiles` | Search files by name, language, role, or pattern |
//! | `project/getBuildConfig` | Parsed build configuration |
//! | `project/getDependencies` | Dependency graph (external + internal) |
//! | `project/getEntryPoints` | Main entry points of the project |
//! | `project/getArchitecture` | High-level architectural overview |
//! | `project/getEntities` | Functions, classes, types defined in the project |
//! | `project/getRelationships` | Relationships between project elements |
//! | `project/queryGraph` | Flexible graph query |
//! | `project/getChanges` | Recent file changes since last sync |

use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::Actor;
use spire_core::actors::ToolInfo;
use spire_core::models::memory_graph::{
    AttrNode, RelationshipType, TraversalDirection,
    TraversalOptions,
};

// ============================================================================
// ProjectQueryMessage
// ============================================================================

/// Messages for the ProjectQuery actor.
pub enum ProjectQueryMessage {
    /// Initialize the actor with its dependencies.
    Initialize {
        memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
        project_root: PathBuf,
        reply_to: oneshot::Sender<anyhow::Result<()>>,
    },
    /// Handle a tool call.
    CallTool {
        tool: String,
        args: serde_json::Value,
        reply_to: oneshot::Sender<serde_json::Value>,
    },
    /// List the tools provided by this actor.
    ListTools {
        reply_to: oneshot::Sender<Vec<ToolInfo>>,
    },
}

// ============================================================================
// ProjectQueryActor
// ============================================================================

/// The ProjectQuery actor — semantic project query tools.
pub struct ProjectQueryActor {
    /// Sender to the MemoryGraph actor.
    memory_graph_tx: Option<mpsc::Sender<MemoryGraphMessage>>,
    /// Project root path.
    project_root: Option<PathBuf>,
}

impl Default for ProjectQueryActor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProjectQueryActor {
    pub fn new() -> Self {
        Self {
            memory_graph_tx: None,
            project_root: None,
        }
    }

    /// Return the tool definitions for this actor.
    pub fn tool_definitions() -> Vec<ToolInfo> {
        vec![
            ToolInfo {
                name: "project/getOverview".to_string(),
                description: "Get a high-level overview of the project — languages, build systems, directory structure, entry points, and total size.".to_string(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            ToolInfo {
                name: "project/getFileTree".to_string(),
                description: "Get the directory/file tree with semantic annotations. Optionally filter by role (source, test, config, docs, etc.) or path prefix.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "role": {"type": "string", "description": "Filter by directory or file role (e.g. 'source_code', 'tests', 'documentation', 'config')"},
                        "prefix": {"type": "string", "description": "Only include paths starting with this prefix"},
                        "maxDepth": {"type": "integer", "description": "Maximum directory depth (default: unlimited)"}
                    }
                }),
            },
            ToolInfo {
                name: "project/getFileDetails".to_string(),
                description: "Get detailed metadata about a specific file — language, role, line count, size, and any entities defined in it.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Relative file path within the project"}
                    },
                    "required": ["path"]
                }),
            },
            ToolInfo {
                name: "project/searchFiles".to_string(),
                description: "Search for files by name, language, role, or path pattern.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Search by filename (substring match)"},
                        "language": {"type": "string", "description": "Filter by programming language (e.g. 'Rust', 'TypeScript', 'Python')"},
                        "role": {"type": "string", "description": "Filter by file role (e.g. 'source', 'test', 'entry_point', 'build_config')"},
                        "pattern": {"type": "string", "description": "Glob-style path pattern (e.g. '**/*.rs')"},
                        "limit": {"type": "integer", "description": "Maximum results (default: 50)"}
                    }
                }),
            },
            ToolInfo {
                name: "project/getBuildConfig".to_string(),
                description: "Get parsed build configuration — build systems, dependencies, scripts, version info.".to_string(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            ToolInfo {
                name: "project/getDependencies".to_string(),
                description: "Get the dependency graph — both external dependencies and internal module relationships.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "description": "Filter by dependency type: 'external', 'internal', or 'all' (default: 'all')"}
                    }
                }),
            },
            ToolInfo {
                name: "project/getEntryPoints".to_string(),
                description: "Get the main entry points of the project — main functions, library entry points, CLI entry points.".to_string(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            ToolInfo {
                name: "project/getArchitecture".to_string(),
                description: "Get a high-level architectural overview — key modules, their responsibilities, and how they relate.".to_string(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            ToolInfo {
                name: "project/getEntities".to_string(),
                description: "Get entities (functions, classes, types, modules) defined in the project. Optionally filter by name, type, or file.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Search by entity name (substring match)"},
                        "entityType": {"type": "string", "description": "Filter by entity type (e.g. 'Function', 'Class', 'Interface', 'Module')"},
                        "file": {"type": "string", "description": "Only entities defined in this file path"},
                        "limit": {"type": "integer", "description": "Maximum results (default: 50)"}
                    }
                }),
            },
            ToolInfo {
                name: "project/getRelationships".to_string(),
                description: "Get relationships between project elements — call graphs, import graphs, dependency chains.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "nodeId": {"type": "string", "description": "Start from this node ID"},
                        "relationshipType": {"type": "string", "description": "Filter by relationship type (e.g. 'depends_on', 'called_by', 'belongs_to')"},
                        "direction": {"type": "string", "description": "Direction: 'in', 'out', or 'both' (default: 'out')"},
                        "depth": {"type": "integer", "description": "Max traversal depth (default: 1)"}
                    }
                }),
            },
            ToolInfo {
                name: "project/queryGraph".to_string(),
                description: "Flexible graph query — search nodes by type, subtype, name, or tags. Returns matching nodes with their relationships.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "nodeType": {"type": "string", "description": "Filter by node type (e.g. 'Project', 'Entity', 'File', 'Directory', 'BuildSystem')"},
                        "subtype": {"type": "string", "description": "Filter by subtype (e.g. 'Function', 'Class', 'Module')"},
                        "name": {"type": "string", "description": "Search by name (substring match)"},
                        "tags": {"type": "array", "items": {"type": "string"}, "description": "Filter by tags"},
                        "limit": {"type": "integer", "description": "Maximum results (default: 50)"},
                        "includeRelationships": {"type": "boolean", "description": "Include relationships for each node (default: false)"}
                    }
                }),
            },
            ToolInfo {
                name: "project/getBuildTarget".to_string(),
                description: "Get target-scoped detail: dependencies, platform, and source files for a specific build target.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Build target name (e.g. 'ai-trap-rock3c')"}
                    },
                    "required": ["name"]
                }),
            },
            ToolInfo {
                name: "project/getChanges".to_string(),
                description: "Get recent changes to the project — new, modified, or deleted files since the last sync.".to_string(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            ToolInfo {
                name: "project/diagnostics".to_string(),
                description: "Get structured diagnostics (errors/warnings) from the knowledge graph — normalized JSON of {file, line, column, message, severity, buildType}. Filter by filePath or severity. Prefer this over reading raw build output.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "filePath": {"type": "string", "description": "Only diagnostics for files containing this path"},
                        "severity": {"type": "string", "description": "Filter by severity: 'error' or 'warning'"},
                        "limit": {"type": "integer", "description": "Maximum results (default: 100)"}
                    }
                }),
            },
            ToolInfo {
                name: "symbols/search".to_string(),
                description: "Search AST symbols (functions, classes, variables, imports) by name or kind. Returns stable symbolId values for use with symbols/definition and symbols/references.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "Symbol name (case-insensitive substring match)"},
                        "kind": {"type": "string", "description": "Filter by kind: 'function', 'class', 'variable', 'import'" },
                        "filePath": {"type": "string", "description": "Only symbols defined in this file"},
                        "limit": {"type": "integer", "description": "Maximum results (default: 50)"}
                    }
                }),
            },
            ToolInfo {
                name: "symbols/definition".to_string(),
                description: "Get the definition (rich graph-backed symbol with body) for a symbolId.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "symbolId": {"type": "string", "description": "Stable graph node id of the symbol"},
                        "includeBody": {"type": "boolean", "description": "Include the full function/class body (default: true)"}
                    },
                    "required": ["symbolId"]
                }),
            },
            ToolInfo {
                name: "symbols/references".to_string(),
                description: "Find call/reference edges pointing at a symbolId (incoming relationships of type calls/references/imports).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "symbolId": {"type": "string", "description": "Stable graph node id of the symbol"},
                        "limit": {"type": "integer", "description": "Maximum results (default: 100)"}
                    },
                    "required": ["symbolId"]
                }),
            },
            ToolInfo {
                name: "symbols/rename".to_string(),
                description: "Rename a symbol project-wide (Phase 3). Renames the declaration site and every edge-resolved call site (same-file 'calls'/'references' edges plus cross-file symbol-index matches). Word-boundary replacements are applied ONLY within each resolved site's span — no whole-project string munging. Cross-file candidates that cannot be tied to a call position are REPORTED as possibleReferences (not auto-renamed) with breakingChanges=true, so you can confirm them before applying. Use dryRun=true to preview the exact per-file diffs without touching anything.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "symbolId": {"type": "string", "description": "Stable graph node id of the symbol"},
                        "newName": {"type": "string", "description": "New identifier (must match [A-Za-z_][A-Za-z0-9_]*)"},
                        "dryRun": {"type": "boolean", "description": "Preview diffs without writing (default: true)"}
                    },
                    "required": ["symbolId", "newName"]
                }),
            },
        ]
    }

    /// Phase 3 — `symbols/rename`.
    ///
    /// Deterministic, span-scoped rename:
    ///   1. Declaration site (the symbol node itself).
    ///   2. Call/import sites resolved by graph edges (caller nodes via
    ///      `calls`/`references`/`imports`, or semantic CalledBy).
    ///   3. Cross-file candidates found via the symbol index that are NOT
    ///      edge-resolved — those are reported as `possibleReferences` and
    ///      NOT auto-renamed (we never guess where the usage is).
    /// Word-boundary replacement is applied ONLY inside each resolved site's
    /// source span. `dryRun=true` (default) returns per-file diffs without
    /// touching disk.
    async fn handle_symbols_rename(&self, args: &serde_json::Value) -> serde_json::Value {
        let symbol_id = match args.get("symbolId").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return serde_json::json!({"error": "missing 'symbolId'"}),
        };
        let new_name = match args.get("newName").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return serde_json::json!({"error": "missing 'newName'"}),
        };
        let dry_run = args.get("dryRun").and_then(|v| v.as_bool()).unwrap_or(true);

        if !Self::is_valid_identifier(new_name) {
            return serde_json::json!({
                "error": format!("'{}' is not a valid identifier (must match [A-Za-z_][A-Za-z0-9_]*)", new_name)
            });
        }

        let target_attr = match self.get_attr_node(symbol_id).await {
            Ok(Some(a)) => a,
            Ok(None) => return serde_json::json!({"error": format!("symbol not found: {symbol_id}")}),
            Err(e) => return serde_json::json!({"error": format!("lookup failed: {e}")}),
        };

        // Old identifier + declaration file/span.
        let old_name = target_attr.name().to_string();
        if old_name == new_name {
            return serde_json::json!({"error": "newName is identical to the current name"});
        }
        let decl_span = Self::attr_node_span(&target_attr); // (file, line, col, line, col)
        let Some((decl_file, decl_line, decl_col, decl_end_line, decl_end_col)) = decl_span else {
            return serde_json::json!({"error": "symbol node has no source span"});
        };

        // Resolve deterministic call sites from graph edges.
        let mut call_sites: Vec<(String, u32, u32, u32, u32)> = Vec::new(); // (file, sl, sc, el, ec)
        let edges = self.get_relationships(symbol_id).await.unwrap_or_default();
        let interesting = ["calls", "references", "imports", "calledby", "semanticallyrelated"];
        for edge in edges {
            let rel = match &edge.edge_type {
                spire_core::models::memory_graph::RelationshipType::Custom(name) => name.to_lowercase(),
                other => format!("{:?}", other).to_lowercase(),
            };
            if interesting.iter().any(|i| rel.contains(i)) {
                if let Ok(Some(from)) = self.get_attr_node(&edge.from_id).await {
                    if let Some(span) = Self::attr_node_span(&from) {
                        call_sites.push(span);
                    }
                }
            }
        }

        // Cross-file candidates via the symbol index (for the possible-reports).
        // Open-envelope read path: query by the string discriminators and
        // project with the envelope accessor view.
        let mut symbols: Vec<spire_core::models::analysis::GraphSymbol> = Vec::new();
        for nt in ["astFunction", "astClass", "astVariable", "astImport"] {
            if let Ok(nodes) = self.query_attr_nodes(Some(nt.to_string()), None, None, None).await {
                symbols.extend(
                    nodes
                        .iter()
                        .filter_map(spire_core::models::analysis::GraphSymbol::from_attr_node),
                );
            }
        }
        let index = self.build_symbol_index(&symbols);
        let target_mod = Self::module_of(&decl_file);
        let cross_candidates: Vec<String> = index
            .iter()
            .filter(|((m, n), _)| *n == old_name && *m != target_mod)
            .flat_map(|(_, ids)| ids.clone())
            .filter(|id| id != symbol_id)
            .collect();
        // Edge-resolved ids are "handled"; the rest are possible.
        let handled: std::collections::HashSet<String> = call_sites.iter().map(|s| s.0.clone()).collect();
        let possible: Vec<String> = cross_candidates
            .into_iter()
            .filter(|id| !handled.contains(id))
            .collect();

        // Per-file diff computation. Only files with at least one resolved
        // replacement are touched.
        let mut file_diffs: Vec<serde_json::Value> = Vec::new();
        let mut applied_any = false;

        // Declaration first — read the file, replace within the decl span.
        if let Some(decl_text) = Self::read_project_file(&decl_file, self.project_root.as_ref()) {
            if let Some((new_text, replaced)) = Self::rename_span_in_file(
                &decl_file,
                &decl_text,
                &old_name,
                &new_name,
                decl_line,
                decl_col,
                decl_end_line,
                decl_end_col,
            ) {
                if replaced > 0 {
                    file_diffs.push(serde_json::json!({
                        "file": decl_file,
                        "site": "declaration",
                        "replacements": replaced,
                        "diff": new_text,
                    }));
                    applied_any = true;
                }
            }
        }

        // Call sites (grouped by file to avoid duplicate reads).
        let mut by_file: std::collections::HashMap<String, Vec<(u32, u32, u32, u32)>> =
            std::collections::HashMap::new();
        for (f, sl, sc, el, ec) in &call_sites {
            by_file.entry(f.clone()).or_default().push((*sl, *sc, *el, *ec));
        }
        for (file, spans) in by_file {
            let mut text = match Self::read_project_file(&file, self.project_root.as_ref()) {
                Some(t) => t,
                None => continue,
            };
            let mut total = 0usize;
            for (sl, sc, el, ec) in &spans {
                if let Some((t, n)) =
                    Self::replace_in_span(&text, &old_name, &new_name, *sl, *sc, *el, *ec)
                {
                    if n > 0 {
                        text = t;
                        total += n;
                    }
                }
            }
            if total > 0 {
                file_diffs.push(serde_json::json!({
                    "file": file,
                    "site": "call",
                    "replacements": total,
                    "diff": text,
                }));
                applied_any = true;
            }
        }

        // Apply writes (unless dry run).
        let mut applied_files: Vec<String> = Vec::new();
        if !dry_run && applied_any {
            for fd in &file_diffs {
                let file = fd["file"].as_str().unwrap_or("");
                let new_text = fd["diff"].as_str().unwrap_or("");
                if let Some(abs) = Self::project_path(file, self.project_root.as_ref()) {
                    if std::fs::write(&abs, new_text).is_ok() {
                        applied_files.push(file.to_string());
                    }
                }
            }
        }

        serde_json::json!({
            "symbolId": symbol_id,
            "oldName": old_name,
            "newName": new_name,
            "dryRun": dry_run,
            "breakingChanges": !possible.is_empty(),
            "filesChanged": if dry_run { file_diffs.len() } else { applied_files.len() },
            "diffs": file_diffs,
            "possibleReferences": possible,
            "note": if possible.is_empty() {
                "All references were resolved and renamed.".to_string()
            } else {
                "Cross-file candidates with no resolvable call site were REPORTED, not renamed. Review 'possibleReferences' and rename them explicitly if they are genuine usages.".to_string()
            },
            "graphRefreshNote": if dry_run { "" } else { "Source files were renamed; re-run the project analyzer / parse to refresh the graph's AST nodes (the graph currently still holds the OLD name for this node)." }.to_string(),
        })
    }

    /// Identifier validity for cross-language renames.
    fn is_valid_identifier(s: &str) -> bool {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if c.is_alphabetic() || c == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_alphanumeric() || c == '_')
    }

    /// Best-effort (line, col) span of an AST node's first line — the actual
    /// name token is on `start_line`; we use start/end to bound the search.
    /// Envelope view: reads the flattened AST fields via typed accessors.
    fn attr_node_span(attr: &spire_core::models::memory_graph::AttrNode) -> Option<(String, u32, u32, u32, u32)> {
        if !(attr.is("astFunction") || attr.is("astClass") || attr.is("astVariable")) {
            return None;
        }
        Some((
            attr.str_prop("file_path")?,
            attr.u32_prop("start_line")?,
            attr.u32_prop("start_col")?,
            attr.u32_prop("end_line")?,
            attr.u32_prop("end_col")?,
        ))
    }

    /// Resolve a project file path (accepts absolute or project-relative).
    fn project_path(
        file: &str,
        root: Option<&PathBuf>,
    ) -> Option<PathBuf> {
        let p = PathBuf::from(file);
        if p.is_absolute() {
            Some(p)
        } else {
            root.map(|r| r.join(&p))
        }
    }

    /// Read a project file (absolute, or relative to `root`).
    fn read_project_file(file: &str, root: Option<&PathBuf>) -> Option<String> {
        let abs = Self::project_path(file, root)?;
        std::fs::read_to_string(&abs).ok()
    }

    /// Word-boundary replace of `old`→`new` inside the span of one node's first
    /// line (start_line .. end_line). Returns the new text + replacement count.
    /// The span is 1-based lines/cols per AST storage; both 0-based and 1-based
    /// are probed defensively so an off-by-one storage convention never causes
    /// a missed or misplaced replacement.
    fn replace_in_span(
        text: &str,
        old: &str,
        new: &str,
        sl: u32,
        sc: u32,
        _el: u32,
        _ec: u32,
    ) -> Option<(String, usize)> {
        // Owned lines — avoids borrowing locals when we splice in `new`.
        let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
        // Probe both 1-based and 0-based line interpretations of the start
        // line so an off-by-one storage convention never misses the name
        // token (it always sits on the declaration/caller's start line).
        let mut candidates: Vec<usize> = Vec::new();
        if sl >= 1 && (sl - 1) < lines.len() as u32 {
            candidates.push((sl - 1) as usize);
        }
        if sl < lines.len() as u32 {
            candidates.push(sl as usize);
        }
        candidates.dedup();

        for li in candidates {
            let line = &lines[li];
            let start_off = (sc.saturating_sub(1) as usize).min(line.len());
            let search = &line[start_off..];
            let mut pos = 0usize;
            while let Some(rel) = search[pos..].find(old) {
                let abs_pos = start_off + pos + rel;
                let before_ok = abs_pos == 0
                    || !line[..abs_pos]
                        .chars()
                        .last()
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false);
                let after_end = abs_pos + old.len();
                let after_ok = after_end >= line.len()
                    || !line[after_end..]
                        .chars()
                        .next()
                        .map(|c| c.is_alphanumeric() || c == '_')
                        .unwrap_or(false);
                if before_ok && after_ok {
                    let mut replaced = lines[li].clone();
                    replaced.replace_range(abs_pos..after_end, new);
                    lines[li] = replaced;
                    return Some((lines.join("\n"), 1));
                }
                pos += rel + old.len();
            }
        }
        None
    }

    /// Rename on the declaration site file; returns (new_text, replaced).
    fn rename_span_in_file(
        _file: &str,
        text: &str,
        old: &str,
        new: &str,
        sl: u32,
        sc: u32,
        el: u32,
        ec: u32,
    ) -> Option<(String, usize)> {
        Self::replace_in_span(text, old, new, sl, sc, el, ec)
    }

    // ── Graph Helpers ──────────────────────────────────────────────────────

    /// Send a message to the MemoryGraph actor and await the response.
    async fn send_to_graph<T, F>(&self, make_msg: F) -> anyhow::Result<T>
    where
        F: FnOnce(oneshot::Sender<anyhow::Result<T>>) -> MemoryGraphMessage,
    {
        let tx_ref = self
            .memory_graph_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MemoryGraph sender not initialized"))?;
        let (tx, rx) = oneshot::channel();
        tx_ref
            .send(make_msg(tx))
            .await
            .map_err(|e| anyhow::anyhow!("MemoryGraph channel closed: {}", e))?;
        rx.await
            .map_err(|e| anyhow::anyhow!("MemoryGraph response error: {}", e))?
    }

    /// Query nodes as the open `AttrNode` envelope (string discriminator).
    async fn query_attr_nodes(
        &self,
        node_type: Option<String>,
        subtype: Option<String>,
        name: Option<String>,
        limit: Option<u32>,
    ) -> anyhow::Result<Vec<AttrNode>> {
        self.send_to_graph(|tx| MemoryGraphMessage::QueryAttrNodes {
            node_type,
            subtype,
            name,
            limit,
            reply_to: tx,
        })
        .await
    }

    /// Get a single node by ID as the open `AttrNode` envelope.
    async fn get_attr_node(&self, id: &str) -> anyhow::Result<Option<AttrNode>> {
        self.send_to_graph(|tx| MemoryGraphMessage::GetAttrNode {
            id: id.to_string(),
            reply_to: tx,
        })
        .await
    }

    /// Get relationships for a node.
    async fn get_relationships(
        &self,
        node_id: &str,
    ) -> anyhow::Result<Vec<spire_core::models::memory_graph::GraphEdge>> {
        self.send_to_graph(|tx| MemoryGraphMessage::GetRelationships {
            node_id: node_id.to_string(),
            reply_to: tx,
        })
        .await
    }

    /// Traverse the graph from a start node.
    async fn traverse(
        &self,
        start_node_id: &str,
        options: TraversalOptions,
    ) -> anyhow::Result<spire_core::models::memory_graph::TraversalResult> {
        self.send_to_graph(|tx| MemoryGraphMessage::Traverse {
            start_node_id: start_node_id.to_string(),
            options,
            reply_to: tx,
        })
        .await
    }

    // ── Tool Implementations ──────────────────────────────────────────────

    /// `project/getOverview` — high-level project summary.
    async fn handle_get_overview(&self) -> serde_json::Value {
        // Find the Project node
        let project_nodes = match self
            .query_attr_nodes(Some("Project".to_string()), None, None, Some(1))
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query project: {}", e)})
            }
        };

        let project = match project_nodes.first() {
            Some(p) => p,
            None => {
                return serde_json::json!(
                    {"error": "No project node found. Has the project been synced?"}
                )
            }
        };

        // Count files and directories
        let file_nodes = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("File".to_string()), None, None)
            .await
            .unwrap_or_default();

        let dir_nodes = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("Directory".to_string()), None, None)
            .await
            .unwrap_or_default();

        // Collect languages from file nodes
        let mut languages: HashMap<String, usize> = HashMap::new();
        for file in &file_nodes {
            if let Some(lang) = file.get("language").and_then(|v| v.as_str()) {
                *languages.entry(lang.to_string()).or_insert(0) += 1;
            }
        }

        // Collect directory roles
        let mut dir_roles: HashMap<String, usize> = HashMap::new();
        for dir in &dir_nodes {
            if let Some(role) = dir.get("role").and_then(|v| v.as_str()) {
                *dir_roles.entry(role.to_string()).or_insert(0) += 1;
            }
        }

        // Find build system nodes
        let build_systems = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("BuildSystem".to_string()), None, None)
            .await
            .unwrap_or_default();

        // Find entry points
        let entry_points: Vec<String> = file_nodes
            .iter()
            .filter(|f| {
                f.get("role")
                    .and_then(|v| v.as_str())
                    .map(|r| r == "entry_point")
                    .unwrap_or(false)
            })
            .map(|f| {
                f.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(f.name())
                    .to_string()
            })
            .collect();

        // Compute totals
        let total_lines: usize = file_nodes
            .iter()
            .filter_map(|f| f.get("lines").and_then(|v| v.as_u64()))
            .sum::<u64>() as usize;

        let mut lang_list: Vec<serde_json::Value> = languages
            .into_iter()
            .map(|(lang, count)| {
                serde_json::json!({
                    "language": lang,
                    "fileCount": count
                })
            })
            .collect();
        lang_list.sort_by(|a, b| b["fileCount"].as_u64().cmp(&a["fileCount"].as_u64()));

        let mut dir_role_list: Vec<serde_json::Value> = dir_roles
            .into_iter()
            .map(|(role, count)| serde_json::json!({"role": role, "count": count}))
            .collect();
        dir_role_list.sort_by(|a, b| b["count"].as_u64().cmp(&a["count"].as_u64()));

        let build_system_list: Vec<serde_json::Value> = build_systems
            .iter()
            .map(|bs| {
                serde_json::json!({
                    "name": bs.name(),
                    "type": bs.get("build_type").and_then(|v| v.as_str()),
                    "projectName": bs.get("project_name").and_then(|v| v.as_str()),
                    "version": bs.get("version").and_then(|v| v.as_str()),
                })
            })
            .collect();

        serde_json::json!({
            "projectName": project.name(),
            "projectRoot": project.get("path"),
            "totalFiles": file_nodes.len(),
            "totalDirs": dir_nodes.len(),
            "totalLines": total_lines,
            "languages": lang_list,
            "directoryRoles": dir_role_list,
            "buildSystems": build_system_list,
            "entryPoints": entry_points,
        })
    }

    /// `project/getFileTree` — directory/file tree with semantic annotations.
    async fn handle_get_file_tree(&self, args: &serde_json::Value) -> serde_json::Value {
        let role_filter = args.get("role").and_then(|v| v.as_str());
        let prefix_filter = args.get("prefix").and_then(|v| v.as_str());
        let max_depth = args.get("maxDepth").and_then(|v| v.as_u64());

        // Get all directory nodes
        let dir_nodes = match self
            .query_attr_nodes(Some("Unknown".to_string()), Some("Directory".to_string()), None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query directories: {}", e)})
            }
        };

        // Get all file nodes
        let file_nodes = match self
            .query_attr_nodes(Some("Unknown".to_string()), Some("File".to_string()), None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => return serde_json::json!({"error": format!("Failed to query files: {}", e)}),
        };

        // Build a tree structure
        let mut tree: serde_json::Value = serde_json::json!({
            "name": "",
            "path": "",
            "type": "root",
            "children": []
        });

        // Index directories by path for role lookup
        let dir_roles: HashMap<&str, &str> = dir_nodes
            .iter()
            .filter_map(|d| {
                let path = d.get("path").and_then(|v| v.as_str())?;
                let role = d.get("role").and_then(|v| v.as_str())?;
                Some((path, role))
            })
            .collect();

        // Add files to tree
        for file in &file_nodes {
            let path = match file.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => continue,
            };

            // Apply filters
            if let Some(role) = role_filter {
                let file_role = file
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if file_role != role {
                    continue;
                }
            }
            if let Some(prefix) = prefix_filter {
                if !path.starts_with(prefix) {
                    continue;
                }
            }

            let parts: Vec<&str> = path.split('/').collect();
            let filename = parts.last().unwrap_or(&path);
            let dir_parts = &parts[..parts.len() - 1];

            let file_info = serde_json::json!({
                "name": filename,
                "path": path,
                "type": "file",
                "language": file.get("language"),
                "role": file.get("role"),
                "lines": file.get("lines"),
            });

            add_to_tree(&mut tree, dir_parts, &file_info, 0, max_depth);
        }

        // Annotate directories with roles
        annotate_tree_roles(&mut tree, &dir_roles);

        tree
    }

    /// `project/getFileDetails` — detailed metadata about a specific file.
    async fn handle_get_file_details(&self, args: &serde_json::Value) -> serde_json::Value {
        let file_path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return serde_json::json!({"error": "Missing required parameter: 'path'"}),
        };

        // Find the file node by path
        let file_nodes = match self
            .query_attr_nodes(Some("Unknown".to_string()), Some("File".to_string()), None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => return serde_json::json!({"error": format!("Failed to query files: {}", e)}),
        };

        let file_node = match file_nodes.into_iter().find(|f| {
            f.get("path")
                .and_then(|v| v.as_str())
                .map(|p| p == file_path)
                .unwrap_or(false)
        }) {
            Some(f) => f,
            None => return serde_json::json!({"error": format!("File not found: {}", file_path)}),
        };

        // Get relationships for this file
        let relationships = self
            .get_relationships(file_node.id())
            .await
            .unwrap_or_default();

        // Get entities defined in this file
        let entities = self
            .query_attr_nodes(Some("Unknown".to_string()), None, None, None)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|e| {
                e.get("file")
                    .and_then(|v| v.as_str())
                    .map(|f| f == file_path)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();

        serde_json::json!({
            "id": file_node.id(),
            "name": file_node.name(),
            "path": file_node.get("path"),
            "language": file_node.get("language"),
            "role": file_node.get("role"),
            "lines": file_node.get("lines"),
            "size": file_node.get("size"),
            "description": file_node.description(),
            "relationships": relationships.iter().map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "type": format!("{:?}", r.edge_type),
                    "fromId": r.from_id,
                    "toId": r.to_id,
                })
            }).collect::<Vec<_>>(),
            "entities": entities.iter().map(|e| {
                serde_json::json!({
                    "id": e.id(),
                    "name": e.name(),
                    "subtype": e.subtype(),
                    "description": e.description(),
                })
            }).collect::<Vec<_>>(),
        })
    }

    /// `project/searchFiles` — search files by name, language, role, or pattern.
    async fn handle_search_files(&self, args: &serde_json::Value) -> serde_json::Value {
        let name_filter = args.get("name").and_then(|v| v.as_str());
        let language_filter = args.get("language").and_then(|v| v.as_str());
        let role_filter = args.get("role").and_then(|v| v.as_str());
        let pattern_filter = args.get("pattern").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

        let file_nodes = match self
            .query_attr_nodes(Some("Unknown".to_string()), Some("File".to_string()), None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => return serde_json::json!({"error": format!("Failed to query files: {}", e)}),
        };

        let results: Vec<serde_json::Value> = file_nodes
            .into_iter()
            .filter(|f| {
                let path = f
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let language = f
                    .get("language")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let role = f
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Name filter (substring match on filename)
                if let Some(name) = name_filter {
                    let filename = std::path::Path::new(path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if !filename.to_lowercase().contains(&name.to_lowercase()) {
                        return false;
                    }
                }

                // Language filter
                if let Some(lang) = language_filter {
                    if !language.eq_ignore_ascii_case(lang) {
                        return false;
                    }
                }

                // Role filter
                if let Some(role_f) = role_filter {
                    if role != role_f {
                        return false;
                    }
                }

                // Pattern filter (simple glob-like match)
                if let Some(pattern) = pattern_filter {
                    if !glob_match::glob_match(pattern, path) {
                        return false;
                    }
                }

                true
            })
            .take(limit)
            .map(|f| {
                serde_json::json!({
                    "id": f.id(),
                    "name": f.name(),
                    "path": f.get("path"),
                    "language": f.get("language"),
                    "role": f.get("role"),
                    "lines": f.get("lines"),
                })
            })
            .collect();

        serde_json::json!({
            "total": results.len(),
            "results": results,
        })
    }

    /// `project/getBuildConfig` — parsed build configuration.
    async fn handle_get_build_config(&self) -> serde_json::Value {
        let build_systems = match self
            .query_attr_nodes(Some("Unknown".to_string()), Some("BuildSystem".to_string()), None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query build systems: {}", e)})
            }
        };

        let systems: Vec<serde_json::Value> = build_systems
            .iter()
            .map(|bs| {
                // Derive the build directory from config_file (e.g. "rust/Cargo.toml" → "rust")
                let config_file = bs.get("config_file").and_then(|v| v.as_str());
                let build_dir = config_file.and_then(|f| {
                    let p = std::path::Path::new(f);
                    p.parent()
                        .map(|parent| parent.to_string_lossy().to_string())
                });

                serde_json::json!({
                    "id": bs.id(),
                    "name": bs.name(),
                    "buildType": bs.get("build_type"),
                    "projectName": bs.get("project_name"),
                    "version": bs.get("version"),
                    "configFile": config_file,
                    "path": build_dir,
                    "scripts": bs.get("scripts"),
                    "dependencies": bs.get("dependencies"),
                })
            })
            .collect();

        serde_json::json!({
            "total": systems.len(),
            "buildSystems": systems,
        })
    }

    /// `project/getDependencies` — dependency graph.
    async fn handle_get_dependencies(&self, args: &serde_json::Value) -> serde_json::Value {
        let dep_type = args.get("type").and_then(|v| v.as_str()).unwrap_or("all");

        // Find the project node
        let project_nodes = match self
            .query_attr_nodes(Some("Project".to_string()), None, None, Some(1))
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query project: {}", e)})
            }
        };

        let project = match project_nodes.first() {
            Some(p) => p,
            None => return serde_json::json!({"error": "No project node found"}),
        };

        // Traverse depends_on relationships from the project
        let traversal = self
            .traverse(
                project.id(),
                TraversalOptions {
                    max_depth: 3,
                    relationship_types: Some(vec![RelationshipType::DependsOn]),
                    max_nodes: Some(200),
                    direction: Some(TraversalDirection::Out),
                },
            )
            .await
            .unwrap_or(spire_core::models::memory_graph::TraversalResult {
                nodes: vec![],
                edges: vec![],
                paths: vec![],
            });

        let mut external_deps: Vec<serde_json::Value> = Vec::new();
        let mut internal_deps: Vec<serde_json::Value> = Vec::new();

        for edge in &traversal.edges {
            let from_node = traversal.nodes.iter().find(|n| n.id() == edge.from_id);
            let to_node = traversal.nodes.iter().find(|n| n.id() == edge.to_id);

            let dep = serde_json::json!({
                "from": from_node.map(|n| n.name()).unwrap_or("unknown"),
                "fromId": edge.from_id,
                "to": to_node.map(|n| n.name()).unwrap_or("unknown"),
                "toId": edge.to_id,
                "type": format!("{:?}", edge.edge_type),
                "weight": edge.weight,
            });

            // Simple heuristic: if the target node has a version property, it's external
            let is_external = to_node
                .and_then(|n| n.get("version"))
                .is_some();

            match dep_type {
                "external" => {
                    if is_external {
                        external_deps.push(dep);
                    }
                }
                "internal" => {
                    if !is_external {
                        internal_deps.push(dep);
                    }
                }
                _ => {
                    if is_external {
                        external_deps.push(dep.clone());
                    } else {
                        internal_deps.push(dep);
                    }
                }
            }
        }

        serde_json::json!({
            "external": external_deps,
            "internal": internal_deps,
            "totalExternal": external_deps.len(),
            "totalInternal": internal_deps.len(),
        })
    }

    /// `project/getEntryPoints` — main entry points.
    async fn handle_get_entry_points(&self) -> serde_json::Value {
        let file_nodes = match self
            .query_attr_nodes(Some("Unknown".to_string()), Some("File".to_string()), None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => return serde_json::json!({"error": format!("Failed to query files: {}", e)}),
        };

        let entry_points: Vec<serde_json::Value> = file_nodes
            .iter()
            .filter(|f| {
                f.get("role")
                    .and_then(|v| v.as_str())
                    .map(|r| r == "entry_point")
                    .unwrap_or(false)
            })
            .map(|f| {
                serde_json::json!({
                    "id": f.id(),
                    "name": f.name(),
                    "path": f.get("path"),
                    "language": f.get("language"),
                    "lines": f.get("lines"),
                })
            })
            .collect();

        serde_json::json!({
            "total": entry_points.len(),
            "entryPoints": entry_points,
        })
    }

    /// `project/getArchitecture` — high-level architectural overview.
    async fn handle_get_architecture(&self) -> serde_json::Value {
        // Get the project node
        let project_nodes = match self
            .query_attr_nodes(Some("Project".to_string()), None, None, Some(1))
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query project: {}", e)})
            }
        };

        let project = match project_nodes.first() {
            Some(p) => p,
            None => return serde_json::json!({"error": "No project node found"}),
        };

        // Get directory structure with roles
        let dir_nodes = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("Directory".to_string()), None, None)
            .await
            .unwrap_or_default();

        // Group directories by role
        let mut dirs_by_role: HashMap<String, Vec<String>> = HashMap::new();
        for dir in &dir_nodes {
            let role = dir
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("directory")
                .to_string();
            let path = dir
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            dirs_by_role.entry(role).or_default().push(path);
        }

        // Get build systems
        let build_systems = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("BuildSystem".to_string()), None, None)
            .await
            .unwrap_or_default();

        // Get entry points
        let file_nodes = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("File".to_string()), None, None)
            .await
            .unwrap_or_default();

        let entry_points: Vec<&str> = file_nodes
            .iter()
            .filter(|f| {
                f.get("role")
                    .and_then(|v| v.as_str())
                    .map(|r| r == "entry_point")
                    .unwrap_or(false)
            })
            .filter_map(|f| f.get("path").and_then(|v| v.as_str()))
            .collect();

        // Build the architecture summary
        let mut modules: Vec<serde_json::Value> = Vec::new();
        for (role, paths) in &dirs_by_role {
            let description = match role.as_str() {
                "source_code" => "Main source code directory",
                "tests" => "Test files and test infrastructure",
                "documentation" => "Project documentation",
                "build_scripts" => "Build scripts and tooling",
                "config" => "Configuration files",
                "examples" => "Example code and usage samples",
                "benchmarks" => "Performance benchmarks",
                "resources" => "Static resources and assets",
                "deployment" => "Deployment and CI/CD configuration",
                "extensions" => "Plugin and extension code",
                "database" => "Database migrations and schema",
                "localization" => "Internationalization and locale files",
                "build_output" => "Build output and compiled artifacts",
                "dependencies" => "Third-party dependencies",
                _ => "General directory",
            };

            modules.push(serde_json::json!({
                "role": role,
                "description": description,
                "directories": paths,
                "count": paths.len(),
            }));
        }

        serde_json::json!({
            "projectName": project.name(),
            "projectRoot": project.get("path"),
            "modules": modules,
            "buildSystems": build_systems.iter().map(|bs| {
                serde_json::json!({
                    "name": bs.name(),
                    "type": bs.get("build_type"),
                    "projectName": bs.get("project_name"),
                })
            }).collect::<Vec<_>>(),
            "entryPoints": entry_points,
        })
    }

    /// `project/getEntities` — entities defined in the project.
    async fn handle_get_entities(&self, args: &serde_json::Value) -> serde_json::Value {
        let name_filter = args.get("name").and_then(|v| v.as_str());
        let entity_type_filter = args.get("entityType").and_then(|v| v.as_str());
        let file_filter = args.get("file").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

        // Query all entity nodes (Unknown type with various subtypes)
        let all_entities = match self
            .query_attr_nodes(Some("Unknown".to_string()), None, None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query entities: {}", e)})
            }
        };

        // Filter to only entity-like nodes (those with a subtype like Function, Class, etc.)
        let entity_subtypes = [
            "Function",
            "Class",
            "Interface",
            "Type",
            "Enum",
            "Struct",
            "Trait",
            "Module",
            "Method",
            "Variable",
            "Constant",
            "Macro",
        ];

        let results: Vec<serde_json::Value> = all_entities
            .into_iter()
            .filter(|e| {
                let subtype_str = e.subtype().unwrap_or("");
                let is_entity = entity_subtypes.contains(&subtype_str);

                if !is_entity {
                    return false;
                }

                // Name filter
                if let Some(name) = name_filter {
                    if !e.name().to_lowercase().contains(&name.to_lowercase()) {
                        return false;
                    }
                }

                // Entity type filter
                if let Some(et) = entity_type_filter {
                    if !subtype_str.eq_ignore_ascii_case(et) {
                        return false;
                    }
                }

                // File filter
                if let Some(file) = file_filter {
                    let entity_file = e
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if entity_file != file {
                        return false;
                    }
                }

                true
            })
            .take(limit)
            .map(|e| {
                serde_json::json!({
                    "id": e.id(),
                    "name": e.name(),
                    "type": e.subtype(),
                    "file": e.get("file"),
                    "line": e.get("line"),
                    "description": e.description(),
                    "visibility": e.get("visibility"),
                })
            })
            .collect();

        serde_json::json!({
            "total": results.len(),
            "entities": results,
        })
    }

    /// `project/getRelationships` — relationships between project elements.
    async fn handle_get_relationships(&self, args: &serde_json::Value) -> serde_json::Value {
        let node_id = args.get("nodeId").and_then(|v| v.as_str());
        let rel_type_filter = args.get("relationshipType").and_then(|v| v.as_str());
        let direction = args
            .get("direction")
            .and_then(|v| v.as_str())
            .unwrap_or("out");
        let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as u8;

        // If no node ID specified, use the project node
        let start_id = if let Some(id) = node_id {
            id.to_string()
        } else {
            let project_nodes = match self
                .query_attr_nodes(Some("Project".to_string()), None, None, Some(1))
                .await
            {
                Ok(nodes) => nodes,
                Err(e) => {
                    return serde_json::json!({"error": format!("Failed to query project: {}", e)})
                }
            };

            match project_nodes.first() {
                Some(p) => p.id().to_string(),
                None => return serde_json::json!({"error": "No project node found"}),
            }
        };

        // Parse relationship type filter
        let rel_types = rel_type_filter.map(|rt| {
            vec![match rt.to_lowercase().as_str() {
                "depends_on" => RelationshipType::DependsOn,
                "called_by" => RelationshipType::CalledBy,
                "belongs_to" => RelationshipType::BelongsTo,
                "imports" => RelationshipType::Unknown,
                _ => RelationshipType::Unknown,
            }]
        });

        // Parse direction
        let dir = match direction {
            "in" => Some(TraversalDirection::In),
            "both" => Some(TraversalDirection::Both),
            _ => Some(TraversalDirection::Out),
        };

        let traversal = match self
            .traverse(
                &start_id,
                TraversalOptions {
                    max_depth: depth,
                    relationship_types: rel_types,
                    max_nodes: Some(100),
                    direction: dir,
                },
            )
            .await
        {
            Ok(t) => t,
            Err(e) => return serde_json::json!({"error": format!("Traversal failed: {}", e)}),
        };

        serde_json::json!({
            "startNodeId": start_id,
            "nodes": traversal.nodes.iter().map(|n| {
                serde_json::json!({
                    "id": n.id(),
                    "name": n.name(),
                    "type": n.node_type_str().to_string(),
                    "subtype": n.subtype(),
                })
            }).collect::<Vec<_>>(),
            "relationships": traversal.edges.iter().map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "type": format!("{:?}", e.edge_type),
                    "fromId": e.from_id,
                    "toId": e.to_id,
                    "weight": e.weight,
                })
            }).collect::<Vec<_>>(),
            "totalNodes": traversal.nodes.len(),
            "totalRelationships": traversal.edges.len(),
        })
    }

    /// `project/queryGraph` — flexible graph query.
    async fn handle_query_graph(&self, args: &serde_json::Value) -> serde_json::Value {
        let node_type_str = args.get("nodeType").and_then(|v| v.as_str());
        let subtype_filter = args.get("subtype").and_then(|v| v.as_str());
        let name_filter = args.get("name").and_then(|v| v.as_str());
        let _tags_filter = args.get("tags").and_then(|v| v.as_array());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let include_rels = args
            .get("includeRelationships")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Parse node type
        let node_type = node_type_str.and_then(|nt| match nt.to_lowercase().as_str() {
            "project" => Some("Project".to_string()),
            "entity" => Some("Entity".to_string()),
            "decision" => Some("Decision".to_string()),
            "activecontext" | "active_context" => Some("ActiveContext".to_string()),
            "blocker" => Some("Blocker".to_string()),
            "milestone" => Some("Milestone".to_string()),
            "standard" => Some("Standard".to_string()),
            "conversation" => Some("Conversation".to_string()),
            "session" => Some("Session".to_string()),
            "mcpserver" | "mcp_server" => Some("Unknown".to_string()),
            _ => None,
        });

        let nodes = match self
            .query_attr_nodes(
                node_type,
                subtype_filter.map(|s| s.to_string()),
                name_filter.map(|n| n.to_string()),
                Some(limit as u32),
            )
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => return serde_json::json!({"error": format!("Query failed: {}", e)}),
        };

        // Optionally fetch relationships for each node
        let mut result_nodes: Vec<serde_json::Value> = Vec::new();
        for node in &nodes {
            let mut node_json = serde_json::json!({
                "id": node.id(),
                "name": node.name(),
                "type": node.node_type_str().to_string(),
                "subtype": node.subtype(),
                "description": node.description(),
                "properties": node.properties.clone(),
            });

            if include_rels {
                let rels = self.get_relationships(node.id()).await.unwrap_or_default();
                node_json["relationships"] = serde_json::json!(rels
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.id,
                            "type": format!("{:?}", r.edge_type),
                            "fromId": r.from_id,
                            "toId": r.to_id,
                        })
                    })
                    .collect::<Vec<_>>());
            }

            result_nodes.push(node_json);
        }

        serde_json::json!({
            "total": result_nodes.len(),
            "nodes": result_nodes,
        })
    }

    /// `project/getBuildTarget` — target-scoped detail fetched from the graph.
    ///
    /// Queries the BuildTarget node by name and returns its kind, the owning
    /// platform(s), its dependencies (traverse DEPENDS_ON), and the source
    /// files in its scope directory.
    async fn handle_get_build_target(&self, args: &serde_json::Value) -> serde_json::Value {
        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return serde_json::json!({"error": "Missing required parameter: 'name'"}),
        };

        // Find the BuildTarget node by name.
        let targets = match self
            .query_attr_nodes(Some("Unknown".to_string()), Some("BuildTarget".to_string()), None, None)
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query targets: {}", e)})
            }
        };

        let target = match targets
            .into_iter()
            .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(&name))
        {
            Some(t) => t,
            None => {
                return serde_json::json!({"error": format!("Build target not found: {}", name)})
            }
        };

        let kind = target
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let config_file = target
            .get("config_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Derive the scope directory from the target name (ai-trap-rock3c → rock3c),
        // falling back to the config file's parent dir.
        let scope_dir = name
            .strip_prefix("ai-trap-")
            .map(|s| s.to_string())
            .or_else(|| {
                std::path::Path::new(&config_file)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .unwrap_or_default();

        // Traverse DEPENDS_ON from the target → dependency nodes. We store deps
        // with a custom "DEPENDS_ON" edge (RelationshipType::Custom), so both
        // the semantic DependsOn and the custom edge must be traversed.
        let traversal = self
            .traverse(
                target.id(),
                TraversalOptions {
                    max_depth: 1,
                    relationship_types: Some(vec![
                        RelationshipType::DependsOn,
                        RelationshipType::Custom("DEPENDS_ON".to_string()),
                    ]),
                    max_nodes: Some(200),
                    direction: Some(TraversalDirection::Out),
                },
            )
            .await
            .unwrap_or(spire_core::models::memory_graph::TraversalResult {
                nodes: vec![],
                edges: vec![],
                paths: vec![],
            });

        let dependencies: Vec<serde_json::Value> = traversal
            .nodes
            .iter()
            .filter(|n| n.id() != target.id())
            .map(|n| {
                serde_json::json!({
                    "name": n.name(),
                    "version": n.get("version"),
                })
            })
            .collect();

        // Traverse BelongsTo from the target → platform nodes.
        let platforms = self
            .traverse(
                target.id(),
                TraversalOptions {
                    max_depth: 2,
                    relationship_types: Some(vec![RelationshipType::BelongsTo]),
                    max_nodes: Some(50),
                    direction: Some(TraversalDirection::Out),
                },
            )
            .await
            .unwrap_or(spire_core::models::memory_graph::TraversalResult {
                nodes: vec![],
                edges: vec![],
                paths: vec![],
            });
        let platform: Vec<String> = platforms
            .nodes
            .iter()
            .filter(|n| n.subtype().map(|s| s == "Platform").unwrap_or(false))
            .map(|n| n.name().to_string())
            .collect();

        // Collect files under the scope directory from the graph. File nodes
        // are stored as NodeType::SourceFile (subtype None), so the filter must
        // match that — not the legacy "File" subtype.
        let files = match self
            .query_attr_nodes(Some("SourceFile".to_string()), None, None, None)
            .await
        {
            Ok(nodes) => {
                let prefix = if scope_dir.is_empty() {
                    String::new()
                } else {
                    format!("{}/", scope_dir)
                };
                nodes
                    .into_iter()
                    .filter(|f| {
                        let path = f
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        scope_dir.is_empty() || path == scope_dir || path.starts_with(&prefix)
                    })
                    .map(|f| {
                        serde_json::json!({
                            "path": f.get("path"),
                            "language": f.get("language"),
                            "role": f.get("role"),
                            "lines": f.get("lines"),
                        })
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        };

        serde_json::json!({
            "name": name,
            "kind": kind,
            "configFile": config_file,
            "platform": platform,
            "dependencies": dependencies,
            "files": files,
        })
    }

    /// `project/getChanges` — recent file changes.
    async fn handle_get_changes(&self) -> serde_json::Value {
        // `project/getChanges` is an intentional stub: only the manifest
        // HASH is persisted (`project.file_manifest_hash` in the graph
        // config); the full manifest is not stored, so real change
        // tracking isn't available yet.
        let manifest: Option<serde_json::Value> = None;

        serde_json::json!({
            "hasManifest": manifest.is_some(),
            "manifest": manifest,
            "note": "Full change tracking requires the sync manifest to be stored. Currently shows basic sync status.",
        })
    }

    /// `project/analysis` — comprehensive project analysis for the webview Project tab.
    ///
    /// Returns data in the format expected by the webview's `renderProjectAnalysis()`:
    ///   - overview: name, root, language, build_system, file_count, loc
    ///   - dependencies: [{name, version}]
    ///   - modules: [{name, path}]
    ///   - build_targets: [{name, kind}]
    async fn handle_analysis(&self) -> serde_json::Value {
        // Get the project node
        let project_nodes = match self
            .query_attr_nodes(Some("Project".to_string()), None, None, Some(1))
            .await
        {
            Ok(nodes) => nodes,
            Err(e) => {
                return serde_json::json!({"error": format!("Failed to query project: {}", e)})
            }
        };

        let project = match project_nodes.first() {
            Some(p) => p,
            None => {
                return serde_json::json!(
                    {"error": "No project node found. Has the project been synced?"}
                )
            }
        };

        // Query file nodes
        let file_nodes = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("File".to_string()), None, None)
            .await
            .unwrap_or_default();

        // Query directory nodes
        let dir_nodes = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("Directory".to_string()), None, None)
            .await
            .unwrap_or_default();

        // Query build system nodes
        let build_systems = self
            .query_attr_nodes(Some("Unknown".to_string()), Some("BuildSystem".to_string()), None, None)
            .await
            .unwrap_or_default();

        // Collect languages
        let mut languages: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for file in &file_nodes {
            if let Some(lang) = file.get("language").and_then(|v| v.as_str()) {
                languages.insert(lang.to_string());
            }
        }

        // Compute total lines
        let total_lines: usize = file_nodes
            .iter()
            .filter_map(|f| f.get("lines").and_then(|v| v.as_u64()))
            .sum::<u64>() as usize;

        // Build overview section
        let primary_language = languages.iter().next().cloned().unwrap_or_default();
        let primary_build_system = build_systems
            .first()
            .map(|bs| bs.name().to_string())
            .unwrap_or_default();

        let overview = serde_json::json!({
            "name": project.name(),
            "root": project.get("path"),
            "language": primary_language,
            "build_system": primary_build_system,
            "file_count": file_nodes.len(),
            "loc": total_lines,
        });

        // Build dependencies section — traverse DependsOn relationships from project
        let dependencies: Vec<serde_json::Value> = {
            let traversal = self
                .traverse(
                    project.id(),
                    TraversalOptions {
                        max_depth: 2,
                        relationship_types: Some(vec![RelationshipType::DependsOn]),
                        max_nodes: Some(200),
                        direction: Some(TraversalDirection::Out),
                    },
                )
                .await
                .unwrap_or(spire_core::models::memory_graph::TraversalResult {
                    nodes: vec![],
                    edges: vec![],
                    paths: vec![],
                });

            traversal
                .nodes
                .iter()
                .filter(|n| n.id() != project.id())
                .map(|n| {
                    serde_json::json!({
                        "name": n.name(),
                        "version": n.get("version"),
                    })
                })
                .collect()
        };

        // Build modules section — from directory nodes with roles
        let modules: Vec<serde_json::Value> = dir_nodes
            .iter()
            .filter_map(|d| {
                let path = d.get("path").and_then(|v| v.as_str())?;
                let name = d
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or(d.name());
                Some(serde_json::json!({
                    "name": name,
                    "path": path,
                }))
            })
            .collect();

        // Build build_targets section — from build system nodes
        let build_targets: Vec<serde_json::Value> = build_systems
            .iter()
            .map(|bs| {
                serde_json::json!({
                    "name": bs.name(),
                    "kind": bs.get("build_type"),
                })
            })
            .collect();

        serde_json::json!({
            "overview": overview,
            "dependencies": dependencies,
            "modules": modules,
            "build_targets": build_targets,
        })
    }

    // ── Message Handler ───────────────────────────────────────────────────

    /// Handle an incoming message.
    pub async fn handle_message(&mut self, msg: ProjectQueryMessage) {
        match msg {
            ProjectQueryMessage::Initialize {
                memory_graph_tx,
                project_root,
                reply_to,
            } => {
                self.memory_graph_tx = Some(memory_graph_tx);
                self.project_root = Some(project_root);
                info!("ProjectQueryActor initialized");
                let _ = reply_to.send(Ok(()));
            }
            ProjectQueryMessage::CallTool {
                tool,
                args,
                reply_to,
            } => {
                let result = self.handle_tool_call(&tool, &args).await;
                let _ = reply_to.send(result);
            }
            ProjectQueryMessage::ListTools { reply_to } => {
                let _ = reply_to.send(Self::tool_definitions());
            }
        }
    }

    // ── Structured diagnostics (Phase 1) ─────────────────────────────────────

    /// `project/diagnostics` — query Diagnostic graph nodes (persisted by the
    /// build manager after build/lint/test runs) and return normalized JSON:
    /// {file, line, column, message, severity, buildType, buildRunId}.
    async fn handle_project_diagnostics(&self, args: &serde_json::Value) -> serde_json::Value {
        let file_filter = args.get("filePath").and_then(|v| v.as_str());
        let severity_filter = args.get("severity").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;

        let nodes = match self
            .query_attr_nodes(Some("diagnostic".to_string()), None, None, None)
            .await
        {
            Ok(n) => n,
            Err(e) => return serde_json::json!({"error": format!("diagnostics query failed: {e}")}),
        };

        let mut diagnostics: Vec<serde_json::Value> = Vec::new();
        for attr in nodes {
            let id = attr.id().to_string();
            let Some(view) = attr.diagnostic() else {
                continue;
            };
            let file_s = view.file.clone().unwrap_or_default();
            if let Some(f) = file_filter {
                if !file_s.contains(f) {
                    continue;
                }
            }
            if let Some(s) = severity_filter {
                if !view.severity.eq_ignore_ascii_case(s) {
                    continue;
                }
            }
            diagnostics.push(serde_json::json!({
                "file": file_s,
                "line": view.line,
                "column": view.column,
                "message": view.message,
                "severity": view.severity,
                "buildType": view.build_type,
                "buildRunId": view.build_run_id,
                "diagnosticId": id,
            }));
        }
        diagnostics.sort_by(|a, b| {
            a["file"].as_str().cmp(&b["file"].as_str())
                .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
        });
        diagnostics.truncate(limit);
        serde_json::json!({ "diagnostics": diagnostics, "count": diagnostics.len() })
    }

    // ── Symbol index (Phase 2) ───────────────────────────────────────────────

    /// Derive a language-neutral module path from a file path. Converts
    /// `crates/ai-traps-mcp-core/src/server.rs` → `ai-traps-mcp-core`,
    /// `src/main.rs` → `.` (root). This is the module key used by the index.
    fn module_of(file_path: &str) -> String {
        let mut parts: Vec<&str> = file_path.split('/').collect();
        // Strip leading "." / "" segments.
        while parts.first().map(|p| *p == "." || p.is_empty()).unwrap_or(false) {
            parts.remove(0);
        }
        // If under a language source dir (src/, Sources/, crates/<m>/src), walk
        // up to the crate/module boundary.
        let mut i = parts.len().saturating_sub(1);
        while i > 0 {
            let seg = parts[i];
            if seg == "src" || seg == "Sources" || seg == "lib" {
                // Crate boundary: crates/<name>/src/... → the crate name.
                if i >= 2 && parts[i - 2] == "crates" {
                    let mut out = parts[..i - 1].join("/");
                    if out.ends_with('/') {
                        out.pop();
                    }
                    return if out.is_empty() { ".".to_string() } else { out };
                }
                // Plain src/<...> under project root → root module.
                return ".".to_string();
            }
            i = i.saturating_sub(1);
        }
        // No source dir — module = parent dir (or root).
        if parts.len() <= 1 {
            ".".to_string()
        } else {
            parts[..parts.len() - 1].join("/")
        }
    }

    /// Project-wide symbol index keyed by (module_path, name). Built on demand
    /// from the graph's AST nodes so cross-file references resolve even when
    /// the parser has not yet stored cross-file `calls` edges (the current
    /// per-file walker only links callees within the same file). Enumerating
    /// references by name across modules is what makes rename-safe tools
    /// possible ("rename never misses a usage").
    fn build_symbol_index(&self, symbols: &[spire_core::models::analysis::GraphSymbol]) -> HashMap<(String, String), Vec<String>> {
        let mut index: HashMap<(String, String), Vec<String>> = HashMap::new();
        for sym in symbols {
            let module = Self::module_of(&sym.file_path);
            index
                .entry((module, sym.name.clone()))
                .or_default()
                .push(sym.symbol_id.clone());
        }
        index
    }

    // ── Symbol tools (Phase 1) ───────────────────────────────────────────────

    /// `symbols/search` — query AST nodes by name/kind and return stable-`symbolId`
    /// graph symbols (projected from AstFunction/AstClass/AstVariable/AstImport).
    async fn handle_symbols_search(&self, args: &serde_json::Value) -> serde_json::Value {
        let name_filter = args.get("name").and_then(|v| v.as_str());
        let kind_filter = args.get("kind").and_then(|v| v.as_str());
        let file_filter = args.get("filePath").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

        let mut symbols: Vec<spire_core::models::analysis::GraphSymbol> = Vec::new();
        for nt in ["astFunction", "astClass", "astVariable", "astImport"] {
            match self.query_attr_nodes(Some(nt.to_string()), None, None, None).await {
                Ok(nodes) => {
                    for attr in nodes {
                        if let Some(sym) = spire_core::models::analysis::GraphSymbol::from_attr_node(&attr) {
                            let kind_hit = kind_filter
                                .map(|k| sym.kind.to_lowercase().contains(&k.to_lowercase()))
                                .unwrap_or(true);
                            if !kind_hit {
                                continue;
                            }
                            let name_hit = name_filter
                                .map(|n| sym.name.to_lowercase().contains(&n.to_lowercase()))
                                .unwrap_or(true);
                            if !name_hit {
                                continue;
                            }
                            let file_hit = file_filter
                                .map(|f| sym.file_path.contains(f))
                                .unwrap_or(true);
                            if file_hit {
                                symbols.push(sym);
                            }
                        }
                    }
                }
                Err(e) => return serde_json::json!({"error": format!("symbol search failed: {e}")}),
            }
        }
        symbols.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.start_col.cmp(&b.start_col))
        });
        symbols.truncate(limit);
        serde_json::json!({ "symbols": symbols, "count": symbols.len() })
    }

    /// `symbols/definition` — fetch a single AST node by id and return its
    /// rich graph symbol. `includeBody=false` drops the (potentially large)
    /// `body`/`signature` text.
    async fn handle_symbols_definition(&self, args: &serde_json::Value) -> serde_json::Value {
        let symbol_id = match args.get("symbolId").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return serde_json::json!({"error": "missing 'symbolId'"}),
        };
        let include_body = args.get("includeBody").and_then(|v| v.as_bool()).unwrap_or(true);

        let attr = match self.get_attr_node(symbol_id).await {
            Ok(Some(a)) => a,
            Ok(None) => return serde_json::json!({"error": format!("symbol not found: {symbol_id}")}),
            Err(e) => return serde_json::json!({"error": format!("lookup failed: {e}")}),
        };
        match spire_core::models::analysis::GraphSymbol::from_attr_node(&attr) {
            Some(mut sym) => {
                if !include_body {
                    sym.body = None;
                    sym.signature = None;
                }
                serde_json::json!({ "symbol": sym })
            }
            None => serde_json::json!({"error": "node is not an AST symbol"}),
        }
    }

    /// `symbols/references` — incoming edges of kind calls/references/imports
    /// pointing at the symbol's node, PLUS cross-file name matches from the
    /// Phase-2 symbol index (so renames never miss a usage even where
    /// cross-file call edges aren't persisted yet). Returns caller context.
    async fn handle_symbols_references(&self, args: &serde_json::Value) -> serde_json::Value {
        let symbol_id = match args.get("symbolId").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return serde_json::json!({"error": "missing 'symbolId'"}),
        };
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;

        // Edge-based references (same-file calls, imports, semantic edges).
        let edges = match self.get_relationships(symbol_id).await {
            Ok(e) => e,
            Err(e) => return serde_json::json!({"error": format!("relationships failed: {e}")}),
        };
        let interesting = ["calls", "references", "imports", "calledby", "semanticallyrelated"];
        let mut refs: Vec<serde_json::Value> = Vec::new();
        for edge in edges {
            let rel = match &edge.edge_type {
                spire_core::models::memory_graph::RelationshipType::Custom(name) => name.to_lowercase(),
                other => format!("{:?}", other).to_lowercase(),
            };
            if interesting.iter().any(|i| rel.contains(i)) {
                let from_node = self.get_attr_node(&edge.from_id).await.ok().flatten();
                refs.push(serde_json::json!({
                    "fromId": edge.from_id,
                    "fromName": from_node.map(|n| n.name().to_string()).unwrap_or_default(),
                    "relationshipType": rel,
                    "toId": edge.to_id,
                    "relationSource": "edge",
                }));
            }
        }

        // Cross-file name matches via the symbol index. Find the target's
        // module+name, then every AST node with the same (module,name) or
        // same name in ANY module (if the target module can't be established,
        // fall back to name-only across the project). Tagged so callers can
        // distinguish graph-backed edges from name heuristics.
        let target_attr = self.get_attr_node(symbol_id).await.ok().flatten();
        if let Some(target) = target_attr.as_ref() {
            let target_name = target.name().to_string();
            // We need the full AST symbol set to build the index.
            let mut symbols: Vec<spire_core::models::analysis::GraphSymbol> = Vec::new();
            for nt in ["astFunction", "astClass", "astVariable", "astImport"] {
                if let Ok(nodes) = self.query_attr_nodes(Some(nt.to_string()), None, None, None).await {
                    symbols.extend(
                        nodes
                            .iter()
                            .filter_map(spire_core::models::analysis::GraphSymbol::from_attr_node),
                    );
                }
            }
            let index = self.build_symbol_index(&symbols);
            let target_mod = match target.str_prop("file_path") {
                Some(fp) => Self::module_of(&fp),
                None => Self::module_of(&target_name),
            };
            // Same-module, same-name matches are genuine local re-uses; any
            // same-name match in another module is a candidate cross-file
            // reference (the parser's per-file call resolution can't see it).
            let candidates: Vec<String> = index
                .get(&(target_mod.clone(), target_name.clone()))
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .chain(
                    index
                        .iter()
                        .filter(|((m, n), _)| *n == target_name && *m != target_mod)
                        .flat_map(|(_, ids)| ids.clone()),
                )
                .filter(|id| id != symbol_id)
                .collect();
            // Also add cross-module matches as explicit relationshipType.
            for other_id in candidates {
                if refs.iter().any(|r| r["fromId"] == other_id) {
                    continue;
                }
                let other = self.get_attr_node(&other_id).await.ok().flatten();
                refs.push(serde_json::json!({
                    "fromId": other_id,
                    "fromName": other.map(|n| n.name().to_string()).unwrap_or_default(),
                    "relationshipType": "cross_file",
                    "toId": symbol_id,
                    "relationSource": "symbol_index",
                }));
            }
        }

        refs.truncate(limit);
        serde_json::json!({ "references": refs, "count": refs.len(), "symbolId": symbol_id })
    }

    /// Route a tool call to the appropriate handler.
    async fn handle_tool_call(&self, tool: &str, args: &serde_json::Value) -> serde_json::Value {
        match tool {
            "project/getOverview" => self.handle_get_overview().await,
            "project/getFileTree" => self.handle_get_file_tree(args).await,
            "project/getFileDetails" => self.handle_get_file_details(args).await,
            "project/searchFiles" => self.handle_search_files(args).await,
            "project/getBuildConfig" => self.handle_get_build_config().await,
            "project/getDependencies" => self.handle_get_dependencies(args).await,
            "project/getEntryPoints" => self.handle_get_entry_points().await,
            "project/getArchitecture" => self.handle_get_architecture().await,
            "project/getEntities" => self.handle_get_entities(args).await,
            "project/getRelationships" => self.handle_get_relationships(args).await,
            "project/queryGraph" => self.handle_query_graph(args).await,
            "project/getBuildTarget" => self.handle_get_build_target(args).await,
            "project/getChanges" => self.handle_get_changes().await,
            "project/analysis" => self.handle_analysis().await,
            "project/diagnostics" => self.handle_project_diagnostics(args).await,
            "symbols/search" => self.handle_symbols_search(args).await,
            "symbols/definition" => self.handle_symbols_definition(args).await,
            "symbols/references" => self.handle_symbols_references(args).await,
            "symbols/rename" => self.handle_symbols_rename(args).await,
            _ => serde_json::json!({"error": format!("Unknown tool: {}", tool)}),
        }
    }
}

// ============================================================================
// Actor trait implementation
// ============================================================================

#[async_trait]
impl Actor for ProjectQueryActor {
    type Message = ProjectQueryMessage;

    async fn handle(&mut self, msg: Self::Message) {
        self.handle_message(msg).await;
    }
}

// ============================================================================
// Free Functions
// ============================================================================

/// Recursively add a file to the tree structure.
fn add_to_tree(
    tree: &mut serde_json::Value,
    path_parts: &[&str],
    file_info: &serde_json::Value,
    depth: usize,
    max_depth: Option<u64>,
) {
    if let Some(max) = max_depth {
        if depth as u64 > max {
            return;
        }
    }

    if path_parts.is_empty() {
        if let Some(children) = tree.get_mut("children").and_then(|c| c.as_array_mut()) {
            children.push(file_info.clone());
        }
        return;
    }

    let dir_name = path_parts[0];
    let children = tree
        .get_mut("children")
        .and_then(|c| c.as_array_mut())
        .unwrap();

    let mut found = false;
    for child in children.iter_mut() {
        if child["name"] == dir_name && child["type"] == "directory" {
            add_to_tree(child, &path_parts[1..], file_info, depth + 1, max_depth);
            found = true;
            break;
        }
    }

    if !found {
        let mut dir_entry = serde_json::json!({
            "name": dir_name,
            "path": "",
            "type": "directory",
            "role": "",
            "children": []
        });
        add_to_tree(
            &mut dir_entry,
            &path_parts[1..],
            file_info,
            depth + 1,
            max_depth,
        );
        children.push(dir_entry);
    }
}

/// Recursively annotate directories with their semantic roles.
fn annotate_tree_roles(tree: &mut serde_json::Value, dir_roles: &HashMap<&str, &str>) {
    // Extract parent path before mutable borrow
    let parent_path = tree["path"].as_str().unwrap_or("").to_string();

    if let Some(children) = tree.get_mut("children").and_then(|c| c.as_array_mut()) {
        for child in children.iter_mut() {
            if child["type"] == "directory" {
                // Build the path for this directory by combining parent path + name
                let dir_name = child["name"].as_str().unwrap_or("");
                let dir_path = if parent_path.is_empty() {
                    dir_name.to_string()
                } else {
                    format!("{}/{}", parent_path, dir_name)
                };
                child["path"] = serde_json::Value::String(dir_path.clone());

                // Look up the role
                if let Some(role) = dir_roles.get(dir_path.as_str()) {
                    child["role"] = serde_json::Value::String(role.to_string());
                }

                // Recurse into children
                annotate_tree_roles(child, dir_roles);
            }
        }
    }
}
