// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Node build module — owns Node.js/npm/pnpm/Yarn project analysis and build logic.

use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

use super::generic_helpers::sha256_hex;
use super::{
    javascript_language_config, parse_with_tree_sitter, AstParseResult, BuildModuleMessage,
    BuildOptions, BuildOutput, McpServerDependency, ModuleCapability, TestOptions,
};
use std::path::PathBuf;

use crate::Actor;

use spire_core::build_types::{BuildMetadata, BuildScript, BuildTarget, Dependency, WorkspaceMember};

/// Static Node build module.
pub struct NodeBuildModule;

impl NodeBuildModule {
    pub fn new() -> Self {
        Self
    }

    /// Parse a JavaScript/TypeScript source file into AST nodes using
    /// tree-sitter-javascript.
    fn parse_source_file_with_tree_sitter(&self, file_path: &PathBuf) -> Result<AstParseResult, String> {
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;
        let content_hash = sha256_hex(&content);
        let config = javascript_language_config();
        Ok(parse_with_tree_sitter(
            file_path,
            &content,
            &content_hash,
            &config,
        ))
    }

    /// Parse package.json → BuildMetadata.
    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let pkg_path = path.join("package.json");
        let content = std::fs::read_to_string(&pkg_path)
            .map_err(|e| format!("Failed to read package.json: {e}"))?;
        let json: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("Invalid package.json: {e}"))?;

        let name = json
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let version = json
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let description = json
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let has_pnpm_lock = path.join("pnpm-lock.yaml").exists();
        let has_yarn_lock = path.join("yarn.lock").exists();
        let has_pnpm_workspace = path.join("pnpm-workspace.yaml").exists();

        let build_system = if has_pnpm_lock || has_pnpm_workspace {
            "pnpm"
        } else if has_yarn_lock {
            "yarn"
        } else {
            "npm"
        };

        let is_vscode_ext = json.get("contributes").is_some()
            || json.get("activationEvents").is_some()
            || json.get("engines").and_then(|e| e.get("vscode")).is_some();

        let project_type = if is_vscode_ext {
            "vscode_extension"
        } else if has_pnpm_workspace {
            "pnpm_workspace"
        } else {
            "node_package"
        };

        // Scripts
        let build_system_name = build_system;
        let scripts: Vec<BuildScript> = json
            .get("scripts")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(sname, cmd)| {
                        let raw_cmd = cmd.as_str().unwrap_or("").to_string();
                        BuildScript {
                            name: sname.clone(),
                            command: raw_cmd.clone(),
                            tool_call: Some(serde_json::json!({
                                "tool": "project/build",
                                "args": { "mode": sname, "command": raw_cmd, "buildSystem": build_system_name }
                            })),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Dependencies
        let mut dependencies = Vec::new();
        if let Some(deps) = json.get("dependencies").and_then(|v| v.as_object()) {
            for (dname, ver) in deps {
                dependencies.push(Dependency {
                    name: dname.clone(),
                    version: ver.as_str().map(|s| s.to_string()),
                    version_req: ver.as_str().map(|s| s.to_string()),
                    kind: Some("normal".to_string()),
                    source: Some("registry".to_string()),
                    source_url: None,
                    features: None,
                    ..Default::default()
                });
            }
        }
        if let Some(deps) = json.get("devDependencies").and_then(|v| v.as_object()) {
            for (dname, ver) in deps {
                dependencies.push(Dependency {
                    name: dname.clone(),
                    version: ver.as_str().map(|s| s.to_string()),
                    version_req: ver.as_str().map(|s| s.to_string()),
                    kind: Some("dev".to_string()),
                    source: Some("registry".to_string()),
                    source_url: None,
                    features: None,
                    ..Default::default()
                });
            }
        }

        // Workspace members
        let workspace_members = detect_workspace_members(path, &json);

        // Entry points from "main" / "bin"
        let mut targets = Vec::new();
        if let Some(main) = json.get("main").and_then(|v| v.as_str()) {
            targets.push(BuildTarget {
                name: name.clone().unwrap_or_else(|| "main".to_string()),
                kind: vec!["lib".to_string()],
                source_path: Some(main.to_string()),
                ..Default::default()
            });
        }
        if let Some(bin) = json.get("bin") {
            if let Some(bin_str) = bin.as_str() {
                targets.push(BuildTarget {
                    name: name.clone().unwrap_or_else(|| "bin".to_string()),
                    kind: vec!["bin".to_string()],
                    source_path: Some(bin_str.to_string()),
                    ..Default::default()
                });
            } else if let Some(bin_obj) = bin.as_object() {
                for (bin_name, bin_path) in bin_obj {
                    targets.push(BuildTarget {
                        name: bin_name.clone(),
                        kind: vec!["bin".to_string()],
                        source_path: bin_path.as_str().map(|s| s.to_string()),
                        ..Default::default()
                    });
                }
            }
        }

        let mut config_files = vec!["package.json".to_string()];
        if has_pnpm_lock {
            config_files.push("pnpm-lock.yaml".to_string());
        }
        if has_pnpm_workspace {
            config_files.push("pnpm-workspace.yaml".to_string());
        }
        if has_yarn_lock {
            config_files.push("yarn.lock".to_string());
        }

        Ok(BuildMetadata {
            project_name: name,
            description,
            version,
            project_type: project_type.to_string(),
            build_system: build_system.to_string(),
            is_workspace: has_pnpm_workspace || !workspace_members.is_empty(),
            workspace_members,
            scripts,
            dependencies,
            targets,
            config_files,
            project_path: Some(path.to_string_lossy().to_string()),
            raw: Some(json),
            ..Default::default()
        })
    }

    /// Run the build script via the detected package manager.
    async fn build(&self, path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
        let (pm, script) = if opts.mode.is_empty() {
            (detect_pm(path), "build".to_string())
        } else {
            (detect_pm(path), opts.mode.clone())
        };
        self.run_pm(path, pm, &["run".to_string(), script]).await
    }

    /// Run the test script.
    async fn test(&self, path: &Path, opts: &TestOptions) -> Result<BuildOutput, String> {
        let pm = detect_pm(path);
        let mut args = vec!["run".to_string(), "test".to_string()];
        if let Some(filter) = &opts.filter {
            args.push("--".to_string());
            args.push(filter.clone());
        }
        self.run_pm(path, pm, &args).await
    }

    async fn run_pm(&self, path: &Path, pm: &str, args: &[String]) -> Result<BuildOutput, String> {
        let start = Instant::now();
        let command_str = format!("{pm} {}", args.join(" "));
        let mut cmd = Command::new(pm);
        cmd.current_dir(path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to execute {pm}: {e}"))?;
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
}

impl Default for NodeBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect the package manager from lock files.
fn detect_pm(path: &Path) -> &'static str {
    if path.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if path.join("yarn.lock").exists() {
        "yarn"
    } else {
        "npm"
    }
}

/// Detect workspace members from package.json workspaces field.
fn detect_workspace_members(root: &Path, json: &serde_json::Value) -> Vec<WorkspaceMember> {
    let mut members = Vec::new();
    let workspaces = json.get("workspaces");
    if workspaces.is_none() {
        return members;
    }
    let workspaces = workspaces.unwrap();

    let patterns: Vec<&str> = if let Some(arr) = workspaces.as_array() {
        arr.iter().filter_map(|v| v.as_str()).collect()
    } else if let Some(obj) = workspaces.as_object() {
        obj.get("packages")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default()
    } else {
        return members;
    };

    for pattern in patterns {
        let glob_path = root.join(pattern);
        if !pattern.contains('*') && !pattern.contains('?') {
            if glob_path.is_dir() {
                if let Some(mname) = glob_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                {
                    members.push(WorkspaceMember {
                        name: mname,
                        path: pattern.to_string(),
                        version: None,
                    });
                }
            }
        } else if let Some(parent) = glob_path.parent() {
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let entry_name = entry.file_name().to_string_lossy().to_string();
                    if entry.path().is_dir() && simple_glob_match(pattern, &entry_name) {
                        let en = entry_name.clone();
                        members.push(WorkspaceMember {
                            name: entry_name,
                            path: format!("{}/{}", pattern.trim_end_matches('*'), en),
                            version: None,
                        });
                    }
                }
            }
        }
    }
    members
}

/// Simple glob matching (supports * and ?).
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        return pattern == name;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        if pattern.contains('?') {
            if pattern.len() != name.len() {
                return false;
            }
            for (p, n) in pattern.chars().zip(name.chars()) {
                if p != '?' && p != n {
                    return false;
                }
            }
            return true;
        }
        return pattern == name;
    }
    if !parts[0].is_empty() && !name.starts_with(parts[0]) {
        return false;
    }
    if !parts.last().unwrap_or(&"").is_empty() && !name.ends_with(parts.last().unwrap_or(&"")) {
        return false;
    }
    let mut pos = parts[0].len();
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = name[pos..].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }
    true
}

#[async_trait]
impl Actor for NodeBuildModule {
    type Message = BuildModuleMessage;


    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "node".to_string(),
                    config_files: vec!["package.json".to_string()],
                    build_system: "npm".to_string(),
                    language: "JavaScript".to_string(),
                    source_extensions: vec![
                        "js".to_string(),
                        "jsx".to_string(),
                        "ts".to_string(),
                        "tsx".to_string(),
                    ],
                supports_clean: false,
                supports_lint: false,
                supports_format: false,
                supports_fix: false,
                mcp_servers: vec![McpServerDependency {
                        name: "npm-mcp".to_string(),
                        package: "npm-mcp-server".to_string(),
                        install_command: "cargo install npm-mcp-server".to_string(),
                        command: "npm-mcp-server".to_string(),
                        args: vec![],
                        autostart: false,
                        build_type: Some("npm".to_string()),
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
                opts,
                build_spec: _,
                event_tx,
                reply_to,
            } => {
                // Not yet streaming for this module — fall back to a batch build
                // and emit a single synthetic "finished" event.
                let result = self.build(&path, &opts).await;
                let _ = event_tx.send(super::BuildEvent {
                    line: format!("Finished {} in {:?}s", path.display(), result.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
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

            BuildModuleMessage::Clean { reply_to, .. } => {
                let _ = reply_to.send(Err("clean not implemented for this module".to_string()));
            }

            BuildModuleMessage::Lint { reply_to, .. } => {
                let _ = reply_to.send(Err("lint not implemented for this module".to_string()));
            }

            BuildModuleMessage::Format { reply_to, .. } => {
                let _ = reply_to.send(Err("format not implemented for this module".to_string()));
            }
            BuildModuleMessage::Fix { reply_to, .. } => {
                let _ = reply_to.send(Err("fix not implemented for this module".to_string()));
            }
            BuildModuleMessage::LintStreaming { reply_to, .. } => {
                let _ = reply_to.send(Err("lint streaming not implemented for this module".to_string()));
            }

            BuildModuleMessage::FixStreaming { reply_to, .. } => {
                let _ = reply_to.send(Err("fix streaming not implemented for this module".to_string()));
            }



            BuildModuleMessage::ParseSourceFile {
                file_path,
                reply_to,
            } => {
                let result = self.parse_source_file_with_tree_sitter(&file_path);
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
                let bc = r#"{
  "name": "__P__",
  "version": "0.1.0",
  "main": "src/index.js",
  "scripts": { "start": "node src/index.js" },
  "dependencies": {}
}
"#.replace("__P__", &project_name);
                let sc = r#"console.log("Hello from __P__!");
"#.replace("__P__", &project_name);
                let _ = reply_to.send(Ok(super::ScaffoldOutput {
                    build_file: "package.json".to_string(),
                    build_content: bc,
                    source_dir: "src".to_string(),
                    source_file: "index.js".to_string(),
                    source_content: sc,
                    ..Default::default()
                }));
            }

            BuildModuleMessage::CallTool { reply_to, .. } => {
                let _ = reply_to.send(serde_json::json!({
                    "error": "Node module CallTool not yet wired"
                }));
            }
        }
    }
}
