// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **live fill**: install the bundled corpora and ingest them into the KnowledgeStore.
//!
//! This is the step that turns a manifest into retrievable knowledge, and it is the one that has to
//! touch the network — nearly every corpus clones from GitHub (`rust` and `device-facts` are the
//! local sources, and the device/board libraries are the largest clones) — and a real embedder, so
//! it is `#[ignore]`d and run deliberately:
//!
//! ```sh
//! # every corpus, into ~/.spire/knowledge
//! cargo test -p spire-code --test rag_fill_tests -- --ignored --nocapture
//!
//! # just one, when refreshing it
//! SPIRE_FILL_CORPUS=esp-idf-hal cargo test -p spire-code --test rag_fill_tests -- --ignored --nocapture
//! ```
//!
//! It writes to the **user-level** store (`~/.spire/knowledge`, or `$SPIRE_KNOWLEDGE_DIR`), the same
//! one the app reads — which is the point: the RAG the app searches is this store. Because both would
//! then be writing it, **quit the app first** (one writer at a time).
//!
//! The wiring below is the app's, in miniature: a KnowledgeStore graph at `knowledge_dir`, a second
//! graph for provenance edges, an embedder registered as a service, and a `RagActor` built from the
//! registry. Deliberately the same shape as `ffi.rs` — a fill that worked here and not in the app
//! would be worse than no test.

use spire_actor::registry::ServiceRegistry;
use spire_actor::ActorSystem;
use spire_code::actors::rag_bundle;
use spire_core::actors::rag::{EmbedderService, RagActor, RagMessage};
use spire_core::actors::MemoryGraphMessage as MgMsg;
use spire_core::models::embedding::Embedder;
use spire_core::subsystems::graph::memory_graph::MemoryGraphActor;
use std::sync::Arc;

/// Which corpora to ingest: all of them, or the ones named in `SPIRE_FILL_CORPUS` (comma-separated)
/// so one can be refreshed without re-cloning the others.
fn corpora() -> Vec<String> {
    match std::env::var("SPIRE_FILL_CORPUS") {
        Ok(list) if !list.trim().is_empty() => list
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect(),
        _ => rag_bundle::BUNDLE
            .iter()
            .map(|(c, _)| (*c).to_string())
            .collect(),
    }
}

/// Initialize a graph actor at `dir` — the two calls the app makes at startup.
async fn init_graph(tx: &tokio::sync::mpsc::Sender<MgMsg>, dir: &std::path::Path) {
    let (t, r) = tokio::sync::oneshot::channel();
    tx.send(MgMsg::Initialize {
        data_dir: dir.to_path_buf(),
        reply_to: t,
    })
    .await
    .expect("send Initialize");
    r.await.expect("Initialize reply").expect("Initialize");
}

/// The real embedder, or `None` with a line saying why — in which case the caller must **return**,
/// not substitute `NoopEmbedder`.
///
/// `NoopEmbedder` is not a slower embedder: it fails every call on purpose, so that RAG surfaces a
/// missing model instead of degrading to zero vectors. A store therefore cannot even be *ingested*
/// without one — the ingest embeds each chunk — which is why there is no lexical-only fallback to
/// fall back to and a machine without the model gets a skip rather than an assertion about a store
/// that was never filled.
///
/// Built **inside** the runtime deliberately: the model is cached on this machine, so no Hugging Face
/// round trip happens and hf-hub's blocking client is never reached. (`ffi.rs` builds it *outside* the
/// runtime because a first-ever download would panic otherwise; if this ever panics with "Cannot start
/// a runtime from within a runtime", that is what changed.)
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

#[ignore = "live fill: clones from GitHub (or reads the local SDK) and embeds — minutes"]
#[tokio::test]
async fn the_bundled_corpora_ingest_into_the_knowledge_store() {
    let store = spire_core::config::knowledge_dir();
    std::fs::create_dir_all(&store).expect("create knowledge dir");
    println!("KnowledgeStore: {}", store.display());

    // 1. Install: the manifests, as `<corpus>/ingest.yaml`, which is what RagView lists.
    let installed = rag_bundle::install_into(&store).expect("install the bundle");
    println!("installed: {installed:?}");

    // 2. The app's RAG wiring.
    let system = ActorSystem::new();
    let (memory_graph_tx, _) = system.spawn(MemoryGraphActor::new());
    let (knowledge_tx, _) = system.spawn(MemoryGraphActor::new());
    // The project graph is only a provenance sink here; the corpus lives in the store.
    let project_data = tempfile::tempdir().expect("project data dir");
    init_graph(&memory_graph_tx, project_data.path()).await;
    init_graph(&knowledge_tx, &store).await;

    // The real embedder, built inside the runtime — see [`real_embedder`].
    let Some(embedder) = real_embedder() else {
        return;
    };
    let registry = Arc::new(ServiceRegistry::new());
    let _ = registry.register_service("embedder", Arc::new(EmbedderService(embedder.clone())));
    let _ = registry.register::<MgMsg>("knowledge_graph", knowledge_tx.clone());
    let _ = registry.register::<MgMsg>("memory_graph", memory_graph_tx.clone());
    {
        let (t, r) = tokio::sync::oneshot::channel();
        knowledge_tx
            .send(MgMsg::InitializeEmbedder {
                model_path: None,
                embedder: Some(embedder),
                reply_to: t,
            })
            .await
            .expect("send InitializeEmbedder");
        let _ = r.await;
    }
    let (rag_tx, _) = system.spawn(RagActor::from_registry(
        knowledge_tx,
        memory_graph_tx,
        registry.clone(),
    ));

    // 3. Ingest each corpus, and keep the counts so retrieval can be checked against them.
    let mut filled: Vec<(String, u32)> = Vec::new();
    for corpus in corpora() {
        let manifest = store.join(&corpus).join("ingest.yaml");
        assert!(
            manifest.is_file(),
            "{} is not installed — is '{corpus}' spelled as it is in the bundle?",
            manifest.display()
        );
        let (t, r) = tokio::sync::oneshot::channel();
        // Reingest *replaces* the domain (clear, then ingest) rather than merging into it, which is
        // what a corrected manifest needs: merging would leave the chunks that the old patterns
        // matched and the new ones beside them. Set `SPIRE_FILL_REINGEST=1` when a manifest changed.
        if std::env::var("SPIRE_FILL_REINGEST").is_ok() {
            rag_tx
                .send(RagMessage::ReingestGraphConfig {
                    manifest_path: manifest,
                    project_root: None,
                    reply_to: t,
                })
                .await
                .expect("send ReingestGraphConfig");
        } else {
            rag_tx
                .send(RagMessage::IngestGraphConfig {
                    manifest_path: manifest,
                    project_root: None,
                    reply_to: t,
                })
                .await
                .expect("send IngestGraphConfig");
        }
        let report = r.await.expect("reply").expect("ingest");
        for source in &report.sources {
            println!(
                "  {} [{}] {} files, {} chunks{}",
                source.id,
                source.source_type,
                source.files,
                source.chunks,
                if source.status == "ok" {
                    String::new()
                } else {
                    format!(" — {}: {}", source.status, source.reason)
                }
            );
        }
        println!(
            "{corpus}: {} chunks, {} entities, {} relationships (skipped: {:?})",
            report.chunks, report.entities, report.relationships, report.sources_skipped
        );
        assert!(
            report.chunks > 0,
            "{corpus} ingested nothing — a manifest whose patterns match no file looks exactly like \
             this: {report:?}"
        );
        filled.push((corpus, report.chunks));
    }

    // 3b. What the store actually holds, per domain. This is the diagnostic that separates "the
    // ingest did not write" from "the write is not visible to a search": `chunk_count` is counted
    // directly over the graph, while `semantic_retrieve` only scans the first 500 `rag_chunk` nodes
    // it is handed — with the pre-existing corpora in this store, a fresh corpus can sit outside that
    // window and be unfindable however well it was written.
    {
        let (t, r) = tokio::sync::oneshot::channel();
        rag_tx
            .send(RagMessage::ListDomains { reply_to: t })
            .await
            .expect("send ListDomains");
        let mut domains = r.await.expect("reply").expect("list");
        domains.sort_by_key(|d| std::cmp::Reverse(d.chunk_count));
        println!("--- domains in the store ({}):", domains.len());
        for d in domains.iter().take(12) {
            println!(
                "    {:<16} {:>6} chunks  {:>4} sources  {:>4} entities",
                d.id, d.chunk_count, d.source_count, d.entity_count
            );
        }
        println!(
            "    total chunks: {}",
            domains.iter().map(|d| d.chunk_count).sum::<u64>()
        );
    }

    // 4. Retrieval, against the domains just filled — the half a chunk count does not prove.
    for (corpus, _) in &filled {
        let (t, r) = tokio::sync::oneshot::channel();
        rag_tx
            .send(RagMessage::Query {
                domain: corpus.clone(),
                query: "toggle a GPIO output pin".to_string(),
                top_k: 3,
                reply_to: t,
            })
            .await
            .expect("send Query");
        let hits = r.await.expect("reply").expect("query");
        let top = hits.first();
        println!(
            "{corpus}: {} hits, best {:.3} from {}",
            hits.len(),
            top.map(|h| h.score).unwrap_or(0.0),
            top.map(|h| h.source_path.clone())
                .unwrap_or_else(|| "—".to_string())
        );
        assert!(
            !hits.is_empty(),
            "{corpus} answered a query with nothing, so its chunks are not retrievable"
        );
    }
}

/// The `device-facts` corpus' premise, on its own: a **part number** answers with that part.
///
/// That corpus is asked by *key* — one document per part, named for it — and the documents that ship
/// are deliberately close (`sht20.md` and `sht30.md`: one family, both measuring without clock
/// stretching, the same CRC polynomial, and a different address, command set, framing and CRC seed).
/// So this is the property the corpus exists for, and the one a similarity search can fail: asking
/// for one part must return *that* part's document and not its neighbour's.
///
/// Unlike the live fill it is **not** `#[ignore]`d: the corpus is a `local` source, so nothing here
/// touches the network. The embedder, though, has to be the real one — `NoopEmbedder` **fails every
/// call** rather than degrading to zero vectors, so a store cannot even be *ingested* without the
/// model; when it is not on this machine the test says so and returns. That is what makes the
/// assertion below a claim about the corpus as the app will search it.
///
/// **The margin is small, and that is the finding worth knowing.** The two documents are ~90%
/// identical text, so for `sht20` the scores came out 0.049 / 0.047 against 0.093 / 0.051 for
/// `sht30`: the right document wins both times, by a hair in one case. Nothing about that is
/// random — the same model and the same text give the same answer every run — but it does mean a
/// change to either document must be **re-measured** rather than assumed to have kept the order, and
/// it is why the seam labels every chunk with its source path (`#### …/sht20.md`) instead of letting
/// the model infer which of two near-identical protocols is this part's. The scores are printed on
/// every run so the margin is visible rather than inferred from a pass.
#[tokio::test]
async fn a_device_facts_query_answers_with_that_part_and_not_its_neighbour() {
    let Some(embedder) = real_embedder() else {
        return;
    };
    let tmp = tempfile::tempdir().expect("temp dir");
    let store = tmp.path().join("knowledge");
    std::fs::create_dir_all(&store).expect("create store");
    // The installer is what puts each document where the manifest's relative `path` resolves to.
    rag_bundle::install_into(&store).expect("install the bundle");

    let system = ActorSystem::new();
    let (memory_graph_tx, _) = system.spawn(MemoryGraphActor::new());
    let (knowledge_tx, _) = system.spawn(MemoryGraphActor::new());
    let project_data = tmp.path().join("project");
    std::fs::create_dir_all(&project_data).expect("create project data dir");
    init_graph(&memory_graph_tx, &project_data).await;
    init_graph(&knowledge_tx, &store).await;

    // Deliberately the real embedder: see the doc comment — retrieval is a vector search, and a
    // store cannot be filled without the model at all.
    {
        let (t, r) = tokio::sync::oneshot::channel();
        knowledge_tx
            .send(MgMsg::InitializeEmbedder {
                model_path: None,
                embedder: Some(embedder.clone()),
                reply_to: t,
            })
            .await
            .expect("send InitializeEmbedder");
        let _ = r.await;
    }
    let registry = Arc::new(ServiceRegistry::new());
    let _ = registry.register_service("embedder", Arc::new(EmbedderService(embedder)));
    let _ = registry.register::<MgMsg>("knowledge_graph", knowledge_tx.clone());
    let _ = registry.register::<MgMsg>("memory_graph", memory_graph_tx.clone());
    let (rag_tx, _) = system.spawn(RagActor::from_registry(
        knowledge_tx,
        memory_graph_tx,
        registry.clone(),
    ));

    let (t, r) = tokio::sync::oneshot::channel();
    rag_tx
        .send(RagMessage::IngestGraphConfig {
            manifest_path: store
                .join(rag_bundle::DEVICE_FACTS_CORPUS)
                .join("ingest.yaml"),
            project_root: None,
            reply_to: t,
        })
        .await
        .expect("send IngestGraphConfig");
    let report = r.await.expect("reply").expect("ingest");
    assert_eq!(
        report.chunks as usize,
        rag_bundle::FACTS_DOCS.len(),
        "one document is one chunk — a split document is half a protocol, which is what the corpus' \
         chunk_size exists to prevent"
    );

    // Each part, and the neighbour that must not answer for it. The byte is what a *retrieved* chunk
    // has to carry to be usable: the command table, not just the framing it shares with the neighbour.
    for (part, neighbour, fact) in [
        ("sht20", "sht30.md", "0xF3"),
        ("sht30", "sht20.md", "0x2400"),
    ] {
        let (t, r) = tokio::sync::oneshot::channel();
        rag_tx
            .send(RagMessage::Query {
                domain: rag_bundle::DEVICE_FACTS_CORPUS.to_string(),
                query: part.to_string(),
                top_k: 3,
                reply_to: t,
            })
            .await
            .expect("send Query");
        let hits = r.await.expect("reply").expect("query");
        let top = hits
            .first()
            .unwrap_or_else(|| panic!("'{part}' answered with nothing"));

        println!(
            "{part}: {} hits — {:#?}",
            hits.len(),
            hits.iter()
                .map(|h| (h.source_path.as_str(), h.score))
                .collect::<Vec<_>>()
        );
        assert!(
            top.source_path.ends_with(&format!("{part}.md")),
            "the key did not select the document: '{part}' was answered by {}",
            top.source_path
        );
        assert!(
            !hits
                .iter()
                .any(|h| h.source_path.ends_with(neighbour) && h.score >= top.score),
            "'{part}' ranked its neighbour ({neighbour}) at least as high: {:#?}",
            hits.iter()
                .map(|h| (h.source_path.clone(), h.score))
                .collect::<Vec<_>>()
        );
        assert!(
            top.text.contains(fact),
            "the chunk retrieved for '{part}' does not carry its own command word ({fact}), so it is \
             not the whole document"
        );
    }
}
