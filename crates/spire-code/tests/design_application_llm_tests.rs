// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **design phase** against a real LLM path — with the *model* replaced, not the code.
//!
//! `application_spec`'s own tests inject a closure, so they prove the six rules and the repair turn
//! but not the wiring. These tests point the **real** `LlmActor` at a fake OpenAI-compatible endpoint:
//! the HTTP request, the response parse, the coordinator's route, the planning-role call, the
//! validation, and the repair as a *second round trip* — all real. Only the model's text is scripted,
//! which is the one thing you want to control anyway.
//!
//! What this cannot tell you is whether a real model gives *good* decompositions. That is a quality
//! question and needs a live endpoint; this is the plumbing, where the bugs are.

use spire_actor::ActorSystem;
use spire_code::actors::{
    ChatActor, CoordinatorActor, CoordinatorMessage, LlmActor, LlmConfig, McpClientActor,
    SystemActor, ToolsActor,
};
use spire_code::build::application_spec::examples;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;
use tokio::sync::mpsc;

mod common;

use common::{fake_llm, fake_llm_logging, mock_sender};
use std::sync::{Arc, Mutex};

/// The coordinator's memory-graph channel with no actor behind it.
fn mock_memory_graph() -> mpsc::Sender<MemoryGraphMessage> {
    let (tx, _rx) = mpsc::channel(64);
    tx
}

/// A coordinator wired to a real LLM actor pointed at `url`.
///
/// Everything else is a mock, and that is the point: the design phase needs the model and nothing
/// else — no project, no files, no build — so nothing else can influence the result.
async fn app(url: &str) -> mpsc::Sender<CoordinatorMessage> {
    let system = ActorSystem::new();
    let (chat_tx, _) = system.spawn(ChatActor::new());
    let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
    let (mcp_tx, _) = system.spawn(McpClientActor::new());
    let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig {
        api_url: url.to_string(),
        // The actor refuses a request with no key, but never checks it against anything.
        api_key: "test-key".to_string(),
        ..LlmConfig::default()
    }));
    let (system_tx, _) = system.spawn(SystemActor::new());

    let (coord_tx, _handle) = system.spawn(CoordinatorActor::new(
        chat_tx,
        tools_tx,
        mcp_tx,
        llm_tx,
        system_tx,
        mock_memory_graph(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
        mock_sender(),
    ));
    coord_tx
}

/// Send one request and wait for the coordinator's answer.
async fn call(
    coord: &mpsc::Sender<CoordinatorMessage>,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let (tx, rx) = tokio::sync::oneshot::channel();
    coord
        .send(CoordinatorMessage::HandleRequest {
            method: method.to_string(),
            params,
            response_tx: tx,
        })
        .await
        .unwrap();
    rx.await.unwrap()
}

/// The board and the form's answers, as the wizard sends them.
fn request() -> serde_json::Value {
    serde_json::json!({
        "board": { "chip": "esp32s3", "bsp": "m5stack_core_s3", "hal": "m5unified" },
        "description": "a PM2.5 meter with a touch screen and a battery gauge",
    })
}

/// The whole path with a model behind it: the request goes out, the answer comes back, and what the
/// caller gets is a spec that passed the checks plus the line the application will state.
#[tokio::test]
async fn designs_an_application_from_the_form() {
    let url = fake_llm(vec![examples::PM25_METER.to_string()]);
    let coord = app(&url).await;

    let result = call(&coord, "createProject/DesignApplication", request()).await;

    assert_eq!(
        result["spec"]["framework"],
        serde_json::json!("actors"),
        "{result}"
    );
    assert_eq!(
        result["spec"]["board"]["bsp"],
        serde_json::json!("m5stack_core_s3")
    );
    assert_eq!(
        result["marker"],
        serde_json::json!("set(SPIRE_APPLICATION_FRAMEWORK actors)"),
        "the line the scaffold will write is shown at review: {result}"
    );
    assert!(
        result["spec"]["units"]
            .as_array()
            .expect("units")
            .iter()
            .any(|u| u["id"] == "sps30"),
        "{result}"
    );
}

/// **The board is the caller's, whatever the model answers with.**
///
/// A live run asked for `espressif/m5stack_core_s3` and got `m5stack/cores3` back — which the scaffold
/// then wrote into `main/idf_component.yml`, where it did not resolve. The board is a *given*: the
/// wizard collected it, the request states it, and the answer's is overwritten rather than trusted.
#[tokio::test]
async fn the_model_cannot_change_the_board_it_was_given() {
    let swapped = examples::PM25_METER
        .replace(r#""bsp": "m5stack_core_s3""#, r#""bsp": "m5stack/cores3""#)
        .replace(r#""hal": "m5unified""#, r#""hal": "esp-idf""#);
    assert_ne!(
        swapped,
        examples::PM25_METER,
        "the fixture's board was rewritten"
    );
    let url = fake_llm(vec![swapped]);
    let coord = app(&url).await;

    let result = call(&coord, "createProject/DesignApplication", request()).await;

    assert_eq!(
        result["spec"]["board"]["bsp"],
        serde_json::json!("m5stack_core_s3"),
        "the BSP the wizard collected, never the model's: {result}"
    );
    assert_eq!(
        result["spec"]["board"]["hal"],
        serde_json::json!("m5unified")
    );
    assert_eq!(
        result["spec"]["board"]["chip"],
        serde_json::json!("esp32s3")
    );
    // And the decomposition the model *did* decide is kept: the board is pinned, nothing else is.
    assert_eq!(
        result["spec"]["units"].as_array().expect("units").len(),
        8,
        "only the board is the caller's: {result}"
    );
}

/// A decomposition with a hole in it gets a *second round trip* through the real actor, with the
/// problems named — and the caller sees only the answer that passed.
#[tokio::test]
async fn a_decomposition_with_a_hole_is_repaired() {
    // An actor with no message: the fault rule 2 exists for.
    let broken = examples::PM25_METER.replace(r#""message": "Reading", "#, "");
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let url = fake_llm_logging(vec![broken, examples::PM25_METER.to_string()], log.clone());
    let coord = app(&url).await;

    let result = call(&coord, "createProject/DesignApplication", request()).await;

    assert_eq!(
        result["spec"]["framework"],
        serde_json::json!("actors"),
        "{result}"
    );
    let requests = log.lock().unwrap();
    assert_eq!(requests.len(), 2, "the first answer was repaired over HTTP");
    assert!(
        requests[1].contains("is an actor with no message"),
        "and the repair turn says what was wrong:\n{}",
        requests[1]
    );
}

/// What the model is *asked* is not observable from its answer, so it is checked on the wire: the
/// board and the user's own words have to be in the request that leaves.
#[tokio::test]
async fn the_board_and_the_form_reach_the_model() {
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let url = fake_llm_logging(vec![examples::PM25_METER.to_string()], log.clone());
    let coord = app(&url).await;

    let _ = call(&coord, "createProject/DesignApplication", request()).await;

    let requests = log.lock().unwrap();
    assert_eq!(requests.len(), 1, "one design request");
    let request = &requests[0];
    for expected in [
        "m5stack_core_s3", // the board
        "m5unified",
        "a PM2.5 meter with a touch screen and a battery gauge", // the user's words
        "human timescale", // and the rule for choosing the framework
    ] {
        assert!(
            request.contains(expected),
            "the request that left is missing {expected:?}:\n{request}"
        );
    }
}

/// The **tool** path, which is the one a model uses: flat arguments in, the same reviewed spec out.
/// The adapter is the only difference from the request path, so this is what proves the model's
/// arguments reach the same design phase the wizard's do.
#[tokio::test]
async fn the_model_can_call_it_as_a_tool() {
    let url = fake_llm(vec![examples::PM25_METER.to_string()]);
    let coord = app(&url).await;

    let result = call(
        &coord,
        "tools/call",
        serde_json::json!({
            "tool": "idf_design_application",
            "args": {
                "chip": "esp32s3",
                "bsp": "m5stack_core_s3",
                "hal": "m5unified",
                "description": "a PM2.5 meter with a touch screen",
            },
        }),
    )
    .await;

    assert_eq!(
        result["spec"]["framework"],
        serde_json::json!("actors"),
        "{result}"
    );
    assert_eq!(
        result["marker"],
        serde_json::json!("set(SPIRE_APPLICATION_FRAMEWORK actors)")
    );
}

/// The **library** the application is built against reaches the model, read from the tree: the design
/// states `"source": "existing"` or `"stub"` about every component it names, and that is a claim about
/// this library — so without it the design is inventing which parts already exist.
#[tokio::test]
async fn the_library_the_application_is_built_against_reaches_the_model() {
    use spire_code::build::idf_projects::{
        add_component, library_scaffold, ComponentKind, HINTS_FILE,
    };

    // A library on disk, with one of the components app 1 needs already written.
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path();
    let scaffold = library_scaffold("sensors", &[]).expect("scaffolds");
    for file in &scaffold.files {
        let path = library.join(&file.path);
        std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
        std::fs::write(&path, &file.content).unwrap();
    }
    add_component(library, "sps30", ComponentKind::Driver, "i2c").expect("installs");
    std::fs::write(
        library.join(HINTS_FILE),
        "# sensors\n\nCall `sensors::begin()` once, before any read.\n",
    )
    .unwrap();

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let url = fake_llm_logging(vec![examples::PM25_METER.to_string()], log.clone());
    let coord = app(&url).await;

    let mut params = request();
    params["libraryRoot"] = serde_json::json!(library.to_string_lossy());
    let result = call(&coord, "createProject/DesignApplication", params).await;
    assert_eq!(
        result["spec"]["framework"],
        serde_json::json!("actors"),
        "{result}"
    );

    let requests = log.lock().unwrap();
    let sent = &requests[0];
    for expected in [
        library.to_string_lossy().as_ref(), // where it is, for a person reading it
        "- `sps30` — driver",               // what it already has, and what that component is
        "- `toolkit` — library",            // including the framework it ships
        // What the model is told to *do* with those facts. (The body is JSON, so the quoted
        // `"source": …` values arrive escaped — the instruction is the part worth asserting.)
        "they are here. Anything else you design has to be written",
        "Call `sensors::begin()` once, before any read.", // its architecture
    ] {
        assert!(
            sent.contains(expected),
            "the design request is missing {expected:?}:\n{sent}"
        );
    }
    assert!(
        !sent.contains("None named"),
        "a library was named, so the no-library wording is absent"
    );
}

/// A path that is **not** a component library is refused by name rather than read as an empty one: a
/// design told "this library has nothing" about the wrong directory designs the wrong application, and
/// says so confidently.
#[tokio::test]
async fn a_library_root_that_is_not_a_library_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("CMakeLists.txt"),
        "project(not_a_library)\n",
    )
    .unwrap();

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let url = fake_llm_logging(vec![examples::PM25_METER.to_string()], log.clone());
    let coord = app(&url).await;

    let mut params = request();
    params["libraryRoot"] = serde_json::json!(tmp.path().to_string_lossy());
    let result = call(&coord, "createProject/DesignApplication", params).await;

    let error = result["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{result}"));
    assert!(error.contains("is not a component library"), "{error}");
    assert!(
        log.lock().unwrap().is_empty(),
        "and the model was not asked at all"
    );
}

/// A board with no chip is refused *before* the model is asked: a guess at the target is a build that
/// fails on hardware, and a model asked anyway would answer about a different board. The endpoint has a
/// valid answer queued, so a design that went out regardless would come back as a spec — which is
/// exactly what this asserts it does not.
#[tokio::test]
async fn a_board_with_no_chip_is_refused_before_the_model_is_asked() {
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let url = fake_llm_logging(vec![examples::PM25_METER.to_string()], log.clone());
    let coord = app(&url).await;

    let result = call(
        &coord,
        "createProject/DesignApplication",
        serde_json::json!({
            "board": { "chip": "", "bsp": "m5stack_core_s3" },
            "description": "anything",
        }),
    )
    .await;

    let error = result["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{result}"));
    assert!(error.contains("chip"), "{error}");
    assert!(
        log.lock().unwrap().is_empty(),
        "the model was not asked at all"
    );
}

/// **The design phase's other door**: a composition a person already has.
///
/// The wizard can hand in a `composition.spire` instead of answering the six questions, and the Rust
/// side reads it with the *same* parser and the same six rules a project's own file is read with — so
/// what is reviewed is held to exactly what a file on a tree is. Nothing is designed: the model is
/// never asked, because the composition is already written down.
#[tokio::test]
async fn a_composition_a_person_already_has_is_read_into_the_design_phase() {
    use spire_code::build::application_spec::{parse_spec, ApplicationSpec};
    use spire_code::build::idf_projects::render_composition;

    // The worked example as the **file** the scaffold writes: YAML, which is what a `composition.spire`
    // is. Rendered rather than written out as a fixture, because this is the round trip such a file
    // comes from — and it is the same byte-for-byte rendering the wizard would have written.
    let designed = parse_spec(examples::PM25_METER).expect("the worked example parses");
    let yaml = render_composition(&designed).expect("and renders as a composition");

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let url = fake_llm_logging(vec![examples::PM25_METER.to_string()], log.clone());
    let coord = app(&url).await;

    let result = call(
        &coord,
        "createProject/ParseComposition",
        serde_json::json!({ "text": yaml, "name": "composition.spire" }),
    )
    .await;

    let read_back: ApplicationSpec = serde_json::from_value(result["spec"].clone())
        .unwrap_or_else(|e| panic!("the spec comes back: {e}\n{result}"));
    assert_eq!(
        read_back, designed,
        "the composition is read back as the design it states: {result}"
    );
    assert_eq!(
        result["marker"],
        serde_json::json!("set(SPIRE_APPLICATION_FRAMEWORK actors)"),
        "the shape the wizard reviews is the design phase's, line for line: {result}"
    );
    assert!(
        log.lock().unwrap().is_empty(),
        "nothing was designed: the composition was already written down"
    );
}

/// A file that is **not** a composition — malformed, or parsing and breaking the rules — is refused by
/// name, and the refusal carries **every** broken rule at once, before any tree exists.
#[tokio::test]
async fn a_composition_that_is_not_one_is_refused_by_name() {
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let url = fake_llm_logging(vec![examples::PM25_METER.to_string()], log.clone());
    let coord = app(&url).await;

    // Not YAML at all: refused by the name the caller gave it, not by a line in this actor.
    let junk = call(
        &coord,
        "createProject/ParseComposition",
        serde_json::json!({ "text": "this is not { a composition", "name": "broken.spire" }),
    )
    .await;
    let error = junk["error"].as_str().unwrap_or_else(|| panic!("{junk}"));
    assert!(
        error.starts_with("broken.spire is not a composition"),
        "the file is named, and it is the caller's name: {error}"
    );

    // YAML that parses and breaks two rules: an actor with no message, and one id declared twice. Both
    // are in the refusal, because a person repairing a file fixes it in one pass.
    let text = r#"
framework: actors
board:
  chip: esp32s3
  bsp: m5stack_core_s3
units:
  - id: sampler
    kind: actor
  - id: sampler
    kind: actor
    message: Report
"#;
    let broken = call(
        &coord,
        "createProject/ParseComposition",
        serde_json::json!({ "text": text, "name": "composition.spire" }),
    )
    .await;
    let error = broken["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{broken}"));
    for expected in [
        "composition.spire states a design that does not hold together",
        "is an actor with no message",
        "is declared twice",
    ] {
        assert!(
            error.contains(expected),
            "{expected:?} missing from the refusal: {error}"
        );
    }

    assert!(
        log.lock().unwrap().is_empty(),
        "and no model was asked to repair it: the file is the thing to fix"
    );
}
