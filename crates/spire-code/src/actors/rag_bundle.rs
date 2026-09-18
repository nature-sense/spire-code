// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **bundled RAG manifests** — the corpora this binary can install.
//!
//! One list, in one place, because three things read it and none of them may drift:
//!
//! * `rag/install-bundle-manifests` writes each manifest into the KnowledgeStore's scan dirs
//!   (`~/.spire/knowledge/<corpus>/ingest.yaml`), which is what makes RagView offer it;
//! * `tests/rag_bundle_manifests.rs` parses each one, so a typo in a YAML fails the suite rather
//!   than silently ingesting an empty corpus;
//! * the live fill test ingests them, so "the bundle" means the same set everywhere.
//!
//! Two families, and the split is worth keeping in mind when reading a corpus's retrieval quality:
//! the **Spire** corpora describe how this application works, while the **embedded** ones are what a
//! generated firmware crate is written against (the esp-rs book, `esp-idf-hal`, ESP-IDF, and the Rust
//! std the board toolchain compiles).
//!
//! `include_str!` rather than a directory scan: the point is to ship exactly these files, and a file
//! that sits in the directory without an entry here is not shipped at all.

use std::path::Path;

/// Each bundled manifest as `(corpus, the file's text)`.
///
/// The corpus is also the directory name the installer uses — `RagManifestInfo::domain` is resolved
/// from the manifest itself, and keeping the two equal is what makes
/// `~/.spire/knowledge/<corpus>/ingest.yaml` discoverable by the scan.
pub const BUNDLE: &[(&str, &str)] = &[
    (
        "spire-core",
        include_str!("../../resources/rag-ingest/spire-core.ingest.yaml"),
    ),
    (
        "spire-actor",
        include_str!("../../resources/rag-ingest/spire-actor.ingest.yaml"),
    ),
    (
        "esp-rs-book",
        include_str!("../../resources/rag-ingest/esp-rs-book.ingest.yaml"),
    ),
    (
        "esp-idf-hal",
        include_str!("../../resources/rag-ingest/esp-idf-hal.ingest.yaml"),
    ),
    (
        "esp-idf",
        include_str!("../../resources/rag-ingest/esp-idf.ingest.yaml"),
    ),
    (
        "rust",
        include_str!("../../resources/rag-ingest/rust.ingest.yaml"),
    ),
];

/// Write the bundle into `dir`, as `<corpus>/ingest.yaml`, and return what was written.
///
/// The KnowledgeStore's scan reads that layout, so this is the step that makes a corpus *offered*
/// (RagView lists the manifest) rather than ingested — the ingest itself is a separate, expensive
/// call. Shared by `rag/install-bundle-manifests` and the live fill test so the two cannot disagree
/// about where a manifest goes.
pub fn install_into(dir: &Path) -> Result<Vec<String>, String> {
    let mut installed = Vec::new();
    for (name, content) in BUNDLE {
        let target = dir.join(name).join("ingest.yaml");
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        std::fs::write(&target, content).map_err(|e| format!("write {name}: {e}"))?;
        installed.push((*name).to_string());
    }
    Ok(installed)
}
