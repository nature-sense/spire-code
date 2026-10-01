// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **bundled RAG manifests** parse and are scopable.
//!
//! These files are the only place the embedded corpora are described, and they ship with the binary
//! (`include_str!` in the coordinator's `rag/install-bundle-manifests`), so a typo in one is a corpus
//! that silently ingests nothing — or ingests everything. The manifests are checked as they ship,
//! `(corpus, the file's text)` read straight out of `include_str!` rather than off a directory scan:
//! a file in the directory but not in the installer is not shipped at all. What this pins are the
//! parts a typo would break — that each file is a valid `GraphRagConfig`, that it names a corpus and
//! at least one enabled source, and that its include patterns are written the way the matcher needs
//! them.
//!
//! The last one is the trap worth a test: `path_matches` matches `include_paths` against the
//! **absolute** path inside a clone, so a pattern like `src/**/*.rs` can never match. Every directory
//! pattern therefore has to carry its `**/` prefix — a rule the spire-core manifest documents in
//! prose, and this one enforces.

use spire_core::actors::rag_ingest::GraphRagConfig;
// `DEVICE_FACTS_CORPUS` and `FACTS_DOCS` are the installer's own values, so the test at the end of this
// file checks the layout, the sizes and the names against what actually ships rather than restating them.
use spire_code::actors::rag_bundle::{BUNDLE, DEVICE_FACTS_CORPUS, FACTS_DOCS};

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
        // A device library: the public header of every driver (a component may hold
        // more than one), the `.eil.yml` catalogue beside it, and the one example that
        // drives it — never the implementation, and never the docs tree.
        (
            "esp-idf-lib",
            "/c/esp-idf-lib/components/bmp280/bmp280.h",
            true,
        ),
        ("esp-idf-lib", "/c/esp-idf-lib/components/color/rgb.h", true),
        (
            "esp-idf-lib",
            "/c/esp-idf-lib/components/bmp280/.eil.yml",
            true,
        ),
        (
            "esp-idf-lib",
            "/c/esp-idf-lib/examples/bme680/default/main/main.c",
            true,
        ),
        ("esp-idf-lib", "/c/esp-idf-lib/README.md", true),
        (
            "esp-idf-lib",
            "/c/esp-idf-lib/components/bmp280/bmp280.c",
            false,
        ),
        (
            "esp-idf-lib",
            "/c/esp-idf-lib/components/bmp280/LICENSE",
            false,
        ),
        ("esp-idf-lib", "/c/esp-idf-lib/CHANGELOG.md", false),
        ("esp-idf-lib", "/c/esp-idf-lib/docs/source/index.rst", false),
        // A board support package: the board's page, its API, its dependency
        // manifest, its headers (at either depth — the M5Stack ones nest), and the
        // bring-up example. Not the sources, not the pictures.
        ("esp-bsp", "/c/esp-bsp/bsp/m5stack_core_s3/README.md", true),
        ("esp-bsp", "/c/esp-bsp/bsp/m5stack_core_s3/API.md", true),
        (
            "esp-bsp",
            "/c/esp-bsp/bsp/m5stack_core_s3/idf_component.yml",
            true,
        ),
        (
            "esp-bsp",
            "/c/esp-bsp/bsp/m5stack_core_s3/include/bsp/m5stack_core_s3.h",
            true,
        ),
        ("esp-bsp", "/c/esp-bsp/examples/display/main/main.c", true),
        ("esp-bsp", "/c/esp-bsp/bsp/m5stack_core_s3/src/bsp.c", false),
        (
            "esp-bsp",
            "/c/esp-bsp/bsp/m5stack_core_s3/CMakeLists.txt",
            false,
        ),
        (
            "esp-bsp",
            "/c/esp-bsp/bsp/m5stack_core_s3/doc/pinout.png",
            false,
        ),
        // A board HAL: the unified API (at both depths — `M5Unified.hpp` beside
        // `utility/IMU_Class.hpp`) and the examples that drive it. Not the
        // implementations, and not the SVG pinouts.
        ("m5unified", "/c/m5unified/src/M5Unified.hpp", true),
        ("m5unified", "/c/m5unified/src/utility/IMU_Class.hpp", true),
        (
            "m5unified",
            "/c/m5unified/examples/Basic/Displays/Displays.ino",
            true,
        ),
        (
            "m5unified",
            "/c/m5unified/examples/Advanced/Mic_FFT/Mic_FFT.ino",
            true,
        ),
        ("m5unified", "/c/m5unified/README.md", true),
        ("m5unified", "/c/m5unified/src/M5Unified.cpp", false),
        ("m5unified", "/c/m5unified/src/utility/IMU_Class.inl", false),
        (
            "m5unified",
            "/c/m5unified/docs/img/pin_def_atom_s3.svg",
            false,
        ),
        // A vendor BSP tree: the board page, its dependency manifest, its pin
        // headers, the driver APIs and the sensors — and neither the
        // implementations, the private headers, nor the LoRa product line.
        (
            "waveshare",
            "/c/waveshare/bsp/esp32_s3_touch_lcd_2_8d/README.md",
            true,
        ),
        (
            "waveshare",
            "/c/waveshare/bsp/esp32_s3_touch_lcd_2_8d/idf_component.yml",
            true,
        ),
        (
            "waveshare",
            "/c/waveshare/bsp/esp32_p4_nano/include/bsp/esp32_p4_nano.h",
            true,
        ),
        (
            "waveshare",
            "/c/waveshare/display/lcd/esp_lcd_axs15260d/include/esp_lcd_axs15260d.h",
            true,
        ),
        (
            "waveshare",
            "/c/waveshare/sensor/qmi8658/include/qmi8658.h",
            true,
        ),
        ("waveshare", "/c/waveshare/README.md", true),
        (
            "waveshare",
            "/c/waveshare/bsp/esp32_p4_nano/esp32_p4_nano.c",
            false,
        ),
        (
            "waveshare",
            "/c/waveshare/bsp/esp32_p4_nano/priv_include/bsp_private.h",
            false,
        ),
        (
            "waveshare",
            "/c/waveshare/lora/esp_lora_1121/README.md",
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
        // On-device inference: the API beside its sources (this component has no
        // `include/`), the call shape from an example's `app_main.cpp`, the
        // catalogue of what can be run, and the operator table. Never the
        // implementation, never a picture, never an example's generated config —
        // and never the training side's Python, which is a tool a person runs.
        (
            "esp-dl",
            "/c/esp-dl/esp-dl/dl/base/dl_base_dotprod.hpp",
            true,
        ),
        (
            "esp-dl",
            "/c/esp-dl/examples/cat_detect/main/app_main.cpp",
            true,
        ),
        ("esp-dl", "/c/esp-dl/examples/cat_detect/README.md", true),
        ("esp-dl", "/c/esp-dl/models/README.md", true),
        ("esp-dl", "/c/esp-dl/operator_support_state.md", true),
        (
            "esp-dl",
            "/c/esp-dl/esp-dl/dl/base/dl_base_dotprod.cpp",
            false,
        ),
        ("esp-dl", "/c/esp-dl/esp-dl/Kconfig", false),
        (
            "esp-dl",
            "/c/esp-dl/examples/cat_detect/main/cat.jpg",
            false,
        ),
        (
            "esp-dl",
            "/c/esp-dl/examples/cat_detect/sdkconfig.defaults.esp32p4",
            false,
        ),
        // The training side is a **second source** of the same corpus: what a model needs and how
        // class names are declared are taken, the Python is not.
        ("esp-dl", "/c/esp-detection/README.md", true),
        ("esp-dl", "/c/esp-detection/train.py", false),
        ("esp-dl", "/c/esp-detection/nn/", false),
    ];

    for (corpus, path, expected) in cases {
        let yaml = BUNDLE
            .iter()
            .find(|(c, _)| c == corpus)
            .map(|(_, y)| *y)
            .unwrap_or_else(|| panic!("{corpus} is not in the bundle"));
        let config: GraphRagConfig = serde_yaml::from_str(yaml).expect("parses");
        // **Every** enabled source, not the first: a corpus made of two repositories (a runtime and
        // its export tooling, say) ingests a file if *either* claims it, so a test that asked only the
        // first would pin half the manifest and call the corpus covered.
        let enabled: Vec<&spire_core::actors::rag_ingest::IngestSource> = config
            .pipeline
            .sources
            .iter()
            .filter(|s| s.enabled)
            .collect();
        let matched = enabled.iter().any(|source| {
            spire_core::actors::rag_ingest::path_matches(std::path::Path::new(path), source)
        });
        assert_eq!(
            matched,
            *expected,
            "{corpus}: {path} — {} by the manifest: {:#?}",
            if matched { "claimed" } else { "not claimed" },
            enabled
                .iter()
                .map(|s| (&s.id, &s.processing.include_paths))
                .collect::<Vec<_>>()
        );
    }
}

/// The embedded corpora are the ones a generated firmware crate is written against, so their
/// presence is worth stating on its own: losing one to a rename would otherwise look like
/// "retrieval is bad" rather than like a missing corpus.
///
/// `esp-idf-lib` and `esp-bsp` are the two **library** corpora — a device driver and a board — and
/// they are the pair a hardware task starts from: the first answers "has somebody written this
/// device", the second "how is this board wired and brought up". `m5unified` and `waveshare` are the
/// two **board** corpora the first real applications are built on: M5Stack's own HAL beside the
/// vendor BSPs, and Waveshare's vendor BSPs and display drivers.
#[test]
fn the_embedded_corpora_are_in_the_bundle() {
    let corpora: Vec<&str> = BUNDLE.iter().map(|(corpus, _)| *corpus).collect();
    for expected in [
        "esp-rs-book",
        "esp-idf-hal",
        "esp-idf",
        "esp-idf-lib",
        "esp-bsp",
        "m5unified",
        "waveshare",
        "esp-dl",
        "rust",
    ] {
        assert!(
            corpora.contains(&expected),
            "'{expected}' is missing from the bundle: {corpora:?}"
        );
    }
}

/// The `device-facts` corpus ships **documents**, not just a manifest — and each document has to fit
/// one chunk.
///
/// Three things make that worth pinning, and none of them is visible from the manifest alone:
///
/// * **The documents are the corpus.** A manifest whose directory is empty ingests nothing, and an
///   ingested-nothing corpus reads as "the store knows no device" — indistinguishable, at the seam,
///   from "the facts are not written down". The installer is what fills that directory, so this
///   checks the file it writes is there and is byte-for-byte what the bundle carries.
/// * **The layout has to be the one the manifest describes.** The manifest points at `docs` relative
///   to its own directory, so the glob only works if the installer writes exactly that tree: two
///   places agreeing about a path with nothing checking it is how a corpus ingests an empty
///   directory for a release without anyone noticing.
/// * **One chunk, or the protocol arrives in halves.** A document larger than the corpus's
///   `chunk_size` is split, and half a protocol is not a smaller answer but a **wrong** one: a model
///   given only the framing section writes a driver with no command words, and one given only the
///   command table writes one with no checksum. So the size and the budget are compared here, and a
///   document that outgrows it fails this test rather than splitting silently at ingest time.
#[test]
fn the_device_facts_corpus_ships_documents_that_each_fit_one_chunk() {
    let yaml = BUNDLE
        .iter()
        .find(|(corpus, _)| *corpus == DEVICE_FACTS_CORPUS)
        .map(|(_, yaml)| *yaml)
        .expect("the device-facts manifest is in the bundle");
    let config: GraphRagConfig = serde_yaml::from_str(yaml).expect("parses");
    let source = config
        .pipeline
        .sources
        .iter()
        .find(|s| s.enabled)
        .expect("an enabled source");
    assert!(!FACTS_DOCS.is_empty(), "the corpus would ship no documents");

    let tmp = tempfile::tempdir().expect("temp dir");
    let corpora = spire_code::actors::rag_bundle::install_into(tmp.path()).expect("installs");
    assert!(
        corpora.contains(&DEVICE_FACTS_CORPUS.to_string()),
        "the installer did not report the corpus: {corpora:?}"
    );

    for (name, text) in FACTS_DOCS {
        // The **name is the key**: the seam asks this corpus with a part number, so a document that
        // could not be reached by one is not answering for any part. `.md` because that is the only
        // pattern the source admits.
        assert!(
            name.ends_with(".md"),
            "{name}: the source only admits `**/*.md`, so this would never be ingested"
        );
        // …and it has to be *about* that part: retrieval is similarity over the text, so a
        // `sht20.md` whose body described another device would be found by the key and answer with
        // the wrong part — the exact failure the keyed corpus exists to remove.
        let part = name.trim_end_matches(".md").to_ascii_uppercase();
        assert!(
            text.to_ascii_uppercase().contains(&part),
            "{name} never names {part}, so its key and its content disagree"
        );
        assert!(
            text.len() <= config.pipeline.settings.chunk_size,
            "{name} is {} bytes and the corpus' chunk_size is {}: it would split into two \
             retrievable halves, and half a protocol is worse than none",
            text.len(),
            config.pipeline.settings.chunk_size
        );

        // Installed where the manifest's relative `path` resolves to — and claimed by its glob.
        let path = tmp.path().join(DEVICE_FACTS_CORPUS).join("docs").join(name);
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} was not installed: {e}", path.display()));
        assert_eq!(
            on_disk, *text,
            "{name} differs between the bundle and the disk"
        );
        assert!(
            spire_core::actors::rag_ingest::path_matches(&path, source),
            "{} is not claimed by `{}`'s patterns: {:#?}",
            path.display(),
            source.id,
            source.processing.include_paths
        );
    }
}
