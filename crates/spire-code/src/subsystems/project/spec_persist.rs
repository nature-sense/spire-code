// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Spec persistence** — how the memory-graph actor layer stores a project's
//! spec in its graph-native form.
//!
//! The decomposed spec ([`super::spec_graph::decompose`]) is written node by
//! node and edge by edge onto the memory graph:
//!
//! - one anchor node (legacy `Unknown`/`appspec`, name == project) stays the
//!   stable root and carries `goal`/`version` plus a rendered `spec_md` review
//!   copy — the **whole-JSON `spec` blob property is gone**;
//! - every decomposed node becomes an `AttrNode` (`node_type = "spec"`, its
//!   `spec_*` discriminator as `subtype`, logical spec name in the `logical`
//!   property, memory name scoped `{project}::{logical}` so upserts never
//!   collide across projects);
//! - every decomposed edge becomes a `Custom(<predicate>)` relationship
//!   between the stored node ids.
//!
//! [`load_spec_graph`] reassembles a `SpecGraph` from `QueryAttrNodes` +
//! `GetRelationships` and rebuilds the [`AppSpec`] via `reconstruct` — so the
//! memory graph round-trips a spec exactly (hard property, tested against the
//! full GIS reference).

use std::collections::{HashMap, HashSet};

use spire_core::models::memory_graph::{AttrNode, GraphEdge, RelationshipInput, RelationshipType};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use tracing::{info, warn};

use super::spec::AppSpec;
use super::spec_graph::{self, node, SpecGraph};

/// `node_type` for decomposed spec nodes (subtype carries the `spec_*`
/// discriminator). Distinct from the anchor's legacy `Unknown` so a project's
/// decomposition is one clean query away.
pub const MG_NODE_TYPE: &str = "spec";
/// Legacy anchor `node_type` (unchanged so existing appspec lookups hold).
pub const MG_ANCHOR_TYPE: &str = "Unknown";
/// Anchor subtype (as before Stage 4).
pub const MG_ANCHOR_SUBTYPE: &str = "appspec";

/// Property that records a node's logical spec name (before the project
/// prefix) so reads are unambiguous without reparsing names.
const PROP_LOGICAL: &str = "logical";
/// Complex (non-scalar) spec properties are serialized to canonical JSON
/// strings under this prefix; scalar values are stored verbatim.
const PROP_JSON_PREFIX: &str = "json:";
/// Human-readable Markdown review copy rendered on the anchor node.
const PROP_REVIEW_MD: &str = "spec_md";

const QUERY_LIMIT_ALL: u32 = 100_000;

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

fn mem_name(project: &str, logical: &str) -> String {
    format!("{project}::{logical}")
}

fn un_mem_name<'a>(project: &str, mem: &'a str) -> Option<&'a str> {
    mem.strip_prefix(&format!("{project}::"))
}

/// Property encoding: scalars verbatim, arrays/objects as canonical JSON
/// strings under `json:` so the open AttrNode store never loses structure.
fn encode_props(props: &[(String, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    let mut out = HashMap::with_capacity(props.len());
    for (k, v) in props {
        match v {
            serde_json::Value::String(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Bool(_) => {
                out.insert(k.clone(), v.clone());
            }
            _ => {
                out.insert(
                    format!("{PROP_JSON_PREFIX}{k}"),
                    serde_json::json!(v.to_string()),
                );
            }
        }
    }
    out
}

/// Inverse of [`encode_props`]: `json:`-prefixed keys are parsed back.
pub fn decode_props(map: &HashMap<String, serde_json::Value>) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::with_capacity(map.len());
    for (k, v) in map {
        if let Some(real) = k.strip_prefix(PROP_JSON_PREFIX) {
            if let Some(s) = v.as_str() {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                    out.push((real.to_string(), parsed));
                    continue;
                }
            }
            out.push((real.to_string(), v.clone()));
        } else if k != PROP_LOGICAL && k != PROP_REVIEW_MD {
            out.push((k.clone(), v.clone()));
        }
    }
    out
}

async fn merge_node(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    node: AttrNode,
) -> Result<AttrNode, String> {
    let (t, r) = tokio::sync::oneshot::channel();
    mg_tx
        .send(MemoryGraphMessage::MergeAttrNode { node, reply_to: t })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    r.await
        .map_err(|e| format!("merge reply lost: {e}"))?
        .map_err(|e| format!("merge failed: {e}"))
}

async fn create_rel(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    predicate: &str,
    from_id: &str,
    to_id: &str,
) -> Result<GraphEdge, String> {
    let (t, r) = tokio::sync::oneshot::channel();
    mg_tx
        .send(MemoryGraphMessage::CreateRelationship {
            rel: RelationshipInput {
                edge_type: RelationshipType::Custom(predicate.to_string()),
                from_id: from_id.to_string(),
                to_id: to_id.to_string(),
                properties: None,
                weight: None,
            },
            reply_to: t,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    r.await
        .map_err(|e| format!("relationship reply lost: {e}"))?
        .map_err(|e| format!("relationship failed: {e}"))
}

/// Persist a spec's full decomposition (anchor + one node per `SpecNode` + one
/// `Custom` relationship per `SpecEdge`). Best-effort for the pieces: the
/// caller already holds the spec; failures are logged. Returns the anchor's
/// stored id when the anchor itself persisted.
pub async fn store_spec_graph(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
    goal: &str,
    g: &SpecGraph,
) -> Option<String> {
    let root = g.nodes.iter().find(|n| n.node_type == node::APPSPEC);
    let Some(root) = root else {
        warn!("[SpecPersist] decompose must carry an appspec root node; nothing stored");
        return None;
    };

    // Human-readable review copy on the anchor (kept small; structured spec
    // lives in the decomposed nodes).
    let review_md = spec_graph::reconstruct(g)
        .ok()
        .map(|s| super::spec_md::spec_to_markdown(&s))
        .unwrap_or_default();

    let anchor = AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: MG_ANCHOR_TYPE.to_string(),
        subtype: Some(MG_ANCHOR_SUBTYPE.to_string()),
        name: project_name.to_string(),
        description: Some(goal.to_string()),
        properties: HashMap::from([
            ("goal".to_string(), serde_json::json!(goal)),
            ("version".to_string(), serde_json::json!(1)),
            (PROP_REVIEW_MD.to_string(), serde_json::json!(review_md)),
        ]),
        embedding_id: None,
        created_at: now(),
        updated_at: now(),
        version: 1,
    };
    let anchor_id = match merge_node(mg_tx, anchor).await {
        Ok(stored) => stored.id,
        Err(e) => {
            warn!("[SpecPersist] appspec anchor store failed for '{project_name}': {e}");
            return None;
        }
    };
    info!("[SpecPersist] stored appspec anchor name={project_name} id={anchor_id}");

    // One AttrNode per decomposed spec node, mapped by logical name → stored id.
    let mut id_by_logical: HashMap<String, String> = HashMap::new();
    id_by_logical.insert(root.name.clone(), anchor_id.clone());
    for n in g.nodes.iter() {
        if n.node_type == node::APPSPEC {
            continue;
        }
        let mut properties = encode_props(&n.properties);
        properties.insert(PROP_LOGICAL.to_string(), serde_json::json!(n.name));
        let child = AttrNode {
            id: uuid::Uuid::new_v4().to_string(),
            node_type: MG_NODE_TYPE.to_string(),
            subtype: Some(n.node_type.clone()),
            name: mem_name(project_name, &n.name),
            description: n.description.clone(),
            properties,
            embedding_id: None,
            created_at: now(),
            updated_at: now(),
            version: 1,
        };
        match merge_node(mg_tx, child).await {
            Ok(stored) => {
                id_by_logical.insert(n.name.clone(), stored.id);
            }
            Err(e) => warn!(
                "[SpecPersist] node store failed for '{}' ({}) in '{project_name}': {e}",
                n.name, n.node_type
            ),
        }
    }

    // One Custom relationship per spec edge.
    for e in &g.edges {
        match (
            id_by_logical.get(&e.from_name),
            id_by_logical.get(&e.to_name),
        ) {
            (Some(from), Some(to)) => {
                if let Err(err) = create_rel(mg_tx, &e.predicate, from, to).await {
                    warn!(
                        "[SpecPersist] edge '{}' {}->{} not stored for '{project_name}': {err}",
                        e.predicate, e.from_name, e.to_name
                    );
                }
            }
            _ => warn!(
                "[SpecPersist] edge '{}' {}->{} references an unstored node; skipped",
                e.predicate, e.from_name, e.to_name
            ),
        }
    }
    Some(anchor_id)
}

async fn query_nodes(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    node_type: Option<&str>,
    subtype: Option<&str>,
    name: Option<&str>,
    limit: u32,
) -> Result<Vec<AttrNode>, String> {
    let (t, r) = tokio::sync::oneshot::channel();
    mg_tx
        .send(MemoryGraphMessage::QueryAttrNodes {
            node_type: node_type.map(str::to_string),
            subtype: subtype.map(str::to_string),
            name: name.map(str::to_string),
            limit: Some(limit),
            reply_to: t,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    r.await
        .map_err(|e| format!("query reply lost: {e}"))?
        .map_err(|e| format!("query failed: {e}"))
}

async fn rels_of_node(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    node_id: &str,
) -> Result<Vec<GraphEdge>, String> {
    let (t, r) = tokio::sync::oneshot::channel();
    mg_tx
        .send(MemoryGraphMessage::GetRelationships {
            node_id: node_id.to_string(),
            reply_to: t,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    r.await
        .map_err(|e| format!("relationships reply lost: {e}"))?
        .map_err(|e| format!("relationships query failed: {e}"))
}

/// Reassemble a stored decomposition and rebuild the spec. Fails when the
/// anchor node is missing or the decomposition is inconsistent.
pub async fn load_spec_graph(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
) -> Result<AppSpec, String> {
    let mut g = SpecGraph::default();
    let mut id_to_logical: HashMap<String, String> = HashMap::new();

    // Anchor → appspec root node.
    let anchors = query_nodes(
        mg_tx,
        Some(MG_ANCHOR_TYPE),
        Some(MG_ANCHOR_SUBTYPE),
        Some(project_name),
        1,
    )
    .await?;
    let anchor = anchors
        .into_iter()
        .next()
        .ok_or_else(|| format!("no stored appspec for project '{project_name}'"))?;
    let mut anchor_props: Vec<(String, serde_json::Value)> = Vec::new();
    for (k, v) in &anchor.properties {
        if k != PROP_REVIEW_MD {
            anchor_props.push((k.clone(), v.clone()));
        }
    }
    g.nodes.push(spec_graph::SpecNode {
        node_type: node::APPSPEC.to_string(),
        name: project_name.to_string(),
        description: anchor.description.clone(),
        properties: anchor_props,
    });
    id_to_logical.insert(anchor.id.clone(), project_name.to_string());

    // Decomposed children (this project's slice of the shared graph DB).
    let children = query_nodes(mg_tx, Some(MG_NODE_TYPE), None, None, QUERY_LIMIT_ALL).await?;
    for c in &children {
        let Some(logical) = un_mem_name(project_name, &c.name) else {
            continue; // some other project's decomposition
        };
        let logical = logical.to_string();
        let props = decode_props(&c.properties);
        g.nodes.push(spec_graph::SpecNode {
            node_type: c.subtype.clone().unwrap_or_default(),
            name: logical.clone(),
            description: c.description.clone(),
            properties: props,
        });
        id_to_logical.insert(c.id.clone(), logical);
    }

    // Edges: every stored node reports its relationships (both directions), so
    // querying each node covers every edge. Dedupe by (predicate, from, to):
    // node upserts are idempotent, but re-storing a spec appends fresh
    // relationships, so the logical edge set must not double up.
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let all_mem_ids: Vec<String> = {
        let mut ids = Vec::with_capacity(1 + children.len());
        ids.push(anchor.id.clone());
        for c in &children {
            if un_mem_name(project_name, &c.name).is_some() {
                ids.push(c.id.clone());
            }
        }
        ids
    };
    for id in all_mem_ids {
        for rel in rels_of_node(mg_tx, &id).await? {
            let RelationshipType::Custom(predicate) = &rel.edge_type else {
                continue;
            };
            let (Some(from), Some(to)) = (
                id_to_logical.get(&rel.from_id),
                id_to_logical.get(&rel.to_id),
            ) else {
                continue; // relationship to a non-spec node (e.g. GENERATED_FROM)
            };
            if !seen.insert((predicate.clone(), from.clone(), to.clone())) {
                continue;
            }
            g.edges.push(spec_graph::SpecEdge {
                predicate: predicate.clone(),
                from_name: from.clone(),
                to_name: to.clone(),
                properties: Vec::new(),
            });
        }
    }

    spec_graph::reconstruct(&g)
}

/// Store the whole decomposition AND read it back — used to (re)link codegen
/// flows when only the graph store is available.
pub async fn roundtrip_spec_graph(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
    goal: &str,
    g: &SpecGraph,
) -> Option<AppSpec> {
    store_spec_graph(mg_tx, project_name, goal, g).await?;
    load_spec_graph(mg_tx, project_name).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    /// In-memory double of the memory-graph actor: real merge-by-key
    /// semantics, filtered queries and both-direction relationship queries.
    struct FakeGraph {
        nodes: Arc<Mutex<Vec<AttrNode>>>,
        edges: Arc<Mutex<Vec<GraphEdge>>>,
    }

    impl FakeGraph {
        fn spawn(&self, mut rx: mpsc::Receiver<MemoryGraphMessage>) {
            let nodes = self.nodes.clone();
            let edges = self.edges.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("fake graph runtime");
                rt.block_on(async move {
                    while let Some(msg) = rx.recv().await {
                        match msg {
                            MemoryGraphMessage::MergeAttrNode { node, reply_to } => {
                                let mut list = nodes.lock().unwrap();
                                let key = |n: &AttrNode| {
                                    (n.node_type.clone(), n.subtype.clone(), n.name.clone())
                                };
                                let existing = list.iter().find(|n| key(n) == key(&node)).cloned();
                                let reply = match existing {
                                    Some(prev) => prev,
                                    None => {
                                        list.push(node.clone());
                                        node
                                    }
                                };
                                drop(list);
                                let _ = reply_to.send(Ok(reply));
                            }
                            MemoryGraphMessage::QueryAttrNodes {
                                node_type,
                                subtype,
                                name,
                                limit,
                                reply_to,
                            } => {
                                let out: Vec<AttrNode> = nodes
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .filter(|n| {
                                        node_type.as_deref().is_none_or(|t| n.node_type == t)
                                    })
                                    .filter(|n| {
                                        subtype
                                            .as_deref()
                                            .is_none_or(|s| n.subtype.as_deref() == Some(s))
                                    })
                                    .filter(|n| name.as_deref().is_none_or(|x| n.name == x))
                                    .take(limit.unwrap_or(u32::MAX) as usize)
                                    .cloned()
                                    .collect();
                                let _ = reply_to.send(Ok(out));
                            }
                            MemoryGraphMessage::CreateRelationship { rel, reply_to } => {
                                let edge = GraphEdge {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    edge_type: rel.edge_type.clone(),
                                    from_id: rel.from_id.clone(),
                                    to_id: rel.to_id.clone(),
                                    properties: rel.properties.clone().unwrap_or_default(),
                                    created_at: now(),
                                    weight: rel.weight,
                                };
                                edges.lock().unwrap().push(edge.clone());
                                let _ = reply_to.send(Ok(edge));
                            }
                            MemoryGraphMessage::GetRelationships { node_id, reply_to } => {
                                let out: Vec<GraphEdge> = edges
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .filter(|e| e.from_id == node_id || e.to_id == node_id)
                                    .cloned()
                                    .collect();
                                let _ = reply_to.send(Ok(out));
                            }
                            _ => {}
                        }
                    }
                });
            });
        }

        fn nodes(&self) -> Vec<AttrNode> {
            self.nodes.lock().unwrap().clone()
        }

        fn edges(&self) -> Vec<GraphEdge> {
            self.edges.lock().unwrap().clone()
        }
    }

    fn fake_pair() -> (mpsc::Sender<MemoryGraphMessage>, FakeGraph) {
        let (tx, rx) = mpsc::channel(128);
        let fake = FakeGraph {
            nodes: Arc::new(Mutex::new(Vec::new())),
            edges: Arc::new(Mutex::new(Vec::new())),
        };
        fake.spawn(rx);
        (tx, fake)
    }

    fn reference_spec() -> AppSpec {
        let raw = include_str!("../../../../../docs/spire-gis.appspec.json");
        serde_json::from_str(raw).expect("reference spec parses")
    }

    #[test]
    fn full_reference_spec_roundtrips_through_the_memory_graph() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let spec = reference_spec();
        let g = spec_graph::decompose(&spec);
        let (tx, fake) = fake_pair();

        let anchor = rt.block_on(store_spec_graph(&tx, "spire-gis", &spec.app.goal, &g));
        assert!(anchor.is_some());

        let stored = fake.nodes();
        assert_eq!(stored.len(), g.nodes.len(), "one AttrNode per SpecNode");
        let anchor_node = stored
            .iter()
            .find(|n| n.subtype.as_deref() == Some(MG_ANCHOR_SUBTYPE))
            .expect("anchor present");
        assert!(
            !anchor_node.properties.contains_key("spec"),
            "no whole-JSON blob property remains"
        );
        assert!(anchor_node.properties.contains_key(PROP_REVIEW_MD));
        for subtype in [
            "spec_type",
            "spec_actor",
            "spec_method",
            "spec_screen",
            "spec_layout",
        ] {
            assert!(
                stored.iter().any(|n| n.subtype.as_deref() == Some(subtype)),
                "stored decomposition must contain '{subtype}' nodes"
            );
        }
        assert_eq!(
            fake.edges().len(),
            g.edges.len(),
            "one Custom relationship per SpecEdge"
        );

        // The anchor's rendered Markdown is the wizard review copy: it parses
        // back to the same spec (modulo the bridge, which the graph carries
        // separately but Markdown does not repeat).
        let review_md = anchor_node
            .properties
            .get(PROP_REVIEW_MD)
            .and_then(|v| v.as_str())
            .expect("anchor carries spec_md");
        let via_md = super::super::spec_md::markdown_to_spec(review_md).expect("md parses");
        assert_eq!(via_md.app, spec.app);
        assert_eq!(via_md.types, spec.types);
        assert_eq!(via_md.graph, spec.graph);
        assert_eq!(via_md.actors, spec.actors);
        assert_eq!(via_md.ui, spec.ui);

        let back = rt
            .block_on(load_spec_graph(&tx, "spire-gis"))
            .expect("decomposition reloads");
        assert_eq!(back, spec);
    }

    #[test]
    fn reserializing_a_project_upserts_onto_the_same_decomposition() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let spec = reference_spec();
        let g = spec_graph::decompose(&spec);
        let (tx, fake) = fake_pair();

        for _ in 0..2 {
            let anchor = rt.block_on(store_spec_graph(&tx, "spire-gis", &spec.app.goal, &g));
            assert!(anchor.is_some());
        }

        let stored = fake.nodes();
        assert_eq!(
            stored.len(),
            g.nodes.len(),
            "second store merges onto existing nodes (ids stable)"
        );
        // Node upserts are idempotent; relationships are append-only in the
        // store, so load dedupes by (predicate, from, to).
        assert!(fake.edges().len() >= g.edges.len());
        let back = rt
            .block_on(load_spec_graph(&tx, "spire-gis"))
            .expect("decomposition reloads after re-store");
        assert_eq!(back, spec);
    }

    #[test]
    fn load_errors_on_missing_project() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, _fake) = fake_pair();
        let err = rt
            .block_on(load_spec_graph(&tx, "no-such-project"))
            .unwrap_err();
        assert!(err.contains("no stored appspec"));
    }
}
