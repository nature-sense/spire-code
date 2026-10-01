// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! `idf_component_edit`'s **retrieval seam**, on the wire: what the model is *asked*.
//!
//! The seam pre-fetches knowledge and puts it in the prompt, because the edit runs through
//! `run_code_modify` — one prompt, no tool calls — so a model that wanted to look its device up could
//! not. Three things follow from that, and all three are observable only in the request body: the
//! material **arrives** (a `sht20` edit is the test — the facts corpus is asked by part number), it
//! arrives **labelled** (this part's protocol under one heading, a comparable driver's code under
//! another), and a corpus that cannot answer leaves **no trace**, rather than an empty heading that
//! reads as "the facts are: nothing".
//!
//! What is real here: the coordinator and its router, a `RagActor` over a real store with the real
//! `device-facts` corpus installed and ingested, a real `LlmActor` posting HTTP to a fake
//! OpenAI-compatible endpoint that records every request body, and the **gate** — the component the
//! model would edit is one this tool scaffolded (`add_component`), so `cmake` configures and builds
//! its host test exactly as it would for a user. The model's *answer* is scripted as `NONE` because
//! the question is the subject, not the answer: a component edit rewrites its whole scope, so
//! `NONE` is not a file and each rewrite is simply rejected by the structural check.
//!
//! Two things have to be on the machine: `cmake` and `ctest` (the gate's own requirement) and the
//! embedding model — a store cannot even be *ingested* without it, since `NoopEmbedder` fails every
//! call rather than degrading to zero vectors. Missing either, these tests say so and return rather
//! than reporting a pass they did not earn.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use spire_actor::registry::ServiceRegistry;
use spire_actor::ActorSystem;
use spire_code::actors::{
    rag_bundle, ChatActor, CoordinatorActor, CoordinatorMessage, FfiSharedState, LlmActor,
    LlmConfig, McpClientActor, SystemActor, ToolsActor,
};
use spire_code::build::edit_history;
use spire_code::build::idf_projects::{add_component, ComponentKind, LIBRARY_MARKER};
use spire_core::actors::rag::{EmbedderService, RagActor, RagMessage};
use spire_core::actors::MemoryGraphMessage as MgMsg;
use spire_core::models::embedding::Embedder;
use spire_core::models::memory_graph::{AttrNode, GraphEdge, RelationshipType};
use spire_core::subsystems::graph::memory_graph::MemoryGraphActor;
use tokio::sync::mpsc;

mod common;

use common::{fake_llm_logging, mock_sender};

/// The heading the reference block is introduced under, copied from the prompt builder rather than
/// re-worded: if that wording changes, these tests follow it — by failing.
const REFERENCE_BLOCK: &str = "## Reference material retrieved for this device";
/// The heading the **device's own protocol** is put under, which is what makes its bytes usable.
const FACTS_HEADING: &str =
    "The device's own protocol, from the device-facts corpus (`device-facts`)";
/// The heading a *comparable* driver's code is put under, with the corpus that would answer it. The
/// corpus name is the part to search for: the role's words alone are **not** enough, because the
/// reference block's own framing prose uses them to explain the two kinds of section — only the
/// heading carries the corpus, which is what makes it a section rather than a sentence.
const PRECEDENT_HEADING: &str = "Somebody else's driver for a comparable device (`esp-idf-lib`)";

/// Does this machine have the gate's own tools? `build_host_test` needs `cmake`, the test leg needs
/// `ctest`, and without them `idf_component_edit` refuses before it ever asks the model — correct
/// behaviour, and a useless fixture for this test.
fn gate_is_runnable() -> bool {
    ["cmake", "ctest"].iter().all(|tool| {
        std::process::Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

/// The real embedder, or `None` with a line saying why — see the note in this file's header: there is
/// no fallback to fall back to, because `NoopEmbedder` fails every call on purpose.
fn real_embedder() -> Option<Arc<dyn Embedder>> {
    match spire_core::embedder::CandleEmbedder::new() {
        Ok(embedder) => Some(Arc::new(embedder)),
        Err(e) => {
            eprintln!(
                "no Candle embedder ({e}) — nothing can be ingested without one, so this is skipped"
            );
            None
        }
    }
}

/// Initialize a graph actor at `dir` — the two calls the app makes at startup.
async fn init_graph(tx: &mpsc::Sender<MgMsg>, dir: &std::path::Path) {
    let (reply, rx) = tokio::sync::oneshot::channel();
    tx.send(MgMsg::Initialize {
        data_dir: dir.to_path_buf(),
        reply_to: reply,
    })
    .await
    .expect("send Initialize");
    rx.await.expect("Initialize reply").expect("Initialize");
}

/// An `idf_component_edit` run's worth of app: a scaffolded `sht20` component, the `device-facts`
/// corpus in a temp store behind a live `RagActor`, and a coordinator whose LLM is the fake endpoint.
struct Seam {
    root: PathBuf,
    coord: mpsc::Sender<CoordinatorMessage>,
    /// The coordinator's **project graph**, cloned so the test can read back what a run recorded.
    graph: mpsc::Sender<MgMsg>,
    /// Every request body the fake endpoint was sent.
    log: Arc<Mutex<Vec<String>>>,
    /// Held so the temp directories outlive the test.
    _tmp: (tempfile::TempDir, tempfile::TempDir),
}

impl Seam {
    /// Everything the coordinator's retrieval and its prompt need, wired the way the app wires it.
    ///
    /// `None` when the embedding model is not on this machine — the corpus cannot be ingested
    /// without it, so there would be nothing to assert about.
    async fn build() -> Option<Self> {
        let embedder = real_embedder()?;
        let root_tmp = tempfile::tempdir().expect("temp root");
        let root = root_tmp.path().to_path_buf();

        // A **library**, because a component belongs to one and `add_component` refuses anything
        // else — and scaffolded rather than hand-written, so the fixture is what this tool really
        // produces, host test and all.
        std::fs::write(
            root.join("CMakeLists.txt"),
            format!("{LIBRARY_MARKER}\nproject(sensors)\n"),
        )
        .expect("write the library's CMakeLists.txt");
        std::fs::write(
            root.join("SPIRE.md"),
            "# sensors\n\nOne driver per device.\n",
        )
        .expect("write SPIRE.md");
        add_component(&root, "sht20", ComponentKind::Driver, "i2c")
            .expect("scaffold the component");

        // The corpus: installed where the manifest's own relative `path` resolves, then ingested.
        let store_tmp = tempfile::tempdir().expect("temp store");
        let store = store_tmp.path().to_path_buf();
        rag_bundle::install_into(&store).expect("install the bundle");

        let system = ActorSystem::new();
        let (memory_graph_tx, _) = system.spawn(MemoryGraphActor::new());
        let (knowledge_tx, _) = system.spawn(MemoryGraphActor::new());
        let provenance = root.join("provenance");
        std::fs::create_dir_all(&provenance).expect("create the provenance data dir");
        init_graph(&memory_graph_tx, &provenance).await;
        init_graph(&knowledge_tx, &store).await;

        // The real embedder: retrieval is a vector search, so the part number selects the document
        // *through* the model, which is the path the app takes. Which corpus answered is the subject.
        {
            let (reply, rx) = tokio::sync::oneshot::channel();
            knowledge_tx
                .send(MgMsg::InitializeEmbedder {
                    model_path: None,
                    embedder: Some(embedder.clone()),
                    reply_to: reply,
                })
                .await
                .expect("send InitializeEmbedder");
            let _ = rx.await;
        }

        let registry = Arc::new(ServiceRegistry::new());
        let _ = registry.register_service("embedder", Arc::new(EmbedderService(embedder)));
        let _ = registry.register::<MgMsg>("knowledge_graph", knowledge_tx.clone());
        let _ = registry.register::<MgMsg>("memory_graph", memory_graph_tx.clone());
        // The same sender the coordinator is handed below: the record a run writes is only the
        // record this test can read if it is the same store.
        let project_graph = memory_graph_tx.clone();
        let (rag_tx, _) = system.spawn(RagActor::from_registry(
            knowledge_tx,
            memory_graph_tx,
            registry.clone(),
        ));
        // Registered under `"rag"`, as `ffi.rs` does: that entry is the *only* way the coordinator's
        // retrieval seam can reach the actor, so a test that skipped it would pass by retrieving
        // nothing — which is exactly the failure mode these tests exist to catch.
        let _ = registry.register::<RagMessage>("rag", rag_tx.clone());
        let (reply, rx) = tokio::sync::oneshot::channel();
        rag_tx
            .send(RagMessage::IngestGraphConfig {
                manifest_path: store
                    .join(rag_bundle::DEVICE_FACTS_CORPUS)
                    .join("ingest.yaml"),
                project_root: None,
                reply_to: reply,
            })
            .await
            .expect("send IngestGraphConfig");
        let report = rx.await.expect("reply").expect("ingest");
        assert!(
            report.chunks > 0,
            "the device-facts corpus ingested nothing, so nothing below could mean anything: {report:?}"
        );

        // The model: scripted as "no file needs to change" — the prompt is the subject.
        let log = Arc::new(Mutex::new(Vec::new()));
        let url = fake_llm_logging(vec!["NONE".to_string()], log.clone());

        let (chat_tx, _) = system.spawn(ChatActor::new());
        let (tools_tx, _) = system.spawn(ToolsActor::new(mock_sender()));
        let (mcp_tx, _) = system.spawn(McpClientActor::new());
        let (llm_tx, _) = system.spawn(LlmActor::new(LlmConfig {
            api_url: url,
            // The actor refuses a request with no key and never checks it against anything.
            api_key: "test-key".to_string(),
            ..LlmConfig::default()
        }));
        let (system_tx, _) = system.spawn(SystemActor::new());
        let (coord, _) = system.spawn(CoordinatorActor::new(
            chat_tx,
            tools_tx,
            mcp_tx,
            llm_tx,
            system_tx,
            // The **real** project graph, not a mock: a component edit writes its record here, and a
            // fixture that mocked it out would make the record unobservable — the write would fail
            // silently, by design, and the test would pass while asserting nothing.
            project_graph.clone(),
            mock_sender(),
            mock_sender(),
            mock_sender(),
            mock_sender(),
            mock_sender(),
        ));

        // The app-only half: `SetFfiDeps` is what gives the router the registry the RAG actor is
        // registered in — which is the seam this test exists for.
        let (watcher_tx, _watcher_rx) =
            mpsc::channel::<spire_code::actors::FileChangeNotification>(8);
        coord
            .send(CoordinatorMessage::SetFfiDeps {
                registry: registry.clone(),
                state: Arc::new(FfiSharedState {
                    project_root: Mutex::new(None),
                    analysis: Mutex::new(None),
                    watcher_out_tx: watcher_tx,
                }),
            })
            .await
            .expect("send SetFfiDeps");

        Some(Self {
            root,
            coord,
            graph: project_graph,
            log,
            _tmp: (root_tmp, store_tmp),
        })
    }

    /// Ask for the component to be written, as the UI does, and return the RPC result together with
    /// every body the model was sent.
    async fn edit(&self, extra: serde_json::Value) -> (serde_json::Value, Vec<String>) {
        let mut args = serde_json::json!({
            "root": self.root.to_string_lossy(),
            "name": "sht20",
            "instruction": "Read temperature and humidity from the SHT20.",
        });
        for (key, value) in extra.as_object().expect("an object") {
            args[key] = value.clone();
        }
        self.log.lock().expect("log").clear();

        let (reply, rx) = tokio::sync::oneshot::channel();
        self.coord
            .send(CoordinatorMessage::HandleRequest {
                method: "tools/call".to_string(),
                params: serde_json::json!({ "tool": "idf_component_edit", "args": args }),
                response_tx: reply,
            })
            .await
            .expect("send HandleRequest");
        let result = rx.await.expect("reply");
        let bodies = self.log.lock().expect("log").clone();
        (result, bodies)
    }
}

/// The prompt a `sht20` edit sends carries the **part's own protocol**, under the heading that says
/// so — and not the one that means "somebody else's driver".
///
/// This is the whole reason the seam asks two corpora: `esp-idf-lib` carries `sht3x` and no `sht20`,
/// so an edit that took its commands from a retrieved driver would write a plausible, *wrong*
/// protocol, and nothing in the old prompt said so. `0xF3` (SHT2x's no-hold temperature command) and
/// `0x988000` (the CRC divisor from the document's last section) are in `sht20.md` and in no other
/// bundled document: the first says the part's own bytes arrived, the second that the *whole*
/// document did rather than a truncated half.
///
/// `device-facts` is small enough that `top_k = 3` also returns `sht30.md` — the near miss this corpus
/// carries on purpose — so what the prompt shows is two labelled chunks and the part's own has to
/// come **first**: a model reads the first block as the answer to "what are this part's bytes".
#[tokio::test]
async fn the_component_edit_prompt_carries_the_parts_own_protocol() {
    if !gate_is_runnable() {
        eprintln!(
            "cmake/ctest are not on PATH — the component gate cannot run, so this is skipped"
        );
        return;
    }
    let Some(seam) = Seam::build().await else {
        return;
    };
    let (result, bodies) = seam.edit(serde_json::json!({})).await;

    // A component edit rewrites its **whole scope** — it never asks "which file", because the scope
    // is the answer — so the model is asked once per file: the header, the source, and the test. A
    // subset of a component's interdependent files cannot agree with itself, which is why the scope
    // is asked for whole. The scripted reply is `NONE`, which is not a file, so each rewrite also
    // fails the structural check and is retried; what matters here is that the **request** reached
    // every ask.
    assert!(
        bodies.len() >= 3,
        "every file in the component's scope must be put to the model: {bodies:#?}"
    );
    for file in ["sht20.hpp", "sht20.cpp", "sht20_test.cpp"] {
        assert!(
            bodies.iter().any(|b| b.contains(file)),
            "{file} is in the component's scope and was never rewritten: {bodies:#?}"
        );
    }
    // The request is the same in every ask — one contract, three files — so the facts are read off
    // the first, which `plan_rewrites` orders to be the header: a contract the rest are written
    // against. Facts missing here were missing everywhere.
    let prompt = &bodies[0];
    assert!(
        prompt.contains(REFERENCE_BLOCK),
        "the retrieved material never reached the prompt: {prompt}"
    );
    assert!(
        prompt.contains(FACTS_HEADING),
        "the device's own protocol was not labelled as such, so a model cannot tell its bytes from a \
         comparable driver's: {prompt}"
    );
    assert!(
        prompt.contains("0xF3"),
        "the prompt does not carry the part's own command word, so the store answered with something \
         else: {prompt}"
    );
    assert!(
        prompt.contains("0x988000"),
        "the prompt carries part of the facts document but not its last section, so a model would be \
         writing from half a protocol: {prompt}"
    );
    assert!(
        !prompt.contains(PRECEDENT_HEADING),
        "`esp-idf-lib` is not ingested in this store, so a precedent section would be a heading with \
         nothing under it — which reads as \"the driver is: nothing\": {prompt}"
    );
    assert!(
        !prompt.contains("(`esp-idf-lib`)"),
        "the corpus an empty section would have come from must not appear as a heading either: \
         {prompt}"
    );
    // Both documents are retrieved (see the doc comment); the part's own must rank first.
    let own = prompt
        .find("sht20.md")
        .expect("the part's own document is in the prompt");
    let neighbour = prompt
        .find("sht30.md")
        .expect("the near miss is retrieved too, and is the point of the corpus");
    assert!(
        own < neighbour,
        "the near miss is presented *before* the part's own document, so the first thing the model \
         reads is another device's commands: {prompt}"
    );
    assert!(
        result["reference"]
            .as_str()
            .unwrap_or_default()
            .contains("device-facts"),
        "the report has to name which corpus answered, or a wrong answer and a silent miss look alike \
         to the caller: {result}"
    );
}

/// A corpus named as an **override** replaces the pair — and when it cannot answer, the prompt says
/// nothing at all rather than showing an empty section.
///
/// The distinction is load-bearing: "the store holds nothing about this device" and "the store holds
/// an empty heading for it" read the same to a model, and the second invites a driver written from a
/// label. The corpus named here is deliberately one that is **not** ingested.
#[tokio::test]
async fn a_domain_that_cannot_answer_leaves_no_trace_in_the_prompt() {
    if !gate_is_runnable() {
        eprintln!(
            "cmake/ctest are not on PATH — the component gate cannot run, so this is skipped"
        );
        return;
    }
    let Some(seam) = Seam::build().await else {
        return;
    };
    let (result, bodies) = seam
        .edit(serde_json::json!({ "domain": "esp-idf-lib" }))
        .await;

    assert!(!bodies.is_empty(), "no request reached the model: {result}");
    for body in &bodies {
        assert!(
            !body.contains(REFERENCE_BLOCK),
            "the override asked `esp-idf-lib`, which answered nothing — and the prompt still shows a \
             reference section, so the model is handed an empty label instead of the absence of \
             facts: {body}"
        );
        assert!(
            !body.contains("device-facts"),
            "a `domain` override must ask that corpus alone: {body}"
        );
    }
    assert!(
        result["reference"]
            .as_str()
            .unwrap_or_default()
            .starts_with("none:"),
        "nothing was retrieved, and the report has to say so rather than leave it to be inferred from \
         the output: {result}"
    );
}

// ============================================================================
// Reading the record back out of the project graph
// ============================================================================

/// The edit records the project graph holds for `component`, read through the same
/// `QueryAttrNodesWhere` a caller would use — so the assertion is about what is *queryable*, not
/// about what some private accessor happens to return.
async fn recorded_edits(graph: &mpsc::Sender<MgMsg>, component: &str) -> Vec<AttrNode> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    graph
        .send(MgMsg::QueryAttrNodesWhere {
            subtype: Some(edit_history::EDIT_SUBTYPE.to_string()),
            props: vec![("component".to_string(), component.to_string())],
            limit: Some(16),
            reply_to: reply,
        })
        .await
        .expect("send QueryAttrNodesWhere");
    rx.await.expect("reply").expect("query the records")
}

/// The component's `Module` spine, looked up by the name the writer scopes it with —
/// `{library}::{component}`, where the library is the root's own directory name.
async fn module_spine(
    graph: &mpsc::Sender<MgMsg>,
    root: &std::path::Path,
    component: &str,
) -> Vec<AttrNode> {
    let library = edit_history::library_name(&root.to_string_lossy());
    let (reply, rx) = tokio::sync::oneshot::channel();
    graph
        .send(MgMsg::QueryAttrNodes {
            node_type: Some(edit_history::MODULE_TYPE.to_string()),
            subtype: Some(edit_history::MODULE_SUBTYPE.to_string()),
            name: Some(format!("{library}::{component}")),
            limit: Some(1),
            reply_to: reply,
        })
        .await
        .expect("send QueryAttrNodes");
    rx.await.expect("reply").expect("query the spine")
}

/// The edges of one type leaving `node_id`.
async fn edges_of(
    graph: &mpsc::Sender<MgMsg>,
    node_id: &str,
    edge_type: RelationshipType,
) -> Vec<GraphEdge> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    graph
        .send(MgMsg::GetRelationships {
            node_id: node_id.to_string(),
            reply_to: reply,
        })
        .await
        .expect("send GetRelationships");
    rx.await
        .expect("reply")
        .expect("get the relationships")
        .into_iter()
        .filter(|edge| edge.edge_type == edge_type)
        .collect()
}

/// **A run is written down, with the instruction that caused it.**
///
/// The report says what *happened*; only the request says what was *asked*, and what was asked is
/// what answers "why does this file look like this" once the transcript is gone. So the
/// `idf_component_edit` RPC has to leave the instruction in the project graph, under the component it
/// was about, reachable from that component's own node — otherwise the record is a note with no
/// filing system, and there is no way to ask one component what has been tried on it.
///
/// The model here answers `NONE`, so this run is **rejected** by the structural check — and the
/// record is written anyway. That is deliberate: "we asked for this and it did not pass the gate" is
/// exactly the kind of history worth keeping, so recording is unconditional and best-effort rather
/// than gated on `report.success`.
#[tokio::test]
async fn a_component_edit_leaves_its_instruction_in_the_project_graph() {
    if !gate_is_runnable() {
        eprintln!(
            "no cmake/ctest — `idf_component_edit` would refuse before the model is asked, so \
             there is no run to record and this test is skipped"
        );
        return;
    }
    let Some(seam) = Seam::build().await else {
        return;
    };
    let instruction = "Read temperature and humidity from the SHT20.";

    let (result, bodies) = seam
        .edit(serde_json::json!({ "instruction": instruction }))
        .await;
    assert!(
        !bodies.is_empty(),
        "no request reached the model, so nothing below could mean anything: {result}"
    );

    // Nothing had been recorded before this run, so the history the response carries is empty: the
    // record this run just wrote must not be echoed back as if it were its own past.
    assert_eq!(
        result["history"],
        serde_json::json!([]),
        "the first run has no prior history: {result:#?}"
    );

    // The record is in the graph, keyed by the component, carrying the instruction verbatim.
    let records = recorded_edits(&seam.graph, "sht20").await;
    assert_eq!(records.len(), 1, "one run, one record: {records:#?}");
    assert_eq!(
        records[0].properties["instruction"],
        serde_json::json!(instruction),
        "the record has to carry what was asked, word for word: {records:#?}"
    );
    assert_eq!(
        records[0].properties["success"],
        serde_json::json!(result["success"]),
        "the record and the report have to agree on the verdict: {records:#?}"
    );

    let library = edit_history::library_name(&seam.root.to_string_lossy());
    assert!(
        records[0].name.contains("sht20") && records[0].name.contains(&library),
        "a record is named under its component and library, so two libraries' `sht20`s cannot \
         collide: {}",
        records[0].name
    );

    // ... and it hangs off the component's own node, which is what makes "this component's history"
    // one traversal rather than a scan of every edit in the project.
    let spine = module_spine(&seam.graph, &seam.root, "sht20").await;
    assert_eq!(spine.len(), 1, "one spine per component: {spine:#?}");
    let edges = edges_of(&seam.graph, &spine[0].id, RelationshipType::HasEdit).await;
    assert_eq!(edges.len(), 1, "the module points at the run: {edges:#?}");
    assert_eq!(edges[0].to_id, records[0].id, "{edges:#?}");
}
