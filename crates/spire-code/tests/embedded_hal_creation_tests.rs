// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **embedded-HAL creation route**, driven the way the wizard drives it.
//!
//! `createProject/Plan` → `createProject/Scaffold` → the coverage and fill tools, through the real
//! coordinator, the real build manager (with its real cargo module), the real filesystem module and
//! a real platform registry.
//!
//! **No model anywhere.** For this structure the plan is deterministic — the scaffold's own writes
//! plus a parse and a host build gate — which is exactly the property that makes the route safe to
//! run unattended: the model is asked later, per backend file, by the fill cascade. A regression
//! that quietly sent this structure down the LLM path (as every other structure goes) would fail
//! here by needing a model that is not wired.
//!
//! The route had never been driven end to end; this is that drive, kept as a test.

use spire_actor::registry::ServiceRegistry;
use spire_actor::{Actor, ActorSystem};
use spire_code::actors::{
    build_default_registry, BuildManagerActor, BuildManagerMessage, CoordinatorActor,
    CoordinatorMessage, FfiSharedState, LlmActor, LlmConfig, McpClientActor, ProjectAnalyzerActor,
    ProjectAnalyzerMessage, ProjectCreationActor, ProjectQueryMessage, SystemActor,
    ToolRouterActor, ToolsActor,
};
use spire_code::build::{BuildModuleMessage, CargoBuildModule};
use spire_code::subsystems::project::project_creation::ProjectCreationMessage;
use spire_core::subsystems::graph::memory_graph::{MemoryGraphActor, MemoryGraphMessage};
use tokio::sync::mpsc;

/// A channel whose messages are drained by a background task (a stand-in for an actor this test
/// does not need).
fn mock<T: Send + 'static>() -> mpsc::Sender<T> {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

fn spawn_module<A: Actor>(actor: A) -> mpsc::Sender<A::Message> {
    let (tx, rx) = mpsc::channel::<A::Message>(32);
    actor.spawn(rx);
    tx
}

async fn register_build_module(
    cap_name: &str,
    module_tx: mpsc::Sender<BuildModuleMessage>,
    bm_tx: &mpsc::Sender<BuildManagerMessage>,
) {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = module_tx
        .send(BuildModuleMessage::DescribeCapabilities { reply_to: reply_tx })
        .await;
    let capability = reply_rx
        .await
        .unwrap_or_else(|_| panic!("module '{cap_name}' did not describe its capabilities"));
    let _ = bm_tx
        .send(BuildManagerMessage::AddModule {
            capability,
            module_tx,
        })
        .await;
}

/// The app's actor graph, reduced to what the creation route touches.
///
/// The filesystem and cargo modules are the **real** ones: the scaffold writes files and asks a
/// build module for the layout, so mocking either would test nothing that matters.
struct Wizard {
    coord: mpsc::Sender<CoordinatorMessage>,
}

impl Wizard {
    /// The harness with **no model wired at all** — the shape that proves the creation route needs
    /// none.
    async fn build() -> Self {
        Self::build_with_llm(None).await
    }

    /// The same harness with a model attached to the build manager, which is what the fill leg
    /// needs (one call per backend file). `None` leaves it detached on purpose.
    async fn build_with_llm(llm_config: Option<LlmConfig>) -> Self {
        let system = ActorSystem::new();

        let (memory_graph_tx, _) = system.spawn(MemoryGraphActor::new());
        let (bm_tx, _) = system.spawn(BuildManagerActor::new(memory_graph_tx.clone()));
        let (llm_tx, _) = system.spawn(LlmActor::new(llm_config.clone().unwrap_or_default()));
        let (mcp_tx, _) = system.spawn(McpClientActor::new());
        let (system_tx, _) = system.spawn(SystemActor::new());

        // Only a harness told to have a model hands one to the build manager — exactly as `ffi.rs`
        // does (it wires the LLM only when a key is configured). Everything else here is unchanged
        // by the presence or absence of a model.
        if llm_config.is_some() {
            let _ = bm_tx
                .send(BuildManagerMessage::SetLlm {
                    llm_tx: llm_tx.clone(),
                })
                .await;
        }

        // The real tool registry: `tools/call` resolves against it, so a tool that is not reachable
        // from here is a tool the UI cannot call.
        let project_query_tx = mock::<ProjectQueryMessage>();
        let tool_registry = build_default_registry(
            mock(),
            project_query_tx.clone(),
            Some(mock()),
            Some(mock()),
            Some(mock()),
            Some(mock()),
            mock(),
            mock(),
            mock(),
            mock(),
            mock(),
            bm_tx.clone(),
            mock(),
        )
        .await
        .expect("build the tool registry");
        let (tool_router_tx, _) = system.spawn(ToolRouterActor::new(tool_registry, mcp_tx.clone()));
        let (tools_tx, _) = system.spawn(ToolsActor::new(tool_router_tx.clone()));

        // The filesystem module, because the scaffold writes through it.
        let fs_tx = spawn_module(spire_core::modules::FilesystemModule::new());

        // The cargo module, because the embedded-HAL layout comes from it — registered by config
        // file exactly as `ffi.rs` registers it, so the scaffold request routes here.
        let cargo_tx = spawn_module(CargoBuildModule::new());
        register_build_module("cargo", cargo_tx, &bm_tx).await;

        let mut creation = ProjectCreationActor::new(fs_tx, bm_tx.clone(), mcp_tx.clone());
        // Deliberately **not** wired to a model: the plan for this structure must not need one. The
        // memory graph is, because the scaffold records what it wrote.
        creation.set_memory_graph(memory_graph_tx.clone());
        let (creation_tx, _) = system.spawn(creation);

        let (coord_tx, _) = system.spawn(CoordinatorActor::new(
            mock(),
            tools_tx,
            mcp_tx,
            llm_tx,
            system_tx,
            memory_graph_tx.clone(),
            project_query_tx.clone(),
            mock(),
            tool_router_tx,
            mock(),
            mock(),
        ));

        let registry = std::sync::Arc::new(ServiceRegistry::new());
        let (analyzer_tx, _) = system.spawn(ProjectAnalyzerActor::new());
        let _ = registry.register::<ProjectAnalyzerMessage>("project.analyzer", analyzer_tx);
        let _ = registry.register::<BuildManagerMessage>("build.manager", bm_tx);
        let _ = registry.register::<MemoryGraphMessage>("memory_graph", memory_graph_tx.clone());
        let _ = registry.register::<MemoryGraphMessage>("knowledge_graph", memory_graph_tx);
        let _ = registry.register::<ProjectQueryMessage>("project.query", project_query_tx);
        let _ = registry.register::<ProjectCreationMessage>("project_creation", creation_tx);
        let _ = coord_tx
            .send(CoordinatorMessage::SetFfiDeps {
                registry,
                state: std::sync::Arc::new(FfiSharedState {
                    project_root: std::sync::Mutex::new(None),
                    analysis: std::sync::Mutex::new(None),
                    watcher_out_tx: mock(),
                }),
            })
            .await;

        Self { coord: coord_tx }
    }

    /// Send one request and wait for the coordinator's answer — the same envelope the UI sends.
    async fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.coord
            .send(CoordinatorMessage::HandleRequest {
                method: method.to_string(),
                params,
                response_tx: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    async fn tool(&self, tool: &str, args: serde_json::Value) -> serde_json::Value {
        self.call(
            "tools/call",
            serde_json::json!({ "tool": tool, "args": args }),
        )
        .await
    }
}

/// A registry with one variant of each family, so `esp32c6` + `rp2040` collapse to two backends.
fn registry() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("platform dir");
    std::fs::write(
        dir.path().join("esp32c6.yaml"),
        "id: esp32c6\nname: ESP32-C6\nos: esp-idf\nfamily: esp32\n\
         library_hints: RISC-V RV32IMAC via esp-idf-hal.\n\
         architecture:\n  cpu_family: riscv\n  cpu: esp32c6\n  endian: little\n  \
         target_triple: riscv32imac-esp-espidf\n\
         rust:\n  target: riscv32imac-esp-espidf\n  idf_target: esp32c6\n  flash: espflash\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("rp2040.yaml"),
        "id: rp2040\nname: Raspberry Pi Pico\nos: rp2040\nfamily: rp2040\n\
         library_hints: Cortex-M0+, no radio, PIO and on-chip USB.\n\
         architecture:\n  cpu_family: arm\n  cpu: rp2040\n  endian: little\n  \
         target_triple: thumbv6m-none-eabi\n\
         rust:\n  target: thumbv6m-none-eabi\n  idf_target: RP2040\n  flash: picotool\n",
    )
    .unwrap();
    dir
}

/// The whole route a user walks: describe → plan → scaffold → measure → plan the fill.
///
/// What it proves, in order: the plan needs **no model** (the creation actor has none wired), the
/// scaffold writes the workspace the emitter describes, the coverage measure reads it back as
/// backend stubs, and the fill plan turns those stubs into work items naming the right files with
/// the board's own hints.
#[tokio::test]
async fn the_wizard_creates_an_embedded_hal_project_and_the_loop_closes() {
    // No lock around `SPIRE_PLATFORM_DIR`: an integration-test binary is its own process, and this
    // is the only test in it, so nothing else can be reading the variable. (The unit-test lock
    // exists because those tests share a process.)
    let platforms = registry();
    std::env::set_var("SPIRE_PLATFORM_DIR", platforms.path());

    let dir = tempfile::tempdir().expect("project dir");
    let root = dir.path().join("blink-workshop");
    let wizard = Wizard::build().await;

    // ── 1. Plan: deterministic, and it says so ──
    let plan = wizard
        .call(
            "createProject/Plan",
            serde_json::json!({
                "goal": "blink an LED on two boards",
                "rootDir": root.to_string_lossy(),
                "projectName": "blink-workshop",
                "language": "Rust",
                "platforms": ["esp32c6", "rp2040"],
                "structure": "embedded_hal",
            }),
        )
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    assert!(
        plan["plan"]["is_template"].as_bool().unwrap_or(false),
        "the plan must be the deterministic one, not a model's: {plan}"
    );
    let steps = plan["plan"]["steps"].as_array().expect("plan steps");
    let descriptions: Vec<&str> = steps
        .iter()
        .filter_map(|s| s["description"].as_str())
        .collect();
    assert!(
        descriptions
            .iter()
            .any(|d| d.contains("crates/blink-workshop-hal/Cargo.toml")),
        "the contract crate is written: {descriptions:?}"
    );
    assert!(
        descriptions
            .iter()
            .any(|d| d.contains("crates/blink-workshop-hal-esp32/Cargo.toml"))
            && descriptions
                .iter()
                .any(|d| d.contains("crates/blink-workshop-hal-rp2040/Cargo.toml")),
        "one backend per family, both families: {descriptions:?}"
    );
    assert_eq!(
        steps.last().map(|s| s["stepType"].clone()),
        Some(serde_json::json!("build")),
        "the plan ends at the build gate: {steps:?}"
    );

    // ── 2. Scaffold: the workspace lands on disk ──
    let scaffold = wizard
        .call(
            "createProject/Scaffold",
            serde_json::json!({
                "projectName": "blink-workshop",
                "rootDir": root.to_string_lossy(),
                "language": "Rust",
                "platforms": ["esp32c6", "rp2040"],
                "structure": "embedded_hal",
            }),
        )
        .await;
    assert!(scaffold.get("error").is_none(), "{scaffold}");

    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("a workspace manifest");
    assert!(
        manifest.contains("structure = \"embedded_hal\""),
        "the declaration the analyzer reads back: {manifest}"
    );
    for path in [
        "crates/blink-workshop-hal/src/lib.rs",
        "crates/blink-workshop-hal/src/hal/led.rs",
        "crates/blink-workshop-hal/src/hal/time.rs",
        "crates/blink-workshop-hal-std/src/lib.rs",
        "crates/blink-workshop-hal-esp32/src/lib.rs",
        "crates/blink-workshop-hal-rp2040/src/lib.rs",
    ] {
        assert!(root.join(path).is_file(), "{path} was not written");
    }

    // ── 3. Measure: a fresh backend is a stub, not coverage ──
    let coverage = wizard
        .tool(
            "hal_missing_impls",
            serde_json::json!({ "root": root.to_string_lossy() }),
        )
        .await;
    for family in ["esp32", "rp2040"] {
        let led = &coverage["platforms"][family]["led"];
        assert_eq!(
            led["is_stub"],
            serde_json::json!(true),
            "{family}: {coverage}"
        );
        assert_eq!(led["implemented"], serde_json::json!(false), "{family}");
        assert_eq!(led["kind"], serde_json::json!("stub"), "{family}");
    }
    assert_eq!(
        coverage["platforms"]["esp32"]["time"]["is_stub"],
        serde_json::json!(true),
        "the interface whose trait name differs from its file stem is measured too: {coverage}"
    );

    // ── 4. The fill plan names the files and carries the boards' notes ──
    let fill = wizard
        .tool(
            "embedded_hal_fill_plan",
            serde_json::json!({ "root": root.to_string_lossy() }),
        )
        .await;
    let items = fill["plan"].as_array().expect("fill items");
    assert_eq!(items.len(), 2, "one item per backend file: {fill}");
    let families: Vec<&str> = items.iter().filter_map(|i| i["family"].as_str()).collect();
    assert_eq!(families, vec!["esp32", "rp2040"], "{fill}");
    for item in items {
        assert!(item["vendor_crate"].as_str().is_some(), "{item}");
        assert!(
            item["file"].as_str().unwrap().ends_with("src/lib.rs"),
            "the file to edit: {item}"
        );
        let prompt = item["prompt"].as_str().expect("a prompt");
        assert!(prompt.contains("unimplemented!()"), "{prompt}");
    }
    let esp_prompt = items[0]["prompt"].as_str().unwrap();
    assert!(
        esp_prompt.contains("RISC-V RV32IMAC via esp-idf-hal"),
        "the board's own hints reach the prompt: {esp_prompt}"
    );
    assert!(
        esp_prompt.contains("crates/blink-workshop-hal/src/hal/led.rs"),
        "and so does the contract it must implement: {esp_prompt}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────
// The live model: the one thing a fake cannot answer
//
// Everything above runs without a model, which is what makes it deterministic. This does the
// opposite — one real call per backend file — because the fill leg's whole value is what a model
// does with that prompt, and only a model can answer that. What is asserted is STRUCTURE (every
// pending trait implemented, no placeholder left, the measure flipping to implemented), never exact
// text: a model may legitimately write the pin handling differently every run.
//
// Gated twice, like the other live test in this crate: `#[ignore]` so a plain `cargo test` never
// spends money or network, and a runtime check so that even `--ignored` skips cleanly with no key
// configured. The key is read through `load_global_llm_config()` — the same call the app makes at
// startup — so it never lives in this file. Run it with:
//
//     cargo test -p spire-code --test embedded_hal_creation_tests -- --ignored
// ─────────────────────────────────────────────────────────────────────────────────────
#[ignore = "live model: spends real calls — run explicitly with `--ignored`"]
#[tokio::test]
async fn a_real_model_fills_the_backends_it_is_asked_for() {
    // ── The gate ──
    let llm_config = spire_core::config::load_global_llm_config();
    if llm_config.api_key.trim().is_empty() {
        eprintln!("skipping: no API key in ~/.spire/llm-config.json");
        return;
    }

    let platforms = registry();
    std::env::set_var("SPIRE_PLATFORM_DIR", platforms.path());
    let dir = tempfile::tempdir().expect("project dir");
    let root = dir.path().join("blink-live");
    let wizard = Wizard::build_with_llm(Some(llm_config)).await;

    // Scaffold first: the fill needs files to fill.
    let scaffold = wizard
        .call(
            "createProject/Scaffold",
            serde_json::json!({
                "projectName": "blink-live",
                "rootDir": root.to_string_lossy(),
                "language": "Rust",
                "platforms": ["esp32c6", "rp2040"],
                "structure": "embedded_hal",
            }),
        )
        .await;
    assert!(scaffold.get("error").is_none(), "{scaffold}");

    let plan = wizard
        .tool(
            "embedded_hal_fill_plan",
            serde_json::json!({ "root": root.to_string_lossy() }),
        )
        .await;
    let items = plan["plan"].as_array().expect("fill items").clone();
    assert_eq!(items.len(), 2, "{plan}");

    let applied = wizard
        .tool(
            "embedded_hal_fill_apply",
            serde_json::json!({ "root": root.to_string_lossy(), "plan": items }),
        )
        .await;
    assert_eq!(
        applied["failures"],
        serde_json::json!([]),
        "the gate refused a real model's answer — the reason is in failures: {applied}"
    );
    assert_eq!(
        applied["applied"].as_array().map(|a| a.len()),
        Some(2),
        "{applied}"
    );

    // The measure is the verdict, not the model's word: both backends implemented, no placeholders.
    let coverage = wizard
        .tool(
            "hal_missing_impls",
            serde_json::json!({ "root": root.to_string_lossy() }),
        )
        .await;
    for family in ["esp32", "rp2040"] {
        assert_eq!(
            coverage["platforms"][family]["led"]["implemented"],
            serde_json::json!(true),
            "{family}: {coverage}"
        );
        assert_eq!(
            coverage["platforms"][family]["time"]["implemented"],
            serde_json::json!(true),
            "{family}: {coverage}"
        );
    }
    for backend in [
        "crates/blink-live-hal-esp32/src/lib.rs",
        "crates/blink-live-hal-rp2040/src/lib.rs",
    ] {
        let source = std::fs::read_to_string(root.join(backend)).unwrap();
        // Only that the file is still a file. Text checks are the wrong tool here, twice over: the
        // scaffold's own header *mentions* `unimplemented!()`, and a correct trait impl may be
        // written `impl<'d> Led for GpioLed<'d>` (which is a word away from `impl Led`). The
        // `implemented` assertions above are the real check — they come from the editor's own
        // measure, which reads impl blocks rather than text.
        assert!(
            source.lines().count() > 20,
            "{backend} came back as a fragment: {source}"
        );
    }
}
