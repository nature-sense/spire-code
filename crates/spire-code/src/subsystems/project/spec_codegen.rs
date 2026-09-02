// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **AppSpec codegen** — deterministic skeleton generation from a VALIDATED
//! [`AppSpec`], read piece-wise straight off its graph-native form
//! ([`spec_graph::decompose`]). Nothing is invented on either side:
//!
//! - `types.rs`      walks `spec_type` nodes (fields/variants via `HAS_FIELD`/
//!   `HAS_VARIANT`).
//! - `actors.rs`     walks `spec_actor` nodes and their `HAS_HANDLER` children.
//! - `lib.rs`        dispatch arms are derived per actor from `HANDLED_BY`
//!   routing edges — never a separate route table.
//! - `AppBridge.swift` walks `spec_method` nodes (the bridge contract).
//! - `Screens.swift` walks `spec_screen` nodes + `HAS_ACTION` children and the
//!   screen's layout subgraph.
//!
//! Output is a flat list of [`GeneratedFile`]s written under the SpireApp
//! fill roots (`crates/<crate>/src`, `ui/swift/Sources`) — i.e. normal
//! `write_source_file` steps the existing fill pipeline can execute. Bodies
//! carry `TODO` markers: the next stage's LLM fill writes the real logic
//! inside the generated, validated contract.

use super::spec::{
    ActorSpec, AppSpec, BridgeMethod, DomainType, LayoutNode, Screen, Type, UiAction, UiBinding,
    UiNavigation,
};
use super::spec_graph::{self, edge, node, SpecGraph};

/// One generated file: fill-root-relative path + full content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
}

/// Normalize a project name to a valid crate id (same rule as the scaffold):
/// lowercase, spaces → hyphens.
pub fn crate_id(project_name: &str) -> String {
    project_name.trim().to_lowercase().replace(' ', "-")
}

/// snake_case → PascalCase (Rust structs / Swift struct names).
fn pascal(s: &str) -> String {
    s.split(['_', '-', '/', ' '])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// The final segment of a bridge method (`map/listLayers` → `listLayers`)
/// camelCased, for Swift wrapper function names.
fn swift_method_id(method: &str) -> String {
    let last = method.rsplit('/').next().unwrap_or(method);
    let mut out = String::new();
    for (i, part) in last.split(['_', '-']).enumerate() {
        if i == 0 {
            out.push_str(part);
        } else {
            out.push_str(&pascal(part));
        }
    }
    out
}

/// Rust type text for a spec [`Type`] (used in serde struct fields).
fn rust_type(ty: &Type) -> String {
    match ty {
        Type::Str => "String".to_string(),
        Type::Int => "i64".to_string(),
        Type::Float => "f64".to_string(),
        Type::Bool => "bool".to_string(),
        Type::List { of } => format!("Vec<{}>", rust_type(of)),
        Type::Option { of } => format!("Option<{}>", rust_type(of)),
        Type::Named { name } => name.clone(),
        // Skeleton-level placeholder for inline records; the LLM fill stage
        // replaces it with a real named type when it writes the method body.
        Type::Record { .. } => {
            "serde_json::Value // TODO(codegen): inline record — hoist to a named type".to_string()
        }
    }
}

// ── Graph readers: piece-wise reads for the generators ────────────────────
// Each generator below consumes ONLY its slice of the spec graph (as the
// actors will persist it in Stage 4), never the serialized AppSpec. Because
// decompose is order-preserving, graph reads reproduce the spec's order.

/// `spec_type` nodes → the domain types they describe.
fn read_graph_types(g: &SpecGraph) -> Vec<DomainType> {
    let mut out = Vec::new();
    for n in g.nodes.iter().filter(|n| n.node_type == node::TYPE) {
        let name = n.name.clone();
        if spec_graph::prop_str(n, "kind").as_deref() == Some("record") {
            let fields = spec_graph::fields_of(g, &name)
                .expect("record fields present on a decomposed graph");
            out.push(DomainType::Record { name, fields });
        } else {
            let variants = spec_graph::children(g, &name, edge::HAS_VARIANT)
                .into_iter()
                .map(|v| spec_graph::prop_str(v, "variant").unwrap_or_default())
                .collect();
            out.push(DomainType::Enum { name, variants });
        }
    }
    out
}

/// `spec_actor` nodes → actor skeletons (handlers from their `HAS_HANDLER`
/// children; state/uses are not part of the skeleton output).
fn read_graph_actors(g: &SpecGraph) -> Vec<ActorSpec> {
    g.nodes
        .iter()
        .filter(|n| n.node_type == node::ACTOR)
        .map(|n| {
            let handlers = spec_graph::children(g, &n.name, edge::HAS_HANDLER)
                .into_iter()
                .map(|h| spec_graph::prop_str(h, "method").unwrap_or_default())
                .collect();
            ActorSpec {
                name: n.name.clone(),
                description: n.description.clone().unwrap_or_default(),
                handlers,
                state: Vec::new(),
                uses: Vec::new(),
            }
        })
        .collect()
}

/// The dispatch routing, derived from `HANDLED_BY`: every `spec_method` node
/// lists the actor that owns it. Grouped per actor, methods in graph order.
fn read_graph_dispatch(g: &SpecGraph) -> Vec<(String, Vec<String>)> {
    let owned = |actor: &str| -> Vec<String> {
        g.nodes
            .iter()
            .filter(|n| n.node_type == node::METHOD)
            .filter(|m| {
                g.edges.iter().any(|e| {
                    e.predicate == edge::HANDLED_BY && e.from_name == m.name && e.to_name == actor
                })
            })
            .map(|m| m.name.clone())
            .collect()
    };
    g.nodes
        .iter()
        .filter(|n| n.node_type == node::ACTOR)
        .map(|n| (n.name.clone(), owned(&n.name)))
        .filter(|(_, methods)| !methods.is_empty())
        .collect()
}

/// `spec_method` nodes → the bridge contract (name, description, params,
/// result). Result/param types are re-parsed from the graph's canonical
/// type-expression properties.
fn read_graph_bridge(g: &SpecGraph) -> Vec<BridgeMethod> {
    g.nodes
        .iter()
        .filter(|n| n.node_type == node::METHOD)
        .map(|n| {
            let ty = spec_graph::prop_str(n, "result")
                .expect("method nodes always carry a 'result' on a decomposed graph");
            BridgeMethod {
                method: n.name.clone(),
                description: n.description.clone().unwrap_or_default(),
                params: spec_graph::fields_of(g, &n.name)
                    .expect("method params present on a decomposed graph"),
                result: spec_graph::type_from_string(&ty)
                    .expect("decomposed 'result' is always a valid type expression"),
            }
        })
        .collect()
}

/// `spec_screen` nodes → screens (id/title), their `HAS_ACTION` actions, and
/// the layout tree hanging off the screen via its `HAS_LAYOUT` root.
fn read_graph_screens(g: &SpecGraph) -> Result<Vec<Screen>, String> {
    let mut ui = Vec::new();
    for n in g.nodes.iter().filter(|n| n.node_type == node::SCREEN) {
        let id = n.name.clone();
        let layout_root = g
            .edges
            .iter()
            .find(|e| e.predicate == edge::HAS_LAYOUT && e.from_name == id)
            .map(|e| e.to_name.clone())
            .ok_or_else(|| format!("screen '{id}' has no HAS_LAYOUT root"))?;
        let layout = spec_graph::rebuild_layout(g, &layout_root)?;
        let actions = spec_graph::children(g, &id, edge::HAS_ACTION)
            .into_iter()
            .map(|a| UiAction {
                id: spec_graph::prop_str(a, "id").unwrap_or_default(),
                description: a.description.clone().unwrap_or_default(),
                bridge: spec_graph::prop_str(a, "bridge").unwrap_or_default(),
            })
            .collect();
        let bindings = spec_graph::children(g, &id, edge::HAS_BINDING)
            .into_iter()
            .map(|b| UiBinding {
                field: spec_graph::prop_str(b, "field").unwrap_or_default(),
                method: spec_graph::prop_str(b, "method").unwrap_or_default(),
            })
            .collect();
        let navigation = spec_graph::children(g, &id, edge::HAS_NAVIGATION)
            .into_iter()
            .map(|x| UiNavigation {
                action_id: spec_graph::prop_str(x, "action_id").unwrap_or_default(),
                to: spec_graph::prop_str(x, "to").unwrap_or_default(),
            })
            .collect();
        ui.push(Screen {
            id,
            title: spec_graph::prop_str(n, "title").unwrap_or_default(),
            layout,
            actions,
            bindings,
            navigation,
        });
    }
    Ok(ui)
}

// ── Rust: domain types (types.rs) ────────────────────────────────────────

fn render_types_rs(g: &SpecGraph) -> String {
    let types = read_graph_types(g);
    let mut out = String::from(
        "// Code-generated by spec_codegen.rs — bridge-derived domain types.\n\
         #![allow(dead_code)]\n\
         use serde::{Deserialize, Serialize};\n\n",
    );
    for t in &types {
        match t {
            DomainType::Record { name, fields } => {
                out.push_str(&format!(
                    "#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]\npub struct {name} {{\n"
                ));
                for f in fields {
                    out.push_str(&format!("    pub {}: {},\n", f.name, rust_type(&f.ty)));
                }
                out.push_str("}\n\n");
            }
            DomainType::Enum { name, variants } => {
                out.push_str(&format!(
                    "#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]\npub enum {name} {{\n"
                ));
                for v in variants {
                    out.push_str(&format!("    {},\n", pascal(v)));
                }
                out.push_str("}\n\n");
            }
        }
    }
    out
}

// ── Rust: actor skeletons (actors.rs) ────────────────────────────────────

fn render_actors_rs(g: &SpecGraph) -> String {
    let actors = read_graph_actors(g);
    let mut out = String::from(
        "// Code-generated by spec_codegen.rs — one skeleton per AppSpec actor.\n\
         // Routing is derived: each actor lists the bridge methods it handles.\n\
         #![allow(dead_code)]\n\
         use serde_json::Value;\n\n",
    );
    for a in &actors {
        let handlers = a.handlers.join("\", \"");
        out.push_str(&format!(
            "pub struct {};\n\nimpl {} {{\n    pub fn new() -> Self {{\n        Self\n    }}\n\n    /// Skeleton handler for the bridge methods this actor owns.\n    pub fn handle(&self, method: &str, params: &Value) -> String {{\n        let _ = (method, params);\n        // TODO({}): implement bodies for \"{}\".\n        serde_json::json!({{\"ok\": true, \"result\": null}}).to_string()\n    }}\n}}\n\n",
            pascal(&a.name),
            pascal(&a.name),
            a.name,
            handlers
        ));
    }
    out
}

/// The `match` arms of the FFI dispatch — every bridge method routed to the
/// actor whose `handlers` lists it (exactly-one-actor is validated upstream).
fn render_dispatch_arms(g: &SpecGraph) -> String {
    let routing = read_graph_dispatch(g);
    let mut out = String::new();
    for (a, methods) in &routing {
        let arms: Vec<String> = methods
            .iter()
            .map(|h| {
                format!(
                    "        \"{h}\" => actors::{}::new().handle(\"{h}\", &params),",
                    pascal(a)
                )
            })
            .collect();
        out.push_str(&arms.join("\n"));
        out.push('\n');
    }
    out.push_str(
        "        _ => serde_json::json!({\"ok\": false, \"error\": \"unknown method\"}).to_string(),\n",
    );
    out
}

fn render_lib_rs(g: &SpecGraph, crate_name: &str) -> String {
    let dispatch = render_dispatch_arms(g);
    format!(
        "//! {crate_name} — Rust core generated from a validated AppSpec.\n\
         //! FFI entry + dispatch are DERIVED from the AppSpec bridge contract\n\
         //! (routing by actor `handlers`); bodies are filled by the next stage.\n\
         #![allow(dead_code)]\n\
         \n\
         mod actors;\n\
         mod types;\n\
         \n\
         use std::ffi::{{CStr, CString}};\n\
         \n\
         /// Dispatch `{{\"method\": ..., \"params\": ...}}` to the owning actor.\n\
         fn dispatch(req: &str) -> String {{\n\
         \x20   let envelope: serde_json::Value = match serde_json::from_str(req) {{\n\
         \x20       Ok(v) => v,\n\
         \x20       Err(e) => {{\n\
         \x20           return serde_json::json!({{\"ok\": false, \"error\": format!(\"bad request: {{e}}\")}}).to_string();\n\
         \x20       }}\n\
         \x20   }};\n\
         \x20   let method = envelope.get(\"method\").and_then(|m| m.as_str()).unwrap_or(\"\");\n\
         \x20   let params = envelope.get(\"params\").cloned().unwrap_or(serde_json::json!({{}}));\n\
         \x20   match method {{\n\
         {dispatch}\
         \x20   }}\n\
         }}\n\
         \n\
         #[no_mangle]\n\
         pub extern \"C\" fn spire_send_json(request: *const std::os::raw::c_char) -> *mut std::os::raw::c_char {{\n\
         \x20   let req = unsafe {{ CStr::from_ptr(request) }}.to_string_lossy().to_string();\n\
         \x20   let reply = dispatch(&req);\n\
         \x20   CString::new(reply)\n\
         \x20       .map(|c| c.into_raw())\n\
         \x20       .unwrap_or(std::ptr::null_mut())\n\
         }}\n\
         \n\
         #[no_mangle]\n\
         pub extern \"C\" fn spire_free_string(p: *mut std::os::raw::c_char) {{\n\
         \x20   if !p.is_null() {{\n\
         \x20       unsafe {{ drop(CString::from_raw(p)) }};\n\
         \x20   }}\n\
         }}\n"
    )
}

// ── Swift: typed bridge wrappers (AppBridge.swift) ───────────────────────

fn render_app_bridge_swift(g: &SpecGraph) -> String {
    let bridge = read_graph_bridge(g);
    let mut out = String::from(
        "import Foundation\n\n\
         // Code-generated by spec_codegen.rs — one typed wrapper per AppSpec\n\
         // bridge method. All calls flow through CoreBridge.send (JSON FFI).\n\
         extension CoreBridge {\n",
    );
    for m in &bridge {
        let id = swift_method_id(&m.method);
        out.push_str(&format!(
            "    /// {method}: {desc}\n    func {id}(params: [String: Any] = [:]) -> String? {{\n\
             \x20       let request: [String: Any] = [\"method\": \"{method}\", \"params\": params]\n\
             \x20       guard let data = try? JSONSerialization.data(withJSONObject: request),\n\
             \x20             let json = String(data: data, encoding: .utf8) else {{ return nil }}\n\
             \x20       // TODO(fill): decode the result into the typed contract.\n\
             \x20       return send(json)\n    }}\n",
            method = m.method,
            desc = m.description.trim(),
            id = id
        ));
    }
    out.push_str("}\n");
    out
}

/// The label of the first layout button referencing `action`, if any.
fn layout_button_label(layout: &LayoutNode, action: &str) -> Option<String> {
    match layout {
        LayoutNode::Button { label, action: a } if a == action => Some(label.clone()),
        LayoutNode::VStack { children } | LayoutNode::HStack { children } => {
            children.iter().find_map(|c| layout_button_label(c, action))
        }
        LayoutNode::List { item } => layout_button_label(item, action),
        _ => None,
    }
}

// ── Swift: screen skeletons (Screens.swift) ──────────────────────────────

fn render_screens_swift(g: &SpecGraph) -> Result<String, String> {
    let ui = read_graph_screens(g)?;
    let mut out = String::from(
        "import SwiftUI\n\n\
         // Code-generated by spec_codegen.rs — one screen skeleton per AppSpec\n\
         // ui screen. Buttons mirror the layout sketch's actions; bodies are\n\
         // TODO hooks for the fill stage.\n",
    );
    for s in &ui {
        let struct_name = pascal(&s.id);
        out.push_str(&format!(
            "struct {struct_name}Screen: View {{\n    @Environment(CoreBridge.self) private var core\n\n    var body: some View {{\n",
        ));
        if s.actions.is_empty() {
            out.push_str("        // TODO(fill): no actions — layout sketch body\n");
        } else {
            out.push_str("        VStack {\n");
            for act in &s.actions {
                let wrapper = swift_method_id(&act.bridge);
                let label =
                    layout_button_label(&s.layout, &act.id).unwrap_or_else(|| act.id.clone());
                out.push_str(&format!(
                    "            // action \"{act_id}\" -> bridge method \"{bridge}\"\n\
                     \x20           Button(\"{label}\") {{ let _ = core.{wrapper}() }}\n",
                    act_id = act.id,
                    bridge = act.bridge,
                    label = label,
                    wrapper = wrapper
                ));
            }
            out.push_str("        }\n");
        }
        out.push_str("    }\n}\n\n");
    }
    Ok(out)
}

// ── Orchestration ────────────────────────────────────────────────────────

/// Deterministic skeleton files generated from a spec **graph**, written under
/// the SpireApp fill roots. Every generator reads only its slice of the graph
/// (types, actors/handlers, HANDLED_BY routing, methods, screens+layout) —
/// the same shape the actor layer will persist in Stage 4.
pub fn generated_files_from_graph(
    g: &SpecGraph,
    project_name: &str,
) -> Result<Vec<GeneratedFile>, String> {
    let crate_name = crate_id(project_name);
    Ok(vec![
        GeneratedFile {
            path: format!("crates/{crate_name}/src/types.rs"),
            content: render_types_rs(g),
        },
        GeneratedFile {
            path: format!("crates/{crate_name}/src/actors.rs"),
            content: render_actors_rs(g),
        },
        GeneratedFile {
            path: format!("crates/{crate_name}/src/lib.rs"),
            content: render_lib_rs(g, &crate_name),
        },
        GeneratedFile {
            path: "ui/swift/Sources/SpireUI/AppBridge.swift".to_string(),
            content: render_app_bridge_swift(g),
        },
        GeneratedFile {
            path: "ui/swift/Sources/SpireUI/Screens.swift".to_string(),
            content: render_screens_swift(g)?,
        },
    ])
}

/// Deterministic skeleton files for a validated spec, written under the
/// SpireApp fill roots. Convenience wrapper over the graph entry: the spec is
/// decomposed and every generator reads the graph piece-wise.
pub fn generated_files(spec: &AppSpec, project_name: &str) -> Vec<GeneratedFile> {
    generated_files_from_graph(&spec_graph::decompose(spec), project_name)
        .expect("a validated AppSpec always decomposes to a codegennable graph")
}

/// Map the generated skeleton files to `write_source_file` creation steps the
/// existing fill pipeline can execute.
pub fn codegen_steps(
    spec: &AppSpec,
    project_name: &str,
) -> Vec<super::project_creation::CreationStep> {
    use super::project_creation::{CreationStep, CreationStepType, StepStatus};
    generated_files(spec, project_name)
        .into_iter()
        .enumerate()
        .map(|(i, f)| CreationStep {
            id: format!("codegen-{}", i + 1),
            step_type: CreationStepType::WriteSourceFile,
            description: format!("Write {}", f.path),
            status: StepStatus::Pending,
            parameters: serde_json::json!({ "path": f.path, "content": f.content }),
            result: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsystems::project::spec_gen::example_gis_spec;

    fn files(spec: &AppSpec, project_name: &str) -> Vec<GeneratedFile> {
        generated_files(spec, project_name)
    }

    fn content<'a>(fs: &'a [GeneratedFile], path: &str) -> &'a str {
        &fs.iter().find(|f| f.path == path).expect(path).content
    }

    #[test]
    fn output_is_deterministic_and_paths_sit_under_fill_roots() {
        let spec = example_gis_spec();
        let a = files(&spec, "spire-gis");
        let b = files(&spec, "spire-gis");
        assert_eq!(a, b, "codegen must be deterministic");
        for f in &a {
            let under_fill_root = f.path.starts_with("crates/") && f.path.ends_with(".rs")
                || f.path.starts_with("ui/swift/Sources/");
            assert!(under_fill_root, "path not under fill roots: {}", f.path);
            assert!(!f.content.is_empty());
        }
    }

    #[test]
    fn types_file_renders_records_and_enums() {
        let spec = example_gis_spec();
        let fs = files(&spec, "spire-gis");
        let rs = content(&fs, "crates/spire-gis/src/types.rs");
        assert!(rs.contains("pub struct LayerInfo"));
        assert!(rs.contains("pub id: String"));
        assert!(rs.contains("pub visible: bool"));
    }

    #[test]
    fn actors_file_lists_handlers_per_actor() {
        let spec = example_gis_spec();
        let fs = files(&spec, "spire-gis");
        let rs = content(&fs, "crates/spire-gis/src/actors.rs");
        assert!(rs.contains("pub struct MapActor"));
        assert!(rs.contains("map/listLayers"));
        assert!(rs.contains("map/addLayer"));
    }

    #[test]
    fn lib_dispatch_covers_every_bridge_method_and_routes_to_the_owning_actor() {
        let spec = example_gis_spec();
        let fs = files(&spec, "spire-gis");
        let rs = content(&fs, "crates/spire-gis/src/lib.rs");
        for m in &spec.bridge {
            assert!(
                rs.contains(&format!("\"{}\" => actors::MapActor::new()", m.method)),
                "dispatch must route '{}' to MapActor\n{}",
                m.method,
                rs
            );
        }
        // No separate route list: each actor's handlers drive the match.
        assert!(rs.contains("match method"));
        assert!(rs.contains("unknown method"));
        assert!(rs.contains("spire_send_json"));
        assert!(rs.contains("spire_free_string"));
    }

    #[test]
    fn bridge_file_has_one_wrapper_per_method() {
        let spec = example_gis_spec();
        let fs = files(&spec, "spire-gis");
        let sw = content(&fs, "ui/swift/Sources/SpireUI/AppBridge.swift");
        assert!(sw.contains("extension CoreBridge"));
        assert!(sw.contains("func listLayers(params: [String: Any] = [:]) -> String?"));
        assert!(sw.contains("\"method\": \"map/listLayers\""));
        assert!(sw.contains("func addLayer(params: [String: Any] = [:]) -> String?"));
        assert!(sw.contains("\"method\": \"map/addLayer\""));
    }

    #[test]
    fn screens_file_renders_a_view_per_screen_with_layout_labels() {
        let spec = example_gis_spec();
        let fs = files(&spec, "spire-gis");
        let sw = content(&fs, "ui/swift/Sources/SpireUI/Screens.swift");
        assert!(sw.contains("struct MapScreen: View"));
        assert!(sw.contains("struct InspectorScreen: View"));
        // Layout sketch labels surface as the button labels.
        assert!(sw.contains("Button(\"Reload\") { let _ = core.listLayers() }"));
        assert!(sw.contains("Button(\"Add layer\") { let _ = core.addLayer() }"));
        // Every action is bound to its bridge method.
        assert!(sw.contains("action \"load\" -> bridge method \"map/listLayers\""));
        assert!(sw.contains("action \"add\" -> bridge method \"map/addLayer\""));
    }

    #[test]
    fn codegen_steps_are_executable_write_source_steps() {
        use super::super::project_creation::{CreationStepType, StepStatus};
        let spec = example_gis_spec();
        let fs = files(&spec, "spire-gis");
        let steps = codegen_steps(&spec, "spire-gis");
        assert_eq!(steps.len(), fs.len());
        for (i, step) in steps.iter().enumerate() {
            assert_eq!(step.id, format!("codegen-{}", i + 1));
            assert_eq!(step.step_type, CreationStepType::WriteSourceFile);
            assert_eq!(step.status, StepStatus::Pending);
            assert!(
                step.parameters.get("path").is_some() && step.parameters.get("content").is_some(),
                "step params must carry path + content"
            );
        }
    }

    #[test]
    fn layout_label_lookup_finds_nested_buttons() {
        let layout = LayoutNode::VStack {
            children: vec![
                LayoutNode::Text {
                    text: "heading".to_string(),
                },
                LayoutNode::List {
                    item: Box::new(LayoutNode::Button {
                        label: "Open".to_string(),
                        action: "open".to_string(),
                    }),
                },
            ],
        };
        assert_eq!(
            layout_button_label(&layout, "open").as_deref(),
            Some("Open")
        );
        assert_eq!(layout_button_label(&layout, "missing"), None);
    }

    #[test]
    fn graph_driven_codegen_matches_the_spec_driven_entry_on_the_reference() {
        let raw = include_str!("../../../../../docs/spire-gis.appspec.json");
        let spec: AppSpec = serde_json::from_str(raw).expect("reference spec parses");
        let g = spec_graph::decompose(&spec);
        let from_graph =
            generated_files_from_graph(&g, "spire-gis").expect("graph codegen succeeds");
        let from_spec = generated_files(&spec, "spire-gis");
        assert_eq!(
            from_spec, from_graph,
            "both entries must produce identical files"
        );
        assert_eq!(from_spec.len(), 5);
    }

    #[test]
    fn graph_driven_codegen_reads_the_reference_graph_piecewise() {
        let raw = include_str!("../../../../../docs/spire-gis.appspec.json");
        let spec: AppSpec = serde_json::from_str(raw).expect("reference spec parses");
        let g = spec_graph::decompose(&spec);
        let fs = generated_files_from_graph(&g, "spire-gis").expect("graph codegen succeeds");
        let types_rs = content(&fs, "crates/spire-gis/src/types.rs");
        // spec_type nodes → records/enums.
        assert!(types_rs.contains("pub struct LayerInfo"));
        assert!(types_rs.contains("pub struct IngestReport"));
        assert!(types_rs.contains("pub enum GeometryType"));
        // spec_actor + HAS_HANDLER nodes → skeletons.
        let actors_rs = content(&fs, "crates/spire-gis/src/actors.rs");
        assert!(actors_rs.contains("pub struct MapActor"));
        assert!(actors_rs.contains("pub struct QueryActor"));
        // HANDLED_BY routing → dispatch, and spec_method nodes → Swift wrappers.
        let lib_rs = content(&fs, "crates/spire-gis/src/lib.rs");
        for m in &spec.bridge {
            let owner = spec
                .actors
                .iter()
                .find(|a| a.handlers.iter().any(|h| h == &m.method))
                .expect("each method has an owning actor");
            let ty = owner.name.split(['_', '-']).collect::<Vec<_>>().join("");
            let actor_ty = ty[..1].to_uppercase() + &ty[1..];
            assert!(
                lib_rs.contains(&format!("\"{}\" => actors::{actor_ty}::new()", m.method)),
                "dispatch must route '{}' to {}
{}",
                m.method,
                owner.name,
                lib_rs
            );
        }
        // spec_screen nodes + layout/action subgraphs → screen skeletons.
        let screens_swift = content(&fs, "ui/swift/Sources/SpireUI/Screens.swift");
        assert!(screens_swift.contains("struct MapScreen: View"));
        assert!(screens_swift.contains("struct InspectorScreen: View"));
        assert!(screens_swift.contains("Button(\"Reload\")"));
        assert!(screens_swift.contains("struct SearchScreen: View"));
    }

    #[test]
    fn graph_readers_see_the_whole_reference_decomposition() {
        let raw = include_str!("../../../../../docs/spire-gis.appspec.json");
        let spec: AppSpec = serde_json::from_str(raw).expect("reference spec parses");
        let g = spec_graph::decompose(&spec);
        let types = read_graph_types(&g);
        assert_eq!(types.len(), spec.types.len());
        let actors = read_graph_actors(&g);
        assert_eq!(actors.len(), spec.actors.len());
        let dispatch = read_graph_dispatch(&g);
        let handled: usize = dispatch.iter().map(|(_, ms)| ms.len()).sum();
        assert_eq!(handled, spec.bridge.len());
        let bridge = read_graph_bridge(&g);
        assert_eq!(bridge.len(), spec.bridge.len());
        for m in &bridge {
            assert_eq!(
                m.result,
                spec.bridge
                    .iter()
                    .find(|b| b.method == m.method)
                    .unwrap()
                    .result
            );
        }
        let ui = read_graph_screens(&g).expect("screens read");
        assert_eq!(ui.len(), spec.ui.len());
        for s in &ui {
            assert_eq!(
                s.layout,
                spec.ui.iter().find(|x| x.id == s.id).unwrap().layout
            );
            assert_eq!(
                s.actions,
                spec.ui.iter().find(|x| x.id == s.id).unwrap().actions
            );
        }
    }
}
