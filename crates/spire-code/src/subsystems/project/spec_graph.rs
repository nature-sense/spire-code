// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Graph-native AppSpec** — the spec as a decomposed property graph.
//!
//! The graph is Spire's single source of truth, so the AppSpec lives there as
//! typed nodes + edges (one node per entity: type, field, variant, actor,
//! handler, method, screen, action, binding, navigation, layout element, graph
//! node/edge type), NOT as a JSON blob. Markdown is a deterministic projection
//! of this graph, and codegen reads it piece-wise.
//!
//! This module is pure: [`decompose`] turns an [`AppSpec`] into a [`SpecGraph`]
//! of node/edge *specs* (no actor I/O), and [`reconstruct`] rebuilds the
//! [`AppSpec`] from those specs. The memory-graph actor layer maps the specs to
//! `AttrNode`/`CreateRelationship` messages. Round-tripping is a hard property:
//! `reconstruct(decompose(spec)) == spec` is tested for the GIS reference.

use super::spec::{
    ActorSpec, AppMeta, AppSpec, BridgeMethod, DomainType, Field, GraphEdgeType, GraphNodeType,
    GraphSchema, LayoutNode, Screen, Type, UiAction, UiBinding, UiNavigation,
};

/// Canonical type-expression grammar used for leaf `ty` properties:
/// `str | int | float | bool | list<T> | option<T> | named<T> | record(f:ty;…)`.
pub fn ty_string(ty: &Type) -> String {
    match ty {
        Type::Str => "str".to_string(),
        Type::Int => "int".to_string(),
        Type::Float => "float".to_string(),
        Type::Bool => "bool".to_string(),
        Type::List { of } => format!("list<{}>", ty_string(of)),
        Type::Option { of } => format!("option<{}>", ty_string(of)),
        Type::Named { name } => format!("named<{name}>"),
        Type::Record { fields } => {
            let inner = fields
                .iter()
                .map(|f| format!("{}:{}", f.name, ty_string(&f.ty)))
                .collect::<Vec<_>>()
                .join(";");
            format!("record({inner})")
        }
    }
}

/// Inverse of [`ty_string`] — a small recursive-descent parser. Deterministic
/// and total on every string [`ty_string`] produces.
pub fn type_from_string(s: &str) -> Result<Type, String> {
    let mut p = TypeParser::new(s);
    let ty = p.parse_type()?;
    if p.peek().is_some() {
        return Err(format!("trailing chars in type '{s}'"));
    }
    Ok(ty)
}

struct TypeParser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> TypeParser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            chars: src.chars().peekable(),
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.chars.peek().copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.chars.peek(), Some(c) if c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn eat(&mut self, expected: char) -> Result<(), String> {
        match self.chars.next() {
            Some(c) if c == expected => Ok(()),
            other => Err(format!("expected '{expected}', got {other:?}")),
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        self.skip_ws();
        let mut out = String::new();
        while let Some(c) = self.chars.peek() {
            if c.is_alphanumeric() || *c == '_' || *c == '/' || *c == '.' || *c == '-' {
                out.push(*c);
                self.chars.next();
            } else {
                break;
            }
        }
        if out.is_empty() {
            Err("expected an identifier".to_string())
        } else {
            Ok(out)
        }
    }

    fn parse_type(&mut self) -> Result<Type, String> {
        let first = self.ident()?;
        match first.as_str() {
            "str" => Ok(Type::Str),
            "int" => Ok(Type::Int),
            "float" => Ok(Type::Float),
            "bool" => Ok(Type::Bool),
            "list" => {
                self.eat('<')?;
                let of = Box::new(self.parse_type()?);
                self.eat('>')?;
                Ok(Type::List { of })
            }
            "option" => {
                self.eat('<')?;
                let of = Box::new(self.parse_type()?);
                self.eat('>')?;
                Ok(Type::Option { of })
            }
            "named" => {
                self.eat('<')?;
                let name = self.ident()?;
                self.eat('>')?;
                Ok(Type::Named { name })
            }
            "record" => {
                self.eat('(')?;
                let mut fields = Vec::new();
                if self.peek() == Some(')') {
                    self.chars.next();
                    return Ok(Type::Record { fields });
                }
                loop {
                    let fname = self.ident()?;
                    self.eat(':')?;
                    let fty = self.parse_type()?;
                    fields.push(Field {
                        name: fname,
                        ty: fty,
                    });
                    match self.peek() {
                        Some(';') => {
                            self.chars.next();
                        }
                        Some(')') => {
                            self.chars.next();
                            break;
                        }
                        other => return Err(format!("expected ';' or ')', got {other:?}")),
                    }
                }
                Ok(Type::Record { fields })
            }
            other => Err(format!("unknown type kind '{other}'")),
        }
    }
}

/// Node-type discriminators for the decomposed spec graph.
pub mod node {
    pub const APPSPEC: &str = "appspec";
    pub const TYPE: &str = "spec_type";
    pub const FIELD: &str = "spec_field";
    pub const VARIANT: &str = "spec_variant";
    pub const GRAPH_NODE: &str = "spec_graph_node";
    pub const GRAPH_EDGE: &str = "spec_graph_edge";
    pub const ACTOR: &str = "spec_actor";
    pub const HANDLER: &str = "spec_handler";
    pub const METHOD: &str = "spec_method";
    pub const SCREEN: &str = "spec_screen";
    pub const ACTION: &str = "spec_action";
    pub const BINDING: &str = "spec_binding";
    pub const NAVIGATION: &str = "spec_navigation";
    pub const LAYOUT: &str = "spec_layout";
}

/// Edge predicates of the decomposed spec graph.
pub mod edge {
    pub const HAS_TYPE: &str = "HAS_TYPE";
    pub const HAS_GRAPH_NODE: &str = "HAS_GRAPH_NODE";
    pub const HAS_GRAPH_EDGE: &str = "HAS_GRAPH_EDGE";
    pub const HAS_ACTOR: &str = "HAS_ACTOR";
    pub const HAS_METHOD: &str = "HAS_METHOD";
    pub const HAS_SCREEN: &str = "HAS_SCREEN";
    pub const HAS_FIELD: &str = "HAS_FIELD";
    pub const HAS_VARIANT: &str = "HAS_VARIANT";
    pub const HAS_HANDLER: &str = "HAS_HANDLER";
    pub const HAS_PARAM: &str = "HAS_PARAM";
    pub const HAS_STATE: &str = "HAS_STATE";
    pub const HAS_ACTION: &str = "HAS_ACTION";
    pub const HAS_BINDING: &str = "HAS_BINDING";
    pub const HAS_NAVIGATION: &str = "HAS_NAVIGATION";
    pub const HAS_LAYOUT: &str = "HAS_LAYOUT";
    pub const LAYOUT_CHILD: &str = "LAYOUT_CHILD";
    pub const OF_TYPE: &str = "OF_TYPE";
    pub const CALLS: &str = "CALLS";
    pub const BINDS: &str = "BINDS";
    pub const NAVIGATES_TO: &str = "NAVIGATES_TO";
    pub const HANDLED_BY: &str = "HANDLED_BY";
}

/// Ordering property carried by ordered collections (fields, methods, screens…).
pub const PROP_ORDER: &str = "order";

/// A node specification in the decomposed spec graph (logical, pre-actor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecNode {
    /// Discriminator (one of the `node::` constants).
    pub node_type: String,
    /// Logical, graph-unique identity (e.g. `LayerInfo:id` for a field).
    pub name: String,
    pub description: Option<String>,
    pub properties: Vec<(String, serde_json::Value)>,
}

/// An edge specification (endpoints by logical node `name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecEdge {
    pub predicate: String,
    pub from_name: String,
    pub to_name: String,
    pub properties: Vec<(String, serde_json::Value)>,
}

/// The decomposed spec as pure data (no actor I/O).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpecGraph {
    pub nodes: Vec<SpecNode>,
    pub edges: Vec<SpecEdge>,
}

pub(crate) fn prop<'a>(n: &'a SpecNode, key: &str) -> Option<&'a serde_json::Value> {
    n.properties.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

pub(crate) fn prop_str(n: &SpecNode, key: &str) -> Option<String> {
    prop(n, key).and_then(|v| v.as_str().map(String::from))
}

fn node(node_type: &str, name: &str) -> SpecNode {
    SpecNode {
        node_type: node_type.to_string(),
        name: name.to_string(),
        description: None,
        properties: Vec::new(),
    }
}

fn with_props(mut n: SpecNode, props: &[(&str, serde_json::Value)]) -> SpecNode {
    for (k, v) in props {
        n.properties.push(((*k).to_string(), v.clone()));
    }
    n
}

fn edge(predicate: &str, from_name: &str, to_name: &str) -> SpecEdge {
    SpecEdge {
        predicate: predicate.to_string(),
        from_name: from_name.to_string(),
        to_name: to_name.to_string(),
        properties: Vec::new(),
    }
}

/// Add a field child node + ordering + an `OF_TYPE` edge for direct named refs.
fn push_field(g: &mut SpecGraph, parent: &str, kind: &str, order: usize, f: &Field) {
    let fname = format!("{parent}.{kind}.{}", f.name);
    g.nodes.push(with_props(
        node(node::FIELD, &fname),
        &[
            ("field", serde_json::json!(f.name)),
            ("ty", serde_json::json!(ty_string(&f.ty))),
            (PROP_ORDER, serde_json::json!(order)),
        ],
    ));
    g.edges.push(edge(edge::HAS_FIELD, parent, &fname));
    if let Type::Named { name } = &f.ty {
        g.edges.push(edge(edge::OF_TYPE, &fname, name));
    }
}

fn push_variants(g: &mut SpecGraph, tname: &str, variants: &[String]) {
    for (j, v) in variants.iter().enumerate() {
        let vname = format!("{tname}.variant.{v}");
        g.nodes.push(with_props(
            node(node::VARIANT, &vname),
            &[
                ("variant", serde_json::json!(v)),
                (PROP_ORDER, serde_json::json!(j)),
            ],
        ));
        g.edges.push(edge(edge::HAS_VARIANT, tname, &vname));
    }
}

/// Types + the app's own graph schema (the first decomposition pass).
fn decompose_core(spec: &AppSpec) -> SpecGraph {
    let root = spec.app.name.clone();
    let mut g = SpecGraph::default();
    g.nodes.push(with_props(
        node(node::APPSPEC, &root),
        &[
            ("goal", serde_json::json!(spec.app.goal)),
            ("version", serde_json::json!(1)),
        ],
    ));

    for (i, t) in spec.types.iter().enumerate() {
        let tname = t.name();
        let kind = match t {
            DomainType::Record { .. } => "record",
            DomainType::Enum { .. } => "enum",
        };
        g.nodes.push(with_props(
            node(node::TYPE, tname),
            &[
                ("kind", serde_json::json!(kind)),
                (PROP_ORDER, serde_json::json!(i)),
            ],
        ));
        g.edges.push(edge(edge::HAS_TYPE, &root, tname));
        match t {
            DomainType::Record { fields, .. } => {
                for (j, f) in fields.iter().enumerate() {
                    push_field(&mut g, tname, "field", j, f);
                }
            }
            DomainType::Enum { variants, .. } => push_variants(&mut g, tname, variants),
        }
    }

    for (i, gn) in spec.graph.nodes.iter().enumerate() {
        g.nodes.push(with_props(
            SpecNode {
                description: Some(gn.description.clone()),
                ..node(node::GRAPH_NODE, &gn.name)
            },
            &[(PROP_ORDER, serde_json::json!(i))],
        ));
        g.edges.push(edge(edge::HAS_GRAPH_NODE, &root, &gn.name));
        for (j, f) in gn.fields.iter().enumerate() {
            push_field(&mut g, &gn.name, "gfield", j, f);
        }
    }
    for (i, ge) in spec.graph.edges.iter().enumerate() {
        let ename = format!("{root}.edge.{}", ge.name);
        g.nodes.push(with_props(
            SpecNode {
                description: Some(ge.description.clone()),
                ..node(node::GRAPH_EDGE, &ename)
            },
            &[
                ("name", serde_json::json!(ge.name)),
                ("from", serde_json::json!(ge.from)),
                ("to", serde_json::json!(ge.to)),
                (PROP_ORDER, serde_json::json!(i)),
            ],
        ));
        g.edges.push(edge(edge::HAS_GRAPH_EDGE, &root, &ename));
    }
    g
}

/// Decompose a validated [`AppSpec`] into its graph-native representation.
/// Deterministic: same spec in → same node/edge set out, in the same order.
pub fn decompose(spec: &AppSpec) -> SpecGraph {
    let mut g = decompose_core(spec);
    let root = spec.app.name.clone();

    // ── actors ────────────────────────────────────────────────────────────
    for (i, a) in spec.actors.iter().enumerate() {
        g.nodes.push(with_props(
            SpecNode {
                description: Some(a.description.clone()),
                ..node(node::ACTOR, &a.name)
            },
            &[
                ("uses", serde_json::json!(a.uses)),
                (PROP_ORDER, serde_json::json!(i)),
            ],
        ));
        g.edges.push(edge(edge::HAS_ACTOR, &root, &a.name));
        for (j, m) in a.handlers.iter().enumerate() {
            let hname = format!("{}.handler.{}", a.name, m);
            g.nodes.push(with_props(
                node(node::HANDLER, &hname),
                &[
                    ("method", serde_json::json!(m)),
                    (PROP_ORDER, serde_json::json!(j)),
                ],
            ));
            g.edges.push(edge(edge::HAS_HANDLER, &a.name, &hname));
        }
        for (j, f) in a.state.iter().enumerate() {
            push_field(&mut g, &a.name, "state", j, f);
        }
    }

    // ── bridge methods (params + derived HANDLED_BY routing) ──────────────
    for (i, m) in spec.bridge.iter().enumerate() {
        g.nodes.push(with_props(
            SpecNode {
                description: Some(m.description.clone()),
                ..node(node::METHOD, &m.method)
            },
            &[
                ("result", serde_json::json!(ty_string(&m.result))),
                (PROP_ORDER, serde_json::json!(i)),
            ],
        ));
        g.edges.push(edge(edge::HAS_METHOD, &root, &m.method));
        for (j, p) in m.params.iter().enumerate() {
            push_field(&mut g, &m.method, "param", j, p);
        }
        if let Type::Named { name } = &m.result {
            g.edges.push(edge(edge::OF_TYPE, &m.method, name));
        }
        for a in &spec.actors {
            if a.handlers.iter().any(|h| h == &m.method) {
                g.edges.push(edge(edge::HANDLED_BY, &m.method, &a.name));
            }
        }
    }

    // ── screens (actions/bindings/navigation/layout) ──────────────────────
    for (i, s) in spec.ui.iter().enumerate() {
        g.nodes.push(with_props(
            node(node::SCREEN, &s.id),
            &[
                ("title", serde_json::json!(s.title)),
                (PROP_ORDER, serde_json::json!(i)),
            ],
        ));
        g.edges.push(edge(edge::HAS_SCREEN, &root, &s.id));
        for (j, act) in s.actions.iter().enumerate() {
            let aname = format!("{}.action.{}", s.id, act.id);
            g.nodes.push(with_props(
                SpecNode {
                    description: Some(act.description.clone()),
                    ..node(node::ACTION, &aname)
                },
                &[
                    ("id", serde_json::json!(act.id)),
                    ("bridge", serde_json::json!(act.bridge)),
                    (PROP_ORDER, serde_json::json!(j)),
                ],
            ));
            g.edges.push(edge(edge::HAS_ACTION, &s.id, &aname));
            g.edges.push(edge(edge::CALLS, &aname, &act.bridge));
        }
        for (j, b) in s.bindings.iter().enumerate() {
            let bname = format!("{}.binding.{}", s.id, b.field);
            g.nodes.push(with_props(
                node(node::BINDING, &bname),
                &[
                    ("field", serde_json::json!(b.field)),
                    ("method", serde_json::json!(b.method)),
                    (PROP_ORDER, serde_json::json!(j)),
                ],
            ));
            g.edges.push(edge(edge::HAS_BINDING, &s.id, &bname));
            g.edges.push(edge(edge::BINDS, &bname, &b.method));
        }
        for (j, n) in s.navigation.iter().enumerate() {
            let nname = format!("{}.navigation.{}", s.id, n.action_id);
            g.nodes.push(with_props(
                node(node::NAVIGATION, &nname),
                &[
                    ("action_id", serde_json::json!(n.action_id)),
                    ("to", serde_json::json!(n.to)),
                    (PROP_ORDER, serde_json::json!(j)),
                ],
            ));
            g.edges.push(edge(edge::HAS_NAVIGATION, &s.id, &nname));
            g.edges.push(edge(edge::NAVIGATES_TO, &nname, &n.to));
        }
        push_layout(&mut g, &s.id, &s.layout, 0);
    }
    g
}

/// Layout sub-tree into `spec_layout` nodes + `LAYOUT_CHILD` edges. Children
/// are numbered by a monotonic traversal counter, so sibling subtrees never
/// collide and order survives the graph.
fn push_layout(g: &mut SpecGraph, screen: &str, layout: &LayoutNode, index: usize) -> usize {
    let lname = format!("{screen}.layout.{index}");
    let mut props: Vec<(&str, serde_json::Value)> = vec![(PROP_ORDER, serde_json::json!(index))];
    let mut children: &[LayoutNode] = &[];
    match layout {
        LayoutNode::Empty => props.push(("kind", serde_json::json!("empty"))),
        LayoutNode::VStack { children: c } => {
            props.push(("kind", serde_json::json!("vstack")));
            children = c;
        }
        LayoutNode::HStack { children: c } => {
            props.push(("kind", serde_json::json!("hstack")));
            children = c;
        }
        LayoutNode::List { item } => {
            props.push(("kind", serde_json::json!("list")));
            children = std::slice::from_ref(item);
        }
        LayoutNode::Text { text } => props.extend([
            ("kind", serde_json::json!("text")),
            ("text", serde_json::json!(text)),
        ]),
        LayoutNode::Button { label, action } => props.extend([
            ("kind", serde_json::json!("button")),
            ("label", serde_json::json!(label)),
            ("action", serde_json::json!(action)),
        ]),
        LayoutNode::Input { placeholder, bind } => props.extend([
            ("kind", serde_json::json!("input")),
            ("placeholder", serde_json::json!(placeholder)),
            ("bind", serde_json::json!(bind)),
        ]),
        LayoutNode::Spacer => props.push(("kind", serde_json::json!("spacer"))),
    }
    g.nodes.push(with_props(node(node::LAYOUT, &lname), &props));
    if index == 0 {
        g.edges.push(edge(edge::HAS_LAYOUT, screen, &lname));
    }
    let mut next = index + 1;
    for c in children {
        let child_name = format!("{screen}.layout.{next}");
        g.edges.push(edge(edge::LAYOUT_CHILD, &lname, &child_name));
        next = push_layout(g, screen, c, next);
    }
    next
}

// ── Reconstruct: SpecGraph → AppSpec ─────────────────────────────────────

fn order_of(n: &SpecNode) -> usize {
    prop(n, PROP_ORDER).and_then(|v| v.as_u64()).unwrap_or(0) as usize
}

/// Child nodes of `parent` via `predicate`, ordered by their `order` property.
pub(crate) fn children<'a>(g: &'a SpecGraph, parent: &str, predicate: &str) -> Vec<&'a SpecNode> {
    let mut out: Vec<&SpecNode> = g
        .edges
        .iter()
        .filter(|e| e.predicate == predicate && e.from_name == parent)
        .filter_map(|e| g.nodes.iter().find(|n| n.name == e.to_name))
        .collect();
    out.sort_by_key(|n| order_of(n));
    out
}

pub(crate) fn fields_of<'a>(g: &'a SpecGraph, parent: &str) -> Result<Vec<Field>, String> {
    children(g, parent, edge::HAS_FIELD)
        .into_iter()
        .map(|n| {
            let name = prop_str(n, "field").ok_or("field node missing 'field' prop")?;
            let ty = prop_str(n, "ty").ok_or("field node missing 'ty' prop")?;
            Ok(Field {
                name,
                ty: type_from_string(&ty)?,
            })
        })
        .collect()
}

pub(crate) fn rebuild_layout(g: &SpecGraph, node_name: &str) -> Result<LayoutNode, String> {
    let n = g
        .nodes
        .iter()
        .find(|n| n.name == node_name)
        .ok_or("layout node not found")?;
    let kind = prop_str(n, "kind").unwrap_or_default();
    let kids = children(g, node_name, edge::LAYOUT_CHILD);
    Ok(match kind.as_str() {
        "empty" => LayoutNode::Empty,
        "vstack" => LayoutNode::VStack {
            children: kids
                .iter()
                .map(|k| rebuild_layout(g, &k.name))
                .collect::<Result<_, _>>()?,
        },
        "hstack" => LayoutNode::HStack {
            children: kids
                .iter()
                .map(|k| rebuild_layout(g, &k.name))
                .collect::<Result<_, _>>()?,
        },
        "list" => LayoutNode::List {
            item: Box::new(rebuild_layout(g, &kids[0].name)?),
        },
        "text" => LayoutNode::Text {
            text: prop_str(n, "text").ok_or("text layout missing 'text'")?,
        },
        "button" => LayoutNode::Button {
            label: prop_str(n, "label").ok_or("button missing 'label'")?,
            action: prop_str(n, "action").ok_or("button missing 'action'")?,
        },
        "input" => LayoutNode::Input {
            placeholder: prop_str(n, "placeholder").ok_or("input missing 'placeholder'")?,
            bind: prop_str(n, "bind").ok_or("input missing 'bind'")?,
        },
        "spacer" => LayoutNode::Spacer,
        other => return Err(format!("unknown layout kind '{other}'")),
    })
}

/// Rebuild an [`AppSpec`] from a decomposed [`SpecGraph`].
pub fn reconstruct(g: &SpecGraph) -> Result<AppSpec, String> {
    let root = g
        .nodes
        .iter()
        .find(|n| n.node_type == node::APPSPEC)
        .ok_or("no appspec node")?;
    let name = root.name.clone();
    let goal = prop_str(root, "goal").unwrap_or_default();

    let mut type_nodes: Vec<&SpecNode> = g
        .nodes
        .iter()
        .filter(|n| n.node_type == node::TYPE)
        .collect();
    type_nodes.sort_by_key(|n| order_of(n));
    let mut types = Vec::new();
    for t in type_nodes {
        let kind = prop_str(t, "kind").ok_or("spec_type missing 'kind'")?;
        types.push(match kind.as_str() {
            "record" => DomainType::Record {
                name: t.name.clone(),
                fields: fields_of(g, &t.name)?,
            },
            "enum" => DomainType::Enum {
                name: t.name.clone(),
                variants: children(g, &t.name, edge::HAS_VARIANT)
                    .into_iter()
                    .map(|v| prop_str(v, "variant").unwrap_or_default())
                    .collect(),
            },
            other => return Err(format!("unknown spec_type kind '{other}'")),
        });
    }

    let mut gn_nodes: Vec<&SpecNode> = g
        .nodes
        .iter()
        .filter(|n| n.node_type == node::GRAPH_NODE)
        .collect();
    gn_nodes.sort_by_key(|n| order_of(n));
    let mut ge_nodes: Vec<&SpecNode> = g
        .nodes
        .iter()
        .filter(|n| n.node_type == node::GRAPH_EDGE)
        .collect();
    ge_nodes.sort_by_key(|n| order_of(n));
    let graph = GraphSchema {
        nodes: gn_nodes
            .into_iter()
            .map(|n| GraphNodeType {
                name: n.name.clone(),
                description: n.description.clone().unwrap_or_default(),
                fields: fields_of(g, &n.name).unwrap_or_default(),
            })
            .collect(),
        edges: ge_nodes
            .into_iter()
            .map(|n| GraphEdgeType {
                name: prop_str(n, "name").unwrap_or_default(),
                from: prop_str(n, "from").unwrap_or_default(),
                to: prop_str(n, "to").unwrap_or_default(),
                description: n.description.clone().unwrap_or_default(),
            })
            .collect(),
    };

    let mut actor_nodes: Vec<&SpecNode> = g
        .nodes
        .iter()
        .filter(|n| n.node_type == node::ACTOR)
        .collect();
    actor_nodes.sort_by_key(|n| order_of(n));
    let mut actors = Vec::new();
    for a in actor_nodes {
        let uses = prop(a, "uses")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        actors.push(ActorSpec {
            name: a.name.clone(),
            description: a.description.clone().unwrap_or_default(),
            handlers: children(g, &a.name, edge::HAS_HANDLER)
                .into_iter()
                .map(|h| prop_str(h, "method").unwrap_or_default())
                .collect(),
            state: fields_of(g, &a.name)?,
            uses,
        });
    }

    let mut out = AppSpec {
        app: AppMeta {
            name: name.clone(),
            goal,
        },
        graph,
        types,
        actors,
        bridge: Vec::new(),
        ui: Vec::new(),
    };
    finish_reconstruct(g, &mut out)?;
    Ok(out)
}

/// Bridge methods + screens (built in a second pass over the graph).
fn finish_reconstruct(g: &SpecGraph, out: &mut AppSpec) -> Result<(), String> {
    let mut method_nodes: Vec<&SpecNode> = g
        .nodes
        .iter()
        .filter(|n| n.node_type == node::METHOD)
        .collect();
    method_nodes.sort_by_key(|n| order_of(n));
    for m in method_nodes {
        let result = type_from_string(&prop_str(m, "result").ok_or("method missing 'result'")?)?;
        out.bridge.push(BridgeMethod {
            method: m.name.clone(),
            description: m.description.clone().unwrap_or_default(),
            params: fields_of(g, &m.name)?,
            result,
        });
    }

    let mut screen_nodes: Vec<&SpecNode> = g
        .nodes
        .iter()
        .filter(|n| n.node_type == node::SCREEN)
        .collect();
    screen_nodes.sort_by_key(|n| order_of(n));
    for s in screen_nodes {
        let actions = children(g, &s.name, edge::HAS_ACTION)
            .into_iter()
            .map(|n| UiAction {
                id: prop_str(n, "id").unwrap_or_default(),
                description: n.description.clone().unwrap_or_default(),
                bridge: prop_str(n, "bridge").unwrap_or_default(),
            })
            .collect();
        let bindings = children(g, &s.name, edge::HAS_BINDING)
            .into_iter()
            .map(|n| UiBinding {
                field: prop_str(n, "field").unwrap_or_default(),
                method: prop_str(n, "method").unwrap_or_default(),
            })
            .collect();
        let navigation = children(g, &s.name, edge::HAS_NAVIGATION)
            .into_iter()
            .map(|n| UiNavigation {
                action_id: prop_str(n, "action_id").unwrap_or_default(),
                to: prop_str(n, "to").unwrap_or_default(),
            })
            .collect();
        let layout = match children(g, &s.name, edge::HAS_LAYOUT).first() {
            Some(root) => rebuild_layout(g, &root.name)?,
            None => LayoutNode::Empty,
        };
        out.ui.push(Screen {
            id: s.name.clone(),
            title: prop_str(s, "title").unwrap_or_default(),
            layout,
            actions,
            bindings,
            navigation,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsystems::project::spec_gen::example_gis_spec;

    fn order_ids(g: &SpecGraph, node_type: &str) -> Vec<String> {
        let mut v: Vec<&SpecNode> = g
            .nodes
            .iter()
            .filter(|n| n.node_type == node_type)
            .collect();
        v.sort_by_key(|n| order_of(n));
        v.into_iter().map(|n| n.name.clone()).collect()
    }

    #[test]
    fn ty_string_roundtrips_every_kind() {
        let cases = [
            Type::Str,
            Type::Int,
            Type::Float,
            Type::Bool,
            Type::Named {
                name: "LayerInfo".to_string(),
            },
            Type::List {
                of: Box::new(Type::Named {
                    name: "LayerInfo".to_string(),
                }),
            },
            Type::Option {
                of: Box::new(Type::Int),
            },
            Type::Record {
                fields: vec![
                    Field {
                        name: "id".to_string(),
                        ty: Type::Str,
                    },
                    Field {
                        name: "ok".to_string(),
                        ty: Type::Bool,
                    },
                ],
            },
            Type::List {
                of: Box::new(Type::Record {
                    fields: vec![Field {
                        name: "v".to_string(),
                        ty: Type::Float,
                    }],
                }),
            },
        ];
        for ty in cases {
            let s = ty_string(&ty);
            let back = type_from_string(&s).unwrap_or_else(|e| panic!("{s}: {e}"));
            assert_eq!(back, ty, "string '{s}' must round-trip");
        }
    }

    #[test]
    fn gis_spec_decomposes_into_a_hypergraph() {
        let g = decompose(&example_gis_spec());
        assert_eq!(order_ids(&g, node::TYPE), vec!["LayerInfo"]);
        assert!(g
            .nodes
            .iter()
            .any(|n| n.node_type == node::ACTOR && n.name == "MapActor"));
        assert!(g
            .nodes
            .iter()
            .any(|n| n.node_type == node::METHOD && n.name == "map/listLayers"));
        assert!(g
            .nodes
            .iter()
            .any(|n| n.node_type == node::SCREEN && n.name == "map"));
        assert!(g.edges.iter().any(|e| e.predicate == edge::CALLS));
        assert!(g
            .edges
            .iter()
            .any(|e| e.predicate == edge::HANDLED_BY && e.to_name == "MapActor"));
        assert!(
            g.nodes
                .iter()
                .filter(|n| n.node_type == node::LAYOUT)
                .count()
                >= 4
        );
    }

    #[test]
    fn gis_spec_roundtrips_through_the_graph() {
        let spec = example_gis_spec();
        let back = reconstruct(&decompose(&spec)).expect("reconstruct");
        assert_eq!(back, spec);
        assert!(back.is_valid());
    }

    #[test]
    fn reconstruct_errors_on_missing_root() {
        let err = reconstruct(&SpecGraph::default()).unwrap_err();
        assert!(err.contains("no appspec node"));
    }

    /// The full SeleneDB-derived SpireGis reference spec round-trips through
    /// the graph exactly (parsed from docs/spire-gis.appspec.json).
    #[test]
    fn full_gis_reference_spec_roundtrips_through_the_graph() {
        let raw = include_str!("../../../../../docs/spire-gis.appspec.json");
        let spec: AppSpec = serde_json::from_str(raw).expect("reference spec parses");
        assert!(spec.is_valid());
        let back = reconstruct(&decompose(&spec)).expect("reconstruct reference spec");
        assert_eq!(back.app, spec.app);
        assert_eq!(back.types, spec.types, "types mismatch");
        assert_eq!(back.graph, spec.graph, "graph mismatch");
        assert_eq!(back.actors, spec.actors, "actors mismatch");
        assert_eq!(back.bridge, spec.bridge, "bridge mismatch");
        assert_eq!(back.ui, spec.ui, "ui mismatch");
    }
}
