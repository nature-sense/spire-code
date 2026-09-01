// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! IntentRouterActor — routes user queries to the correct handler based on intent.
//!
//! This actor sits between the CoordinatorActor (which receives user messages)
//! and the various handler actors (LLM, BuildOrchestrator, ErrorAnalyzer, etc.).
//!
//! Flow:
//!   1. CoordinatorActor receives a user message
//!   2. CoordinatorActor sends IntentRouterMessage::RouteQuery to IntentRouterActor
//!   3. IntentRouterActor queries the memory graph for matching intents
//!   4. IntentRouterActor returns a RouteResult indicating which handler to invoke
//!   5. CoordinatorActor dispatches to the appropriate handler
//!
//! Intent matching is done by querying the memory graph for nodes with
//! subtype="intent" and matching the query text against the intent's patterns
//! and description. If no intent matches, the default "chat" intent is returned,
//! which routes to the LLM for free-form conversation.
//!
//! Routing is fully config-driven: the `handler` and `action` fields from
//! config/intents.json determine which RouteResult variant is returned.
//! State requirements and approval gating are also checked at routing time.

use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::actors::Actor;
use spire_core::models::memory_graph::SearchOptions;

// ============================================================================
// RouteResult — the output of intent routing
// ============================================================================

/// The result of routing a user query to a handler.
#[derive(Debug, Clone)]
pub enum RouteResult {
    /// Route to the LLM for free-form chat / general assistance.
    Chat,
    /// Route to the build orchestrator for build-related intents.
    Build {
        intent_name: String,
        confidence: f64,
        parameters: HashMap<String, String>,
    },
    /// Route to the error analyzer for error analysis intents.
    AnalyzeError {
        intent_name: String,
        confidence: f64,
        error_text: String,
    },
    /// Route to the project analyzer for project analysis intents.
    AnalyzeProject {
        intent_name: String,
        confidence: f64,
    },
    /// Route to the project query for information retrieval intents.
    QueryProject {
        intent_name: String,
        confidence: f64,
        query: String,
    },
    /// Route to the tool orchestrator for tool execution intents.
    ExecuteTool {
        intent_name: String,
        confidence: f64,
        tool_name: String,
        parameters: HashMap<String, String>,
    },
    /// Intent matched but required states are not met.
    StateBlocked {
        intent_name: String,
        confidence: f64,
        missing_states: Vec<String>,
    },
    /// Intent matched but requires user approval before proceeding.
    NeedsApproval {
        intent_name: String,
        confidence: f64,
    },
    /// Route to the PlanOrchestrator for plan creation.
    Plan {
        intent_name: String,
        confidence: f64,
        parameters: HashMap<String, String>,
    },
}

// ============================================================================
// Messages
// ============================================================================

/// Messages for the IntentRouterActor.
#[derive(Debug)]
pub enum IntentRouterMessage {
    /// Route a user query to the appropriate handler.
    RouteQuery {
        query: String,
        reply_to: oneshot::Sender<RouteResult>,
    },
}

// ============================================================================
// Actor
// ============================================================================

/// The IntentRouterActor — routes user queries to handlers based on intent.
pub struct IntentRouterActor {
    memory_graph_tx: mpsc::Sender<MemoryGraphMessage>,
}

impl IntentRouterActor {
    pub fn new(memory_graph_tx: mpsc::Sender<MemoryGraphMessage>) -> Self {
        Self { memory_graph_tx }
    }

    /// Route a user query to the appropriate handler.
    async fn route_query(&self, query: String) -> RouteResult {
        let query_lower = query.to_lowercase();
        info!("[INTENT_ROUTER] QUERY received: \"{}\"", query);

        // Query the memory graph for intent nodes
        let intents = match self.query_intents().await {
            Some(intents) => intents,
            None => {
                info!("[INTENT_ROUTER] no intents found in graph, defaulting to Chat");
                return RouteResult::Chat;
            }
        };

        info!(
            "[INTENT_ROUTER] INTENTS loaded: {} intents from graph: {:?}",
            intents.len(),
            intents.iter().map(|n| n.name()).collect::<Vec<_>>()
        );

        // Score each intent against the query
        let mut best_match: Option<(String, u32, f64, String)> = None; // (name, priority, confidence, matched_pattern)

        for intent in &intents {
            let name = intent.name().to_lowercase();
            let description = intent.description().unwrap_or("").to_lowercase();

            // Check patterns stored in properties
            if let Some(patterns) = intent.get("patterns") {
                if let Some(pattern_array) = patterns.as_array() {
                    for pattern in pattern_array {
                        if let Some(pattern_str) = pattern.as_str() {
                            let pattern_lower = pattern_str.to_lowercase();
                            let matched = query_lower.contains(&pattern_lower);
                            let priority = intent
                                .get("priority")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32;
                            debug!(
                                "[INTENT_ROUTER] SCAN intent='{}' (P={}): pattern=\"{}\" → {}",
                                intent.name(),
                                priority,
                                pattern_str,
                                if matched { "MATCHED" } else { "no match" }
                            );
                            if matched {
                                // Confidence = priority / 10.0 (max priority is 10)
                                let confidence = priority as f64 / 10.0;

                                match &best_match {
                                    Some((_, best_priority, _, _))
                                        if priority <= *best_priority => {}
                                    _ => {
                                        best_match = Some((
                                            intent.name().to_string(),
                                            priority,
                                            confidence,
                                            pattern_str.to_string(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Also check if the intent name or description appears in the query
            let name_desc_match = query_lower.contains(&name) || query_lower.contains(&description);
            let priority = intent
                .get("priority")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            debug!(
                "[INTENT_ROUTER] SCAN intent='{}' (P={}): name/desc match → {}",
                intent.name(),
                priority,
                if name_desc_match {
                    "MATCHED"
                } else {
                    "no match"
                }
            );

            if name_desc_match {
                // Name/description match gets slightly lower confidence than pattern match
                let confidence = (priority as f64 / 10.0) * 0.9;

                match &best_match {
                    Some((_, best_priority, _, _)) if priority <= *best_priority => {}
                    _ => {
                        best_match = Some((
                            intent.name().to_string(),
                            priority,
                            confidence,
                            "".to_string(),
                        ));
                    }
                }
            }
        }

        // Route based on the best matching intent
        match best_match {
            Some((intent_name, priority, confidence, pattern)) => {
                info!("[INTENT_ROUTER] BEST MATCH: intent='{}', priority={}, confidence={:.2}, matched_pattern=\"{}\"", intent_name, priority, confidence, pattern);

                // Look up the matched intent's properties for handler/action/state/approval
                let matched_intent = intents.iter().find(|n| n.name() == intent_name);
                match matched_intent {
                    Some(intent) => {
                        // Step 1: Check state requirements
                        let state_reqs = intent
                            .get("state_requirements")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();

                        if !state_reqs.is_empty() {
                            match self.check_state_requirements(&state_reqs).await {
                                Ok(missing) if !missing.is_empty() => {
                                    info!("[INTENT_ROUTER] STATE CHECK: intent='{}' required={:?}, existing=partial, missing={:?} — BLOCKED", intent_name, state_reqs, missing);
                                    return RouteResult::StateBlocked {
                                        intent_name: intent_name.clone(),
                                        confidence,
                                        missing_states: missing,
                                    };
                                }
                                Ok(_missing) => {
                                    info!("[INTENT_ROUTER] STATE CHECK: intent='{}' required={:?} — ALL SATISFIED", intent_name, state_reqs);
                                }
                                Err(e) => {
                                    warn!("[INTENT_ROUTER] state check error (non-fatal): {}", e);
                                }
                            }
                        } else {
                            info!(
                                "[INTENT_ROUTER] STATE CHECK: intent='{}' no state requirements",
                                intent_name
                            );
                        }

                        // Step 2: Check approval requirement
                        let requires_approval = intent
                            .get("requires_approval")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        if requires_approval {
                            info!("[INTENT_ROUTER] APPROVAL GATE: intent='{}' requires approval — NeedsApproval", intent_name);
                            return RouteResult::NeedsApproval {
                                intent_name: intent_name.clone(),
                                confidence,
                            };
                        } else {
                            info!(
                                "[INTENT_ROUTER] APPROVAL GATE: intent='{}' no approval needed",
                                intent_name
                            );
                        }

                        // Step 3: Route to handler based on config-driven handler/action fields
                        let handler = intent
                            .get("handler")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let action = intent
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        info!("[INTENT_ROUTER] ROUTING: intent='{}' → handler='{}', action='{}', confidence={}", intent_name, handler, action, confidence);

                        self.route_to_handler(
                            &intent_name,
                            &query_lower,
                            handler,
                            action,
                            confidence,
                        )
                    }
                    None => {
                        // Shouldn't happen since we matched it, but fallback
                        RouteResult::Chat
                    }
                }
            }
            None => {
                // Keyword matching failed — try semantic fallback via vector search
                info!("[INTENT_ROUTER] KEYWORD: no match found for query");
                info!(
                    "[INTENT_ROUTER] SEMANTIC FALLBACK: trying vector search for query: {}",
                    query
                );
                match self.semantic_match_intent(&query).await {
                    Some((intent_name, similarity)) => {
                        info!("[INTENT_ROUTER] SEMANTIC FALLBACK: matched intent='{}' (similarity={:.2})", intent_name, similarity);

                        // Look up the matched intent's properties
                        let matched_intent = intents.iter().find(|n| n.name() == intent_name);
                        match matched_intent {
                            Some(intent) => {
                                // Check state requirements
                                let state_reqs = intent
                                    .get("state_requirements")
                                    .and_then(|v| v.as_array())
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(|v| v.as_str().map(String::from))
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();

                                if !state_reqs.is_empty() {
                                    match self.check_state_requirements(&state_reqs).await {
                                        Ok(missing) if !missing.is_empty() => {
                                            return RouteResult::StateBlocked {
                                                intent_name: intent_name.clone(),
                                                confidence: similarity,
                                                missing_states: missing,
                                            };
                                        }
                                        Ok(_) => {}
                                        Err(e) => warn!(
                                            "IntentRouterActor: state check error (non-fatal): {}",
                                            e
                                        ),
                                    }
                                }

                                // Check approval
                                let requires_approval = intent
                                    .get("requires_approval")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);

                                if requires_approval {
                                    return RouteResult::NeedsApproval {
                                        intent_name: intent_name.clone(),
                                        confidence: similarity,
                                    };
                                }

                                let handler = intent
                                    .get("handler")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let action = intent
                                    .get("action")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");

                                self.route_to_handler(
                                    &intent_name,
                                    &query_lower,
                                    handler,
                                    action,
                                    similarity,
                                )
                            }
                            None => RouteResult::Chat,
                        }
                    }
                    None => {
                        info!("IntentRouterActor: no intent matched (keyword or semantic), defaulting to Chat for query: {}", query);
                        RouteResult::Chat
                    }
                }
            }
        }
    }

    /// Check which required states are missing by querying the graph.
    /// Returns Ok(missing_states) where missing_states is a list of state names
    /// that don't exist in the graph.
    async fn check_state_requirements(
        &self,
        required_states: &[String],
    ) -> Result<Vec<String>, String> {
        // Query the graph for state nodes
        let (tx, rx) = oneshot::channel();
        self.memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Standard".to_string()),
                subtype: Some("build_state".to_string()),
                name: None,
                limit: None,
                reply_to: tx,
            })
            .await
            .map_err(|e| format!("Failed to send QueryAttrNodes: {}", e))?;

        let existing_states: Vec<String> = match rx.await {
            Ok(Ok(nodes)) => nodes
                .into_iter()
                .filter_map(|n| {
                    // Check if the state has been marked as active
                    n.get("active")
                        .and_then(|v| v.as_bool())
                        .filter(|&active| active)
                        .map(|_| n.name().to_string())
                })
                .collect(),
            Ok(Err(e)) => return Err(format!("QueryAttrNodes error: {}", e)),
            Err(e) => return Err(format!("QueryAttrNodes oneshot error: {}", e)),
        };

        // Find which required states are missing
        let missing: Vec<String> = required_states
            .iter()
            .filter(|req| !existing_states.contains(req))
            .cloned()
            .collect();

        Ok(missing)
    }

    /// Attempt to match a query to an intent via semantic (vector) search.
    ///
    /// Falls back to the memory graph's `SearchContext` which embeds the query
    /// and performs cosine similarity against all nodes. We post-filter for
    /// intent nodes (subtype="intent") and return the best match above threshold
    /// along with its similarity score.
    async fn semantic_match_intent(&self, query: &str) -> Option<(String, f64)> {
        let (tx, rx) = oneshot::channel();
        if self
            .memory_graph_tx
            .send(MemoryGraphMessage::SearchContext {
                query: query.to_string(),
                options: Some(SearchOptions {
                    top_k: Some(5),
                    threshold: Some(0.6),
                    node_types: None,
                    max_depth: Some(0),
                    include_structural: Some(false),
                    recency_weight: Some(0.0),
                }),
                reply_to: tx,
            })
            .await
            .is_err()
        {
            warn!("IntentRouterActor: failed to send SearchContext to memory graph");
            return None;
        }

        match rx.await {
            Ok(Ok(result)) => {
                // Post-filter for intent nodes (subtype="intent") above threshold
                for scored in &result.nodes {
                    if scored.node.subtype() == Some("intent")
                        && scored.similarity >= 0.6
                    {
                        return Some((scored.node.name().to_string(), scored.similarity));
                    }
                }
                None
            }
            Ok(Err(e)) => {
                warn!("IntentRouterActor: SearchContext error: {}", e);
                None
            }
            Err(e) => {
                warn!("IntentRouterActor: SearchContext oneshot error: {}", e);
                None
            }
        }
    }

    /// Route a matched intent to the appropriate handler based on config-driven
    /// `handler` and `action` fields from the intent node properties.
    ///
    /// This replaces the old hardcoded `route_to_handler()` with a dispatch table
    /// that reads from the config. The mapping is:
    ///
    /// | handler           | RouteResult variant  |
    /// |-------------------|----------------------|
    /// | BuildOrchestrator | Build                |
    /// | ErrorAnalyzer     | AnalyzeError         |
    /// | ProjectAnalyzer   | AnalyzeProject       |
    /// | ProjectQuery      | QueryProject         |
    /// | ToolOrchestrator  | ExecuteTool          |
    /// | (anything else)   | Chat                 |
    fn route_to_handler(
        &self,
        intent_name: &str,
        query_lower: &str,
        handler: &str,
        action: &str,
        confidence: f64,
    ) -> RouteResult {
        match handler {
            "BuildOrchestrator" => {
                let mut parameters = HashMap::new();
                parameters.insert("query".to_string(), query_lower.to_string());
                if !action.is_empty() {
                    parameters.insert("action".to_string(), action.to_string());
                }
                RouteResult::Build {
                    intent_name: intent_name.to_string(),
                    confidence,
                    parameters,
                }
            }
            "ErrorAnalyzer" => RouteResult::AnalyzeError {
                intent_name: intent_name.to_string(),
                confidence,
                error_text: query_lower.to_string(),
            },
            "ProjectAnalyzer" => RouteResult::AnalyzeProject {
                intent_name: intent_name.to_string(),
                confidence,
            },
            "ProjectQuery" => RouteResult::QueryProject {
                intent_name: intent_name.to_string(),
                confidence,
                query: query_lower.to_string(),
            },
            "ToolOrchestrator" => RouteResult::ExecuteTool {
                intent_name: intent_name.to_string(),
                confidence,
                tool_name: String::new(),
                parameters: HashMap::new(),
            },
            "PlanOrchestrator" => {
                let mut parameters = HashMap::new();
                parameters.insert("query".to_string(), query_lower.to_string());
                if !action.is_empty() {
                    parameters.insert("action".to_string(), action.to_string());
                }
                RouteResult::Plan {
                    intent_name: intent_name.to_string(),
                    confidence,
                    parameters,
                }
            }
            _ => {
                // Default to Chat for unrecognized handlers
                RouteResult::Chat
            }
        }
    }

    /// Query all intent nodes from the memory graph.
    async fn query_intents(&self) -> Option<Vec<spire_core::models::memory_graph::AttrNode>> {
        let (tx, rx) = oneshot::channel();
        if self
            .memory_graph_tx
            .send(MemoryGraphMessage::QueryAttrNodes {
                node_type: Some("Standard".to_string()),
                subtype: Some("intent".to_string()),
                name: None,
                limit: None,
                reply_to: tx,
            })
            .await
            .is_err()
        {
            warn!("IntentRouterActor: failed to send QueryAttrNodes to memory graph");
            return None;
        }

        match rx.await {
            Ok(Ok(nodes)) => {
                if nodes.is_empty() {
                    info!("IntentRouterActor: no intent nodes found in graph");
                }
                // Deduplicate by name as defense-in-depth against graph-level duplication
                let mut seen = std::collections::HashSet::new();
                let deduped: Vec<_> = nodes
                    .into_iter()
                    .filter(|n| seen.insert(n.name().to_string()))
                    .collect();
                Some(deduped)
            }
            Ok(Err(e)) => {
                warn!("IntentRouterActor: QueryAttrNodes error: {}", e);
                None
            }
            Err(e) => {
                warn!("IntentRouterActor: QueryAttrNodes oneshot error: {}", e);
                None
            }
        }
    }
}

#[async_trait]
impl Actor for IntentRouterActor {
    type Message = IntentRouterMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            IntentRouterMessage::RouteQuery { query, reply_to } => {
                let result = self.route_query(query).await;
                let _ = reply_to.send(result);
            }
        }
    }
}
