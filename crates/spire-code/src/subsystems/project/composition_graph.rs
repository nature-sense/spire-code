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
//!   kind), and [`node::MESSAGE`] (a message type, deduplicated by name);
//! * **edges** — containment ([`edge::HAS_BOARD`], [`edge::HAS_BOARD_FACT`], [`edge::HAS_FACET`],
//!   [`edge::HAS_UNIT`], [`edge::HAS_MESSAGE`]) and the composition's own wiring ([`edge::USES`],
//!   [`edge::SENDS_TO`], [`edge::WIRED_TO`]). The board's facts hang off the **board**, not the anchor,
//!   because a fact is about a board.
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
    framework_from_name, ApplicationSpec, BoardChoice, BoardFact, ComponentRole, NetworkFacet,
    PowerFacet, SecurityFacet, StorageFacet, Unit, UnitKind, UnitSource,
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
}

/// Edge predicates of the decomposed composition graph.
pub mod edge {
    pub const HAS_BOARD: &str = "HAS_BOARD";
    pub const HAS_BOARD_FACT: &str = "HAS_BOARD_FACT";
    pub const HAS_FACET: &str = "HAS_FACET";
    pub const HAS_UNIT: &str = "HAS_UNIT";
    pub const HAS_MESSAGE: &str = "HAS_MESSAGE";
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
}
