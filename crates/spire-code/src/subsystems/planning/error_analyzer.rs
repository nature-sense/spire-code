// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! ErrorAnalyzer actor — analyzes build errors and returns fix strategies.
//!
//! Receives per-system build results from BuildOrchestrator, queries the
//! graph for error type patterns, matches them against diagnostics, and
//! returns a FixPlan with ordered fix strategies across all failed systems.
//!
//! All graph access goes through `MemoryGraphMessage` — no direct `GraphDb`
//! reference. The MemoryGraphActor internally translates these into GQL queries.

use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use tokio::sync::mpsc;
use tracing::info;

use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::Actor;
use spire_core::models::memory_graph::{
    AnnotatedError, BuildContext, FixPlan, FixStrategy, ScoredFix,
    SystemBuildResult,
};

/// Messages for the ErrorAnalyzer actor.
#[derive(Debug)]
pub enum ErrorAnalyzerMessage {
    /// Analyze build errors across all failed systems and return fix strategies.
    AnalyzeErrors {
        /// All per-system results from the build
        system_results: Vec<SystemBuildResult>,
        /// Build run ID to query stored diagnostic nodes
        build_run_id: String,
        reply_to: tokio::sync::oneshot::Sender<Result<FixPlan>>,
    },
    /// Validate whether a fix strategy is applicable.
    ValidateFix {
        fix_name: String,
        context: BuildContext,
        reply_to: tokio::sync::oneshot::Sender<Result<bool>>,
    },
}

/// The ErrorAnalyzer actor.
pub struct ErrorAnalyzer {
    /// Sender to the MemoryGraphActor for all graph queries.
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
}

impl ErrorAnalyzer {
    pub fn new(memory_graph_tx: mpsc::Sender<MemoryGraphMessage>) -> Self {
        Self { memory_graph_tx }
    }

    /// Analyze build errors across all failed systems.
    ///
    /// For each error in each failed system, this method:
    /// 1. Queries the graph for Diagnostic nodes with matching build_run_id
    /// 2. Attempts to find the associated File node via HasDiagnostic edges
    /// 3. Matches each diagnostic against error_type detection_patterns via regex
    /// 4. For each matched error_type, looks up fix strategies
    /// 5. Produces a FixPlan with per-error annotated errors and merged ordered fixes
    async fn analyze_errors(
        &self,
        system_results: &[SystemBuildResult],
        build_run_id: &str,
    ) -> Result<FixPlan> {
        info!(
            "ErrorAnalyzer: analyzing errors from {} system(s)",
            system_results.len()
        );

        let mut all_annotated: Vec<AnnotatedError> = Vec::new();

        // ── Step 1: Load error type configurations from graph ──
        let error_types = self.load_error_types().await?;

        for system in system_results {
            if system.success {
                continue; // skip successful systems
            }

            // ── Step 2: Query Diagnostic nodes from the graph for this build run ──
            let diagnostics = self
                .query_diagnostics(build_run_id, &system.build_type)
                .await?;

            // Create a map of (file, line) → diagnostic node ID for quick lookup
            let diagnostic_map: std::collections::HashMap<(String, u32), String> = diagnostics
                .iter()
                .filter_map(|attr| {
                    let file = attr.str_prop("file")?;
                    let line = attr.u32_prop("line")?;
                    Some(((file, line), attr.id().to_string()))
                })
                .collect();

            // ── Step 3: Analyze each error ──
            for error in &system.errors {
                let diagnostic_node_id = diagnostic_map
                    .get(&(
                        error.file.clone().unwrap_or_default(),
                        error.line.unwrap_or(0),
                    ))
                    .cloned();

                // Try to find the file node ID via graph traversal
                let file_node_id = self
                    .find_file_node_id(error.file.as_deref(), diagnostic_node_id.as_deref())
                    .await;

                // ── Step 4: Match error against error_type patterns ──
                let mut fix_options: Vec<ScoredFix> = Vec::new();

                for et in &error_types {
                    if Self::matches_error_type(&error.error_text, &et.detection_patterns) {
                        // Look up fix strategies for this error type
                        let strategies = self.lookup_fix_strategies(&et.name).await?;
                        for strategy in strategies {
                            fix_options.push(ScoredFix {
                                strategy: strategy.clone(),
                                confidence: strategy.success_rate, // use success_rate as initial confidence
                                required_tools: Vec::new(),
                                validation_tools: Vec::new(),
                            });
                        }
                    }
                }

                // Sort fix options by confidence descending
                fix_options.sort_by(|a, b| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                // If no fix strategies matched, add a generic fallback
                if fix_options.is_empty() {
                    let generic = self.lookup_fix_strategy("generic-fix").await?;
                    if let Some(g) = generic {
                        fix_options.push(ScoredFix {
                            strategy: g,
                            confidence: 0.3,
                            required_tools: Vec::new(),
                            validation_tools: Vec::new(),
                        });
                    }
                }

                all_annotated.push(AnnotatedError {
                    error: error.clone(),
                    build_type: system.build_type.clone(),
                    build_path: system.path.clone(),
                    file_node_id,
                    diagnostic_node_id,
                    fix_options,
                });
            }
        }

        // ── Step 5: Build the ordered fix list (merged across all errors) ──
        let mut ordered_fixes: Vec<ScoredFix> = Vec::new();
        let mut seen_strategies = std::collections::HashSet::new();

        // Collect all fix options from all annotated errors
        for annotated in &all_annotated {
            for fix in &annotated.fix_options {
                // Deduplicate by strategy name
                if seen_strategies.insert(fix.strategy.name.clone()) {
                    ordered_fixes.push(fix.clone());
                }
            }
        }

        // Sort by confidence descending
        ordered_fixes.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        info!(
            "ErrorAnalyzer: generated FixPlan with {} errors, {} ordered fixes",
            all_annotated.len(),
            ordered_fixes.len()
        );

        Ok(FixPlan {
            errors: all_annotated,
            ordered_fixes,
            max_iterations: 5,
        })
    }

    /// Load all error type configurations from the graph.
    async fn load_error_types(&self) -> Result<Vec<ErrorTypeFromGraph>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("errorType".to_string()),
                subtype: None,
                name: None,
                limit: None,
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        let mut error_types = Vec::new();

        for attr in nodes {
            error_types.push(ErrorTypeFromGraph {
                name: attr.name().to_string(),
                detection_patterns: attr.str_array_prop("detection_patterns"),
            });
        }

        Ok(error_types)
    }

    /// Query Diagnostic nodes from the graph for a specific build run and
    /// system (open-envelope read; filtered client-side on the flattened
    /// `build_run_id`/`build_type` fields).
    async fn query_diagnostics(
        &self,
        build_run_id: &str,
        build_type: &str,
    ) -> Result<Vec<spire_core::models::memory_graph::AttrNode>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("diagnostic".to_string()),
                subtype: None,
                name: None,
                limit: Some(500),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        Ok(nodes
            .into_iter()
            .filter(|a| a.str_prop("build_run_id").as_deref() == Some(build_run_id))
            .filter(|a| a.str_prop("build_type").as_deref() == Some(build_type))
            .collect())
    }

    /// Try to find the File node ID associated with this error.
    /// First checks if we can traverse from the diagnostic node via HasDiagnostic edges,
    /// then falls back to a direct query by file path.
    async fn find_file_node_id(
        &self,
        file: Option<&str>,
        diagnostic_node_id: Option<&str>,
    ) -> Option<String> {
        // If we have a diagnostic node ID, try reverse traversal to find its file node
        if let Some(diag_id) = diagnostic_node_id {
            let (tx, rx) = tokio::sync::oneshot::channel();
            // Traverse "in" direction on HasDiagnostic edges to find the file node
            self.memory_graph_tx
                .send(MemoryGraphMessage::GetRelationships {
                    node_id: diag_id.to_string(),
                    reply_to: tx,
                })
                .await
                .ok()?;

            if let Ok(Ok(edges)) = rx.await {
                for edge in &edges {
                    if edge.edge_type
                        == spire_core::models::memory_graph::RelationshipType::HasDiagnostic
                    {
                        // The from_id is the file node, to_id is the diagnostic node
                        return Some(edge.from_id.clone());
                    }
                }
            }
        }

        // Fallback: query by file path
        if let Some(file_path) = file {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.memory_graph_tx
                .send(MemoryGraphMessage::QueryAttrNodes {
                    node_type: Some("Unknown".to_string()),
                    subtype: Some("File".to_string()),
                    name: Some(file_path.to_string()),
                    limit: Some(1),
                    reply_to: tx,
                })
                .await
                .ok()?;

            if let Ok(Ok(nodes)) = rx.await {
                return nodes.first().map(|n| n.id().to_string());
            }
        }

        None
    }

    /// Check if an error message matches any of the detection patterns for an error type.
    fn matches_error_type(error_text: &str, patterns: &[String]) -> bool {
        let text_lower = error_text.to_lowercase();
        for pattern in patterns {
            // Try regex first (case-insensitive — build output is normalized to
            // lowercase before matching), then substring match.
            if let Ok(re) = Regex::new(&format!("(?i){}", pattern)) {
                if re.is_match(&text_lower) {
                    return true;
                }
            } else if text_lower.contains(&pattern.to_lowercase()) {
                return true;
            }
        }
        false
    }

    /// Look up all fix strategies associated with an error type.
    async fn lookup_fix_strategies(&self, error_type_name: &str) -> Result<Vec<FixStrategy>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("fixStrategy".to_string()),
                subtype: None,
                name: None,
                limit: Some(50),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        let mut strategies = Vec::new();

        for attr in nodes {
            // Check if this fix strategy is linked to the error type via the
            // `applies_to` property.
            let linked_types = attr.str_array_prop("applies_to");

            if linked_types.is_empty() || linked_types.contains(&error_type_name.to_string()) {
                strategies.push(FixStrategy {
                    id: attr.id().to_string(),
                    name: attr.name().to_string(),
                    description: attr
                        .description()
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    category: attr
                        .str_prop("category")
                        .unwrap_or_else(|| "general".to_string()),
                    confidence_threshold: attr.f64_prop("confidence_threshold").unwrap_or(0.5),
                    success_rate: attr.f64_prop("success_rate").unwrap_or(0.5),
                    execution_steps: attr.str_array_prop("steps"),
                    has_rollback: attr.bool_prop("has_rollback").unwrap_or(false),
                });
            }
        }

        Ok(strategies)
    }

    /// Look up a single fix strategy by name.
    async fn lookup_fix_strategy(&self, name: &str) -> Result<Option<FixStrategy>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("fixStrategy".to_string()),
                subtype: None,
                name: Some(name.to_string()),
                limit: Some(1),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        Ok(nodes.into_iter().next().map(|attr| FixStrategy {
            id: attr.id().to_string(),
            name: attr.name().to_string(),
            description: attr
                .description()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            category: attr
                .str_prop("category")
                .unwrap_or_else(|| "general".to_string()),
            confidence_threshold: attr.f64_prop("confidence_threshold").unwrap_or(0.5),
            success_rate: attr.f64_prop("success_rate").unwrap_or(0.5),
            execution_steps: attr.str_array_prop("steps"),
            has_rollback: attr.bool_prop("has_rollback").unwrap_or(false),
        }))
    }

    /// Validate whether a fix strategy is applicable for the given context.
    async fn validate_fix(&self, fix_name: &str, _context: &BuildContext) -> Result<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("fixStrategy".to_string()),
                subtype: None,
                name: Some(fix_name.to_string()),
                limit: Some(1),
                reply_to: tx,
            })
            .await?;

        let nodes = rx.await??;
        Ok(!nodes.is_empty())
    }
}

#[async_trait]
impl Actor for ErrorAnalyzer {
    type Message = ErrorAnalyzerMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            ErrorAnalyzerMessage::AnalyzeErrors {
                system_results,
                build_run_id,
                reply_to,
            } => {
                let result = self.analyze_errors(&system_results, &build_run_id).await;
                let _ = reply_to.send(result);
            }
            ErrorAnalyzerMessage::ValidateFix {
                fix_name,
                context,
                reply_to,
            } => {
                let result = self.validate_fix(&fix_name, &context).await;
                let _ = reply_to.send(result);
            }
        }
    }
}

/// Internal type representing an error type loaded from the graph.
struct ErrorTypeFromGraph {
    name: String,
    detection_patterns: Vec<String>,
}
