// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **bundled RAG manifests** parse and are scopable.
//!
//! These files are the only place the embedded corpora are described, and they ship with the binary
//! (`include_str!` in the coordinator's `rag/install-bundle-manifests`), so a typo in one is a corpus
//! that silently ingests nothing — or ingests everything. This pins the parts a typo would break:
//! that each file is a valid `GraphRagConfig`, that it names a corpus and at least one enabled
//! source, and that its include patterns are written the way the matcher needs them.
//!
//! The last one is the trap worth a test: `path_matches` matches `include_paths` against the
//! **absolute** path inside a clone, so a pattern like `src/**/*.rs` can never match. Every directory
//! pattern therefore has to carry its `**/` prefix — a rule the spire-core manifest documents in
//! prose, and this one enforces.

use spire_core::actors::rag_ingest::GraphRagConfig;
// The same list the installer writes: a manifest added to the bundle is covered here without anyone
// remembering to add it twice, and one removed from it fails the "embedded corpora are in the
// bundle" test below.
use spire_code::actors::rag_bundle::BUNDLE;

/// Every bundled manifest, as `(corpus, the file's text)`.
///
/// `include_str!` rather than a directory scan: the point is to check exactly what the binary ships,
/// and a file that is in the directory but not in the installer is not shipped at all.

/// The source types `rag_ingest::fetch_source_files` implements. A manifest naming anything else
/// would fail at ingest time with `unsupported source type`, which is a long way from the typo.
const SUPPORTED_TYPES: &[&str] = &[
    "local",
    "markdown",
    "text",
    "code",
    "pdf",
    "pdf_extraction",
    "github_repo",
    "github_org",
    "web_page",
    "mailing_list",
];

#[test]
fn every_bundled_manifest_parses_and_names_a_corpus_with_an_enabled_source() {
    for (expected_corpus, yaml) in BUNDLE {
        let config: GraphRagConfig = serde_yaml::from_str(yaml)
            .unwrap_or_else(|e| panic!("{expected_corpus}.ingest.yaml does not parse: {e}"));

        assert_eq!(
            config.pipeline.corpus, *expected_corpus,
            "the corpus is what the installer's directory name says, and what retrieval scopes to"
        );
        assert!(
            !config.pipeline.name.trim().is_empty(),
            "{expected_corpus}: the pipeline needs a human name — it is what RagView lists"
        );
        assert!(
            !config.pipeline.domains.is_empty(),
            "{expected_corpus}: a domain is what makes the corpus retrievable"
        );

        let enabled: Vec<&str> = config
            .pipeline
            .sources
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.id.as_str())
            .collect();
        assert!(
            !enabled.is_empty(),
            "{expected_corpus}: every source is disabled, so ingesting it would do nothing"
        );

        for source in config.pipeline.sources.iter().filter(|s| s.enabled) {
            assert!(
                SUPPORTED_TYPES.contains(&source.source_type.as_str()),
                "{expected_corpus}/{}: unsupported source type '{}'",
                source.id,
                source.source_type
            );
            // A fetchable source needs its locator; a local one needs a path.
            if matches!(source.source_type.as_str(), "github_repo" | "web_page") {
                assert!(
                    !source.url.trim().is_empty(),
                    "{expected_corpus}/{}: a {} needs a url",
                    source.id,
                    source.source_type
                );
            }
            if source.source_type == "local" {
                assert!(
                    !source.path.trim().is_empty(),
                    "{expected_corpus}/{}: a local source needs a path",
                    source.id
                );
            }
        }
    }
}

/// An **include** pattern must be written to match an absolute path, or the corpus is empty.
///
/// `include_paths` is matched against the full path of each file in the clone
/// (`/…/esp-idf-hal/src/gpio.rs`), so a pattern without a `**/` prefix anchors at the start of that
/// path and matches nothing — and the failure looks like an empty corpus, not an error. Bare file
/// *names* are the one exception, because `include_files` is matched against the basename too.
///
/// `exclude_paths` is deliberately **not** checked the same way: a pattern that cannot match excludes
/// nothing, which is harmless (the spire-core manifest carries a redundant `target/**` next to the
/// `**/target/**` that does the work). The asymmetry is the point — one direction loses the corpus,
/// the other merely fails to trim it.
#[test]
fn include_patterns_are_written_to_match_absolute_paths() {
    for (corpus, yaml) in BUNDLE {
        let config: GraphRagConfig = serde_yaml::from_str(yaml).expect("parses");
        for source in config.pipeline.sources.iter().filter(|s| s.enabled) {
            for pattern in source.processing.include_paths.iter() {
                assert!(
                    pattern.starts_with("**/"),
                    "{corpus}/{}: include pattern '{pattern}' cannot match an absolute path — \
                     prefix it with `**/`, or the corpus ingests nothing",
                    source.id
                );
            }
            // An include *file* is the documented bare-name form; it may not be a glob at all.
            for pattern in source.processing.include_files.iter() {
                assert!(
                    pattern.starts_with("**/") || !pattern.contains('/'),
                    "{corpus}/{}: include file '{pattern}' is neither a bare name nor absolute-safe",
                    source.id
                );
            }
        }
    }
}

/// The patterns must match the files they claim to — **at every depth**.
///
/// `glob_to_regex` turns `**` into `.*` and `*` into `[^/]*`, so a pattern whose `**` is *followed by
/// another path segment* needs a directory at that position: `**/src/**/*.rs` matches
/// `src/peripherals/gpio.rs` and **not** `src/gpio.rs`. That is how the esp-idf-hal corpus first came
/// to hold 16 files instead of a hundred — nothing failed, the corpus was simply mostly missing, and a
/// chunk count cannot tell the two apart. This pins each manifest against the shape of the tree it
/// points at, which is the only place the difference is visible.
#[test]
fn include_patterns_match_the_real_files_and_not_the_neighbours() {
    // (corpus, a path in that source's tree, whether the source claims it)
    let cases: &[(&str, &str, bool)] = &[
        // A book: chapters at the top of src/, more in subdirectories, artwork beside them.
        ("esp-rs-book", "/c/esp-rs-book/src/index.md", true),
        ("esp-rs-book", "/c/esp-rs-book/src/advanced/hal.md", true),
        ("esp-rs-book", "/c/esp-rs-book/README.md", true),
        ("esp-rs-book", "/c/esp-rs-book/src/images/board.png", false),
        // A HAL: the API at the top of src/ and in subdirectories, plus examples.
        ("esp-idf-hal", "/c/esp-idf-hal/src/gpio.rs", true),
        ("esp-idf-hal", "/c/esp-idf-hal/src/pcnt/channel.rs", true),
        ("esp-idf-hal", "/c/esp-idf-hal/examples/blink.rs", true),
        ("esp-idf-hal", "/c/esp-idf-hal/examples/i2c/scan.rs", true),
        ("esp-idf-hal", "/c/esp-idf-hal/tests/hil.rs", false),
        // SDK docs: English only, at both depths, and not the C sources.
        ("esp-idf", "/c/esp-idf/docs/en/index.rst", true),
        (
            "esp-idf",
            "/c/esp-idf/docs/en/api-guides/tools/idf-monitor.rst",
            true,
        ),
        ("esp-idf", "/c/esp-idf/docs/zh_CN/index.rst", false),
        (
            "esp-idf",
            "/c/esp-idf/components/esp_wifi/src/wifi.c",
            false,
        ),
        // Std: the three crates' sources at both depths; neither the sibling crates
        // nor the crates' own tests.
        (
            "rust",
            "/t/lib/rustlib/src/rust/library/core/src/lib.rs",
            true,
        ),
        (
            "rust",
            "/t/lib/rustlib/src/rust/library/core/src/num/mod.rs",
            true,
        ),
        (
            "rust",
            "/t/lib/rustlib/src/rust/library/std/src/sys/pal/unix/thread.rs",
            true,
        ),
        (
            "rust",
            "/t/lib/rustlib/src/rust/library/portable-simd/crates/core_simd/src/lib.rs",
            false,
        ),
        (
            "rust",
            "/t/lib/rustlib/src/rust/library/core/tests/ops.rs",
            false,
        ),
    ];

    for (corpus, path, expected) in cases {
        let yaml = BUNDLE
            .iter()
            .find(|(c, _)| c == corpus)
            .map(|(_, y)| *y)
            .unwrap_or_else(|| panic!("{corpus} is not in the bundle"));
        let config: GraphRagConfig = serde_yaml::from_str(yaml).expect("parses");
        let source = config
            .pipeline
            .sources
            .iter()
            .find(|s| s.enabled)
            .expect("an enabled source");
        let matched =
            spire_core::actors::rag_ingest::path_matches(std::path::Path::new(path), source);
        assert_eq!(
            matched,
            *expected,
            "{corpus}: {path} — {} by the manifest: {:#?}",
            if matched { "claimed" } else { "not claimed" },
            source.processing.include_paths
        );
    }
}

/// The embedded corpora are the ones a generated firmware crate is written against, so their
/// presence is worth stating on its own: losing one to a rename would otherwise look like
/// "retrieval is bad" rather than like a missing corpus.
#[test]
fn the_embedded_corpora_are_in_the_bundle() {
    let corpora: Vec<&str> = BUNDLE.iter().map(|(corpus, _)| *corpus).collect();
    for expected in ["esp-rs-book", "esp-idf-hal", "esp-idf", "rust"] {
        assert!(
            corpora.contains(&expected),
            "'{expected}' is missing from the bundle: {corpora:?}"
        );
    }
}
