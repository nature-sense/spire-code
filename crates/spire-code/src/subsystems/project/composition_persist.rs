// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Composition persistence** — how the memory-graph actor layer stores a project's IDF
//! composition in its graph-native form.
//!
//! The decomposed composition ([`super::composition_graph::decompose`]) is written node by node and
//! edge by edge onto the memory graph:
//!
//! - one **anchor** node (`node_type = "idf_composition"`, `subtype = "anchor"`, name == the project)
//!   is the stable, project-scoped root; it carries the framework, the justification and a rendered
//!   `composition.spire` copy — the file projection, never the source;
//! - every decomposed node becomes an `AttrNode` (`node_type = "idf_composition"`, its `node::`
//!   discriminator as `subtype`, the logical name in the `logical` property, memory name scoped
//!   `{project}::{logical}` so upserts never collide across projects);
//! - every decomposed edge becomes a `Custom(<predicate>)` relationship between the stored node ids.
//!
//! [`load_composition_graph`] reassembles a [`CompositionGraph`] from `QueryAttrNodes` +
//! `GetRelationships` and rebuilds the [`ApplicationSpec`] via
//! [`reconstruct`](super::composition_graph::reconstruct) — so the memory graph round-trips a
//! composition exactly (hard property, tested against the PM2.5 fixture).
//!
//! The AttrNode store plumbing (the property codec, the merge/query/relationship helpers and the
//! `{project}::{logical}` scoping) is shared with [`super::spec_persist`], the sibling writer for the
//! native `AppSpec`.
//!
//! # The design record
//!
//! Storing a composition is not the same as **deciding** one — a person may edit `composition.spire`
//! and have the graph re-written from it long after the design was agreed — so the decision is stored
//! as a node of its own. [`store_design_record`] writes a `design_record` child of the anchor (via
//! `HAS_DESIGN_RECORD`) carrying the door the design came through, when it was decided, and the
//! fingerprint of the composition it ratifies; [`load_design_freshness`] compares that fingerprint
//! with the composition in the graph, so a reader is *told* whether the record still stands
//! ([`DesignFreshness`]) rather than left to assume it. A composition edited after the decision is
//! [`DesignFreshness::Stale`]; a project whose composition was stored with no decision recorded is
//! [`DesignFreshness::Absent`].

use std::collections::{HashMap, HashSet};

use spire_core::models::memory_graph::{AttrNode, RelationshipType};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use tracing::{info, warn};

use super::composition_graph::{
    self, edge, node, CompositionEdge, CompositionGraph, CompositionNode, DesignFreshness,
    DesignRecord, DESIGN_RECORD_NAME, ROOT,
};
use super::spec_persist::{
    create_rel, decode_props, encode_props, mem_name, merge_node, query_nodes, rels_of_node,
    un_mem_name, PROP_LOGICAL, QUERY_LIMIT_ALL,
};
use crate::build::application_spec::ApplicationSpec;

/// `node_type` for every decomposed composition node (the `node::` subtype carries the kind).
pub const MG_NODE_TYPE: &str = "idf_composition";

/// Property carrying the rendered `composition.spire` review copy on the anchor.
pub const PROP_COMPOSITION: &str = "composition";
/// Property carrying the stored node's version.
const PROP_VERSION: &str = "version";

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

/// Persist a composition's full decomposition (anchor + one node per `CompositionNode` + one `Custom`
/// relationship per `CompositionEdge`). Best-effort for the pieces: the caller already holds the spec;
/// failures are logged. Returns the anchor's stored id when the anchor itself persisted.
pub async fn store_composition_graph(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
    spec: &ApplicationSpec,
    g: &CompositionGraph,
) -> Option<String> {
    let Some(anchor_node) = g.nodes.iter().find(|n| n.node_type == node::ANCHOR) else {
        warn!("[CompositionPersist] decompose must carry an anchor node; nothing stored");
        return None;
    };

    // Rendered `composition.spire` (YAML) review copy on the anchor — the file projection of the
    // graph, kept for a reader; the structured composition lives in the decomposed nodes.
    let review = serde_yaml::to_string(spec).unwrap_or_default();

    let mut anchor_props = encode_props(&anchor_node.properties);
    anchor_props.insert(PROP_VERSION.to_string(), serde_json::json!(1));
    anchor_props.insert(PROP_COMPOSITION.to_string(), serde_json::json!(review));

    let anchor = AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: MG_NODE_TYPE.to_string(),
        subtype: Some(node::ANCHOR.to_string()),
        name: project_name.to_string(),
        description: Some(spec.justification.clone()),
        properties: anchor_props,
        embedding_id: None,
        created_at: now(),
        updated_at: now(),
        version: 1,
    };
    let anchor_id = match merge_node(mg_tx, anchor).await {
        Ok(stored) => stored.id,
        Err(e) => {
            warn!("[CompositionPersist] anchor store failed for '{project_name}': {e}");
            return None;
        }
    };
    info!("[CompositionPersist] stored composition anchor name={project_name} id={anchor_id}");

    // One AttrNode per decomposed node, mapped by logical name → stored id.
    let mut id_by_logical: HashMap<String, String> = HashMap::new();
    id_by_logical.insert(ROOT.to_string(), anchor_id.clone());
    for n in g.nodes.iter() {
        if n.node_type == node::ANCHOR {
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
                "[CompositionPersist] node store failed for '{}' ({}) in '{project_name}': {e}",
                n.name, n.node_type
            ),
        }
    }

    // One Custom relationship per composition edge.
    for e in &g.edges {
        match (
            id_by_logical.get(&e.from_name),
            id_by_logical.get(&e.to_name),
        ) {
            (Some(from), Some(to)) => {
                if let Err(err) = create_rel(mg_tx, &e.predicate, from, to).await {
                    warn!(
                        "[CompositionPersist] edge '{}' {}->{} not stored for '{project_name}': {err}",
                        e.predicate, e.from_name, e.to_name
                    );
                }
            }
            _ => warn!(
                "[CompositionPersist] edge '{}' {}->{} references an unstored node; skipped",
                e.predicate, e.from_name, e.to_name
            ),
        }
    }
    Some(anchor_id)
}

/// Reassemble a stored composition and rebuild the spec. Fails when the anchor node is missing or the
/// decomposition is inconsistent.
pub async fn load_composition_graph(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
) -> Result<ApplicationSpec, String> {
    let mut g = CompositionGraph::default();
    let mut id_to_logical: HashMap<String, String> = HashMap::new();

    // Anchor → composition root node.
    let anchors = query_nodes(
        mg_tx,
        Some(MG_NODE_TYPE),
        Some(node::ANCHOR),
        Some(project_name),
        1,
    )
    .await?;
    let anchor = anchors
        .into_iter()
        .next()
        .ok_or_else(|| format!("no stored composition for project '{project_name}'"))?;
    g.nodes.push(CompositionNode {
        node_type: node::ANCHOR.to_string(),
        name: ROOT.to_string(),
        description: anchor.description.clone(),
        properties: decode_props(&anchor.properties),
    });
    id_to_logical.insert(anchor.id.clone(), ROOT.to_string());

    // Decomposed children (this project's slice of the shared graph DB).
    let children = query_nodes(mg_tx, Some(MG_NODE_TYPE), None, None, QUERY_LIMIT_ALL).await?;
    for c in &children {
        if c.subtype.as_deref() == Some(node::ANCHOR) {
            continue;
        }
        let Some(logical) = un_mem_name(project_name, &c.name) else {
            continue; // some other project's composition
        };
        let logical = logical.to_string();
        g.nodes.push(CompositionNode {
            node_type: c.subtype.clone().unwrap_or_default(),
            name: logical.clone(),
            description: c.description.clone(),
            properties: decode_props(&c.properties),
        });
        id_to_logical.insert(c.id.clone(), logical);
    }

    // Edges: every stored node reports its relationships (both directions), so querying each node
    // covers every edge. Dedupe by (predicate, from, to): node upserts are idempotent, but re-storing
    // appends fresh relationships, so the logical edge set must not double up.
    let all_mem_ids: Vec<String> = {
        let mut ids = Vec::with_capacity(1 + children.len());
        ids.push(anchor.id.clone());
        for c in &children {
            if c.subtype.as_deref() != Some(node::ANCHOR)
                && un_mem_name(project_name, &c.name).is_some()
            {
                ids.push(c.id.clone());
            }
        }
        ids
    };
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    for id in all_mem_ids {
        for rel in rels_of_node(mg_tx, &id).await? {
            let RelationshipType::Custom(predicate) = &rel.edge_type else {
                continue;
            };
            let (Some(from), Some(to)) = (
                id_to_logical.get(&rel.from_id),
                id_to_logical.get(&rel.to_id),
            ) else {
                continue; // relationship to a non-composition node
            };
            if !seen.insert((predicate.clone(), from.clone(), to.clone())) {
                continue;
            }
            g.edges.push(CompositionEdge {
                predicate: predicate.clone(),
                from_name: from.clone(),
                to_name: to.clone(),
            });
        }
    }

    composition_graph::reconstruct(&g)
}

/// Store the whole decomposition AND read it back — for callers that only have the graph store.
pub async fn roundtrip_composition_graph(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
    spec: &ApplicationSpec,
    g: &CompositionGraph,
) -> Option<ApplicationSpec> {
    store_composition_graph(mg_tx, project_name, spec, g).await?;
    load_composition_graph(mg_tx, project_name).await.ok()
}

/// Persist the **design record** — the decision, not the composition — as a child of the project's
/// anchor.
///
/// The anchor has to exist first: the record is a fact *about* a composition, and a record with nothing
/// to ratify is worse than no record, because a reader would take it for a decided design. So a project
/// whose composition was never stored returns `None` rather than being given one by the back door.
///
/// Returns the stored record node's id. Relationships are append-only in the store, so re-deciding a
/// design appends another `HAS_DESIGN_RECORD` to the same (upserted) node — a reader that wants the
/// link rather than the node should dedupe, as [`load_composition_graph`] does.
pub async fn store_design_record(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
    record: &DesignRecord,
) -> Option<String> {
    let anchors = match query_nodes(
        mg_tx,
        Some(MG_NODE_TYPE),
        Some(node::ANCHOR),
        Some(project_name),
        1,
    )
    .await
    {
        Ok(nodes) => nodes,
        Err(e) => {
            warn!("[CompositionPersist] anchor lookup failed for '{project_name}': {e}");
            return None;
        }
    };
    let Some(anchor) = anchors.into_iter().next() else {
        warn!(
            "[CompositionPersist] no composition is stored for '{project_name}', so there is nothing \
             for a design record to ratify; not stored"
        );
        return None;
    };

    let spec_node = composition_graph::design_record_node(record);
    let mut properties = encode_props(&spec_node.properties);
    properties.insert(PROP_LOGICAL.to_string(), serde_json::json!(spec_node.name));
    let node = AttrNode {
        id: uuid::Uuid::new_v4().to_string(),
        node_type: MG_NODE_TYPE.to_string(),
        subtype: Some(node::DESIGN_RECORD.to_string()),
        name: mem_name(project_name, &spec_node.name),
        description: spec_node.description.clone(),
        properties,
        embedding_id: None,
        created_at: now(),
        updated_at: now(),
        version: 1,
    };
    let stored = match merge_node(mg_tx, node).await {
        Ok(stored) => stored,
        Err(e) => {
            warn!("[CompositionPersist] design record store failed for '{project_name}': {e}");
            return None;
        }
    };
    if let Err(e) = create_rel(mg_tx, edge::HAS_DESIGN_RECORD, &anchor.id, &stored.id).await {
        warn!(
            "[CompositionPersist] design record {} not linked to the anchor for \
             '{project_name}': {e}",
            stored.id
        );
    }
    info!(
        "[CompositionPersist] stored design record for '{project_name}': v{} from {} ({})",
        record.version,
        record.source.as_str(),
        record.fingerprint
    );
    Some(stored.id)
}

/// Read a project's stored [`DesignRecord`], when it has one.
///
/// `Ok(None)` means **no design was ever decided**, and never "the record is broken": a record that is
/// present and unreadable is an `Err`, because a caller that confused the two would offer to re-design
/// over a decision nobody superseded.
pub async fn load_design_record(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
) -> Result<Option<DesignRecord>, String> {
    let nodes = query_nodes(
        mg_tx,
        Some(MG_NODE_TYPE),
        Some(node::DESIGN_RECORD),
        Some(&mem_name(project_name, DESIGN_RECORD_NAME)),
        QUERY_LIMIT_ALL,
    )
    .await?;
    let Some(node) = nodes.into_iter().next() else {
        return Ok(None);
    };
    let g = CompositionGraph {
        nodes: vec![CompositionNode {
            node_type: node::DESIGN_RECORD.to_string(),
            name: DESIGN_RECORD_NAME.to_string(),
            description: node.description.clone(),
            properties: decode_props(&node.properties),
        }],
        edges: Vec::new(),
    };
    match composition_graph::reconstruct_design_record(&g)? {
        Some(record) => Ok(Some(record)),
        None => Err(format!(
            "the stored design record for '{project_name}' is present but unreadable"
        )),
    }
}

/// **Does the stored design still ratify the stored composition?** — [`DesignFreshness`], read out of
/// the memory graph.
///
/// `Err` only when something is stored and broken, or when the project states no composition at all:
/// this module's loaders agree that a project with nothing stored is an error naming the project
/// rather than a silently empty answer. A composition that *is* stored with no decision recorded is
/// [`DesignFreshness::Absent`] — the honest "nobody has designed this yet".
pub async fn load_design_freshness(
    mg_tx: &tokio::sync::mpsc::Sender<MemoryGraphMessage>,
    project_name: &str,
) -> Result<DesignFreshness, String> {
    let spec = load_composition_graph(mg_tx, project_name).await?;
    let record = load_design_record(mg_tx, project_name).await?;
    Ok(composition_graph::freshness(record.as_ref(), &spec))
}

#[cfg(test)]
mod tests {
    use super::composition_graph::{fingerprint, DesignSource};
    use super::*;
    use crate::build::application_spec::{examples, parse_spec};
    use spire_core::models::memory_graph::GraphEdge;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    /// In-memory double of the memory-graph actor: real merge-by-key semantics, filtered queries and
    /// both-direction relationship queries (mirrors `spec_persist`'s double).
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

    fn pm25() -> ApplicationSpec {
        parse_spec(examples::PM25_METER).expect("the PM2.5 example parses")
    }

    #[test]
    fn storing_then_loading_a_composition_roundtrips_through_the_memory_graph() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let spec = pm25();
        let g = composition_graph::decompose(&spec);
        let (tx, fake) = fake_pair();

        let anchor = rt.block_on(store_composition_graph(&tx, "pm25-reader", &spec, &g));
        assert!(anchor.is_some(), "the anchor persisted");

        let stored = fake.nodes();
        assert_eq!(
            stored.len(),
            g.nodes.len(),
            "one AttrNode per CompositionNode"
        );
        let anchor_node = stored
            .iter()
            .find(|n| n.subtype.as_deref() == Some(node::ANCHOR))
            .expect("anchor present");
        assert_eq!(anchor_node.name, "pm25-reader", "the anchor is the project");
        assert_eq!(anchor_node.node_type, MG_NODE_TYPE);
        assert!(
            anchor_node.properties.contains_key(PROP_COMPOSITION),
            "the anchor carries the rendered composition"
        );
        assert!(
            anchor_node.properties.contains_key("json:wiring"),
            "the wiring list is stored as JSON"
        );
        assert!(
            !stored.iter().any(|n| n.properties.contains_key("wiring")),
            "wiring is not stored raw (it is a complex property)"
        );
        assert_eq!(
            fake.edges().len(),
            g.edges.len(),
            "one Custom relationship per CompositionEdge"
        );

        let back = rt
            .block_on(load_composition_graph(&tx, "pm25-reader"))
            .expect("the composition reloads");
        assert_eq!(back, spec);
    }

    #[test]
    fn restoring_a_composition_upserts_nodes_and_load_dedupes_relationships() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let spec = pm25();
        let g = composition_graph::decompose(&spec);
        let (tx, fake) = fake_pair();

        for _ in 0..2 {
            let anchor = rt.block_on(store_composition_graph(&tx, "pm25-reader", &spec, &g));
            assert!(anchor.is_some());
        }

        assert_eq!(
            fake.nodes().len(),
            g.nodes.len(),
            "the second store merges onto existing nodes (ids stable)"
        );
        // Node upserts are idempotent; relationships are append-only in the store, so load dedupes.
        assert!(fake.edges().len() >= g.edges.len());
        let back = rt
            .block_on(load_composition_graph(&tx, "pm25-reader"))
            .expect("the composition reloads after a re-store");
        assert_eq!(back, spec);
    }

    #[test]
    fn load_errors_on_a_missing_project() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, _fake) = fake_pair();
        let err = rt
            .block_on(load_composition_graph(&tx, "no-such-project"))
            .unwrap_err();
        assert!(err.contains("no stored composition"));
    }

    fn stored_edges(fake: &FakeGraph, predicate: &str) -> usize {
        fake.edges()
            .iter()
            .filter(|e| matches!(&e.edge_type, RelationshipType::Custom(p) if p == predicate))
            .count()
    }

    #[test]
    fn a_design_record_hangs_off_the_anchor_and_reloads_without_touching_the_composition() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let spec = pm25();
        let g = composition_graph::decompose(&spec);
        let (tx, fake) = fake_pair();
        rt.block_on(store_composition_graph(&tx, "pm25-reader", &spec, &g))
            .expect("the composition persisted first");

        let record = DesignRecord::now(&spec, DesignSource::CompositionFile, 2);
        let id = rt.block_on(store_design_record(&tx, "pm25-reader", &record));
        assert!(id.is_some(), "the record persisted with its anchor present");

        let stored = fake.nodes();
        let record_node = stored
            .iter()
            .find(|n| n.subtype.as_deref() == Some(node::DESIGN_RECORD))
            .expect("the record node is stored");
        assert_eq!(
            record_node.name, "pm25-reader::design_record",
            "the record is scoped to its project like every other node"
        );
        assert!(
            record_node.properties.contains_key("fingerprint"),
            "the fingerprint is what makes the record checkable"
        );
        let anchor_node = stored
            .iter()
            .find(|n| n.subtype.as_deref() == Some(node::ANCHOR))
            .expect("anchor present");
        assert_eq!(
            stored_edges(&fake, edge::HAS_DESIGN_RECORD),
            1,
            "the record hangs off the anchor"
        );
        let link = fake
            .edges()
            .into_iter()
            .find(|e| {
                matches!(&e.edge_type, RelationshipType::Custom(p) if p == edge::HAS_DESIGN_RECORD)
            })
            .expect("the link exists");
        assert_eq!(
            (link.from_id, link.to_id),
            (anchor_node.id.clone(), record_node.id.clone())
        );

        assert_eq!(
            rt.block_on(load_design_record(&tx, "pm25-reader"))
                .expect("the record reloads"),
            Some(record)
        );
        // The record is a child of the same project, so the composition read back has to be unmoved by
        // it: a record is a fact about a composition, never part of one.
        assert_eq!(
            rt.block_on(load_composition_graph(&tx, "pm25-reader"))
                .expect("the composition reloads"),
            spec
        );
    }

    #[test]
    fn a_design_record_needs_a_composition_to_ratify() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, _fake) = fake_pair();
        let record = DesignRecord::now(&pm25(), DesignSource::Model, 1);

        assert!(
            rt.block_on(store_design_record(&tx, "nowhere", &record))
                .is_none(),
            "a record with nothing to ratify is not stored"
        );
        assert_eq!(
            rt.block_on(load_design_record(&tx, "nowhere"))
                .expect("loads"),
            None
        );
        let err = rt
            .block_on(load_design_freshness(&tx, "nowhere"))
            .unwrap_err();
        assert!(err.contains("no stored composition"), "{err}");
    }

    #[test]
    fn freshness_is_absent_before_a_decision_and_current_after_one() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let spec = pm25();
        let g = composition_graph::decompose(&spec);
        let (tx, _fake) = fake_pair();
        rt.block_on(store_composition_graph(&tx, "pm25-reader", &spec, &g))
            .expect("the composition persisted");

        assert_eq!(
            rt.block_on(load_design_freshness(&tx, "pm25-reader"))
                .expect("freshness reads"),
            DesignFreshness::Absent,
            "a composition nobody has decided is undecided, which is not the same as stale"
        );

        let record = DesignRecord::now(&spec, DesignSource::Answers, 1);
        rt.block_on(store_design_record(&tx, "pm25-reader", &record))
            .expect("the record persisted");
        assert_eq!(
            rt.block_on(load_design_freshness(&tx, "pm25-reader"))
                .expect("freshness reads"),
            DesignFreshness::Current
        );
    }

    #[test]
    fn a_record_decided_from_another_design_reads_as_stale() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // The graph holds a design the record was never decided from: the record ratifies the PM2.5
        // meter, while what is stored is something else entirely.
        let stored = parse_spec(examples::INSECT_TRAP).expect("the insect trap example parses");
        let g = composition_graph::decompose(&stored);
        let (tx, _fake) = fake_pair();
        rt.block_on(store_composition_graph(&tx, "trap", &stored, &g))
            .expect("the composition persisted");
        let record = DesignRecord::now(&pm25(), DesignSource::Answers, 1);
        rt.block_on(store_design_record(&tx, "trap", &record))
            .expect("the record persisted");

        match rt
            .block_on(load_design_freshness(&tx, "trap"))
            .expect("freshness reads")
        {
            DesignFreshness::Stale { recorded, current } => {
                assert_eq!(recorded, record.fingerprint);
                assert_eq!(current, fingerprint(&stored));
            }
            other => panic!("a record from another design must read as stale, got {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_stored_record_is_an_error_not_an_absent_one() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let spec = pm25();
        let g = composition_graph::decompose(&spec);
        let (tx, fake) = fake_pair();
        rt.block_on(store_composition_graph(&tx, "pm25-reader", &spec, &g))
            .expect("the composition persisted");

        // What a half-written or foreign graph holds: the right node, none of the provenance.
        let mut properties = HashMap::new();
        properties.insert("framework".to_string(), serde_json::json!("actors"));
        fake.nodes.lock().unwrap().push(AttrNode {
            id: "corrupt".to_string(),
            node_type: MG_NODE_TYPE.to_string(),
            subtype: Some(node::DESIGN_RECORD.to_string()),
            name: mem_name("pm25-reader", DESIGN_RECORD_NAME),
            description: None,
            properties,
            embedding_id: None,
            created_at: now(),
            updated_at: now(),
            version: 1,
        });

        let err = rt
            .block_on(load_design_record(&tx, "pm25-reader"))
            .unwrap_err();
        assert!(
            err.contains("design record") && err.contains("source_kind"),
            "the refusal names the record and the property it is missing: {err}"
        );
        assert!(
            rt.block_on(load_design_freshness(&tx, "pm25-reader"))
                .is_err(),
            "a broken record is never reported as merely undecided"
        );
    }
}
