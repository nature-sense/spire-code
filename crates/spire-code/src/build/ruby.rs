// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Ruby build module — Gemfile analysis and build/test execution (via Bundler).

use async_trait::async_trait;
use std::path::Path;

use super::generic_helpers::parse_source_file_std;
use super::{
    BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability, TestOptions,
};

use super::generic_helpers::{parse_key_value, run_cmd};
use crate::Actor;
use spire_core::build_types::BuildMetadata;

/// Static Ruby build module.
pub struct RubyBuildModule;

impl RubyBuildModule {
    pub fn new() -> Self {
        Self
    }

    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let mut metadata = parse_key_value(path, "Gemfile", "")?;
        metadata.build_system = "Bundler".to_string();
        metadata.project_type = "Ruby_project".to_string();
        Ok(metadata)
    }

    async fn build(&self, path: &Path, _opts: &BuildOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "bundle", &["exec", "rake", "build"]).await
    }

    async fn test(&self, path: &Path, _opts: &TestOptions) -> Result<BuildOutput, String> {
        run_cmd(path, "bundle", &["exec", "rspec"]).await
    }
}

impl Default for RubyBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for RubyBuildModule {
    type Message = BuildModuleMessage;


    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "ruby".to_string(),
                    config_files: vec!["Gemfile".to_string()],
                    build_system: "Bundler".to_string(),
                    language: "Ruby".to_string(),
                    source_extensions: vec!["rb".to_string()],
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
                let result = parse_source_file_std(&std::path::PathBuf::from(&file_path), "Ruby");
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
                let bc = r#"source "https://rubygems.org"
gem "rake"
"#.replace("__P__", &project_name);
                let sc = r#"puts "Hello from __P__!"
"#.replace("__P__", &project_name);
                let _ = reply_to.send(Ok(super::ScaffoldOutput {
                    build_file: "Gemfile".to_string(),
                    build_content: bc,
                    source_dir: "".to_string(),
                    source_file: "main.rb".to_string(),
                    source_content: sc,
                    ..Default::default()
                }));
            }

            BuildModuleMessage::CallTool { reply_to, .. } => {
                let _ = reply_to
                    .send(serde_json::json!({ "error": "ruby module CallTool not yet wired" }));
            }
        }
    }
}
