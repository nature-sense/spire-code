// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **live fill**: install the bundled corpora and ingest them into the KnowledgeStore.
//!
//! This is the step that turns a manifest into retrievable knowledge, and it is the one that has to
//! touch the network (three corpora clone from GitHub) and a real embedder, so it is `#[ignore]`d and
//! run deliberately:
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

    // Built inside the runtime on purpose: the model is cached on this machine, so no Hugging Face
    // round trip happens and hf-hub's blocking client is never reached. (ffi.rs builds it *outside*
    // the runtime because a first-ever download would panic otherwise; if this test ever panics with
    // "Cannot start a runtime from within a runtime", that is what changed.)
    let embedder: Arc<dyn Embedder> = match spire_core::embedder::CandleEmbedder::new() {
        Ok(e) => Arc::new(e),
        Err(e) => {
            eprintln!("no Candle embedder ({e}) — the corpus would be lexical-only");
            Arc::new(spire_core::embedder::NoopEmbedder)
        }
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
