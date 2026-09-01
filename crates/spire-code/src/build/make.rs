// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Make build module — Makefile analysis and build/test execution.

use async_trait::async_trait;
use std::path::Path;

use super::generic_helpers::parse_source_file_std;
use super::{
    BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability, TestOptions,
};

use super::generic_helpers::{parse_key_value, run_cmd};
use crate::Actor;
use spire_core::build_types::BuildMetadata;

/// Static Make build module.
pub struct MakeBuildModule;

impl MakeBuildModule {
    pub fn new() -> Self {
        Self
    }

    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let mut metadata = parse_key_value(path, "Makefile", "")?;
        metadata.build_system = "Make".to_string();
        metadata.project_type = "Make_project".to_string();
        Ok(metadata)
    }

    async fn build(&self, path: &Path, _opts: &BuildOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "make", &["build"]).await
    }

    async fn test(&self, path: &Path, _opts: &TestOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "make", &["test"]).await
    }
}

impl Default for MakeBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for MakeBuildModule {
    type Message = BuildModuleMessage;


    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "make".to_string(),
                    config_files: vec!["Makefile".to_string()],
                    build_system: "Make".to_string(),
                    language: "C".to_string(),
                    source_extensions: vec![
                        "c".to_string(),
                        "cpp".to_string(),
                        "h".to_string(),
                        "hpp".to_string(),
                    ],
                supports_clean: false,
                supports_lint: false,
                supports_format: false,
                supports_fix: false,
                mcp_servers: vec![],
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
                let result = parse_source_file_std(&std::path::PathBuf::from(&file_path), "C/C++");
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
                let bc = r#"$(CC) -std=c++17 src/main.cpp -o __P__
"#.replace("__P__", &project_name);
                let sc = r#"#include <iostream>
int main() {
    std::cout << "Hello from __P__!" << std::endl;
    return 0;
}
"#.replace("__P__", &project_name);
                let _ = reply_to.send(Ok(super::ScaffoldOutput {
                    build_file: "Makefile".to_string(),
                    build_content: bc,
                    source_dir: "src".to_string(),
                    source_file: "main.cpp".to_string(),
                    source_content: sc,
                    ..Default::default()
                }));
            }

            BuildModuleMessage::CallTool { reply_to, .. } => {
                let _ = reply_to
                    .send(serde_json::json!({ "error": "make module CallTool not yet wired" }));
            }
        }
    }
}
