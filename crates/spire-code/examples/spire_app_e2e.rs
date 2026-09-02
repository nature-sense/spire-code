// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Dev E2E harness: write the deterministic AppSpec-codegen skeleton files
//! into a REAL SpireApp monorepo so the generated Rust + Swift code can be
//! compiled by the real toolchains.
//!
//! Usage:
//!   cargo run -p spire-code --example spire_app_e2e [repo-root] [spec.json]
//!
//! Defaults to `~/naturesense/spire/spire-gis`. When `spec.json` is given it is
//! parsed (and must validate) as the AppSpec to generate from; otherwise the
//! canonical GIS spec is used. Only writes the five skeleton files under the
//! repo's fill roots (types.rs, actors.rs, lib.rs, AppBridge.swift,
//! Screens.swift) plus `<root>/.spire/appspec.json`.

use std::path::PathBuf;

use spire_code::subsystems::project::spec::{
    ActorSpec, AppMeta, AppSpec, BridgeMethod, DomainType, Field, GraphEdgeType, GraphNodeType,
    GraphSchema, LayoutNode, Screen, Type, UiAction, UiBinding, UiNavigation,
};

/// The canonical GIS spec (mirrors `spec_gen::example_gis_spec`, rebuilt from
/// the public schema types because that fixture is crate-private).
fn gis_spec() -> AppSpec {
    AppSpec {
        app: AppMeta {
            name: "spire-gis".to_string(),
            goal: "view and edit map layers".to_string(),
        },
        graph: GraphSchema {
            nodes: vec![GraphNodeType {
                name: "map_layer".to_string(),
                description: "a rendered map layer".to_string(),
                fields: vec![
                    Field {
                        name: "id".to_string(),
                        ty: Type::Str,
                    },
                    Field {
                        name: "visible".to_string(),
                        ty: Type::Bool,
                    },
                ],
            }],
            edges: vec![GraphEdgeType {
                name: "contains".to_string(),
                from: "map_layer".to_string(),
                to: "map_layer".to_string(),
                description: String::new(),
            }],
        },
        types: vec![DomainType::Record {
            name: "LayerInfo".to_string(),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    ty: Type::Str,
                },
                Field {
                    name: "visible".to_string(),
                    ty: Type::Bool,
                },
            ],
        }],
        actors: vec![ActorSpec {
            name: "MapActor".to_string(),
            description: "holds layer state".to_string(),
            handlers: vec!["map/listLayers".to_string(), "map/addLayer".to_string()],
            state: vec![Field {
                name: "layers".to_string(),
                ty: Type::List {
                    of: Box::new(Type::Named {
                        name: "LayerInfo".to_string(),
                    }),
                },
            }],
            uses: vec!["build_types".to_string()],
        }],
        bridge: vec![
            BridgeMethod {
                method: "map/listLayers".to_string(),
                description: String::new(),
                params: vec![],
                result: Type::List {
                    of: Box::new(Type::Named {
                        name: "LayerInfo".to_string(),
                    }),
                },
            },
            BridgeMethod {
                method: "map/addLayer".to_string(),
                description: String::new(),
                params: vec![Field {
                    name: "id".to_string(),
                    ty: Type::Str,
                }],
                result: Type::Record {
                    fields: vec![Field {
                        name: "ok".to_string(),
                        ty: Type::Bool,
                    }],
                },
            },
        ],
        ui: vec![
            Screen {
                id: "map".to_string(),
                title: "Map".to_string(),
                layout: LayoutNode::VStack {
                    children: vec![
                        LayoutNode::Button {
                            label: "Reload".to_string(),
                            action: "load".to_string(),
                        },
                        LayoutNode::Button {
                            label: "Add layer".to_string(),
                            action: "add".to_string(),
                        },
                    ],
                },
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
                bindings: vec![UiBinding {
                    field: "layers".to_string(),
                    method: "map/listLayers".to_string(),
                }],
                navigation: vec![UiNavigation {
                    action_id: "add".to_string(),
                    to: "inspector".to_string(),
                }],
            },
            Screen {
                id: "inspector".to_string(),
                title: "Inspector".to_string(),
                actions: vec![],
                ..Screen::default()
            },
        ],
    }
}
fn main() {
    let repo_root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").expect("HOME");
            PathBuf::from(format!("{home}/naturesense/spire/spire-gis"))
        });
    let project_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "spire-gis".to_string());

    // Optional external spec JSON (from a live createProject/GenerateSpec);
    // otherwise the canonical embedded GIS spec is used.
    let spec = match std::env::args().nth(2) {
        Some(path) => {
            let raw = std::fs::read_to_string(&path).expect("read spec.json");
            let spec: AppSpec = serde_json::from_str(&raw)
                .expect("spec.json must deserialize as an AppSpec");
            assert!(spec.is_valid(), "spec.json must validate");
            println!(
                "spire_app_e2e: loaded external spec from {path} ({} types, {} actors, {} methods, {} screens)",
                spec.types.len(),
                spec.actors.len(),
                spec.bridge.len(),
                spec.ui.len()
            );
            spec
        }
        None => {
            let spec = gis_spec();
            assert!(
                spec.is_valid(),
                "the GIS reference spec must validate before codegen"
            );
            spec
        }
    };

    let files =
        spire_code::subsystems::project::spec_codegen::generated_files(&spec, &project_name);
    println!(
        "spire_app_e2e: repo={} project={project_name} — {} skeleton files",
        repo_root.display(),
        files.len()
    );
    for f in &files {
        let target = repo_root.join(&f.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&target, &f.content).expect("write skeleton file");
        println!("  wrote {} ({} bytes)", f.path, f.content.len());
    }

    // Keep the validated spec next to the generated code for inspection.
    let dot_spire = repo_root.join(".spire");
    std::fs::create_dir_all(&dot_spire).ok();
    let spec_json = serde_json::to_string_pretty(&spec).expect("serialize spec");
    std::fs::write(dot_spire.join("appspec.json"), spec_json).ok();
    println!("  wrote .spire/appspec.json");
    println!("spire_app_e2e: done — run `cargo build` and `swift build` in the repo to validate.");
}

