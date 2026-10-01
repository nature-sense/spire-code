// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Component edit history** — "why does this file look like this", written down.
//!
//! An `idf_component_edit` run is a typed instruction from a person to the model. Its report says
//! what *happened*; the instruction says what was *asked* — and until it is written down, the answer
//! to "why is `reset()` declared out of line here?" lives only in a chat transcript. This module puts
//! both in the memory graph, where they are queryable per component ("every edit that touched
//! `sps30`", "which instructions did the gate reject") instead of remembered.
//!
//! Three pieces, on the **project graph** the coordinator owns
//! (`CoordinatorActor::memory_graph_tx`) — the same store `spec_persist` writes a project's
//! composition into, and as persistent as it is (SeleneDB snapshot + WAL, recovered on restart):
//!
//! - a [`MODULE_TYPE`] node, one per component, named `{library}::{component}` — the stable spine a
//!   history hangs off, so records accrete per component instead of floating free;
//! - an [`EDIT_RECORD_TYPE`] node per run, carrying the instruction and the gate's verdict;
//! - a `has_edit` edge from the module to the record, which is what makes "this component's
//!   history" one traversal rather than a scan.
//!
//! **Best-effort by design.** By the time this runs the edit has already passed its gate, so a graph
//! that is closed or a store that rejects the write is a lost note, not a lost edit: a caller logs
//! and carries on. The failure mode to avoid is turning a good edit into a failed one because a
//! memory write did not land.
//!
//! **Nothing here reads the model's own words.** What the model returns is a plan — a list of paths
//! from the scope it was given, and the contents for each; everything else in the reply is prose the
//! parser drops, so the model could not describe its own intent even if it tried. The instruction is
//! the caller's, and it is the caller's words that are recorded — verbatim.

use std::collections::HashMap;

use spire_core::models::memory_graph::{AttrNode, RelationshipInput, RelationshipType};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

/// `node_type` for a component's stable spine node — one per edited component.
pub const MODULE_TYPE: &str = "Module";
/// `subtype` for that spine: an IDF component (as opposed to some other module kind later).
pub const MODULE_SUBTYPE: &str = "idf_component";
/// `node_type` for one edit run.
pub const EDIT_RECORD_TYPE: &str = "EditRecord";
/// `subtype` for an edit run against an IDF component. Distinct from [`MODULE_SUBTYPE`] so a history
/// query is exact — the module node carries the same `component` property, and a filter that cannot
/// tell them apart would return the spine alongside its records.
pub const EDIT_SUBTYPE: &str = "idf_component_edit";

/// How many prior records the tool response carries. Enough to show the pattern of what has been
/// asked of a component; the graph holds all of them for a caller that walks the edges.
pub const RESPONSE_HISTORY_LIMIT: u32 = 5;

/// A runaway prompt should not become a runaway node. Long enough for a real instruction with a
/// pasted excerpt, short enough that one edit cannot dominate the store.
const MAX_INSTRUCTION_CHARS: usize = 4000;
/// A node `description` is a label, not a document.
const MAX_DESCRIPTION_CHARS: usize = 160;

/// What one edit run is to be recorded as — decoupled from `ModifyCodeReport` so the writer and the
/// reader share one description and a test does not have to build a live report to exercise it.
pub struct EditFacts<'a> {
    /// The instruction, verbatim — the whole point of the record.
    pub instruction: &'a str,
    /// The component, as the tool was asked for it (`rolling_average`).
    pub component: &'a str,
    /// The component library's root, as the tool was asked for it.
    pub root: &'a str,
    /// Did the change survive its gate?
    pub success: bool,
    /// How far verification reached, already worded (`"HostOnly"`, `"WithTarget"`).
    pub verified: &'a str,
    /// The files already said what was asked: nothing moved, but the request is in effect.
    pub up_to_date: bool,
    pub files_changed: &'a [String],
    pub files_reverted: &'a [String],
    pub files_skipped: &'a [String],
    /// What the optional second gate did, in the caller's terms.
    pub chip_build: &'a str,
    /// What could not be checked, in the user's words.
    pub caveats: &'a [String],
}

/// The library a component belongs to, as it is named in the graph.
///
/// The scoped name follows `spec_persist`'s `{project}::{logical}` convention: the component's own
/// name is not unique across libraries, and two `sps30`s that are different code must not share one
/// history. The full root is still recorded on the node, so nothing is lost to the short name.
pub fn library_name(root: &str) -> String {
    std::path::Path::new(root)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.to_string())
}

/// `{library}::{component}` — the graph name shared by a component's module node.
fn scoped_name(library: &str, component: &str) -> String {
    format!("{library}::{component}")
}

/// Cap a stray prompt so one edit cannot fill the store.
fn cap(text: &str) -> String {
    if text.chars().count() <= MAX_INSTRUCTION_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_INSTRUCTION_CHARS).collect();
    out.push_str("…[truncated]");
    out
}

/// The first non-empty line of the instruction, capped — what a list of records shows.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= MAX_DESCRIPTION_CHARS {
        return line.to_string();
    }
    let mut out: String = line.chars().take(MAX_DESCRIPTION_CHARS).collect();
    out.push('…');
    out
}

// ============================================================================
// Transport — one round-trip per graph operation. Failures come back as strings
// so a caller can log them and carry on rather than fail the edit.
// ============================================================================

async fn store_node(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    node: AttrNode,
) -> Result<AttrNode, String> {
    let (reply, rx) = oneshot::channel();
    graph
        .send(MemoryGraphMessage::StoreAttrNode {
            node,
            reply_to: reply,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    rx.await
        .map_err(|e| format!("store reply lost: {e}"))?
        .map_err(|e| format!("store failed: {e}"))
}

async fn merge_node(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    node: AttrNode,
) -> Result<AttrNode, String> {
    let (reply, rx) = oneshot::channel();
    graph
        .send(MemoryGraphMessage::MergeAttrNode {
            node,
            reply_to: reply,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    rx.await
        .map_err(|e| format!("merge reply lost: {e}"))?
        .map_err(|e| format!("merge failed: {e}"))
}

async fn create_rel(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    edge_type: RelationshipType,
    from_id: &str,
    to_id: &str,
) -> Result<(), String> {
    let (reply, rx) = oneshot::channel();
    graph
        .send(MemoryGraphMessage::CreateRelationship {
            rel: RelationshipInput {
                edge_type,
                from_id: from_id.to_string(),
                to_id: to_id.to_string(),
                properties: None,
                weight: None,
            },
            reply_to: reply,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    rx.await
        .map_err(|e| format!("relationship reply lost: {e}"))?
        .map_err(|e| format!("relationship failed: {e}"))?;
    Ok(())
}

async fn query_by_name(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    node_type: &str,
    subtype: &str,
    name: &str,
) -> Result<Vec<AttrNode>, String> {
    let (reply, rx) = oneshot::channel();
    graph
        .send(MemoryGraphMessage::QueryAttrNodes {
            node_type: Some(node_type.to_string()),
            subtype: Some(subtype.to_string()),
            name: Some(name.to_string()),
            limit: Some(1),
            reply_to: reply,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    rx.await
        .map_err(|e| format!("query reply lost: {e}"))?
        .map_err(|e| format!("query failed: {e}"))
}

async fn query_by_props(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    subtype: &str,
    props: &[(&str, &str)],
    limit: u32,
) -> Result<Vec<AttrNode>, String> {
    let (reply, rx) = oneshot::channel();
    graph
        .send(MemoryGraphMessage::QueryAttrNodesWhere {
            subtype: Some(subtype.to_string()),
            props: props
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            limit: Some(limit),
            reply_to: reply,
        })
        .await
        .map_err(|e| format!("memory graph channel closed: {e}"))?;
    rx.await
        .map_err(|e| format!("query reply lost: {e}"))?
        .map_err(|e| format!("query failed: {e}"))
}

// ============================================================================
// Public API
// ============================================================================

/// The component's module node — read back when it is there, created once when it is not.
///
/// **Read first, on purpose.** `MergeAttrNode` upserts by `(node_type, subtype, name)`, but it does
/// it with a `DETACH DELETE` and a re-insert, which would take the module's whole `has_edit` history
/// with it. So an existing spine is looked up and reused; the merge path is only for the first edit
/// of a component — and it is a *merge* rather than a store so that two edits racing on that first
/// write settle on one node instead of forking the history.
pub async fn module_node(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    library: &str,
    component: &str,
) -> Result<AttrNode, String> {
    let name = scoped_name(library, component);
    if let Some(existing) = query_by_name(graph, MODULE_TYPE, MODULE_SUBTYPE, &name)
        .await?
        .into_iter()
        .next()
    {
        return Ok(existing);
    }

    let now = chrono::Utc::now();
    merge_node(
        graph,
        AttrNode {
            id: uuid::Uuid::new_v4().to_string(),
            node_type: MODULE_TYPE.to_string(),
            subtype: Some(MODULE_SUBTYPE.to_string()),
            name,
            description: Some(format!(
                "IDF component '{component}' of the '{library}' component library"
            )),
            properties: HashMap::from([
                ("component".to_string(), serde_json::json!(component)),
                ("library".to_string(), serde_json::json!(library)),
            ]),
            embedding_id: None,
            created_at: now,
            updated_at: now,
            version: 1,
        },
    )
    .await
}

/// Write one edit record and link it to the component's module node, returning
/// `(module_id, record_id)`.
///
/// The record is named `{library}::{component}::{uuid}` — unique per run, so a re-run is a new
/// record rather than an overwrite. History is appended, never rewritten: "we asked for this twice
/// and twice nothing moved" is exactly the fact a single overwritten note would destroy.
pub async fn record_component_edit(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    facts: &EditFacts<'_>,
) -> Result<(String, String), String> {
    let library = library_name(facts.root);
    let module = module_node(graph, &library, facts.component).await?;

    let instruction = cap(facts.instruction);
    let now = chrono::Utc::now();
    let mut properties: HashMap<String, serde_json::Value> = HashMap::new();
    properties.insert("component".to_string(), serde_json::json!(facts.component));
    properties.insert("library".to_string(), serde_json::json!(library));
    properties.insert("root".to_string(), serde_json::json!(facts.root));
    properties.insert("instruction".to_string(), serde_json::json!(instruction));
    properties.insert("success".to_string(), serde_json::json!(facts.success));
    properties.insert("verified".to_string(), serde_json::json!(facts.verified));
    properties.insert(
        "up_to_date".to_string(),
        serde_json::json!(facts.up_to_date),
    );
    properties.insert(
        "chip_build".to_string(),
        serde_json::json!(facts.chip_build),
    );
    // Lists go in as lists. The AttrNode store writes a complex property as a JSON literal and
    // resolves it back to an array, so a reader gets a list to iterate rather than a string to take
    // apart. Only the scalars are what a `QueryAttrNodesWhere` can filter on, and `component` is one.
    properties.insert(
        "files_changed".to_string(),
        serde_json::json!(facts.files_changed),
    );
    properties.insert(
        "files_reverted".to_string(),
        serde_json::json!(facts.files_reverted),
    );
    properties.insert(
        "files_skipped".to_string(),
        serde_json::json!(facts.files_skipped),
    );
    properties.insert("caveats".to_string(), serde_json::json!(facts.caveats));

    let id = uuid::Uuid::new_v4().to_string();
    store_node(
        graph,
        AttrNode {
            id: id.clone(),
            node_type: EDIT_RECORD_TYPE.to_string(),
            subtype: Some(EDIT_SUBTYPE.to_string()),
            name: format!("{}::{id}", scoped_name(&library, facts.component)),
            description: Some(first_line(&instruction)),
            properties,
            embedding_id: None,
            created_at: now,
            updated_at: now,
            version: 1,
        },
    )
    .await?;

    create_rel(graph, RelationshipType::HasEdit, &module.id, &id).await?;
    Ok((module.id, id))
}

/// How many rows to pull before choosing the newest: the graph-side `LIMIT` caps the read but the
/// store's row order is not a promise, so the ceiling is generous and the ordering happens here.
const HISTORY_READ_CEILING: u32 = 200;

/// The most recent edits of one component, newest first, ready for a tool response.
///
/// Read-only and best-effort: a caller that cannot reach the graph gets an empty list and a log
/// line rather than an error — the response it is decorating has already happened, and a missing
/// history must not look like a failed edit.
pub async fn component_edit_history(
    graph: &mpsc::Sender<MemoryGraphMessage>,
    root: &str,
    component: &str,
    limit: u32,
) -> Vec<serde_json::Value> {
    let library = library_name(root);
    let mut records = match query_by_props(
        graph,
        EDIT_SUBTYPE,
        &[("component", component)],
        HISTORY_READ_CEILING,
    )
    .await
    {
        Ok(nodes) => nodes,
        Err(e) => {
            warn!("[EditHistory] history unreadable for {library}::{component}: {e}");
            return Vec::new();
        }
    };

    records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
    records.truncate(limit as usize);
    records.iter().map(record_to_json).collect()
}

/// One record as the caller sees it: what was asked, how far it was checked, what moved.
fn record_to_json(node: &AttrNode) -> serde_json::Value {
    let prop = |key: &str| {
        node.properties
            .get(key)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    serde_json::json!({
        "instruction": prop("instruction"),
        "success": prop("success"),
        "verified": prop("verified"),
        "up_to_date": prop("up_to_date"),
        "files_changed": prop("files_changed"),
        "at": node.created_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spire_core::actors::Actor;
    use spire_core::models::memory_graph::GraphEdge;
    use spire_core::subsystems::graph::memory_graph::MemoryGraphActor;

    /// A real `MemoryGraphActor` over a fresh in-memory store — the actor the app runs, so the
    /// round trip under test is the real one (`StoreAttrNode` / `QueryAttrNodesWhere` /
    /// `CreateRelationship`), not a stand-in.
    async fn fresh_graph(dir: &std::path::Path) -> mpsc::Sender<MemoryGraphMessage> {
        let (tx, rx) = mpsc::channel(64);
        let _join = MemoryGraphActor::new().spawn(rx);
        let (reply, ready) = oneshot::channel();
        tx.send(MemoryGraphMessage::InitializeInMemory {
            data_dir: dir.to_path_buf(),
            reply_to: reply,
        })
        .await
        .expect("send InitializeInMemory");
        ready.await.expect("init reply").expect("init");
        tx
    }

    async fn relationships(
        graph: &mpsc::Sender<MemoryGraphMessage>,
        node_id: &str,
    ) -> Vec<GraphEdge> {
        let (reply, rx) = oneshot::channel();
        graph
            .send(MemoryGraphMessage::GetRelationships {
                node_id: node_id.to_string(),
                reply_to: reply,
            })
            .await
            .expect("send GetRelationships");
        rx.await.expect("reply").expect("get relationships")
    }

    fn facts<'a>(
        instruction: &'a str,
        component: &'a str,
        root: &'a str,
        up_to_date: bool,
        changed: &'a [String],
    ) -> EditFacts<'a> {
        EditFacts {
            instruction,
            component,
            root,
            success: true,
            verified: "HostOnly",
            up_to_date,
            files_changed: changed,
            files_reverted: &[],
            files_skipped: &[],
            chip_build: "not run: no chip named",
            caveats: &[],
        }
    }

    /// A component's history is a spine plus one record per run — appended, never overwritten, and
    /// scoped to the component so one module's edits cannot appear in another's.
    #[tokio::test]
    async fn edits_are_recorded_per_component_and_read_back_newest_first() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let graph = fresh_graph(tmp.path()).await;
        let root = "/home/someone/spire-idf";
        let changed = vec!["src/rolling_average.cpp".to_string()];

        let (module, _) = record_component_edit(
            &graph,
            &facts(
                "Move reset() out of the header.",
                "rolling_average",
                root,
                false,
                &changed,
            ),
        )
        .await
        .expect("record the first edit");

        // Distinct timestamps, so "newest first" is a property under test rather than a tie.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        let (again, _) = record_component_edit(
            &graph,
            &facts(
                "Move reset() out of the header.",
                "rolling_average",
                root,
                true,
                &[],
            ),
        )
        .await
        .expect("record the second edit");

        // One spine per component, reused: a second run must not fork the module node, or the
        // history would split in two the first time a component is edited twice.
        assert_eq!(
            module, again,
            "the second edit forked the module spine instead of hanging off it"
        );

        let history = component_edit_history(&graph, root, "rolling_average", 5).await;
        assert_eq!(history.len(), 2, "both runs are history: {history:#?}");
        assert_eq!(
            history[0]["instruction"], "Move reset() out of the header.",
            "the instruction is the record: {history:#?}"
        );
        assert_eq!(
            history[0]["up_to_date"],
            serde_json::json!(true),
            "the newest run comes first: {history:#?}"
        );
        assert_eq!(history[1]["up_to_date"], serde_json::json!(false));
        assert_eq!(
            history[0]["files_changed"],
            serde_json::json!([]),
            "a no-op moved nothing, and the record says so: {history:#?}"
        );
        assert_eq!(
            history[1]["files_changed"],
            serde_json::json!(["src/rolling_average.cpp"]),
            "the files a run changed have to survive the round trip as a list, not as a blob of \
             text: {history:#?}"
        );

        // The edge is what makes per-module history a traversal: the module points at every run.
        let mut edges = relationships(&graph, &module).await;
        edges.retain(|e| e.edge_type == RelationshipType::HasEdit);
        assert_eq!(edges.len(), 2, "one has_edit edge per run: {edges:#?}");
    }

    /// Two components of the same library are two histories — the point of scoping a record by
    /// component rather than by library.
    #[tokio::test]
    async fn one_components_history_is_not_anothers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let graph = fresh_graph(tmp.path()).await;

        let (rolling, _) = record_component_edit(
            &graph,
            &facts(
                "Move reset() out of the header.",
                "rolling_average",
                "/x/spire-idf",
                false,
                &[],
            ),
        )
        .await
        .expect("record rolling_average");
        let (sps30, _) = record_component_edit(
            &graph,
            &facts(
                "Read PM2.5 from the SPS30.",
                "sps30",
                "/x/spire-idf",
                false,
                &[],
            ),
        )
        .await
        .expect("record sps30");

        assert_ne!(rolling, sps30, "two components, two module nodes");

        let history = component_edit_history(&graph, "/x/spire-idf", "sps30", 5).await;
        assert_eq!(history.len(), 1, "{history:#?}");
        assert_eq!(history[0]["instruction"], "Read PM2.5 from the SPS30.");
    }

    /// The short name is what scopes the graph node, so it has to be the library's own name.
    #[test]
    fn a_library_is_named_by_its_last_path_segment() {
        assert_eq!(library_name("/home/someone/spire-idf"), "spire-idf");
        assert_eq!(library_name("sensors"), "sensors");
        assert_eq!(scoped_name("spire-idf", "sps30"), "spire-idf::sps30");
    }

    /// A pasted excerpt must not become an unbounded node, and the cut has to be visible.
    #[test]
    fn a_runaway_instruction_is_capped_and_marked() {
        let long = "x".repeat(MAX_INSTRUCTION_CHARS + 50);
        let capped = cap(&long);
        assert!(capped.ends_with("…[truncated]"), "{capped}");
        assert_eq!(
            capped.chars().count(),
            MAX_INSTRUCTION_CHARS + "…[truncated]".chars().count()
        );
        assert_eq!(cap("short"), "short");
    }

    /// The node description is a label: the first thing asked, not the whole instruction.
    #[test]
    fn a_description_is_the_first_real_line() {
        assert_eq!(
            first_line("\n\n  Read the SHT20.\nMore text."),
            "Read the SHT20."
        );
        assert_eq!(first_line("   "), "");
        let long = "y".repeat(MAX_DESCRIPTION_CHARS + 10);
        assert!(first_line(&long).ends_with('…'));
    }
}
