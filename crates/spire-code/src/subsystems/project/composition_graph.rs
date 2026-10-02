// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Graph-native composition** — the IDF [`ApplicationSpec`] as a decomposed property graph.
//!
//! The graph is Spire's single source of truth, so a project's **composition** lives there as typed
//! nodes and edges (one node per entity) rather than as a JSON/file blob: the anchor (framework and
//! justification), the **board**, its **board facts**, the **facets** (power/storage/network/security),
//! every **unit** (a component, an actor or a ramen stage), and every **message** type. Files
//! (`composition.spire`, `SPIRE.application.json`) are rendered copies of this graph, never its source.
//!
//! This module is pure: [`decompose`] turns an [`ApplicationSpec`] into a [`CompositionGraph`] of
//! node/edge *specs* (no actor I/O), and [`reconstruct`] rebuilds the [`ApplicationSpec`] from those
//! specs. The memory-graph actor layer ([`super::composition_persist`]) maps the specs to
//! `AttrNode`/`CreateRelationship` messages. Round-tripping is a hard property:
//! `reconstruct(decompose(spec)) == spec`.
//!
//! # The taxonomy
//!
//! * **nodes** — [`node::ANCHOR`] (framework + justification), [`node::BOARD`] (chip/bsp/hal),
//!   [`node::BOARD_FACT`] (a bus/device/address triple), [`node::FACET`] (a power/storage/network/
//!   security lens), [`node::COMPONENT`]/[`node::ACTOR`]/[`node::STAGE`] (a unit, discriminated by its
//!   kind), [`node::MESSAGE`] (a message type, deduplicated by name), and [`node::DESIGN_RECORD`] (the
//!   decision that produced the composition);
//! * **edges** — containment ([`edge::HAS_BOARD`], [`edge::HAS_BOARD_FACT`], [`edge::HAS_FACET`],
//!   [`edge::HAS_UNIT`], [`edge::HAS_MESSAGE`], [`edge::HAS_DESIGN_RECORD`]) and the composition's own
//!   wiring ([`edge::USES`], [`edge::SENDS_TO`], [`edge::WIRED_TO`]). The board's facts hang off the
//!   **board**, not the anchor, because a fact is about a board.
//!
//! # The design record, and staleness
//!
//! A composition can be **stored** without having been **decided**: a person edits `composition.spire`
//! and the graph is re-written from it, which is the supported way to change a design. So the decision
//! is a node of its own rather than a flag on the anchor — [`DesignRecord`], carrying the door the
//! design came through ([`DesignSource`]), when it was decided, its version, and the [`fingerprint`] of
//! the composition it ratifies. [`edge::HAS_DESIGN_RECORD`] hangs it off the anchor.
//!
//! [`freshness`] then answers the one question a reader of the graph has: **does the record still
//! ratify the composition in front of it?** Equal fingerprints are [`DesignFreshness::Current`]; a
//! composition edited since the decision is [`DesignFreshness::Stale`] — the graph's form of the
//! file-side rule that a record must not go on describing a composition a person has since changed
//! (`idf_projects::read_application_and_sync_record`). No record at all is [`DesignFreshness::Absent`],
//! which is honest rather than an error: it is the ordinary state of a project before the design
//! phase, and it is not the same answer as "the design you decided no longer applies".
//!
//! # A note on order and on reference lists
//!
//! The memory store's edges are unordered, so every collection whose *order* is part of the spec
//! (`uses`, `sends_to`, the `wiring` list) is kept as a property on its owner — exactly the way
//! `spec_graph` keeps an actor's `uses` — and the store round-trips it verbatim. The `USES`/`SENDS_TO`/
//! `WIRED_TO` edges are a **derived, traversable projection** of those properties (so "what does
//! `air_quality` use?" is one hop), rebuilt on every store and never read back: the property is
//! authoritative. Port names (`pulls`/`pushes`) are labels, not entities, so they too are scalars.

use serde_json::{json, Value};

use crate::build::application_spec::{
    framework_from_name, ApplicationFramework, ApplicationSpec, BoardChoice, BoardFact,
    ComponentRole, NetworkFacet, PowerFacet, SecurityFacet, StorageFacet, Unit, UnitKind,
    UnitSource,
};

/// Node-type discriminators for the decomposed composition graph (stored as the `subtype` of an
/// `idf_composition` AttrNode).
pub mod node {
    pub const ANCHOR: &str = "anchor";
    pub const BOARD: &str = "board";
    pub const BOARD_FACT: &str = "board_fact";
    pub const FACET: &str = "facet";
    pub const COMPONENT: &str = "component";
    pub const ACTOR: &str = "actor";
    pub const STAGE: &str = "stage";
    pub const MESSAGE: &str = "message";
    /// The decision that produced the composition (one per project; see [`super::DesignRecord`]).
    pub const DESIGN_RECORD: &str = "design_record";
}

/// Edge predicates of the decomposed composition graph.
pub mod edge {
    pub const HAS_BOARD: &str = "HAS_BOARD";
    pub const HAS_BOARD_FACT: &str = "HAS_BOARD_FACT";
    pub const HAS_FACET: &str = "HAS_FACET";
    pub const HAS_UNIT: &str = "HAS_UNIT";
    pub const HAS_MESSAGE: &str = "HAS_MESSAGE";
    /// Anchor → the [`node::DESIGN_RECORD`] that ratifies the composition.
    pub const HAS_DESIGN_RECORD: &str = "HAS_DESIGN_RECORD";
    pub const USES: &str = "USES";
    pub const SENDS_TO: &str = "SENDS_TO";
    pub const WIRED_TO: &str = "WIRED_TO";
}

/// Logical name of the anchor node. The store renames it to the project name (see
/// [`super::composition_persist`]); `reconstruct` finds the anchor by its type, so the node's name is
/// only a label here.
pub const ROOT: &str = "composition";

/// Logical name of the single board node.
const BOARD_NAME: &str = "board";

/// Logical name of the single design-record node. The store scopes it `{project}::{this}`, so a
/// project's record is one query away and never collides with another project's.
pub const DESIGN_RECORD_NAME: &str = "design_record";

/// Ordering property carried by ordered collections (`board_facts`, `units`).
pub const PROP_ORDER: &str = "order";

const P_FRAMEWORK: &str = "framework";
const P_JUSTIFICATION: &str = "justification";
const P_WIRING: &str = "wiring";
const P_CHIP: &str = "chip";
const P_BSP: &str = "bsp";
const P_HAL: &str = "hal";
const P_BUS: &str = "bus";
const P_DEVICE: &str = "device";
const P_ADDRESS: &str = "address";
const P_FACET: &str = "facet";
const P_ROLE: &str = "role";
const P_SOURCE: &str = "source";
const P_REGISTRY: &str = "registry";
const P_PROVIDES: &str = "provides";
const P_USES: &str = "uses";
const P_STATE: &str = "state";
const P_SENDS_TO: &str = "sends_to";
const P_PULLS: &str = "pulls";
const P_PUSHES: &str = "pushes";
const P_SLEEPS: &str = "sleeps";
const P_WAKE: &str = "wake";
const P_STORAGE_ROLE: &str = "storage_role";
const P_NETWORK_ROLE: &str = "network_role";
const P_REQUIRES: &str = "requires";
// Design-record properties (`node::DESIGN_RECORD`): the provenance of the composition, kept on the
// record itself so it reads as one self-contained decision rather than needing the anchor joined in.
const P_SOURCE_KIND: &str = "source_kind";
const P_DECIDED_AT: &str = "decided_at";
const P_RECORD_VERSION: &str = "design_version";
const P_FINGERPRINT: &str = "fingerprint";
const P_BUDGET: &str = "budget_mah";
const P_TARGET_DAYS: &str = "target_days";
const P_MEDIUM: &str = "medium";
const P_RETENTION: &str = "retention";
const P_TRANSPORT: &str = "transport";
const P_PROVISION: &str = "provision";

/// A node specification in the decomposed composition graph (logical, pre-actor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionNode {
    /// Discriminator (one of the `node::` constants).
    pub node_type: String,
    /// Logical, graph-unique identity (a unit id, a message name, `board`, `composition`…).
    pub name: String,
    pub description: Option<String>,
    pub properties: Vec<(String, Value)>,
}

impl CompositionNode {
    fn new(node_type: &str, name: &str) -> Self {
        Self {
            node_type: node_type.to_string(),
            name: name.to_string(),
            description: None,
            properties: Vec::new(),
        }
    }

    /// Attach the node's human-readable description (a no-op on an empty string).
    fn described(mut self, description: &str) -> Self {
        if !description.is_empty() {
            self.description = Some(description.to_string());
        }
        self
    }

    fn with(mut self, key: &str, value: Value) -> Self {
        self.properties.push((key.to_string(), value));
        self
    }
}

/// An edge specification (endpoints by logical node `name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionEdge {
    pub predicate: String,
    pub from_name: String,
    pub to_name: String,
}

/// The decomposed composition as pure data (no actor I/O).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompositionGraph {
    pub nodes: Vec<CompositionNode>,
    pub edges: Vec<CompositionEdge>,
}

pub(crate) fn prop<'a>(n: &'a CompositionNode, key: &str) -> Option<&'a Value> {
    n.properties.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

pub(crate) fn prop_str(n: &CompositionNode, key: &str) -> Option<String> {
    prop(n, key).and_then(|v| v.as_str().map(str::to_string))
}

fn prop_u32(n: &CompositionNode, key: &str) -> Option<u32> {
    prop(n, key).and_then(Value::as_u64).map(|v| v as u32)
}

fn prop_bool(n: &CompositionNode, key: &str) -> bool {
    prop(n, key).and_then(Value::as_bool).unwrap_or(false)
}

/// An ordered list property (a JSON array of strings).
fn prop_list(n: &CompositionNode, key: &str) -> Vec<String> {
    prop(n, key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn order_of(n: &CompositionNode) -> usize {
    prop(n, PROP_ORDER).and_then(Value::as_u64).unwrap_or(0) as usize
}

/// Child nodes of `parent` via `predicate`, ordered by their `order` property.
fn children<'a>(
    g: &'a CompositionGraph,
    parent: &str,
    predicate: &str,
) -> Vec<&'a CompositionNode> {
    let mut out: Vec<&CompositionNode> = g
        .edges
        .iter()
        .filter(|e| e.predicate == predicate && e.from_name == parent)
        .filter_map(|e| g.nodes.iter().find(|n| n.name == e.to_name))
        .collect();
    out.sort_by_key(|n| order_of(n));
    out
}

fn edge(predicate: &str, from: &str, to: &str) -> CompositionEdge {
    CompositionEdge {
        predicate: predicate.to_string(),
        from_name: from.to_string(),
        to_name: to.to_string(),
    }
}

/// The `node::` subtype a [`UnitKind`] decomposes to.
fn kind_str(kind: UnitKind) -> &'static str {
    match kind {
        UnitKind::Component => node::COMPONENT,
        UnitKind::Actor => node::ACTOR,
        UnitKind::Stage => node::STAGE,
    }
}

fn unit_role(s: &str) -> Option<ComponentRole> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

fn unit_source(s: &str) -> Option<UnitSource> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

/// `from -> to`, as `wiring` and `validate` write it.
fn parse_wiring(entry: &str) -> Option<(&str, &str)> {
    let (from, to) = entry.split_once("->")?;
    let (from, to) = (from.trim(), to.trim());
    (!from.is_empty() && !to.is_empty()).then_some((from, to))
}

/// Decompose an [`ApplicationSpec`] into its graph-native representation.
/// Deterministic: same spec in → same node/edge set out, in the same order.
pub fn decompose(spec: &ApplicationSpec) -> CompositionGraph {
    let mut g = CompositionGraph::default();

    // Anchor: the framework, its justification and the wiring — the product-wide facts.
    g.nodes.push(
        CompositionNode::new(node::ANCHOR, ROOT)
            .described(&spec.justification)
            .with(P_FRAMEWORK, json!(spec.framework.as_str()))
            .with(P_JUSTIFICATION, json!(spec.justification))
            .with(P_WIRING, json!(spec.wiring)),
    );

    // Board: chip/bsp/hal — the silicon and the support package the application is written against.
    g.nodes.push(
        CompositionNode::new(node::BOARD, BOARD_NAME)
            .with(P_CHIP, json!(spec.board.chip))
            .with(P_BSP, json!(spec.board.bsp))
            .with(P_HAL, json!(spec.board.hal)),
    );
    g.edges.push(edge(edge::HAS_BOARD, ROOT, BOARD_NAME));

    for (i, f) in spec.board_facts.iter().enumerate() {
        let name = format!("board_fact.{i}");
        g.nodes.push(
            CompositionNode::new(node::BOARD_FACT, &name)
                .with(P_BUS, json!(f.bus))
                .with(P_DEVICE, json!(f.device))
                .with(P_ADDRESS, json!(f.address))
                .with(PROP_ORDER, json!(i)),
        );
        g.edges.push(edge(edge::HAS_BOARD_FACT, BOARD_NAME, &name));
    }

    push_facets(&mut g, spec);

    // Messages first (deduped, first-seen order) so units can point at them.
    let mut messages: Vec<&str> = Vec::new();
    for u in &spec.units {
        if let Some(m) = &u.message {
            if !messages.contains(&m.as_str()) {
                messages.push(m.as_str());
            }
        }
    }
    for m in &messages {
        g.nodes.push(CompositionNode::new(node::MESSAGE, m));
    }

    for (i, u) in spec.units.iter().enumerate() {
        let mut n = CompositionNode::new(kind_str(u.kind), &u.id)
            .with(PROP_ORDER, json!(i))
            .with(P_USES, json!(u.uses))
            .with(P_SENDS_TO, json!(u.sends_to))
            .with(P_SLEEPS, json!(u.sleeps));
        if let Some(v) = &u.role {
            n = n.with(P_ROLE, json!(v));
        }
        if let Some(v) = &u.source {
            n = n.with(P_SOURCE, json!(v));
        }
        for (key, value) in [
            (P_REGISTRY, &u.registry),
            (P_PROVIDES, &u.provides),
            (P_BUS, &u.bus),
            (P_STATE, &u.state),
            (P_PULLS, &u.pulls),
            (P_PUSHES, &u.pushes),
            (P_WAKE, &u.wake),
            (P_STORAGE_ROLE, &u.storage_role),
            (P_NETWORK_ROLE, &u.network_role),
        ] {
            if let Some(v) = value {
                n = n.with(key, json!(v));
            }
        }
        g.nodes.push(n);

        g.edges.push(edge(edge::HAS_UNIT, ROOT, &u.id));
        for target in &u.uses {
            g.edges.push(edge(edge::USES, &u.id, target));
        }
        for target in &u.sends_to {
            g.edges.push(edge(edge::SENDS_TO, &u.id, target));
        }
        if let Some(m) = &u.message {
            g.edges.push(edge(edge::HAS_MESSAGE, &u.id, m));
        }
    }

    // Derived wiring edges: the traversable form of the `wiring` property above.
    for w in &spec.wiring {
        if let Some((from, to)) = parse_wiring(w) {
            g.edges.push(edge(edge::WIRED_TO, from, to));
        }
    }

    g
}

/// One `facet` node per facet the composition states (a facet nobody states is absent, not empty).
fn push_facets(g: &mut CompositionGraph, spec: &ApplicationSpec) {
    if let Some(p) = &spec.power {
        let mut n = CompositionNode::new(node::FACET, "power").with(P_FACET, json!("power"));
        if let Some(v) = p.budget_mah {
            n = n.with(P_BUDGET, json!(v));
        }
        if let Some(v) = p.target_days {
            n = n.with(P_TARGET_DAYS, json!(v));
        }
        g.nodes.push(n);
        g.edges.push(edge(edge::HAS_FACET, ROOT, "power"));
    }
    if let Some(s) = &spec.storage {
        let mut n = CompositionNode::new(node::FACET, "storage").with(P_FACET, json!("storage"));
        if let Some(v) = &s.medium {
            n = n.with(P_MEDIUM, json!(v));
        }
        if let Some(v) = &s.retention {
            n = n.with(P_RETENTION, json!(v));
        }
        g.nodes.push(n);
        g.edges.push(edge(edge::HAS_FACET, ROOT, "storage"));
    }
    if let Some(net) = &spec.network {
        let mut n = CompositionNode::new(node::FACET, "network").with(P_FACET, json!("network"));
        if let Some(v) = &net.transport {
            n = n.with(P_TRANSPORT, json!(v));
        }
        if let Some(v) = &net.provision {
            n = n.with(P_PROVISION, json!(v));
        }
        g.nodes.push(n);
        g.edges.push(edge(edge::HAS_FACET, ROOT, "network"));
    }
    if let Some(sec) = &spec.security {
        g.nodes.push(
            CompositionNode::new(node::FACET, "security")
                .with(P_FACET, json!("security"))
                .with(P_REQUIRES, json!(sec.requires)),
        );
        g.edges.push(edge(edge::HAS_FACET, ROOT, "security"));
    }
}

/// Rebuild an [`ApplicationSpec`] from a decomposed [`CompositionGraph`].
pub fn reconstruct(g: &CompositionGraph) -> Result<ApplicationSpec, String> {
    let anchor = g
        .nodes
        .iter()
        .find(|n| n.node_type == node::ANCHOR)
        .ok_or("no composition anchor node")?;

    let framework = prop_str(anchor, P_FRAMEWORK)
        .ok_or("the composition anchor is missing its 'framework' property")?;
    let framework = framework_from_name(&framework)?;
    let justification = prop_str(anchor, P_JUSTIFICATION).unwrap_or_default();
    let wiring = prop_list(anchor, P_WIRING);

    let board_node = g.nodes.iter().find(|n| n.node_type == node::BOARD);
    let board = match board_node {
        Some(b) => BoardChoice {
            chip: prop_str(b, P_CHIP).unwrap_or_default(),
            bsp: prop_str(b, P_BSP).unwrap_or_default(),
            hal: prop_str(b, P_HAL).unwrap_or_default(),
        },
        None => BoardChoice::default(),
    };
    let board_facts = match board_node {
        Some(b) => children(g, &b.name, edge::HAS_BOARD_FACT)
            .into_iter()
            .map(|n| BoardFact {
                bus: prop_str(n, P_BUS).unwrap_or_default(),
                device: prop_str(n, P_DEVICE).unwrap_or_default(),
                address: prop_str(n, P_ADDRESS).unwrap_or_default(),
            })
            .collect(),
        None => Vec::new(),
    };

    let (mut power, mut storage, mut network, mut security) = (None, None, None, None);
    for f in children(g, &anchor.name, edge::HAS_FACET) {
        match prop_str(f, P_FACET).as_deref() {
            Some("power") => {
                power = Some(PowerFacet {
                    budget_mah: prop_u32(f, P_BUDGET),
                    target_days: prop_u32(f, P_TARGET_DAYS),
                })
            }
            Some("storage") => {
                storage = Some(StorageFacet {
                    medium: prop_str(f, P_MEDIUM),
                    retention: prop_str(f, P_RETENTION),
                })
            }
            Some("network") => {
                network = Some(NetworkFacet {
                    transport: prop_str(f, P_TRANSPORT),
                    provision: prop_str(f, P_PROVISION),
                })
            }
            Some("security") => {
                security = Some(SecurityFacet {
                    requires: prop_list(f, P_REQUIRES),
                })
            }
            _ => {}
        }
    }

    let mut units = Vec::new();
    for n in children(g, &anchor.name, edge::HAS_UNIT) {
        let kind = match n.node_type.as_str() {
            node::COMPONENT => UnitKind::Component,
            node::ACTOR => UnitKind::Actor,
            node::STAGE => UnitKind::Stage,
            other => {
                return Err(format!(
                    "'{other}' is not a unit kind in a composition graph"
                ))
            }
        };
        let message = g
            .edges
            .iter()
            .find(|e| e.predicate == edge::HAS_MESSAGE && e.from_name == n.name)
            .map(|e| e.to_name.clone());
        units.push(Unit {
            id: n.name.clone(),
            kind,
            role: prop_str(n, P_ROLE).as_deref().and_then(unit_role),
            source: prop_str(n, P_SOURCE).as_deref().and_then(unit_source),
            registry: prop_str(n, P_REGISTRY),
            provides: prop_str(n, P_PROVIDES),
            bus: prop_str(n, P_BUS),
            uses: prop_list(n, P_USES),
            message,
            state: prop_str(n, P_STATE),
            sends_to: prop_list(n, P_SENDS_TO),
            pulls: prop_str(n, P_PULLS),
            pushes: prop_str(n, P_PUSHES),
            sleeps: prop_bool(n, P_SLEEPS),
            wake: prop_str(n, P_WAKE),
            storage_role: prop_str(n, P_STORAGE_ROLE),
            network_role: prop_str(n, P_NETWORK_ROLE),
        });
    }

    Ok(ApplicationSpec {
        framework,
        justification,
        board,
        board_facts,
        units,
        wiring,
        power,
        storage,
        network,
        security,
    })
}

/// **How a design came to be** — the door the composition arrived through.
///
/// Recorded rather than inferred, because the three doors are not interchangeable to a reader: a
/// decomposition a person *wrote* in `composition.spire` and one a *model* proposed from six answers
/// carry different confidence, and a record that did not say which is a record that cannot say "a
/// person reviewed this" and mean anything by it.
///
/// The axis is **who decided** — the only division the doors actually make. The design form
/// (`createProject/DesignApplication`) is one door with two readings: a caller that pins the framework
/// has decided, and the model fills the decomposition in around that choice; a caller that pins
/// nothing is asking the model to *choose and justify* one. Both are one model round trip, and they
/// are not the same fact about the design — which is what [`Self::Answers`] and [`Self::Model`] say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesignSource {
    /// The **caller's answers** decided: `createProject/DesignApplication` with the framework pinned,
    /// so the decomposition was filled in around a choice that was already made.
    Answers,
    /// A `composition.spire` a person handed in (`createProject/ParseComposition`) — or the one a tree
    /// already carried, which is the same file by a different road.
    CompositionFile,
    /// The **model** decided: `createProject/DesignApplication` with no framework pinned, where the
    /// choice is the model's to make and justify and a person accepts it at review.
    Model,
}

impl DesignSource {
    pub fn as_str(self) -> &'static str {
        match self {
            DesignSource::Answers => "answers",
            DesignSource::CompositionFile => "composition_file",
            DesignSource::Model => "model",
        }
    }

    pub fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "answers" => Ok(DesignSource::Answers),
            "composition_file" => Ok(DesignSource::CompositionFile),
            "model" => Ok(DesignSource::Model),
            other => Err(format!(
                "'{other}' is not a design source (expected `answers`, `composition_file` or `model`)"
            )),
        }
    }

    /// **The door an accepted design came through**, from what the caller said and what the tree did —
    /// the rule the leg that writes a tree records a decision by.
    ///
    /// The two inputs are the only things a scaffold knows, and they are not the same kind of thing. A
    /// caller that **states** a door is believed: the door is not recoverable from the spec (nothing in
    /// a composition says who chose it), so what the caller says is the only record of it — and a caller
    /// reading *this* project's `composition.spire` is exactly the `CompositionFile` case, which is why
    /// a stated door wins even over a tree that decided.
    ///
    /// A caller that says nothing leaves one fact behind: a tree that already carried a design **was**
    /// the design — the leg resolved the two and dropped the caller's copy, so the composition written
    /// is the file's. That is read off the tree rather than guessed, and recorded as
    /// [`DesignSource::CompositionFile`].
    ///
    /// `None` — neither stated nor readable — records **nothing**: the composition is stored and no
    /// decision is attached to it, so [`freshness`] answers [`DesignFreshness::Absent`]. Guessing here
    /// would mean choosing between "the caller's answers" and "the model's choice", which is precisely
    /// the distinction the record exists to keep, over a request that declined to make it.
    pub fn accepted(
        stated: Option<DesignSource>,
        caller_design_dropped: bool,
    ) -> Option<DesignSource> {
        match (stated, caller_design_dropped) {
            (Some(stated), _) => Some(stated),
            (None, true) => Some(DesignSource::CompositionFile),
            (None, false) => None,
        }
    }
}

/// The **design record**: the decision that produced a composition, and the fingerprint of the
/// composition it ratifies.
///
/// Stored as a [`node::DESIGN_RECORD`] child of the anchor ([`edge::HAS_DESIGN_RECORD`]). The framework
/// and justification are mirrored here from the anchor deliberately: a decision should read as one
/// self-contained statement, and a reader asking "why is this an actors application?" should not have
/// to join two nodes to find out. `fingerprint` is what makes the record *checkable* rather than merely
/// informative — see [`freshness`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignRecord {
    pub framework: ApplicationFramework,
    pub justification: String,
    pub source: DesignSource,
    /// The design version this decision accepted (the design phase counts the specs it has decided).
    pub version: u32,
    pub decided_at: chrono::DateTime<chrono::Utc>,
    /// [`fingerprint`] of the composition **as decided**. The basis of staleness: the composition may
    /// be edited afterwards, and the record has to be able to say so.
    pub fingerprint: String,
}

impl DesignRecord {
    /// The record for `spec`, decided now, through `source`, as design `version`.
    pub fn now(spec: &ApplicationSpec, source: DesignSource, version: u32) -> Self {
        Self::at(spec, source, version, chrono::Utc::now())
    }

    /// [`Self::now`] with the clock passed in, so a caller that already stamped the decision — the
    /// design phase does — records *that* instant rather than a second one microseconds later.
    pub fn at(
        spec: &ApplicationSpec,
        source: DesignSource,
        version: u32,
        decided_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            framework: spec.framework,
            justification: spec.justification.clone(),
            source,
            version,
            decided_at,
            fingerprint: fingerprint(spec),
        }
    }

    /// Whether this record still ratifies `spec` — [`freshness`], for the single-record case.
    pub fn ratifies(&self, spec: &ApplicationSpec) -> bool {
        self.fingerprint == fingerprint(spec)
    }
}

/// The canonical fingerprint of a composition: order-exact, and stable for one design.
///
/// It is taken over the composition's **canonical JSON** — the very bytes the file-side record
/// (`SPIRE.application.json`) holds — so the fingerprint a design record carries is the hash of the
/// record a person can open, and two things that are the same design fingerprint the same however they
/// happened to be serialized. FNV-1a rather than a cryptographic hash: this answers "has it changed?",
/// between two values one process already holds, and it is a change detector rather than a defence
/// against a forger.
pub fn fingerprint(spec: &ApplicationSpec) -> String {
    let canonical = serde_json::to_string(spec).unwrap_or_default();
    format!("{:016x}", fnv1a64(&canonical))
}

fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Whether a [`DesignRecord`] still ratifies the composition beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignFreshness {
    /// No design was ever recorded for this project — there is nothing to be stale against.
    Absent,
    /// The record ratifies the composition as it stands.
    Current,
    /// **The composition moved on.** `recorded` is the fingerprint the design was decided from and
    /// `current` the one in the graph now, so a reader sees that they differ without re-deriving
    /// either.
    Stale { recorded: String, current: String },
}

impl DesignFreshness {
    pub fn is_current(&self) -> bool {
        matches!(self, DesignFreshness::Current)
    }

    /// Whether a design was decided at all — `Current` *or* `Stale`. Only [`Self::Absent`] is "no":
    /// a stale record is a decision that no longer applies, which is not the same answer as none.
    pub fn is_recorded(&self) -> bool {
        !matches!(self, DesignFreshness::Absent)
    }
}

/// Does `record` still ratify `spec`? The pure derivation behind
/// [`load_design_freshness`](super::composition_persist::load_design_freshness).
pub fn freshness(record: Option<&DesignRecord>, spec: &ApplicationSpec) -> DesignFreshness {
    let Some(record) = record else {
        return DesignFreshness::Absent;
    };
    let current = fingerprint(spec);
    if record.fingerprint == current {
        DesignFreshness::Current
    } else {
        DesignFreshness::Stale {
            recorded: record.fingerprint.clone(),
            current,
        }
    }
}

/// The `design_record` node for `record` — a projection of the *decision*, not of the composition.
pub fn design_record_node(record: &DesignRecord) -> CompositionNode {
    CompositionNode::new(node::DESIGN_RECORD, DESIGN_RECORD_NAME)
        .described(&record.justification)
        .with(P_FRAMEWORK, json!(record.framework.as_str()))
        .with(P_JUSTIFICATION, json!(record.justification))
        .with(P_SOURCE_KIND, json!(record.source.as_str()))
        .with(P_RECORD_VERSION, json!(record.version))
        .with(P_DECIDED_AT, json!(record.decided_at.to_rfc3339()))
        .with(P_FINGERPRINT, json!(record.fingerprint))
}

/// Attach `record` to a decomposed composition: the record node, plus the anchor's
/// [`edge::HAS_DESIGN_RECORD`].
///
/// Deliberately **not** part of [`decompose`], because decomposing a composition is not deciding one:
/// the graph a store writes is the composition, and a record is added when a design is *accepted*.
/// [`reconstruct`] ignores the node, so attaching a record never changes the composition read back.
pub fn attach_design_record(g: &mut CompositionGraph, record: &DesignRecord) {
    g.nodes.push(design_record_node(record));
    g.edges
        .push(edge(edge::HAS_DESIGN_RECORD, ROOT, DESIGN_RECORD_NAME));
}

/// Read the [`DesignRecord`] back out of a decomposed composition, when it carries one.
///
/// `Ok(None)` means *no record*, and never *a broken record*: a node that is there but missing its
/// framework, source, timestamp or fingerprint is refused, because reading a corrupt decision as no
/// decision is the confident kind of wrong — a caller would report the project as merely undecided and
/// let it be re-designed over a decision nobody ever superseded.
pub fn reconstruct_design_record(g: &CompositionGraph) -> Result<Option<DesignRecord>, String> {
    let Some(n) = g.nodes.iter().find(|n| n.node_type == node::DESIGN_RECORD) else {
        return Ok(None);
    };
    let missing = |what: &str| format!("the design record is missing its '{what}' property");
    let framework = prop_str(n, P_FRAMEWORK).ok_or_else(|| missing("framework"))?;
    let source = prop_str(n, P_SOURCE_KIND).ok_or_else(|| missing("source_kind"))?;
    let decided_at = prop_str(n, P_DECIDED_AT).ok_or_else(|| missing("decided_at"))?;
    let fingerprint = prop_str(n, P_FINGERPRINT).ok_or_else(|| missing("fingerprint"))?;
    Ok(Some(DesignRecord {
        framework: framework_from_name(&framework)?,
        justification: prop_str(n, P_JUSTIFICATION).unwrap_or_default(),
        source: DesignSource::from_name(&source)?,
        version: prop_u32(n, P_RECORD_VERSION).unwrap_or(1),
        decided_at: chrono::DateTime::parse_from_rfc3339(&decided_at)
            .map_err(|e| format!("the design record's 'decided_at' is not a timestamp: {e}"))?
            .with_timezone(&chrono::Utc),
        fingerprint,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::application_spec::{examples, parse_spec};

    fn pm25() -> ApplicationSpec {
        parse_spec(examples::PM25_METER).expect("the PM2.5 example parses")
    }

    fn count(g: &CompositionGraph, subtype: &str) -> usize {
        g.nodes.iter().filter(|n| n.node_type == subtype).count()
    }

    fn edge_count(g: &CompositionGraph, predicate: &str) -> usize {
        g.edges.iter().filter(|e| e.predicate == predicate).count()
    }

    #[test]
    fn pm25_decomposes_into_the_expected_shape() {
        let g = decompose(&pm25());
        assert_eq!(
            g.nodes.len(),
            16,
            "anchor + board + 2 facts + 8 units + 4 messages"
        );
        assert_eq!(
            g.edges.len(),
            29,
            "16 containment + 3 USES + 5 SENDS_TO + 5 WIRED_TO"
        );
        assert_eq!(count(&g, node::ANCHOR), 1);
        assert_eq!(count(&g, node::BOARD), 1);
        assert_eq!(count(&g, node::BOARD_FACT), 2);
        assert_eq!(count(&g, node::COMPONENT), 3);
        assert_eq!(count(&g, node::ACTOR), 5);
        assert_eq!(count(&g, node::MESSAGE), 4, "Tick/Reading/Report/Gesture");
        assert_eq!(edge_count(&g, edge::HAS_BOARD), 1);
        assert_eq!(edge_count(&g, edge::HAS_UNIT), 8);
        assert_eq!(edge_count(&g, edge::HAS_MESSAGE), 5);
        // Board facts hang off the board, not the anchor.
        assert_eq!(edge_count(&g, edge::HAS_BOARD_FACT), 2);
        assert!(g
            .edges
            .iter()
            .any(|e| e.predicate == edge::HAS_BOARD_FACT && e.from_name == BOARD_NAME));
        // The composition's own wiring is a traversable relationship.
        assert!(g.edges.iter().any(|e| e.predicate == edge::USES
            && e.from_name == "sampler"
            && e.to_name == "sps30"));
        assert!(g.edges.iter().any(|e| e.predicate == edge::SENDS_TO
            && e.from_name == "touch"
            && e.to_name == "sampler"));
        assert!(g.edges.iter().any(|e| e.predicate == edge::WIRED_TO
            && e.from_name == "sampler"
            && e.to_name == "air_quality"));
    }

    #[test]
    fn pm25_roundtrips_through_the_graph() {
        let spec = pm25();
        assert_eq!(reconstruct(&decompose(&spec)).expect("reconstruct"), spec);
    }

    #[test]
    fn the_ramen_composition_roundtrips_through_the_graph() {
        let spec = parse_spec(examples::INSECT_TRAP).expect("the insect-trap example parses");
        assert_eq!(reconstruct(&decompose(&spec)).expect("reconstruct"), spec);
    }

    #[test]
    fn facets_and_unit_annotations_survive_the_roundtrip() {
        let spec = parse_spec(
            r#"{
              "framework": "actors",
              "justification": "a battery-powered logger",
              "board": { "chip": "esp32c6", "bsp": "", "hal": "" },
              "board_facts": [ { "bus": "i2c", "device": "sht20", "address": "0x40" } ],
              "units": [
                { "id": "sht20", "kind": "component", "role": "driver", "source": "stub",
                  "bus": "i2c", "provides": "temperature" },
                { "id": "logger", "kind": "actor", "message": "Tick", "uses": ["sht20"],
                  "state": "the last sample", "sends_to": ["uplink"],
                  "sleeps": true, "wake": "timer@60s", "storage_role": "sink" },
                { "id": "uplink", "kind": "actor", "message": "Batch", "network_role": "uplink" }
              ],
              "wiring": [ "logger -> uplink" ],
              "power": { "budget_mah": 2000, "target_days": 30 },
              "storage": { "medium": "flash", "retention": "until_uploaded" },
              "network": { "transport": "wifi", "provision": "softap" },
              "security": { "requires": ["tls", "encrypted_nvs"] }
            }"#,
        )
        .expect("the facet example parses");

        let g = decompose(&spec);
        assert_eq!(count(&g, node::FACET), 4);
        for kind in ["power", "storage", "network", "security"] {
            assert!(
                g.nodes
                    .iter()
                    .any(|n| n.node_type == node::FACET && n.name == kind),
                "there is a '{kind}' facet node"
            );
        }
        assert_eq!(reconstruct(&g).expect("reconstruct"), spec);
    }

    #[test]
    fn reconstruct_errors_on_a_missing_anchor() {
        let err = reconstruct(&CompositionGraph::default()).unwrap_err();
        assert!(err.contains("no composition anchor"));
    }

    fn pm25_record() -> DesignRecord {
        DesignRecord::now(&pm25(), DesignSource::Model, 1)
    }

    #[test]
    fn the_design_record_attaches_without_disturbing_the_composition() {
        let spec = pm25();
        let mut g = decompose(&spec);
        let record = DesignRecord::now(&spec, DesignSource::Answers, 3);
        attach_design_record(&mut g, &record);

        assert_eq!(
            count(&g, node::DESIGN_RECORD),
            1,
            "one record per composition"
        );
        assert!(
            g.edges
                .iter()
                .any(|e| e.predicate == edge::HAS_DESIGN_RECORD && e.from_name == ROOT),
            "the record hangs off the anchor"
        );

        // The point of attaching it to the *same* graph: the composition read back is untouched.
        assert_eq!(reconstruct(&g).expect("reconstruct"), spec);
        assert_eq!(
            reconstruct_design_record(&g).expect("the record reads back"),
            Some(record)
        );
    }

    #[test]
    fn every_design_source_round_trips_through_its_node() {
        for source in [
            DesignSource::Answers,
            DesignSource::CompositionFile,
            DesignSource::Model,
        ] {
            let spec = pm25();
            let mut g = decompose(&spec);
            let record = DesignRecord::now(&spec, source, 1);
            attach_design_record(&mut g, &record);
            let back = reconstruct_design_record(&g)
                .expect("the record reads back")
                .expect("there is a record");
            assert_eq!(
                back.source,
                source,
                "{} did not round-trip",
                source.as_str()
            );
            // The mirrored framework/justification make the record read as one statement.
            assert_eq!(back.framework, spec.framework);
            assert_eq!(back.justification, spec.justification);
        }
    }

    #[test]
    fn the_door_a_scaffold_records_is_stated_or_read_off_the_tree() {
        // A caller that says which door its design came through is believed — including when the tree
        // also decided, because only the caller knows whether it read this project's own file.
        for stated in [
            DesignSource::Answers,
            DesignSource::CompositionFile,
            DesignSource::Model,
        ] {
            assert_eq!(
                DesignSource::accepted(Some(stated), false),
                Some(stated),
                "a stated door survives a tree that decided nothing"
            );
            assert_eq!(
                DesignSource::accepted(Some(stated), true),
                Some(stated),
                "and one that decided: the door is not recoverable from the spec"
            );
        }

        // Nothing stated, over a tree whose own `composition.spire` was the design that got written:
        // the file decided, which the leg knows because it dropped the caller's copy.
        assert_eq!(
            DesignSource::accepted(None, true),
            Some(DesignSource::CompositionFile),
            "a tree that decided is read off the tree"
        );

        // Nothing stated, over a tree that decided nothing: no record at all. The composition is still
        // stored; what is not invented is a door — `answers` and `model` are the reader's distinction.
        assert_eq!(
            DesignSource::accepted(None, false),
            None,
            "an unstated door is not guessed at"
        );
    }

    #[test]
    fn a_record_ratifies_the_composition_it_was_decided_from() {
        let spec = pm25();
        let record = DesignRecord::now(&spec, DesignSource::Answers, 1);

        assert!(record.ratifies(&spec));
        let fresh = freshness(Some(&record), &spec);
        assert_eq!(fresh, DesignFreshness::Current);
        assert!(fresh.is_current());
        assert!(fresh.is_recorded());
    }

    #[test]
    fn a_record_ratifies_a_reserialized_copy_of_the_same_design() {
        // The fingerprint is over the canonical JSON, so a second read of the same file — the ordinary
        // way a spec reaches this module — is the same design and must not read as stale.
        let text = crate::build::application_spec::examples::PM25_METER;
        let a = parse_spec(text).expect("parses");
        let b = parse_spec(text).expect("parses");
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert!(DesignRecord::now(&a, DesignSource::Model, 1).ratifies(&b));
    }

    #[test]
    fn no_record_is_absent_rather_than_stale() {
        let fresh = freshness(None, &pm25());
        assert_eq!(fresh, DesignFreshness::Absent);
        assert!(!fresh.is_current());
        assert!(
            !fresh.is_recorded(),
            "absent is 'no decision', not a stale one"
        );
    }

    #[test]
    fn editing_the_composition_makes_the_record_stale() {
        let spec = pm25();
        let record = pm25_record();

        // The supported way to change a design: edit it. One line is enough to invalidate the record,
        // which is the whole point — a reader must never be told the old decision still stands.
        let mut edited = spec.clone();
        edited.justification = "a second look at the same product".to_string();

        assert!(!record.ratifies(&edited));
        match freshness(Some(&record), &edited) {
            DesignFreshness::Stale { recorded, current } => {
                assert_eq!(
                    recorded, record.fingerprint,
                    "the fingerprint it was decided from"
                );
                assert_eq!(current, fingerprint(&edited), "the one in the graph now");
                assert_ne!(recorded, current);
            }
            other => panic!("an edited composition must read as stale, got {other:?}"),
        }
        // …and the same record still ratifies the composition it *was* decided from, so staleness is
        // about the composition and not about the record having gone off on its own.
        assert!(record.ratifies(&spec));
    }

    #[test]
    fn the_fingerprint_is_order_exact_and_framework_sensitive() {
        let spec = pm25();

        let mut reordered = spec.clone();
        reordered.units.swap(0, 1);
        assert_ne!(
            fingerprint(&spec),
            fingerprint(&reordered),
            "the order of the units is part of the design"
        );

        let mut other_framework = spec.clone();
        other_framework.framework = ApplicationFramework::Ramen;
        assert_ne!(
            fingerprint(&spec),
            fingerprint(&other_framework),
            "the framework is the first thing a design decides"
        );

        assert_eq!(fingerprint(&spec), fingerprint(&spec.clone()));
    }

    #[test]
    fn a_broken_design_record_is_refused_rather_than_read_as_no_record() {
        // A composition carries no record until a design is decided — that is `Ok(None)`, not an error.
        assert_eq!(
            reconstruct_design_record(&decompose(&pm25())),
            Ok(None),
            "an undecided composition has no record"
        );

        let mut g = CompositionGraph::default();
        g.nodes.push(
            CompositionNode::new(node::DESIGN_RECORD, DESIGN_RECORD_NAME)
                .with(P_FRAMEWORK, json!("actors"))
                .with(P_SOURCE_KIND, json!("model"))
                .with(P_DECIDED_AT, json!("2026-10-01T00:00:00Z")),
        );
        let err = reconstruct_design_record(&g).unwrap_err();
        assert!(err.contains("fingerprint"), "{err}");

        // A source the tool does not know is a refusal too, never a pass-through: an unreadable
        // provenance is worse than none, because a reader trusts it.
        g.nodes[0] = CompositionNode::new(node::DESIGN_RECORD, DESIGN_RECORD_NAME)
            .with(P_FRAMEWORK, json!("actors"))
            .with(P_SOURCE_KIND, json!("vibes"))
            .with(P_DECIDED_AT, json!("2026-10-01T00:00:00Z"))
            .with(P_FINGERPRINT, json!("deadbeefdeadbeef"));
        let err = reconstruct_design_record(&g).unwrap_err();
        assert!(err.contains("vibes"), "{err}");
    }
}
