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

    /// Remove common Python build artifacts (`build/`, `dist/`,
    /// `__pycache__/`, `.pytest_cache/`, `*.egg-info/`) in pure Rust — no
    /// shell globbing needed.
    async fn clean(&self, path: &Path, _metadata: &BuildMetadata) -> Result<BuildOutput, String> {
        let mut removed: Vec<String> = Vec::new();
        for entry in walkdir::WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() || entry.path() == path {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let is_artifact = name == "build"
                || name == "dist"
                || name == ".pytest_cache"
                || name == "__pycache__"
                || name.ends_with(".egg-info");
            if is_artifact {
                match std::fs::remove_dir_all(entry.path()) {
                    Ok(()) => removed.push(entry.path().to_string_lossy().to_string()),
                    Err(e) => removed.push(format!("{} (failed: {})", entry.path().display(), e)),
                }
            }
        }
        Ok(BuildOutput {
            success: true,
            command: "python clean (remove build artifacts)".to_string(),
            duration_secs: 0.0,
            output: if removed.is_empty() {
                "Nothing to clean".to_string()
            } else {
                removed.join("\n")
            },
            exit_code: Some(0),
        })
    }

    /// Run `ruff check .`, falling back to `flake8 .` when ruff is unavailable.
    async fn lint(
        &self,
        path: &Path,
        _metadata: &BuildMetadata,
        _platform: Option<&str>,
    ) -> Result<BuildOutput, String> {
        match run_cmd(path, "ruff", &["check", "."]).await {
            Ok(o) => Ok(o),
            Err(_) => run_cmd(path, "flake8", &["."]).await,
        }
    }

    /// Run `ruff format --check .` (non-destructive formatting check).
    async fn format(&self, path: &Path, _metadata: &BuildMetadata) -> Result<BuildOutput, String> {
        run_cmd(path, "ruff", &["format", "--check", "."]).await
    }

    /// Run `ruff check --fix .` to auto-fix lint issues.
    async fn fix(&self, path: &Path, _metadata: &BuildMetadata) -> Result<BuildOutput, String> {
        run_cmd(path, "ruff", &["check", "--fix", "."]).await
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
                supports_clean: true,
                supports_lint: true,
                supports_format: true,
                supports_fix: true,
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

            BuildModuleMessage::Clean {
                path,
                metadata,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.clean(&path, &metadata).await);
            }

            BuildModuleMessage::Lint {
                path,
                metadata,
                platform,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.lint(&path, &metadata, platform.as_deref()).await);
            }

            BuildModuleMessage::Format {
                path,
                metadata,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.format(&path, &metadata).await);
            }
            BuildModuleMessage::Fix {
                path,
                metadata,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.fix(&path, &metadata).await);
            }
            BuildModuleMessage::LintStreaming {
                path,
                metadata,
                platform,
                event_tx,
                reply_to,
                ..
            } => {
                // Batch lint + a synthetic finished event (no per-line streaming yet).
                let result = self.lint(&path, &metadata, platform.as_deref()).await;
                let _ = event_tx.send(super::BuildEvent {
                    line: format!("Finished lint {} in {:?}s", path.display(), result.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::FixStreaming {
                path,
                metadata,
                event_tx,
                reply_to,
                ..
            } => {
                // Batch fix + a synthetic finished event.
                let result = self.fix(&path, &metadata).await;
                let _ = event_tx.send(super::BuildEvent {
                    line: format!("Finished fix {} in {:?}s", path.display(), result.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
                let _ = reply_to.send(result);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `clean` removes common Python build artifacts but never source files.
    #[tokio::test]
    async fn clean_removes_artifacts_keeps_sources() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Artifacts to remove.
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::fs::create_dir_all(root.join(".pytest_cache")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("src").join("__pycache__")).unwrap();
        std::fs::create_dir_all(root.join("my_pkg.egg-info")).unwrap();
        // Real source that must survive.
        std::fs::write(root.join("src").join("main.py"), "print('hi')\n").unwrap();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname = 'x'\n").unwrap();

        let module = PythonBuildModule::new();
        let out = module.clean(root, &BuildMetadata::default()).await.unwrap();
        assert!(out.success, "clean should succeed");

        assert!(!root.join("build").exists());
        assert!(!root.join("dist").exists());
        assert!(!root.join(".pytest_cache").exists());
        assert!(!root.join("src").join("__pycache__").exists());
        assert!(!root.join("my_pkg.egg-info").exists());
        assert!(root.join("src").join("main.py").exists());
        assert!(root.join("pyproject.toml").exists());
    }

    /// Python module declares clean/lint/format/fix support.
    #[tokio::test]
    async fn capability_flags() {
        let system = spire_actor::ActorSystem::new();
        let (tx, _handle) = system.spawn(PythonBuildModule::new());
        let (r_tx, r_rx) = tokio::sync::oneshot::channel();
        tx.send(BuildModuleMessage::DescribeCapabilities { reply_to: r_tx })
            .await
            .unwrap();
        let cap = r_rx.await.unwrap();
        assert!(cap.supports_clean);
        assert!(cap.supports_lint);
        assert!(cap.supports_format);
        assert!(cap.supports_fix);
    }
}

