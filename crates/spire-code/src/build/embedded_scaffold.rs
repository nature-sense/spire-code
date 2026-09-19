// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **The embedded container** — the on-device side of a firmware family, as a project.
//!
//! A container is a Cargo workspace holding **one library crate**: the actor system, the executor seam,
//! the `embedded-hal` re-export, and the peripheral drivers. As boards need them, BSP crates join it.
//! Applications are built *against* it rather than containing it, so there is one container per family
//! and many applications — which is why it is a project type of its own.
//!
//! # What creation emits
//!
//! The **framework**, fixed: the crate's manifest and `lib.rs`, the actor system, the two runtimes
//! (`executor.rs`, `embassy.rs`), and the framework's own tests. None of it is the model's to write —
//! the actor system is ours, identical in every container, and a generated actor system would be a
//! worse one every time.
//!
//! What the model writes arrives later, through the two routine operations in this module: [`add_bsp`]
//! — one board's facts (which pin its LED is on, whether it is active-low) — and [`add_driver`] — one
//! device's protocol. Both emit *typed* `todo!()`s, so a container and the application depending on it
//! compile and link before either is filled.
//!
//! # Where the framework's source lives
//!
//! Authored once, in the `spire-embedded` checkout, and **vendored** here under
//! `crates/spire-code/templates/container/`. Creation copies those files with the crate renamed to the
//! project, and a test asserts the copies still equal the checkout byte-for-byte — so a fix to the
//! actor system cannot land in one place and miss the other without that test going red. Embedding the
//! framework as string literals here was rejected for the same reason a created container has no copied
//! `drivers/`: one source, not two.
//!
//! # What creation deliberately does not do
//!
//! It does not pick crates by board, and it does not validate a platform list. A container is
//! **board-agnostic**: boards arrive one BSP at a time afterwards, and each is refused *then* if its
//! chip has no wiring row. (The ids the wizard collected are echoed back for the UI, and used for
//! nothing else.)
//!
//! # One invariant worth stating
//!
//! The crate is named after the **project**, which is the project directory's own name — because
//! [`add_bsp`] and [`add_driver`] derive it that way at run time, from the directory they are handed.
//! Creation must therefore emit that name **verbatim**: normalising it (lower-casing, replacing
//! spaces) would put the crate where the add-operations would never look.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Embedded-HAL** scaffold — the Rust *one contract, N board families* workspace.
//!
//! The Rust analogue of `ProjectStructure::Hal`, which builds a C++ HAL as `hal/api/*.hpp` plus
//! `hal/implementations/<platform>/`. Here the contract is a set of `trait`s in one crate, an
//! implementation is an `impl` in a **backend crate per board family**, and the wiring is Cargo
//! instead of Meson:
//!
//! ```text
//! crates/<prefix>-hal            the contract: no_std, dependency-free
//! crates/<prefix>-hal-std        the std actor executor (host-testable; std families only)
//! crates/<prefix>-hal-esp32      backend: esp-idf-hal  (std  — reuses the std executor)
//! crates/<prefix>-hal-rp2040     backend: rp2040-hal   (no_std — supplies its own executor)
//! ```
//!
//! Three things decide the shape, and each was measured rather than assumed:
//!
//! 1. **A backend per FAMILY, not per chip.** `spire-hal-esp32` serves esp32/esp32s3/esp32c6/
//!    esp32p4: the chip is not a cargo feature in the esp-idf crates, it is the `MCU`
//!    environment variable plus `--target`. So a variant is a *platform* entry, and one backend
//!    crate serves however many variants are selected.
//! 2. **The contract is `no_std` even though the first family is `std`.** The second family
//!    (rp2040) is not, and a `std` backend can implement `no_std` traits while the reverse is
//!    impossible. That is why the contract crate is dependency-free, and why the *std* executor
//!    is a crate of its own: an rp2040 backend cannot use it.
//! 3. **`default-members` excludes the backends.** A host `cargo test` must not need a cross
//!    toolchain or a vendor SDK — it runs the contract's own tests, which is what makes the
//!    abstraction testable without hardware.

use spire_core::build_types::ProjectStructure;
use toml_edit::DocumentMut;

/// The crate name the vendored templates were authored under.
///
/// The template files are the `spire-embedded` checkout's own, so "rename the crate" is a substitution
/// of two strings rather than a rewrite: `spire-embedded` is the crate, `spire_embedded` is how Rust
/// code refers to it.
const TEMPLATE_CRATE: &str = "spire-embedded";

/// Emit the embedded **container**: the framework, fixed, under the marker that declares the type.
///
/// `platforms` is echoed back for the UI and used for nothing else — a container is board-agnostic, and
/// boards arrive one BSP at a time afterwards. It stays in the signature because the wizard's selection
/// is still worth recording against the project.
pub(crate) fn embedded_scaffold(
    project_name: &str,
    platforms: &[String],
) -> Result<super::ScaffoldOutput, String> {
    // **Verbatim.** `add_bsp` and `add_driver` derive the crate name from the project directory's own
    // name, so normalising it here — lower-casing, replacing spaces, appending a suffix — would put the
    // crate where those operations would never look.
    let crate_name = project_name.trim();
    if crate_name.is_empty() {
        return Err(
            "an embedded container needs a project name: it is the crate's name, and the name the \
             add-operations look for"
                .to_string(),
        );
    }
    let crate_id = crate_name.replace('-', "_");
    let crate_dir = format!("crates/{crate_name}");

    let rename = |text: &str| {
        text.replace(TEMPLATE_CRATE, crate_name)
            .replace("spire_embedded", &crate_id)
    };

    let workspace = workspace_manifest(crate_name);
    let lib = rename(include_str!("../../templates/container/src/lib.rs"));

    // The framework, file by file, straight from the vendored copy: nothing is re-typed here, and the
    // drift test compares these against the checkout they came from.
    let framework = [
        (
            include_str!("../../templates/container/Cargo.toml"),
            "Cargo.toml",
        ),
        (
            include_str!("../../templates/container/src/lib.rs"),
            "src/lib.rs",
        ),
        (
            include_str!("../../templates/container/src/actor.rs"),
            "src/actor.rs",
        ),
        (
            include_str!("../../templates/container/src/executor.rs"),
            "src/executor.rs",
        ),
        (
            include_str!("../../templates/container/src/embassy.rs"),
            "src/embassy.rs",
        ),
        (
            include_str!("../../templates/container/tests/actor.rs"),
            "tests/actor.rs",
        ),
        (
            include_str!("../../templates/container/tests/executor.rs"),
            "tests/executor.rs",
        ),
        (
            include_str!("../../templates/container/tests/embassy.rs"),
            "tests/embassy.rs",
        ),
    ];

    let mut files = vec![
        super::ScaffoldFile {
            path: "Cargo.toml".to_string(),
            content: workspace.clone(),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "README.md".to_string(),
            content: readme(crate_name),
            structural: true,
            ..Default::default()
        },
    ];
    for (template, relative) in framework {
        files.push(super::ScaffoldFile {
            path: format!("{crate_dir}/{relative}"),
            content: rename(template),
            structural: true,
            ..Default::default()
        });
    }
    // The growth surface, empty at creation: `add_driver` owns this file from here on.
    files.push(super::ScaffoldFile {
        path: format!("{crate_dir}/src/drivers/mod.rs"),
        content: DRIVERS_MOD_RS.to_string(),
        structural: true,
        ..Default::default()
    });

    Ok(super::ScaffoldOutput {
        build_file: "Cargo.toml".to_string(),
        build_content: workspace,
        source_dir: "crates".to_string(),
        source_file: format!("{crate_dir}/src/lib.rs"),
        source_content: lib,
        files,
        platform_targets: platforms.to_vec(),
        // Nothing is fillable at creation. The framework is fixed, and the fillable leaves — a board's
        // facts, a device's protocol — arrive with `add_bsp` / `add_driver`.
        fill_roots: Vec::new(),
        // The container's dependency tables are the framework's, and the framework is not the model's to
        // edit. A driver or BSP added later brings its own manifest.
        dependency_sections: Vec::new(),
        structure: ProjectStructure::Embedded,
        embedded: true,
    })
}

/// The workspace manifest — and the **marker** that makes this project recognizable.
///
/// `[workspace.metadata.spire] structure = "embedded"` is what `cargo.rs::analyze` reads: there is no
/// layout guess, and the value is the enum's own key rather than a prettier spelling, so the marker and
/// the project type cannot drift apart.
fn workspace_manifest(crate_name: &str) -> String {
    format!(
        r#"# {crate_name} — the embedded container: the actor framework, the peripheral drivers, and a
# BSP crate per board that needs one.
#
# One crate, and it is host-checkable: the actor system and the peripherals are tested against fakes,
# so `cargo test` here needs no vendor SDK, no cross toolchain and no board.
#
# Boards and devices arrive through spire-code (`add_bsp`, `add_driver`) rather than by hand: a BSP is
# a crate beside this one, a driver is a module inside it.

[workspace]
resolver = "2"
members = ["crates/{crate_name}"]

# A host `cargo test` must not need a cross toolchain or a vendor SDK, so only the library is a default
# member. A BSP crate builds for its own chip, explicitly.
default-members = ["crates/{crate_name}"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-3.0-or-later"

# Declares this project's type to Spire. Read, never guessed: a layout that happens to look like this
# one is not this project type.
[workspace.metadata.spire]
structure = "embedded"
"#
    )
}

/// The container's README: what it is, and how it is checked.
///
/// Deliberately short. The framework's own reasoning lives with the framework — in `actor.rs` and in the
/// `spire-embedded` README — and a second copy here would be a second copy to keep true.
fn readme(crate_name: &str) -> String {
    format!(
        r#"# {crate_name}

The **embedded container** for this product: the on-device side of its firmware, as a Cargo workspace.

## What is in it

- the **actor system** — a `Message` type plus a `handle`, the same shape as the host's actor;
- **peripherals** (`src/drivers/`) — devices written against `embedded-hal`'s traits, so the same driver
  runs on any board and on a host fake;
- as they are needed, a **BSP** crate per board (`crates/{crate_name}-bsp-*`), holding that board's own
  facts: which pin its LED is on, whether it is active-low, which bus its display is on.

There is no HAL here, and that is the point. The peripheral traits are `embedded-hal`'s and the HAL that
implements them for a chip is the *vendor's* crate — so nothing in this workspace knows a chip, a vendor
SDK or an OS.

## Checking it

```sh
cargo test                  # the actor system and the peripherals, against fakes
cargo test --all-features   # plus the std and embassy runtimes
```

None of that needs a vendor SDK, a cross toolchain or a board — which is what writing peripherals
against traits buys.

## Growing it

Boards and devices are added by spire-code, not by hand: a BSP for a board with no upstream crate, a
driver for a device with no upstream one. Both are emitted as *typed* `todo!()`s, so the container and
every application built against it compile and link before either is written.
"#
    )
}

/// The drivers module, empty at creation — `add_driver` owns this file from here on.
const DRIVERS_MOD_RS: &str = r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Peripherals: device drivers written against `embedded-hal`'s traits.**
//!
//! The default for a device is the upstream crate. A driver belongs here for the two cases where that is
//! not the answer: there is no crate for the device that speaks `embedded-hal` 1.x, or the project needs
//! the driver to be *actor-shaped* — behind a message type rather than a call surface.
//!
//! The shape is the same either way, and it is the shape an upstream crate already has: **generic over
//! the traits, never over a board.** A driver that names a vendor type works on one chip only. Every
//! driver here is host-tested against fakes rather than hardware, which is what makes that checkable.
//!
//! spire-code adds one module per device (`add_driver`), so this file starts with no modules.
"#;

// ---------------------------------------------------------------------------------------------
// The container's *routine* work: a BSP added as required.
//
// The actor framework is fixed and scaffolded once. A BSP is not: it exists because a particular
// board has no upstream crate, and its whole content is that board's facts — which pin, which bus,
// which polarity. So this is the operation that runs often, and the one worth getting right.
// ---------------------------------------------------------------------------------------------

/// The BSP crate for one **board**.
///
/// One per board rather than one per family: two boards on one chip are two crates, because the
/// facts they carry are the boards'. The *vendor* crate they wrap is chosen per chip, which is the
/// other question — see [`vendor_for`].
pub(crate) fn bsp_crate(platform_id: &str) -> String {
    format!("spire-bsp-{platform_id}")
}

/// The container's library crate **name** — the crate the driver modules live in.
///
/// Named **after the project**, which is what makes it relative rather than fixed: our container is
/// the project `spire-embedded` holding `crates/spire-embedded`, and a container named
/// `weather-embedded` holds `crates/weather-embedded`. `spire-embedded` is the name of *one* project,
/// not a crate id every container shares, so it is read from the directory the caller passed.
///
/// Refuses rather than guessing when there is nothing there: a driver written into a directory that
/// does not exist would be a file no compiler reads, and the failure would look like a missing module.
fn container_crate(root: &std::path::Path) -> Result<String, String> {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("cannot read a project name from {}", root.display()))?;
    let dir = format!("crates/{name}");
    if !root.join(&dir).join("Cargo.toml").is_file() {
        return Err(format!(
            "no container library at {} — the container's crate is named after its project, so \
             '{dir}' is what this expects",
            root.join(&dir).display()
        ));
    }
    Ok(name)
}

/// The vendor crate a board's BSP wraps, and the version to ask for.
///
/// One row per chip, and only for chips proven on a board: `esp32c3` is the pilot. A BSP for a chip
/// nobody has flashed is a crate whose `todo!()`s might never be fillable.
fn vendor_for(platform_id: &str) -> Option<(&'static str, &'static str, &'static [&'static str])> {
    match platform_id {
        // `unstable` is **measured, not assumed**: `esp_hal::delay` is gated behind it, and the compiler
        // says so in as many words ("found an item that was configured out … gated behind the `unstable`
        // feature") when a BSP built without it fails on `use esp_hal::delay::Delay`. A BSP's second job
        // is a delay for the drivers' `DelayNs`, so that absence is not a nicety — and the version is the
        // one the pilot application was built, flashed and run with.
        "esp32c3" => Some(("esp-hal", "1.2", &["unstable"])),
        _ => None,
    }
}

/// A board's BSP: two files, one of which is the fill's.
pub(crate) fn bsp_files(
    container: &str,
    platform_id: &str,
    vendor: &str,
    vendor_version: &str,
    vendor_features: &[&str],
) -> Vec<super::ScaffoldFile> {
    let name = bsp_crate(platform_id);
    // One list, chip first: the chip selects the vendor crate's build, and the rest are what this board's
    // facts need from it. Joined here rather than templated as fragments, so the manifest reads as one
    // feature array however many there are.
    let features = std::iter::once(format!("\"{platform_id}\""))
        .chain(
            vendor_features
                .iter()
                .map(|feature| format!("\"{feature}\"")),
        )
        .collect::<Vec<String>>()
        .join(", ");
    let manifest = BSP_MANIFEST
        .replace("__NAME__", &name)
        .replace("__CHIP__", platform_id)
        .replace("__VENDOR__", vendor)
        .replace("__VENDOR_VERSION__", vendor_version)
        .replace("__FEATURES__", &features)
        .replace("__CONTAINER__", container);
    vec![
        super::ScaffoldFile {
            path: format!("crates/{name}/Cargo.toml"),
            content: manifest,
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: format!("crates/{name}/src/lib.rs"),
            content: bsp_lib(&name, platform_id, vendor),
            // The fill's file: the board's pins are the one thing no scaffold can know, and they are
            // the whole reason this crate exists.
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::HalImplementation),
        },
    ]
}

/// The BSP's source, with the vendor crate's identifier form substituted for its `use` lines.
fn bsp_lib(name: &str, platform_id: &str, vendor: &str) -> String {
    BSP_LIB_RS
        .replace("__NAME__", name)
        .replace("__CHIP__", platform_id)
        .replace("__VENDOR__", vendor)
        .replace("__VENDOR_ID__", &vendor.replace('-', "_"))
}

/// A BSP's manifest — the vendor HAL, and the crate whose trait names its signatures use.
const BSP_MANIFEST: &str = r#"# __NAME__ — one board's support package.
#
# A BSP is not a HAL. The peripheral traits are `embedded-hal`'s and the HAL is the vendor's crate,
# so there is nothing of ours to implement here; what lives in this crate is **this board's facts**,
# which the vendor cannot know and no application should have to carry.
#
# The default is an upstream BSP. This crate exists because this board has none.
[package]
name = "__NAME__"
description = "The __CHIP__ board's facts — pins, buses, polarity — over __VENDOR__."
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
# The actor system and the peripherals module, for the trait names this crate's signatures use. The
# container's crate is named after its project, so this is a sibling of it under `crates/`.
__CONTAINER__ = { path = "../__CONTAINER__" }
# The vendor HAL: the only crate here that knows a chip, and the crate the feature selects. The features
# are the chip, plus whatever this board's facts ask of the vendor — `unstable`, here, because a delay
# lives behind it and this crate's second job is a delay.
__VENDOR__ = { version = "__VENDOR_VERSION__", features = [__FEATURES__] }
"#;

/// A BSP's source: the board's constructors, with the pins left to the fill.
const BSP_LIB_RS: &str = r#"//! The `__NAME__` BSP: **this board's facts**, over `__VENDOR__`.
//!
//! Nothing here implements a peripheral trait. `__VENDOR__`'s output driver *is* an `embedded-hal`
//! `OutputPin` and its delay *is* a `DelayNs`, so a wrapper would be a second implementation of
//! something the vendor crate already provides — and one that stood between this board and every
//! driver written against the ecosystem.
//!
//! What a BSP adds is knowledge the vendor cannot have: which pin the LED is on, whether it is
//! active-low, which bus the display is on. Putting it here means no application carries it, and no
//! two applications can disagree about it.
//!
//! The `todo!()`s are the fill's. They are **typed**, so this crate compiles and links before any of
//! them is written — which is what lets an application that depends on it be built and flashed while
//! the board's pins are still unknown.
//!
//! `no_std` is not decoration here. A bare-metal target has no `std` to link, so a library that does not
//! say so fails with "can't find crate for `std`" — and, far less legibly, with "cannot find macro
//! `todo`", because the prelude it was silently using is std's.

#![no_std]

use __VENDOR_ID__::delay::Delay;
use __VENDOR_ID__::gpio::Output;
use __VENDOR_ID__::peripherals::Peripherals;

/// This board, and the peripherals its facts are taken from.
pub struct Board {
    peripherals: Peripherals,
}

impl Board {
    /// Take the peripherals **once**, here, so that every constructor below is a board fact rather
    /// than an argument each caller has to remember. (`__VENDOR__`'s peripheral singletons are
    /// `Copy`, so a `&self` can hand one out.)
    pub fn new(peripherals: Peripherals) -> Self {
        Self { peripherals }
    }

    /// The board's LED, as an `embedded-hal` output.
    ///
    /// TODO: this board's pin. Polarity belongs here too — an active-low LED is inverted inside this
    /// function, once, and nothing above the board has to know. A board whose LED is *addressable*
    /// needs no output at all: it takes a bus and `spire_embedded::drivers::Ws2812`, because a level
    /// is not a colour.
    pub fn led(&self) -> Output<'static> {
        let _ = self.peripherals.GPIO8;
        todo!("this board's LED pin")
    }

    /// The board's blocking delay, as an `embedded-hal` `DelayNs`.
    ///
    /// `__VENDOR__`'s delay already implements it, so there is nothing to wrap and no error to map.
    pub fn delay(&self) -> Delay {
        Delay::new()
    }
}
"#;

/// Add a board's BSP to the container — **the routine operation**.
///
/// The container's framework is scaffolded once; a BSP is added *as required*, for a board that has
/// no upstream crate. So this writes two files and the workspace member that makes them exist, and it
/// refuses rather than half-does: a manifest that cannot be read, a crate directory that already
/// exists, or a chip with no known vendor crate all stop before anything is written.
pub(crate) fn add_bsp(
    root: &std::path::Path,
    platform_id: &str,
) -> Result<serde_json::Value, String> {
    let platform = crate::platform::Platform::from_registry(platform_id)
        .ok_or_else(|| format!("unknown platform '{platform_id}' (see ~/.spire/platforms)"))?;
    if !platform.is_embedded() {
        return Err(format!(
            "platform '{platform_id}' is not an embedded platform (os '{}'); a BSP needs a board it \
             can be built for",
            platform.os
        ));
    }
    let (vendor, version, features) = vendor_for(platform_id).ok_or_else(|| {
        format!(
            "no BSP wiring is known for '{platform_id}' yet. `esp32c3` is the one row measured on a \
             board — built, flashed, LED cycling — and another chip is a row here, not a guess"
        )
    })?;

    let name = bsp_crate(platform_id);
    // The container this BSP is a sibling of, named after its project — refused here, before anything is
    // written, because the BSP's manifest path-deps that crate.
    let container = container_crate(root)?;
    let crate_dir = root.join("crates").join(&name);
    if crate_dir.exists() {
        return Err(format!(
            "board '{platform_id}' already has a BSP ({})",
            crate_dir.display()
        ));
    }

    // The member list is what makes the crate part of the container, so it is read and **edited**
    // before anything is written: a container whose manifest cannot be read, or cannot take the
    // member, is refused — not half-edited.
    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let member = format!("crates/{name}");
    if workspace_members(&manifest)?
        .iter()
        .any(|listed| listed == &member)
    {
        return Err(format!(
            "`{member}` is already a workspace member — the manifest and the crate directory \
             disagree, so nothing was written"
        ));
    }
    let updated = with_workspace_member(&manifest, &member)?;

    let files = bsp_files(&container, platform_id, vendor, version, features);
    let mut written: Vec<String> = Vec::new();
    for file in &files {
        let target = root.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&target, &file.content)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        written.push(file.path.clone());
    }

    std::fs::write(&manifest_path, &updated)
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;

    Ok(serde_json::json!({
        "board": platform_id,
        "crate": name,
        "vendor_crate": vendor,
        "written": written,
        "workspace_member": member,
        "note": "the board's facts are typed `todo!()`s in src/lib.rs, so the crate builds and links \
                 until the fill writes them",
    }))
}

/// The workspace members a container declares, parsed.
///
/// Parsed rather than grepped, because it is the *shape* of this list that decides whether an edit lands
/// in it — and because a substring test for the member is what let the first version of `add_bsp`
/// believe a manifest it had just broken was fine.
fn workspace_members(manifest: &str) -> Result<Vec<String>, String> {
    let doc: DocumentMut = manifest
        .parse()
        .map_err(|e| format!("the container's Cargo.toml does not parse: {e}"))?;
    doc.get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(|members| members.as_array())
        .map(|members| {
            members
                .iter()
                .filter_map(|member| member.as_str().map(str::to_string))
                .collect()
        })
        .ok_or_else(|| {
            "the container's Cargo.toml has no `[workspace] members` array — there is nothing to add \
             the crate to"
                .to_string()
        })
}

/// The container's manifest, with one more workspace member.
///
/// `toml_edit` rather than an inserted line, for two **measured** reasons. First, **the file is not
/// ours**: it carries the prose explaining why the crate set is what it is, and a round-trip through a
/// plain TOML parser would delete every comment in it. Second, **the list has more than one shape**: a
/// scaffolded container writes one member per line, while the framework's own manifest writes them
/// *inline* (`members = ["crates/spire-embedded"]`). A line-oriented insertion that understood only the
/// first shape appended the new member after the last line of the file — inside `[workspace.package]`,
/// as a bare string with no `=` — and cargo then refused to read the container's own manifest. The
/// fixture could not notice, because it was written in the other shape; the live build could not help
/// but notice.
fn with_workspace_member(manifest: &str, member: &str) -> Result<String, String> {
    let mut doc: DocumentMut = manifest
        .parse()
        .map_err(|e| format!("the container's Cargo.toml does not parse: {e}"))?;
    let array = doc
        .get_mut("workspace")
        .and_then(|workspace| workspace.get_mut("members"))
        .and_then(|members| members.as_array_mut())
        .ok_or_else(|| {
            "the container's Cargo.toml has no `[workspace] members` array — there is nothing to add \
             the crate to"
                .to_string()
        })?;
    array.push(member);
    Ok(doc.to_string())
}

// ---------------------------------------------------------------------------------------------
// The other routine operation: a driver added as required.
// ---------------------------------------------------------------------------------------------

/// A driver's bus: the trait as a bound writes it, the `use` that names it, and its module's name.
///
/// A device's bus is a **device fact**, so it is an input rather than a guess: a strip is on SPI, a
/// sensor on I²C, and a driver written for the wrong one is a driver that never compiles.
fn driver_bus(bus: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match bus {
        "spi" => Some(("SpiBus<u8>", "embedded_hal::spi::SpiBus", "spi")),
        "i2c" => Some(("I2c", "embedded_hal::i2c::I2c", "i2c")),
        _ => None,
    }
}

/// A device's module identifier, from whatever a caller called it.
fn driver_id(device: &str) -> String {
    device
        .trim()
        .to_lowercase()
        .replace(' ', "-")
        .replace('-', "_")
}

/// The driver's type name: the module's identifier with its first letter raised.
///
/// Crude but honest, and the fill is free to rename it — the file is the fill's.
fn driver_type(id: &str) -> String {
    format!("{}{}", id[..1].to_uppercase(), &id[1..])
}

/// A driver: the module, and the host test that proves it against a fake bus.
///
/// Both are fillable — the *protocol* is the thing no scaffold can know — and the role is `Shared`
/// rather than a HAL role: a driver is code every board of the family uses, not one board's
/// implementation of anything.
pub(crate) fn driver_files(
    container_dir: &str,
    device: &str,
    bus: &str,
) -> Result<Vec<super::ScaffoldFile>, String> {
    let (bound, use_path, bus_name) = driver_bus(bus).ok_or_else(|| {
        format!(
            "'{bus}' is not a bus this scaffold knows — pass `spi` or `i2c`, because a device's bus \
             is a device fact and a driver written for the wrong one never compiles"
        )
    })?;
    let id = driver_id(device);
    let type_name = driver_type(&id);

    let fill = |template: &str| {
        template
            .replace("__TYPE__", &type_name)
            .replace("__DEVICE__", &id)
            .replace("__BUS__", bus_name)
            .replace("__BUS_USE__", use_path)
            .replace("__BUS_BOUND__", bound)
    };

    Ok(vec![
        super::ScaffoldFile {
            path: format!("{container_dir}/src/drivers/{id}.rs"),
            content: fill(DRIVER_RS),
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::Shared),
        },
        super::ScaffoldFile {
            path: format!("{container_dir}/tests/{id}.rs"),
            content: fill(DRIVER_TEST_RS),
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::Shared),
        },
    ])
}

/// A driver's module: the bus-generic skeleton, with the device's protocol left to the fill.
const DRIVER_RS: &str = r#"//! A `__DEVICE__` driver, over an `__BUS__` bus.
//!
//! It lives here rather than as a dependency for one of the two reasons this container exists: no
//! upstream crate speaks `embedded-hal` 1.x for this device, or the project needs the driver to be
//! *actor-shaped* — behind a message type rather than a call surface.
//!
//! What the skeleton fixes is what every driver here looks like: generic over the bus and the delay,
//! no vendor type in sight, and errors that pass the bus's own through untouched. What the fill
//! writes is the device's protocol — the part no scaffold can know, and the part the host test is
//! for.

use embedded_hal::delay::DelayNs;
use __BUS_USE__;

/// Why a driver call failed.
///
/// The bus's own error is passed through **untouched** in [`DriverError::Bus`] rather than flattened
/// into an error type of ours: the peripheral's failure modes are the peripheral's, and a driver that
/// invents its own vocabulary for them hides what the vendor crate actually reported.
#[derive(Debug, PartialEq, Eq)]
pub enum DriverError<E> {
    /// The bus refused the transfer.
    Bus(E),
    /// TODO: this device's own failure modes, if it has any worth naming.
    Protocol,
}

/// A `__DEVICE__`.
pub struct __TYPE__<S, D> {
    bus: S,
    delay: D,
}

impl<S, D> __TYPE__<S, D> {
    /// Build one over a bus and a delay.
    pub fn new(bus: S, delay: D) -> Self {
        Self { bus, delay }
    }
}

impl<S, D> __TYPE__<S, D>
where
    S: __BUS_BOUND__,
    D: DelayNs,
{
    /// TODO: what this device is for, in the terms a caller thinks in.
    ///
    /// TODO: if this is an I²C device, its **address** is a device fact — one board may carry two of
    /// the same part on different addresses, which is why the constructor is where it belongs.
    pub fn read(&mut self) -> Result<u16, DriverError<S::Error>> {
        let _ = (&mut self.bus, &mut self.delay);
        todo!("the __DEVICE__ protocol")
    }
}
"#;

/// A driver's host test: a fake bus that records, and an assertion the *board* cannot make.
const DRIVER_TEST_RS: &str = r#"//! The `__DEVICE__` driver against a **fake bus** — no hardware.
//!
//! This file exists before the driver is written, and that is the point: a driver whose first real
//! test is a board is a driver that gets debugged on a board. A fake bus records what it was given,
//! so the protocol's **bytes** — the one thing an eye cannot check — become assertions.

use core::cell::RefCell;
use std::rc::Rc;

use embedded_hal::delay::DelayNs;
use __BUS_USE__;

/// A bus that keeps what it was given, behind an `Rc` so the test can read it after the driver has
/// taken the bus by value — which is how the driver takes it, because a bus has exactly one owner.
#[derive(Clone, Default)]
struct FakeBus {
    sent: Rc<RefCell<Vec<u8>>>,
}

impl FakeBus {
    fn sent(&self) -> Vec<u8> {
        self.sent.borrow().clone()
    }
}

/// TODO: implement `__BUS_USE__` for `FakeBus`. Every method records into `self.sent` and returns
/// `Ok(())`; the recording is what the test asserts on, so none of them has to be clever.

/// A delay that records instead of sleeping, so the test is instantaneous and can assert the *value*.
#[derive(Clone, Default)]
struct FakeDelay {
    waited: Rc<RefCell<Vec<u32>>>,
}

impl DelayNs for FakeDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.waited.borrow_mut().push(ns);
    }
}

#[test]
#[ignore = "the __DEVICE__ protocol is not written yet — the fill writes it, and removes this"]
fn a_driver_writes_what_the_device_expects() {
    // TODO: build the driver over the fakes, make one call, and assert the recorded bytes against
    // the datasheet's sequence. Start with the encoding: the wire format is the protocol, and it is
    // the part that a fake bus checks better than a board ever could.
    todo!("assert `FakeBus::sent()` against the __DEVICE__ datasheet")
}
"#;

/// Add a device driver to the container — **the other routine operation**.
///
/// The skeleton is fixed and the protocol is the fill's, so this writes the module and its host test
/// and *registers* the module. The last part matters as much as the first two: a file the module list
/// does not name is a file the compiler never reads, and the failure looks like "my driver does not
/// exist" rather than a missing line.
///
/// It refuses rather than half-does, for the same reasons `add_bsp` does: a container with no library
/// crate, a device that already has a driver, or a bus this scaffold does not know all stop before
/// anything is written.
pub(crate) fn add_driver(
    root: &std::path::Path,
    device: &str,
    bus: &str,
) -> Result<serde_json::Value, String> {
    // The container's crate — named after its project — read and refused here, before anything is
    // written: a driver written into a crate that is not there is a module no compiler reads.
    let container = container_crate(root)?;
    let container_dir = format!("crates/{container}");
    // Validates the bus, and refuses by name if it is one this scaffold does not know.
    let files = driver_files(&container_dir, device, bus)?;
    let id = driver_id(device);
    let type_name = driver_type(&id);

    let library = root.join(&container_dir);
    let module_path = library.join("src/drivers").join(format!("{id}.rs"));
    if module_path.exists() {
        return Err(format!(
            "device '{id}' already has a driver ({})",
            module_path.display()
        ));
    }

    // The module list is read before anything is written: absent is fine (it is created below),
    // unreadable is not.
    let list_path = library.join("src/drivers/mod.rs");
    let existing = match std::fs::read_to_string(&list_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", list_path.display())),
    };

    let mut written: Vec<String> = Vec::new();
    for file in &files {
        let target = root.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&target, &file.content)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        written.push(file.path.clone());
    }

    let list = match existing {
        None => format!(
            "//! Peripheral drivers, one device per module.\n\npub mod {id};\n\n\
             pub use {id}::{type_name};\n"
        ),
        Some(text) => insert_into_driver_list(&text, &id, &type_name),
    };
    std::fs::write(&list_path, list)
        .map_err(|e| format!("cannot write {}: {e}", list_path.display()))?;

    Ok(serde_json::json!({
        "device": id,
        "bus": bus,
        "written": written,
        "registered_in": format!("crates/{container}/src/drivers/mod.rs"),
        "note": "the protocol is a `todo!()` and the host test is ignored until the fill writes it — \
                 the crate builds either way",
    }))
}

/// Add a module and its re-export to a `drivers/mod.rs`, beside the others.
///
/// Anchored on the *last* `pub mod`/`pub use` line rather than appended to the end, so the file keeps
/// the shape a hand-written one has — modules first, then re-exports — and so that adding two drivers
/// in a row leaves a file that still reads like one a person wrote.
fn insert_into_driver_list(text: &str, id: &str, type_name: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let after_last = |prefix: &str, lines: &[String]| {
        lines
            .iter()
            .rposition(|line| line.trim_start().starts_with(prefix))
            .map(|index| index + 1)
    };
    match after_last("pub mod ", &lines) {
        Some(at) => lines.insert(at, format!("pub mod {id};")),
        None => lines.push(format!("pub mod {id};")),
    }
    match after_last("pub use ", &lines) {
        Some(at) => lines.insert(at, format!("pub use {id}::{type_name};")),
        None => lines.push(format!("pub use {id}::{type_name};")),
    }
    let mut updated = lines.join("\n");
    updated.push('\n');
    updated
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// A hermetic registry, so the tests can name a family the machine may not have (rp2040) and
    /// can prove the refusals without touching `~/.spire/platforms`.
    fn registry(entries: &[(&str, &str, Option<&str>)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (id, os, family) in entries {
            let family_line = family.map(|f| format!("family: {f}\n")).unwrap_or_default();
            std::fs::write(
                dir.path().join(format!("{id}.yaml")),
                format!(
                    "id: {id}\nname: {id}\nos: {os}\n{family_line}architecture:\n  cpu_family: x\n  \
                     cpu: x\n  endian: little\n  target_triple: x\n"
                ),
            )
            .unwrap();
        }
        dir
    }

    /// **A container is the framework, and nothing else.** Creation emits one library crate whose
    /// contents are fixed — the actor system and the two runtimes — plus an *empty* `drivers/` module for
    /// the devices that arrive later. No `<name>-hal`, no `-std` crate, no per-family backend: those
    /// belonged to a shape that had a contract to implement, and there is no contract any more.
    #[test]
    fn a_container_is_the_framework_and_an_empty_drivers_module() {
        let out = embedded_scaffold("weather-embedded", &[]).expect("a container");

        let mut paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "Cargo.toml",
                "README.md",
                "crates/weather-embedded/Cargo.toml",
                "crates/weather-embedded/src/actor.rs",
                "crates/weather-embedded/src/drivers/mod.rs",
                "crates/weather-embedded/src/embassy.rs",
                "crates/weather-embedded/src/executor.rs",
                "crates/weather-embedded/src/lib.rs",
                "crates/weather-embedded/tests/actor.rs",
                "crates/weather-embedded/tests/embassy.rs",
                "crates/weather-embedded/tests/executor.rs",
            ],
            "the container's files, exactly"
        );

        // Nothing is the model's to write at creation: the framework is fixed, and the skeletons that
        // *are* fillable — a board's facts, a device's protocol — arrive with `add_bsp` / `add_driver`.
        assert!(out.fill_roots.is_empty(), "{:?}", out.fill_roots);
        assert!(
            out.dependency_sections.is_empty(),
            "{:?}",
            out.dependency_sections
        );
        assert!(
            out.files.iter().all(|f| f.structural),
            "the framework is structural, not fillable"
        );

        // The drivers module exists and names no driver: `add_driver` owns it from here on.
        let drivers = out
            .files
            .iter()
            .find(|f| f.path == "crates/weather-embedded/src/drivers/mod.rs")
            .expect("the drivers module")
            .content
            .clone();
        assert!(!drivers.contains("pub mod "), "{drivers}");

        // The workspace wiring: one member — the library — and it *is* a default member, so a host
        // `cargo test` needs no cross toolchain and no vendor SDK.
        for expected in [
            "members = [\"crates/weather-embedded\"]",
            "default-members = [\"crates/weather-embedded\"]",
        ] {
            assert!(
                out.build_content.contains(expected),
                "missing `{expected}`:\n{}",
                out.build_content
            );
        }
        assert_eq!(out.source_file, "crates/weather-embedded/src/lib.rs");
        assert_eq!(out.structure, ProjectStructure::Embedded);
        assert!(out.embedded, "the wizard sets embedded for this type");
    }

    /// The crate is named after the project **verbatim** — nothing is normalised.
    ///
    /// `add_bsp` and `add_driver` derive the crate name from the project *directory's* own name, so a
    /// name rewritten here (lower-cased, spaces replaced, a suffix appended) would be a crate they would
    /// never find. The deleted emitter normalised (`Weather Node` → `weather-node-hal`); this one must
    /// not, which is the whole reason the name is passed through untouched.
    #[test]
    fn the_crate_is_named_after_the_project_verbatim() {
        let out = embedded_scaffold("Weather Node", &[]).expect("a container");
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.contains(&"crates/Weather Node/src/lib.rs"),
            "the directory name is the crate name, whatever it is: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("-hal")),
            "no contract crate and no suffix: {paths:?}"
        );

        // The crate's own manifest carries that same name, so cargo and the add-operations agree.
        let manifest = out
            .files
            .iter()
            .find(|f| f.path == "crates/Weather Node/Cargo.toml")
            .expect("the crate manifest")
            .content
            .clone();
        assert!(manifest.contains("name = \"Weather Node\""), "{manifest}");

        // A nameless project is refused rather than emitted as a bare `crates/`.
        let err = embedded_scaffold("   ", &[]).unwrap_err();
        assert!(err.contains("needs a project name"), "{err}");
    }

    /// The **marker** the analyzer reads, and the rename — the two things a created container has to get
    /// right for everything downstream to find it.
    #[test]
    fn the_marker_is_the_analyzers_key_and_no_template_name_survives() {
        let out = embedded_scaffold("weather-embedded", &[]).expect("a container");

        assert!(
            out.build_content.contains("[workspace.metadata.spire]"),
            "{}",
            out.build_content
        );
        assert!(
            out.build_content.contains(&format!(
                "structure = \"{}\"",
                ProjectStructure::Embedded.as_str()
            )),
            "the value is the enum's own key:\n{}",
            out.build_content
        );

        // The templates are the `spire-embedded` checkout's own files, so the rename has to reach every
        // one of them: a leftover `spire_embedded` would name a crate that does not exist.
        for file in &out.files {
            assert!(
                !file.content.contains("spire_embedded"),
                "{} still refers to the template's crate:\n{}",
                file.path,
                file.content
            );
        }
        let lib = out
            .files
            .iter()
            .find(|f| f.path == "crates/weather-embedded/src/lib.rs")
            .expect("the crate's lib")
            .content
            .clone();
        assert!(lib.contains("weather-embedded"), "{lib}");
    }

    /// The platform list is **echoed, not used**: a container is board-agnostic, and boards arrive one
    /// BSP at a time. So creation succeeds with none — or with ids this scaffold has no wiring row for, or
    /// that are not embedded at all, because none of that is a container's business yet.
    #[test]
    fn a_container_is_board_agnostic_so_creation_takes_any_platform_list() {
        let none = embedded_scaffold("weather-embedded", &[]).expect("no platforms at all");
        assert!(none.platform_targets.is_empty());

        let some = embedded_scaffold(
            "weather-embedded",
            &["esp32h2".to_string(), "rpi5".to_string()],
        )
        .expect("creation does not resolve boards");
        assert_eq!(some.platform_targets.len(), 2, "echoed for the UI");
        assert_eq!(
            some.files.len(),
            none.files.len(),
            "the platform list changes nothing about what is emitted"
        );
    }

    /// **The vendored framework still equals the checkout it came from.**
    ///
    /// Creation emits the framework from `crates/spire-code/templates/container/`, and the framework is
    /// authored in the `spire-embedded` checkout. Two copies, so something has to keep them equal — and a
    /// fix landing in one and not the other is exactly the drift this shape exists to avoid. The
    /// comparison is byte-for-byte, with the crate renamed to the checkout's own project name so the
    /// substitution cancels out.
    ///
    /// Skipped, with a note, when the checkout is not on this machine: there is then nothing to compare.
    /// `drivers/` is excluded on purpose — it is the growth surface, not framework.
    #[test]
    fn the_vendored_framework_still_equals_the_checkout() {
        let Some(source) = embedded_container() else {
            return;
        };
        let name = source.file_name().unwrap().to_string_lossy().to_string();
        let out = embedded_scaffold(&name, &[]).expect("a container");

        for relative in [
            "Cargo.toml",
            "src/lib.rs",
            "src/actor.rs",
            "src/executor.rs",
            "src/embassy.rs",
            "tests/actor.rs",
            "tests/executor.rs",
            "tests/embassy.rs",
        ] {
            let emitted = out
                .files
                .iter()
                .find(|f| f.path == format!("crates/{name}/{relative}"))
                .unwrap_or_else(|| panic!("the scaffold emits crates/{name}/{relative}"));
            let reference =
                std::fs::read_to_string(source.join(format!("crates/{name}/{relative}")))
                    .unwrap_or_else(|e| panic!("reading the checkout's {relative}: {e}"));
            assert_eq!(
                emitted.content, reference,
                "{relative} has drifted from the checkout: copy the change into \
                 crates/spire-code/templates/container/ (or back out of it)"
            );
        }
    }

    /// Not a test of the content but of its **compilability**: write the container somewhere a real
    /// `cargo` can be pointed at, so the emitted framework is built as code rather than trusted as text.
    ///
    /// Ignored by default because it writes outside the target directory, which a test should not do
    /// unasked:
    ///
    /// ```sh
    /// cargo test -p spire-code --lib dump_scaffold -- --ignored --nocapture
    /// cd "$(cargo test … | tail -1)" && cargo test && cargo clippy --all-targets --all-features
    /// ```
    #[test]
    #[ignore = "writes the scaffold to a real directory for a real cargo build"]
    fn dump_scaffold_for_a_real_build() {
        // No platform list: a container is board-agnostic, so creation needs no registry at all.
        let out = embedded_scaffold("spire-demo", &[]).expect("a container");
        let root = std::env::temp_dir().join("spire-demo");
        let _ = std::fs::remove_dir_all(&root);
        for f in &out.files {
            let path = root.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
        }
        println!("{}", root.display());
    }

    /// The container's crate, relative to `root`, as the derivation names it: after the project.
    ///
    /// A temporary directory's own name — deliberately **not** `spire-embedded`. That name belongs to one
    /// project, and a fixture that reused it would pass while every other container's crate was looked
    /// for in the wrong place.
    fn container_dir(root: &Path) -> String {
        format!("crates/{}", root.file_name().unwrap().to_string_lossy())
    }

    /// A container on disk: a workspace manifest with a members list, one member per line, and the library
    /// crate that member names — because the add-operations require it to be there.
    fn container_on_disk(root: &Path) -> PathBuf {
        let member = container_dir(root);
        let name = member.trim_start_matches("crates/");
        std::fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nresolver = \"2\"\n# One crate, and it is host-checkable.\nmembers = [\n    \
                 \"{member}\",\n]\n\n[workspace.metadata.spire]\nstructure = \"embedded\"\n"
            ),
        )
        .unwrap();
        create_library(root, &member, name);
        root.join("Cargo.toml")
    }

    /// The same, with the members **inline** — which is how the framework's own manifest writes them.
    ///
    /// A second shape because one was not enough. The editor's first version inserted the new member on
    /// the line after the last `"…",` line it could find; this file has no such line, so it appended the
    /// member to the *end of the file* — inside `[workspace.package]`, as a bare string with no `=`. Cargo
    /// then refused to read the container's own manifest, and nothing in the fixture's shape could have
    /// shown that.
    fn container_on_disk_inline(root: &Path) -> PathBuf {
        let member = container_dir(root);
        let name = member.trim_start_matches("crates/");
        std::fs::write(
            root.join("Cargo.toml"),
            format!(
                "# A container with its crate set on one line.\n[workspace]\nresolver = \"2\"\n\
                 members = [\"{member}\"]\n\n[workspace.package]\nversion = \"0.1.0\"\n\
                 edition = \"2021\"\n"
            ),
        )
        .unwrap();
        create_library(root, &member, name);
        root.join("Cargo.toml")
    }

    /// The library crate a container's workspace member names.
    fn create_library(root: &Path, member: &str, name: &str) {
        let library = root.join(member);
        std::fs::create_dir_all(library.join("src/drivers")).unwrap();
        std::fs::write(
            library.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .unwrap();
    }

    /// **The routine operation**: a board's BSP, added to an existing container.
    ///
    /// The framework is scaffolded once; a BSP is what gets added as required, and its content is the
    /// board's facts — left as *typed* `todo!()`s so the crate builds and links before the fill writes
    /// them. That typing is the whole trick: an application depending on this BSP can be built and
    /// flashed while the pins are still unknown.
    #[test]
    fn a_bsp_is_added_to_the_container() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[("esp32c3", "esp-hal", Some("esp32"))]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let root = tempfile::tempdir().unwrap();
        let manifest_path = container_on_disk(root.path());

        let out = add_bsp(root.path(), "esp32c3").expect("a BSP for the pilot board");
        assert_eq!(out["crate"], "spire-bsp-esp32c3");
        assert_eq!(out["vendor_crate"], "esp-hal");

        let lib = std::fs::read_to_string(root.path().join("crates/spire-bsp-esp32c3/src/lib.rs"))
            .expect("the BSP's source");
        for expected in [
            "#![no_std]",
            "pub struct Board",
            "pub fn led(&self) -> Output<'static>",
            "todo!(\"this board's LED pin\")",
            "use esp_hal::gpio::Output;",
            "use esp_hal::peripherals::Peripherals;",
        ] {
            assert!(lib.contains(expected), "missing `{expected}`:\n{lib}");
        }

        let manifest =
            std::fs::read_to_string(root.path().join("crates/spire-bsp-esp32c3/Cargo.toml"))
                .expect("the BSP's manifest");
        // The BSP path-deps the container's own crate — named after the project, not after ours.
        let member = container_dir(root.path());
        let container = member.trim_start_matches("crates/");
        let container_dep = format!("{container} = {{ path = \"../{container}\" }}");
        for expected in [
            "name = \"spire-bsp-esp32c3\"",
            container_dep.as_str(),
            // The chip **and** `unstable`, not the chip alone: pinning the feature list as it was is what
            // let a BSP that cannot see `esp_hal::delay` pass every host test it had.
            "esp-hal = { version = \"1.2\", features = [\"esp32c3\", \"unstable\"] }",
        ] {
            assert!(
                manifest.contains(expected),
                "missing `{expected}`:\n{manifest}"
            );
        }

        // The member, in the one list — asserted by *parsing* the result. Presence was never the thing
        // that could go wrong here; the shape was.
        let after = std::fs::read_to_string(&manifest_path).unwrap();
        assert_eq!(
            workspace_members(&after).expect("the edited manifest still parses"),
            vec![
                container_dir(root.path()),
                "crates/spire-bsp-esp32c3".to_string()
            ],
            "the members stay one list:\n{after}"
        );
        assert!(after.contains("structure = \"embedded\""), "{after}");
        // And the container's prose survives. This file is not ours: it explains why the crate set is
        // what it is, and a plain TOML round-trip would have deleted every word of it.
        assert!(
            after.contains("# One crate, and it is host-checkable."),
            "{after}"
        );
    }

    /// The shape that broke it: a container whose members are listed **inline**.
    ///
    /// This is what the framework's own manifest looks like — `members = ["crates/spire-embedded"]` on one
    /// line — so `add_bsp` is run against this shape in practice, not hypothetically. The first version
    /// appended the new member to the wrong end of the file and produced a manifest cargo would not read.
    /// The assertion that catches it is not "the member is present" but "the result still parses".
    #[test]
    fn a_bsp_is_added_to_a_container_whose_members_are_inline() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[("esp32c3", "esp-hal", Some("esp32"))]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let root = tempfile::tempdir().unwrap();
        let manifest_path = container_on_disk_inline(root.path());

        add_bsp(root.path(), "esp32c3").expect("a BSP for the pilot board");

        let after = std::fs::read_to_string(&manifest_path).unwrap();
        assert_eq!(
            workspace_members(&after).expect("the edited manifest still parses"),
            vec![
                container_dir(root.path()),
                "crates/spire-bsp-esp32c3".to_string()
            ],
            "the member goes into the list, wherever the list is:\n{after}"
        );
        // The comment above `[workspace]` is still there: the container's prose is not ours to lose.
        assert!(
            after.starts_with("# A container with its crate set on one line."),
            "{after}"
        );
    }

    /// Adding the same board twice is refused — before anything is written, not after.
    #[test]
    fn adding_a_bsp_twice_is_refused() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[("esp32c3", "esp-hal", Some("esp32"))]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let root = tempfile::tempdir().unwrap();
        let manifest_path = container_on_disk(root.path());

        add_bsp(root.path(), "esp32c3").expect("the first");
        let before = std::fs::read_to_string(&manifest_path).unwrap();
        let err = add_bsp(root.path(), "esp32c3").unwrap_err();
        assert!(err.contains("already has a BSP"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&manifest_path).unwrap(),
            before,
            "a refused add must not touch the manifest"
        );
    }

    /// A BSP needs a board, and a chip whose vendor crate is known — both refusals name the reason.
    #[test]
    fn a_bsp_needs_a_board_and_a_known_vendor_crate() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[
            ("esp32s3", "esp-hal", Some("esp32")),
            ("x86-64", "linux", None),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let root = tempfile::tempdir().unwrap();
        container_on_disk(root.path());

        // A chip with no row: refused by name, and the refusal says what *is* known.
        let err = add_bsp(root.path(), "esp32s3").unwrap_err();
        assert!(
            err.contains("no BSP wiring is known for 'esp32s3'"),
            "{err}"
        );
        assert!(err.contains("esp32c3"), "{err}");

        // A host platform is not a board.
        let err = add_bsp(root.path(), "x86-64").unwrap_err();
        assert!(err.contains("not an embedded platform"), "{err}");
    }

    /// A container's library, with a driver already in it — the state a second add must respect.
    fn library_on_disk(root: &Path) {
        let member = container_dir(root);
        create_library(root, &member, member.trim_start_matches("crates/"));
        std::fs::write(
            root.join(&member).join("src/drivers/mod.rs"),
            "//! Peripheral drivers, one device per module.\n\npub mod ws2812;\n\npub use ws2812::Ws2812;\n",
        )
        .unwrap();
    }

    /// A driver is added **and registered**: the module file alone would be a file the compiler never
    /// reads, and that failure looks like the driver not existing.
    #[test]
    fn a_driver_is_added_and_registered_beside_the_others() {
        let root = tempfile::tempdir().unwrap();
        library_on_disk(root.path());

        let out = add_driver(root.path(), "BME280", "i2c").expect("a sensor driver");
        assert_eq!(out["device"], "bme280", "the name is normalized");
        assert_eq!(out["bus"], "i2c");

        let container = container_dir(root.path());
        let module =
            std::fs::read_to_string(root.path().join(&container).join("src/drivers/bme280.rs"))
                .expect("the driver's module");
        for expected in [
            "use embedded_hal::i2c::I2c;",
            "pub struct Bme280<S, D>",
            "S: I2c,",
            "todo!(\"the bme280 protocol\")",
        ] {
            assert!(module.contains(expected), "missing `{expected}`:\n{module}");
        }

        // The host test exists, is *ignored* rather than failing, and says who un-ignores it.
        let test = std::fs::read_to_string(root.path().join(&container).join("tests/bme280.rs"))
            .expect("the driver's test");
        assert!(
            test.contains("#[ignore = \"the bme280 protocol is not written yet"),
            "{test}"
        );

        // Registered beside the existing module, modules before re-exports.
        let list = std::fs::read_to_string(root.path().join(&container).join("src/drivers/mod.rs"))
            .unwrap();
        assert!(list.contains("pub mod bme280;"), "{list}");
        assert!(list.contains("pub use bme280::Bme280;"), "{list}");
        assert!(
            list.find("pub mod ws2812;").unwrap() < list.find("pub mod bme280;").unwrap(),
            "the new module joins the list rather than replacing it:\n{list}"
        );
        assert!(
            list.find("pub use ws2812::Ws2812;").unwrap()
                < list.find("pub use bme280::Bme280;").unwrap(),
            "{list}"
        );
    }

    /// The two refusals, both before anything is written: no library, and a bus nobody knows.
    #[test]
    fn a_driver_needs_a_library_and_a_known_bus() {
        // A container-shaped directory whose library crate is missing: nothing to add a driver to.
        let empty = tempfile::tempdir().unwrap();
        std::fs::write(
            empty.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\nmembers = []\n",
        )
        .unwrap();
        let err = add_driver(empty.path(), "bme280", "i2c").unwrap_err();
        assert!(err.contains("no container library at"), "{err}");

        // A bus this scaffold does not know: refused by name, and the refusal says what it takes.
        let root = tempfile::tempdir().unwrap();
        library_on_disk(root.path());
        let err = add_driver(root.path(), "bme280", "uart").unwrap_err();
        assert!(
            err.contains("'uart' is not a bus this scaffold knows"),
            "{err}"
        );
        assert!(err.contains("`spi` or `i2c`"), "{err}");
        assert!(
            !root
                .path()
                .join(container_dir(root.path()))
                .join("src/drivers/bme280.rs")
                .exists(),
            "a refused add must not leave a module behind"
        );
    }

    /// The live test: a generated BSP, **built** for its chip inside a real container.
    ///
    /// The fixtures prove the emission — the files, the content, the refusals. This proves the
    /// emission *compiles*: against the real `spire-embedded` library, against the real `esp-hal`, in
    /// a workspace whose manifests resolve. That is a check no string can make, and it is the one that
    /// catches a constructor whose shape does not typecheck against the vendor crate — a `&self` that
    /// hands out a peripheral the vendor only gives away by value, say.
    ///
    /// The container is **copied** first, so the user's checkout is never written to: a test that
    /// added a crate to it would change the thing it measures.
    ///
    /// Ignored by default: it needs the target, a container checkout and a network.
    ///
    /// ```sh
    /// SPIRE_EMBEDDED_ROOT=../spire-embedded cargo test -p spire-code --lib \
    ///     a_generated_bsp_builds_for_its_chip -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live build: needs the target, a container checkout and a network"]
    fn a_generated_bsp_builds_for_its_chip() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[("esp32c3", "esp-hal", Some("esp32"))]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let Some(source) = embedded_container() else {
            return;
        };

        // The copy keeps the container's **directory name**, because that name *is* the project name and
        // the library crate is named after the project. A copy at a random temp path is a container whose
        // crate is nowhere — which is what the first run of this test reported.
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join(source.file_name().unwrap());
        copy_sources(&source, &root);
        add_bsp(&root, "esp32c3").expect("a BSP for the pilot board");

        let toolchain = stable_toolchain();
        let output = cargo_in(
            &root,
            toolchain.as_ref(),
            &[
                "build",
                "-p",
                "spire-bsp-esp32c3",
                "--target",
                "riscv32imc-unknown-none-elf",
            ],
        );
        assert!(
            output.status.success(),
            "a generated BSP must build for its chip.\n{}",
            output_of(&output)
        );

        // Linked, not merely compiled: the `.rlib` is the artifact a dependent crate would get.
        let artifact =
            root.join("target/riscv32imc-unknown-none-elf/debug/libspire_bsp_esp32c3.rlib");
        let size = std::fs::metadata(&artifact)
            .map(|meta| meta.len())
            .unwrap_or_default();
        assert!(
            size > 0,
            "no rlib at {} (the BSP compiled nothing)",
            artifact.display()
        );
    }

    /// The live test for the **other** routine operation: a generated driver, host-compiled *and*
    /// chip-compiled inside a real container.
    ///
    /// Two builds, because two different things can be wrong and each is invisible to the other. The chip
    /// build catches a driver that quietly assumes `std`: a skeleton is generic over the bus and nothing
    /// else, so an `alloc` type or a `String` in a driver reads fine on the host and is undeclarable on a
    /// bare-metal target. The host build reads the **test file** — `cargo build` never looks inside
    /// `tests/`, so an emitted test that does not compile would sit there unnoticed until someone ran it,
    /// which for a driver whose test is `#[ignore]`d is nobody.
    ///
    /// Ignored by default: it needs the target, a container checkout and a network.
    ///
    /// ```sh
    /// SPIRE_EMBEDDED_ROOT=../spire-embedded cargo test -p spire-code --lib \
    ///     a_generated_driver_compiles_for_its_chip_and_on_the_host -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live build: needs the target, a container checkout and a network"]
    fn a_generated_driver_compiles_for_its_chip_and_on_the_host() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[("esp32c3", "esp-hal", Some("esp32"))]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let Some(source) = embedded_container() else {
            return;
        };

        // The copy keeps the container's directory name — the project name the crate is named after.
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join(source.file_name().unwrap());
        copy_sources(&source, &root);
        add_driver(&root, "bme280", "i2c").expect("a driver for an i2c device");
        // The copied container's crate, by the same derivation the add used. It is `spire-embedded` here
        // because that is *this* container's project name — not because the name is fixed.
        let container = container_crate(&root).expect("the copied container's crate");

        let toolchain = stable_toolchain();
        // The module the emitter registered is a module the compiler actually reads — including the
        // `pub use` line beside it, which names the type it re-exports.
        let chip = cargo_in(
            &root,
            toolchain.as_ref(),
            &[
                "build",
                "-p",
                container.as_str(),
                "--target",
                "riscv32imc-unknown-none-elf",
            ],
        );
        assert!(
            chip.status.success(),
            "a generated driver must compile for the chip.\n{}",
            output_of(&chip)
        );

        // And the emitted *test* compiles, on the host, where a test can run at all.
        let host = cargo_in(
            &root,
            toolchain.as_ref(),
            &["test", "-p", container.as_str(), "--no-run"],
        );
        assert!(
            host.status.success(),
            "the emitted driver test must compile on the host.\n{}",
            output_of(&host)
        );

        // Exit status is not evidence: cargo can succeed without building anything, and a test that only
        // checks a status cannot tell that from a real compile. The artifacts can.
        let rlib = root.join(format!(
            "target/riscv32imc-unknown-none-elf/debug/lib{}.rlib",
            container.replace('-', "_")
        ));
        let size = std::fs::metadata(&rlib)
            .map(|meta| meta.len())
            .unwrap_or_default();
        assert!(
            size > 0,
            "no rlib at {} (the chip build compiled nothing)",
            rlib.display()
        );

        // The driver's test binary: the file `cargo build` never reads, compiled because it is a test.
        let deps = root.join("target/debug/deps");
        let bme280: Vec<String> = std::fs::read_dir(&deps)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .filter(|name| name.starts_with("bme280-"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !bme280.is_empty(),
            "no `bme280-*` test binary in {} — the emitted test was never compiled",
            deps.display()
        );

        println!(
            "driver compiled for the chip ({} bytes) and its host test built ({} binaries)",
            size,
            bme280.len()
        );
    }

    /// The live test: scaffold a container and **build and test it** on the host.
    ///
    /// The framework files come from the checkout, which is already proven — but the *workspace* is this
    /// scaffold's own emission: the manifest, the members, the crate's `[[test]]` tables, the empty
    /// `drivers/` module. Only a real cargo run says whether those fit together, and it is cheap here for
    /// the very reason the container exists: no vendor crate anywhere in the graph, so a host build is the
    /// whole check.
    ///
    /// Ignored by default: it needs a network (the crate's two dependencies come from crates.io).
    ///
    /// ```sh
    /// cargo test -p spire-code --lib a_scaffolded_container_builds -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live build: needs a network for the container's dependencies"]
    fn a_scaffolded_container_builds_and_tests_on_the_host() {
        let out = embedded_scaffold("container-smoke", &[]).expect("a container");

        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("container-smoke");
        for file in &out.files {
            let target = root.join(&file.path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, &file.content).unwrap();
        }

        let toolchain = stable_toolchain();
        let output = cargo_in(&root, toolchain.as_ref(), &["test"]);
        assert!(
            output.status.success(),
            "a freshly scaffolded container must build and test.\n{}",
            output_of(&output)
        );

        // The verdict is the summary line, not the exit code: a container that silently ran *no* tests
        // would exit 0, and this test exists to catch a workspace whose `[[test]]` tables point at files
        // that are not there.
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("test result: ok."),
            "no test ran in the scaffolded container:\n{stdout}\n{}",
            output_of(&output)
        );
        let passed: u32 = stdout
            .split("test result: ok. ")
            .filter_map(|rest| rest.split_whitespace().next())
            .filter_map(|n| n.parse::<u32>().ok())
            .sum();
        assert!(
            passed > 0,
            "the container ran no tests at all:\n{stdout}\n{}",
            output_of(&output)
        );
    }

    /// Copy a container's sources, skipping what a build regenerates or git owns.
    fn copy_sources(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let name = entry.file_name();
            let text = name.to_string_lossy();
            if matches!(text.as_ref(), "target" | ".git" | ".embuild") {
                continue;
            }
            let source = entry.path();
            let target = to.join(&name);
            if source.is_dir() {
                std::fs::create_dir_all(&target).unwrap();
                copy_sources(&source, &target);
            } else {
                std::fs::copy(&source, &target).unwrap();
            }
        }
    }

    /// The container the live tests build against, if this machine has one.
    ///
    /// `SPIRE_EMBEDDED_ROOT` first, then the sibling checkout — and `None`, with a note, rather than a
    /// panic when neither exists: these tests are also run on machines with no container checkout and no
    /// bare-metal target, where the honest outcome is a skip.
    fn embedded_container() -> Option<PathBuf> {
        let source = std::env::var("SPIRE_EMBEDDED_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spire-embedded")
            });
        if container_crate(&source).is_err() {
            println!(
                "no container checkout at {} — set SPIRE_EMBEDDED_ROOT to build this",
                source.display()
            );
            return None;
        }
        Some(source)
    }

    /// `cargo`, from a toolchain the caller found, in a container.
    ///
    /// The toolchain is passed in rather than looked up here — see [`stable_toolchain`] for why the one a
    /// machine happens to have first is not good enough. Both live tests need the same environment, so it
    /// is assembled in one place.
    fn cargo_in(dir: &Path, toolchain: Option<&PathBuf>, args: &[&str]) -> std::process::Output {
        let mut command = std::process::Command::new(
            toolchain
                .map(|dir| dir.join("bin/cargo"))
                .unwrap_or_else(|| PathBuf::from("cargo")),
        );
        command.args(args).current_dir(dir);
        if let Some(dir) = toolchain {
            command
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        dir.join("bin").display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .env("DYLD_FALLBACK_LIBRARY_PATH", dir.join("lib"));
        }
        command.output().expect("cargo runs")
    }

    /// Both streams of a failed command, for an assertion message.
    ///
    /// The success case needs neither; the failure case is unreadable without them, and a live test that
    /// says only "it failed" costs a rerun to learn anything.
    fn output_of(output: &std::process::Output) -> String {
        format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    /// The machine's stable rustup toolchain, if it has one.
    ///
    /// A bare-metal target needs *that* toolchain's `core`, and the `cargo` a machine happens to have
    /// first may know nothing about it — which fails as "can't find crate for `core`" and reads like a
    /// missing target rather than a missing toolchain. (The app scaffold's live test carries its own
    /// copy: they are separate test modules with no shared home yet.)
    fn stable_toolchain() -> Option<PathBuf> {
        let toolchains =
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".rustup/toolchains");
        let mut stable: Vec<PathBuf> = std::fs::read_dir(&toolchains)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .map(|name| name.to_string_lossy().starts_with("stable-"))
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        stable.sort();
        stable.pop()
    }
}
