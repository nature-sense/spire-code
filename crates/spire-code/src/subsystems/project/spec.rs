// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Formal **AppSpec** — a machine-checkable specification of a SpireApp
//! (SwiftUI + Rust actors), with the **bridge contract** as the single source
//! of truth.
//!
//! Structure:
//! ```text
//! AppSpec { app, types, actors, bridge, ui }
//! ├── app     — project name + goal
//! ├── types   — shared vocabulary: named records/enums both sides reference
//! ├── actors  — the Rust side: each actor lists the bridge methods it handles
//! │             (routing is DERIVED — there is no separate route table)
//! ├── bridge  — the JSON method contract: method + params + result types
//! └── ui      — SwiftUI screens whose actions bind to bridge methods
//! ```
//!
//! [`validate`] enforces the invariants that keep the three parts coherent:
//! unique names, every bridge method handled by exactly one actor, every UI
//! action pointing at a real method, and every type reference resolving. The
//! requirements LLM pass emits this JSON; validation gates it before any code
//! is planned.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Top-level specification for a SpireApp.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AppSpec {
    pub app: AppMeta,
    /// Shared vocabulary — named types the bridge, actors and UI reference.
    #[serde(default)]
    pub types: Vec<DomainType>,
    /// Backend actors (the Rust side). Each lists the bridge methods it handles.
    #[serde(default)]
    pub actors: Vec<ActorSpec>,
    /// The JSON bridge contract (method + params + result).
    #[serde(default)]
    pub bridge: Vec<BridgeMethod>,
    /// SwiftUI screens and their actions.
    #[serde(default)]
    pub ui: Vec<Screen>,
}

/// Project-level metadata.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AppMeta {
    /// Project / crate name (convention: `spire-<name>`).
    pub name: String,
    /// High-level goal the app implements.
    #[serde(default)]
    pub goal: String,
}

/// A named data type shared across the spec (referenced by `Type::Named`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainType {
    /// A named struct: `{ kind: "record", name, fields }`.
    Record { name: String, fields: Vec<Field> },
    /// A named enum: `{ kind: "enum", name, variants }`.
    Enum { name: String, variants: Vec<String> },
}

impl DomainType {
    /// The type's name regardless of variant.
    pub fn name(&self) -> &str {
        match self {
            DomainType::Record { name, .. } | DomainType::Enum { name, .. } => name,
        }
    }
}

/// A single named field (used in records, params, actor state).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

/// The type algebra. Strict by design — there is no permissive `json` escape
/// hatch, so every value a bridge method returns or an actor holds must be
/// fully described by primitives, named domain types, lists, options or inline
/// records. This is what makes the contract machine-checkable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Type {
    Str,
    Int,
    Float,
    Bool,
    /// A homogeneous list: `{ kind: "list", of: <type> }`.
    List {
        of: Box<Type>,
    },
    /// An optional value: `{ kind: "option", of: <type> }`.
    Option {
        of: Box<Type>,
    },
    /// Reference to a `DomainType` in `AppSpec::types`: `{ kind: "named", name }`.
    Named {
        name: String,
    },
    /// Inline anonymous object: `{ kind: "record", fields }`.
    Record {
        fields: Vec<Field>,
    },
}

/// A backend actor (the Rust side). All functionality lives in actors.
///
/// Routing is **derived**: each entry of `handlers` is a bridge method name
/// this actor implements, so a bridge method is served by exactly the actor
/// that lists it (enforced by [`validate`]).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ActorSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Bridge method names this actor handles (must each exist in `bridge`).
    #[serde(default)]
    pub handlers: Vec<String>,
    /// Backend-internal state fields (typed against `types` + primitives).
    #[serde(default)]
    pub state: Vec<Field>,
    /// spire-core subsystems/crates it leans on
    /// (e.g. "embedder", "rag", "llm", "build_types", "config").
    #[serde(default)]
    pub uses: Vec<String>,
}

/// A JSON bridge method — the contract at the intersection of UI and backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BridgeMethod {
    /// JSON method name, e.g. `"map/listLayers"`.
    pub method: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: Vec<Field>,
    pub result: Type,
}

/// A SwiftUI screen and the user actions it exposes.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Screen {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub actions: Vec<UiAction>,
}

/// A single user action that invokes a bridge method.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UiAction {
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// Bridge method this action calls (must exist in `bridge`).
    pub bridge: String,
}

/// A single validation finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpecIssue {
    pub severity: SpecIssueSeverity,
    /// Where the problem lives (e.g. `bridge[map/listLayers].result`).
    pub path: String,
    pub message: String,
}

/// Severity of a [`SpecIssue`]. Errors block generation; warnings don't.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecIssueSeverity {
    Error,
    Warning,
}

fn error(path: impl Into<String>, message: impl Into<String>) -> SpecIssue {
    SpecIssue {
        severity: SpecIssueSeverity::Error,
        path: path.into(),
        message: message.into(),
    }
}

fn warn(path: impl Into<String>, message: impl Into<String>) -> SpecIssue {
    SpecIssue {
        severity: SpecIssueSeverity::Warning,
        path: path.into(),
        message: message.into(),
    }
}

impl AppSpec {
    /// True when the spec has no error-severity issues (warnings allowed).
    pub fn is_valid(&self) -> bool {
        !validate(self)
            .iter()
            .any(|i| i.severity == SpecIssueSeverity::Error)
    }
}

/// Validate a spec against the formal invariants.
///
/// Returns every issue (errors AND warnings). Callers gate generation on the
/// absence of [`SpecIssueSeverity::Error`] issues — see [`AppSpec::is_valid`].
///
/// Invariants:
/// - `app.name` is non-empty.
/// - No duplicate domain-type, actor, bridge-method, screen or action names.
/// - Every bridge method is handled by **exactly one** actor (`handlers`).
/// - Every `UiAction.bridge` resolves to a defined bridge method.
/// - Every `Type::Named` resolves to a defined domain type; `list`/`option`/
///   record payloads are well-formed and fields are unique per record.
/// - Warnings (non-blocking): actor with no handlers, bridge method no UI
///   action calls, domain type nothing references.
pub fn validate(spec: &AppSpec) -> Vec<SpecIssue> {
    let mut issues: Vec<SpecIssue> = Vec::new();

    // ── app meta ──────────────────────────────────────────────────────────
    if spec.app.name.trim().is_empty() {
        issues.push(error("app.name", "app name is required"));
    }

    // ── uniqueness of named things ────────────────────────────────────────
    let type_names: HashSet<&str> = spec.types.iter().map(DomainType::name).collect();

    let mut seen_type: HashSet<&str> = HashSet::new();
    for t in &spec.types {
        if !seen_type.insert(t.name()) {
            issues.push(error(
                format!("types[{}]", t.name()),
                "duplicate domain type name",
            ));
        }
    }

    let mut seen_actor: HashSet<&str> = HashSet::new();
    for a in &spec.actors {
        if a.name.trim().is_empty() {
            issues.push(error("actors[]", "actor name is required"));
        } else if !seen_actor.insert(a.name.as_str()) {
            issues.push(error(format!("actors[{}]", a.name), "duplicate actor name"));
        }
    }

    let mut seen_method: HashSet<&str> = HashSet::new();
    for m in &spec.bridge {
        if m.method.trim().is_empty() {
            issues.push(error("bridge[]", "bridge method name is required"));
        } else if !seen_method.insert(m.method.as_str()) {
            issues.push(error(
                format!("bridge[{}]", m.method),
                "duplicate bridge method",
            ));
        }
    }

    let mut seen_screen: HashSet<&str> = HashSet::new();
    for s in &spec.ui {
        if s.id.trim().is_empty() {
            issues.push(error("ui[]", "screen id is required"));
        } else if !seen_screen.insert(s.id.as_str()) {
            issues.push(error(format!("ui[{}]", s.id), "duplicate screen id"));
        }
    }

    // ── bridge is total: every method has exactly one actor handler ───────
    let method_set: HashSet<&str> = spec.bridge.iter().map(|m| m.method.as_str()).collect();
    let mut handler_counts: HashMap<&str, usize> = HashMap::new();
    for a in &spec.actors {
        let mut seen_handler: HashSet<&str> = HashSet::new();
        for h in &a.handlers {
            if !method_set.contains(h.as_str()) {
                issues.push(error(
                    format!("actors[{}.handlers]", a.name),
                    format!("handler '{h}' is not a defined bridge method"),
                ));
            }
            if !seen_handler.insert(h.as_str()) {
                issues.push(error(
                    format!("actors[{}.handlers]", a.name),
                    format!("actor lists handler '{h}' more than once"),
                ));
            }
            *handler_counts.entry(h.as_str()).or_insert(0) += 1;
        }
    }
    for m in &spec.bridge {
        match handler_counts.get(m.method.as_str()).copied().unwrap_or(0) {
            0 => issues.push(error(
                format!("bridge[{}]", m.method),
                "bridge method is handled by no actor",
            )),
            n if n > 1 => issues.push(error(
                format!("bridge[{}]", m.method),
                format!("bridge method is handled by {n} actors (must be exactly one)"),
            )),
            _ => {}
        }
    }

    // ── UI actions reference real bridge methods ──────────────────────────
    let mut action_methods: HashSet<&str> = HashSet::new();
    for s in &spec.ui {
        let mut seen_action: HashSet<&str> = HashSet::new();
        for act in &s.actions {
            if !seen_action.insert(act.id.as_str()) {
                issues.push(error(
                    format!("ui[{}.actions]", s.id),
                    format!("duplicate action '{}'", act.id),
                ));
            }
            if !method_set.contains(act.bridge.as_str()) {
                issues.push(error(
                    format!("ui[{}.actions[{}]", s.id, act.id),
                    format!("action references undefined bridge method '{}'", act.bridge),
                ));
            }
            action_methods.insert(act.bridge.as_str());
        }
    }

    // ── types resolve + are well-formed ───────────────────────────────────
    for m in &spec.bridge {
        for p in &m.params {
            check_type(
                &format!("bridge[{}].params.{}", m.method, p.name),
                &p.ty,
                &type_names,
                &mut issues,
            );
        }
        check_type(
            &format!("bridge[{}].result", m.method),
            &m.result,
            &type_names,
            &mut issues,
        );
    }
    for a in &spec.actors {
        for f in &a.state {
            check_type(
                &format!("actors[{}].state.{}", a.name, f.name),
                &f.ty,
                &type_names,
                &mut issues,
            );
        }
    }
    for t in &spec.types {
        match t {
            DomainType::Record { name, fields } => {
                let mut seen_field: HashSet<&str> = HashSet::new();
                for f in fields {
                    if !seen_field.insert(f.name.as_str()) {
                        issues.push(error(
                            format!("types[{name}]"),
                            format!("duplicate field '{}'", f.name),
                        ));
                    }
                    check_type(
                        &format!("types[{name}].{}", f.name),
                        &f.ty,
                        &type_names,
                        &mut issues,
                    );
                }
            }
            DomainType::Enum { name, variants } => {
                let mut seen_variant: HashSet<&str> = HashSet::new();
                for v in variants {
                    if !seen_variant.insert(v.as_str()) {
                        issues.push(error(
                            format!("types[{name}]"),
                            format!("duplicate variant '{v}'"),
                        ));
                    }
                }
            }
        }
    }

    // ── warnings ──────────────────────────────────────────────────────────
    for a in &spec.actors {
        if a.handlers.is_empty() {
            issues.push(warn(
                format!("actors[{}]", a.name),
                "actor handles no bridge methods (dead actor)",
            ));
        }
    }
    for m in &spec.bridge {
        if !action_methods.contains(m.method.as_str()) {
            issues.push(warn(
                format!("bridge[{}]", m.method),
                "bridge method is referenced by no UI action",
            ));
        }
    }
    let used_types: HashSet<String> = {
        let mut used = HashSet::new();
        collect_named_types_in_spec(spec, &mut used);
        used
    };
    for t in &spec.types {
        if !used_types.contains(t.name()) {
            issues.push(warn(
                format!("types[{}]", t.name()),
                "domain type is referenced by nothing (unused)",
            ));
        }
    }

    issues
}

/// Recursively verify a [`Type`] against the defined domain types.
fn check_type(path: &str, ty: &Type, type_names: &HashSet<&str>, issues: &mut Vec<SpecIssue>) {
    match ty {
        Type::Str | Type::Int | Type::Float | Type::Bool => {}
        Type::List { of } => check_type(&format!("{path}.of"), of, type_names, issues),
        Type::Option { of } => check_type(&format!("{path}.of"), of, type_names, issues),
        Type::Named { name } => {
            if !type_names.contains(name.as_str()) {
                issues.push(error(
                    path.to_string(),
                    format!("unresolved type reference '{name}'"),
                ));
            }
        }
        Type::Record { fields } => {
            for f in fields {
                check_type(&format!("{path}.{}", f.name), &f.ty, type_names, issues);
            }
        }
    }
}

/// Collect every named type used anywhere in the spec (for the unused-type
/// warning).
fn collect_named_types_in_spec(spec: &AppSpec, out: &mut HashSet<String>) {
    for m in &spec.bridge {
        for p in &m.params {
            collect_named_types(&p.ty, out);
        }
        collect_named_types(&m.result, out);
    }
    for a in &spec.actors {
        for f in &a.state {
            collect_named_types(&f.ty, out);
        }
    }
    for t in &spec.types {
        if let DomainType::Record { fields, .. } = t {
            for f in fields {
                collect_named_types(&f.ty, out);
            }
        }
    }
}

fn collect_named_types(ty: &Type, out: &mut HashSet<String>) {
    match ty {
        Type::Named { name } => {
            out.insert(name.clone());
        }
        Type::List { of } | Type::Option { of } => collect_named_types(of, out),
        Type::Record { fields } => {
            for f in fields {
                collect_named_types(&f.ty, out);
            }
        }
        Type::Str | Type::Int | Type::Float | Type::Bool => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, ty: Type) -> Field {
        Field {
            name: name.to_string(),
            ty,
        }
    }

    fn str_type() -> Type {
        Type::Str
    }
    fn bool_type() -> Type {
        Type::Bool
    }
    fn named(name: &str) -> Type {
        Type::Named {
            name: name.to_string(),
        }
    }

    /// The canonical GIS spec from the design doc — fully valid.
    fn gis_spec() -> AppSpec {
        AppSpec {
            app: AppMeta {
                name: "spire-gis".to_string(),
                goal: "view and edit map layers".to_string(),
            },
            types: vec![DomainType::Record {
                name: "LayerInfo".to_string(),
                fields: vec![field("id", str_type()), field("visible", bool_type())],
            }],
            actors: vec![ActorSpec {
                name: "MapActor".to_string(),
                description: "holds layer state".to_string(),
                handlers: vec!["map/listLayers".to_string(), "map/addLayer".to_string()],
                state: vec![field(
                    "layers",
                    Type::List {
                        of: Box::new(named("LayerInfo")),
                    },
                )],
                uses: vec!["build_types".to_string()],
            }],
            bridge: vec![
                BridgeMethod {
                    method: "map/listLayers".to_string(),
                    description: String::new(),
                    params: vec![],
                    result: Type::List {
                        of: Box::new(named("LayerInfo")),
                    },
                },
                BridgeMethod {
                    method: "map/addLayer".to_string(),
                    description: String::new(),
                    params: vec![field("id", str_type())],
                    result: Type::Record {
                        fields: vec![field("ok", bool_type())],
                    },
                },
            ],
            ui: vec![Screen {
                id: "map".to_string(),
                title: "Map".to_string(),
                actions: vec![
                    UiAction {
                        id: "load".to_string(),
                        description: String::new(),
                        bridge: "map/listLayers".to_string(),
                    },
                    UiAction {
                        id: "add".to_string(),
                        description: String::new(),
                        bridge: "map/addLayer".to_string(),
                    },
                ],
            }],
        }
    }

    fn errors(issues: &[SpecIssue]) -> Vec<&SpecIssue> {
        issues
            .iter()
            .filter(|i| i.severity == SpecIssueSeverity::Error)
            .collect()
    }

    #[test]
    fn valid_gis_spec_passes() {
        let issues = validate(&gis_spec());
        assert!(errors(&issues).is_empty(), "unexpected errors: {issues:?}");
    }

    #[test]
    fn valid_gis_spec_roundtrips_through_json() {
        let spec = gis_spec();
        let json = serde_json::to_value(&spec).unwrap();
        let back: AppSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn duplicate_bridge_method_is_an_error() {
        let mut spec = gis_spec();
        spec.bridge.push(spec.bridge[0].clone());
        let issues = validate(&spec);
        assert!(
            errors(&issues)
                .iter()
                .any(|i| i.message.contains("duplicate bridge method")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn unhandled_bridge_method_is_an_error() {
        let mut spec = gis_spec();
        spec.bridge.push(BridgeMethod {
            method: "map/removeLayer".to_string(),
            description: String::new(),
            params: vec![],
            result: bool_type(),
        });
        let issues = validate(&spec);
        assert!(
            errors(&issues)
                .iter()
                .any(|i| i.message.contains("handled by no actor")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn two_actors_claiming_same_method_is_an_error() {
        let mut spec = gis_spec();
        spec.actors.push(ActorSpec {
            name: "MapActor2".to_string(),
            description: String::new(),
            handlers: vec!["map/listLayers".to_string()],
            state: vec![],
            uses: vec![],
        });
        let issues = validate(&spec);
        assert!(
            errors(&issues)
                .iter()
                .any(|i| i.message.contains("handled by 2 actors")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn dangling_ui_action_is_an_error() {
        let mut spec = gis_spec();
        spec.ui[0].actions.push(UiAction {
            id: "boom".to_string(),
            description: String::new(),
            bridge: "map/doesNotExist".to_string(),
        });
        let issues = validate(&spec);
        assert!(
            errors(&issues)
                .iter()
                .any(|i| i.message.contains("undefined bridge method")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn unresolved_named_type_is_an_error() {
        let mut spec = gis_spec();
        // Refer to a type that was never defined.
        spec.bridge[0].result = Type::List {
            of: Box::new(named("MissingType")),
        };
        let issues = validate(&spec);
        assert!(
            errors(&issues).iter().any(|i| i
                .message
                .contains("unresolved type reference 'MissingType'")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn handlerless_actor_is_a_warning_not_error() {
        let mut spec = gis_spec();
        spec.actors.push(ActorSpec {
            name: "IdleActor".to_string(),
            description: String::new(),
            handlers: vec![],
            state: vec![],
            uses: vec![],
        });
        let issues = validate(&spec);
        assert!(errors(&issues).is_empty(), "unexpected errors: {issues:?}");
        assert!(
            issues
                .iter()
                .any(|i| i.severity == SpecIssueSeverity::Warning
                    && i.message.contains("dead actor")),
            "expected dead-actor warning: {issues:?}"
        );
        // A warning-only spec is still considered valid for generation.
        assert!(spec.is_valid());
    }

    #[test]
    fn missing_type_def_is_an_error() {
        let mut spec = gis_spec();
        spec.bridge[1].result = named("LayerInfo");
        // Remove the LayerInfo definition entirely.
        spec.types.clear();
        let issues = validate(&spec);
        assert!(
            errors(&issues)
                .iter()
                .any(|i| i.message.contains("unresolved type reference 'LayerInfo'")),
            "issues: {issues:?}"
        );
    }
}
