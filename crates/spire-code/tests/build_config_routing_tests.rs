// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Regression tests for multi-config-per-directory analysis routing.
//!
//! A directory can legitimately hold SEVERAL build configs — the SpireApp
//! shape puts a Cargo workspace root (`Cargo.toml`) AND a `Makefile` in the
//! same directory, plus a SwiftPM app under `ui/swift`. The analyzer must
//! analyze EACH discovered config file (not re-detect one arbitrary config
//! per directory), so the Rust workspace, the Make wrapper and the Swift app
//! all appear — without duplicates.

use spire_code::build::{
    BuildModuleMessage, CargoBuildModule, MakeBuildModule, ModuleCapability, SwiftBuildModule,
};
use spire_code::subsystems::build::{BuildManagerActor, BuildManagerMessage};
use spire_core::analyzer::scanner::discover_build_files;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;

use spire_actor::ActorSystem;
use std::path::Path;
use tokio::sync::mpsc;

/// Spawn a build module and return its capability + message sender.
async fn describe_module<A: spire_actor::Actor<Message = BuildModuleMessage>>(
    system: &ActorSystem,
    module: A,
) -> (ModuleCapability, mpsc::Sender<BuildModuleMessage>) {
    let (tx, _handle) = system.spawn(module);
    let (r_tx, r_rx) = tokio::sync::oneshot::channel();
    tx.send(BuildModuleMessage::DescribeCapabilities { reply_to: r_tx })
        .await
        .unwrap();
    (r_rx.await.unwrap(), tx)
}

/// A channel whose messages are drained (mock memory-graph persistence).
fn drain_sender<T: Send + 'static>() -> mpsc::Sender<T> {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

fn spire_app_layout(root: &Path) {
    // Workspace root carrying BOTH a Cargo workspace and a Makefile wrapper.
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"crates/spire-gis\"]\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace.dependencies]\nspire-actor = { path = \"../spire-actor\" }\nspire-core = { path = \"../spire-core\" }\n",
    )
    .unwrap();
    std::fs::write(root.join("Makefile"), "build:\n\t@echo hi\n").unwrap();
    std::fs::write(root.join(".gitignore"), "/target/\n").unwrap();

    // Cargo member crate (workspace member — never an independent subproject).
    std::fs::create_dir_all(root.join("crates/spire-gis/src")).unwrap();
    std::fs::write(
        root.join("crates/spire-gis/Cargo.toml"),
        "[package]\nname = \"spire-gis\"\nversion.workspace = true\nedition.workspace = true\n\n[lib]\ncrate-type = [\"cdylib\", \"rlib\"]\n",
    )
    .unwrap();
    std::fs::write(root.join("crates/spire-gis/src/lib.rs"), "").unwrap();

    // SwiftPM companion app.
    std::fs::create_dir_all(root.join("ui/swift/Sources/SpireUI")).unwrap();
    std::fs::write(
        root.join("ui/swift/Package.swift"),
        "// swift-tools-version: 5.10\nimport PackageDescription\n\nlet package = Package(\n    name: \"SpireUI\",\n    platforms: [.macOS(.v14)],\n    products: [.executable(name: \"SpireUI\", targets: [\"SpireUI\"])],\n    targets: [.executableTarget(name: \"SpireUI\", path: \"Sources/SpireUI\")]\n)\n",
    )
    .unwrap();
    std::fs::write(root.join("ui/swift/Sources/SpireUI/App.swift"), "import SwiftUI\n").unwrap();
}

#[tokio::test]
async fn multi_config_directory_analyzes_each_discovered_config() {
    let system = ActorSystem::new();

    // Real modules for the layout under test.
    let (cargo_cap, cargo_tx) = describe_module(&system, CargoBuildModule::new()).await;
    let (make_cap, make_tx) = describe_module(&system, MakeBuildModule::new()).await;
    let (swift_cap, swift_tx) = describe_module(&system, SwiftBuildModule::new()).await;

    // Real BuildManagerActor with the three modules registered.
    let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(
        drain_sender::<MemoryGraphMessage>(),
        buffer,
        notify,
    ));
    for (cap, module_tx) in [(cargo_cap, cargo_tx), (make_cap, make_tx), (swift_cap, swift_tx)] {
        bm_tx
            .send(BuildManagerMessage::AddModule {
                capability: cap,
                module_tx,
            })
            .await
            .unwrap();
    }

    let tmp = tempfile::tempdir().unwrap();
    // The scanner skips hidden dirs (and tempfile's root starts with `.tmp`),
    // so lay the project out in a non-hidden subdirectory.
    let root = tmp.path().join("spire-gis");
    std::fs::create_dir_all(&root).unwrap();
    spire_app_layout(&root);

    // Discovery: the workspace root Cargo.toml + root Makefile + ui/swift
    // Package.swift (the member crate Cargo.toml is intentionally skipped).
    let discovered = discover_build_files(&root, false);
    assert_eq!(discovered.len(), 3, "discovered: {discovered:?}");

    // Route each discovered config exactly as the ProjectAnalyzer does.
    let mut build_systems: Vec<String> = Vec::new();
    for (build_file, dir) in &discovered {
        let target = if dir == "." || dir.is_empty() {
            root.to_path_buf()
        } else {
            root.join(dir)
        };
        let config_file = Path::new(build_file)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        let (r_tx, r_rx) = tokio::sync::oneshot::channel();
        bm_tx
            .send(BuildManagerMessage::AnalyzeProject {
                path: target.clone(),
                config_file: config_file.clone(),
                reply_to: r_tx,
            })
            .await
            .unwrap();
        let meta = r_rx
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("analysis failed for {build_file}: {e}"));
        if meta.build_system == "Cargo" {
            assert_eq!(
                meta.structure,
                spire_core::build_types::ProjectStructure::SpireApp
            );
            assert_eq!(meta.project_type, "spire_app");
            assert!(
                !meta.domains.is_empty(),
                "SpireApp should carry core/ui domains"
            );
        }
        build_systems.push(meta.build_system);
    }

    build_systems.sort();
    assert_eq!(
        build_systems,
        vec![
            "Cargo".to_string(),
            "Make".to_string(),
            "SwiftPM".to_string()
        ],
        "every config analyzed exactly once: {build_systems:?}"
    );
}

