// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! ProjectAnalyzerActor — semantic project analysis for LLM understanding.
//!
//! This actor produces a structured, semantic representation of a project's
//! directory structure, build systems, languages, and architecture. The output
//! is designed to give an LLM a rich understanding of the project without
//! needing to read every file.
//!
//! # Architecture
//!
//! Build system analysis is **delegated to external MCP servers** (mcp-cargo,
//! mcp-node, etc.) via the McpClientActor. The ProjectAnalyzerActor:
//!
//! 1. Scans the filesystem for build config files (Cargo.toml, package.json, etc.)
//! 2. Discovers which MCP servers can analyze each build file by calling
//!    `describe_analysis_capabilities` on each connected MCP server
//! 3. Delegates analysis to the matching MCP server via its `analyze` tool
//! 4. Falls back to the local `analyzer` module for file tree, language
//!    detection, and architecture summary (which don't need MCP servers)
//!
//! This is a **pure actor-based** design — no direct function calls to
//! build parsers. All build analysis flows through the actor message system.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::subsystems::build::build_manager::BuildManagerMessage;
use spire_core::subsystems::mcp::mcp_client::McpClientMessage;
use spire_core::actors::Actor;
use spire_core::analyzer::models::*;
use spire_core::analyzer::scanner;
use spire_core::analyzer::tree_builder;

// ============================================================================
// ProjectAnalysis — the structured output
// ============================================================================

/// Complete semantic analysis of a project.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectAnalysis {
    /// Project root path.
    pub project_root: String,
    /// Project name (inferred from directory name or build config).
    pub project_name: String,
    /// The full file tree with semantic annotations.
    pub file_tree: DirectoryNode,
    /// Build system metadata (one per detected build system).
    pub build_systems: Vec<BuildMetadata>,
    /// Language breakdown (language → file count).
    pub languages: Vec<LanguageBreakdown>,
    /// Directory role breakdown (role → directory count).
    pub directory_roles: Vec<RoleBreakdown>,
    /// File role breakdown (role → file count).
    pub file_roles: Vec<RoleBreakdown>,
    /// Key entry points (main files, lib files, etc.).
    pub entry_points: Vec<String>,
    /// Architecture summary (human-readable).
    pub architecture_summary: String,
    /// Total file count.
    pub total_files: usize,
    /// Total directory count.
    pub total_dirs: usize,
    /// Estimated total lines of code.
    pub total_lines: usize,
}

/// Language breakdown entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LanguageBreakdown {
    pub language: String,
    pub file_count: usize,
    pub line_estimate: usize,
}

/// Role breakdown entry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoleBreakdown {
    pub role: String,
    pub count: usize,
}

// ============================================================================
// ProjectAnalyzerMessage
// ============================================================================

/// Messages for the ProjectAnalyzer actor.
pub enum ProjectAnalyzerMessage {
    /// Initialize the actor with resources.
    Initialize {
        /// Sender to the McpClientActor for delegating build analysis.
        mcp_client_tx: mpsc::Sender<McpClientMessage>,
        reply_to: oneshot::Sender<Result<()>>,
    },
    /// Set the BuildManager sender so build analysis routes in-process.
    SetBuildManager {
        build_manager_tx: mpsc::Sender<BuildManagerMessage>,
        reply_to: oneshot::Sender<Result<()>>,
    },
    /// Analyze a project at the given root path.
    Analyze {
        project_root: PathBuf,
        reply_to: oneshot::Sender<Result<ProjectAnalysis>>,
    },
    /// Get a summary of the project (lighter weight than full analysis).
    Summarize {
        project_root: PathBuf,
        reply_to: oneshot::Sender<Result<String>>,
    },
}

// ============================================================================
// ProjectAnalyzerActor
// ============================================================================

/// The ProjectAnalyzer actor — semantic project analysis via MCP delegation.
pub struct ProjectAnalyzerActor {
    /// Sender to the McpClientActor for delegating build analysis to MCP servers.
    mcp_client_tx: Option<mpsc::Sender<McpClientMessage>>,
    /// Sender to the BuildManagerActor for routing build analysis to in-process modules.
    build_manager_tx: Option<mpsc::Sender<BuildManagerMessage>>,
}

impl ProjectAnalyzerActor {
    pub fn new() -> Self {
        Self {
            mcp_client_tx: None,
            build_manager_tx: None,
        }
    }

    /// Analyze build systems via the in-process BuildManager (preferred).
    async fn analyze_build_systems_via_manager(
        &self,
        project_root: &Path,
        build_configs: &[(PathBuf, PathBuf)],
        bm_tx: &mpsc::Sender<BuildManagerMessage>,
    ) -> Vec<BuildMetadata> {
        let mut out = Vec::new();
        info!(
            "ProjectAnalyzer: BuildManager analysis starting with {} build configs",
            build_configs.len()
        );
        for (build_file, dir) in build_configs {
            let target = if dir.as_os_str().is_empty() {
                project_root.to_path_buf()
            } else if dir.is_absolute() {
                dir.clone()
            } else {
                project_root.join(dir)
            };
            info!(
                "ProjectAnalyzer: analyzing via BuildManager: {:?} (build file: {})",
                target, build_file.display()
            );
            let (tx, rx) = oneshot::channel();
            if bm_tx
                .send(BuildManagerMessage::AnalyzeProject {
                    path: target.clone(),
                    reply_to: tx,
                })
                .await
                .is_err()
            {
                warn!("ProjectAnalyzer: BuildManager channel closed; cannot analyze");
                continue;
            }
            match rx.await {
                Ok(Ok(mut meta)) => {
                    // Normalize project_path to be relative to the project root.
                    // Build modules may return absolute paths; the UI and the MCP
                    // fallback path both expect relative paths.
                    if let Some(ref path) = meta.project_path {
                        let rel = Path::new(path)
                            .strip_prefix(project_root)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| path.clone());
                        meta.project_path = Some(rel);
                    }
                    info!(
                        "ProjectAnalyzer: BuildManager returned metadata: name={:?} build_system='{}' path='{}'",
                        meta.project_name, meta.build_system, meta.project_path.as_deref().unwrap_or("")
                    );
                    out.push(meta);
                }
                Ok(Err(e)) => {
                    warn!("ProjectAnalyzer: BuildManager analysis failed for {:?}: {}", target, e);
                }
                Err(e) => {
                    warn!("ProjectAnalyzer: BuildManager response lost for {:?}: {}", target, e);
                }
            }
        }
        info!(
            "ProjectAnalyzer: {} build systems via BuildManager",
            out.len()
        );
        out
    }

    /// Perform a full semantic analysis of a project.
    async fn analyze(&self, project_root: &Path) -> Result<ProjectAnalysis> {
        let start = std::time::Instant::now();
        info!(
            "ProjectAnalyzer: analyzing project at {}",
            project_root.display()
        );

        // 1. Scan the project (collect files with metadata)
        let _files = scanner::scan_directory(project_root, false);
        debug!("ProjectAnalyzer: scanned files");

        // 2. Build the file tree with semantic annotations
        let file_tree = tree_builder::build_file_tree(project_root, false);
        debug!("ProjectAnalyzer: built file tree");

        // 3. Detect build config files and delegate analysis to BuildManager
        //    (preferred, in-process) or MCP servers (fallback).
        let build_configs = scanner::discover_build_files(project_root, false);
        // Convert Vec<(String, String)> to Vec<(PathBuf, PathBuf)>
        let build_configs_pathbuf: Vec<(PathBuf, PathBuf)> = build_configs
            .into_iter()
            .map(|(a, b)| (PathBuf::from(a), PathBuf::from(b)))
            .collect();
        let build_systems = match &self.build_manager_tx {
            Some(bm_tx) => {
                let systems = self
                    .analyze_build_systems_via_manager(project_root, &build_configs_pathbuf, bm_tx)
                    .await;
                if !systems.is_empty() {
                    systems
                } else {
                    self.analyze_build_systems_via_mcp(project_root, &build_configs_pathbuf)
                        .await
                }
            }
            None => {
                self.analyze_build_systems_via_mcp(project_root, &build_configs_pathbuf)
                    .await
            }
        };
        debug!(
            "ProjectAnalyzer: detected {} build systems",
            build_systems.len()
        );

        // 4. Compute language breakdown from the file tree
        let mut lang_map: HashMap<String, (usize, usize)> = HashMap::new();
        collect_languages_from_tree(&file_tree, &mut lang_map);
        let mut languages: Vec<LanguageBreakdown> = lang_map
            .into_iter()
            .map(
                |(language, (file_count, line_estimate))| LanguageBreakdown {
                    language,
                    file_count,
                    line_estimate,
                },
            )
            .collect();
        languages.sort_by(|a, b| b.file_count.cmp(&a.file_count));

        // 5. Compute directory role breakdown
        let mut dir_role_map: HashMap<String, usize> = HashMap::new();
        collect_directory_roles(&file_tree, &mut dir_role_map);
        let mut directory_roles: Vec<RoleBreakdown> = dir_role_map
            .into_iter()
            .map(|(role, count)| RoleBreakdown { role, count })
            .collect();
        directory_roles.sort_by(|a, b| b.count.cmp(&a.count));

        // 6. Compute file role breakdown
        let mut file_role_map: HashMap<String, usize> = HashMap::new();
        collect_file_roles(&file_tree, &mut file_role_map);
        let mut file_roles: Vec<RoleBreakdown> = file_role_map
            .into_iter()
            .map(|(role, count)| RoleBreakdown { role, count })
            .collect();
        file_roles.sort_by(|a, b| b.count.cmp(&a.count));

        // 7. Identify entry points
        let entry_points: Vec<String> = find_entry_points(&file_tree);

        // 8. Build architecture summary
        let architecture_summary = build_architecture_summary(
            &build_systems,
            &languages,
            &directory_roles,
            &file_roles,
            &entry_points,
            &file_tree,
        );

        // 9. Compute totals
        let total_files = file_tree.total_file_count;
        let total_dirs = count_directories(&file_tree);
        let total_lines = file_tree.total_lines;

        let project_name = project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
            .to_string();

        let analysis = ProjectAnalysis {
            project_root: project_root.to_string_lossy().to_string(),
            project_name,
            file_tree,
            build_systems,
            languages,
            directory_roles,
            file_roles,
            entry_points,
            architecture_summary,
            total_files,
            total_dirs,
            total_lines,
        };

        info!(
            "ProjectAnalyzer: analysis complete in {:?} — {} files, {} dirs, {} build systems",
            start.elapsed(),
            total_files,
            total_dirs,
            analysis.build_systems.len(),
        );

        Ok(analysis)
    }

    /// Analyze build systems by delegating to MCP servers.
    ///
    /// For each discovered build config file, this method:
    /// 1. Calls `describe_analysis_capabilities` on all connected MCP servers
    ///    to find which server can handle the build file
    /// 2. Calls the matching server's `analyze` tool with the project path
    /// 3. Collects the results into a `Vec<BuildMetadata>`
    async fn analyze_build_systems_via_mcp(
        &self,
        project_root: &Path,
        build_configs: &[(PathBuf, PathBuf)],
    ) -> Vec<BuildMetadata> {
        let mcp_client_tx = match &self.mcp_client_tx {
            Some(tx) => tx.clone(),
            None => {
                warn!("ProjectAnalyzer: no mcp_client_tx available, skipping MCP build analysis");
                return Vec::new();
            }
        };

        if build_configs.is_empty() {
            return Vec::new();
        }

        // Step 1: Discover which MCP servers expose a `describe_analysis_capabilities`
        // tool. Uses `GetConnectedServersWithTools` to get each server's tool list,
        // then filters to only build-capable servers.
        let server_names = self.discover_mcp_servers(&mcp_client_tx).await;

        if server_names.is_empty() {
            warn!("ProjectAnalyzer: no MCP servers available for build analysis");
            return Vec::new();
        }

        // Step 2: Query each server's capabilities
        let mut capability_map: HashMap<String, McpServerCapability> = HashMap::new();
        for server_name in &server_names {
            match self
                .query_server_capabilities(&mcp_client_tx, server_name)
                .await
            {
                Some(cap) => {
                    debug!(
                        "ProjectAnalyzer: server '{}' capabilities: {:?}",
                        server_name, cap
                    );
                    capability_map.insert(server_name.clone(), cap);
                }
                None => {
                    debug!(
                        "ProjectAnalyzer: server '{}' has no describe_analysis_capabilities tool",
                        server_name
                    );
                }
            }
        }

        // Step 3: For each build config file, find a matching server and delegate
        let mut build_systems: Vec<BuildMetadata> = Vec::new();
        for (build_file, parent_dir) in build_configs {
            let build_file_name = build_file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            // Find a server whose supported_files include this build file
            let matching_server = capability_map.iter().find(|(_name, cap)| {
                cap.capabilities.iter().any(|pattern| {
                    // Simple glob matching: check if the build file name matches
                    // the supported file pattern
                    if pattern == &build_file_name {
                        return true;
                    }
                    // Handle wildcard patterns like "*.toml"
                    if let Some(ext) = pattern.strip_prefix("*.") {
                        if build_file_name.ends_with(ext) {
                            return true;
                        }
                    }
                    false
                })
            });

            match matching_server {
                Some((server_name, cap)) => {
                    let tool_name = cap.analyzer_tool.as_deref().unwrap_or("analyze");
                    debug!(
                        "ProjectAnalyzer: delegating '{}' to server '{}' (tool: {})",
                        build_file_name, server_name, tool_name
                    );
                    // Make parent_dir absolute by joining with project_root
                    let analysis_target = if parent_dir.is_absolute() {
                        parent_dir.clone()
                    } else if parent_dir.as_os_str().is_empty() {
                        project_root.to_path_buf()
                    } else {
                        project_root.join(parent_dir)
                    };
                    match self
                        .delegate_analysis(&mcp_client_tx, server_name, tool_name, &analysis_target)
                        .await
                    {
                        Some(mut metadata) => {
                            // Fill project_name from resolved absolute target path if missing
                            if metadata.project_name.is_none() {
                                if let Some(dir_name) =
                                    analysis_target.file_name().and_then(|n| n.to_str())
                                {
                                    metadata.project_name = Some(dir_name.to_string());
                                }
                            }
                            // Fill project_path as relative to project_root
                            let rel_path = analysis_target
                                .strip_prefix(project_root)
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|_| {
                                    analysis_target
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("unknown")
                                        .to_string()
                                });
                            metadata.project_path = Some(rel_path);
                            tracing::info!(
                                "ProjectAnalyzer: got build metadata: name={:?} build_system='{}' project_type='{}' path='{}'",
                                metadata.project_name, metadata.build_system, metadata.project_type,
                                analysis_target.display()
                            );
                            build_systems.push(metadata);
                        }
                        None => {
                            warn!(
                                "ProjectAnalyzer: MCP analysis failed for '{}' via server '{}' (target path: '{}')",
                                build_file_name, server_name, analysis_target.display()
                            );
                        }
                    }
                }
                None => {
                    debug!(
                        "ProjectAnalyzer: no MCP server can handle '{}', skipping",
                        build_file_name
                    );
                }
            }
        }

        build_systems
    }

    /// Discover MCP servers that expose a `describe_analysis_capabilities` tool.
    ///
    /// Uses `GetConnectedServersWithTools` to get each server's tool list, then
    /// filters to only servers that advertise the capability introspection tool.
    /// This avoids calling `describe_analysis_capabilities` on servers that don't
    /// implement it (e.g., mcp-git, mcp-filesystem, mcp-terminal, etc.).
    async fn discover_mcp_servers(
        &self,
        mcp_client_tx: &mpsc::Sender<McpClientMessage>,
    ) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        if mcp_client_tx
            .send(McpClientMessage::GetConnectedServersWithTools { reply_to: tx })
            .await
            .is_err()
        {
            warn!("ProjectAnalyzer: failed to send GetConnectedServersWithTools");
            return Vec::new();
        }

        match rx.await {
            Ok(servers_with_tools) => {
                let capable: Vec<String> = servers_with_tools
                    .into_iter()
                    .filter(|(_name, tools)| {
                        tools
                            .iter()
                            .any(|t| t.name == "describe_analysis_capabilities")
                    })
                    .map(|(name, _tools)| name)
                    .collect();
                debug!(
                    "ProjectAnalyzer: discovered {} build-capable MCP servers: {:?}",
                    capable.len(),
                    capable
                );
                capable
            }
            Err(e) => {
                warn!(
                    "ProjectAnalyzer: GetConnectedServersWithTools response error: {}",
                    e
                );
                Vec::new()
            }
        }
    }

    /// Query a single MCP server's analysis capabilities.
    async fn query_server_capabilities(
        &self,
        mcp_client_tx: &mpsc::Sender<McpClientMessage>,
        server_name: &str,
    ) -> Option<McpServerCapability> {
        let (tx, rx) = oneshot::channel();
        if mcp_client_tx
            .send(McpClientMessage::CallTool {
                server_name: server_name.to_string(),
                tool_name: "describe_analysis_capabilities".to_string(),
                arguments: None,
                reply_to: tx,
            })
            .await
            .is_err()
        {
            warn!(
                "ProjectAnalyzer: failed to send CallTool to server '{}'",
                server_name
            );
            return None;
        }

        match rx.await {
            Ok(Ok(result)) => {
                // Parse the text content as JSON
                for content in &result.content {
                    if let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = content {
                        // Try parsing as McpServerCapability first, then fall back
                        // to flexible parsing (MCP servers may use different field names)
                        if let Ok(cap) = serde_json::from_str::<McpServerCapability>(&tc.text) {
                            return Some(cap);
                        }
                        // Flexible fallback: parse as Value and extract fields
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&tc.text) {
                            let name = val
                                .get("name")
                                .or(val.get("server_name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or(server_name)
                                .to_string();
                            let capabilities: Vec<String> = val
                                .get("supported_files")
                                .or(val.get("capabilities"))
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let analyzer_tool = val
                                .get("analyzer_tool")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            return Some(McpServerCapability {
                                name,
                                capabilities,
                                analyzer_tool,
                            });
                        }
                    }
                }
                warn!(
                    "ProjectAnalyzer: could not parse capabilities from server '{}'",
                    server_name
                );
                None
            }
            Ok(Err(e)) => {
                warn!(
                    "ProjectAnalyzer: server '{}' describe_analysis_capabilities failed: {}",
                    server_name, e
                );
                None
            }
            Err(e) => {
                warn!(
                    "ProjectAnalyzer: server '{}' describe_analysis_capabilities response error: {}",
                    server_name, e
                );
                None
            }
        }
    }

    /// Delegate build analysis to a specific MCP server.
    async fn delegate_analysis(
        &self,
        mcp_client_tx: &mpsc::Sender<McpClientMessage>,
        server_name: &str,
        tool_name: &str,
        project_root: &Path,
    ) -> Option<BuildMetadata> {
        let mut args = serde_json::Map::new();
        // Use send both `path` and `project_root` so it works regardless of which
        // parameter name the MCP server uses
        let path_str = project_root.to_string_lossy().to_string();
        args.insert(
            "path".to_string(),
            serde_json::Value::String(path_str.clone()),
        );
        args.insert(
            "project_root".to_string(),
            serde_json::Value::String(path_str),
        );

        let (tx, rx) = oneshot::channel();
        if mcp_client_tx
            .send(McpClientMessage::CallTool {
                server_name: server_name.to_string(),
                tool_name: tool_name.to_string(),
                arguments: Some(args),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            warn!(
                "ProjectAnalyzer: failed to send CallTool to server '{}' for tool '{}'",
                server_name, tool_name
            );
            return None;
        }

        match rx.await {
            Ok(Ok(result)) => {
                for content in &result.content {
                    if let rust_mcp_sdk::schema::ContentBlock::TextContent(tc) = content {
                        let text = &tc.text;
                        tracing::info!(
                            "ProjectAnalyzer: raw MCP response from {}: {}",
                            server_name,
                            text
                        );
                        // Try to parse as BuildMetadata directly
                        match serde_json::from_str::<BuildMetadata>(text) {
                            Ok(metadata) => {
                                tracing::info!("ProjectAnalyzer: parsed BuildMetadata: build_system='{}' project_type='{}' project_name={:?}",
                                    metadata.build_system, metadata.project_type, metadata.project_name);
                                return Some(metadata);
                            }
                            Err(e1) => {
                                tracing::warn!(
                                    "ProjectAnalyzer: serde parse error for {}: {}",
                                    server_name,
                                    e1
                                );
                                // Try to parse as a wrapper { success: true, ... }
                                match serde_json::from_str::<serde_json::Value>(text) {
                                    Ok(wrapper) => {
                                        if let Some(inner) = wrapper.get("success") {
                                            if inner.as_bool() == Some(true) {
                                                // The actual metadata might be nested
                                                if let Some(data) = wrapper.get("data") {
                                                    match serde_json::from_value::<BuildMetadata>(
                                                        data.clone(),
                                                    ) {
                                                        Ok(metadata) => return Some(metadata),
                                                        Err(e2) => {
                                                            warn!(
                                                                "ProjectAnalyzer: could not parse BuildMetadata from server '{}' tool '{}': direct={}, nested={}",
                                                                server_name, tool_name, e1, e2
                                                            );
                                                        }
                                                    }
                                                } else {
                                                    warn!(
                                                        "ProjectAnalyzer: server '{}' tool '{}' returned success=true but no 'data' field. Raw: {}",
                                                        server_name, tool_name, text
                                                    );
                                                }
                                            } else {
                                                warn!(
                                                    "ProjectAnalyzer: server '{}' tool '{}' returned success=false. Raw: {}",
                                                    server_name, tool_name, text
                                                );
                                            }
                                        } else {
                                            warn!(
                                                "ProjectAnalyzer: could not parse BuildMetadata from server '{}' tool '{}': {}. Raw: {}",
                                                server_name, tool_name, e1, text
                                            );
                                        }
                                    }
                                    Err(_) => {
                                        warn!(
                                            "ProjectAnalyzer: could not parse BuildMetadata from server '{}' tool '{}': {}. Raw: {}",
                                            server_name, tool_name, e1, text
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                None
            }
            Ok(Err(e)) => {
                warn!(
                    "ProjectAnalyzer: server '{}' tool '{}' failed: {}",
                    server_name, tool_name, e
                );
                None
            }
            Err(e) => {
                warn!(
                    "ProjectAnalyzer: server '{}' tool '{}' response error: {}",
                    server_name, tool_name, e
                );
                None
            }
        }
    }

    /// Generate a concise text summary of the project.
    async fn summarize(&self, project_root: &Path) -> Result<String> {
        let analysis = self.analyze(project_root).await?;
        Ok(format_project_summary(&analysis))
    }
}

impl Default for ProjectAnalyzerActor {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Actor Trait Implementation
// ============================================================================

#[async_trait]
impl Actor for ProjectAnalyzerActor {
    type Message = ProjectAnalyzerMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            ProjectAnalyzerMessage::Initialize {
                mcp_client_tx,
                reply_to,
            } => {
                self.mcp_client_tx = Some(mcp_client_tx);
                info!("ProjectAnalyzerActor: initialized with MCP client channel");
                let _ = reply_to.send(Ok(()));
            }
            ProjectAnalyzerMessage::SetBuildManager {
                build_manager_tx,
                reply_to,
            } => {
                self.build_manager_tx = Some(build_manager_tx);
                info!("ProjectAnalyzerActor: BuildManager channel set");
                let _ = reply_to.send(Ok(()));
            }
            ProjectAnalyzerMessage::Analyze {
                project_root,
                reply_to,
            } => {
                let result = self.analyze(&project_root).await;
                let _ = reply_to.send(result);
            }
            ProjectAnalyzerMessage::Summarize {
                project_root,
                reply_to,
            } => {
                let result = self.summarize(&project_root).await;
                let _ = reply_to.send(result);
            }
        }
    }
}

// ============================================================================
// Tree Traversal Helpers
// ============================================================================

/// Recursively collect language statistics from the file tree.
fn collect_languages_from_tree(dir: &DirectoryNode, map: &mut HashMap<String, (usize, usize)>) {
    for file in &dir.files {
        let entry = map.entry(file.language.clone()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += file.lines_estimated;
    }
    for subdir in &dir.directories {
        collect_languages_from_tree(subdir, map);
    }
}

/// Recursively collect directory role statistics.
fn collect_directory_roles(dir: &DirectoryNode, map: &mut HashMap<String, usize>) {
    *map.entry(dir.role.clone()).or_insert(0) += 1;
    for subdir in &dir.directories {
        collect_directory_roles(subdir, map);
    }
}

/// Recursively collect file role statistics.
fn collect_file_roles(dir: &DirectoryNode, map: &mut HashMap<String, usize>) {
    for file in &dir.files {
        *map.entry(file.role.clone()).or_insert(0) += 1;
    }
    for subdir in &dir.directories {
        collect_file_roles(subdir, map);
    }
}

/// Recursively find entry point files.
fn find_entry_points(dir: &DirectoryNode) -> Vec<String> {
    let mut entries = Vec::new();
    for file in &dir.files {
        if file.role == "entry_point" {
            entries.push(file.path.clone());
        }
    }
    for subdir in &dir.directories {
        entries.extend(find_entry_points(subdir));
    }
    entries
}

/// Count total directories in the tree (recursive).
fn count_directories(dir: &DirectoryNode) -> usize {
    let mut count = 1; // this directory
    for subdir in &dir.directories {
        count += count_directories(subdir);
    }
    count
}

// ============================================================================
// Summary Builders
// ============================================================================

/// Build a human-readable architecture summary.
fn build_architecture_summary(
    build_systems: &[BuildMetadata],
    languages: &[LanguageBreakdown],
    directory_roles: &[RoleBreakdown],
    file_roles: &[RoleBreakdown],
    entry_points: &[String],
    file_tree: &DirectoryNode,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    // Project type
    if let Some(bs) = build_systems.first() {
        parts.push(format!(
            "Build system: {} ({})",
            bs.build_system, bs.project_type
        ));
    }

    // Domains — named slices the LLM edits within, with their editability
    // constraints. For Hal projects these are `common` (toolkit + contracts,
    // read-only/shared) and one platform domain per target (rpi5/rock3c —
    // app + HAL impl, fillable only for that platform).
    let domain_parts: Vec<String> = build_systems
        .iter()
        .flat_map(|bs| bs.domains.iter())
        .map(|d| {
            let edit = match d.editability {
                DomainEditability::ReadOnly => "read_only",
                DomainEditability::Shared => "shared",
                DomainEditability::Fillable => "fillable",
            };
            let deps: Vec<String> = d
                .dependencies
                .iter()
                .map(|dep| dep.name.clone())
                .collect();
            let mut s = format!(
                "  - {} ({}): editability={}",
                d.name,
                d.kind,
                edit
            );
            if !d.files.is_empty() {
                s.push_str(&format!("\n    files: {}", d.files.join(", ")));
            }
            if !deps.is_empty() {
                s.push_str(&format!("\n    dependencies: {}", deps.join(", ")));
            }
            if !d.contracts.is_empty() {
                s.push_str(&format!("\n    contracts: {}", d.contracts.join(", ")));
            }
            s
        })
        .collect();
    if !domain_parts.is_empty() {
        parts.push(format!("Domains (use these as edit scope):\n{}", domain_parts.join("\n")));
    }

    // Languages
    if !languages.is_empty() {
        let lang_str: Vec<String> = languages
            .iter()
            .map(|l| format!("{} ({} files)", l.language, l.file_count))
            .collect();
        parts.push(format!("Languages: {}", lang_str.join(", ")));
    }

    // Directory structure
    if !directory_roles.is_empty() {
        let dir_str: Vec<String> = directory_roles
            .iter()
            .map(|r| format!("{}: {}", r.role, r.count))
            .collect();
        parts.push(format!("Directory structure: {}", dir_str.join(", ")));
    }

    // File roles
    if !file_roles.is_empty() {
        let file_str: Vec<String> = file_roles
            .iter()
            .map(|r| format!("{}: {}", r.role, r.count))
            .collect();
        parts.push(format!("File types: {}", file_str.join(", ")));
    }

    // Entry points
    if !entry_points.is_empty() {
        parts.push(format!("Entry points: {}", entry_points.join(", ")));
    }

    // Totals
    parts.push(format!(
        "Total: {} files, {} directories, ~{} lines of code",
        file_tree.total_file_count,
        count_directories(file_tree),
        file_tree.total_lines,
    ));

    parts.join("\n")
}

/// Format the full project analysis as a human-readable summary string.
fn format_project_summary(analysis: &ProjectAnalysis) -> String {
    let mut output = String::new();

    output.push_str(&format!("Project: {}\n", analysis.project_name));
    output.push_str(&format!("Root: {}\n", analysis.project_root));
    output.push_str(&format!(
        "Files: {}, Dirs: {}, Lines: ~{}\n\n",
        analysis.total_files, analysis.total_dirs, analysis.total_lines
    ));

    // Build systems
    if !analysis.build_systems.is_empty() {
        output.push_str("=== Build Systems ===\n");
        for bs in &analysis.build_systems {
            output.push_str(&format!("  {} ({})\n", bs.build_system, bs.project_type));
            if let Some(ref name) = bs.project_name {
                output.push_str(&format!("    Name: {}\n", name));
            }
            if let Some(ref ver) = bs.version {
                output.push_str(&format!("    Version: {}\n", ver));
            }
            if !bs.scripts.is_empty() {
                output.push_str("    Scripts:\n");
                for script in &bs.scripts {
                    output.push_str(&format!("      {}: {}\n", script.name, script.command));
                }
            }
        }
        output.push('\n');
    }

    // Languages
    if !analysis.languages.is_empty() {
        output.push_str("=== Languages ===\n");
        for lang in &analysis.languages {
            output.push_str(&format!(
                "  {}: {} files, ~{} lines\n",
                lang.language, lang.file_count, lang.line_estimate
            ));
        }
        output.push('\n');
    }

    // Directory roles
    if !analysis.directory_roles.is_empty() {
        output.push_str("=== Directory Structure ===\n");
        for role in &analysis.directory_roles {
            output.push_str(&format!("  {}: {} dirs\n", role.role, role.count));
        }
        output.push('\n');
    }

    // Entry points
    if !analysis.entry_points.is_empty() {
        output.push_str("=== Entry Points ===\n");
        for ep in &analysis.entry_points {
            output.push_str(&format!("  {}\n", ep));
        }
        output.push('\n');
    }

    // Architecture summary
    output.push_str("=== Architecture Summary ===\n");
    output.push_str(&analysis.architecture_summary);
    output.push('\n');

    output
}
