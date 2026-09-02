// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Cargo build module — owns Rust/Cargo project analysis and build logic.
//!
//! This is the pilot module for the static build-module architecture. It is a
//! long-lived `ChildActor` spawned once at startup. The `BuildManagerActor`
//! routes requests to it via the shared `BuildModuleMessage` protocol.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;
use tokio::io::AsyncBufReadExt;

use super::generic_helpers::sha256_hex;
use super::{
    parse_with_tree_sitter, rust_language_config, AstParseResult, BuildModuleMessage,
    BuildOptions, BuildOutput, McpServerDependency, ModuleCapability, TestOptions,
};
use crate::Actor;

use spire_core::build_types::BuildMetadata;

/// Static Cargo build module.
pub struct CargoBuildModule;

impl CargoBuildModule {
    pub fn new() -> Self {
        Self
    }

    /// Parse a Rust source file into AST nodes using tree-sitter-rust.
    fn parse_source_file(&self, file_path: &Path) -> Result<AstParseResult, String> {
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read {}: {e}", file_path.display()))?;
        let content_hash = sha256_hex(&content);
        let config = rust_language_config();
        Ok(parse_with_tree_sitter(
            file_path,
            &content,
            &content_hash,
            &config,
        ))
    }

    /// Parse Cargo.toml → BuildMetadata.
    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let cargo_toml = path.join("Cargo.toml");
        if !cargo_toml.exists() {
            return Err(format!("No Cargo.toml found in {}", path.display()));
        }

        let content = std::fs::read_to_string(&cargo_toml)
            .map_err(|e| format!("Failed to read Cargo.toml: {e}"))?;

        // Resolve workspace-inherited dependency versions from the workspace root.
        let workspace_versions = find_workspace_versions(path);

        let mut name = None;
        let mut version = None;
        let mut description = None;
        let mut in_package = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[package]" {
                in_package = true;
                continue;
            }
            if in_package {
                if trimmed.starts_with('[') {
                    break;
                }
                if let Some(val) = trimmed.strip_prefix("name = ") {
                    name = Some(val.trim_matches('"').to_string());
                }
                if let Some(val) = trimmed.strip_prefix("version = ") {
                    version = Some(val.trim_matches('"').to_string());
                }
                if let Some(val) = trimmed.strip_prefix("description = ") {
                    description = Some(val.trim_matches('"').to_string());
                }
            }
        }

        // Parse [dependencies] and [dev-dependencies] sections.
        let mut dependencies: Vec<spire_core::build_types::Dependency> = Vec::new();
        let mut current_section: Option<String> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                current_section = Some(trimmed[1..trimmed.len() - 1].to_string());
                continue;
            }
            // Only look in dependency sections
            let section_name = current_section.as_deref().unwrap_or("");
            let in_deps = section_name == "dependencies"
                || section_name == "dev-dependencies"
                || section_name == "build-dependencies";
            if !in_deps {
                continue;
            }
            // Skip comments and empty lines
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // Format: name = "version" OR name = { version = "...", ... }
            // Also handle target-specific deps: [target.'cfg(...)'.dependencies]
            if let Some(eq) = trimmed.find('=') {
                let dep_name = trimmed[..eq].trim().to_string();
                if dep_name.is_empty() {
                    continue;
                }
                let rhs = trimmed[eq + 1..].trim();
                // Extract version from inline table: { version = "1.2", ... }
                //  or plain string: "1.2"
                let version_req = if rhs.starts_with('{') {
                    let version_here = rhs.find("version")
                        .and_then(|vi| {
                            let after = &rhs[vi..];
                            after.find('=').map(|ei| &after[ei + 1..])
                        })
                        .map(|v| v.trim().trim_start_matches(['"', '\'']))
                        .map(|v| {
                            let end = v
                                .find(['"', '\'', ',', '}'])
                                .unwrap_or(v.len());
                            v[..end].trim().to_string()
                        })
                        .filter(|s| !s.is_empty());
                    // Workspace-inherited dep: { workspace = true } — resolve
                    // version from the workspace root's [workspace.dependencies].
                    version_here.or_else(|| workspace_versions.get(&dep_name).cloned())
                } else {
                    rhs.trim_matches('"')
                        .trim_matches('\'')
                        .split_whitespace()
                        .next()
                        .map(|s| s.to_string())
                };
                dependencies.push(spire_core::build_types::Dependency {
                    name: dep_name,
                    version: version_req.clone(),
                    version_req,
                    kind: Some("cargo".to_string()),
                    source: Some("crates.io".to_string()),
                    source_url: None,
                    features: None,
                    ..Default::default()
                });
            }
        }

        let mut is_workspace = content.contains("[workspace]");
        let mut workspace_members: Vec<spire_core::build_types::WorkspaceMember> = Vec::new();
        let mut targets: Vec<spire_core::build_types::BuildTarget> = Vec::new();

        if is_workspace {
            // Parse [workspace] members (bare strings = relative dirs) into
            // WorkspaceMember entries so the graph/UI can list members. Also
            // reads each member's Cargo.toml to derive name + a lib/bin target.
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("members") {
                    // inline: members = ["a", "b"]
                    if let Some(eq) = trimmed.find('=') {
                        let rhs = &trimmed[eq + 1..];
                        for item in rhs.split(',') {
                            let item = item
                                .trim()
                                .trim_start_matches('[')
                                .trim_end_matches(']')
                                .trim_matches('"')
                                .trim();
                            if !item.is_empty() {
                                workspace_members.push(spire_core::build_types::WorkspaceMember {
                                    name: Path::new(item)
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_else(|| item.to_string()),
                                    path: item.to_string(),
                                    version: None,
                                });
                            }
                        }
                    }
                    continue;
                }
                if trimmed.starts_with('"') && trimmed.ends_with('"') && !trimmed.ends_with(']')
                {
                    // multi-line member list: "dir",
                    let item = trimmed.trim_matches('"').trim();
                    if !item.is_empty() && !item.contains('[') {
                        workspace_members.push(spire_core::build_types::WorkspaceMember {
                            name: Path::new(item)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| item.to_string()),
                            path: item.to_string(),
                            version: None,
                        });
                    }
                }
            }
        }

        let mut normalized_platforms: Vec<String> = Vec::new();

        // ── Legacy platform-workspace normalization ──
        // A Cargo WORKSPACE whose members are registered platforms (+ optional
        // shared `core`) is the OLD multi-platform scaffold: core/ + rpi5/ +
        // rock3c/ member crates each with a stub src/lib.rs. Conceptually it is
        // ONE project whose sources live in the shared crate, cross-compiled
        // for each platform. Normalize it to the single-project multi-target
        // model (is_workspace=false, no members, platform_targets + one
        // BuildTarget per platform) so the UI renders ONE project — never
        // per-platform subprojects with stub sources.
        if is_workspace && !workspace_members.is_empty() {
            let plat_dir = spire_core::build_types::Platform::default_platform_dir();
            let registry = spire_core::build_types::Platform::load_directory(&plat_dir)
                .ok()
                .unwrap_or_default();
            let registry_ids: Vec<String> = registry.iter().map(|p| p.id.clone()).collect();
            let platform_members: Vec<spire_core::build_types::WorkspaceMember> = workspace_members
                .iter()
                .filter(|m| registry_ids.iter().any(|id| id == &m.name))
                .cloned()
                .collect();
            let has_other = workspace_members
                .iter()
                .any(|m| m.name != "core" && !registry_ids.contains(&m.name));
            if !platform_members.is_empty() && !has_other {
                let core_root = workspace_members
                    .iter()
                    .find(|m| m.name == "core")
                    .map(|m| m.path.clone())
                    .unwrap_or_default();
                let source_root = if core_root.is_empty() {
                    "src".to_string()
                } else {
                    format!("{core_root}/src")
                };
                let mut sources: Vec<String> = Vec::new();
                if let Ok(entries) = std::fs::read_dir(path.join(&source_root)) {
                    for e in entries.flatten() {
                        if e.path().is_file() {
                            sources.push(e.file_name().to_string_lossy().to_string());
                        }
                    }
                }
                if name.is_none() {
                    name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string());
                }
                let mut per_platform: Vec<spire_core::build_types::BuildTarget> = Vec::new();
                for p in &platform_members {
                    normalized_platforms.push(p.name.clone());
                    let triple = registry
                        .iter()
                        .find(|r| r.id == p.name)
                        .map(|r| r.architecture.target_triple.clone());
                    per_platform.push(spire_core::build_types::BuildTarget {
                        name: p.name.clone(),
                        kind: vec!["lib".to_string()],
                        source_path: Some(source_root.clone()),
                        source_files: sources.clone(),
                        dependencies: Vec::new(),
                        platform: p.name.clone(),
                        source_kind: spire_core::build_types::SourceKind::Single,
                        source_units: vec![spire_core::build_types::SourceUnit {
                            role: spire_core::build_types::SourceRole::App,
                            path: source_root.clone(),
                            language: "Rust".to_string(),
                        }],
                        build_spec: triple.map(|t| spire_core::build_types::BuildSpec {
                            command: "cargo".to_string(),
                            arguments: vec!["build".to_string(), "--target".to_string(), t],
                            working_dir: String::new(),
                            env: Vec::new(),
                        }),
                    });
                }
                is_workspace = false;
                workspace_members.clear();
                targets = per_platform;
            }
        }

        // Targets: a crate with src/main.rs is a bin, src/lib.rs is a lib.
        // For a workspace, each member contributes its default target.
        for m in &workspace_members {
            let member_dir = path.join(&m.path);
            let has_lib = member_dir.join("src/lib.rs").exists();
            let has_bin = member_dir.join("src/main.rs").exists();
            let kind = if has_lib {
                "lib"
            } else if has_bin {
                "bin"
            } else {
                continue;
            };
            let mut source_files = Vec::new();
            if let Ok(entries) = std::fs::read_dir(member_dir.join("src")) {
                for e in entries.flatten() {
                    if e.path().is_file() {
                        source_files.push(e.file_name().to_string_lossy().to_string());
                    }
                }
            }
            targets.push(spire_core::build_types::BuildTarget {
                name: m.name.clone(),
                kind: vec![kind.to_string()],
                source_path: Some(format!("{}/src", m.path)),
                source_files,
                dependencies: Vec::new(),
                ..Default::default()
            });
        }
        // Non-workspace single crate: main.rs → bin, lib.rs → lib.
        let mut crate_sources: Vec<String> = Vec::new();
        if !is_workspace {
            let has_lib = path.join("src/lib.rs").exists();
            let has_bin = path.join("src/main.rs").exists();
            let kind = if has_lib {
                "lib"
            } else if has_bin {
                "bin"
            } else {
                ""
            };
            if !kind.is_empty() {
                if let Ok(entries) = std::fs::read_dir(path.join("src")) {
                    for e in entries.flatten() {
                        if e.path().is_file() {
                            crate_sources.push(e.file_name().to_string_lossy().to_string());
                        }
                    }
                }
                targets.push(spire_core::build_types::BuildTarget {
                    name: name.clone().unwrap_or_default(),
                    kind: vec![kind.to_string()],
                    source_path: Some("src".to_string()),
                    source_files: crate_sources.clone(),
                    dependencies: Vec::new(),
                    ..Default::default()
                });
            }
        }

        // Single-crate CROSS-COMPILE: when `.cargo/config.toml` carries
        // `[target.<triple>]` blocks, each target triple maps to ONE cross
        // platform (registry). Emit a build target per platform (name = platform
        // id, e.g. "rpi5") + the platform_targets list, so the graph renders
        // selectable build-target nodes exactly like Meson's per-platform
        // executables — one project, multiple targets.
        let mut cross_platform_targets: Vec<String> = Vec::new();
        if !is_workspace {
            let config_path = path.join(".cargo").join("config.toml");
            if let Ok(config) = std::fs::read_to_string(&config_path) {
                let plat_dir = spire_core::build_types::Platform::default_platform_dir();
                if let Ok(all) = spire_core::build_types::Platform::load_directory(&plat_dir) {
                    let mut per_platform: Vec<spire_core::build_types::BuildTarget> = Vec::new();
                    for line in config.lines() {
                        let trimmed = line.trim();
                        let Some(inner) = trimmed
                            .strip_prefix("[target.")
                            .and_then(|s| s.strip_suffix(']'))
                        else {
                            continue;
                        };
                        let triple = inner.trim();
                        // One `.cargo/config.toml` `[target.<triple>]` block can
                        // map to MULTIPLE registry platforms that share the same
                        // triple (e.g. rock3c + a7s both use
                        // aarch64-linux-gnu). Emit one build target per matching
                        // platform so no board variant is dropped by an
                        // arbitrary sort order.
                        for p in all
                            .iter()
                            .filter(|p| p.architecture.target_triple == triple)
                        {
                            if !cross_platform_targets.contains(&p.id) {
                                cross_platform_targets.push(p.id.clone());
                            }
                            per_platform.push(spire_core::build_types::BuildTarget {
                                name: p.id.clone(),
                                kind: vec!["lib".to_string()],
                                source_path: Some("src".to_string()),
                                source_files: crate_sources.clone(),
                                dependencies: Vec::new(),
                                platform: p.id.clone(),
                                source_kind: spire_core::build_types::SourceKind::Single,
                                source_units: vec![spire_core::build_types::SourceUnit {
                                    role: spire_core::build_types::SourceRole::App,
                                    path: "src".to_string(),
                                    language: "Rust".to_string(),
                                }],
                                build_spec: Some(spire_core::build_types::BuildSpec {
                                    command: "cargo".to_string(),
                                    arguments: vec![
                                        "build".to_string(),
                                        "--target".to_string(),
                                        p.architecture.target_triple.clone(),
                                    ],
                                    working_dir: String::new(),
                                    env: Vec::new(),
                                }),
                            });
                        }
                    }
                    if !per_platform.is_empty() {
                        targets = per_platform;
                    }
                }
            }
        }

        // Deduplicate deps (workspace deps may be listed alongside real ones).
        dependencies.sort_by(|a, b| a.name.cmp(&b.name));
        dependencies.dedup_by(|a, b| a.name == b.name && a.version == b.version);

        // SpireApp shape: a Cargo workspace that depends on the Spire framework
        // (spire-actor + spire-core in [workspace.dependencies]) and carries the
        // host SwiftUI companion app under ui/swift.
        let structure = if is_workspace
            && path.join("ui").join("swift").join("Package.swift").exists()
            && content.contains("spire-actor")
            && content.contains("spire-core")
        {
            spire_core::build_types::ProjectStructure::SpireApp
        } else {
            spire_core::build_types::ProjectStructure::default()
        };

        // Named slices for the SpireApp shape: `core` (the Rust crate) and `ui`
        // (the SwiftUI app). Sibling domains, not platform×common like HAL.
        let domains: Vec<spire_core::build_types::ProjectDomain> =
            if structure == spire_core::build_types::ProjectStructure::SpireApp {
                vec![
                    spire_core::build_types::ProjectDomain {
                        id: "core".to_string(),
                        name: "Core".to_string(),
                        kind: "common".to_string(),
                        files: vec!["crates".to_string()],
                        dependencies: Vec::new(),
                        build_spec: Some(spire_core::build_types::BuildSpec {
                            command: "cargo".to_string(),
                            arguments: vec!["build".to_string()],
                            working_dir: String::new(),
                            env: Vec::new(),
                        }),
                        editability: spire_core::build_types::DomainEditability::Fillable,
                        contracts: Vec::new(),
                    },
                    spire_core::build_types::ProjectDomain {
                        id: "ui".to_string(),
                        name: "UI".to_string(),
                        kind: "common".to_string(),
                        files: vec!["ui/swift".to_string()],
                        dependencies: Vec::new(),
                        build_spec: Some(spire_core::build_types::BuildSpec {
                            command: "swift".to_string(),
                            arguments: vec!["build".to_string()],
                            working_dir: "ui/swift".to_string(),
                            env: Vec::new(),
                        }),
                        editability: spire_core::build_types::DomainEditability::Fillable,
                        contracts: Vec::new(),
                    },
                ]
            } else {
                Vec::new()
            };

        Ok(BuildMetadata {
            project_name: name,
            description,
            version,
            project_type: if structure
                == spire_core::build_types::ProjectStructure::SpireApp
            {
                "spire_app".to_string()
            } else if is_workspace {
                "rust_workspace".to_string()
            } else {
                "rust_crate".to_string()
            },
            build_system: "Cargo".to_string(),
            is_workspace,
            workspace_members,
            targets,
            platform_targets: if !normalized_platforms.is_empty() {
                normalized_platforms
            } else {
                cross_platform_targets
            },
            config_files: vec!["Cargo.toml".to_string()],
            project_path: Some(path.to_string_lossy().to_string()),
            dependencies,
            structure,
            domains,
            ..Default::default()
        })
    }

    /// Resolve the cargo `--target` triple for a registered platform, if any.
    fn target_arg(&self, platform: Option<&str>) -> Option<String> {
        platform
            .and_then(spire_core::platform::CrossSpec::for_platform)
            .map(|c| c.target_triple)
    }

    /// Write `<project>/.cargo/config.toml` from the platform registry so a
    /// pure-Rust project cross-compiles (linker + sysroot + pkg-config) the
    /// same way Meson does. No-op for host builds / unknown platforms.
    fn write_cargo_config(&self, path: &Path, platform: Option<&str>) -> Result<(), String> {
        let Some(plat) = platform else { return Ok(()) };
        let Some(spec) = spire_core::platform::CrossSpec::for_platform(plat) else {
            return Ok(());
        };
        let Some(toml) = spec.cargo_config else { return Ok(()); };
        // Sanity gate: fail fast when the target's sysroot is missing or
        // unpopulated instead of writing a `.cargo/config.toml` whose
        // `--sysroot` points at a nonexistent directory.
        if let Some(platform_def) = spire_core::build_types::Platform::from_registry(plat) {
            let (ok, reason) = platform_def.sysroot_ok();
            if !ok {
                return Err(format!(
                    "platform '{plat}' cross-build blocked: {reason}. \
                     Populate the target sysroot (e.g. {}/usr) or fix the \
                     platform registry seed before cross-compiling.",
                    platform_def.sysroot.root
                ));
            }
        }
        let dir = path.join(".cargo");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create .cargo: {e}"))?;
        std::fs::write(dir.join("config.toml"), toml)
            .map_err(|e| format!("Failed to write .cargo/config.toml: {e}"))
    }

    /// Run `cargo build` with the given options (+ platform --target).
    async fn build(&self, path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
        self.write_cargo_config(path, opts.platform.as_deref())?;
        let mut args = vec!["build".to_string()];
        if opts.mode == "release" {
            args.push("--release".to_string());
        }
        if let Some(t) = self.target_arg(opts.platform.as_deref()) {
            args.push("--target".to_string());
            args.push(t);
        }
        self.run_cargo(path, &args).await
    }

    /// Run `cargo test` with the given options.
    async fn test(&self, path: &Path, opts: &TestOptions) -> Result<BuildOutput, String> {
        let mut args = vec!["test".to_string()];
        if let Some(filter) = &opts.filter {
            args.push(filter.clone());
        }
        self.run_cargo(path, &args).await
    }

    /// Run `cargo clean`.
    async fn clean(&self, path: &Path) -> Result<BuildOutput, String> {
        self.run_cargo(path, &["clean".to_string()]).await
    }

    /// Run `cargo clippy -- -D warnings`.
    async fn lint(&self, path: &Path) -> Result<BuildOutput, String> {
        self.run_cargo(
            path,
            &[
                "clippy".to_string(),
                "--".to_string(),
                "-D".to_string(),
                "warnings".to_string(),
            ],
        )
        .await
    }

    /// Run `cargo fmt --check`.
    async fn format(&self, path: &Path) -> Result<BuildOutput, String> {
        self.run_cargo(path, &["fmt".to_string(), "--check".to_string()])
            .await
    }

    /// Run `cargo fix --allow-dirty --allow-staged` then `cargo clippy --fix`
    /// to auto-fix as many compiler and clippy warnings as possible.
    async fn fix(&self, path: &Path) -> Result<BuildOutput, String> {
        // Pass 1: compiler-suggested fixes.
        let mut combined = self
            .run_cargo(
                path,
                &[
                    "fix".to_string(),
                    "--allow-dirty".to_string(),
                    "--allow-staged".to_string(),
                ],
            )
            .await?;

        // Pass 2: clippy lint fixes (broader coverage).
        match self
            .run_cargo(
                path,
                &[
                    "clippy".to_string(),
                    "--fix".to_string(),
                    "--allow-dirty".to_string(),
                    "--allow-staged".to_string(),
                ],
            )
            .await
        {
            Ok(second) => {
                combined.output = format!("{}\n{}", combined.output, second.output);
                combined.success = combined.success && second.success;
                combined.duration_secs += second.duration_secs;
            }
            Err(e) => {
                combined.output = format!("{}\nclippy --fix failed: {}", combined.output, e);
            }
        }
        Ok(combined)
    }

    /// Execute `cargo` and stream each output line as a BuildEvent.
    async fn run_cargo_streaming(
        &self,
        path: &Path,
        args: &[String],
        event_tx: &tokio::sync::mpsc::UnboundedSender<super::BuildEvent>,
        is_lint: bool,
    ) -> Result<BuildOutput, String> {
        let start = Instant::now();
        let command_str = format!("cargo {}", args.join(" "));

        let mut cmd = Command::new("cargo");
        cmd.current_dir(path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn cargo: {e}"))?;

        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;

        // Stream stdout line-by-line through the stateful build line parser,
        // which aggregates multi-line warning/error blocks into single events.
        let mut out_reader = tokio::io::BufReader::new(stdout).lines();
        let mut all_output = String::new();
        let event_tx_stdout = event_tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut parser = BuildLineParser::new(is_lint);
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
            let mut parser = BuildLineParser::new(is_lint);
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

        let status = child.wait().await.map_err(|e| format!("cargo wait failed: {e}"))?;
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

    /// Execute `cargo` and capture structured output.
    async fn run_cargo(&self, path: &Path, args: &[String]) -> Result<BuildOutput, String> {
        let start = Instant::now();
        let command_str = format!("cargo {}", args.join(" "));

        let mut cmd = Command::new("cargo");
        cmd.current_dir(path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to execute cargo: {e}"))?;

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

/// Walk up from a Cargo crate directory to find the workspace root and parse
/// `[workspace.dependencies]` into a name → version lookup map.
///
/// This resolves versions for `{ workspace = true }` inherited dependencies.
fn find_workspace_versions(path: &Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut dir = Some(path);
    while let Some(d) = dir {
        let candidate = d.join("Cargo.toml");
        if let Ok(content) = std::fs::read_to_string(&candidate) {
            if content.contains("[workspace]") {
                // Found the workspace root — parse [workspace.dependencies].
                let mut in_ws_deps = false;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('[') && trimmed.ends_with(']') {
                        let section = &trimmed[1..trimmed.len() - 1];
                        in_ws_deps = section == "workspace.dependencies";
                        continue;
                    }
                    if !in_ws_deps {
                        continue;
                    }
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue;
                    }
                    if let Some(eq) = trimmed.find('=') {
                        let dep_name = trimmed[..eq].trim().to_string();
                        if dep_name.is_empty() {
                            continue;
                        }
                        let rhs = trimmed[eq + 1..].trim();
                        let version = if rhs.starts_with('{') {
                            rhs.find("version")
                                .and_then(|vi| {
                                    let after = &rhs[vi..];
                                    after.find('=').map(|ei| &after[ei + 1..])
                                })
                                .map(|v| v.trim().trim_start_matches(['"', '\'']))
                                .map(|v| {
                                    let end = v
                                        .find(['"', '\'', ',', '}'])
                                        .unwrap_or(v.len());
                                    v[..end].trim().to_string()
                                })
                                .filter(|s| !s.is_empty())
                        } else {
                            rhs.trim_matches('"')
                                .trim_matches('\'')
                                .split_whitespace()
                                .next()
                                .map(|s| s.to_string())
                        };
                        if let Some(v) = version {
                            map.insert(dep_name, v);
                        }
                    }
                }
                break;
            }
        }
        dir = d.parent();
    }
    map
}

/// Construct cargo build args from BuildOptions, including the platform
/// `--target <triple>` when the selected platform resolves in the registry.
fn build_args(opts: &BuildOptions) -> Vec<String> {
    let mut args = vec!["build".to_string()];
    if opts.mode == "release" {
        args.push("--release".to_string());
    }
    // Target a specific workspace member (e.g. `cargo build --package spire-ffi`)
    // so building a subproject in a workspace doesn't always build the whole workspace.
    if let Some(pkg) = &opts.package {
        if !pkg.is_empty() {
            args.push("--package".to_string());
            args.push(pkg.clone());
        }
    }
    if let Some(triple) = opts
        .platform
        .as_deref()
        .and_then(spire_core::platform::CrossSpec::for_platform)
        .map(|c| c.target_triple)
    {
        args.push("--target".to_string());
        args.push(triple);
    }
    args
}

/// Classify a build-tool stdout/stderr line into (level, target name).
/// Stateful parser that aggregates multi-line warning/error blocks into
/// single structured BuildEvents. Shared by the Cargo and Swift modules.
/// A block looks like:
///
///   warning: unused variable `x`
///     --> src/main.rs:10:9
///      |
///   10 |     let x = 42;
///      |         ^ help: ...
///
/// The parser emits one BuildEvent {level:"warning"/"error", message, file,
/// line_number, detail} per block, and passes through ordinary lines
/// ("Compiling X", "Finished", etc.) individually.
pub(crate) struct BuildLineParser {
    /// When true, "error:" lines from a linter (e.g. clippy -- -D warnings) are
    /// treated as warnings — they're lint findings, not compilation failures.
    is_lint: bool,
    /// Current block kind: "warning", "error", or None (not in a block).
    block_level: Option<&'static str>,
    /// Accumulated raw lines of the current block.
    block_lines: Vec<String>,
    /// Message extracted from the first block line.
    block_message: Option<String>,
    /// File:line parsed from the "  -->" line.
    block_file: Option<String>,
    block_line_number: Option<u32>,
}

impl BuildLineParser {
    pub(crate) fn new(is_lint: bool) -> Self {
        Self {
            is_lint,
            block_level: None,
            block_lines: Vec::new(),
            block_message: None,
            block_file: None,
            block_line_number: None,
        }
    }
    /// Feed one output line; returns zero or more BuildEvents to emit.
    pub(crate) fn feed(&mut self, line: &str, _is_stdout: bool) -> Vec<super::BuildEvent> {
        let trimmed = line.trim_start();

        // Start of a warning block: "warning: msg"
        if let Some(msg) = trimmed.strip_prefix("warning:") {
            let ev = self.flush();
            self.block_level = Some("warning");
            self.block_message = Some(msg.trim().to_string());
            self.block_lines.push(line.to_string());
            return ev;
        }
        // Start of an error block: "error: msg" or "error[E####]: msg"
        if let Some(rest) = trimmed.strip_prefix("error") {
            let msg = rest.trim_start_matches([':', '[', ']']).trim().to_string();
            let ev = self.flush();
            // Lint drivers promote warnings to errors with `-D warnings`;
            // render those as warnings in the UI.
            self.block_level = if self.is_lint { Some("warning") } else { Some("error") };
            self.block_message = Some(msg);
            self.block_lines.push(line.to_string());
            return ev;
        }

        // Inside a warning/error block.
        if self.block_level.is_some() {
            // Location line: "  --> path:line:col"
            if let Some(path_part) = trimmed.strip_prefix("-->") {
                let loc = path_part.trim();
                self.block_file = loc.split(':').next().map(|s| s.to_string());
                self.block_line_number = loc.split(':').nth(1).and_then(|ln| ln.trim().parse::<u32>().ok());
            }
            // Code-context / continuation lines belong to the block.
            let is_context = trimmed.contains("-->")
                || trimmed.contains('|')
                || trimmed.starts_with('^')
                || trimmed.starts_with('=')
                || line.trim().is_empty();
            if is_context {
                self.block_lines.push(line.to_string());
                return Vec::new();
            }
            // A non-context line ends the block; classify this line on its own.
            let mut out = self.flush();
            out.push(self.classify_plain(line));
            return out;
        }

        // Ordinary line (not inside a block).
        vec![self.classify_plain(line)]
    }

    /// Emit the aggregated block event (if any) and reset block state.
    fn flush(&mut self) -> Vec<super::BuildEvent> {
        if self.block_level.is_none() {
            return Vec::new();
        }
        let level = self.block_level.take().unwrap().to_string();
        let message = self.block_message.take();
        let file = self.block_file.take();
        let line_number = self.block_line_number.take();
        let raw = std::mem::take(&mut self.block_lines).join("\n");
        vec![super::BuildEvent {
            line: raw,
            level,
            target: None,
            file,
            line_number,
            message,
            detail: None,
        }]
    }

    /// Called at end of stream to emit any trailing block.
    pub(crate) fn finish(&mut self, _is_stdout: bool) -> Vec<super::BuildEvent> {
        self.flush()
    }

    /// Classify an ordinary (non-block) line.
    fn classify_plain(&self, line: &str) -> super::BuildEvent {
        let trimmed = line.trim_start();
        let (level, target) = if let Some(rest) = trimmed.strip_prefix("Compiling") {
            let name = rest.split_whitespace().next().map(|s| s.to_string());
            ("compiling", name)
        } else if trimmed.starts_with("Finished") {
            ("finished", None)
        } else {
            ("info", None)
        };
        super::BuildEvent {
            line: line.to_string(),
            level: level.to_string(),
            target,
            file: None,
            line_number: None,
            message: None,
            detail: None,
        }
    }
}

impl Default for CargoBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for CargoBuildModule {
    type Message = BuildModuleMessage;


    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "cargo".to_string(),
                    config_files: vec!["Cargo.toml".to_string()],
                    build_system: "Cargo".to_string(),
                    language: "Rust".to_string(),
                    source_extensions: vec!["rs".to_string()],
                    supports_clean: true,
                    supports_lint: true,
                    supports_format: true,
                    supports_fix: true,
                    mcp_servers: vec![McpServerDependency {
                        // In-process tool namespace: the LLM emits
                        // tool_call { server_name: "build/cargo", ... } and
                        // execute_step routes it via BuildManager directly.
                        // autostart=false — no external server is spawned.
                        name: "build/cargo".to_string(),
                        package: String::new(),
                        install_command: String::new(),
                        command: String::new(),
                        args: vec![],
                        autostart: false,
                        build_type: Some("Cargo".to_string()),
                        // Only these tools are exposed to the LLM for plan
                        // generation/execution — controlled, whitelisted access.
                        allowed_for_steps: vec![
                            "discover_dependencies".to_string(),
                            "audit_security".to_string(),
                        ],
                        allowed_tools: vec![
                            "search_crates".to_string(),
                            "get_crate_info".to_string(),
                            "get_dependencies".to_string(),
                            "audit_dependencies".to_string(),
                            "get_crate_versions".to_string(),
                        ],
                    }],
                });
            }

            BuildModuleMessage::ParseSourceFile {
                file_path,
                reply_to,
            } => {
                let result = self.parse_source_file(&PathBuf::from(&file_path));
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Analyze { path, reply_to } => {
                let result = self.analyze(&path);
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Build {
                path,
                metadata: _metadata,
                opts,
                build_spec,
                reply_to,
            } => {
                // A normalized `build_spec` (from the selected target) is
                // executed directly — command + args + env, cwd = module root
                // (or the spec's working_dir). Otherwise fall back to the
                // existing per-tool logic.
                let result = match build_spec {
                    Some(spec) => super::generic_helpers::run_build_spec(&path, &spec).await,
                    None => self.build(&path, &opts).await,
                };
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::BuildStreaming {
                path,
                metadata: _metadata,
                opts,
                build_spec,
                event_tx,
                reply_to,
            } => {
                let result = match build_spec {
                    Some(spec) => {
                        let out = super::generic_helpers::run_build_spec(&path, &spec).await;
                        // Emit a single synthetic finished event for streaming
                        // consumers (mirrors the Meson module's fallback).
                        let _ = event_tx.send(super::BuildEvent {
                            line: format!(
                                "Finished {} in {:?}s",
                                path.display(),
                                out.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)
                            ),
                            level: "finished".to_string(),
                            target: None,
                            file: None,
                            line_number: None,
                            message: None,
                            detail: None,
                        });
                        out
                    }
                    None => {
                        let _ = self.write_cargo_config(&path, opts.platform.as_deref());
                        self.run_cargo_streaming(
                            &path,
                            &build_args(&opts),
                            &event_tx,
                            false,
                        )
                        .await
                    }
                };
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::LintStreaming {
                path,
                metadata: _metadata,
                platform,
                event_tx,
                reply_to,
            } => {
                let _ = self.write_cargo_config(&path, platform.as_deref());
                let mut args = vec![
                    "clippy".to_string(),
                    "--".to_string(),
                    "-D".to_string(),
                    "warnings".to_string(),
                ];
                if let Some(triple) = platform
                    .as_deref()
                    .and_then(spire_core::platform::CrossSpec::for_platform)
                    .map(|c| c.target_triple)
                {
                    args.push("--target".to_string());
                    args.push(triple);
                }
                let result = self
                    .run_cargo_streaming(&path, &args, &event_tx, true)
                    .await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::FixStreaming {
                path,
                metadata: _metadata,
                event_tx,
                reply_to,
            } => {
                // Pass 1: compiler-suggested fixes (`cargo fix`). These fix
                // rustc-recommended issues (unused imports, etc.).
                let _ = self
                    .run_cargo_streaming(
                        &path,
                        &[
                            "fix".to_string(),
                            "--allow-dirty".to_string(),
                            "--allow-staged".to_string(),
                        ],
                        &event_tx,
                        false,
                    )
                    .await;
                // Pass 2: clippy auto-fixes. Most actionable warnings come from
                // clippy lints, so this pass is essential for "Fix Warnings" to
                // actually do anything. `is_lint=true` renders clippy errors
                // (promoted via -D warnings) as warnings in the UI.
                let result = self
                    .run_cargo_streaming(
                        &path,
                        &[
                            "clippy".to_string(),
                            "--fix".to_string(),
                            "--allow-dirty".to_string(),
                            "--allow-staged".to_string(),
                        ],
                        &event_tx,
                        true,
                    )
                    .await;
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

            BuildModuleMessage::Clean {
                path,
                metadata: _metadata,
                reply_to,
            } => {
                let result = self.clean(&path).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Lint {
                path,
                metadata: _metadata,
                platform: _platform,
                reply_to,
            } => {
                let result = self.lint(&path).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Format {
                path,
                metadata: _metadata,
                reply_to,
            } => {
                let result = self.format(&path).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Fix {
                path,
                metadata: _metadata,
                reply_to,
            } => {
                let result = self.fix(&path).await;
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::ScaffoldBuildConfig {
                project_name,
                goal,
                platforms,
                structure,
                embedded: _,
                reply_to,
            } => {
                let result = self.scaffold_layout(&project_name, &goal, &platforms, structure);
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::CallTool {
                tool_name,
                args,
                reply_to,
            } => {
                let result = self.call_tool_async(&tool_name, &args).await;
                match result {
                    Ok(v) => {
                        let _ = reply_to.send(serde_json::json!({
                            "result": {"content": [{"type":"text","text":serde_json::to_string_pretty(&v).unwrap_or_default()}], "isError": false}
                        }));
                    }
                    Err(e) => {
                        let _ = reply_to.send(serde_json::json!({
                            "result": {"content": [{"type":"text","text":e}], "isError": true}
                        }));
                    }
                }
            }
        }
    }
}

impl CargoBuildModule {
    /// Scaffold a Cargo project, optionally as a multi-platform workspace.
    ///
    /// `platforms` uses registry ids. When the list contains any id other than
    /// `"host"`, a Cargo workspace is emitted (Option A / the "Spire way"): a
    /// shared `core` lib plus one ROOT-LEVEL per-platform leaf crate
    /// (`rpi5/`, `rock3c/`, …) — members are never nested under a `crates/`
    /// dir (which would collide with Spire's own source tree). Each leaf has
    /// its own `src/` and a `build.rs` emitting native link-search directives
    /// from the platform registry's sysroot. Shared dependencies live ONCE in
    /// the root `[workspace.dependencies]` (the single fillable dependency
    /// section). Plus `.cargo/config.toml` carrying `[target.<triple>]` blocks
    /// for every non-host platform (via `Platform::cargo_config()`). No host
    /// binary is generated — the workspace is device-library only.
    /// Otherwise the legacy single-binary scaffold is produced unchanged.
    fn scaffold_layout(
        &self,
        project_name: &str,
        _goal: &str,
        platforms: &[String],
        structure: spire_core::build_types::ProjectStructure,
    ) -> Result<super::ScaffoldOutput, String> {
        // SpireApp: Rust/SwiftUI monorepo built on the Spire framework.
        if structure == spire_core::build_types::ProjectStructure::SpireApp {
            return Ok(super::spire_app_scaffold::spire_app_scaffold(project_name));
        }
        let cross: Vec<&String> = platforms.iter().filter(|p| *p != "host").collect();
        if cross.is_empty() {
            // Legacy single-binary scaffold.
            let bc = r#"[package]
name = "__P__"
version = "0.1.0"
edition = "2021"

[dependencies]
"#.replace("__P__", project_name);
            let sc = r#"fn main() {
    println!("Hello from __P__!");
}
"#.replace("__P__", project_name);
            return Ok(super::ScaffoldOutput {
                build_file: "Cargo.toml".to_string(),
                build_content: bc,
                source_dir: "src".to_string(),
                source_file: "main.rs".to_string(),
                source_content: sc,
                files: Vec::new(),
                platform_targets: platforms.to_vec(),
                structure,
                ..Default::default()
            });
        }

        // Multi-platform CROSS-COMPILE of a SINGLE crate (the correct model for
        // "one set of sources, different cross-compiler settings per target"):
        // one `[package]` + one `[dependencies]`, a single `src/lib.rs`, and
        // `.cargo/config.toml` carrying `[target.<triple>]` linker/sysroot
        // settings per platform. NO workspace, NO member crates — the platforms
        // are build targets (like Meson's executable('...-rpi5')), not separate
        // subprojects. Per-target native link-search is handled by cfg-gated
        // `build.rs` output (CARGO_CFG_TARGET_ARCH) rather than one crate per
        // platform.
        let mut files = Vec::new();

        files.push(super::ScaffoldFile {
            path: "Cargo.toml".to_string(),
            content: format!(
                "[package]\nname = \"{project_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
            ),
            structural: true,
            ..Default::default()
        });

        // Single entry point — shared by every cross target. Binary (main.rs),
        // not a lib stub: an MCP server is an executable crate.
        files.push(super::ScaffoldFile {
            path: "src/main.rs".to_string(),
            content: format!(
                "fn main() {{\n    println!(\"Hello from {project_name}!\");\n}}\n"
            ),
            // Source stub — the LLM may fill it.
            structural: false,
            ..Default::default()
        });

        // build.rs emits native link-search for the ACTIVE cross target, keyed
        // on CARGO_CFG_TARGET_ARCH so one crate serves all platforms. Link-lib
        // names are intentionally empty for now (filled by the LLM/user).
        let mut build_rs = String::new();
        for plat in &cross {
            if let Some(platform) = spire_core::build_types::Platform::from_registry(plat) {
                if !platform.sysroot.lib_dirs.is_empty() {
                    build_rs.push_str(&format!(
                        "if env::var(\"CARGO_CFG_TARGET_ARCH\").ok().as_deref() == Some(\"{arch}\") {{\n",
                        arch = platform.architecture.cpu_family
                    ));
                    for dir in &platform.sysroot.lib_dirs {
                        build_rs.push_str(&format!(
                            "    println!(\"cargo:rustc-link-search={dir}\");\n"
                        ));
                    }
                    build_rs.push_str("}\n");
                }
            }
        }
        build_rs.insert_str(
            0,
            "use std::env;\n\n",
        );
        build_rs.push_str("// TODO: add cargo:rustc-link-lib=... for device libraries\n");
        files.push(super::ScaffoldFile {
            path: "build.rs".to_string(),
            content: build_rs,
            structural: true,
            ..Default::default()
        });

        // .cargo/config.toml with [target.<triple>] blocks for every cross platform.
        let mut cargo_config = String::new();
        for plat in &cross {
            if let Some(platform) = spire_core::build_types::Platform::from_registry(plat) {
                if let Some(toml) = platform.cargo_config() {
                    cargo_config.push_str(&toml);
                    cargo_config.push('\n');
                }
            }
        }
        files.push(super::ScaffoldFile {
            path: ".cargo/config.toml".to_string(),
            content: cargo_config,
            structural: true,
            ..Default::default()
        });

        // Fill roots: the single src/ dir. The LLM may add/modify source files
        // and subdirectories under it, but never under a structural file's path.
        let fill_roots = vec!["src".to_string()];
        // Dependency section: the single manifest's [dependencies] — one source
        // of truth, shared by every build target.
        let dependency_sections = vec!["Cargo.toml".to_string()];

        Ok(super::ScaffoldOutput {
            build_file: "Cargo.toml".to_string(),
            build_content: files
                .iter()
                .find(|f| f.path == "Cargo.toml")
                .map(|f| f.content.clone())
                .unwrap_or_default(),
            source_dir: "src".to_string(),
            source_file: "main.rs".to_string(),
            source_content: files
                .iter()
                .find(|f| f.path == "src/main.rs")
                .map(|f| f.content.clone())
                .unwrap_or_default(),
            files,
            platform_targets: platforms.to_vec(),
            fill_roots,
            dependency_sections,
            structure,
            ..Default::default()
        })
    }

    /// Async generic tool dispatch used by `BuildModuleMessage::CallTool`.
    /// Routes project-level operations (build/clean/lint/format/test/check/add
    /// dependency/local deps) to the same underlying builders, plus the
    /// crates.io REST tools via [`Self::call_tool`].
    async fn call_tool_async(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let get = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        match tool_name {
            "build" => {
                let path = get("path");
                let opts = BuildOptions {
                    mode: get("mode"),
                    package: args
                        .get("package")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    platform: args
                        .get("platform")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    target: args
                        .get("target")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                };
                let out = self.build(Path::new(&path), &opts).await?;
                Ok(serde_json::to_value(out).unwrap_or(serde_json::json!({"error": "serialize"})))
            }
            "clean" => {
                let path = get("path");
                let out = self.clean(Path::new(&path)).await?;
                Ok(serde_json::to_value(out).unwrap_or(serde_json::json!({"error": "serialize"})))
            }
            "clippy" | "lint" => {
                let path = get("path");
                let out = self.lint(Path::new(&path)).await?;
                Ok(serde_json::to_value(out).unwrap_or(serde_json::json!({"error": "serialize"})))
            }
            "fmt" | "format" => {
                let path = get("path");
                let out = self.format(Path::new(&path)).await?;
                Ok(serde_json::to_value(out).unwrap_or(serde_json::json!({"error": "serialize"})))
            }
            "test" => {
                let path = get("path");
                let opts = TestOptions {
                    filter: args
                        .get("filter")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                };
                let out = self.test(Path::new(&path), &opts).await?;
                Ok(serde_json::to_value(out).unwrap_or(serde_json::json!({"error": "serialize"})))
            }
            "check" => {
                let path = get("path");
                let out = self.run_cargo(Path::new(&path), &["check".to_string()]).await?;
                Ok(serde_json::to_value(out).unwrap_or(serde_json::json!({"error": "serialize"})))
            }
            "add_dependency" | "declare_dependencies" => {
                // `declare_dependencies` is the structural-guard tool used
                // during the fill phase: the LLM may add dependencies ONLY via
                // this tool (runs `cargo add`), never by editing Cargo.toml
                // directly (Cargo.toml is structural/locked).
                //
                // `path` may be a directory containing Cargo.toml, a direct
                // path to a Cargo.toml manifest, or the workspace-root manifest
                // (the multi-platform scaffold's SINGLE fillable dependency
                // section). Normalize it to the manifest path so
                // `cargo add --manifest-path` targets the correct manifest, and
                // add `--workspace` when that manifest declares `[workspace]`
                // so shared dependencies land in `[workspace.dependencies]`
                // exactly once — never duplicated across member manifests.
                let raw_path = get("path");
                let mut manifest_path = PathBuf::from(&raw_path);
                if !raw_path.ends_with("Cargo.toml") {
                    manifest_path = manifest_path.join("Cargo.toml");
                }
                let is_workspace_root = std::fs::read_to_string(&manifest_path)
                    .map(|c| c.contains("[workspace]"))
                    .unwrap_or(false);
                let mut added = Vec::new();
                let deps: Vec<serde_json::Value> = if let Some(arr) =
                    args.get("dependencies").and_then(|v| v.as_array())
                {
                    arr.clone()
                } else {
                    args.get("crate")
                        .and_then(|v| v.as_str())
                        .map(|s| serde_json::json!({ "name": s }))
                        .into_iter()
                        .collect()
                };
                if deps.is_empty() {
                    return Err("declare_dependencies: 'dependencies' (array of {name,version,features}) is required".to_string());
                }
                let mut last: Option<BuildOutput> = None;
                for dep in deps {
                    let crate_name = dep
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if crate_name.is_empty() {
                        continue;
                    }
                    let mut args_vec = vec!["add".to_string(), crate_name.clone()];
                    args_vec.push("--manifest-path".to_string());
                    args_vec.push(manifest_path.to_string_lossy().to_string());
                    if is_workspace_root {
                        // Add to [workspace.dependencies] (single source of
                        // truth) — members inherit via `{ workspace = true }`.
                        args_vec.push("--workspace".to_string());
                    }
                    let version = dep
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !version.is_empty() {
                        args_vec.push(version);
                    }
                    if let Some(feats) = dep.get("features").and_then(|v| v.as_array()) {
                        let feats_str: Vec<String> = feats
                            .iter()
                            .filter_map(|f| f.as_str().map(|s| s.to_string()))
                            .collect();
                        if !feats_str.is_empty() {
                            args_vec.push("--features".to_string());
                            args_vec.push(feats_str.join(","));
                        }
                    }
                    let out = self
                        .run_cargo(
                            manifest_path.parent().unwrap_or(Path::new(".")),
                            &args_vec,
                        )
                        .await?;
                    if !out.success {
                        return Err(format!("cargo add {crate_name} failed: {}", out.output));
                    }
                    added.push(crate_name);
                    last = Some(out);
                }
                Ok(serde_json::json!({
                    "added": added,
                    "result": last.map(|o| serde_json::to_value(o).unwrap_or_default()).unwrap_or(serde_json::json!({}))
                }))
            }
            "get_dependencies" => {
                // Local dependency extraction from the project's Cargo.toml
                // (analogous to the legacy project_install/project_sync paths).
                let path = get("path");
                if path.is_empty() {
                    return Err("get_dependencies: 'path' is required".to_string());
                }
                let metadata = self.analyze(Path::new(&path))?;
                let deps: Vec<serde_json::Value> = metadata
                    .dependencies
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "name": d.name,
                            "version": d.version,
                            "version_req": d.version_req,
                            "kind": d.kind,
                            "source": d.source,
                        })
                    })
                    .collect();
                Ok(serde_json::json!({ "dependencies": deps }))
            }
            _ => Self::call_tool(tool_name, args),
        }
    }

    /// Generic in-process tool dispatch: crates.io REST API tools.
    /// These are exposed to the LLM through the BuildManager's tool list
    /// and executed directly (no external MCP process needed).
    fn call_tool(tool_name: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
        const UA: &str = "spire-crates-mcp/0.1 (spire)";
        let get = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

        let run = |url: &str| -> Result<serde_json::Value, String> {
            reqwest::blocking::Client::builder()
                .user_agent(UA)
                .build()
                .map_err(|e| e.to_string())?
                .get(url)
                .send()
                .map_err(|e| e.to_string())?
                .json::<serde_json::Value>()
                .map_err(|e| e.to_string())
        };

        let result: Result<serde_json::Value, String> = match tool_name {
            "search_crates" => {
                let q = get("query");
                run(&format!("https://crates.io/api/v1/crates?q={}&per_page=5", q))
            }
            "get_crate_info" => {
                let n = get("name");
                run(&format!("https://crates.io/api/v1/crates/{}", n))
            }
            "get_crate_versions" => {
                let n = get("name");
                run(&format!("https://crates.io/api/v1/crates/{}/versions", n))
            }
            "get_dependencies" => {
                let n = get("name");
                let v = get("version");
                let url = if v.is_empty() {
                    format!("https://crates.io/api/v1/crates/{}/dependencies", n)
                } else {
                    format!("https://crates.io/api/v1/crates/{}/{}/dependencies", n, v)
                };
                run(&url)
            }
            "get_dependency_docs" => {
                let n = get("name");
                let v = get("version");
                // Fetch crate top-level info (includes description + links).
                let info = run(&format!("https://crates.io/api/v1/crates/{}", n))?;
                let crate_data = &info["crate"];
                let description = crate_data["description"].as_str().unwrap_or("No description available.");
                let max_version = crate_data["max_version"].as_str().unwrap_or("unknown");
                let documentation = crate_data["documentation"].as_str().unwrap_or("");
                let homepage = crate_data["homepage"].as_str().unwrap_or("");
                let repository = crate_data["repository"].as_str().unwrap_or("");
                let requested = if v.is_empty() { max_version } else { v.as_str() };

                // Try to fetch the README body (rich docs).
                let readme = reqwest::blocking::Client::builder()
                    .user_agent(UA)
                    .build()
                    .map_err(|e| e.to_string())?
                    .get(format!("https://crates.io/api/v1/crates/{}/readme", n))
                    .send()
                    .ok()
                    .filter(|r| r.status().is_success())
                    .and_then(|r| r.text().ok())
                    .unwrap_or_default();

                let mut md = String::new();
                md.push_str(&format!("# {}\n\n", n));
                md.push_str(&format!("**Version:** {}\n\n", requested));
                md.push_str("## Description\n\n");
                md.push_str(&format!("{}\n\n", description));
                if !readme.is_empty() {
                    md.push_str("## README\n\n");
                    // Cap the README at a reasonable size to avoid huge responses.
                    let capped: String = readme.chars().take(12000).collect();
                    md.push_str(&capped);
                    md.push('\n');
                }
                md.push_str("\n## Links\n\n");
                if !documentation.is_empty() {
                    md.push_str(&format!("- [Documentation]({})\n", documentation));
                }
                if !repository.is_empty() {
                    md.push_str(&format!("- [Repository]({})\n", repository));
                }
                if !homepage.is_empty() {
                    md.push_str(&format!("- [Homepage]({})\n", homepage));
                }
                if md.is_empty() {
                    Err("could not fetch dependency docs".to_string())
                } else {
                    Ok(serde_json::json!({ "markdown": md }))
                }
            }
            "audit_dependencies" => {
                let n = get("name");
                let v = get("version");
                let resolved = if v.is_empty() {
                    run(&format!("https://crates.io/api/v1/crates/{}/versions", n))
                        .ok()
                        .and_then(|json| json.get("versions").cloned())
                        .and_then(|arr| arr.as_array().cloned())
                        .and_then(|arr| arr.into_iter().find(|x| x["yanked"] != serde_json::json!(true)))
                        .and_then(|x| x["num"].as_str().map(|s| s.to_string()))
                        .unwrap_or_default()
                } else {
                    v
                };
                if resolved.is_empty() {
                    return Err("could not resolve crate version".to_string());
                }
                let body = serde_json::json!({"query": {"package": {"name": n, "ecosystem": "crates.io"}, "version": resolved}});
                let osv = |b: serde_json::Value| -> Result<serde_json::Value, String> {
                    reqwest::blocking::Client::builder()
                        .user_agent(UA)
                        .build()
                        .map_err(|e| e.to_string())?
                        .post("https://api.osv.dev/v1/query")
                        .json(&b)
                        .send()
                        .map_err(|e| e.to_string())?
                        .json::<serde_json::Value>()
                        .map_err(|e| e.to_string())
                };
                osv(body)
            }
            _ => return Err(format!("unknown tool: {}", tool_name)),
        };

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_layout_host_returns_legacy_single() {
        let out = CargoBuildModule::new()
            .scaffold_layout(
                "demo",
                "goal",
                &["host".to_string()],
                spire_core::build_types::ProjectStructure::Native,
            )
            .unwrap();
        assert!(out.files.is_empty());
        assert_eq!(out.build_file, "Cargo.toml");
        assert!(out.build_content.contains("[package]"));
        assert!(out.source_file.contains("main.rs"));
        assert_eq!(out.platform_targets, vec!["host".to_string()]);
    }

    #[test]
    fn scaffold_layout_spire_app_emits_monorepo() {
        use spire_core::build_types::ProjectStructure;
        let out = CargoBuildModule::new()
            .scaffold_layout("spire-quicknotes", "", &[], ProjectStructure::SpireApp)
            .unwrap();
        assert_eq!(out.structure, ProjectStructure::SpireApp);
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        for expected in [
            "Cargo.toml",
            "crates/spire-quicknotes/Cargo.toml",
            "crates/spire-quicknotes/src/lib.rs",
            "crates/spire-quicknotes/src/main.rs",
            "ui/swift/Package.swift",
            "ui/swift/Sources/SpireUI/App.swift",
            "ui/swift/Sources/SpireUI/ContentView.swift",
            "ui/swift/Sources/SpireUI/Bridge/CoreBridge.swift",
            "build/assemble-app.sh",
            "Makefile",
            ".gitignore",
        ] {
            assert!(paths.contains(&expected), "missing {expected}");
        }
        assert_eq!(
            out.fill_roots,
            vec!["crates/spire-quicknotes/src", "ui/swift/Sources"]
        );
        assert_eq!(
            out.dependency_sections,
            vec!["crates/spire-quicknotes/Cargo.toml"]
        );
        assert_eq!(out.platform_targets, vec!["host"]);

        // Workspace manifest: path deps + the strip=none dylib fix + member.
        let ws = out
            .files
            .iter()
            .find(|f| f.path == "Cargo.toml")
            .expect("workspace Cargo.toml");
        assert!(ws.structural);
        assert!(ws.content.contains("spire-actor = { path = \"../spire-actor\" }"));
        assert!(ws.content.contains("spire-core = { path = \"../spire-core\" }"));
        assert!(ws.content.contains("strip = \"none\""));
        assert!(ws.content.contains("crates/spire-quicknotes"));

        // Crate manifest: cdylib+rlib + workspace deps.
        let cc = out
            .files
            .iter()
            .find(|f| f.path == "crates/spire-quicknotes/Cargo.toml")
            .unwrap();
        assert!(cc.content.contains("crate-type = [\"cdylib\", \"rlib\"]"));
        assert!(cc.content.contains("spire-actor = { workspace = true }"));

        // Source stubs fillable, build glue structural, FFI symbols present.
        let lib = out
            .files
            .iter()
            .find(|f| f.path == "crates/spire-quicknotes/src/lib.rs")
            .unwrap();
        assert!(!lib.structural);
        assert!(lib.content.contains("spire_send_json"));
        let sh = out
            .files
            .iter()
            .find(|f| f.path == "build/assemble-app.sh")
            .unwrap();
        assert!(sh.structural);
    }

    #[test]
    fn analyze_detects_spire_app_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = [\"crates/spire-quicknotes\"]\n\n[workspace.dependencies]\nspire-actor = { path = \"../spire-actor\" }\nspire-core = { path = \"../spire-core\" }\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/spire-quicknotes/src")).unwrap();
        std::fs::write(
            root.join("crates/spire-quicknotes/Cargo.toml"),
            "[package]\nname = \"spire-quicknotes\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("crates/spire-quicknotes/src/lib.rs"), "").unwrap();
        std::fs::create_dir_all(root.join("ui/swift")).unwrap();
        std::fs::write(root.join("ui/swift/Package.swift"), "// swift-tools-version: 5.10").unwrap();

        let meta = CargoBuildModule::new().analyze(root).unwrap();
        assert_eq!(
            meta.structure,
            spire_core::build_types::ProjectStructure::SpireApp
        );
        assert_eq!(meta.project_type, "spire_app");
        assert_eq!(meta.domains.len(), 2);
        let ids: Vec<&str> = meta.domains.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"core"));
        assert!(ids.contains(&"ui"));
    }

    #[test]
    fn scaffold_layout_multi_emits_single_crate_with_per_target_config() {
        // Serialize against `SPIRE_PLATFORM_DIR` mutating tests (build_manager
        // fixtures) — `from_registry` reads that process-global env var.
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let out = CargoBuildModule::new()
            .scaffold_layout(
                "demo",
                "goal",
                &["rpi5".to_string(), "rock3c".to_string()],
                spire_core::build_types::ProjectStructure::Hal,
            )
            .unwrap();
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        // SINGLE crate model: one Cargo.toml, one src/lib.rs, build.rs, and
        // .cargo/config.toml carrying per-target blocks. NO workspace member
        // crates (core/, rpi5/, rock3c/) — platforms are build targets, not
        // subprojects.
        assert!(paths.contains(&"Cargo.toml"));
        assert!(paths.contains(&"src/main.rs"));
        assert!(paths.contains(&"build.rs"));
        assert!(paths.contains(&".cargo/config.toml"));
        assert!(!paths.contains(&"core/Cargo.toml"));
        assert!(!paths.contains(&"rpi5/Cargo.toml"));
        assert!(!paths.contains(&"rock3c/Cargo.toml"));
        assert_eq!(out.platform_targets, vec!["rpi5".to_string(), "rock3c".to_string()]);

        // Single [dependencies] section (no [workspace.dependencies]).
        let root = out.files.iter().find(|f| f.path == "Cargo.toml").unwrap();
        assert!(root.content.contains("[dependencies]"));
        assert!(!root.content.contains("[workspace.dependencies]"));
        assert!(!root.content.contains("[workspace]"));

        // build.rs carries the cfg-gated link-search + TODO (no link-lib yet).
        let build_rs = out.files.iter().find(|f| f.path == "build.rs").unwrap();
        assert!(build_rs.content.contains("CARGO_CFG_TARGET_ARCH"));
        assert!(build_rs.content.contains("cargo:rustc-link-search="));
        assert!(build_rs.content.contains("cargo:rustc-link-lib"));

        // The single src/ dir is the only fill root.
        assert_eq!(out.fill_roots, vec!["src".to_string()]);
        // The single manifest's [dependencies] is the fillable dep section.
        assert_eq!(out.dependency_sections, vec!["Cargo.toml".to_string()]);
    }

    #[test]
    fn scaffolding_multi_platform_roundtrips_through_analyze() {
        // Serialize against `SPIRE_PLATFORM_DIR` mutating tests (build_manager
        // fixtures) — the analyzer reads that process-global env var.
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        // Write the scaffolded files to a temp dir, then verify the analyzer
        // sees ONE crate (not a workspace) with the crate's single lib target.
        let tmp = tempfile::tempdir().unwrap();
        let out = CargoBuildModule::new()
            .scaffold_layout(
                "demo",
                "goal",
                &["rpi5".to_string(), "rock3c".to_string()],
                spire_core::build_types::ProjectStructure::Hal,
            )
            .unwrap();
        for f in out.files {
            let p = tmp.path().join(&f.path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, &f.content).unwrap();
        }
        let meta = CargoBuildModule::new().analyze(tmp.path()).unwrap();
        assert!(!meta.is_workspace);
        assert!(meta.workspace_members.is_empty());
        // The .cargo/config.toml maps target triples → cross platforms, so when
        // the registry is available the analyze emits one build target per
        // platform (e.g. rpi5/rock3c) + the platform_targets list — one
        // project, multiple build targets like Meson. If the registry YAMLs are
        // absent, fall back to the single crate lib target.
        if !meta.platform_targets.is_empty() {
            let names: Vec<&str> = meta.targets.iter().map(|t| t.name.as_str()).collect();
            assert!(names.contains(&"rpi5"), "targets: {names:?}");
            assert!(names.contains(&"rock3c"), "targets: {names:?}");
            assert!(meta.targets.iter().all(|t| t.kind == vec!["lib"]));
        } else {
            assert_eq!(meta.targets.len(), 1);
            assert_eq!(meta.targets[0].name, "demo");
            assert_eq!(meta.targets[0].kind, vec!["lib"]);
        }
    }
}
