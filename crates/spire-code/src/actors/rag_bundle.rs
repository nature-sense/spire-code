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
//! generated firmware crate is written against — the esp-rs book, `esp-idf-hal`, ESP-IDF and its
//! documentation, the Rust std the board toolchain compiles, and the **library** corpora a hardware
//! task starts from: `esp-idf-lib` (ninety-odd device drivers), `esp-bsp` (board support packages,
//! M5Stack's among them), `m5unified` (M5Stack's own board HAL), `waveshare` (Waveshare's BSPs and
//! display drivers), and `esp-dl` (the on-device inference runtime, its prebuilt models and the
//! examples that drive them — the one corpus an application that runs a *model* cannot do without).
//!
//! `device-facts` is a third thing, and the only corpus here that is neither this application nor
//! somebody else's code: one document per **part number**, stating that part's protocol — command
//! words, framing, checksum, malformed replies — with its provenance. The library corpora answer
//! "has somebody written this device, and how is it driven"; they cannot answer "what are the bytes
//! for *this* part", because a device the library does not carry is not in it (there is no `sht20`
//! in `esp-idf-lib`, only `sht3x` — a different device with different commands) and because the
//! bytes live in the `components/*/*.c` files those manifests exclude. So the two are asked
//! **together**, and the seam labels which section is which: the facts for the bytes, `esp-idf-lib`
//! for the shape.
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
        "esp-idf-lib",
        include_str!("../../resources/rag-ingest/esp-idf-lib.ingest.yaml"),
    ),
    (
        "esp-bsp",
        include_str!("../../resources/rag-ingest/esp-bsp.ingest.yaml"),
    ),
    (
        "m5unified",
        include_str!("../../resources/rag-ingest/m5unified.ingest.yaml"),
    ),
    (
        "esp-dl",
        include_str!("../../resources/rag-ingest/esp-dl.ingest.yaml"),
    ),
    (
        "waveshare",
        include_str!("../../resources/rag-ingest/waveshare.ingest.yaml"),
    ),
    (
        "rust",
        include_str!("../../resources/rag-ingest/rust.ingest.yaml"),
    ),
    (
        DEVICE_FACTS_CORPUS,
        include_str!("../../resources/rag-ingest/device-facts.ingest.yaml"),
    ),
];

/// The corpus whose documents ship beside its manifest, instead of being fetched at ingest time.
///
/// The directory name the installer uses, so the manifest's relative `path: docs` resolves against
/// the manifest's own directory — see [`install_into`].
pub const DEVICE_FACTS_CORPUS: &str = "device-facts";

/// The documents the `device-facts` corpus ships, as `(file name, the text)`, installed beside its
/// manifest as `<store>/device-facts/docs/<file name>`.
///
/// An explicit list, and `include_str!`, for the reason [`BUNDLE`] gives: a document added to
/// `resources/device-facts/` without an entry here is **not shipped**, so what the corpus can answer
/// is reviewable in the source tree rather than assembled at install time. The manifest globs the
/// installed directory (`**/*.md`) rather than naming these files, which is only safe because
/// *this* list is what decides what reaches that directory — the gate and the glob cannot disagree
/// about which documents are part of the corpus.
///
/// The file name is the **part number** the document answers for, because that is the key the
/// corpus is asked with: a component's edit looks its device up as `sht20`, and a document named
/// `sht20.md` is what makes that a keyed lookup rather than a similarity search.
///
/// More than one document ships, and the ones that do are deliberately **near misses** of each
/// other: `sht20.md` and `sht30.md` are one family measuring without clock stretching with the same
/// CRC polynomial, and they disagree about the address, the command words, the framing and the
/// checksum's seed. A corpus of unrelated documents would be answered correctly by accident and
/// prove nothing; a near-miss pair is what the key has to separate, and
/// `tests/rag_fill_tests.rs` pins that a query for one part answers with that part's document rather
/// than its neighbour's — by a small margin, which is exactly why the seam labels every chunk with
/// its source path rather than trusting the ranking alone.
pub const FACTS_DOCS: &[(&str, &str)] = &[
    (
        "sht20.md",
        include_str!("../../resources/device-facts/sht20.md"),
    ),
    (
        "sht30.md",
        include_str!("../../resources/device-facts/sht30.md"),
    ),
];

/// Write the bundle into `dir`, as `<corpus>/ingest.yaml`, and return what was written.
///
/// The KnowledgeStore's scan reads that layout, so this is the step that makes a corpus *offered*
/// (RagView lists the manifest) rather than ingested — the ingest itself is a separate, expensive
/// call. Shared by `rag/install-bundle-manifests` and the live fill test so the two cannot disagree
/// about where a manifest goes.
///
/// [`FACTS_DOCS`] are written too, as `<store>/device-facts/docs/<name>`, because that corpus has no
/// repository to fetch them from: its manifest points at `docs` **relative to its own directory**,
/// so installing the manifest is what puts the documents where the ingest will look for them. A
/// store filled by an older binary therefore gains the corpus by re-installing, and its ingest is
/// the same deliberate step every other corpus needs.
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
    for (name, content) in FACTS_DOCS {
        let target = dir.join(DEVICE_FACTS_CORPUS).join("docs").join(name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
        }
        std::fs::write(&target, content).map_err(|e| format!("write {name}: {e}"))?;
    }
    Ok(installed)
}
