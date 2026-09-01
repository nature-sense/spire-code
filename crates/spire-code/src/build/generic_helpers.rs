// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Shared helpers for the language-specific build modules.
//!
//! `run_cmd` executes an external command in a project directory and captures
//! structured `BuildOutput`. `parse_key_value` extracts `name` / `version` /
//! `description` from a simple `key = value` config file, optionally scoped to
//! a `[section]` (empty section = whole file).

use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

use super::{AstEdgeData, AstNodeData, AstParseResult, BuildOutput};
use spire_core::build_types::{BuildMetadata, BuildSpec};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Execute a program in a project dir and capture structured output.
pub async fn run_cmd(path: &Path, program: &str, args: &[&str]) -> Result<BuildOutput, String> {
    run_cmd_with_env(path, program, args, &[])
        .await
}

/// Execute a program in a project dir with an optional env overrides list,
/// capturing structured output. `working_dir` (when non-empty) is joined onto
/// `path` so a `BuildSpec` can target a subdirectory of the module root.
pub async fn run_cmd_with_env(
    path: &Path,
    program: &str,
    args: &[&str],
    env: &[(String, String)],
) -> Result<BuildOutput, String> {
    let start = Instant::now();
    let command_str = format!("{program} {}", args.join(" "));
    let mut cmd = Command::new(program);
    cmd.current_dir(path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to execute {program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(BuildOutput {
        success: output.status.success(),
        command: command_str,
        duration_secs: start.elapsed().as_secs_f64(),
        output: if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}\n{stderr}")
        },
        exit_code: output.status.code(),
    })
}

/// Execute a normalized `BuildSpec` invocation: `working_dir` (relative to the
/// module root) is joined onto `path`, env overrides are applied, and the
/// command's stdout/stderr are captured into a `BuildOutput`. Modules route
/// `Build`/`BuildStreaming` directly to this when the selected target carries a
/// `build_spec`.
pub async fn run_build_spec(path: &Path, spec: &BuildSpec) -> Result<BuildOutput, String> {
    let cwd = if spec.working_dir.is_empty() {
        path.to_path_buf()
    } else {
        path.join(&spec.working_dir)
    };
    let args: Vec<&str> = spec.arguments.iter().map(|s| s.as_str()).collect();
    run_cmd_with_env(&cwd, &spec.command, &args, &spec.env).await
}

/// Lightweight regex-based source parser for generic modules (go, cmake, make,
/// maven, gradle, ruby). Extracts functions, classes, and imports without a
/// full tree-sitter grammar.
pub fn parse_source_file_basic(
    file_path: &Path,
    language: &str,
    content: &str,
    content_hash: &str,
) -> AstParseResult {
    let mut nodes: Vec<AstNodeData> = Vec::new();
    let mut edges: Vec<AstEdgeData> = Vec::new();

    // ── Pass 1: imports ────────────────────────────────────────────
    let import_patterns: &[(&str, &str)] = &[
        ("import", r"(?m)^\s*(?:import|from)\s+([A-Za-z0-9_\.]+)"),
        (
            "use",
            r"(?m)^\s*(?:use|using|include|require)\s+([A-Za-z0-9_\.:\/]+)",
        ),
    ];
    let mut import_lines: HashSet<u32> = HashSet::new();
    for (label, pat) in import_patterns {
        let re = match Regex::new(pat) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for cap in re.captures_iter(content) {
            let name = cap.get(1).map(|m| m.as_str().to_string());
            let pos = cap.get(0).map(|m| m.start()).unwrap_or(0);
            let (line, col) = offset_to_line_col(content, pos);
            import_lines.insert(line);
            let node = AstNodeData {
                node_type: "import".to_string(),
                name: name.clone(),
                text: cap
                    .get(0)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default(),
                start_line: line,
                start_col: col,
                end_line: line,
                end_col: col + cap.get(0).map(|m| m.as_str().len() as u32).unwrap_or(0),
                depth: 0,
                is_public: false,
                is_async: false,
                signature: None,
                return_type: None,
                children: Vec::new(),
            };
            let idx = nodes.len();
            nodes.push(node);
            edges.push(AstEdgeData {
                from_index: idx,
                to_index: idx,
                edge_type: "imports".to_string(),
                order: Some(idx as u32),
                field: Some(label.to_string()),
            });
        }
    }

    // ── Pass 2: function / class declarations ──────────────────────
    let decl_patterns: &[(&str, &str)] = &[
        (
            "function",
            r"(?m)^\s*(?:pub\s+|public\s+|export\s+)?(?:async\s+|fn\s+|func\s+|def\s+|function\s+)([A-Za-z0-9_]+)\s*(?:<[^>]+>)?\s*\(",
        ),
        (
            "class",
            r"(?m)^\s*(?:pub\s+|public\s+|export\s+)?(?:class\s+|struct\s+|trait\s+|type\s+|interface\s+|enum\s+|protocol\s+|extension\s+)([A-Za-z0-9_]+)",
        ),
    ];
    for (label, pat) in decl_patterns {
        let re = match Regex::new(pat) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for cap in re.captures_iter(content) {
            let name = cap.get(1).map(|m| m.as_str().to_string());
            let pos = cap.get(0).map(|m| m.start()).unwrap_or(0);
            let (line, col) = offset_to_line_col(content, pos);
            if import_lines.contains(&line) {
                continue;
            }
            let full_match = cap
                .get(0)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let is_public = full_match.starts_with("pub ")
                || full_match.starts_with("public ")
                || full_match.starts_with("export ");
            let is_async = full_match.contains("async ");
            let text = full_match.clone();
            let signature_len = full_match.len() as u32;
            let node = AstNodeData {
                node_type: label.to_string(),
                name: name.clone(),
                text,
                start_line: line,
                start_col: col,
                end_line: line,
                end_col: col + signature_len,
                depth: 0,
                is_public,
                is_async,
                signature: Some(full_match),
                return_type: None,
                children: Vec::new(),
            };
            nodes.push(node);
        }
    }

    AstParseResult {
        file_path: file_path.to_string_lossy().to_string(),
        language: language.to_string(),
        content_hash: content_hash.to_string(),
        has_errors: nodes.is_empty(),
        nodes,
        edges,
        docs: Vec::new(),
    }
}

/// Convert a byte offset in a string to a (1-based line, 0-based col).
pub fn offset_to_line_col(content: &str, offset: usize) -> (u32, u32) {
    let mut line = 1u32;
    let mut col = 0u32;
    for (i, ch) in content.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Compute a SHA-256 hex digest of file content (for incremental re-parse).
pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Parse a source file from disk using the basic regex parser. Returns an
/// `AstParseResult` with a computed content hash. This is the default
/// implementation for modules without a dedicated tree-sitter grammar.
pub fn parse_source_file_std(file_path: &Path, language: &str) -> Result<AstParseResult, String> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;
    let hash = sha256_hex(&content);
    Ok(parse_source_file_basic(
        file_path, language, &content, &hash,
    ))
}

/// Parse a C/C++ source/header file. Runs the basic parser (imports + class
/// declarations), then `extract_cpp_methods` to add member-function nodes
/// (interface methods, constructors, destructors) as `child` edges of their
/// enclosing class/struct, with `signature`/`return_type` populated and the
/// `virtual`/`= 0`/`= default` markers preserved in `signature` so HAL
/// contract consumers can derive the pure-virtual method set.
///
/// NOTE: this remains the generic AST-storage parser (headers parsed into the
/// knowledge graph via `ParseSourceFile`). HAL contract *semantics* (which
/// methods are pure-virtual/abstract) are handled by the tree-sitter
/// `extract_contract_methods_cpp` — this function only provides the AST nodes.
pub fn parse_cpp_source_file_std(file_path: &Path) -> Result<AstParseResult, String> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;
    let hash = sha256_hex(&content);

    // Pass 1: the generic parser (imports + class/struct headers).
    let mut base = parse_source_file_basic(file_path, "C/C++", &content, &hash);

    // Pass 2: member-function extraction. `extract_cpp_methods` returns
    // (enclosing class name, method node) pairs; resolve each method's parent
    // to the class node index in the base list and emit the `child` edge.
    use std::collections::HashMap;
    let extracted = extract_cpp_methods(&content);
    if !extracted.is_empty() {
        let mut class_index: HashMap<&str, usize> = HashMap::new();
        for (i, n) in base.nodes.iter().enumerate() {
            if n.node_type == "class" {
                if let Some(name) = n.name.as_deref() {
                    class_index.entry(name).or_insert(i);
                }
            }
        }
        let mut method_nodes: Vec<AstNodeData> = Vec::new();
        let mut child_edges: Vec<AstEdgeData> = Vec::new();
        for (class_name, method) in extracted {
            let method_idx = base.nodes.len() + method_nodes.len();
            if let Some(&parent) = class_index.get(class_name.as_str()) {
                child_edges.push(AstEdgeData {
                    from_index: parent,
                    to_index: method_idx,
                    edge_type: "child".to_string(),
                    order: Some(method_nodes.len() as u32),
                    field: None,
                });
            }
            method_nodes.push(method);
        }
        base.nodes.extend(method_nodes);
        base.edges.extend(child_edges);
    }
    base.has_errors = base.nodes.is_empty();
    // Attach the annotated doc comments so the persisted AST is
    // self-describing (same HalDocs the HAL graph/viewer consume).
    base.docs = parse_hal_docs(&content);
    Ok(base)
}

/// Extract member-function declarations (methods, constructors, destructors)
/// from C++ `class`/`struct` bodies.
///
/// For each class/struct we brace-match its body and match member declarations
/// of the forms:
///
///   virtual ReturnType name(params) const override = 0;
///   ReturnType name(params) const;
///   explicit ClassName(args);
///   virtual ~ClassName();
///
/// Each match yields an `AstNodeData` with `node_type = "method"`, the full
/// declaration in `signature` (so `virtual` / `= 0` / `= default` are visible
/// to downstream HAL-contract tooling), `return_type` (None for ctor/dtor),
/// and `is_public` from the access specifier active at that method's line.
/// The returned vector is `(enclosing class name, method node)` — the caller
/// resolves the class node index to build the `child` edge.
fn extract_cpp_methods(content: &str) -> Vec<(String, AstNodeData)> {
    let mut out: Vec<(String, AstNodeData)> = Vec::new();

    // Class/struct header with an opening brace on the same logical line:
    //   class CameraHAL {   /   struct Foo : public Bar {
    let class_re = Regex::new(
        r"(?m)^\s*(?:class|struct)\s+([A-Za-z_][A-Za-z0-9_]*)\b[^{]*\{",
    )
    .unwrap();

    // Member-function declaration regex with named groups. The return-type
    // group is greedy but followed by a method-name token + `(`, so it stops
    // at the longest prefix that leaves a valid name/`(`. `~`-prefixed names
    // are destructors (no return type). End delimiter `;` or `{` (inline).
    let method_re = Regex::new(
        r"(?m)^\s*(?P<head>virtual\s+|static\s+|explicit\s+|virtual\s+static\s+)*\s*(?P<ret>[\w:<>,.*&\[\]\s]+?)?\s*(?P<name>[A-Za-z_~][A-Za-z0-9_:~]*)\s*\((?P<params>[^;{}]*?)\)\s*(?:const\s+)?(?:override\s+)?(?P<purity>=\s*(?:0|default|delete))?\s*[;{]",
    )
    .unwrap();

    for cap in class_re.captures_iter(content) {
        let Some(cname) = cap.get(1) else { continue };
        let class_name = cname.as_str().to_string();
        let Some(body_start_match) = cap.get(0) else { continue };
        // The match ends exactly at the class body's `{`.
        let body_open = body_start_match.end() - 1;
        let Some(body_close) = match_closing_brace(content, body_open) else {
            continue; // unbalanced — skip this class
        };
        let body = &content[body_open + 1..body_close];
        let body_start = body_open + 1;

        // Access region resolution: walk the body's lines and record per-line
        // the last access specifier seen. C++ default access differs by kind:
        //   class  → private
        //   struct → public
        // Determine the kind from the matched header (`struct X {` → public).
        let header_text = cap.get(0).map(|g| g.as_str()).unwrap_or("");
        let header_kind_struct = header_text.trim_start().starts_with("struct");
        let mut line_public: Vec<bool> = Vec::new();
        let mut is_public = header_kind_struct; // class default: private; struct default: public
        for line in body.lines() {
            let t = line.trim_start();
            if t.starts_with("public:") {
                is_public = true;
            } else if t.starts_with("private:") || t.starts_with("protected:") {
                is_public = false;
            }
            line_public.push(is_public);
        }

        for m in method_re.captures_iter(body) {
            let Some(mname) = m.name("name") else { continue };
            let name = mname.as_str().trim();
            // Reject control-flow / statement keywords that a `(…)` could
            // otherwise capture (e.g. `if (…) {` inside an inline body).
            if name.is_empty()
                || name == "if" || name == "for" || name == "while" || name == "switch"
                || name == "return" || name == "catch" || name == "sizeof"
            {
                continue;
            }
            // The return-type group may have greedily eaten a scoped name
            // (e.g. `Some::Type method`), but `name` already matched a token
            // that includes `::` segments — dedupe: if the matched `name`
            // equals a word just seen in `ret`, treat it as part of the
            // qualified name. Simple heuristic: if `ret` ends with `name`,
            // the group boundary was wrong — prefer the LONGEST name (the
            // qualified one).
            let name = {
                let raw = name;
                if let Some(ret) = m.name("ret") {
                    let ret_last_ws = ret.as_str().trim_end();
                    let last_word = ret_last_ws.rsplit(|c: char| c.is_whitespace() || c == ':' || c == '<' || c == ',').next().unwrap_or("");
                    if !last_word.is_empty() && raw.starts_with(last_word) {
                        // `ret` already contains the last segment — combine:
                        // qualified name = `<ret stripped of trailing ws> + raw`.
                        // This is rare; keep raw and drop the ret's last word.
                        format!("{}::{}", last_word, raw)
                    } else {
                        raw.to_string()
                    }
                } else {
                    raw.to_string()
                }
            };

            let full = m.get(0).map(|g| g.as_str().trim().to_string()).unwrap_or_default();
            if full.is_empty() {
                continue;
            }

            // Return type from the named group (None for ctor/dtor).
            let return_type = if name.starts_with('~') || name.contains('~') {
                None
            } else {
                match m.name("ret") {
                    Some(ret) => {
                        let r = ret.as_str().trim();
                        if r.is_empty() { None } else { Some(r.to_string()) }
                    }
                    None => None,
                }
            };

            // Purity marker (`= 0`, `= default`, `= delete`) preserved in the
            // signature and exposed as a property via `signature`.
            let purity = m.name("purity").map(|p| p.as_str().trim().to_string());

            // Absolute position for line/col.
            let rel = m.get(0).map(|g| g.start()).unwrap_or(0);
            let (abs_line, abs_col) = offset_to_line_col(content, body_start + rel);

            // Access region: the line the method starts on.
            let rel_line = body[..rel].matches('\n').count();
            let is_public = line_public.get(rel_line).copied().unwrap_or(false);

            let mut sig = full.clone();
            if let Some(p) = &purity {
                // Ensure the purity marker is retained even though the regex
                // captured it outside the `full` match is unlikely — but keep.
                if !sig.contains("=") && p.starts_with('=') {
                    sig.push(' ');
                    sig.push_str(p);
                }
            }

            // Precompute for `end_col` (full is moved into `text` below).
            let full_len = full.chars().count() as u32;

            out.push((
                class_name.clone(),
                AstNodeData {
                    node_type: "method".to_string(),
                    name: Some(name),
                    text: full,
                    start_line: abs_line,
                    start_col: abs_col,
                    // End = start + declared length (single-line decls).
                    end_line: abs_line,
                    end_col: abs_col + full_len,
                    depth: 1,
                    is_public,
                    is_async: false,
                    signature: Some(sig),
                    return_type,
                    children: Vec::new(),
                },
            ));
        }
    }

    out
}

/// Find the offset just past the `}` matching the `{` at `open`.
fn match_closing_brace(content: &str, open: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b'"' => {
                // skip string literal (best-effort)
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    break;
                }
            }
            _ => {}
        }
    }
    None
}

/// Classification of a C/C++ header under `hal/api/` (or the legacy
/// `toolkit/src/hal/api/`), derived from its **AST** rather than raw text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalHeaderKind {
    /// Declares at least one public pure-virtual method — a real HAL contract.
    Contract,
    /// Declares classes/structs but NO pure-virtual methods — a pure-data
    /// definition (e.g. `types.hpp`, `frame_buffer.hpp`) legitimate as a
    /// shared support header.
    DataOnly,
    /// No class/struct declarations (free functions, stray decls).
    NoClass,
    /// File could not be read or parsed.
    ParseError,
}

/// Classify a header using the **tree-sitter-C++ CST** (`extract_contract_methods_cpp`):
///
/// - a header is `Contract` when at least one public pure-virtual method exists;
/// - `DataOnly` when there are class/struct nodes but none has a public
///   pure-virtual method — this is exactly the case the user reported (3 `.hpp`
///   files of pure data definitions);
/// - `NoClass` when the AST has no class/struct declarations;
/// - `ParseError` when reading fails.
/// Returns `true` when any declared class in the header derives the HAL module
/// marker base (`HalModule`, possibly qualified `hal::HalModule`).
fn derives_hal_module(content: &str) -> bool {
    extract_cpp_base_classes(content)
        .iter()
        .any(|(_, bases)| bases.iter().any(|b| b == "HalModule" || b == "hal::HalModule"))
}

pub fn classify_hal_header(path: &Path) -> HalHeaderKind {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HalHeaderKind::ParseError,
    };
    let classes = extract_contract_methods_cpp(&content);
    // Phase 1 (HAL discoverability): a header is a Contract when it declares a
    // class that derives `hal::HalModule` — the semantic, self-describing rule.
    if derives_hal_module(&content) {
        return HalHeaderKind::Contract;
    }
    if classes.is_empty() {
        // Did the file declare any class/struct at all? Distinguish "no class"
        // from "classes but no contract" so data-only headers (structs etc.)
        // are not mislabeled as NoClass.
        let has_class_decl = content.contains("class ") || content.contains("struct ");
        return if has_class_decl {
            HalHeaderKind::DataOnly
        } else {
            HalHeaderKind::NoClass
        };
    }
    // Legacy fallback: any class with ≥1 pure-virtual method is a contract.
    // Kept so pre-HalModule projects (e.g. the current `ct` namespace) still
    // classify until they migrate.
    HalHeaderKind::Contract
}

/// Extract `name` / `version` / `description` from a `key = value` config.
///
/// `section` may be empty (parse the whole file) or a heading like `[project]`.
pub fn parse_key_value(path: &Path, config: &str, section: &str) -> Result<BuildMetadata, String> {
    let content = std::fs::read_to_string(path.join(config))
        .map_err(|e| format!("Failed to read {config}: {e}"))?;
    let mut name = None;
    let mut version = None;
    let mut description = None;
    let mut in_section = section.is_empty();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = section.is_empty() || trimmed[1..trimmed.len() - 1] == *section;
            continue;
        }
        if !in_section {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if name.is_none() && lower.starts_with("name = ") {
            if let Some(val) = trimmed.split('=').nth(1) {
                name = Some(val.trim().trim_matches('"').trim_matches('\'').to_string());
            }
        }
        if version.is_none() && lower.starts_with("version = ") {
            if let Some(val) = trimmed.split('=').nth(1) {
                version = Some(val.trim().trim_matches('"').trim_matches('\'').to_string());
            }
        }
        if description.is_none() && lower.starts_with("description = ") {
            if let Some(val) = trimmed.split('=').nth(1) {
                description = Some(val.trim().trim_matches('"').trim_matches('\'').to_string());
            }
        }
    }

    Ok(BuildMetadata {
        project_name: name,
        description,
        version,
        config_files: vec![config.to_string()],
        project_path: Some(path.to_string_lossy().to_string()),
        ..Default::default()
    })
}

/// Validate a proposed HAL contract header and summarize it for the
/// contract-first flow (Stage 0).
///
/// Reuses the C++ header extractor: the content must contain at least one
/// abstract class (a `class`/`struct` with ≥1 public pure-virtual (`= 0`)
/// method) and no impls (no `.cpp` body is parsed here — the file is a header
/// by construction). Returns a compact, deterministic contract summary:
///
///   ```text
///   CameraHAL: virtual bool start(); virtual std::uint32_t capture(int timeout_ms);
///   ```
///
/// which the LLM-facing plan step echoes so the user sees exactly what the
/// contract will register, and which the "add target" placeholder generator
/// can consume as the method list.
pub fn summarize_hal_header(content: &str) -> Result<String, String> {
    // The tree-sitter-C++ CST is the only contract extractor — there is no
    // regex fallback. (The regex path was removed when the analyzer and the
    // coverage/fill queue were converged onto the same AST pipeline.)
    let classes = extract_contract_methods_cpp(content);
    if classes.is_empty() {
        return Err("no public pure-virtual methods found in HAL header".to_string());
    }
    let mut summaries: Vec<String> = Vec::new();
    for (class_name, methods) in &classes {
        let parts: Vec<String> = methods
            .iter()
            .map(|m| {
                let ret = m.return_type.trim();
                if ret.is_empty() {
                    format!("{}({}) = 0", m.name, m.params.trim())
                } else {
                    format!("{} {}({}) = 0", ret, m.name, m.params.trim())
                }
            })
            .collect();
        summaries.push(format!("{}: {}", class_name, parts.join("; ")));
    }
    if summaries.is_empty() {
        return Err("no public pure-virtual methods found in HAL header".to_string());
    }
    Ok(summaries.join("\n"))
}

/// One public pure-virtual contract method, parsed from a `summarize_hal_header`
/// summary line (`Ret name(params) = 0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HalContractMethod {
    pub name: String,
    pub return_type: String,
    pub params: String,
}

/// Parse a contract summary (from `summarize_hal_header`) into
/// `(class name, [methods])` per line. Method names = the last token before
/// `(`; return types precede them; params come from inside the parens.
/// Default-argument values (`= x`) are stripped so the values can be reused in
/// out-of-class definitions (C++ forbids repeating defaults there).
pub fn parse_hal_contract_summary(summary: &str) -> Vec<(String, Vec<HalContractMethod>)> {
    let mut classes: Vec<(String, Vec<HalContractMethod>)> = Vec::new();
    for line in summary.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(colon) = line.find(':') else { continue };
        let class_name = line[..colon].trim().to_string();
        let mut methods: Vec<HalContractMethod> = Vec::new();
        for part in line[colon + 1..].split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let Some(po) = part.find('(') else { continue };
            let Some(close_rel) = part[po..].find(')') else { continue };
            let bare_params = &part[po + 1..po + close_rel];
            // Strip default-argument values: `int timeout_ms = 100` → `int timeout_ms`.
            let params: String = bare_params
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .map(|p| {
                    p.split_once('=')
                        .map(|(head, _)| head.trim().to_string())
                        .unwrap_or_else(|| p.to_string())
                })
                .collect::<Vec<_>>()
                .join(", ");
            let head = part[..po].trim_end();
            let tokens: Vec<&str> = head.split_whitespace().collect();
            if tokens.is_empty() {
                continue;
            }
            let name = tokens.last().unwrap().to_string();
            let return_type = tokens[..tokens.len() - 1].join(" ");
            methods.push(HalContractMethod {
                name,
                return_type,
                params,
            });
        }
        if !methods.is_empty() {
            classes.push((class_name, methods));
        }
    }
    classes
}

/// Sentinel line placed at the top of every generated HAL placeholder stub.
/// Machine-detectable (cheap substring check — no AST needed) so coverage and
/// fill planning can tell a pending stub apart from a real implementation.
pub const SPIRE_HAL_STUB_SENTINEL: &str = "SPIRE-HAL-STUB";

/// True when `content` carries the pending-implementation sentinel
/// (`SPIRE-HAL-STUB`). Used by the HAL coverage/queue tooling to treat a
/// stem-matching file as "needs a real implementation" instead of implemented.
pub fn is_hal_stub(content: &str) -> bool {
    content.contains(SPIRE_HAL_STUB_SENTINEL)
}

/// True when the file at `path` carries the pending-implementation sentinel.
pub fn is_hal_stub_file(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|c| is_hal_stub(&c))
        .unwrap_or(false)
}

/// Generate the per-method `override` placeholder implementation source for one
/// HAL interface on one platform (Stage 2 — "add target"). Each pure-virtual
/// method becomes an out-of-class definition with a `// TODO` body:
///
///   ```cpp
///   #include "camera_hal.hpp"
///   bool CameraHAL::start() { /* TODO: implement for rpi5 */ return {}; }
///   ```
///
/// `header_stem` is the contract header basename without extension
/// (e.g. "camera_hal" → `#include "camera_hal.hpp"`).
pub fn generate_hal_placeholder_source(
    header_stem: &str,
    class_name: &str,
    methods: &[HalContractMethod],
    platform: &str,
) -> String {
    let mut src = String::new();
    // Machine-detectable pending marker + a compiler-visible pragma so builds
    // surface the stub as needing a real implementation without breaking them
    // (werror=false → the message is a notice, not an error).
    src.push_str(&format!(
        "// {}: {header_stem}.cpp — {platform} implementation pending.\n\
         // Replace the TODO bodies below with a real implementation.\n\
         #pragma message(\"SPIRE HAL stub needs implementation: {header_stem}.cpp ({platform})\")\n\n",
        SPIRE_HAL_STUB_SENTINEL
    ));
    src.push_str(&format!("#include \"{header_stem}.hpp\"\n\n"));
    for m in methods {
        let ret = m.return_type.trim();
        let ret_void = ret.is_empty() || ret == "void";
        src.push_str(&format!(
            "{ret} {class_name}::{name}({params}) {{\n    /* TODO: implement for {platform} */\n",
            class_name = class_name,
            name = m.name,
            params = m.params
        ));
        if !ret_void {
            src.push_str("    return {};\n");
        }
        src.push_str("}\n\n");
    }
    src
}

/// Generate the **concrete declaration header** for a HAL module pair.
///
/// `impl_class` is the concrete class name (e.g. `CameraHalImx219`), derived
/// from `<Interface><Variant>`. The header declares the class deriving from the
/// contract's abstract base (`header_stem.hpp`), public overrides + destructor,
/// and a private-state block placeholder the LLM/manual writer fills in (SDK
/// handles, locks, pools). The `SPIRE-HAL-STUB` sentinel marks it pending.
pub fn generate_hal_module_header(
    header_stem: &str,
    impl_class: &str,
    base_class: &str,
    methods: &[HalContractMethod],
    platform: &str,
) -> String {
    let mut h = format!(
        "// {0}: {impl_class}.hpp — {platform} implementation pending.\n\
         // Replace the state + TODO bodies below with a real implementation.\n\
         #pragma message(\"SPIRE HAL stub needs implementation: {impl_class}.hpp ({platform})\")\n\n",
        SPIRE_HAL_STUB_SENTINEL
    );
    h.push_str(&format!("#include \"hal/api/{header_stem}.hpp\"\n\n"));
    h.push_str(&format!("namespace hal {{\n\n"));
    h.push_str(&format!("/// {impl_class} — {platform} implementation of {base_class}.\n"));
    h.push_str(&format!("class {impl_class} : public {base_class} {{\n"));
    h.push_str("public:\n");
    h.push_str(&format!("    {impl_class}() = default;\n"));
    h.push_str(&format!("    ~{impl_class}() override = default;\n\n"));
    for m in methods {
        let ret = m.return_type.trim();
        // Skip the destructor (already declared); other lifecycle/contract
        // methods get an override declaration.
        h.push_str(&format!(
            "    {ret} {name}({params}) override;\n\n",
            name = m.name,
            params = m.params
        ));
    }
    h.push_str("private:\n");
    h.push_str("    // TODO: platform state (SDK handles, buffers, locks, config).\n");
    h.push_str("    struct Impl;\n");
    h.push_str("    Impl* impl_ = nullptr;\n");
    h.push_str("};\n\n");
    h.push_str("} // namespace hal\n");
    h
}

/// Derive a concrete HAL implementation class name from an interface stem +
/// variant token (`camera_hal` + `rock3c` → `CameraHalRock3c`). Every
/// underscore-separated segment is capitalized (camelCase), matching the
/// codebase convention: `video_scaler` + `rpi5` → `VideoScalerRpi5`.
/// Shared by `hal_fill_plan` (module-pair scaffolding) and `hal_generate_impl`
/// (semantic LLM generation) so both paths always agree on the class name.
pub fn hal_impl_class_name(iface: &str, variant: &str) -> String {
    let cap_all = |s: &str| -> String {
        s.split('_')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let mut c = p.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<String>()
    };
    format!("{}{}", cap_all(iface), cap_all(variant))
}

/// Resolve the deterministic module-pair filenames for a NEW HAL
/// implementation: `<iface>_<plat>.{cpp,hpp}` (class `CameraHalRock3c`), with
/// numeric disambiguation when the exact pair already exists on disk so a
/// re-generation never overwrites an unrelated sibling
/// (`camera_hal_imx219.cpp`). Returns
/// `(concrete class name, .cpp filename, .hpp filename)`.
pub fn resolve_hal_impl_names(
    iface: &str,
    plat: &str,
    impl_dir: &std::path::Path,
) -> (String, String, String) {
    let mut token = plat.to_string();
    let mut n = 1usize;
    while impl_dir.join(format!("{iface}_{token}.cpp")).exists()
        || impl_dir.join(format!("{iface}_{token}.hpp")).exists()
    {
        n += 1;
        token = format!("{plat}_{n}");
    }
    (
        hal_impl_class_name(iface, &token),
        format!("{iface}_{token}.cpp"),
        format!("{iface}_{token}.hpp"),
    )
}

/// Semantic-generation variant of [`resolve_hal_impl_names`]: REUSE the base
/// `<iface>_<plat>.{cpp,hpp}` pair when the name is free or only occupied by a
/// pending STUB (the real generation replaces it), and disambiguate with a
/// numeric suffix only when a REAL implementation already sits there — so Fix
/// cleanly replaces scaffold stubs (left by `hal_add_target`/`hal_fill`)
/// instead of piling up `_2` siblings.
pub fn resolve_semantic_hal_impl_names(
    iface: &str,
    plat: &str,
    impl_dir: &std::path::Path,
) -> (String, String, String) {
    let cpp = impl_dir.join(format!("{iface}_{plat}.cpp"));
    let hpp = impl_dir.join(format!("{iface}_{plat}.hpp"));
    // A file is safe to replace when it doesn't exist or carries the pending
    // SPIRE-HAL-STUB sentinel (i.e. it's scaffold, not working code).
    let cpp_replaceable = !cpp.exists() || is_hal_stub_file(&cpp);
    let hpp_replaceable = !hpp.exists() || is_hal_stub_file(&hpp);
    if cpp_replaceable && hpp_replaceable {
        return (
            hal_impl_class_name(iface, plat),
            format!("{iface}_{plat}.cpp"),
            format!("{iface}_{plat}.hpp"),
        );
    }
    resolve_hal_impl_names(iface, plat, impl_dir)
}

/// Default library/hardware-technique hints for the Stage-1 implementation
/// prompt. The UI can override these (`library_hints` arg); this map gives a
/// sane default for the seeded platforms and a generic fallback otherwise.
pub fn hal_platform_library_hints(platform: &str) -> String {
    for (id, hint) in [
        ("rpi5", "libcamera / V4L2 (linux/videodev2.h, libcamera/libcamera.h), mmap frame capture via IOCTLs"),
        ("rock3c", "rknn-toolkit2 (rknn_api.h), MPP (rockchip/mpp_buffer.h, rk_mpi.h), RGA (rga.h / librga)"),
        ("rock5b", "rknn-toolkit2 (rknn_api.h), MPP (rockchip/mpp_buffer.h, rk_mpi.h), RGA (rga.h / librga)"),
        ("imx219", "libcamera camera-sensor API, I2C (linux/i2c-dev.h) register programming"),
        ("a7s", "Linux V4L2 (linux/videodev2.h), mmap streaming, I2C (linux/i2c-dev.h)"),
    ] {
        if id == platform {
            return hint.to_string();
        }
    }
    format!("Use the platform's standard SDK and drivers (see ~/.spire/platforms/{platform}.yaml); prefer POSIX/Linux system APIs where available.")
}

/// Generate a CLEAN concrete declaration header for a HAL module pair — the
/// same deterministic surface as [`generate_hal_module_header`] but WITHOUT the
/// `SPIRE-HAL-STUB` sentinel, `#pragma message` or TODO comments, so the file
/// reads as a real implementation and the coverage analyzer never flags it.
pub fn generate_hal_module_header_clean(
    header_stem: &str,
    impl_class: &str,
    base_class: &str,
    methods: &[HalContractMethod],
    platform: &str,
) -> String {
    let mut h = format!("#include \"hal/api/{header_stem}.hpp\"\n\n");
    h.push_str("namespace hal {\n\n");
    h.push_str(&format!("/// {impl_class} — {platform} implementation of {base_class}.\n"));
    h.push_str(&format!("class {impl_class} : public {base_class} {{\n"));
    h.push_str("public:\n");
    h.push_str(&format!("    {impl_class}() = default;\n"));
    h.push_str(&format!("    ~{impl_class}() override = default;\n\n"));
    for m in methods {
        let ret = m.return_type.trim();
        if m.name.starts_with('~') {
            continue;
        }
        h.push_str(&format!(
            "    {ret} {name}({params}) override;\n\n",
            name = m.name,
            params = m.params
        ));
    }
    h.push_str("private:\n");
    h.push_str("    // Platform state (SDK handles, buffers, locks, config).\n");
    h.push_str("    struct Impl;\n");
    h.push_str("    Impl* impl_ = nullptr;\n");
    h.push_str("};\n\n");
    h.push_str("} // namespace hal\n");
    h
}

/// Generate the **full module pair** (declaration header + definition source)
/// for one HAL interface × platform variant.
///
/// The pair is the atomic unit of an implementation: the header declares the
/// concrete class + private state, the source provides every method body.
/// Returns `(header, source)`. The output of this function is what the
/// linter-style "Fix → Confirm/Reject" viewer shows as two tabs.
pub fn generate_hal_module_pair(
    header_stem: &str,
    base_class: &str,
    impl_class: &str,
    methods: &[HalContractMethod],
    platform: &str,
) -> (String, String) {
    let header = generate_hal_module_header(
        header_stem,
        impl_class,
        base_class,
        methods,
        platform,
    );
    let source = generate_hal_placeholder_source(header_stem, impl_class, methods, platform);
    (header, source)
}

/// Build the Stage-1 reference-implementation prompt for ONE HAL interface on
/// ONE target (the smallest constrained step that defeats LLM drift).
///
/// The prompt carries exactly the context the model needs and nothing more:
///   (a) the validated contract summary (the binding interface),
///   (b) the target's hardware profile from the registry,
///   (c) the "techniques + libraries" hint block for that platform
///       (e.g. rpi5 → libcamera/V4L2; rock3c → rknn/mpp/rga),
///   (d) the per-target out-of-class definition you must implement, and
///   (e) the acceptance gate: the placeholder must compile on this target
///       (`meson compile -C build-<platform> <target>`).
pub fn generate_hal_impl_prompt(
    contract_summary: &str,
    header_stem: &str,
    class_name: &str,
    platform_id: &str,
    platform_name: &str,
    hardware_profile: &str,
    library_hints: &str,
    build_target_name: &str,
) -> String {
    format!(
        r#"Implement one HAL interface for one target.

CONTRACT (binding — do not change signatures):
{contract_summary}

TARGET: {platform_name} ({platform_id})
HARDWARE PROFILE:
{hardware_profile}

TECHNIQUES + LIBRARIES TO USE:
{library_hints}

YOUR TASK:
Write a single .cpp file that provides OUT-OF-CLASS definitions for every
public pure-virtual method of `{class_name}` (from `{header_stem}.hpp`), using
the target's hardware/libraries above. The interface is the contract — the
implementation must match it exactly (names, return types, parameter lists).

RULES:
- One interface, one target, one file. Do NOT touch other interfaces or platforms.
- Do NOT modify `{header_stem}.hpp`.
- Return values for non-void methods on errors: value-initialized objects or a
  documented error value consistent with the contract's docs.
- The file will be compiled against the target's cross toolchain.

GATE:
The implementation is accepted only when it compiles on this target:
  meson compile -C build-{platform_id} {build_target_name}
"#,
        hardware_profile = hardware_profile.trim(),
        library_hints = library_hints.trim(),
    )
}

/// Stage 2 ("add target") Meson wiring: produce the `files(...)` line(s) for a
/// new platform's `hal/` source list inside `hal/meson.build`:
///
///   ```text
///   hal_impl_rock3c_sources = files(
///       'implementations/rock3c/camera_hal_stub.cpp',
///       'implementations/rock3c/h264_encoder_stub.cpp',
///   )
///   ```
///
/// `interface_stems` are the contract header basenames (e.g. `camera_hal`,
/// `h264_encoder`) — one placeholder per interface is generated for the new
/// platform. The caller appends the returned section to `hal/meson.build`.
pub fn hal_meson_var_section(platform: &str, interface_stems: &[String]) -> String {
    let entries: Vec<String> = interface_stems
        .iter()
        .map(|stem| format!("        'implementations/{platform}/{stem}_stub.cpp',"))
        .collect();
    format!(
        "hal_impl_{platform}_sources = files(\n{}\n    )\n",
        entries.join("\n")
    )
}

/// The structural difference between two approved HAL contract versions
/// (Stage 3 — contract change). Used to emit per-target `hal_interface_change`
/// reconcile steps: adapt every stale implementation to the new signature.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HalContractChange {
    /// Methods present in the new contract but absent from the old.
    pub added: Vec<String>,
    /// Methods present in the old contract but absent from the new.
    pub removed: Vec<String>,
    /// Methods whose signature (return type or parameter list) changed:
    /// `(method name, "old → new")`.
    pub changed: Vec<(String, String)>,
}

/// Compare the method sets of two contract summaries. Methods are keyed by
/// name; a same-named method with a different return type or params is
/// `changed`. Ordering is deterministic (sorted) so plans are reproducible.
pub fn diff_hal_contracts(old_summary: &str, new_summary: &str) -> HalContractChange {
    // Flatten (class, methods) → name-keyed methods (first class wins).
    let mut old_map: std::collections::BTreeMap<String, HalContractMethod> =
        std::collections::BTreeMap::new();
    for (_, methods) in parse_hal_contract_summary(old_summary) {
        for m in methods {
            old_map.entry(m.name.clone()).or_insert(m);
        }
    }
    let mut new_map: std::collections::BTreeMap<String, HalContractMethod> =
        std::collections::BTreeMap::new();
    for (_, methods) in parse_hal_contract_summary(new_summary) {
        for m in methods {
            new_map.entry(m.name.clone()).or_insert(m);
        }
    }

    let mut change = HalContractChange::default();
    for (name, new_m) in &new_map {
        match old_map.get(name) {
            None => change.added.push(name.clone()),
            Some(old_m) => {
                if old_m.return_type.trim() != new_m.return_type.trim()
                    || old_m.params.trim() != new_m.params.trim()
                {
                    change.changed.push((
                        name.clone(),
                        format!(
                            "{} {}({}) → {} {}({})",
                            old_m.return_type.trim(),
                            name,
                            old_m.params,
                            new_m.return_type.trim(),
                            name,
                            new_m.params
                        ),
                    ));
                }
            }
        }
    }
    for (name, old_m) in &old_map {
        if !new_map.contains_key(name) {
            change.removed.push(old_m.name.clone());
        }
    }
    change.added.sort();
    change.removed.sort();
    change.changed.sort_by(|a, b| a.0.cmp(&b.0));
    change
}

// ============================================================================
// AST-based HAL implementation coverage (per-function gaps)
// ============================================================================

/// One out-of-class method definition found in a HAL implementation `.cpp`
/// (e.g. `bool CameraHalRpi5::init(const PipelineConfig& cfg) { … }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HalImplMethod {
    pub name: String,
    pub return_type: String,
    pub params: String,
}

/// Per-interface coverage for one platform. `implemented` is true only when
/// every contract pure-virtual method is present with a matching signature.
#[derive(Debug, Clone, Default)]
pub struct HalInterfaceCoverage {
    pub implemented: bool,
    /// True when at least one stem-matching impl file exists for this platform.
    /// Distinguishes "no implementation at all" (needs a scaffolded class)
    /// from "partial implementation" (missing methods can be added).
    pub has_impl: bool,
    /// True when the matching impl file carries the `SPIRE-HAL-STUB` sentinel —
    /// i.e. a generated placeholder rather than a real implementation. The UI
    /// surfaces this as `stub` maturity (distinct from `partial`, which has a
    /// genuine file with missing/drifted methods).
    pub is_stub: bool,
    /// Contract method names with no implementation match (missing override).
    pub missing: Vec<String>,
    /// Full signatures (name, return_type, params) of the missing contract
    /// methods — what an LLM needs to add them without re-reading the header.
    pub missing_sigs: Vec<HalContractMethod>,
    /// Human-readable drift: `method: contract `old` → impl `new``.
    pub drifted: Vec<String>,
}

/// Build a tree-sitter C++ parser (fresh per call — `Parser` is not Sync).
fn cpp_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    let _ = p.set_language(&tree_sitter_cpp::LANGUAGE.into());
    p
}

/// Collect the named children of a node into a Vec (uses an internal cursor).
fn named_children_of<'t>(node: tree_sitter::Node<'t>) -> Vec<tree_sitter::Node<'t>> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        out.push(child);
    }
    out
}

/// Extract the method NAME and PARAMS from a tree-sitter `function_declarator`
/// node using its AST fields (not string parsing):
/// - name: the `declarator` child (identifier / qualified_identifier), last
///   `::`-component only.
/// - params: the `parameters`/`parameter_list` child text, parens stripped.
fn function_declarator_name_params<'t>(
    fd: tree_sitter::Node<'t>,
    content: &str,
) -> (String, String) {
    let name = fd
        .child_by_field_name("declarator")
        .and_then(|d| d.utf8_text(content.as_bytes()).ok())
        .map(|t| {
            let t = t.trim();
            t.rsplit("::").next().unwrap_or(t).trim().to_string()
        })
        .unwrap_or_default();
    let params = fd
        .child_by_field_name("parameters")
        .or_else(|| fd.named_children(&mut fd.walk()).find(|c| c.kind() == "parameter_list"))
        .and_then(|p| p.utf8_text(content.as_bytes()).ok())
        .map(|t| {
            let t = t.trim();
            if t.starts_with('(') && t.ends_with(')') && t.len() >= 2 {
                t[1..t.len() - 1].trim().to_string()
            } else {
                t.to_string()
            }
        })
        .unwrap_or_default();
    (name, params)
}

/// Extract contract methods from a C++ header **using a real tree-sitter-C++ CST**.
///
/// Walks `class_specifier`/`struct_specifier` nodes. For each body, tracks the
/// active access region (`public:`/`private:`/`protected:` — structs default
/// public, classes private) and collects every `field_declaration` whose
/// declarator is a function (a method). A method counts as a contract method
/// only when it is `virtual` **and pure** (`= 0` in its source text) and
/// public — the same gate the regex extractor enforced, but identified by the
/// AST rather than brace-matching.
///
/// Returns `(class name, [methods])` per abstract class, matching the shape
/// `summarize_hal_header` consumes.
pub fn extract_contract_methods_cpp(content: &str) -> Vec<(String, Vec<HalContractMethod>)> {
    use tree_sitter::Node;

    let mut parser = cpp_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut classes: Vec<(String, Vec<HalContractMethod>)> = Vec::new();

    fn walk(node: Node, content: &str, classes: &mut Vec<(String, Vec<HalContractMethod>)>) {
        let kind = node.kind();
        if kind == "class_specifier" || kind == "struct_specifier" {
            let class_name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content.as_bytes()).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "(unnamed)".to_string());
            let struct_kind = kind == "struct_specifier";
            let mut methods: Vec<HalContractMethod> = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                // Active access region: structs default public, classes private.
                let mut is_public = struct_kind;
                for child in named_children_of(body) {
                    let ck = child.kind();
                    if ck == "access_specifier" {
                        let text = child.utf8_text(content.as_bytes()).unwrap_or("");
                        is_public = text.trim_start().starts_with("public");
                        continue;
                    }
                    if ck != "field_declaration" {
                        continue;
                    }
                    // Sibling analysis of the field_declaration children:
                    // return-type node + function_declarator, then trailing `;`.
                    // Note: tree-sitter-cpp does NOT emit a named 'virtual' child;
                    // the keyword is an unnamed token — detect it from the raw
                    // declaration text instead.
                    let text = child.utf8_text(content.as_bytes()).unwrap_or("");
                    let is_virtual = text.contains("virtual");
                    let mut fd: Option<Node> = None;
                    let mut return_type = String::new();
                    for c in named_children_of(child) {
                        let ck = c.kind();
                        if ck == "function_declarator" {
                            fd = Some(c);
                            continue;
                        }
                        // First non-declarator named child = the return type
                        // (`bool`, `std::uint32_t`, …).
                        if return_type.is_empty() {
                            return_type = c.utf8_text(content.as_bytes()).unwrap_or("").trim().to_string();
                        }
                    }
                    let Some(fd) = fd else { continue };
                    // Pure-virtual marker: `= 0` (with any spacing) in the text.
                    let is_pure = text.contains("= 0") || text.contains("=0");
                    if !is_virtual || !is_pure || !is_public {
                        continue;
                    }
                    let (name, params) = function_declarator_name_params(fd, content);
                    if name.is_empty() || name.starts_with('~') {
                        continue;
                    }
                    methods.push(HalContractMethod {
                        name,
                        return_type,
                        params,
                    });
                }
            }
            if !methods.is_empty() {
                classes.push((class_name, methods));
            }
            return;
        }
        for c in named_children_of(node) {
            walk(c, content, classes);
        }
    }

    walk(tree.root_node(), content, &mut classes);
    classes
}

/// Extract class/struct declarations with their base-class names using the
/// **tree-sitter-C++ CST**. For each `class_specifier`/`struct_specifier` we
/// read the `base_class_clause` children (e.g. `: public H264Encoder`) and
/// collect every unqualified base identifier. Returns
/// `(class name, [base class names])`.
///
/// The analyzer uses this to attribute an implementation file to an interface
/// when its class public-inherits the contract class — the same signal the
/// older regex `extract_cpp_hal_facts` produced, but identified by the AST.
pub fn extract_cpp_base_classes(content: &str) -> Vec<(String, Vec<String>)> {
    use tree_sitter::Node;

    let mut parser = cpp_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();

    fn walk(node: Node, content: &str, out: &mut Vec<(String, Vec<String>)>) {
        let kind = node.kind();
        if kind == "class_specifier" || kind == "struct_specifier" {
            let class_name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content.as_bytes()).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "(unnamed)".to_string());
            let mut bases: Vec<String> = Vec::new();
            for c in named_children_of(node) {
                if c.kind() == "base_class_clause" {
                    for b in named_children_of(c) {
                        // `type_identifier` / `qualified_identifier` = the base.
                        if let Some(base) = b.utf8_text(content.as_bytes()).ok() {
                            // Strip any `::` qualifier to the last segment and
                            // drop access keywords if they slipped in.
                            let name = base
                                .trim()
                                .rsplit("::")
                                .next()
                                .unwrap_or(base.trim())
                                .trim()
                                .to_string();
                            if !name.is_empty()
                                && name != "public"
                                && name != "private"
                                && name != "protected"
                                && name != "virtual"
                            {
                                if !bases.contains(&name) {
                                    bases.push(name);
                                }
                            }
                        }
                    }
                }
            }
            if !class_name.starts_with("(unnamed)") {
                out.push((class_name, bases));
            }
            return;
        }
        for c in named_children_of(node) {
            walk(c, content, out);
        }
    }

    walk(tree.root_node(), content, &mut out);
    out
}

/// Extract out-of-class method definitions (`Ret Class::name(params) { … }`)
/// from a C++ `.cpp` using a real tree-sitter-C++ CST.
///
/// Walks `function_definition` nodes whose declarator is a `function_declarator`
/// over a `qualified_identifier` (`Class::name`). Constructors/destructors
/// (`~name`) are skipped. Return type comes from the definition's `type` field.
/// Returns the same `HalImplMethod` shape the regex extractor produced, but the
/// identification of "this is an out-of-class definition" comes from the AST —
/// immune to brace-matching and comment issues.
pub fn extract_cpp_method_definitions_ts(content: &str) -> Vec<HalImplMethod> {
    use tree_sitter::Node;

    let mut parser = cpp_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut out: Vec<HalImplMethod> = Vec::new();

    fn walk(node: Node, content: &str, out: &mut Vec<HalImplMethod>) {
        if node.kind() == "function_definition" {
            let return_type = node
                .child_by_field_name("type")
                .and_then(|t| t.utf8_text(content.as_bytes()).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let declarator = node.child_by_field_name("declarator");
            if let Some(d) = declarator {
                let fd = if d.kind() == "function_declarator" {
                    d
                } else {
                    d.named_children(&mut d.walk())
                        .find(|c| c.kind() == "function_declarator")
                        .unwrap_or(d)
                };
                if fd.kind() == "function_declarator" {
                    // `Class::name` from the AST declarator node. Skip
                    // destructors (`~name`) AND constructors (`Class::Class` —
                    // the last `::` segment equals the class name), which are
                    // not contract methods.
                    let raw = fd
                        .child_by_field_name("declarator")
                        .and_then(|q| q.utf8_text(content.as_bytes()).ok())
                        .unwrap_or("");
                    let is_ctor = {
                        let parts: Vec<&str> = raw.rsplit("::").collect();
                        parts.len() >= 2
                            && !parts[0].trim().is_empty()
                            && parts[0].trim() == parts[1].trim()
                    };
                    let (name, params) = function_declarator_name_params(fd, content);
                    if !name.is_empty() && !name.starts_with('~') && !is_ctor {
                        out.push(HalImplMethod {
                            name,
                            return_type,
                            params,
                        });
                    }
                }
            }
            return;
        }
        for c in named_children_of(node) {
            walk(c, content, out);
        }
    }

    walk(tree.root_node(), content, &mut out);
    out
}

/// True when an implementation file's stem attributes it to an interface stem.
/// Matches when the stem appears as a whole component of the (underscore/dot- or
/// start/end-bounded) file stem — so `mpp_h264_encoder.cpp` implements
/// `h264_encoder`, `camera_hal_v4l2_rkaiq.cpp` implements `camera_hal`, and
/// `camera_hal_imx219.cpp` implements `camera_hal` (not `imx219`).
fn file_implements_stem(file_stem: &str, stem: &str) -> bool {
    if file_stem == stem {
        return true;
    }
    let bytes: Vec<char> = file_stem.chars().collect();
    let needle: Vec<char> = stem.chars().collect();
    if needle.is_empty() || needle.len() > bytes.len() {
        return false;
    }
    for i in 0..=(bytes.len() - needle.len()) {
        if bytes[i..i + needle.len()] == needle[..] {
            let before_ok = i == 0 || bytes[i - 1] == '_' || bytes[i - 1] == '.';
            let after_ok = i + needle.len() == bytes.len()
                || bytes[i + needle.len()] == '_'
                || bytes[i + needle.len()] == '.';
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

/// Normalize a parameter list for signature comparison: strip default values
/// (`int qp = 26` → `int qp`), keep ONLY each parameter's TYPE (drop the
/// parameter NAME, so `int width` in the contract matches `int w` in the impl),
/// trim, and collapse whitespace so formatting/name differences never cause
/// false drift.
pub fn normalize_params(params: &str) -> String {
    params
        .split(',')
        .map(|p| {
            let head = p
                .split_once('=')
                .map(|(head, _)| head.trim())
                .unwrap_or_else(|| p.trim());
            // Split into tokens, drop the trailing identifier (the name) when
            // the head has more than one token (i.e. when it's `Type name`).
            let tokens: Vec<&str> = head.split_whitespace().collect();
            if tokens.len() <= 1 {
                tokens.iter().copied().collect::<Vec<_>>().join(" ")
            } else {
                tokens[..tokens.len() - 1].join(" ")
            }
        })
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Human-readable `Ret name(params)` signature for drift messages.
fn hal_sig(return_type: &str, name: &str, params: &str) -> String {
    let ret = return_type.trim();
    if ret.is_empty() {
        format!("{name}({})", params.trim())
    } else {
        format!("{ret} {name}({})", params.trim())
    }
}

/// Compare one contract interface's method set against a platform's
/// stem-matching implementation files. Files are matched the same way the
/// analyzer attributes them (`<stem>.cpp` / `<stem>_*.cpp`); the comparison
/// itself is AST-level (method name + normalized params + return type), so an
/// implementation whose class name differs from the contract (`ICameraHAL` →
/// `CameraHalRpi5`) still counts as long as its out-of-class methods match.
pub fn hal_interface_coverage(
    contract_methods: &[HalContractMethod],
    stem: &str,
    impl_dir: &std::path::Path,
) -> HalInterfaceCoverage {
    // Contract method set (pure-virtuals, passed as structured data straight
    // from the tree-sitter extractor — no summary-string round-trip that could
    // drop methods).
    let contract_methods: Vec<HalContractMethod> = contract_methods.to_vec();

    // Implementation methods from every file attributed to this interface
    // (stem as a whole underscore/dot-bounded component — so `mpp_h264_encoder`
    // implements `h264_encoder`), parsed with the tree-sitter-C++ CST.
    let mut impl_methods: Vec<HalImplMethod> = Vec::new();
    // A pending stub (`SPIRE-HAL-STUB` sentinel) means the interface is NOT
    // actually implemented here — even if its placeholder method signatures
    // happen to match the contract. The stub must surface as "needs filling".
    let mut stubbed = false;
    if impl_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(impl_dir) {
            for e in entries.flatten() {
                let Some(fname) = e.file_name().to_str().map(|s| s.to_string()) else { continue };
                let Some(file_stem) = fname.split('.').next() else { continue };
                if !file_implements_stem(file_stem, stem) {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(e.path()) {
                    stubbed |= is_hal_stub(&content);
                    impl_methods.extend(extract_cpp_method_definitions_ts(&content));
                }
            }
        }
    }

    let mut cov = HalInterfaceCoverage {
        implemented: false,
        has_impl: !impl_methods.is_empty(),
        is_stub: stubbed,
        missing: Vec::new(),
        missing_sigs: Vec::new(),
        drifted: Vec::new(),
    };
    // A pending stub reports EVERY contract method as missing (with the full
    // signatures the fill planner needs), never "implemented".
    if stubbed {
        cov.missing = contract_methods.iter().map(|m| m.name.clone()).collect();
        cov.missing_sigs = contract_methods.clone();
        return cov;
    }
    for cm in &contract_methods {
        let Some(im) = impl_methods.iter().find(|m| m.name == cm.name) else {
            cov.missing.push(cm.name.clone());
            cov.missing_sigs.push(cm.clone());
            continue;
        };
        let contract_params = normalize_params(&cm.params);
        let impl_params = normalize_params(&im.params);
        let ret_mismatch = !cm.return_type.trim().is_empty()
            && !im.return_type.trim().is_empty()
            && cm.return_type.split_whitespace().collect::<Vec<_>>().join(" ")
                != im.return_type.split_whitespace().collect::<Vec<_>>().join(" ");
        if contract_params != impl_params || ret_mismatch {
            cov.drifted.push(format!(
                "{}: contract `{}` → impl `{}`",
                cm.name,
                hal_sig(&cm.return_type, &cm.name, &cm.params),
                hal_sig(&im.return_type, &im.name, &im.params)
            ));
        }
    }
    cov.implemented = cov.missing.is_empty() && cov.drifted.is_empty();
    cov
}

/// Compute per-platform, per-interface AST coverage for a HAL project.
///
/// Contracts come from `hal/api/*.hpp` (canonical) + `toolkit/src/hal/api/*.hpp`
/// (legacy); per-platform impl dirs come from `hal/implementations/<plat>`
/// (canonical) + `<plat>/hal` (legacy). Returns
/// `platform → interface stem → HalInterfaceCoverage`.
pub fn hal_platform_coverage_map(
    root: &std::path::Path,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, HalInterfaceCoverage>> {
    use std::collections::BTreeMap;

    // 1. Contracts: stem → pure-virtual method set, extracted DIRECTLY from the
    // tree-sitter CST (no summary-string round-trip, which would drop methods
    // with `;`-heavy multi-line params).
    let mut contracts: BTreeMap<String, Vec<HalContractMethod>> = BTreeMap::new();
    let mut header_dirs: Vec<std::path::PathBuf> = Vec::new();
    for dir in [root.join("hal").join("api"), root.join("toolkit").join("src").join("hal").join("api")]
    {
        if dir.is_dir() {
            header_dirs.push(dir);
        }
    }
    for dir in &header_dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let ep = e.path();
                let Some(ext) = ep.extension().and_then(|x| x.to_str()) else { continue };
                if ext != "hpp" && ext != "h" {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&ep) else { continue };
                let classes = extract_contract_methods_cpp(&content);
                // Flatten all abstract classes' methods (a header usually has one).
                let mut methods: Vec<HalContractMethod> = Vec::new();
                for (_, m) in &classes {
                    methods.extend(m.iter().cloned());
                }
                if methods.is_empty() {
                    continue;
                }
                let Some(stem) = ep.file_stem().and_then(|s| s.to_str()) else { continue };
                contracts.entry(stem.to_string()).or_insert(methods);
            }
        }
    }
    if contracts.is_empty() {
        return BTreeMap::new();
    }

    // 2. Platform impl dirs: canonical + legacy.
    let mut platform_dirs: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    let canonical_root = root.join("hal").join("implementations");
    if canonical_root.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&canonical_root) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    if let Some(plat) = e.file_name().to_str().map(|s| s.to_string()) {
                        if !plat.starts_with('.') {
                            platform_dirs.entry(plat).or_insert(e.path());
                        }
                    }
                }
            }
        }
    }
    let skip = ["toolkit", "hal", "build", "build-native", "subprojects", ".git"];
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
            if skip.contains(&name) || name.starts_with('.') {
                continue;
            }
            let legacy = p.join("hal");
            if legacy.is_dir() {
                platform_dirs.entry(name.to_string()).or_insert(legacy);
            }
        }
    }

    // 3. Coverage per platform × interface.
    let mut coverage: BTreeMap<String, BTreeMap<String, HalInterfaceCoverage>> = BTreeMap::new();
    for (plat, dir) in platform_dirs {
        let mut iface_map: BTreeMap<String, HalInterfaceCoverage> = BTreeMap::new();
        for (stem, methods) in &contracts {
            let c = hal_interface_coverage(methods, stem, &dir);
            iface_map.insert(stem.clone(), c);
        }
        coverage.insert(plat, iface_map);
    }
    coverage
}

// ============================================================================
// Phase 2 — structured docs + queryable HAL graph
// ============================================================================

/// One parsed structured-doc tag (e.g. `@brief`, `@id`, `@param cfg`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalDocTag {
    pub name: String,
    pub key: String,   // "@brief" → "brief"; "@param cfg" → "param"
    pub value: String,
}

/// A structured doc comment attached to a contract, datatype, method or field.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalDoc {
    pub target: String,     // declaration name (class, method, field)
    pub kind: String,       // "contract" | "type" | "method" | "field" | "module"
    pub tags: Vec<HalDocTag>,
    pub prose: String,      // non-tag comment text (the LLM guidance)
}

/// Parse Doxygen-style structured comments from a C/C++ header.
///
/// Recognizes `/// line` and `/** … */` blocks immediately preceding a
/// declaration (`class/struct`, `virtual … (…)`, or a data member). Tags are
/// `@name` lines (and `@param <key>` which captures a following key name);
/// everything else is prose. The declaration name is taken from the first
/// following declaration line (`name`, `name(`, `Type name`).
pub fn parse_hal_docs(content: &str) -> Vec<HalDoc> {
    let mut out: Vec<HalDoc> = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    let mut pending: Vec<String> = Vec::new();
    let mut in_block = false;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if in_block {
            if t.starts_with("*/") {
                in_block = false;
            } else {
                let c = t.trim_start_matches('*').trim_start();
                pending.push(c.to_string());
            }
            i += 1;
            continue;
        }
        if t.starts_with("/**") {
            in_block = true;
            let rest = t.trim_start_matches("/**").trim_start();
            if !rest.starts_with('/') {
                pending.push(rest.trim_start_matches('*').trim().to_string());
            }
            i += 1;
            continue;
        }
        if t.starts_with("///") {
            pending.push(t.trim_start_matches('/').trim().to_string());
            i += 1;
            continue;
        }
        // Plain `//` comment (not `///`): also accumulate into the pending doc
        // block as PROSE - the ai-traps HAL headers carry rich untagged prose
        // in `//` form which must reach the viewer and the LLM. Box separator
        // lines of U+2500 (─) are dropped as noise.
        if t.starts_with("//") && !t.starts_with("///") {
            let body = t.trim_start_matches('/').trim();
            let is_sep = body.is_empty()
                || body.contains("\u{2500}\u{2500}\u{2500}");
            if !is_sep {
                pending.push(body.to_string());
            }
            i += 1;
            continue;
        }
        // Non-comment line: if we have a pending doc block and the line is a
        // declaration, attach it.
        let trimmed = t.trim_start_matches("static ").trim_start();
        // Strip any trailing `// …` BEFORE name extraction so inline field
        // comments never leak into the declaration name (they belong in prose).
        let stripped_line = trimmed
            .find("//")
            .map(|idx| trimmed[..idx].trim_end().to_string())
            .unwrap_or_else(|| trimmed.to_string());
        if !pending.is_empty() && !t.is_empty() && !t.starts_with('#') {
            let kind = if trimmed.starts_with("class ") || trimmed.starts_with("struct ") {
                if trimmed.contains("= 0") || trimmed.contains(":") { "contract" } else { "type" }
            } else if trimmed.contains('(') { "method" } else { "field" };
            let name = if trimmed.starts_with("class ") || trimmed.starts_with("struct ") {
                // Class name = the token AFTER `class`/`struct` (not a base class).
                trimmed
                    .split_whitespace()
                    .nth(1)
                    .map(|s| s.trim_end_matches([':', '{']).trim().to_string())
                    .unwrap_or_default()
            } else if let Some(po) = trimmed.find('(') {
                // Method name = the token immediately before `(`.
                trimmed[..po]
                    .split_whitespace()
                    .last()
                    .unwrap_or("")
                    .trim_end_matches('=')
                    .trim()
                    .to_string()
            } else {
                // Field name = the identifier BEFORE `=` (the default/value
                // follows it). Numeric/pointer literal values are never names.
                let head_eq = stripped_line.split('=').next().unwrap_or(&stripped_line);
                head_eq
                    .split(|c: char| c == ' ' || c == '\t' || c == ';' || c == ',' || c == '{')
                    .filter(|s| !s.is_empty())
                    .next_back()
                    .unwrap_or("")
                    .trim_end_matches([';', ','])
                    .to_string()
            };
            let (tags, mut prose) = parse_doc_tags(&pending);
            // Inline/trailing `//` comment on the SAME declaration line is
            // valuable field/method prose (e.g. "// Pointer to pixel data").
            if let Some(cl) = t.find("//") {
                let inline = t[cl + 2..].trim();
                if !inline.is_empty() {
                    if !prose.is_empty() {
                        prose.push(' ');
                    }
                    prose.push_str(inline);
                }
            }
            out.push(HalDoc { target: name, kind: kind.to_string(), tags, prose });
            pending.clear();
        } else if t.is_empty() {
            // blank line: only spaces between doc and decl? keep pending.
        }
        i += 1;
    }
    out
}

fn parse_doc_tags(lines: &[String]) -> (Vec<HalDocTag>, String) {
    let mut tags = Vec::new();
    let mut prose = String::new();
    for l in lines {
        let t = l.trim();
        if t == "@" || t.is_empty() {
            continue;
        }
        if t.starts_with('@') {
            let mut parts = t.splitn(2, ' ');
            let name = parts.next().unwrap_or("").to_string();
            let rest = parts.next().unwrap_or("").trim().to_string();
            let (key, value) = if name == "@param" || name == "@platform-note" {
                // "@param cfg …" → key "param", value "cfg …"
                let mut k = rest.splitn(2, ' ');
                let pk = k.next().unwrap_or("").to_string();
                let pv = k.next().unwrap_or("").to_string();
                (pk, format!("{pv}"))
            } else {
                (name.trim_start_matches('@').to_string(), rest)
            };
            tags.push(HalDocTag { name: name.clone(), key, value });
        } else {
            if !prose.is_empty() {
                prose.push(' ');
            }
            prose.push_str(t);
        }
    }
    (tags, prose)
}

/// Node in the queryable HAL graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalGraphNode {
    pub id: String,
    pub kind: String, // HalModule | HalContract | HalType | HalImpl | HalMethod
    pub name: String,
    pub platform: String,
    pub doc: Option<HalDoc>,
}

/// Edge in the queryable HAL graph: derives / implements / uses / missing / drifted.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalGraphEdge {
    pub kind: String, // derives | implements | uses | missing | drifted
    pub from: String,
    pub to: String,
}

/// Whole-project HAL graph: nodes + edges, built from headers + impl dirs +
/// the AST coverage. Exposed by the `hal_graph` / `hal_query` tool surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalGraph {
    pub nodes: Vec<HalGraphNode>,
    pub edges: Vec<HalGraphEdge>,
}

/// Build a queryable HAL graph for a project root.
///
/// Honors the canonical `hal/api` + `hal/implementations/<plat>` layout (and
/// legacy `toolkit/src/hal/api` + `<plat>/hal`). Node kinds: `HalContract` per
/// header, `HalType` per data header, `HalMethod` per pure-virtual method,
/// `HalImpl` per implementation file. Edges: `derives` (contract → HalModule),
/// `implements` (impl → contract), `missing` (contract → platform with no
/// stem-matching impl), `drifted` (contract → platform when the impl is
/// incomplete).
pub fn hal_graph(root: &std::path::Path) -> HalGraph {
    let mut g = HalGraph::default();
    let header_dirs: Vec<std::path::PathBuf> = [
        root.join("hal").join("api"),
        root.join("hal").join("types"),
        root.join("toolkit").join("src").join("hal").join("api"),
    ]
    .into_iter()
    .filter(|d| d.is_dir())
    .collect();
    let mut impl_dirs: Vec<std::path::PathBuf> = Vec::new();
    let canonical = root.join("hal").join("implementations");
    if canonical.is_dir() {
        if let Ok(es) = std::fs::read_dir(&canonical) {
            for e in es.flatten() {
                if e.path().is_dir() {
                    impl_dirs.push(e.path());
                }
            }
        }
    }
    let cov = hal_platform_coverage_map(root);
    let mut contract_stems: Vec<String> = Vec::new();

    for dir in &header_dirs {
        if let Ok(es) = std::fs::read_dir(dir) {
            for e in es.flatten() {
                let p = e.path();
                let Some(ext) = p.extension().and_then(|x| x.to_str()) else { continue };
                if ext != "hpp" && ext != "h" {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&p) else { continue };
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
                let docs = parse_hal_docs(&content);
                let classes = extract_contract_methods_cpp(&content);
                let _kind = if derives_hal_module(&content) || !classes.is_empty() {
                    "HalContract"
                } else {
                    "HalType"
                };
                // HalModule declaration node if present.
                if derives_hal_module(&content) {
                    for (cname, cbase) in extract_cpp_base_classes(&content) {
                        if cbase.iter().any(|b| b == "HalModule") {
                            let cname_for_id = cname.clone();
                            let cname_str = cname.clone();
                            g.nodes.push(HalGraphNode {
                                id: format!("contract:{}", cname_for_id),
                                kind: "HalContract".into(),
                                name: cname_str,
                                platform: "common".into(),
                                doc: docs.iter().find(|d| d.target == cname).cloned(),
                            });
                            g.edges.push(HalGraphEdge {
                                kind: "derives".into(),
                                from: format!("contract:{}", cname),
                                to: "module:hal::HalModule".into(),
                            });
                            contract_stems.push(stem.clone());
                            for m in classes.iter().flat_map(|(_, ms)| ms) {
                                g.nodes.push(HalGraphNode {
                                    id: format!("method:{}:{}", cname, m.name),
                                    kind: "HalMethod".into(),
                                    name: m.name.clone(),
                                    platform: "common".into(),
                                    doc: docs.iter().find(|d| d.target == m.name).cloned(),
                                });
                            }
                        }
                    }
                } else {
                    // data-only header → HalType node
                    g.nodes.push(HalGraphNode {
                        id: format!("type:{}", stem),
                        kind: "HalType".into(),
                        name: stem,
                        platform: "common".into(),
                        doc: docs.first().cloned(),
                    });
                }
            }
        }
    }
    // HalModule node.
    g.nodes.push(HalGraphNode {
        id: "module:hal::HalModule".into(),
        kind: "HalModule".into(),
        name: "hal::HalModule".into(),
        platform: "common".into(),
        doc: None,
    });
    // Impls + missing/drifted edges from coverage.
    for plat_dir in &impl_dirs {
        let Some(plat) = plat_dir.file_name().and_then(|s| s.to_str()).map(String::from) else { continue };
        let Some(ifaces) = cov.get(&plat) else { continue };
        for (stem, c) in ifaces {
            if c.has_impl {
                g.nodes.push(HalGraphNode {
                    id: format!("impl:{}:{}", plat, stem),
                    kind: "HalImpl".into(),
                    name: stem.clone(),
                    platform: plat.clone(),
                    doc: None,
                });
                g.edges.push(HalGraphEdge {
                    kind: "implements".into(),
                    from: format!("impl:{}:{}", plat, stem),
                    to: format!("contract:{}", stem),
                });
                if !c.implemented {
                    g.edges.push(HalGraphEdge {
                        kind: "drifted".into(),
                        from: format!("contract:{}", stem),
                        to: format!("platform:{}", plat),
                    });
                }
            } else {
                g.edges.push(HalGraphEdge {
                    kind: "missing".into(),
                    from: format!("contract:{}", stem),
                    to: format!("platform:{}", plat),
                });
            }
        }
    }
    g
}

/// Filter a HAL graph by query substrings (node kind / name / platform).
/// Returns matching nodes and the edges that touch them.
pub fn hal_query(graph: &HalGraph, query: &str) -> (Vec<HalGraphNode>, Vec<HalGraphEdge>) {
    let q = query.to_lowercase();
    let nodes: Vec<HalGraphNode> = graph
        .nodes
        .iter()
        .filter(|n| q.is_empty() || n.kind.to_lowercase().contains(&q) || n.name.to_lowercase().contains(&q) || n.platform.to_lowercase().contains(&q))
        .cloned()
        .collect();
    let ids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let edges = graph
        .edges
        .iter()
        .filter(|e| ids.contains(e.from.as_str()) || ids.contains(e.to.as_str()))
        .cloned()
        .collect();
    (nodes, edges)
}

/// Serialize parsed HAL docs into a compact LLM-facing block.
pub fn hal_doc_block(docs: &[HalDoc]) -> String {
    let mut out = String::new();
    for d in docs {
        out.push_str(&format!("--- {} ({}) ---\n", d.target, d.kind));
        for t in &d.tags {
            if t.value.is_empty() {
                out.push_str(&format!("{}{}\n", t.name, if t.key == "param" { " <name>" } else { "" }));
            } else {
                out.push_str(&format!("{} {}\n", t.name, t.value));
            }
        }
        if !d.prose.is_empty() {
            out.push_str(&format!("{}\n", d.prose));
        }
    }
    out
}

// ============================================================================
// Phase 6 — HAL viewer: human-readable documentation + verification
// ============================================================================

/// One documented contract method (signature + attached HalDoc tags/prose).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalMethodDoc {
    pub name: String,
    pub return_type: String,
    pub params: String,
    #[serde(default)]
    pub tags: Vec<HalDocTag>,
    #[serde(default)]
    pub prose: String,
}

/// Extract inline `// …` comments from data-type member declarations
/// (e.g. `int w = 0; // width in pixels`). Used when a field has NO leading
/// doc block - the trailing comment is still valuable field prose.
fn inline_field_docs(content: &str) -> Vec<HalFieldDoc> {
    use std::collections::BTreeMap;
    let mut out: BTreeMap<(String, String), HalFieldDoc> = BTreeMap::new();
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(ci) = t.find("//") {
            let head = t[..ci].trim();
            let comment = t[ci + 2..].trim();
            if head.is_empty() || comment.is_empty() {
                continue;
            }
            // Field name = identifier BEFORE `=` (the default follows it);
            // type = the tokens before the name (e.g. "uint32_t" for "width").
            let before_eq = head.split('=').next().unwrap_or(head).trim();
            let tokens: Vec<&str> = before_eq
                .split([' ', '\t', ';', ','])
                .filter(|s| !s.is_empty())
                .collect();
            let Some(name) = tokens.last() else { continue };
            let name = name.trim_end_matches([';', ',']).to_string();
            if name.is_empty()
                || name.contains('(')
                || name.contains(':')
                || name.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                continue; // skip functions/methods/namespaces/literal values
            }
            let type_name = tokens[..tokens.len() - 1].join(" ");
            out.entry((name.clone(), type_name.clone()))
                .and_modify(|e| {
                    if !e.prose.is_empty() { e.prose.push(' '); }
                    e.prose.push_str(&comment);
                })
                .or_insert(HalFieldDoc {
                    name: name,
                    type_name,
                    tags: Vec::new(),
                    prose: comment.to_string(),
                });
        }
    }
    out.into_iter().map(|(_, f)| f).collect()
}

/// One documented struct field (member-level annotated docs).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalFieldDoc {
    pub name: String,
    #[serde(default)]
    pub type_name: String,
    #[serde(default)]
    pub tags: Vec<HalDocTag>,
    #[serde(default)]
    pub prose: String,
}

/// One core datatype (struct header in `hal/types` or a data-only `hal/api` header).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalTypeDoc {
    pub name: String,
    pub header: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub tags: Vec<HalDocTag>,
    #[serde(default)]
    pub prose: String,
    #[serde(default)]
    pub fields: Vec<HalFieldDoc>,
}

/// Per-platform implementation status for one contract.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalPlatformDoc {
    pub platform: String,
    pub implemented: bool,
    pub has_impl: bool,
    #[serde(default)]
    pub missing: Vec<String>,
    #[serde(default)]
    pub drifted: Vec<String>,
}

/// One contract page for the viewer.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalContractDoc {
    pub stem: String,
    pub class_name: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub tags: Vec<HalDocTag>,
    #[serde(default)]
    pub prose: String,
    pub header: String,
    #[serde(default)]
    pub methods: Vec<HalMethodDoc>,
    #[serde(default)]
    pub uses_types: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<HalPlatformDoc>,
}

/// The full documentation payload: contracts + datatype pages.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalDocReport {
    #[serde(default)]
    pub contracts: Vec<HalContractDoc>,
    #[serde(default)]
    pub types: Vec<HalTypeDoc>,
}

/// One verification issue (severity + path + message + suggested fix).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalIssue {
    pub severity: String, // "error" | "warning" | "info"
    pub title: String,
    pub path: String,
    pub message: String,
    #[serde(rename = "suggested_fix")]
    pub suggested_fix: String,
}

/// One HAL documentation-lint issue (report-only; the LLM writes the fix).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalDocLintIssue {
    pub severity: String, // "error" | "warning" | "info"
    pub title: String,
    pub path: String,
    /// Contract class or method/field symbol this issue belongs to.
    pub symbol: String,
    pub message: String,
    /// Focused LLM prompt that asks for the corrected `/// @…` doc block only.
    #[serde(rename = "fix_prompt")]
    pub fix_prompt: String,
}

/// Per-file lint result: issues + which symbols are clean.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalDocLintFile {
    pub path: String,
    #[serde(default)]
    pub issues: Vec<HalDocLintIssue>,
}

/// Full HAL documentation-lint report (inputs: `hal/api/*.hpp` headers).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalDocLintReport {
    #[serde(default)]
    pub files: Vec<HalDocLintFile>,
}

/// Known HAL doc tags (whitelist): anything else is flagged.
const HAL_DOC_TAGS: &[&str] = &[
    "@brief", "@id", "@param", "@return", "@lifespan", "@ownership", "@zero-copy",
    "@platform-note", "@thread-safety", "@performance", "@error", "@note",
];

/// Lint `hal/api/*.hpp` doc completeness against what the HAL graph actually
/// consumes (`parse_hal_docs` + `extract_contract_methods_cpp`), so a clean
/// lint is exactly equivalent to a well-formed HAL AST.
pub fn hal_doc_lint(root: &std::path::Path) -> HalDocLintReport {
    let mut report = HalDocLintReport::default();
    let api = root.join("hal").join("api");
    let Ok(es) = ::std::fs::read_dir(&api) else { return report };
    for e in es.flatten() {
        let p = e.path();
        let Some(ext) = p.extension().and_then(|x| x.to_str()) else { continue };
        if ext != "hpp" && ext != "h" {
            continue;
        }
        let Ok(content) = ::std::fs::read_to_string(&p) else { continue };
        let rel = p.to_string_lossy().to_string();
        let docs = parse_hal_docs(&content);
        let classes = extract_contract_methods_cpp(&content);
        if classes.is_empty() {
            continue; // data-only headers lint their fields instead (future rule)
        }
        let mut issues: Vec<HalDocLintIssue> = Vec::new();

        // Rule 1: contract class has @brief + @id.
        let cdoc = docs.iter().find(|d| classes.iter().any(|(c, _)| *c == d.target));
        if !cdoc.map(|d| d.tags.iter().any(|t| t.name == "@brief")).unwrap_or(false) {
            issues.push(HalDocLintIssue {
                severity: "error".into(),
                title: "contract missing @brief".into(),
                path: rel.clone(),
                symbol: classes.first().map(|(c, _)| c.clone()).unwrap_or_default(),
                message: "class-level doc block lacks a `@brief`".into(),
                fix_prompt: "Write a one-line `/// @brief` describing this HAL contract.".into(),
            });
        }
        if !cdoc.map(|d| d.tags.iter().any(|t| t.name == "@id")).unwrap_or(false) {
            issues.push(HalDocLintIssue {
                severity: "error".into(),
                title: "contract missing @id".into(),
                path: rel.clone(),
                symbol: classes.first().map(|(c, _)| c.clone()).unwrap_or_default(),
                message: "class-level doc block lacks an `@id` (e.g. hal.camera)".into(),
                fix_prompt: "Write `/// @id <module-id>` (e.g. hal.camera).".into(),
            });
        }

        // Per-method rules 2-4 + any orphan `// prose` before a method (rule 6).
        for (cname, ms) in &classes {
            for m in ms {
                let mdoc = docs.iter().find(|d| d.target == m.name);
                let sym = format!("{}::{}", cname, m.name);
                let has_abrief = mdoc.map(|d| d.tags.iter().any(|t| t.name == "@brief")).unwrap_or(false);
                if !has_abrief {
                    issues.push(HalDocLintIssue {
                        severity: "error".into(),
                        title: "method missing @brief".into(),
                        path: rel.clone(),
                        symbol: sym.clone(),
                        message: format!("{} missing `@brief` in the graph", m.name),
                        fix_prompt: format!(
                            "Write a one-line `/// @brief` for `{}` (returns {}).",
                            m.name,
                            if m.return_type.is_empty() { "void".into() } else { m.return_type.clone() }
                        ),
                    });
                }
                // Params: name list from the signature string.
                let param_names: Vec<String> = m.params
                    .split(',')
                    .filter_map(|part| {
                        // Strip any default (`= <value>`) BEFORE the name, so
                        // `int quality = 85` -> "quality", not "85".
                        let head = part.split('=').next().unwrap_or(part);
                        head.trim().split_whitespace().last()
                            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || c == '_'))
                            .map(|w| w.to_string())
                    })
                    .collect();
                let have = mdoc.map(|d| d.tags.iter().filter(|t| t.name == "@param").collect::<Vec<_>>()).unwrap_or_default();
                for pn in &param_names {
                    if !have.iter().any(|t| t.key == *pn) {
                        issues.push(HalDocLintIssue {
                            severity: "error".into(),
                            title: "method missing @param".into(),
                            path: rel.clone(),
                            symbol: sym.clone(),
                            message: format!("{} parameter `{}` undocumented", m.name, pn),
                            fix_prompt: format!(
                                "Write `/// @param {} <description>` for the `{}` parameter of `{}`.", pn, pn, m.name
                            ),
                        });
                    }
                }
                if !m.return_type.is_empty() && m.return_type != "void" {
                    let has_return = mdoc.map(|d| d.tags.iter().any(|t| t.name == "@return")).unwrap_or(false);
                    if !has_return {
                        issues.push(HalDocLintIssue {
                            severity: "error".into(),
                            title: "method missing @return".into(),
                            path: rel.clone(),
                            symbol: sym.clone(),
                            message: format!("{} returns `{}` but has no `@return`", m.name, m.return_type),
                            fix_prompt: format!(
                                "Write `/// @return <description>` for the `{}` return value of `{}`.",
                                m.return_type, m.name
                            ),
                        });
                    }
                }
            }
        }

        // Rule 5: whitelist tags (scan all lines for `@word`).
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("///") || t.starts_with("//") || t.starts_with("/**") {
                let body = t.trim_start_matches(['/', '*']).trim();
                for word in body.split_whitespace() {
                    if word.starts_with('@') {
                        let tag = word.split(|c: char| c == ' ' || c == '(').next().unwrap_or(word);
                        if !HAL_DOC_TAGS.contains(&tag) {
                            issues.push(HalDocLintIssue {
                                severity: "warning".into(),
                                title: "unknown doc tag".into(),
                                path: rel.clone(),
                                symbol: tag.to_string(),
                                message: format!("unknown tag `{tag}`"),
                                fix_prompt: format!("Replace `{tag}` with a whitelisted HAL tag or remove it."),
                            });
                            break; // one per line is enough
                        }
                    }
                }
            }
        }

        report.files.push(HalDocLintFile { path: rel, issues });
    }
    report
}

/// One C++ syntax error: (line, col) + the tree-sitter node kind + context.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CppSyntaxError {
    pub line: u32,
    pub col: u32,
    pub kind: String,
    pub context: String,
}

/// Result of a structural C++ syntax check (tree-sitter CST).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CppSyntaxReport {
    pub ok: bool,
    #[serde(default)]
    pub errors: Vec<CppSyntaxError>,
}

/// Structural C++ syntax check using the same `tree-sitter-cpp` CST the HAL
/// analyzer consumes. `ok == false` means the parser hit ERROR/MISSING nodes.
/// Structural only — missing `#include`s / ill-typed expressions need a compiler.
pub fn cpp_syntax_check(content: &str) -> CppSyntaxReport {
    let mut parser = cpp_parser();
    let Some(tree) = parser.parse(content, None) else {
        return CppSyntaxReport { ok: false, errors: Vec::new() };
    };
    let root = tree.root_node();
    if !root.has_error() {
        return CppSyntaxReport { ok: true, errors: Vec::new() };
    }
    let mut errors = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            let (row, col) = (node.start_position().row, node.start_position().column);
            let context = content.lines().nth(row as usize).map(|l| l.trim().to_string()).unwrap_or_default();
            errors.push(CppSyntaxError { line: row as u32 + 1, col: col as u32 + 1, kind: node.kind().to_string(), context });
            continue; // don't descend into erroneous nodes
        }
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            stack.push(c);
        }
    }
    CppSyntaxReport { ok: false, errors }
}

/// Strip a ```code fence (optionally tagged `cpp`) from an LLM response so the
/// returned text is a raw file. Shared by the coordinator's `hal/fixPropose`
/// and the build manager's `hal_generate_impl` semantic generation.
pub fn strip_code_fences(text: &str) -> String {
    let t = text.trim();
    if t.starts_with("```") {
        let after = &t[3..];
        let after = after
            .strip_prefix("cpp")
            .map(str::trim_start)
            .unwrap_or_else(|| after.trim_start());
        let pos = after.find("```").unwrap_or(after.len());
        after[..pos].trim().to_string()
    } else {
        t.to_string()
    }
}

/// Build ONE LLM prompt for a whole file's lint issues (auto fix loop input).
pub fn hal_doc_fix_prompt_all(path: &str, issues: &[HalDocLintIssue]) -> String {
    let mut out = String::new();
    out.push_str("Fix the HAL documentation issues in this C++ header.\n");
    out.push_str(&format!("File: {path}\n\n"));
    for (i, issue) in issues.iter().enumerate() {
        out.push_str(&format!("{}. Symbol: {}   Issue: {}   {} \n", i + 1, issue.symbol, issue.title, issue.message));
    }
    out.push_str("Return ONLY the corrected `/// @...` comment block(s) for the symbols listed, ready to paste into the file.");
    out
}

/// Compute the HAL state snapshot for a project root: the single source of
/// truth the inline right-pane card renders (contract state + per-impl states).
pub fn compute_hal_state(root: &std::path::Path) -> HalStateSnapshot {
    let mut snap = HalStateSnapshot::default();
    let lint = hal_doc_lint(root);
    let api = root.join("hal").join("api");

    let mut contract_headers: Vec<(std::path::PathBuf, String)> = Vec::new();
    if let Ok(es) = ::std::fs::read_dir(&api) {
        for e in es.flatten() {
            let p = e.path();
            let Some(ext) = p.extension().and_then(|x| x.to_str()) else { continue };
            if ext != "hpp" && ext != "h" {
                continue;
            }
            let Ok(content) = ::std::fs::read_to_string(&p) else { continue };
            if extract_cpp_base_classes(&content)
                .iter()
                .any(|(_, bs)| bs.iter().any(|b| b == "HalModule"))
            {
                contract_headers.push((p, content));
            }
        }
    }

    if contract_headers.is_empty() {
        snap.contract = HalContractState::NoContract;
        snap.implementations = hal_impl_rows(root);
        return snap;
    }

    // Syntax gate.
    let mut syntax_errors = 0usize;
    for (_, content) in &contract_headers {
        let sr = cpp_syntax_check(content);
        if !sr.ok {
            syntax_errors += sr.errors.len();
        }
    }
    snap.syntax_errors = syntax_errors;
    if syntax_errors > 0 {
        snap.contract = HalContractState::InvalidSyntax { errors: syntax_errors };
        snap.implementations = hal_impl_rows(root);
        return snap;
    }

    // Lint gate (only contract files).
    let mut issues_count = 0usize;
    for (p, _) in &contract_headers {
        let rel = p.to_string_lossy().to_string();
        for f in &lint.files {
            if f.path == rel {
                issues_count += f.issues.len();
            }
        }
    }
    snap.lint_issues = issues_count;
    snap.contract = if issues_count > 0 {
        HalContractState::LintDirty { issues: issues_count }
    } else {
        HalContractState::LintClean
    };
    snap.implementations = hal_impl_rows(root);
    snap
}

/// Per-platform implementation rows for the status card.
fn hal_impl_rows(root: &std::path::Path) -> Vec<HalImplRow> {
    let coverage = hal_platform_coverage_map(root);
    let impl_root = root.join("hal").join("implementations");
    let mut rows = Vec::new();
    for (plat, ifaces) in &coverage {
        let mut row = HalImplRow { platform: plat.clone(), state: HalImplState::Missing, contracts: Vec::new() };
        let mut stub = false;
        if let Ok(fs) = ::std::fs::read_dir(impl_root.join(plat)) {
            for f in fs.flatten() {
                if let Ok(c) = ::std::fs::read_to_string(f.path()) {
                    if c.contains("SPIRE-HAL-STUB") {
                        stub = true;
                        break;
                    }
                }
            }
        }
        let mut missing_all: Vec<String> = Vec::new();
        let mut drifted_all: Vec<String> = Vec::new();
        let mut has_impl_any = false;
        for (stem, cov) in ifaces {
            row.contracts.push(stem.clone());
            if cov.has_impl {
                has_impl_any = true;
            }
            if !cov.implemented {
                for m in &cov.missing {
                    if !missing_all.contains(m) {
                        missing_all.push(m.clone());
                    }
                }
                for d in &cov.drifted {
                    if !drifted_all.contains(d) {
                        drifted_all.push(d.clone());
                    }
                }
            }
        }
        row.state = if stub && (has_impl_any || !missing_all.is_empty()) {
            HalImplState::StubPending
        } else if !has_impl_any && missing_all.is_empty() {
            HalImplState::Missing
        } else if !missing_all.is_empty() {
            HalImplState::Incomplete { missing: missing_all }
        } else if !drifted_all.is_empty() {
            HalImplState::Drifted { drifted: drifted_all }
        } else {
            HalImplState::Complete
        };
        rows.push(row);
    }
    rows.sort_by(|a, b| a.platform.cmp(&b.platform));
    rows
}

/// Single-file lint issues for one HAL header path (whole-file flow).
pub fn hal_doc_lint_file(root: &std::path::Path, path: &str) -> Vec<HalDocLintIssue> {
    hal_doc_lint(root)
        .files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.issues.clone())
        .unwrap_or_default()
}

/// Whole-file rewrite prompt: ask the LLM to return the COMPLETE corrected
/// header (all `/// @…` docs fixed), keeping the code identical.
pub fn hal_doc_fix_prompt_whole(path: &str, content: &str, issues: &[HalDocLintIssue]) -> String {
    let mut out = String::new();
    out.push_str("Fix the HAL documentation lints in this C++ header by rewriting the WHOLE file.\n");
    out.push_str(&format!("File: {path}\n\nIssues:\n"));
    for (i, issue) in issues.iter().enumerate() {
        out.push_str(&format!("{}. [{}] {} — {}\n", i + 1, issue.symbol, issue.title, issue.message));
    }
    out.push_str(&format!("\nCurrent content:\n```cpp\n{content}\n```\n\n"));
    out.push_str("Return ONLY the complete corrected header inside a ```cpp``` block. Keep all code and signatures identical; add/fix only the `/// @...` doc comments.");
    out
}

/// Build the ordered per-file fix plan from a lint report (dirty files only).
pub fn hal_fix_plan_from_lint(lint: &HalDocLintReport) -> HalFixPlan {
    let mut steps = Vec::new();
    for file in &lint.files {
        if file.issues.is_empty() {
            continue;
        }
        steps.push(HalFixPlanStep {
            path: file.path.clone(),
            issues: file.issues.clone(),
            prompt: hal_doc_fix_prompt_all(&file.path, &file.issues),
        });
    }
    HalFixPlan { steps }
}

/// Contract state machine (inline right-pane status + actions).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HalContractState {
    NoContract,
    InvalidSyntax { errors: usize },
    LintDirty { issues: usize },
    #[default]
    LintClean,
    Fixing,
}

/// Per-platform implementation state machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HalImplState {
    #[default]
    Missing,
    StubPending,
    Incomplete { missing: Vec<String> },
    Drifted { drifted: Vec<String> },
    Complete,
    SyntaxError,
}

/// One implementation row for the inline status card.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalImplRow {
    pub platform: String,
    pub state: HalImplState,
    #[serde(default)]
    pub contracts: Vec<String>,
}

/// Full HAL snapshot: contract + per-impl states + lint/syntax detail.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalStateSnapshot {
    pub contract: HalContractState,
    #[serde(default)]
    pub implementations: Vec<HalImplRow>,
    #[serde(default)]
    pub lint_issues: usize,
    #[serde(default)]
    pub syntax_errors: usize,
}

/// One file in the fix plan ("Plan fixes" payload).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalFixPlanStep {
    pub path: String,
    #[serde(default)]
    pub issues: Vec<HalDocLintIssue>,
    pub prompt: String,
}

/// Ordered per-file fix plan from the lint report.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HalFixPlan {
    #[serde(default)]
    pub steps: Vec<HalFixPlanStep>,
}

/// Build a focused LLM prompt that asks the model to emit only the corrected
/// `/// @…` doc block for a given lint issue (member context is included).
pub fn hal_doc_fix_prompt(issue: &HalDocLintIssue) -> String {
    format!(
        "Fix this HAL documentation issue.
Symbol: {}
Issue: {}
Path: {}

{}",
        issue.symbol, issue.message, issue.path, issue.fix_prompt
    )
}

/// Build the HAL documentation report for a project root (viewer input).
pub fn hal_report(root: &std::path::Path) -> HalDocReport {
    use std::collections::BTreeSet;
    let mut report = HalDocReport::default();
    let mut type_names: Vec<String> = Vec::new();
    let mut contract_stems: BTreeSet<String> = BTreeSet::new();
    let coverage = hal_platform_coverage_map(root);
    let scan_dir = root.join("hal").join("api");

    if let Ok(es) = std::fs::read_dir(&scan_dir) {
        for e in es.flatten() {
            let p = e.path();
            let Some(ext) = p.extension().and_then(|x| x.to_str()) else { continue };
            if ext != "hpp" && ext != "h" {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&p) else { continue };
            let classes = extract_contract_methods_cpp(&content);
            let docs = parse_hal_docs(&content);
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };

            if classes.is_empty() {
                // Data-only header → a HalTypeDoc.
                for (cname, _) in extract_cpp_base_classes(&content) {
                    type_names.push(cname.clone());
                    let brief = docs
                        .iter()
                        .find(|d| d.target == cname)
                        .and_then(|d| d.tags.iter().find(|t| t.name == "@brief").map(|t| t.value.clone()))
                        .unwrap_or_default();
                    let type_doc = docs.iter().find(|d| d.target == cname);
                    let tags = type_doc.map(|d| d.tags.clone()).unwrap_or_default();
                    let prose = type_doc.map(|d| d.prose.clone()).unwrap_or_default();
                    let mut fields: Vec<HalFieldDoc> = docs
                        .iter()
                        .filter(|d| d.kind == "field")
                        .map(|d| HalFieldDoc {
                            name: d.target.clone(),
                            type_name: String::new(),
                            tags: d.tags.clone(),
                            prose: d.prose.clone(),
                        })
                        .collect();
                    // Merge inline `//` field comments (no leading doc block).
                    for inline in inline_field_docs(&content) {
                        if let Some(f) = fields.iter_mut().find(|f| f.name == inline.name) {
                            if !inline.prose.is_empty() {
                                if !f.prose.is_empty() { f.prose.push(' '); }
                                f.prose.push_str(&inline.prose);
                            }
                        } else {
                            fields.push(inline);
                        }
                    }
                    report.types.push(HalTypeDoc {
                        name: cname,
                        header: p.to_string_lossy().to_string(),
                        brief,
                        tags,
                        prose,
                        fields,
                    });
                }
                continue;
            }
            contract_stems.insert(stem.clone());

            let bases = extract_cpp_base_classes(&content);
            let Some((class_name, _)) = bases
                .iter()
                .find(|(_, bs)| bs.iter().any(|b| b == "HalModule"))
                .cloned()
            else {
                continue;
            };
            let cdoc = docs.iter().find(|d| d.target == class_name);
            let id = cdoc.and_then(|d| d.tags.iter().find(|t| t.name == "@id").map(|t| t.value.clone())).unwrap_or_default();
            let brief = cdoc.and_then(|d| d.tags.iter().find(|t| t.name == "@brief").map(|t| t.value.clone())).unwrap_or_default();

            let mut methods = Vec::new();
            for (_, ms) in &classes {
                for m in ms {
                    let mdoc = docs.iter().find(|d| d.target == m.name);
                    methods.push(HalMethodDoc {
                        name: m.name.clone(),
                        return_type: m.return_type.clone(),
                        params: m.params.clone(),
                        tags: mdoc.map(|d| d.tags.clone()).unwrap_or_default(),
                        prose: mdoc.map(|d| d.prose.clone()).unwrap_or_default(),
                    });
                }
            }

            let uses_types = type_names
                .iter()
                .filter(|t| content.contains(t.as_str()))
                .cloned()
                .collect();

            let mut platforms = Vec::new();
            for (plat, ifaces) in &coverage {
                if let Some(cov) = ifaces.get(&stem) {
                    platforms.push(HalPlatformDoc {
                        platform: plat.clone(),
                        implemented: cov.implemented,
                        has_impl: cov.has_impl,
                        missing: cov.missing.clone(),
                        drifted: cov.drifted.clone(),
                    });
                }
            }

            let ctags = cdoc.map(|d| d.tags.clone()).unwrap_or_default();
            let cprose = cdoc.map(|d| d.prose.clone()).unwrap_or_default();
            report.contracts.push(HalContractDoc {
                stem,
                class_name,
                id,
                brief,
                tags: ctags,
                prose: cprose,
                header: p.to_string_lossy().to_string(),
                methods,
                uses_types,
                platforms,
            });
        }
    }
    report
}

/// Verify a HAL project and return issues (documentation correctness + coverage).
pub fn hal_verify(root: &std::path::Path) -> Vec<HalIssue> {
    use std::collections::BTreeSet;
    let mut issues = Vec::new();
    let mut contract_stems: BTreeSet<String> = BTreeSet::new();
    let api = root.join("hal").join("api");

    if let Ok(es) = std::fs::read_dir(&api) {
        for e in es.flatten() {
            let p = e.path();
            let Some(ext) = p.extension().and_then(|x| x.to_str()) else { continue };
            if ext != "hpp" && ext != "h" {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&p) else { continue };
            let classes = extract_contract_methods_cpp(&content);
            if classes.is_empty() {
                continue; // data-only; not verified here
            }
            let rel = p.to_string_lossy().to_string();
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
            contract_stems.insert(stem);

            if !content.contains("namespace hal") {
                issues.push(HalIssue {
                    severity: "error".into(),
                    title: "contract not in namespace hal".into(),
                    path: rel.clone(),
                    message: "contract must declare `namespace hal`".into(),
                    suggested_fix: "rename the namespace to `hal`".into(),
                });
            }
            let bases = extract_cpp_base_classes(&content);
            if !bases.iter().any(|(_, bs)| bs.iter().any(|b| b == "HalModule")) {
                issues.push(HalIssue {
                    severity: "error".into(),
                    title: "contract does not derive hal::HalModule".into(),
                    path: rel.clone(),
                    message: "contract class must derive hal::HalModule".into(),
                    suggested_fix: "derive `hal::HalModule` publicly".into(),
                });
            }
            let docs = parse_hal_docs(&content);
            let cdoc = docs.iter().find(|d| classes.iter().any(|(c, _)| *c == d.target));
            let mid = cdoc.and_then(|d| d.tags.iter().find(|t| t.name == "@id").map(|t| t.value.clone()));
            if mid.is_none() {
                issues.push(HalIssue {
                    severity: "warning".into(),
                    title: "contract missing @id doc".into(),
                    path: rel.clone(),
                    message: "add `/// @id <module-id>`".into(),
                    suggested_fix: "add an @id tag and matching id() override".into(),
                });
            } else if let Some(mid) = &mid {
                let expected = format!("return \"{mid}\"");
                if !content.contains(&expected) {
                    issues.push(HalIssue {
                        severity: "warning".into(),
                        title: "@id and id() disagree".into(),
                        path: rel.clone(),
                        message: format!("doc @id `{mid}` must match `id()` override"),
                        suggested_fix: format!("make `id()` return `{mid}`"),
                    });
                }
            }
            for (cname, ms) in &classes {
                for m in ms {
                    let brief = docs
                        .iter()
                        .find(|d| d.target == m.name)
                        .map(|d| d.tags.iter().any(|t| t.name == "@brief"))
                        .unwrap_or(false);
                    if !brief {
                        issues.push(HalIssue {
                            severity: "info".into(),
                            title: "method missing @brief".into(),
                            path: rel.clone(),
                            message: format!("{cname}::{}({}) — add a one-line @brief", m.name, m.params),
                            suggested_fix: format!("document `{}` with `/// @brief …`", m.name),
                        });
                    }
                }
            }
        }
    }

    let coverage = hal_platform_coverage_map(root);
    for (plat, ifaces) in &coverage {
        for (stem, cov) in ifaces {
            if cov.implemented {
                continue;
            }
            let mut msg = format!("{}: {} missing [{}]", plat, stem, cov.missing.join(", "));
            if !cov.drifted.is_empty() {
                msg.push_str(&format!(" + drifted [{}]", cov.drifted.join(", ")));
            }
            issues.push(HalIssue {
                severity: "warning".into(),
                title: "incomplete HAL implementation".into(),
                path: format!("hal/implementations/{plat}"),
                message: msg,
                suggested_fix: "fill missing/drifted methods or mark SPIRE-HAL-STUB pending".into(),
            });
        }
    }

    let impl_root = root.join("hal").join("implementations");
    if let Ok(ps) = std::fs::read_dir(&impl_root) {
        for p in ps.flatten() {
            if !p.path().is_dir() {
                continue;
            }
            if let Ok(fs) = std::fs::read_dir(p.path()) {
                for f in fs.flatten() {
                    let fname = f.file_name().to_string_lossy().to_string();
                    if !fname.ends_with(".cpp") {
                        continue;
                    }
                    let file_stem = fname.trim_end_matches(".cpp").to_string();
                    if !contract_stems.iter().any(|s| file_implements_stem(&file_stem, s)) {
                        issues.push(HalIssue {
                            severity: "info".into(),
                            title: "orphan HAL implementation".into(),
                            path: f.path().to_string_lossy().to_string(),
                            message: format!("{fname}: stem does not match any contract"),
                            suggested_fix: "rename to `<contract>_<impl>.cpp` or remove".into(),
                        });
                    }
                }
            }
        }
    }
    issues
}

/// Build a RICH Stage-1 implementation prompt: contract docs + datatype docs +
/// the platform capability matrix (from RAG). Extends
/// [`generate_hal_impl_prompt`] with the structured knowledge the LLM needs
/// to write a platform-specific implementation.
pub fn generate_hal_impl_prompt_rich(
    contract_summary: &str,
    header_stem: &str,
    class_name: &str,
    platform_id: &str,
    platform_name: &str,
    hardware_profile: &str,
    library_hints: &str,
    build_target_name: &str,
    contract_docs: &str,
    datatype_docs: &str,
    capability_matrix: &str,
) -> String {
    let base = generate_hal_impl_prompt(
        contract_summary, header_stem, class_name,
        platform_id, platform_name, hardware_profile, library_hints, build_target_name,
    );
    let mut r = String::new();
    r.push_str("## CONTRACT STRUCTURED DOCS\n");
    r.push_str(if contract_docs.trim().is_empty() { "(none)\n" } else { contract_docs });
    r.push_str("\n## DATATYPE DOCS\n");
    r.push_str(if datatype_docs.trim().is_empty() { "(none)\n" } else { datatype_docs });
    r.push_str(&format!("\n## PLATFORM CAPABILITY MATRIX ({platform_name})\n"));
    r.push_str(if capability_matrix.trim().is_empty() { "(unavailable)\n" } else { capability_matrix });
    r.push_str(&format!("\n{base}"));
    r
}

/// Render parsed HAL docs (`parse_hal_docs`) into the compact prompt block the
/// implementation prompt consumes — one line per declaration, tags indented.
pub fn hal_docs_to_prompt_text(docs: &[HalDoc]) -> String {
    let mut out = String::new();
    for d in docs {
        out.push_str(&format!("[{}/{}] {}\n", d.kind, d.target, d.prose.trim()));
        for t in &d.tags {
            let bare = t.name.trim_start_matches('@');
            if t.key.is_empty() || t.key == bare {
                out.push_str(&format!("    {}: {}\n", t.name, t.value));
            } else {
                out.push_str(&format!("    {} {}: {}\n", t.name, t.key, t.value));
            }
        }
    }
    out.trim_end().to_string()
}

/// Parse every `hal/types/*.hpp` header and render its structured docs as the
/// DATATYPE DOCS block for the implementation prompt (empty when none exist).
pub fn datatype_docs_to_prompt_text(root: &std::path::Path) -> String {
    let dir = root.join("hal").join("types");
    let mut out = String::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            let Some(ext) = p.extension().and_then(|x| x.to_str()) else { continue };
            if ext != "hpp" && ext != "h" {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&p) else { continue };
            let text = hal_docs_to_prompt_text(&parse_hal_docs(&content));
            if text.is_empty() {
                continue;
            }
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                out.push_str(&format!("## {name}\n{text}\n"));
            }
        }
    }
    out.trim_end().to_string()
}

/// Build the SEMANTIC module-pair implementation prompt for ONE HAL interface
/// on ONE target (the smallest constrained step that defeats LLM drift).
///
/// Unlike the legacy [`generate_hal_impl_prompt`] — which asked for out-of-class
/// definitions of the abstract CONTRACT class — this targets a concrete module
/// pair: the deterministic declaration header (already written by
/// [`generate_hal_module_header_clean`]) plus the `.cpp` the LLM must produce.
/// The header text is embedded so the model implements exactly that class
/// surface (`Impl` PIMPL state + ctor/dtor + every `override`), and the
/// structured contract/datatype docs + RAG capability matrix are attached when
/// available.
pub fn generate_hal_impl_prompt_pair(
    contract_summary: &str,
    header_stem: &str,
    impl_class: &str,
    base_class: &str,
    platform_id: &str,
    platform_name: &str,
    hardware_profile: &str,
    library_hints: &str,
    build_target_name: &str,
    impl_header: &str,
    contract_docs: &str,
    datatype_docs: &str,
    capability_matrix: &str,
) -> String {
    let mut r = String::new();
    r.push_str("## CONTRACT STRUCTURED DOCS\n");
    r.push_str(if contract_docs.trim().is_empty() { "(none)\n" } else { contract_docs });
    r.push_str("\n## DATATYPE DOCS\n");
    r.push_str(if datatype_docs.trim().is_empty() { "(none)\n" } else { datatype_docs });
    r.push_str(&format!("\n## PLATFORM CAPABILITY MATRIX ({platform_name})\n"));
    r.push_str(if capability_matrix.trim().is_empty() { "(unavailable)\n" } else { capability_matrix });
    r.push_str(&format!(
        r#"Implement the .cpp definition file for ONE HAL module pair.

CONTRACT (binding — do not change signatures):
{contract_summary}

TARGET: {platform_name} ({platform_id})
HARDWARE PROFILE:
{hardware_profile}

TECHNIQUES + LIBRARIES TO USE:
{library_hints}

IMPLEMENTATION HEADER (already written — implement against THIS file exactly):
{impl_header}

YOUR TASK:
Write a single .cpp file that provides OUT-OF-CLASS definitions for the module
pair declared in the header above (`{impl_class} : public {base_class}`):
  1. the `{impl_class}::Impl` struct definition (private state: SDK handles,
     buffers, locks, config),
  2. the constructor/destructor (allocate/free `impl_`),
  3. a real (non-TODO) body for every `override` method,
using the target's hardware/libraries above and matching the contract's names,
return types and parameter lists EXACTLY.

RULES:
- One interface, one target, one file. Do NOT touch other interfaces or platforms.
- Do NOT modify `{header_stem}.hpp` or the impl header.
- Do NOT emit `SPIRE-HAL-STUB` or `#pragma message` — this is a real implementation.
- Non-void methods return a real value: value-initialized or a documented error
  value consistent with the contract's docs.
- Return ONLY the .cpp source code — no markdown fences, no prose, no file path.

GATE:
The implementation is accepted only when it compiles on this target:
  meson compile -C build-{platform_id} {build_target_name}
"#,
        contract_summary = contract_summary.trim(),
        hardware_profile = hardware_profile.trim(),
        library_hints = library_hints.trim(),
        impl_header = impl_header.trim(),
        platform_name = platform_name.trim(),
        platform_id = platform_id.trim(),
        impl_class = impl_class.trim(),
        base_class = base_class.trim(),
        header_stem = header_stem.trim(),
        build_target_name = build_target_name.trim(),
    ));
    r
}

/// Upsert a platform's real implementation `.cpp` files into the
/// `hal_impl_<plat>_sources` list in `hal/meson.build`.
///
/// When the variable already exists its `files(...)` list is rewritten to
/// include the new files, dropping legacy `_stub.cpp` entries whose interface
/// stem is superseded by one of the new implementations (the real pair
/// replaces the placeholder). When absent, a fresh section is appended.
/// Idempotent — an already-listed file is never duplicated.
pub fn hal_meson_upsert_sources(
    meson_content: &str,
    platform: &str,
    interface_stems: &[String],
    cpp_file_names: &[String],
) -> String {
    let section = || {
        format!(
            "hal_impl_{platform}_sources = files(\n{}\n    )\n",
            cpp_file_names
                .iter()
                .map(|f| format!("        'implementations/{platform}/{f}',"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let var = format!("hal_impl_{platform}_sources");
    let Some(start) = meson_content.find(&format!("{var} = files(")) else {
        return format!("{meson_content}{}", section());
    };
    let Some(end_rel) = meson_content[start..].find(')') else {
        return format!("{meson_content}{}", section());
    };
    let end = start + end_rel;
    let block = &meson_content[start..end];
    // Extract every quoted path inside `files(...)` (single- or multi-line).
    let mut existing: Vec<String> = Vec::new();
    let mut rest = block;
    while let Some(s) = rest.find('\'') {
        let after = &rest[s + 1..];
        if let Some(e) = after.find('\'') {
            existing.push(after[..e].to_string());
            rest = &after[e + 1..];
        } else {
            break;
        }
    }
    // Drop legacy stub entries superseded by a new real file.
    let mut merged: Vec<String> = existing
        .into_iter()
        .filter(|entry| {
            let Some(basename) = entry.rsplit('/').next().map(String::from) else {
                return true;
            };
            let Some(stub_stem) = basename.strip_suffix("_stub.cpp") else {
                return true;
            };
            !interface_stems.iter().any(|iface| file_implements_stem(stub_stem, iface))
        })
        .collect();
    for f in cpp_file_names {
        let entry = format!("implementations/{platform}/{f}");
        if !merged.contains(&entry) {
            merged.push(entry);
        }
    }
    merged.sort();
    let entries: Vec<String> = merged
        .iter()
        .map(|e| format!("        '{e}',"))
        .collect();
    let replacement = format!("{var} = files(\n{}\n    )", entries.join("\n"));
    format!("{}{}{}", &meson_content[..start], replacement, &meson_content[end + 1..])
}

/// Remove every stem-matching file for an interface that still carries the
/// `SPIRE-HAL-STUB` sentinel (legacy `<iface>_stub.cpp` placeholders or older
/// generated single-file stubs) after a REAL implementation pair is written —
/// otherwise the coverage analyzer ORs the stub flag across all matching files
/// and keeps reporting the interface as "needs filling". Returns the removed
/// filenames.
pub fn remove_stale_hal_stubs(impl_dir: &std::path::Path, iface: &str) -> Vec<String> {
    let mut removed = Vec::new();
    if let Ok(entries) = std::fs::read_dir(impl_dir) {
        for e in entries.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            let Some(file_stem) = fname.split('.').next().map(String::from) else { continue };
            if !file_implements_stem(&file_stem, iface) {
                continue;
            }
            if is_hal_stub_file(&e.path()) && std::fs::remove_file(e.path()).is_ok() {
                removed.push(fname);
            }
        }
    }
    removed
}

/// Flatten a `hal_platform_coverage_map` result into per-platform incomplete
/// interface names (for the UI status row) and per-interface platform lists
/// (backward-compatible `missing` shape used by the missing-impl queue).
pub fn flatten_hal_coverage(
    coverage: &std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, HalInterfaceCoverage>,
    >,
) -> (
    std::collections::BTreeMap<String, Vec<String>>, // platform → incomplete interfaces
    std::collections::BTreeMap<String, Vec<String>>, // interface → platforms missing it
) {
    let mut by_platform: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut by_interface: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (plat, ifaces) in coverage {
        for (iface, cov) in ifaces {
            if !cov.implemented {
                by_platform.entry(plat.clone()).or_default().push(iface.clone());
                by_interface.entry(iface.clone()).or_default().push(plat.clone());
            }
        }
    }
    (by_platform, by_interface)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C++ header method extractor must surface a HAL contract's abstract
    /// class as class + method nodes with `child` edges, correct access
    /// regions, return types, and the pure-virtual (`= 0`) markers in the
    /// signature — the AST-graph source of truth the HAL contract tooling
    /// ("add target" placeholders, contract-change diffing) reads from.
    #[test]
    fn cpp_header_method_extractor_captures_abstract_class_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("camera_hal.hpp");
        std::fs::write(
            &path,
            r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
private:
    int fd_ = -1;
};
"#,
        )
        .unwrap();

        let result = parse_cpp_source_file_std(&path).unwrap();

        // Class node present.
        let klass = result
            .nodes
            .iter()
            .find(|n| n.node_type == "class" && n.name.as_deref() == Some("CameraHAL"))
            .expect("CameraHAL class node");
        let class_idx = result
            .nodes
            .iter()
            .position(|n| std::ptr::eq(n, klass))
            .unwrap();

        // Methods: destructor + the two pure-virtual interface methods.
        let methods: Vec<&AstNodeData> = result
            .nodes
            .iter()
            .filter(|n| n.node_type == "method")
            .collect();
        let names: Vec<&str> = methods
            .iter()
            .filter_map(|m| m.name.as_deref())
            .collect();
        assert!(
            names.contains(&"~CameraHAL"),
            "expected destructor method, got: {names:?}"
        );
        assert!(names.contains(&"start"), "methods: {names:?}");
        assert!(names.contains(&"capture"), "methods: {names:?}");
        // The private member `int fd_ = -1;` has no parens → NOT a method.
        assert!(!names.contains(&"fd_"), "fd_ must not be a method: {names:?}");

        // Pure-virtual markers preserved in the signature.
        let start = methods
            .iter()
            .find(|m| m.name.as_deref() == Some("start"))
            .unwrap();
        let sig = start.signature.as_deref().unwrap_or("");
        assert!(
            sig.contains("= 0") || sig.contains("=0"),
            "start() must carry the pure-virtual marker, got: {sig}"
        );

        // Return types.
        let capture = methods
            .iter()
            .find(|m| m.name.as_deref() == Some("capture"))
            .unwrap();
        assert_eq!(
            capture.return_type.as_deref(),
            Some("std::uint32_t"),
            "capture return type"
        );

        // Access regions: `start`/`capture` after `public:` are public.
        assert!(start.is_public, "start() must be public");
        assert!(capture.is_public, "capture() must be public");

        // child edges from the class to each method.
        let child_edges: Vec<&AstEdgeData> = result
            .edges
            .iter()
            .filter(|e| e.edge_type == "child" && e.from_index == class_idx)
            .collect();
        assert_eq!(
            child_edges.len(),
            methods.len(),
            "every method must be a child of CameraHAL"
        );
    }

    /// Contract-first (Stage 0) validation: `summarize_hal_header` must accept
    /// an abstract-class contract and reject a header with no pure-virtual
    /// public methods.
    #[test]
    fn summarize_hal_header_validates_abstract_class_contract() {
        let ok = r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#;
        let summary = summarize_hal_header(ok).expect("valid abstract contract");
        assert!(summary.contains("CameraHAL"), "summary: {summary}");
        assert!(summary.contains("start"), "summary: {summary}");
        assert!(summary.contains("capture"), "summary: {summary}");
        assert!(summary.contains("= 0"), "summary must expose pure-virtual: {summary}");

        // A header with only concrete methods (no `= 0`) is not a contract.
        let bad = r#"#pragma once
class NotAbstract {
public:
    void do_thing();
};
"#;
        assert!(
            summarize_hal_header(bad).is_err(),
            "non-abstract header must be rejected"
        );
    }

    /// Stage 2 foundation ("add target"): `parse_hal_contract_summary` +
    /// `generate_hal_placeholder_source` must turn the contract summary into
    /// per-method out-of-class override stubs with a TODO + value return.
    #[test]
    fn hal_contract_summary_produces_placeholder_sources() {
        let ok = r#"#pragma once
#include <cstdint>

class CameraHAL {
public:
    virtual ~CameraHAL() = default;
    virtual bool start() = 0;
    virtual std::uint32_t capture(int timeout_ms) = 0;
};
"#;
        let summary = summarize_hal_header(ok).unwrap();
        let classes = parse_hal_contract_summary(&summary);
        assert_eq!(classes.len(), 1, "one class: {classes:?}");
        let (class_name, methods) = &classes[0];
        assert_eq!(class_name, "CameraHAL");
        let names: Vec<&str> = methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"start"), "methods: {names:?}");
        assert!(names.contains(&"capture"), "methods: {names:?}");

        let src = generate_hal_placeholder_source("camera_hal", class_name, methods, "rpi5");
        assert!(src.contains("#include \"camera_hal.hpp\""));
        assert!(src.contains("bool CameraHAL::start()"), "src:\n{src}");
        assert!(src.contains("std::uint32_t CameraHAL::capture(int timeout_ms)"), "src:\n{src}");
        assert!(src.contains("/* TODO: implement for rpi5 */"), "src:\n{src}");
        // Non-void methods return a value-initialized object.
        assert!(src.contains("return {};"), "src:\n{src}");
    }

    /// Stage 2 foundation (add-target wiring): `hal_meson_var_section` must
    /// emit one `files(...)` entry per interface for the new platform's
    /// `hal/meson.build` source list.
    #[test]
    fn hal_meson_var_section_lists_one_stub_per_interface() {
        let section = hal_meson_var_section(
            "rock3c",
            &["camera_hal".to_string(), "h264_encoder".to_string()],
        );
        assert!(
            section.contains("hal_impl_rock3c_sources = files("),
            "vars section: {section}"
        );
        assert!(
            section.contains("'implementations/rock3c/camera_hal_stub.cpp'"),
            "camera_hal stub: {section}"
        );
        assert!(
            section.contains("'implementations/rock3c/h264_encoder_stub.cpp'"),
            "h264 stub: {section}"
        );
        // Deterministic ordering (no dedupe surprises for a single platform).
        let camera = section.find("camera_hal_stub").unwrap();
        let h264 = section.find("h264_encoder_stub").unwrap();
        assert!(camera < h264, "interface order preserved: {section}");
    }

    /// Stage 1 foundation (reference implementation): `generate_hal_impl_prompt`
    /// must embed the contract, the target's hardware profile + library hints,
    /// and the per-target build gate — the exact constrained context that keeps
    /// the LLM on one interface, one target, one file.
    #[test]
    fn hal_impl_prompt_embeds_contract_hardware_and_gate() {
        let summary = "CameraHAL: bool start() = 0; std::uint32_t capture(int timeout_ms) = 0";
        let prompt = generate_hal_impl_prompt(
            summary,
            "camera_hal",
            "CameraHAL",
            "rpi5",
            "Raspberry Pi 5",
            "BCM2712 (Cortex-A76), RK 6.1 kernel, libcamera stack",
            "libcamera, V4L2, media-ctl, Raspberry Pi Camera Module 3 (IMX708)",
            "ai-trap-rpi5",
        );
        assert!(prompt.contains(summary), "must embed the contract summary");
        assert!(
            prompt.contains("Raspberry Pi 5 (rpi5)"),
            "must identify the target: {prompt}"
        );
        assert!(
            prompt.contains("libcamera") && prompt.contains("IMX708"),
            "must embed the library hints: {prompt}"
        );
        assert!(
            prompt.contains("meson compile -C build-rpi5 ai-trap-rpi5"),
            "must embed the per-target build gate: {prompt}"
        );
        assert!(
            prompt.contains("camera_hal.hpp"),
            "must reference the contract header: {prompt}"
        );
    }

    /// Stage 3 foundation (contract change): `diff_hal_contracts` must detect
    /// added / removed / signature-changed methods so stale per-platform
    /// implementations can be flagged for reconcile.
    #[test]
    fn hal_contract_diff_detects_signature_changes() {
        let old_summary =
            "CameraHAL: bool start() = 0; std::uint32_t capture(int timeout_ms) = 0";
        let new_summary = "CameraHAL: bool start() = 0; bool teardown() = 0; \
                           std::uint32_t capture(int timeout_ms, int mode) = 0";
        let change = diff_hal_contracts(old_summary, new_summary);
        assert_eq!(change.added, vec!["teardown".to_string()], "{change:?}");
        assert!(change.removed.is_empty(), "{change:?}");
        // capture changed its parameter list → flagged as changed.
        assert!(
            change.changed.iter().any(|(name, _)| name == "capture"),
            "capture must be flagged changed: {change:?}"
        );
        assert!(
            !change.changed.iter().any(|(name, _)| name == "start"),
            "unchanged start must not be flagged: {change:?}"
        );

        // Removed method.
        let removed = diff_hal_contracts(
            "CameraHAL: bool start() = 0; bool teardown() = 0",
            "CameraHAL: bool start() = 0",
        );
        assert_eq!(removed.removed, vec!["teardown".to_string()], "{removed:?}");
    }

    /// The out-of-class definition extractor must capture `Ret Class::method(params) { … }`
    /// while ignoring constructors/destructors and commented-out prototypes.
    #[test]
    fn extract_cpp_method_definitions_parses_out_of_class_bodies() {
        let src = r#"// bool CameraHalRpi5::init(const PipelineConfig& cfg) { return false; }
namespace ct {
bool CameraHalRpi5::init(const PipelineConfig& cfg) {
    return true;
}
void CameraHalRpi5::release_frames() { }
CameraHalRpi5::CameraHalRpi5() : impl_(nullptr) {}
CameraHalRpi5::~CameraHalRpi5() { shutdown(); }
std::vector<uint8_t> Rpi5JpegEncoder::encode_crop(int src_dma_fd,
                                                  uint32_t src_w,
                                                  int quality = 85) {
    return {};
}
}
"#;
        let methods = extract_cpp_method_definitions_ts(src);
        let names: Vec<&str> = methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"init"), "names: {names:?}");
        assert!(names.contains(&"release_frames"), "names: {names:?}");
        assert!(names.contains(&"encode_crop"), "names: {names:?}");
        // Constructors/destructors are NOT contract methods.
        assert!(!names.contains(&"CameraHalRpi5"), "ctor must be skipped: {names:?}");
        assert!(!names.contains(&"~CameraHalRpi5"), "dtor must be skipped: {names:?}");
        // Commented prototype must NOT match (comments stripped first).
        assert_eq!(names.iter().filter(|n| **n == "init").count(), 1, "no comment match: {names:?}");

        let encode = methods.iter().find(|m| m.name == "encode_crop").unwrap();
        assert_eq!(encode.return_type, "std::vector<uint8_t>");
        // Multi-line params normalized: defaults stripped, names dropped
        // (signature comparison is type-based), whitespace collapsed.
        assert_eq!(normalize_params(&encode.params), "int, uint32_t, int");
    }

    /// `hal_interface_coverage` compares the contract's pure-virtual method set
    /// against a platform's stem-matching impl files (AST-level, class-name
    /// agnostic) and reports missing + drifted functions.
    #[test]
    fn hal_interface_coverage_reports_missing_and_drifted_functions() {
        let contract = "ICameraHAL: bool init(const PipelineConfig& cfg) = 0; \
                        bool acquire_frames(FrameBuffer& full, FrameBuffer& medium, FrameBuffer& lores) = 0; \
                        void release_frames() = 0; \
                        void shutdown() = 0";
        let dir = tempfile::tempdir().unwrap();

        // Complete impl (different class name, all methods present).
        std::fs::write(
            dir.path().join("camera_hal_rpi5.cpp"),
            r#"namespace ct {
bool CameraHalRpi5::init(const PipelineConfig& cfg) { return true; }
bool CameraHalRpi5::acquire_frames(FrameBuffer& full, FrameBuffer& medium,
                                   FrameBuffer& lores) { return true; }
void CameraHalRpi5::release_frames() { }
void CameraHalRpi5::shutdown() { }
}
"#,
        )
        .unwrap();
        let classes = parse_hal_contract_summary(contract);
        let mut contract_methods: Vec<HalContractMethod> = Vec::new();
        for (_, m) in &classes {
            contract_methods.extend(m.iter().cloned());
        }
        let contract_methods = contract_methods;
        let complete = hal_interface_coverage(&contract_methods, "camera_hal", dir.path());
        assert!(complete.implemented, "all methods present: {complete:?}");
        assert!(complete.missing.is_empty(), "missing: {:?}", complete.missing);

        // Drifted impl: wrong return type and a drifted param.
        std::fs::write(
            dir.path().join("camera_hal_imx219.cpp"),
            r#"namespace ct {
bool CameraHALR3::init(const PipelineConfig& cfg) { return true; }
bool CameraHALR3::acquire_frames(FrameBuffer& full, FrameBuffer& medium) { return true; }
void CameraHALR3::release_frames(int extra) { }
void CameraHALR3::shutdown() { }
}
"#,
        )
        .unwrap();
        let drifted = hal_interface_coverage(&contract_methods, "camera_hal", dir.path());
        assert!(!drifted.implemented, "param/arity drift must fail: {drifted:?}");
        // acquire_frames has 2 params vs contract 3 → drifted, not missing.
        assert!(!drifted.missing.contains(&"acquire_frames".to_string()), "drift not missing: {drifted:?}");
        assert!(drifted.drifted.iter().any(|d| d.contains("acquire_frames")), "drifted: {:?}", drifted.drifted);
        // release_frames has an extra param → drifted.
        assert!(drifted.drifted.iter().any(|d| d.contains("release_frames")), "drifted: {:?}", drifted.drifted);

        // Missing impl file → every contract method missing.
        let missing = hal_interface_coverage(&contract_methods, "h264_encoder", dir.path());
        assert!(!missing.implemented);
        assert_eq!(missing.missing.len(), 4, "no h264 impl: {missing:?}");
    }

    /// Real-world validation: run the full AST coverage on the ai-traps
    /// project (canonical HAL layout). gated by SPIRE_AI_TRAPS_INTEGRATION.
    #[test]
    fn hal_platform_coverage_real_ai_traps() {
        let Ok(root) = std::env::var("SPIRE_AI_TRAPS_INTEGRATION") else {
            eprintln!("skipped: set SPIRE_AI_TRAPS_INTEGRATION=/abs/path/ai-traps");
            return;
        };
        let root = std::path::Path::new(&root);
        assert!(root.join("hal/api/camera_hal.hpp").exists(), "ai-traps hal/api missing");

        // Debug: extract the out-of-class methods per file + the CONTRACT
        // methods per header, so a trivially-"implemented" interface (zero
        // contract methods) is visible.
        for plat in ["rpi5", "rock3c"] {
            let dir = root.join("hal/implementations").join(plat);
            eprintln!("PLATFORM DIR {plat}: {}", dir.display());
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    let content = std::fs::read_to_string(e.path()).unwrap_or_default();
                    let methods = extract_cpp_method_definitions_ts(&content);
                    let names: Vec<&str> = methods.iter().map(|m| m.name.as_str()).collect();
                    eprintln!("  {plat} FILE {name}: {names:?}");
                }
            }
        }
        if let Ok(api) = std::fs::read_dir(root.join("hal/api")) {
            for e in api.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let content = std::fs::read_to_string(e.path()).unwrap_or_default();
                let classes = extract_contract_methods_cpp(&content);
                eprintln!("CONTRACT {name}: {classes:?}");
                // Also dump the SUMMARY + its parse, so a summary→parse
                // round-trip loss (0 methods) is visible.
                let summary = summarize_hal_header(&content);
                let parsed = summary.as_deref().map(parse_hal_contract_summary);
                eprintln!("SUMMARY {name}: ok={} parsed={parsed:?}", summary.is_ok());
            }
        }

        let coverage = hal_platform_coverage_map(root);
        eprintln!("COVERAGE MAP: {coverage:#?}");

        // Expected: rpi5 implements camera_hal, h264_encoder, jpeg_encoder
        // (7 methods); missing classifier_hal (6) + inference_hal (6) +
        // video_scaler (2) = 14 missing, 0 drifted.
        let rpi5 = coverage.get("rpi5").expect("rpi5 coverage");
        let rpi5_missing: usize = rpi5.values().map(|c| c.missing.len()).sum();
        let rpi5_drifted: usize = rpi5.values().map(|c| c.drifted.len()).sum();
        assert_eq!(rpi5_missing, 14, "rpi5 missing must be 14: {rpi5:#?}");
        assert_eq!(rpi5_drifted, 0, "rpi5 drifted must be 0: {rpi5:#?}");

        // rock3c implements camera_hal, video_scaler, h264_encoder (via
        // mpp_h264_encoder) and inference_hal (rknn/python/subprocess — but
        // these impls define only init/detect/shutdown, NOT the getters
        // last_inference_us/input_width/input_height). True gaps:
        // classifier_hal (6) + inference_hal getters (3) + jpeg_encoder (1)
        // = 10 missing, 0 drifted.
        let rock3c = coverage.get("rock3c").expect("rock3c coverage");
        let rock3c_missing: usize = rock3c.values().map(|c| c.missing.len()).sum();
        let rock3c_drifted: usize = rock3c.values().map(|c| c.drifted.len()).sum();
        assert_eq!(rock3c_missing, 10, "rock3c missing must be 10: {rock3c:#?}");
        assert_eq!(rock3c_drifted, 0, "rock3c drifted must be 0: {rock3c:#?}");
    }

    /// Phase 2: structured docs are parsed into tags+prose, and a project's
    /// HAL graph can be built and queried (contracts, types, impls, missing).
    #[test]
    fn hal_docs_and_graph_query() {
        let source = r#"namespace hal {
/// @brief   Hardware camera capture contract.
/// @id      hal.camera
/// The pipeline serializes calls per module instance.
struct ICameraHAL : hal::HalModule {
    const char* id() const override { return "hal.camera"; }

    /// @brief Acquire one frame.
    /// @param timeout_ms Wait in ms.
    /// @return true on success.
    virtual bool capture(int timeout_ms) = 0;
};
/// @brief Shared frame buffer descriptor.
struct FrameBuffer { uint32_t width = 0; };
}"#;
        let docs = parse_hal_docs(source);
        let camera = docs.iter().find(|d| d.target == "ICameraHAL");
        assert!(camera.is_some(), "contract doc attached: {docs:?}");
        let c = camera.unwrap();
        assert_eq!(c.kind, "contract");
        assert!(
            c.tags.iter().any(|t| t.name == "@id" && t.value == "hal.camera"),
            "id tag parsed: {:?}", c.tags
        );
        assert!(c.tags.iter().any(|t| t.name == "@brief"), "brief parsed");
        assert!(!c.prose.is_empty(), "prose kept");
        let cap = docs.iter().find(|d| d.target == "capture");
        assert!(cap.is_some(), "method doc attached: {docs:?}");
        assert_eq!(cap.unwrap().kind, "method");
        let fb = docs.iter().find(|d| d.target == "FrameBuffer");
        assert!(fb.is_some(), "type doc attached: {docs:?}");

        // Build the graph on a temp project and query it.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/types")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::write(root.join("hal/api/camera_hal.hpp"), source).unwrap();
        std::fs::write(
            root.join("hal/api/h264_encoder.hpp"),
            "namespace hal { struct H264Encoder : hal::HalModule { virtual int init(int w) = 0; }; }\n",
        )
        .unwrap();
        std::fs::write(root.join("hal/types/frame_buffer.hpp"), "namespace hal { struct FrameBuffer {}; }\n")
            .unwrap();
        std::fs::write(
            root.join("hal/implementations/rpi5/camera_hal_imx219.cpp"),
            "bool Rpi5Cam::capture(int timeout_ms) { return true; }\n",
        )
        .unwrap();

        let graph = hal_graph(root);
        assert!(
            graph.nodes.iter().any(|n| n.kind == "HalModule"),
            "HalModule node: {:?}", graph.nodes
        );
        assert!(
            graph.nodes.iter().any(|n| n.kind == "HalContract" && n.name == "ICameraHAL"),
            "contract node: {:?}", graph.nodes
        );
        assert!(
            graph.nodes.iter().any(|n| n.kind == "HalType" && n.name == "frame_buffer"),
            "type node: {:?}", graph.nodes
        );
        assert!(
            graph.nodes.iter().any(|n| n.kind == "HalImpl" && n.platform == "rpi5"),
            "impl node: {:?}", graph.nodes
        );
        assert!(
            graph.edges.iter().any(|e| e.kind == "derives"),
            "derives edge"
        );
        assert!(
            graph.edges.iter().any(|e| e.kind == "missing"),
            "h264 missing edge (rpi5 has only camera): {:?}", graph.edges
        );

        let (nodes, _edges) = hal_query(&graph, "rpi5");
        assert!(!nodes.is_empty(), "platform query matches impl");
        let (nodes2, _) = hal_query(&graph, "HalType");
        assert!(nodes2.iter().any(|n| n.name == "frame_buffer"), "kind query: {nodes2:?}");
    }

    /// Phase 6: `hal_report` builds the documentation payload (contracts +
    /// datatypes + per-platform status) and `hal_verify` surfaces issues
    /// (namespace, HalModule, @id↔id(), coverage, orphans).
    #[test]
    fn hal_report_and_verify() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            r#"namespace hal {
// Camera capture contract.
// Acquires frames from the sensor.
/// @id hal.camera
struct ICameraHAL : hal::HalModule {
    const char* id() const override { return "hal.camera"; }

    /// @brief Initialise the camera.
    virtual bool init() = 0;
};
}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("hal/api/frame_buffer.hpp"),
            "namespace hal {
/// @brief Shared frame buffer.
struct FrameBuffer { int w = 0; // width in pixels
 int dma_fd = -1; // zero-copy handle (dma-buf)
};
}",
        ).unwrap();
        std::fs::write(
            root.join("hal/api/frame_buffer.hpp"),
            "namespace hal {\n/// @brief Shared frame buffer.\nstruct FrameBuffer { int w = 0; };\n}",
        ).unwrap();
        std::fs::write(
            root.join("hal/implementations/rpi5/camera_hal_rpi5.cpp"),
            "bool Rpi5Cam::init() { return true; }\n",
        ).unwrap();

        let report = hal_report(root);
        assert_eq!(report.contracts.len(), 1, "contracts: {report:#?}");
        // Plain `//` prose now lands in the contract doc prose.
        let cam = report.contracts.iter().find(|c| c.stem == "camera_hal").expect("camera");
        assert!(!cam.prose.is_empty(), "plain // prose must be captured: {cam:#?}");
        assert!(cam.tags.iter().any(|t| t.name == "@id"), "id tag kept: {cam:#?}");
        // Inline/trailing field comments land on the datatype's fields.
        // Untagged `//` prose in the data-type header is captured as type prose.
        let fb = report.types.iter().find(|t| t.name == "FrameBuffer").expect("FrameBuffer");
        assert!(!fb.brief.is_empty(), "type brief populated: {fb:#?}");
        let c = &report.contracts[0];
        assert_eq!(c.id, "hal.camera");
        assert_eq!(c.class_name, "ICameraHAL");
        assert_eq!(c.methods.len(), 1);
        assert_eq!(c.methods[0].name, "init");
        assert!(
            c.platforms.iter().any(|p| p.platform == "rpi5" && p.implemented),
            "rpi5 implemented: {c:#?}"
        );
        assert!(
            report.types.iter().any(|t| t.name == "FrameBuffer"),
            "types: {report:#?}"
        );

        let issues = hal_verify(root);
        assert!(
            issues.is_empty(),
            "expected no issues on clean project: {issues:#?}"
        );

        // Break something: remove the id() override → @id↔id() issue fires.
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            r#"namespace hal {
struct ICameraHAL : hal::HalModule {
    virtual bool init() = 0;
};
}"#,
        )
        .unwrap();
        let issues = hal_verify(root);
        assert!(
            issues.iter().any(|i| i.title == "contract missing @id doc"),
            "missing @id: {issues:#?}"
        );

        // Add an orphan impl with a stem matching no contract.
        std::fs::write(
            root.join("hal/implementations/rpi5/audio_hal_rpi5.cpp"),
            "bool Rpi5Audio::init() { return true; }\n",
        ).unwrap();
        let issues = hal_verify(root);
        assert!(
            issues.iter().any(|i| i.title == "orphan HAL implementation"),
            "orphan: {issues:#?}"
        );
    }

    /// Step 1: `cpp_syntax_check` validates a header structurally (tree-sitter
    /// CST), and `hal_doc_fix_prompt_all` carries every issue for the LLM.
    #[test]
    fn cpp_syntax_check_and_fix_prompt() {
        let ok_content = "namespace hal { struct A : HalModule { virtual void f() = 0; }; }";
        let r = cpp_syntax_check(ok_content);
        assert!(r.ok, "clean header must pass: {r:?}");

        let bad = "namespace hal { struct A { virtual void f() = 0; ";
        let r = cpp_syntax_check(bad);
        assert!(!r.ok, "unclosed brace must fail");
        assert!(!r.errors.is_empty(), "must report a location");

        let issues = vec![HalDocLintIssue {
            severity: "error".into(),
            title: "method missing @brief".into(),
            path: "hal/api/camera_hal.hpp".into(),
            symbol: "ICameraHAL::start".into(),
            message: "no @brief".into(),
            fix_prompt: "write one".into(),
        }];
        let prompt = hal_doc_fix_prompt_all("hal/api/camera_hal.hpp", &issues);
        assert!(prompt.contains("hal/api/camera_hal.hpp"));
        assert!(prompt.contains("ICameraHAL::start"));
        assert!(prompt.contains("method missing @brief"));
    }

    /// Phase 7: `hal_doc_lint` flags missing @brief/@param/@return per method and
    /// unknown tags, with an LLM-ready `fix_prompt` per issue.
    #[test]
    fn hal_doc_lint_catches_missing_docs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            r#"namespace hal {
/// @brief Camera capture contract.
/// @id hal.camera
struct ICameraHAL : hal::HalModule {
    const char* id() const override { return "hal.camera"; }
    /// @brief Stub prose (no params, has return).
    virtual bool start(const char* device, int width) = 0;
};
}"#,
        )
        .unwrap();

        let report = hal_doc_lint(root);
        let file = report.files.first().expect("one file");
        let titles: Vec<&str> = file.issues.iter().map(|i| i.title.as_str()).collect();
        // start() has no @param docs for device/width and no @return.
        assert!(
            titles.iter().any(|t| *t == "method missing @param"),
            "missing @param issue: {titles:?}"
        );
        assert!(
            titles.iter().any(|t| *t == "method missing @return"),
            "missing @return issue: {titles:?}"
        );
        // Every issue carries an LLM fix prompt.
        assert!(
            file.issues.iter().all(|i| !i.fix_prompt.is_empty()),
            "fix_prompt must be populated"
        );
    }

    /// Phase 1 discoverability: a header whose class derives `hal::HalModule`
    /// is a Contract even without pure-virtual methods visible to the old
    /// heuristic (and a plain abstract class WITHOUT the base stays a legacy
    /// Contract for pre-migration projects).
    #[test]
    fn classify_hal_header_hal_module_rule() {
        let tmp = tempfile::tempdir().unwrap();

        // New identity: contract derives HalModule.
        let module = tmp.path().join("camera_hal.hpp");
        std::fs::write(
            &module,
            r#"namespace hal {
class HalModule { public: virtual ~HalModule() = default; virtual const char* id() const = 0; };
struct ICameraHAL : hal::HalModule {
    const char* id() const override { return "hal.camera"; }
    virtual bool init() = 0;
};
}"#,
        )
        .unwrap();
        assert_eq!(
            classify_hal_header(&module),
            HalHeaderKind::Contract,
            "deriving HalModule ⇒ Contract"
        );

        // Legacy: abstract class with pure-virtual but no HalModule base still
        // classifies so pre-migration `ct`-namespace projects keep working.
        let legacy = tmp.path().join("legacy_hal.hpp");
        std::fs::write(
            &legacy,
            "class LegacyCam { public: virtual bool start() = 0; };",
        )
        .unwrap();
        assert_eq!(
            classify_hal_header(&legacy),
            HalHeaderKind::Contract,
            "pure-virtual fallback must persist for legacy projects"
        );

        // Datatype: structs, no pure-virtual, no HalModule base ⇒ DataOnly.
        let data = tmp.path().join("frame_buffer.hpp");
        std::fs::write(&data, "namespace hal { struct FrameBuffer { int w = 0; int h = 0; }; }")
            .unwrap();
        assert_eq!(
            classify_hal_header(&data),
            HalHeaderKind::DataOnly,
            "pure data header must stay DataOnly"
        );
    }

    /// A `SPIRE-HAL-STUB` sentinel file must report the interface as NOT
    /// implemented (every contract method missing for the fill queue), even
    /// when the placeholder method signatures happen to match the contract.
    #[test]
    fn hal_stub_sentinel_marks_placeholder_as_unimplemented() {
        let dir = tempfile::tempdir().unwrap();

        // Placeholder generated by `generate_hal_placeholder_source` — carries
        // the sentinel + a `#pragma message` and has signature-identical
        // methods.
        let methods = vec![
            HalContractMethod { name: "start".into(), return_type: "bool".into(), params: "".into() },
            HalContractMethod { name: "capture".into(), return_type: "std::uint32_t".into(), params: "int timeout_ms".into() },
        ];
        let src = generate_hal_placeholder_source("camera_hal", "CameraHAL", &methods, "a7s");
        assert!(
            src.contains("SPIRE-HAL-STUB") && src.contains("#pragma message("),
            "stub must carry sentinel + pragma: {src}"
        );
        assert!(is_hal_stub(&src), "sentinel detectable");
        std::fs::write(dir.path().join("camera_hal_stub.cpp"), &src).unwrap();

        // The stub must NOT count as implemented even though its method
        // signatures match the contract exactly.
        let cov = hal_interface_coverage(&methods, "camera_hal", dir.path());
        assert!(!cov.implemented, "stub must not be implemented: {cov:?}");
        assert!(cov.has_impl, "stub exists → has_impl false would wrongly mean no class to fill");
        assert_eq!(cov.missing.len(), 2, "every contract method must be missing: {cov:?}");
        assert_eq!(cov.missing_sigs.len(), 2, "fill planner needs full signatures: {cov:?}");
        assert!(cov.drifted.is_empty(), "no drift for a pending stub: {cov:?}");

        // A real implementation without the sentinel still counts as implemented.
        std::fs::remove_file(dir.path().join("camera_hal_stub.cpp")).unwrap();
        std::fs::write(
            dir.path().join("camera_hal_a7s.cpp"),
            "bool CameraHalA7s::start() { return true; }\nstd::uint32_t CameraHalA7s::capture(int timeout_ms) { return 0; }\n",
        )
        .unwrap();
        let real = hal_interface_coverage(&methods, "camera_hal", dir.path());
        assert!(real.implemented, "real impl must be implemented: {real:?}");
    }

    /// End-to-end: `hal_platform_coverage_map` + `flatten_hal_coverage` compute
    /// per-platform incomplete interfaces and the missing-queue shape.
    #[test]
    fn hal_platform_coverage_map_flattens_per_platform_gaps() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rock3c")).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            "class ICameraHAL {\npublic:\n    virtual bool init() = 0;\n    virtual void shutdown() = 0;\n};\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/api/h264_encoder.hpp"),
            "class H264Encoder {\npublic:\n    virtual ~H264Encoder() = default;\n    virtual int init(int width, int height, int qp = 26) = 0;\n};\n",
        )
        .unwrap();
        // rpi5: camera + h264 complete; rock3c: only camera.
        std::fs::write(
            root.join("hal/implementations/rpi5/camera_hal_rpi5.cpp"),
            "bool Rpi5Cam::init() { return true; }\nvoid Rpi5Cam::shutdown() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/implementations/rpi5/h264_encoder_rpi5.cpp"),
            "int Rpi5H264::init(int width, int height, int qp) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/implementations/rock3c/camera_hal_ov5647.cpp"),
            "bool RockCam::init() { return true; }\nvoid RockCam::shutdown() {}\n",
        )
        .unwrap();

        let coverage = hal_platform_coverage_map(root);
        let (by_platform, by_interface) = flatten_hal_coverage(&coverage);

        // rpi5 is fully covered (both interfaces implemented).
        assert!(
            !by_platform.contains_key("rpi5"),
            "rpi5 must be complete: {by_platform:?}"
        );
        // rock3c is missing h264_encoder.
        assert_eq!(
            by_platform.get("rock3c").map(|v| v.as_slice()),
            Some(&["h264_encoder".to_string()][..]),
            "rock3c gaps: {by_platform:?}"
        );
        assert_eq!(
            by_interface.get("h264_encoder").map(|v| v.as_slice()),
            Some(&["rock3c".to_string()][..]),
            "h264 missing queue: {by_interface:?}"
        );
    }

    // ── Semantic module-pair generation helpers ─────────────────────────

    #[test]
    fn module_pair_names_agree_with_fill_scaffolding() {
        assert_eq!(hal_impl_class_name("camera_hal", "rock3c"), "CameraHalRock3c");
        assert_eq!(hal_impl_class_name("video_scaler", "rpi5"), "VideoScalerRpi5");
        assert_eq!(hal_impl_class_name("mpp_h264_encoder", "imx219"), "MppH264EncoderImx219");

        let dir = tempfile::tempdir().unwrap();
        let impl_dir = dir.path().join("hal").join("implementations").join("rock3c");
        std::fs::create_dir_all(&impl_dir).unwrap();
        let (class, cpp, hpp) = resolve_hal_impl_names("camera_hal", "rock3c", &impl_dir);
        assert_eq!(class, "CameraHalRock3c");
        assert_eq!(cpp, "camera_hal_rock3c.cpp");
        assert_eq!(hpp, "camera_hal_rock3c.hpp");
        // Existing sibling (imx219) never collides; existing pair disambiguates
        // with a numeric suffix instead of overwriting.
        std::fs::write(impl_dir.join("camera_hal_rock3c.cpp"), "// x").unwrap();
        let (class2, cpp2, _) = resolve_hal_impl_names("camera_hal", "rock3c", &impl_dir);
        assert_eq!(class2, "CameraHalRock3c2");
        assert_eq!(cpp2, "camera_hal_rock3c_2.cpp");
    }

    #[test]
    fn semantic_impl_names_reuse_stub_pair_but_never_real_impl() {
        let dir = tempfile::tempdir().unwrap();
        let impl_dir = dir.path().join("hal").join("implementations").join("rpi5");
        std::fs::create_dir_all(&impl_dir).unwrap();

        // No files → base pair.
        let (class, cpp, hpp) = resolve_semantic_hal_impl_names("classifier_hal", "rpi5", &impl_dir);
        assert_eq!(class, "ClassifierHalRpi5");
        assert_eq!(cpp, "classifier_hal_rpi5.cpp");
        assert_eq!(hpp, "classifier_hal_rpi5.hpp");

        // A pending STUB pair (SPIRE-HAL-STUB sentinel) is REPLACED, not
        // disambiguated — semantic generation reuses the name.
        std::fs::write(impl_dir.join("classifier_hal_rpi5.cpp"), format!("// {SPIRE_HAL_STUB_SENTINEL}\n")).unwrap();
        std::fs::write(impl_dir.join("classifier_hal_rpi5.hpp"), format!("// {SPIRE_HAL_STUB_SENTINEL}\n")).unwrap();
        let (class2, cpp2, _) = resolve_semantic_hal_impl_names("classifier_hal", "rpi5", &impl_dir);
        assert_eq!(class2, "ClassifierHalRpi5", "stub pair must be reused, not suffixed");
        assert_eq!(cpp2, "classifier_hal_rpi5.cpp");

        // A REAL implementation occupies the name → disambiguate, never
        // overwrite working code.
        std::fs::write(impl_dir.join("classifier_hal_rpi5.cpp"), "bool ClassifierHalRpi5::init(const std::string&, float) { return true; }\n").unwrap();
        let (class3, cpp3, _) = resolve_semantic_hal_impl_names("classifier_hal", "rpi5", &impl_dir);
        assert_eq!(class3, "ClassifierHalRpi52");
        assert_eq!(cpp3, "classifier_hal_rpi5_2.cpp");
    }

    #[test]
    fn clean_module_header_has_no_stub_sentinel() {
        let methods = vec![
            HalContractMethod { name: "start".into(), return_type: "bool".into(), params: String::new() },
            HalContractMethod { name: "capture".into(), return_type: "std::uint32_t".into(), params: "int timeout_ms".into() },
        ];
        let h = generate_hal_module_header_clean("camera_hal", "CameraHalRock3c", "CameraHAL", &methods, "rock3c");
        assert!(!h.contains(SPIRE_HAL_STUB_SENTINEL), "clean header: {h}");
        assert!(!h.contains("#pragma message"), "clean header: {h}");
        assert!(h.contains("class CameraHalRock3c : public CameraHAL {"), "clean header: {h}");
        assert!(h.contains("bool start() override;"), "clean header: {h}");
        assert!(h.contains("std::uint32_t capture(int timeout_ms) override;"), "clean header: {h}");
        assert!(h.contains("struct Impl;"), "PIMPL state slot: {h}");
        assert!(h.contains("#include \"hal/api/camera_hal.hpp\""), "clean header: {h}");
    }

    #[test]
    fn meson_upsert_replaces_stub_entries_and_appends_when_absent() {
        // Existing variable: the stub for the same interface is replaced by
        // the real file; an unrelated interface's stub is preserved.
        let meson = "hal_impl_rock3c_sources = files(\n        'implementations/rock3c/camera_hal_stub.cpp',\n        'implementations/rock3c/h264_encoder_stub.cpp',\n    )\n";
        let updated = hal_meson_upsert_sources(
            meson, "rock3c",
            &["camera_hal".to_string()],
            &["camera_hal_rock3c.cpp".to_string()],
        );
        assert!(updated.contains("'implementations/rock3c/camera_hal_rock3c.cpp'"), "updated: {updated}");
        assert!(!updated.contains("camera_hal_stub.cpp"), "stub must be dropped: {updated}");
        assert!(updated.contains("h264_encoder_stub.cpp"), "unrelated stub kept: {updated}");
        // Idempotent: running again changes nothing.
        let updated2 = hal_meson_upsert_sources(
            &updated, "rock3c",
            &["camera_hal".to_string()],
            &["camera_hal_rock3c.cpp".to_string()],
        );
        assert_eq!(updated, updated2, "upsert must be idempotent");
        // Absent variable → a fresh section is appended.
        let fresh = hal_meson_upsert_sources(
            "project('x', 'cpp')\n", "rpi5",
            &["camera_hal".to_string()],
            &["camera_hal_rpi5.cpp".to_string()],
        );
        assert!(fresh.contains("hal_impl_rpi5_sources = files("), "fresh: {fresh}");
        assert!(fresh.contains("'implementations/rpi5/camera_hal_rpi5.cpp'"), "fresh: {fresh}");
    }

    #[test]
    fn semantic_impl_prompt_targets_module_pair() {
        let methods = vec![
            HalContractMethod { name: "start".into(), return_type: "bool".into(), params: String::new() },
        ];
        let header = generate_hal_module_header_clean("camera_hal", "CameraHalRpi5", "CameraHAL", &methods, "rpi5");
        let prompt = generate_hal_impl_prompt_pair(
            "CameraHAL: bool start() = 0", "camera_hal", "CameraHalRpi5", "CameraHAL",
            "rpi5", "Raspberry Pi 5", "cpu_family: aarch64\ncpu: armv8", "libcamera / V4L2",
            "camera_hal-rpi5", &header,
            "@brief Starts the camera.", "(none)", "",
        );
        assert!(prompt.contains("CameraHalRpi5 : public CameraHAL"), "pair target: {prompt}");
        assert!(prompt.contains("CameraHalRpi5::Impl"), "PIMPL state: {prompt}");
        assert!(prompt.contains("class CameraHalRpi5 : public CameraHAL"), "header embedded: {prompt}");
        assert!(prompt.contains("meson compile -C build-rpi5 camera_hal-rpi5"), "gate: {prompt}");
        assert!(prompt.contains("@brief Starts the camera."), "contract docs: {prompt}");
        assert!(
            prompt.contains("Do NOT emit `SPIRE-HAL-STUB`"),
            "prompt must forbid the sentinel: {prompt}"
        );
    }

    #[test]
    fn strip_code_fences_removes_cpp_block() {
        assert_eq!(strip_code_fences("```cpp\nint main() {}\n```"), "int main() {}");
        assert_eq!(strip_code_fences("```\nint main() {}\n```"), "int main() {}");
        assert_eq!(strip_code_fences("int main() {}"), "int main() {}");
    }
}
