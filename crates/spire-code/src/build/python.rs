// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Python build module — pyproject.toml analysis and build/test execution.

use async_trait::async_trait;
use std::path::{Path, PathBuf};

use super::generic_helpers::{parse_key_value, run_cmd, sha256_hex};
use super::{
    parse_with_tree_sitter, python_language_config, AstParseResult, BuildModuleMessage,
    BuildOptions, BuildOutput, McpServerDependency, ModuleCapability, TestOptions,
};
use crate::Actor;
use spire_core::build_types::BuildMetadata;

/// Static Python build module.
pub struct PythonBuildModule;

impl PythonBuildModule {
    pub fn new() -> Self {
        Self
    }

    /// Parse a Python source file into AST nodes using tree-sitter-python.
    fn parse_source_file_with_tree_sitter(
        &self,
        file_path: &PathBuf,
    ) -> Result<AstParseResult, String> {
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;
        let content_hash = sha256_hex(&content);
        let config = python_language_config();
        Ok(parse_with_tree_sitter(
            file_path,
            &content,
            &content_hash,
            &config,
        ))
    }

    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let mut metadata = parse_key_value(path, "pyproject.toml", "project")?;
        metadata.build_system = "Python".to_string();
        metadata.project_type = "Python_project".to_string();
        Ok(metadata)
    }

    async fn build(&self, path: &Path, _opts: &BuildOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "python", &["-m", "build"]).await
    }

    async fn test(&self, path: &Path, _opts: &TestOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "pytest", &[]).await
    }
}

impl Default for PythonBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for PythonBuildModule {
    type Message = BuildModuleMessage;


    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "python".to_string(),
                    config_files: vec!["pyproject.toml".to_string()],
                    build_system: "Python".to_string(),
                    language: "Python".to_string(),
                    source_extensions: vec!["py".to_string()],
                supports_clean: false,
                supports_lint: false,
                supports_format: false,
                supports_fix: false,
                mcp_servers: vec![McpServerDependency {
                        name: "pypi-mcp".to_string(),
                        package: "pypi-mcp-server".to_string(),
                        install_command: "cargo install pypi-mcp-server".to_string(),
                        command: "pypi-mcp-server".to_string(),
                        args: vec![],
                        autostart: false,
                        build_type: Some("PyPI".to_string()),
                        allowed_for_steps: vec!["discover_dependencies".to_string()],
                        allowed_tools: vec![],
                    }],
                });
            }
            BuildModuleMessage::Analyze { path, reply_to } => {
                let _ = reply_to.send(self.analyze(&path));
            }
            BuildModuleMessage::Build {
                path,
                opts,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.build(&path, &opts).await);
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
                opts,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.test(&path, &opts).await);
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
                let bc = r#"[project]
name = "__P__"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = []

[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"
"#.replace("__P__", &project_name);
                let sc = r#"def main():
    print("Hello from __P__!")

if __name__ == "__main__":
    main()
"#.replace("__P__", &project_name);
                let _ = reply_to.send(Ok(super::ScaffoldOutput {
                    build_file: "pyproject.toml".to_string(),
                    build_content: bc,
                    source_dir: "".to_string(),
                    source_file: "main.py".to_string(),
                    source_content: sc,
                    ..Default::default()
                }));
            }

            BuildModuleMessage::CallTool { reply_to, .. } => {
                let _ = reply_to
                    .send(serde_json::json!({ "error": "python module CallTool not yet wired" }));
            }
        }
    }
}
