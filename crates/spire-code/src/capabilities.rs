// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **resolved profile** — a board's capabilities, read back from the graph.
//!
//! [`crate::capability_vocabulary`] says what a capability *is*; the codec
//! ([`crate::actors::platform_codec`] with `seeder_input`) writes it; this is the read side. It
//! walks `board -> realizes -> capability -> via -> chip` and collects the values the seeder stored
//! on each edge, so the answer comes from the same graph the writer wrote rather than a second parse
//! of the YAML — the two cannot disagree.
//!
//! Composed from the graph's own query (`GetRelationships`), so it needs no new storage and no
//! schema: an edge's `properties` already carry the values (`interface`, `cores`, `role`, …).

use serde_json::{json, Value};
use spire_core::models::memory_graph::{GraphEdge, RelationshipType};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use tokio::sync::mpsc;

/// Every edge touching `id`, in both directions — a realization points *from* the board and a
/// provision points *from* the chip, so an outgoing-only read would miss half the walk.
async fn edges_of(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    id: &str,
) -> anyhow::Result<Vec<GraphEdge>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    graph
        .send(MemoryGraphMessage::GetRelationships {
            node_id: id.to_string(),
            reply_to: tx,
        })
        .await
        .map_err(|_| anyhow::anyhow!("memory graph actor not available"))?;
    rx.await
        .map_err(|_| anyhow::anyhow!("memory graph response error"))?
}

/// An edge's stored values as plain JSON (`null` when it carries none).
fn values(edge: &GraphEdge) -> Value {
    serde_json::to_value(&edge.properties).unwrap_or(Value::Null)
}

/// The chip a capability is realized `via`, if the board named one.
fn via_chip(cap_edges: &[GraphEdge], capability: &str) -> Option<String> {
    cap_edges
        .iter()
        .find(|e| e.edge_type == RelationshipType::Via && e.from_id == capability)
        .map(|e| e.to_id.clone())
}

/// A chip's own values for one capability — its `provides` edge.
fn provided_values(chip_edges: &[GraphEdge], chip: &str, capability: &str) -> Option<Value> {
    chip_edges
        .iter()
        .find(|e| {
            e.edge_type == RelationshipType::Provides && e.from_id == chip && e.to_id == capability
        })
        .map(values)
}

/// The resolved profile for one `board`: every capability it realizes (with the board's own
/// values), the chip each is `via` and that chip's values for it, and the companion silicon it
/// carries.
///
/// The caller passes the graph, because the two entry points seed different stores: the app seeds
/// the shared knowledge store, the CLI the project graph. The walk is identical either way.
pub async fn resolved_profile(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    board_id: &str,
) -> anyhow::Result<Value> {
    let mut realizes: Vec<Value> = Vec::new();
    let mut carries: Vec<Value> = Vec::new();
    let mut pins: Vec<Value> = Vec::new();

    for edge in &edges_of(graph, board_id).await? {
        match &edge.edge_type {
            // The board realizes a capability: the board's values, plus the chip that provides it
            // (and that chip's values), when the board named one.
            RelationshipType::Realizes if edge.from_id == board_id => {
                let capability = edge.to_id.clone();
                let cap_edges = edges_of(graph, &capability).await?;
                let via = via_chip(&cap_edges, &capability);
                let provides = match &via {
                    Some(chip) => {
                        let chip_edges = edges_of(graph, chip).await?;
                        provided_values(&chip_edges, chip, &capability)
                    }
                    None => None,
                };
                realizes.push(json!({
                    "capability": capability,
                    "properties": values(edge),
                    "via": via,
                    "provides": provides,
                }));
            }
            // The companion silicon the board carries, with what is written beside it.
            RelationshipType::Carries if edge.from_id == board_id => {
                carries.push(json!({
                    "chip": edge.to_id,
                    "properties": values(edge),
                }));
            }
            // Wiring: its own node per function, and the edge carries the assignment. The function
            // is the node's name, recovered from its board-scoped id (`{board}/pins/{function}`).
            RelationshipType::Pins if edge.from_id == board_id => {
                let function = edge
                    .to_id
                    .strip_prefix(&format!("{board_id}/pins/"))
                    .unwrap_or(&edge.to_id)
                    .to_string();
                pins.push(json!({
                    "function": function,
                    "properties": values(edge),
                }));
            }
            _ => {}
        }
    }

    Ok(json!({
        "board": board_id,
        "realizes": realizes,
        "carries": carries,
        "pins": pins,
    }))
}
