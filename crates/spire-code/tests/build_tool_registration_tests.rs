// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Regression test: every build action the UI can invoke MUST be routable.
//!
//! `build_default_registry` registers exactly the tools a backend advertises
//! via `ListTools`. A tool implemented by `BuildManagerActor::call_tool` but
//! missing from its `list_tools()` is unreachable through `tools/call`: the
//! ToolRouter finds no handler, falls through to the MCP catch-all and fails
//! instantly — on every target. That is exactly what happened to
//! `build_clean` / `build_lint` (and `build_format`), so the Clean and Lint
//! buttons never worked.
//!
//! This builds the REAL registry against a REAL `BuildManagerActor` (with the
//! real build modules registered), so it fails if any UI build action is not
//! advertised — the unit test in `build_manager.rs` only checks the name list,
//! this one checks the wiring the app actually uses.

use spire_actor::ActorSystem;
use spire_code::actors::build_default_registry;
use spire_code::build::{
    BuildModuleMessage, CargoBuildModule, MakeBuildModule, MesonBuildModule, ModuleCapability,
    SwiftBuildModule,
};
use spire_code::subsystems::build::{BuildManagerActor, BuildManagerMessage};
use spire_core::actors::rag::RagMessage;
use spire_core::modules::{
    FilesystemMessage, GitMessage, ProcessMessage, SearchMessage, TerminalMessage,
};
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use spire_core::transport::socket::TransportMessage;
use tokio::sync::mpsc;

/// A channel whose messages are drained by a background task (mock backend).
fn drain<T: Send + 'static>() -> mpsc::Sender<T> {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

/// Spawn a build module and return its advertised capability + sender.
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

#[tokio::test]
async fn ui_build_actions_are_routable_through_tools_call() {
    let system = ActorSystem::new();

    // A real BuildManagerActor with the real build modules registered.
    let (bm_tx, _bm_handle) = system.spawn(BuildManagerActor::new(drain::<MemoryGraphMessage>()));
    let modules = vec![
        describe_module(&system, CargoBuildModule::new()).await,
        describe_module(&system, MesonBuildModule).await,
        describe_module(&system, MakeBuildModule::new()).await,
        describe_module(&system, SwiftBuildModule::new()).await,
    ];
    for (cap, module_tx) in modules {
        bm_tx
            .send(BuildManagerMessage::AddModule {
                capability: cap,
                module_tx,
            })
            .await
            .unwrap();
    }

    // The REAL registry the app builds at startup, against that BuildManager.
    let registry = build_default_registry(
        drain::<TransportMessage>(),
        drain::<spire_code::subsystems::project::project_query::ProjectQueryMessage>(),
        None,
        None,
        None,
        None,
        drain::<FilesystemMessage>(),
        drain::<GitMessage>(),
        drain::<ProcessMessage>(),
        drain::<SearchMessage>(),
        drain::<TerminalMessage>(),
        bm_tx,
        drain::<RagMessage>(),
    )
    .await
    .expect("build_default_registry");

    let registered: Vec<String> = registry.list().into_iter().map(|t| t.name).collect();

    // Every action the right-pane / action-rail can invoke on a build target.
    for name in [
        "build_analyze",
        "build_build",
        "build_verify",
        "build_test",
        "build_flash",
        "build_clean",
        "build_lint",
        "build_format",
        "build_fix",
    ] {
        assert!(
            registered.iter().any(|n| n == name),
            "'{name}' is handled by BuildManagerActor::call_tool but is NOT in \
             the ToolRegistry, so tools/call cannot reach it (the ToolRouter \
             falls through to the MCP catch-all). Registered: {registered:?}"
        );
        assert!(
            registry
                .call(name, serde_json::json!({ "path": "/nonexistent" }))
                .is_some(),
            "'{name}' is listed but the registry resolves no handler for it"
        );
    }

    // Negative control: the registry is NOT a catch-all, so the assertions
    // above are meaningful (an unknown name resolves to no handler — which is
    // exactly how the missing build_clean/build_lint names failed before).
    assert!(
        registry
            .call("build_definitely_not_a_tool", serde_json::json!({}))
            .is_none(),
        "registry resolved an unknown tool name — the assertions above are vacuous"
    );
}
