// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Go build module — go.mod analysis and build/test execution.

use async_trait::async_trait;
use std::path::Path;

use super::generic_helpers::parse_source_file_std;
use super::{
    BuildModuleMessage, BuildOptions, BuildOutput, McpServerDependency, ModuleCapability, TestOptions,
};

use super::generic_helpers::{parse_key_value, run_cmd};
use crate::Actor;
use spire_core::build_types::BuildMetadata;

/// Static Go build module.
pub struct GoBuildModule;

impl GoBuildModule {
    pub fn new() -> Self {
        Self
    }

    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let mut metadata = parse_key_value(path, "go.mod", "")?;
        metadata.build_system = "Go".to_string();
        metadata.project_type = "Go_project".to_string();
        Ok(metadata)
    }

    async fn build(&self, path: &Path, _opts: &BuildOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "go", &["build", "./..."]).await
    }

    async fn test(&self, path: &Path, _opts: &TestOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "go", &["test", "./..."]).await
    }
}

impl Default for GoBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for GoBuildModule {
    type Message = BuildModuleMessage;


    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "go".to_string(),
                    config_files: vec!["go.mod".to_string()],
                    build_system: "Go".to_string(),
                    language: "Go".to_string(),
                    source_extensions: vec!["go".to_string()],
                supports_clean: false,
                supports_lint: false,
                supports_format: false,
                supports_fix: false,
                mcp_servers: vec![McpServerDependency {
                        name: "gomod-mcp".to_string(),
                        package: "gomod-mcp-server".to_string(),
                        install_command: "cargo install gomod-mcp-server".to_string(),
                        command: "gomod-mcp-server".to_string(),
                        args: vec![],
                        autostart: false,
                        build_type: Some("Go".to_string()),
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
                let result = parse_source_file_std(&std::path::PathBuf::from(&file_path), "Go");
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
                let bc = r#"module __P__

go 1.22
"#.replace("__P__", &project_name);
                let sc = r#"package main

import "fmt"

func main() {
    fmt.Println("Hello from __P__!")
}
"#.replace("__P__", &project_name);
                let _ = reply_to.send(Ok(super::ScaffoldOutput {
                    build_file: "go.mod".to_string(),
                    build_content: bc,
                    source_dir: "".to_string(),
                    source_file: "main.go".to_string(),
                    source_content: sc,
                    ..Default::default()
                }));
            }

            BuildModuleMessage::CallTool { reply_to, .. } => {
                let _ = reply_to
                    .send(serde_json::json!({ "error": "go module CallTool not yet wired" }));
            }
        }
    }
}
