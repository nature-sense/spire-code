// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Swift build module — owns SwiftPM/Xcode project analysis and build logic.

use async_trait::async_trait;
use std::path::Path;
use std::process::{Command as SyncCommand, Stdio};
use std::time::Instant;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

use super::cargo::BuildLineParser;
use super::generic_helpers::{offset_to_line_col, sha256_hex};
use super::{
    AstEdgeData, AstNodeData, AstParseResult, BuildModuleMessage, BuildOptions, BuildOutput,
    McpServerDependency, ModuleCapability, TestOptions,
};

use crate::Actor;

use spire_core::build_types::{BuildMetadata, BuildTarget, Dependency};
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;

/// Static Swift build module.
pub struct SwiftBuildModule;

impl SwiftBuildModule {
    pub fn new() -> Self {
        Self
    }

    /// Parse a Swift source file into AST nodes.
    ///
    /// `tree-sitter-swift 0.7` is ABI-incompatible with the `tree-sitter 0.25`
    /// used by the Rust/JS/Python grammars, so Swift uses a purpose-built regex
    /// scanner that understands Swift declarations: imports, types
    /// (class/struct/enum/protocol/extension/actor), functions (incl.
    /// `init`/`deinit`, `async`, `throws`, access modifiers, `mutating`,
    /// `static`/`class` funcs, property wrappers) and stored `let`/`var`
    /// properties. Braces are tracked to assign nesting depth, produce
    /// `child` parenthood edges, and resolve `calls` edges from function
    /// bodies to sibling functions.
    fn parse_source_file(&self, file_path: &Path) -> Result<AstParseResult, String> {
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;
        let content_hash = sha256_hex(&content);

        // ── 1. Collect declaration spans ─────────────────────────────
        let import_re = Regex::new(r"(?m)^\s*(?:@_exported\s+)?import\s+([A-Za-z_][A-Za-z0-9_.]*)")
            .map_err(|e| format!("Bad import regex: {e}"))?;
        let type_re = Regex::new(
            r"(?m)^\s*(?:(?:public|private|internal|fileprivate|open)\s+)?(?:(?:final|indirect)\s+)?(?:@\w+\s+)*(class|struct|enum|protocol|extension|actor)\s+([A-Za-z_][A-Za-z0-9_]*)",
        )
        .map_err(|e| format!("Bad type regex: {e}"))?;
        let func_re = Regex::new(
            r"(?m)^\s*(?:(?:public|private|internal|fileprivate|open|static|class|override|mutating|nonmutating|final|convenience|required)\s+)*(async\s+)?(?:func\s+([A-Za-z_][A-Za-z0-9_]*)|(init|deinit)\b)",
        )
        .map_err(|e| format!("Bad function regex: {e}"))?;
        let var_re = Regex::new(
            r"(?m)^\s*(?:@\w+\s+)*(?:(?:public|private|internal|fileprivate|open|static)\s+)*(?:private(?:\s+set)?\s+)?(let|var)\s+([A-Za-z_][A-Za-z0-9_]*)",
        )
        .map_err(|e| format!("Bad variable regex: {e}"))?;

        let mut decls: Vec<SwiftDecl> = Vec::new();

        for cap in import_re.captures_iter(&content) {
            let m = cap.get(0).unwrap();
            let line_end = content[m.end()..]
                .find('\n')
                .map(|i| m.end() + i)
                .unwrap_or(content.len());
            decls.push(SwiftDecl {
                start: m.start(),
                end: line_end.max(m.end()),
                kind: "import".to_string(),
                name: Some(cap.get(1).unwrap().as_str().to_string()),
                text: m.as_str().to_string(),
                is_public: false,
                is_async: false,
            });
        }

        for cap in type_re.captures_iter(&content) {
            let m = cap.get(0).unwrap();
            let text = m.as_str().to_string();
            decls.push(SwiftDecl {
                start: m.start(),
                end: compute_decl_end(&content, m.end()),
                kind: "class".to_string(),
                name: Some(cap.get(2).unwrap().as_str().to_string()),
                text: text.clone(),
                is_public: text.trim_start().starts_with("public")
                    || text.trim_start().starts_with("open"),
                is_async: false,
            });
        }

        for cap in func_re.captures_iter(&content) {
            let m = cap.get(0).unwrap();
            let text = m.as_str().to_string();
            let name = cap
                .get(2)
                .map(|g| g.as_str().to_string())
                .or_else(|| cap.get(3).map(|g| g.as_str().to_string()))
                .unwrap_or_default();
            decls.push(SwiftDecl {
                start: m.start(),
                end: compute_decl_end(&content, m.end()),
                kind: "function".to_string(),
                name: Some(name),
                text: text.clone(),
                is_public: text.trim_start().starts_with("public")
                    || text.trim_start().starts_with("open"),
                is_async: text.contains("async "),
            });
        }

        for cap in var_re.captures_iter(&content) {
            let m = cap.get(0).unwrap();
            let line_end = content[m.end()..]
                .find('\n')
                .map(|i| m.end() + i)
                .unwrap_or(content.len());
            decls.push(SwiftDecl {
                start: m.start(),
                end: line_end.max(m.end()),
                kind: "variable".to_string(),
                name: Some(cap.get(2).unwrap().as_str().to_string()),
                text: m.as_str().to_string(),
                is_public: false,
                is_async: false,
            });
        }

        decls.sort_by_key(|d| d.start);

        // ── 2. Convert spans to nodes + `child` edges ────────────────
        let mut nodes: Vec<AstNodeData> = Vec::new();
        let mut edges: Vec<AstEdgeData> = Vec::new();
        // Nesting stack: (decl end offset, node index).
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut fn_name_to_index: HashMap<String, usize> = HashMap::new();
        let mut node_idx_by_start: HashMap<usize, usize> = HashMap::new();

        for decl in &decls {
            while let Some(&(end, _)) = stack.last() {
                if end < decl.start {
                    stack.pop();
                } else {
                    break;
                }
            }
            let depth = stack.len() as u32;
            let (start_line, start_col) = offset_to_line_col(&content, decl.start);
            let (end_line, end_col) = offset_to_line_col(&content, decl.end.min(content.len()));
            let text = decl.text.trim();
            let cut = text.find('{').unwrap_or(text.len());
            let signature = Some(text[..cut].trim().to_string());
            let return_type = if decl.kind == "function" {
                extract_swift_return_type(&decl.text)
            } else {
                None
            };

            let idx = nodes.len();
            nodes.push(AstNodeData {
                node_type: decl.kind.clone(),
                name: decl.name.clone(),
                text: decl.text.clone(),
                start_line,
                start_col,
                end_line,
                end_col,
                depth,
                is_public: decl.is_public,
                is_async: decl.is_async,
                signature,
                return_type,
                children: Vec::new(),
            });
            node_idx_by_start.insert(decl.start, idx);

            if let Some(&(_, parent_idx)) = stack.last() {
                edges.push(AstEdgeData {
                    from_index: parent_idx,
                    to_index: idx,
                    edge_type: "child".to_string(),
                    order: None,
                    field: None,
                });
            }
            if decl.kind == "function" {
                if let Some(name) = &decl.name {
                    fn_name_to_index.insert(name.clone(), idx);
                }
            }
            stack.push((decl.end, idx));
        }

        // ── 3. Resolve `calls` edges inside function bodies ──────────
        let call_re = Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")
            .map_err(|e| format!("Bad call regex: {e}"))?;
        let skipped_keywords = [
            "if", "iflet", "guard", "while", "for", "switch", "catch", "return", "repeat",
            "defer", "where", "in", "try", "case", "continue", "break",
        ];
        for decl in &decls {
            if decl.kind != "function" {
                continue;
            }
            let caller_idx = match node_idx_by_start.get(&decl.start) {
                Some(&i) => i,
                None => continue,
            };
            let body = &content[decl.start..decl.end.min(content.len())];
            for cap in call_re.captures_iter(body) {
                let callee = cap.get(1).unwrap().as_str();
                if skipped_keywords.contains(&callee) {
                    continue;
                }
                if let Some(&callee_idx) = fn_name_to_index.get(callee) {
                    if caller_idx != callee_idx {
                        edges.push(AstEdgeData {
                            from_index: caller_idx,
                            to_index: callee_idx,
                            edge_type: "calls".to_string(),
                            order: None,
                            field: None,
                        });
                    }
                }
            }
        }

        Ok(AstParseResult {
            file_path: file_path.to_string_lossy().to_string(),
            language: "Swift".to_string(),
            content_hash,
            has_errors: nodes.is_empty(),
            nodes,
            edges,
            docs: Vec::new(),
        })
    }

    /// Analyze a Swift/Xcode project → BuildMetadata.
    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let has_package = path.join("Package.swift").exists();
        let has_xcodeproj = find_xcodeproj(path);
        let has_xcworkspace = find_xcworkspace(path);

        let mut config_files = Vec::new();
        let mut build_system = String::new();
        if has_package {
            config_files.push("Package.swift".to_string());
            build_system = "SwiftPM".to_string();
        }
        if let Some(ref p) = has_xcodeproj {
            config_files.push(p.clone());
            if build_system.is_empty() {
                build_system = "Xcode".to_string();
            }
        }
        if let Some(ref p) = has_xcworkspace {
            config_files.push(p.clone());
        }

        let project_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Parse SwiftPM (targets, dependencies) via `swift package dump-package`.
        let (targets, dependencies, tools_version) = if has_package {
            parse_swiftpm(path).unwrap_or_default()
        } else {
            (Vec::new(), Vec::new(), None)
        };

        // Xcode schemes (best-effort).
        let schemes: Vec<String> = if has_xcworkspace.is_some() || has_xcodeproj.is_some() {
            parse_xcode_schemes(path)
        } else {
            Vec::new()
        };

        let raw = serde_json::json!({
            "swift_tools_version": tools_version,
            "xcode_schemes": schemes,
        });

        Ok(BuildMetadata {
            project_name: Some(project_name),
            version: None,
            project_type: if has_package {
                "swift-package".into()
            } else {
                "xcode".into()
            },
            build_system,
            targets,
            dependencies,
            config_files,
            project_path: Some(path.to_string_lossy().to_string()),
            raw: Some(raw),
            ..Default::default()
        })
    }

    async fn build(&self, path: &Path, _opts: &BuildOptions) -> Result<BuildOutput, String> {
        // `--no-color-diagnostics` strips ANSI escape codes so the log stays
        // clean. Must appear AFTER the subcommand, e.g. `swift build --no-color-diagnostics`.
        self.run_swift(path, &["build".to_string(), "--no-color-diagnostics".to_string()])
            .await
    }

    /// Run `swift package clean`.
    async fn clean(&self, path: &Path) -> Result<BuildOutput, String> {
        self.run_swift(
            path,
            &[
                "package".to_string(),
                "clean".to_string(),
                "--no-color-diagnostics".to_string(),
            ],
        )
        .await
    }

    /// Run `swiftlint lint --quiet --path <path>` (batch). Returns an
    /// actionable error if swiftlint isn't installed.
    async fn lint(&self, path: &Path) -> Result<BuildOutput, String> {
        let path_str = path.to_string_lossy().to_string();
        self.run_external("swiftlint", &["lint", "--quiet", "--path", &path_str])
            .await
    }

    /// Run `swiftformat --lint <path>` (batch). Returns an actionable error
    /// if swiftformat isn't installed.
    async fn format(&self, path: &Path) -> Result<BuildOutput, String> {
        let path_str = path.to_string_lossy().to_string();
        self.run_external("swiftformat", &["--lint", &path_str]).await
    }

    /// Run `swiftlint --fix --path <path>` to auto-fix what swiftlint can.
    async fn fix(&self, path: &Path) -> Result<BuildOutput, String> {
        let path_str = path.to_string_lossy().to_string();
        self.run_external("swiftlint", &["--fix", "--path", &path_str]).await
    }

    /// Execute an external tool (swiftlint / swiftformat) with a friendly
    /// install hint if the binary is missing.
    async fn run_external(&self, binary: &str, args: &[&str]) -> Result<BuildOutput, String> {
        let mut cmd = tokio::process::Command::new(binary);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = cmd.output().await.map_err(|e| {
            format!(
                "{binary} not available: {e}. Install with `brew install {binary}` (and add to PATH)."
            )
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(BuildOutput {
            success: output.status.success(),
            command: format!("{binary} {}", args.join(" ")),
            duration_secs: 0.0,
            output: if stderr.is_empty() {
                stdout
            } else {
                format!("{stdout}\n{stderr}")
            },
            exit_code: output.status.code(),
        })
    }

    /// Execute an external tool (swiftlint / swiftformat) and stream each
    /// output line as a BuildEvent to the UI, mirroring the Swift/Cargo
    /// streaming path. Returns an actionable error if the binary is missing.
    async fn run_external_streaming(
        &self,
        binary: &str,
        args: &[String],
        event_tx: &tokio::sync::mpsc::UnboundedSender<super::BuildEvent>,
    ) -> Result<BuildOutput, String> {
        let start = Instant::now();
        let command_str = format!("{binary} {}", args.join(" "));

        let mut cmd = tokio::process::Command::new(binary);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            format!(
                "{binary} not available: {e}. Install with `brew install {binary}` (and add to PATH)."
            )
        })?;

        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        let mut out_reader = tokio::io::BufReader::new(stdout).lines();
        let mut all_output = String::new();
        let event_tx_stdout = event_tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut parser = BuildLineParser::new(true);
            while let Ok(Some(line)) = out_reader.next_line().await {
                for ev in parser.feed(&line, true) {
                    let _ = event_tx_stdout.send(ev);
                }
                all_output.push_str(&line);
                all_output.push('\n');
            }
            for ev in parser.finish(true) {
                let _ = event_tx_stdout.send(ev);
            }
            all_output
        });

        let mut err_reader = tokio::io::BufReader::new(stderr).lines();
        let mut all_err = String::new();
        let event_tx_stderr = event_tx.clone();
        let stderr_task = tokio::spawn(async move {
            let mut parser = BuildLineParser::new(true);
            while let Ok(Some(line)) = err_reader.next_line().await {
                for ev in parser.feed(&line, false) {
                    let _ = event_tx_stderr.send(ev);
                }
                all_err.push_str(&line);
                all_err.push('\n');
            }
            for ev in parser.finish(false) {
                let _ = event_tx_stderr.send(ev);
            }
            all_err
        });

        let status = child.wait().await.map_err(|e| format!("{binary} wait failed: {e}"))?;
        let (out, err) = tokio::join!(stdout_task, stderr_task);
        let stdout = out.map_err(|e| e.to_string())?;
        let stderr = err.map_err(|e| e.to_string())?;

        let combined = if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}\n{stderr}")
        };

        Ok(BuildOutput {
            success: status.success(),
            command: command_str,
            duration_secs: start.elapsed().as_secs_f64(),
            output: combined,
            exit_code: status.code(),
        })
    }

    async fn test(&self, path: &Path, opts: &TestOptions) -> Result<BuildOutput, String> {
        // `--no-color-diagnostics` strips ANSI escape codes so the log stays clean.
        let mut args = vec!["test".to_string(), "--no-color-diagnostics".to_string()];
        if let Some(filter) = &opts.filter {
            args.push("--filter".to_string());
            args.push(filter.clone());
        }
        self.run_swift(path, &args).await
    }

    async fn run_swift(&self, path: &Path, args: &[String]) -> Result<BuildOutput, String> {
        let start = Instant::now();
        let command_str = format!("swift {}", args.join(" "));
        let mut cmd = Command::new("swift");
        cmd.current_dir(path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to execute swift: {e}"))?;
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

    /// Execute `swift` and stream each output line as a BuildEvent to the UI.
    /// Mirrors the Cargo module's streaming behavior: spawn the process, pipe
    /// stdout/stderr, parse each line through the shared BuildLineParser, and
    /// emit events in real-time so the UI shows incremental build progress.
    async fn run_swift_streaming(
        &self,
        path: &Path,
        args: &[String],
        event_tx: &tokio::sync::mpsc::UnboundedSender<super::BuildEvent>,
    ) -> Result<BuildOutput, String> {
        let start = Instant::now();
        let command_str = format!("swift {}", args.join(" "));

        let mut cmd = Command::new("swift");
        cmd.current_dir(path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn swift: {e}"))?;

        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        // Stream stdout line-by-line through the shared build line parser.
        let mut out_reader = tokio::io::BufReader::new(stdout).lines();
        let mut all_output = String::new();
        let event_tx_stdout = event_tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut parser = BuildLineParser::new(false);
            while let Ok(Some(line)) = out_reader.next_line().await {
                for ev in parser.feed(&line, true) {
                    let _ = event_tx_stdout.send(ev);
                }
                all_output.push_str(&line);
                all_output.push('\n');
            }
            for ev in parser.finish(true) {
                let _ = event_tx_stdout.send(ev);
            }
            all_output
        });

        // Stream stderr line-by-line through the same parser.
        let mut err_reader = tokio::io::BufReader::new(stderr).lines();
        let mut all_err = String::new();
        let event_tx_stderr = event_tx.clone();
        let stderr_task = tokio::spawn(async move {
            let mut parser = BuildLineParser::new(false);
            while let Ok(Some(line)) = err_reader.next_line().await {
                for ev in parser.feed(&line, false) {
                    let _ = event_tx_stderr.send(ev);
                }
                all_err.push_str(&line);
                all_err.push('\n');
            }
            for ev in parser.finish(false) {
                let _ = event_tx_stderr.send(ev);
            }
            all_err
        });

        let status = child.wait().await.map_err(|e| format!("swift wait failed: {e}"))?;
        let (out, err) = tokio::join!(stdout_task, stderr_task);
        let stdout = out.map_err(|e| e.to_string())?;
        let stderr = err.map_err(|e| e.to_string())?;

        let combined = if stderr.is_empty() {
            stdout
        } else {
            format!("{stdout}\n{stderr}")
        };

        Ok(BuildOutput {
            success: status.success(),
            command: command_str,
            duration_secs: start.elapsed().as_secs_f64(),
            output: combined,
            exit_code: status.code(),
        })
    }
}

impl Default for SwiftBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

fn find_xcodeproj(root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().map(|e| e == "xcodeproj").unwrap_or(false) {
            return p.file_name().map(|n| n.to_string_lossy().to_string());
        }
    }
    None
}

fn find_xcworkspace(root: &Path) -> Option<String> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().map(|e| e == "xcworkspace").unwrap_or(false) {
            return p.file_name().map(|n| n.to_string_lossy().to_string());
        }
    }
    None
}

/// Run `swift package dump-package` and extract targets + dependencies.
fn parse_swiftpm(
    root: &Path,
) -> Result<(Vec<BuildTarget>, Vec<Dependency>, Option<String>), String> {
    let output = SyncCommand::new("swift")
        .args(["package", "dump-package"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("Failed to run swift package dump-package: {e}"))?;
    if !output.status.success() {
        return Ok((Vec::new(), Vec::new(), None));
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse dump-package JSON: {e}"))?;

    let tools_version = json
        .get("toolsVersion")
        .and_then(|v| v.get("_version"))
        .and_then(|v| v.as_str())
        .or_else(|| json.get("toolsVersion").and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    let mut targets = Vec::new();
    if let Some(arr) = json.get("targets").and_then(|v| v.as_array()) {
        for t in arr {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source_path = t
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            // SwiftPM targets are single-source-set (host / Single): one
            // source group per target, mirroring the Rust single-target shape.
            let source_units = source_path
                .as_ref()
                .map(|p| {
                    vec![spire_core::build_types::SourceUnit {
                        role: spire_core::build_types::SourceRole::App,
                        path: p.clone(),
                        language: "Swift".to_string(),
                    }]
                })
                .unwrap_or_default();
            targets.push(BuildTarget {
                name,
                kind: vec![t
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("regular")
                    .to_lowercase()],
                source_path,
                source_units,
                ..Default::default()
            });
        }
    }

    let mut dependencies = Vec::new();
    if let Some(arr) = json.get("dependencies").and_then(|v| v.as_array()) {
        for d in arr {
            if let Some(sc) = d.get("sourceControl").and_then(|v| v.as_array()) {
                for sc_entry in sc {
                    dependencies.push(Dependency {
                        name: guess_dep_name(sc_entry),
                        version: None,
                        version_req: sc_entry
                            .get("requirement")
                            .and_then(|r| r.get("range"))
                            .and_then(|range| {
                                // SwiftPM 6+ uses an array: range: [{"lowerBound": "...", "upperBound": "..."}]
                                if let Some(arr) = range.as_array() {
                                    arr.first()
                                        .and_then(|r| r.get("lowerBound"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.to_string())
                                } else {
                                    range.get("lowerBound").and_then(|v| v.as_str()).map(|s| s.to_string())
                                }
                            }),
                        kind: Some("remote".to_string()),
                        source: Some("sourceControl".to_string()),
                        source_url: sc_entry
                            .get("url")
                            .or_else(|| {
                                sc_entry
                                    .get("location")
                                    .and_then(|l| l.get("remote"))
                                    .and_then(|r| r.get(0))
                                    .and_then(|r| r.get("urlString"))
                            })
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        features: None,
                        ..Default::default()
                    });
                }
            }
            if let Some(fs) = d.get("fileSystem").and_then(|v| v.as_array()) {
                for fs_entry in fs {
                    dependencies.push(Dependency {
                        name: fs_entry
                            .get("path")
                            .and_then(|v| v.as_str())
                            .map(|s| {
                                Path::new(s)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| s.to_string())
                            })
                            .unwrap_or_else(|| "unknown".to_string()),
                        version: None,
                        version_req: None,
                        kind: Some("local".to_string()),
                        source: Some("fileSystem".to_string()),
                        source_url: None,
                        features: None,
                        ..Default::default()
                    });
                }
            }
        }
    }

    Ok((targets, dependencies, tools_version))
}

fn guess_dep_name(entry: &serde_json::Value) -> String {
    if let Some(url) = entry.get("url").and_then(|v| v.as_str()) {
        let p = url.trim_end_matches(".git");
        p.rsplit('/').next().unwrap_or(url).to_string()
    } else if let Some(id) = entry.get("identity").and_then(|v| v.as_str()) {
        id.to_string()
    } else {
        "unknown".to_string()
    }
}

/// Query `xcodebuild -list -json` for schemes (best-effort).
fn parse_xcode_schemes(root: &Path) -> Vec<String> {
    let output = SyncCommand::new("xcodebuild")
        .arg(root.join(find_xcodeproj(root).unwrap_or_default()))
        .args(["-list", "-json"])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                return json
                    .get("project")
                    .and_then(|p| p.get("schemes"))
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
            }
        }
    }
    Vec::new()
}

#[async_trait]
impl Actor for SwiftBuildModule {
    type Message = BuildModuleMessage;


    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "swift".to_string(),
                    config_files: vec!["Package.swift".to_string()],
                    build_system: "SwiftPM".to_string(),
                    language: "Swift".to_string(),
                    source_extensions: vec!["swift".to_string()],
                supports_clean: true,
                supports_lint: true,
                supports_format: true,
                supports_fix: true,
                mcp_servers: vec![McpServerDependency {
                        name: "swiftpm-mcp".to_string(),
                        package: "swiftpm-mcp-server".to_string(),
                        install_command: "cargo install swiftpm-mcp-server".to_string(),
                        command: "swiftpm-mcp-server".to_string(),
                        args: vec![],
                        autostart: false,
                        build_type: Some("SwiftPM".to_string()),
                        allowed_for_steps: vec!["discover_dependencies".to_string()],
                        allowed_tools: vec![],
                    }],
                });
            }

            BuildModuleMessage::Analyze { path, reply_to } => {
                let result = self.analyze(&path);
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Build {
                path,
                metadata: _metadata,
                opts,
                build_spec: _,
                reply_to,
            } => {
                let result = self.build(&path, &opts).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::BuildStreaming {
                path,
                metadata: _metadata,
                opts: _opts,
                build_spec: _,
                event_tx,
                reply_to,
            } => {
                // Stream swift build output line-by-line to the UI (async push,
                // no polling) — mirrors the Cargo module's streaming behavior.
                // `--no-color-diagnostics` (AFTER the subcommand) strips ANSI
                // escape codes so the log is clean.
                let args = vec!["build".to_string(), "--no-color-diagnostics".to_string()];
                let result = self.run_swift_streaming(&path, &args, &event_tx).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Test {
                path,
                metadata: _metadata,
                opts,
                reply_to,
            } => {
                let result = self.test(&path, &opts).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Clean { path, reply_to, .. } => {
                let result = self.clean(&path).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Lint { path, reply_to, .. } => {
                let result = self.lint(&path).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Format { path, reply_to, .. } => {
                let result = self.format(&path).await;
                let _ = reply_to.send(result);
            }
            BuildModuleMessage::Fix { path, reply_to, .. } => {
                let result = self.fix(&path).await;
                let _ = reply_to.send(result);
            }
            BuildModuleMessage::LintStreaming {
                path,
                event_tx,
                reply_to,
                ..
            } => {
                let path_str = path.to_string_lossy().to_string();
                let args = vec![
                    "lint".to_string(),
                    "--quiet".to_string(),
                    "--path".to_string(),
                    path_str,
                ];
                let result = self.run_external_streaming("swiftlint", &args, &event_tx).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::FixStreaming {
                path,
                event_tx,
                reply_to,
                ..
            } => {
                let path_str = path.to_string_lossy().to_string();
                let args = vec![
                    "--fix".to_string(),
                    "--path".to_string(),
                    path_str,
                ];
                let result = self.run_external_streaming("swiftlint", &args, &event_tx).await;
                let _ = reply_to.send(result);
            }



            BuildModuleMessage::ParseSourceFile {
                file_path,
                reply_to,
            } => {
                let result = self.parse_source_file(&PathBuf::from(&file_path));
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::ScaffoldBuildConfig {
                project_name,
                goal: _goal,
                platforms: _platforms,
                structure: _structure,
                embedded: _,
                reply_to,
            } => {
                let bc = r#"// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "__P__",
    targets: [.executableTarget(name: "__P__", path: "Sources")]
)
"#.replace("__P__", &project_name);
                let sc = r#"print("Hello from __P__!")
"#.replace("__P__", &project_name);
                let _ = reply_to.send(Ok(super::ScaffoldOutput {
                    build_file: "Package.swift".to_string(),
                    build_content: bc,
                    source_dir: "Sources".to_string(),
                    source_file: "main.swift".to_string(),
                    source_content: sc,
                    ..Default::default()
                }));
            }

            BuildModuleMessage::CallTool { reply_to, .. } => {
                let _ = reply_to.send(serde_json::json!({
                    "error": "Swift module CallTool not yet wired"
                }));
            }
        }
    }
}

/// A Swift declaration span collected by the regex scanner.
struct SwiftDecl {
    start: usize,
    end: usize,
    kind: String,
    name: Option<String>,
    text: String,
    is_public: bool,
    is_async: bool,
}

/// Find the end offset of a declaration body. If the header line (or a nearby
/// line) opens a `{`, walk braces to the matching `}`. Otherwise the
/// declaration is header-only (import, a `var` line, a protocol requirement),
/// so it ends at the nominal position.
fn compute_decl_end(content: &str, after_header: usize) -> usize {
    let mut search_from = after_header;
    loop {
        let line_end_local = content[search_from..]
            .find('\n')
            .map(|i| search_from + i)
            .unwrap_or(content.len());
        let line = &content[search_from..line_end_local];
        if let Some(rel) = line.find('{') {
            let brace_pos = search_from + rel;
            return match_bracing(content, brace_pos).unwrap_or(line_end_local.max(brace_pos + 1));
        }
        if line_end_local >= content.len() || search_from > after_header + 800 {
            break;
        }
        search_from = line_end_local + 1;
    }
    after_header
}

/// Given a `{` offset, walk the content and return the offset just past the
/// matching `}` (or None if unbalanced).
fn match_bracing(content: &str, open: usize) -> Option<usize> {
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
            _ => {}
        }
    }
    None
}

/// Extract the Swift return type after `->` in a function signature.
fn extract_swift_return_type(text: &str) -> Option<String> {
    let idx = text.find("->")?;
    let after = &text[idx + 2..];
    let cut = after.find('{').or_else(|| after.find(';')).unwrap_or(after.len());
    let rt = after[..cut].trim();
    if rt.is_empty() {
        None
    } else {
        Some(rt.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn swift_parser_extracts_imports_types_functions_variables() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Sample.swift");
        let code = r#"import SwiftUI
import Foundation

public struct ContentView: View {
    @State private var count = 0

    var body: some View {
        VStack {
            Text("Hello")
        }
    }

    func increment() {
        count += 1
        helper()
    }

    private func helper() -> Int {
        return 42
    }
}

enum Mode {
    case idle
    case running
}
"#;
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(code.as_bytes()).unwrap();
        f.flush().unwrap();

        let module = SwiftBuildModule::new();
        let result = module.parse_source_file(&path).unwrap();

        let imports = result
            .nodes
            .iter()
            .filter(|n| n.node_type == "import")
            .count();
        let classes = result
            .nodes
            .iter()
            .filter(|n| n.node_type == "class")
            .count();
        let funcs = result
            .nodes
            .iter()
            .filter(|n| n.node_type == "function")
            .count();
        let vars = result
            .nodes
            .iter()
            .filter(|n| n.node_type == "variable")
            .count();

        assert_eq!(imports, 2, "expected 2 imports");
        assert!(classes >= 2, "expected ContentView + Mode, got {}", classes);
        assert!(funcs >= 2, "expected increment + helper, got {}", funcs);
        // `count` (@State) and `body` (computed property) are variables.
        assert!(vars >= 2, "expected count + body, got {}", vars);

        let names: Vec<&str> = result
            .nodes
            .iter()
            .map(|n| n.name.as_deref().unwrap_or(""))
            .collect();
        assert!(names.contains(&"ContentView"));
        assert!(names.contains(&"increment"));
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"count"));
        assert!(names.contains(&"body"));

        // Nested functions should have depth >= 1.
        let increment = result
            .nodes
            .iter()
            .find(|n| n.name.as_deref() == Some("increment"))
            .unwrap();
        assert!(
            increment.depth >= 1,
            "increment should be nested, depth={}",
            increment.depth
        );

        // increment() calls helper() → a `calls` edge should exist.
        let calls: Vec<&AstEdgeData> = result
            .edges
            .iter()
            .filter(|e| e.edge_type == "calls")
            .collect();
        assert!(
            !calls.is_empty(),
            "expected a calls edge from increment → helper"
        );
    }
}
