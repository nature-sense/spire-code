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

/// What a backend crate needs for one board family.
///
/// The scaffold carries this because it is the knowledge that is expensive to rediscover — the
/// vendor crate, the one non-obvious fact about it, and whether the family has `std`. The fill
/// prompt reads the same facts from here rather than restating them: a second copy would drift
/// the moment either one changed.
pub(crate) struct FamilySpec {
    /// The vendor HAL crate: prose only (the backend's description and doc header).
    pub(crate) vendor_crate: &'static str,
    /// The crates this backend builds on: the vendor HAL first, then whatever its API needs in
    /// scope to be callable at all.
    ///
    /// A list rather than one line because the count is a **measured** fact, not a rule: esp-idf-hal
    /// re-exports the sys crate under it (one is right), while rp2040-hal's GPIO and timer methods
    /// are `embedded-hal`/`nb` traits it does not re-export (one is not enough). The first backend a
    /// model filled failed to compile on exactly that difference.
    deps: &'static [&'static str],
    /// The note emitted next to it, for the one thing that is easy to get wrong.
    vendor_note: &'static str,
    /// True when this family's executor is the shared std one.
    ///
    /// Under esp-idf a `std::thread` **is** a FreeRTOS task, so the std executor is the ESP32
    /// executor. An rp2040 has no OS at all: that backend supplies its own `Spawner`, which is
    /// why it does not depend on the `-std` crate.
    pub(crate) uses_std_executor: bool,
    /// How this family is actually built, for the README — the command that was made to work, with
    /// the one thing that fails in a way that does not name its cause.
    ///
    /// Per family, because the two are nothing alike: an esp-idf build needs the espup toolchain,
    /// `-Zbuild-std` and `MCU`, while an rp2040 build needs only a target that is installed. It is
    /// *data* rather than a paragraph in the README template so that a project which gains a board
    /// later (`embedded_hal_add_platform`) can append this family's block instead of the tool
    /// silently leaving the README describing one board. `__CRATE__` is the backend crate's name.
    pub(crate) readme_build: &'static str,
}

/// The vendor HAL for a board family, or `None` when this scaffold does not know it.
///
/// Returning `None` rather than defaulting is what stops the scaffold from emitting a backend
/// crate that cannot build: every entry here was checked against a real build.
pub(crate) fn family_spec(family: &str) -> Option<FamilySpec> {
    match family {
        "esp32" => Some(FamilySpec {
            vendor_crate: "esp-idf-hal",
            deps: &["esp-idf-hal = \"0.47\""],
            vendor_note: "# One dependency, not two: esp-idf-hal re-exports esp-idf-sys as\n\
                          # `esp_idf_hal::sys`. There are no per-chip features — the chip is the\n\
                          # `MCU` environment variable esp-idf-sys reads, plus `--target`.",
            uses_std_executor: true,
            readme_build: "# esp32 family (std, esp-idf). The toolchain and the SDK are installed\n\
                           # **outside this project** and only referenced by environment:\n\
                           # `espup install` provides the `esp` rustup toolchain (and the clang\n\
                           # bindgen needs), and esp-idf-sys downloads ESP-IDF itself on the first\n\
                           # build — shared machine-wide by ESP_IDF_TOOLS_INSTALL_DIR=global, so a\n\
                           # second project does not copy 5 GB. Spire reads ESP_TOOLCHAIN_BIN (else\n\
                           # RUSTUP_HOME, else ~/.rustup) and honours an exported LIBCLANG_PATH; by\n\
                           # hand `source ~/export-esp.sh` covers both. The FIRST build compiles\n\
                           # ESP-IDF and takes minutes; later builds are seconds.\n\
                           MCU=esp32 cargo build --target xtensa-esp32-espidf \\\n\
                           \x20   -Zbuild-std=std,panic_abort -p __CRATE__",
        }),
        "rp2040" => Some(FamilySpec {
            vendor_crate: "rp2040-hal",
            // `embedded-hal` and `cortex-m` are here at the versions `rp2040-hal` 0.10.2 itself
            // depends on — `embedded-hal = "1.0.0"` and `cortex-m = "0.7.2"`, both straight out of
            // its manifest. Names alone are not enough: a backend declaring `embedded-hal = "0.2"`
            // pulls in a second copy of the crate whose `OutputPin` is not the one implemented on
            // `Pin`, so `set_high` never resolves; and a backend reaching for `cortex_m::asm::nop`
            // rp2040-hal *uses* `cortex-m` without re-exporting it (a real model wrote a `nop` spin
            // for `DelayMs` and pinned the belief that the re-export existed). Both were measured
            // from the compiler, and both are pinned here so they cannot regress silently.
            deps: &[
                "rp2040-hal = \"0.10\"",
                "embedded-hal = \"1\"",
                "cortex-m = \"0.7\"",
            ],
            vendor_note:
                "# The blocking HAL: this backend's actor executor is synchronous, so it\n\
                          # needs no async runtime. (An embassy-rp backend is the async\n\
                          # alternative, and would still drive the same synchronous `handle`.)\n\
                          # `embedded-hal` 1.x and `cortex-m` 0.7 are here because rp2040-hal's GPIO\n\
                          # and delay methods *are* their traits and it re-exports neither.",
            uses_std_executor: false,
            readme_build: "# rp2040 family (no_std).\n\
                           cargo build --target thumbv6m-none-eabi -p __CRATE__",
        }),
        _ => None,
    }
}

/// The contract crate's name for a project: `<name>-hal`, normalized to a valid crate id.
fn contract_crate(project_name: &str) -> String {
    format!(
        "{}-hal",
        project_name.trim().to_lowercase().replace(' ', "-")
    )
}

/// Emit the embedded-HAL workspace.
///
/// `platforms` are registry ids (the wizard's selection, filtered to embedded platforms); they
/// are collapsed to their **families** here, so selecting esp32c6 and esp32s3 produces one
/// backend crate, not two. An id whose family this scaffold does not know is refused by name:
/// a backend crate that cannot build is worse than no crate.
pub(crate) fn embedded_scaffold(
    project_name: &str,
    platforms: &[String],
) -> Result<super::ScaffoldOutput, String> {
    let hal = contract_crate(project_name);
    let hal_id = hal.replace('-', "_");
    let display = project_name.trim();

    // Platform id → family, deduplicated and sorted: the crate set must not depend on the order
    // the wizard happened to list the selection in.
    let mut families: Vec<(String, FamilySpec)> = Vec::new();
    for id in platforms {
        let platform = crate::platform::Platform::from_registry(id)
            .ok_or_else(|| format!("unknown platform '{id}' (see ~/.spire/platforms)"))?;
        if !platform.is_embedded() {
            return Err(format!(
                "platform '{id}' is not an embedded platform (os '{}'); the embedded-HAL \
                 project type needs boards it can flash",
                platform.os
            ));
        }
        let family = platform.family.clone().ok_or_else(|| {
            format!("platform '{id}' names no `family`, so no backend can be chosen")
        })?;
        if families.iter().any(|(known, _)| known == &family) {
            continue;
        }
        let spec = family_spec(&family).ok_or_else(|| {
            format!("no backend is known for family '{family}' (platform '{id}')")
        })?;
        families.push((family, spec));
    }
    families.sort_by(|a, b| a.0.cmp(&b.0));

    let mut files = vec![
        super::ScaffoldFile {
            path: "Cargo.toml".to_string(),
            content: workspace_manifest(display, &hal, &families),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "README.md".to_string(),
            content: readme(display, &hal, &families),
            structural: true,
            ..Default::default()
        },
    ];
    files.extend(contract_files(&hal, &hal_id, display));
    files.extend(std_executor_files(&hal, &hal_id));
    for (family, spec) in &families {
        files.extend(backend_files(&hal, &hal_id, family, spec));
    }

    // Fillable leaves: the contract that is authored, and each backend's implementation.
    let mut fill_roots = vec![format!("crates/{hal}/src"), format!("crates/{hal}-std/src")];
    fill_roots.extend(
        families
            .iter()
            .map(|(family, _)| format!("crates/{hal}-{family}/src")),
    );

    Ok(super::ScaffoldOutput {
        build_file: "Cargo.toml".to_string(),
        build_content: workspace_manifest(display, &hal, &families),
        source_dir: "crates".to_string(),
        source_file: format!("crates/{hal}/src/lib.rs"),
        source_content: contract_lib(&hal_id),
        files,
        platform_targets: platforms.to_vec(),
        fill_roots,
        // The backend dependency tables. They are edited through the module's
        // `declare_dependencies`, never a raw write — the rule every other scaffold follows.
        dependency_sections: families
            .iter()
            .map(|(family, _)| format!("crates/{hal}-{family}/Cargo.toml"))
            .collect(),
        structure: ProjectStructure::Embedded,
        embedded: true,
    })
}

/// The workspace manifest — and the **marker** that makes this project recognizable.
///
/// `[workspace.metadata.spire] structure = "embedded"` is what `cargo.rs::analyze` reads:
/// there is no layout guess, and the value is the enum's own key rather than a prettier spelling,
/// so the marker and the project type cannot drift apart.
fn workspace_manifest(display: &str, hal: &str, families: &[(String, FamilySpec)]) -> String {
    let members: String = families
        .iter()
        .map(|(family, _)| format!("    \"crates/{hal}-{family}\",\n"))
        .collect();
    let backends: String = families
        .iter()
        .map(|(family, _)| format!("#   crates/{hal}-{family} — backend: {family}\n"))
        .collect();

    r#"# __DISPLAY__ — the embedded HAL: one contract, one backend per board family.
#
#   crates/__HAL__ — the contract (no_std, dependency-free): what firmware programs
#   crates/__HAL__-std — the std actor executor: host-testable, reused by std families
__BACKENDS__#
# Check the contract with no cross toolchain and no vendor SDK:
#
#     cargo test -p __HAL__
#
# Building a backend needs its own target; see README.md for the per-family commands.

[workspace]
resolver = "2"
members = [
    "crates/__HAL__",
    "crates/__HAL__-std",
__MEMBERS__]

# A host `cargo test` must not need a cross toolchain or a vendor SDK, so the backends are not
# default members. Building one explicitly still works, and is how it gets verified.
default-members = ["crates/__HAL__", "crates/__HAL__-std"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-3.0-or-later"

# Declares this project's type to Spire. The value is `ProjectStructure::Embedded`'s own key.
[workspace.metadata.spire]
structure = "embedded"
"#
    .replace("__DISPLAY__", display)
    .replace("__BACKENDS__", &backends)
    .replace("__MEMBERS__", &members)
    .replace("__HAL__", hal)
}

/// One family's README build block, templated with the backend crate's name.
///
/// Shared by the scaffold (which renders all of a project's families) and
/// `embedded_hal_add_platform` (which appends the one family a project just gained) — so a README
/// written at creation and one grown later cannot disagree about how a board is built.
pub(crate) fn readme_family_block(hal: &str, family: &str, spec: &FamilySpec) -> String {
    spec.readme_build
        .replace("__CRATE__", &format!("{hal}-{family}"))
}

/// The README: what the shape is, and how each family is actually built.
///
/// The commands are the ones that were made to work, because each fails in a way that does not
/// name its real cause — a README that just says "build it" is how that knowledge gets lost. They
/// are rendered **per selected family** (`FamilySpec::readme_build`): a one-board project must not
/// advertise another board's toolchain, and a board added later must be able to extend the same
/// section rather than find it hard-coded.
fn readme(display: &str, hal: &str, families: &[(String, FamilySpec)]) -> String {
    let rows: String = families
        .iter()
        .map(|(family, _)| format!("- `crates/{hal}-{family}` — the {family} backend\n"))
        .collect();
    let builds: String = families
        .iter()
        .map(|(family, spec)| readme_family_block(hal, family, spec))
        .collect::<Vec<_>>()
        .join("\n\n");
    r#"# __DISPLAY__ — embedded HAL

One **contract**, one **backend per board family**: firmware depends on `__HAL__` and never on a
vendor SDK, so swapping boards is a backend plus a target triple rather than a rewrite.

- `crates/__HAL__` — the contract: `actor::{Actor, Mailbox, Spawner}` plus the HAL traits.
  `no_std` and dependency-free, so it compiles for every family *and* for the host.
- `crates/__HAL__-std` — the std actor executor (one thread per actor, one bounded queue per
  mailbox). Used by std families; a `no_std` family supplies its own `Spawner` instead.
__ROWS__
## Check the contract (anywhere)

```sh
cargo test -p __HAL__
```

No cross toolchain, no vendor SDK — that is the point of the seam, and why the backends are not
default members.

## Build a backend

Each family needs its own toolchain and target triple: the platform registry entry
(`~/.spire/platforms/*.yaml`) carries the triple, and for esp-idf the `MCU` variable names the
chip. Spire passes both when it builds the project; by hand:

```sh
__BUILDS__
```

"#
    .replace("__DISPLAY__", display)
    .replace("__ROWS__", &rows)
    .replace("__BUILDS__", &builds)
    .replace("__HAL__", hal)
}

/// The contract crate: `no_std`, `embedded-hal` re-exported, host-testable.
///
/// **Nothing here is fillable, and that is the change.** The peripheral traits are `embedded-hal`'s
/// — every vendor HAL and every driver crate implements them, so authoring our own was the
/// re-invention that stood between this project and the ecosystem. What the crate still owns is the
/// *actor* contract (`actor.rs`), and what a backend now owes is its **board**: constructors, not a
/// trait implementation, because the compiler already enforces the traits.
fn contract_files(hal: &str, hal_id: &str, display: &str) -> Vec<super::ScaffoldFile> {
    let structural = |path: String, content: String| super::ScaffoldFile {
        path,
        content,
        structural: true,
        ..Default::default()
    };

    vec![
        structural(
            format!("crates/{hal}/Cargo.toml"),
            [
                "[package]",
                &format!("name = \"{hal}\""),
                &format!(
                    "description = \"The board-family-agnostic seam for {display} firmware: the \
                     actor contract plus the embedded-hal traits a project programs against.\""
                ),
                "version.workspace = true",
                "edition.workspace = true",
                "license.workspace = true",
                "",
                "# One dependency, and it is the point of the crate: the peripheral traits are the",
                "# ecosystem's (`OutputPin`, `DelayNs`, `I2c`, …), re-exported so the project names",
                "# one version of them — the one its backends were compiled against. A trait of our",
                "# own would be one no driver crate could be used with.",
                "#",
                "# Everything else stays out: a further dependency is one more thing that can drag",
                "# `std`, a vendor SDK or an allocator into the seam. A backend is where",
                "# dependencies belong.",
                "[dependencies]",
                "embedded-hal = \"1.0\"",
                "",
            ]
            .join("\n"),
        ),
        structural(format!("crates/{hal}/src/lib.rs"), contract_lib(hal_id)),
        structural(format!("crates/{hal}/src/actor.rs"), ACTOR_RS.to_string()),
    ]
}
/// The contract crate's `lib.rs`. `no_std` outside tests, so the host can still test it — which is
/// what makes the abstraction verifiable without hardware.
fn contract_lib(hal_id: &str) -> String {
    format!(
        "//! `{hal_id}` — the board-family-agnostic seam for this firmware project.\n\
         //!\n\
         //! Two layers, and only one of them is ours to design:\n\
         //!\n\
         //! 1. **The peripheral traits are `embedded-hal`'s**, re-exported below. A GPIO is a\n\
         //!    `digital::OutputPin`, a delay is a `delay::DelayNs`, a bus is an `i2c::I2c` or a\n\
         //!    `spi::SpiDevice`. Every vendor HAL implements them and so does every driver crate,\n\
         //!    so this crate does not define its own: a trait of ours is a trait no driver can be\n\
         //!    used with.\n\
         //!\n\
         //! 2. **The actor contract is ours** (`actor`), because no ecosystem crate has one and\n\
         //!    because it is the unit the drift measure reads: a message with no handler is a\n\
         //!    missing implementation.\n\
         //!\n\
         //! 3. **Devices are upstream crates by default.** A sensor, a display or a strip has a\n\
         //!    crate written against these traits, and it drops onto a `Board` bus without an\n\
         //!    adapter. Writing a driver is the fallback — for a device nobody has one for, or for\n\
         //!    one that has to be actor-shaped — and its shape is then fixed: generic over the\n\
         //!    traits, vendor types left in the backend, and host-tested against fakes. A driver\n\
         //!    that names a chip is a driver that works on one chip.\n\
         //!\n\
         //! A board family is a **backend** crate, and what it supplies is constructors —\n\
         //! `Board::led`, `Board::delay` — not a trait implementation: the compiler already\n\
         //! enforces the traits. Nothing here knows about a vendor SDK, an OS or an executor; the\n\
         //! executor is injected (see `actor::Spawner`).\n\
         \n\
         #![cfg_attr(not(test), no_std)]\n\
         \n\
         pub mod actor;\n\
         \n\
         /// The ecosystem peripheral traits, at the version the backends were compiled against.\n\
         ///\n\
         /// Re-exported rather than left to each crate so the project names one version of\n\
         /// `embedded-hal`: two copies of it in one workspace are two different `OutputPin`\n\
         /// traits, and the error reads like the wrong import.\n\
         pub use embedded_hal;\n"
    )
}

/// The actor contract: the same *shape* as the host's `spire_actor::Actor`, synchronously.
///
/// Structural, because this is the seam: it is what lets the same actor run on a FreeRTOS task
/// and on a bare-metal scheduler, and it is deliberately not something a fill phase rewrites.
const ACTOR_RS: &str = r#"//! The on-device actor contract.
//!
//! Deliberately the same *shape* as the host's `spire_actor::Actor` — an associated `Message`
//! type and a `handle` that consumes one — with one difference that belongs to the runtime
//! rather than the design: the host's is `async` on a tokio task, this one is synchronous.
//!
//! Nothing here spawns anything. The executor belongs to the backend — see [`Spawner`] — so the
//! same actor code is what a backend swaps a queue and a task underneath.

/// Something that consumes messages, one at a time.
pub trait Actor {
    /// What this actor understands.
    ///
    /// For a HAL module this is also its **contract**: the message set *is* the interface, which
    /// is what lets the drift analysis measure firmware the way it measures a HAL.
    type Message;

    /// Consume one message.
    ///
    /// Must not block indefinitely: the executor is a single task, so a long `handle` starves
    /// every other message to this actor. A backend that needs waiting puts it *between*
    /// messages, not inside this call.
    fn handle(&mut self, msg: Self::Message);
}

/// A handle to a *running* actor — the only thing a producer holds.
///
/// No `std`, no allocator, no knowledge of queues: the backend decides the storage (a FreeRTOS
/// queue, a static ring). Sending is non-blocking, so a producer can never be parked by a busy
/// consumer.
pub trait Mailbox {
    type Message;

    /// Deliver, or hand the message back.
    ///
    /// Returning it on failure is deliberate: a dropped message in firmware is a silent bug, so
    /// the discard is forced to be explicit at the call site.
    fn try_send(&self, msg: Self::Message) -> Result<(), SendError<Self::Message>>;
}

/// The message a [`Mailbox`] refused to take, returned to the sender.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<M>(pub M);

/// The executor seam: a backend spawns an actor and hands back a mailbox.
///
/// This is the *only* thing a board family has to supply for the actor model to work. A backend
/// owns the spawned actor's storage, which is why this takes the actor **by value**: under
/// esp-idf that storage is a heap allocation, and on a `no_std` family it is a static — the
/// difference stays behind this trait.
pub trait Spawner {
    /// The mailbox type this backend produces.
    type Mailbox<M>: Mailbox<Message = M>;

    /// Start `actor` on the backend's executor.
    fn spawn<A>(&self, actor: A) -> Self::Mailbox<A::Message>
    where
        A: Actor + Send + 'static,
        A::Message: Send + 'static;
}
"#;

/// The std executor — a crate of its own because it is neither ESP32-specific nor `no_std`.
///
/// Structural: this is machinery. A family that needs a different executor supplies one as its
/// own `Spawner` rather than editing this.
fn std_executor_files(hal: &str, hal_id: &str) -> Vec<super::ScaffoldFile> {
    vec![
        super::ScaffoldFile {
            path: format!("crates/{hal}-std/Cargo.toml"),
            content: format!(
                "[package]\n\
                 name = \"{hal}-std\"\n\
                 description = \"The std actor executor for {hal}: one thread (a FreeRTOS task under \
                 esp-idf) per actor, one bounded queue per mailbox.\"\n\
                 version.workspace = true\n\
                 edition.workspace = true\n\
                 license.workspace = true\n\
                 \n\
                 [dependencies]\n\
                 {hal} = {{ path = \"../{hal}\" }}\n"
            ),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: format!("crates/{hal}-std/src/lib.rs"),
            content: STD_EXECUTOR_RS.replace("__HAL_ID__", hal_id),
            structural: true,
            ..Default::default()
        },
    ]
}

/// The std executor's source. `__HAL_ID__` is the contract crate in identifier form.
const STD_EXECUTOR_RS: &str = r#"//! The **std executor** for the contract.
//!
//! Not ESP32-specific: under esp-idf `std::thread` **is** a FreeRTOS task and
//! `mpsc::sync_channel` is a bounded queue, so "the FreeRTOS executor" is std's, with no custom
//! task or queue code to write or trust. The same executor also runs on a Linux host, which is
//! what makes it testable at all.
//!
//! A `no_std` family cannot use this crate and supplies its own `Spawner`. That is the seam
//! working as intended, not a gap.

use __HAL_ID__::actor::{Actor, Mailbox, SendError, Spawner};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};

/// How many messages a mailbox holds before `try_send` hands the message back.
pub const DEFAULT_MAILBOX_DEPTH: usize = 16;

/// A bounded mailbox, backed by `std::sync::mpsc`.
pub struct QueueMailbox<M> {
    tx: SyncSender<M>,
}

impl<M> Mailbox for QueueMailbox<M> {
    type Message = M;

    fn try_send(&self, msg: M) -> Result<(), SendError<M>> {
        self.tx.try_send(msg).map_err(|e| match e {
            TrySendError::Full(msg) => SendError(msg),
            TrySendError::Disconnected(msg) => SendError(msg),
        })
    }
}

/// Spawns each actor on its own thread — a FreeRTOS task under esp-idf.
pub struct StdSpawner;

impl Spawner for StdSpawner {
    type Mailbox<M> = QueueMailbox<M>;

    fn spawn<A>(&self, mut actor: A) -> Self::Mailbox<A::Message>
    where
        A: Actor + Send + 'static,
        A::Message: Send + 'static,
    {
        let (tx, rx) = sync_channel::<A::Message>(DEFAULT_MAILBOX_DEPTH);
        std::thread::spawn(move || {
            while let Ok(msg) = rx.recv() {
                actor.handle(msg);
            }
        });
        QueueMailbox { tx }
    }
}
"#;

/// One backend crate per family: the vendor HAL behind the contract's traits.
///
/// The manifest is **structural** — the vendor dependency and the executor choice are not the
/// model's to invent — while `src/lib.rs` is fillable (`fill_role: HalImplementation`):
/// implementing the contract for this board is exactly what the fill phase is for.
pub(crate) fn backend_files(
    hal: &str,
    hal_id: &str,
    family: &str,
    spec: &FamilySpec,
) -> Vec<super::ScaffoldFile> {
    let mut manifest = format!(
        "[package]\n\
         name = \"{hal}-{family}\"\n\
         description = \"The {family} backend for {hal}: {} behind the board-agnostic traits.\"\n\
         version.workspace = true\n\
         edition.workspace = true\n\
         license.workspace = true\n\
         \n\
         [dependencies]\n\
         {hal} = {{ path = \"../{hal}\" }}\n",
        spec.vendor_crate
    );
    if spec.uses_std_executor {
        manifest.push_str(&format!("{hal}-std = {{ path = \"../{hal}-std\" }}\n"));
    }
    for dep in spec.deps {
        manifest.push_str(dep);
        manifest.push('\n');
    }
    manifest.push_str("\n\n");
    manifest.push_str(spec.vendor_note);
    manifest.push('\n');

    vec![
        super::ScaffoldFile {
            path: format!("crates/{hal}-{family}/Cargo.toml"),
            content: manifest,
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: format!("crates/{hal}-{family}/src/lib.rs"),
            content: backend_lib(hal_id, family, spec),
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::HalImplementation),
        },
    ]
}

/// The backend's source — the fillable half, assembled from the template plus the executor
/// paragraph that differs by family.
fn backend_lib(hal_id: &str, family: &str, spec: &FamilySpec) -> String {
    // The snippet is resolved *before* it is spliced in: a placeholder inside injected text is
    // not seen by a `replace` pass over the template it is injected into, which silently emits
    // `__HAL_ID__` into the user's project.
    let executor = if spec.uses_std_executor {
        STD_EXECUTOR_REEXPORT.replace("__HAL_ID__", hal_id)
    } else {
        OWN_EXECUTOR_STUB.to_string()
    };
    BACKEND_LIB_RS
        .replace("__FAMILY__", family)
        .replace("__VENDOR__", spec.vendor_crate)
        .replace("__HAL_ID__", hal_id)
        .replace(
            "__NO_STD__",
            if spec.uses_std_executor {
                ""
            } else {
                "#![no_std]\n"
            },
        )
        .replace("__EXECUTOR__", &executor)
}
/// The backend template. Constructors with `unimplemented!()` bodies — see the header.
///
/// `__NO_STD__` is the crate-level attribute a `no_std` family needs and a `std` one must not have:
/// a `thumbv6m` target has no `std` to link, and an esp-idf backend that declared `no_std` could not
/// call `std::thread` — which *is* its executor. It sits after the `//!` block because inner
/// attributes may follow inner doc comments, and before `use` because that is where an inner
/// attribute must be.
const BACKEND_LIB_RS: &str = r#"//! The __FAMILY__ backend: the board, over `__VENDOR__`.
//!
//! Types from `__VENDOR__` live here and nowhere else: an actor holds the *traits* — `embedded-hal`'s
//! `OutputPin`, `DelayNs`, … — so it does not change when the board does.
//!
//! What this crate supplies is **constructors**, not a trait implementation: the compiler already
//! enforces the peripheral contract, so what is left is the board's own facts — which pin, which
//! polarity, which delay. The stub bodies below are `unimplemented!()` on purpose: the board measure
//! reports them as unfinished and the fill writes them, and a stub that quietly did nothing would
//! look finished, which is worse than one that fails loudly.
__NO_STD__
use __HAL_ID__::embedded_hal::delay::DelayNs;
use __HAL_ID__::embedded_hal::digital::{ErrorType, OutputPin};

__EXECUTOR__
/// This board family's constructors.
///
/// A **type**, not a trait: a `trait Board` would be a contract of our own invention again, one level
/// up, and the peripheral contract is already the compiler's job.
pub struct Board;

/// What `led` returns until its body is written.
///
/// A concrete type rather than `impl OutputPin` with a diverging body: `-> impl Trait { unimplemented!() }`
/// does not **compile** — for a body that never returns the compiler infers the hidden type as `()`, and
/// `()` is not an output pin. It implements the trait *for real* so the application above it can be
/// built, linked and flashed before this backend is filled, and every method panics: reaching one would
/// mean the constructor it came from had not been written.
pub struct UnimplementedLed;

impl ErrorType for UnimplementedLed {
    type Error = core::convert::Infallible;
}

impl OutputPin for UnimplementedLed {
    fn set_high(&mut self) -> Result<(), Self::Error> {
        unimplemented!("Board::led has not been written yet")
    }

    fn set_low(&mut self) -> Result<(), Self::Error> {
        unimplemented!("Board::led has not been written yet")
    }
}

/// What `delay` returns until its body is written. Same reason as [`UnimplementedLed`].
pub struct UnimplementedDelay;

impl DelayNs for UnimplementedDelay {
    fn delay_ns(&mut self, _ns: u32) {
        unimplemented!("Board::delay has not been written yet")
    }
}

impl Board {
    /// The LED, as an `embedded-hal` output.
    ///
    /// TODO: take this family's pin type, hand it to __VENDOR__'s own output driver, and return that
    /// — it already implements `OutputPin`. An active-low LED inverts here, once, so nothing above
    /// the board has to know.
    pub fn led() -> UnimplementedLed {
        unimplemented!("Board::led")
    }

    /// This family's blocking delay, as an `embedded-hal` `DelayNs`.
    ///
    /// TODO: return __VENDOR__'s delay type — it already implements `DelayNs`.
    pub fn delay() -> UnimplementedDelay {
        unimplemented!("Board::delay")
    }
}
"#;

/// For a `std` family the executor is the shared one — no per-family task code to write.
const STD_EXECUTOR_REEXPORT: &str = r#"/// The executor is not this family's: under esp-idf a `std::thread` **is** a FreeRTOS task, so
/// the shared std executor is this backend's executor too.
pub use __HAL_ID___std::StdSpawner;"#;

/// For a `no_std` family the executor is this backend's own obligation.
const OWN_EXECUTOR_STUB: &str = r#"/// A `no_std` family supplies its own executor: `Spawner` is the backend's only obligation.
///
/// Start synchronous — a static slot per actor, a ring mailbox, and a `run()` that drains them —
/// because the contract's `handle` is synchronous and needs no async runtime. An async executor
/// can come later without touching the contract.
///
/// TODO: `impl Spawner for StaticSpawner`, the static mailbox, and `run()`.
pub struct StaticSpawner;"#;

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

/// The container's library crate — the actor framework, the drivers and the executors.
///
/// **One name, not one per project.** The container is a singleton: applications of this family are
/// built against it, and each names it by this same string wherever it lives.
const EMBEDDED_CRATE: &str = "spire-embedded";

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
        .replace("__FEATURES__", &features);
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
# The actor system and the peripherals module, for the trait names this crate's signatures use.
spire-embedded = { path = "../spire-embedded" }
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

    let files = bsp_files(platform_id, vendor, version, features);
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
pub(crate) fn driver_files(device: &str, bus: &str) -> Result<Vec<super::ScaffoldFile>, String> {
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
            path: format!("crates/spire-embedded/src/drivers/{id}.rs"),
            content: fill(DRIVER_RS),
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::Shared),
        },
        super::ScaffoldFile {
            path: format!("crates/spire-embedded/tests/{id}.rs"),
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
    // Validates the bus, and refuses by name if it is one this scaffold does not know.
    let files = driver_files(device, bus)?;
    let id = driver_id(device);
    let type_name = driver_type(&id);

    let library = root.join("crates").join(EMBEDDED_CRATE);
    if !library.join("Cargo.toml").is_file() {
        return Err(format!(
            "no `{EMBEDDED_CRATE}` library at {} — a driver belongs to the container's library, so \
             this needs the container's directory",
            library.display()
        ));
    }
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
        "registered_in": format!("crates/{EMBEDDED_CRATE}/src/drivers/mod.rs"),
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

    /// **Two variants of one family are ONE backend crate.** The chip is `MCU` plus `--target`,
    /// not a cargo feature, so a backend serves a family however many variants are selected.
    #[test]
    fn one_backend_per_family_not_per_variant() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[
            ("esp32c6", "esp-idf", Some("esp32")),
            ("esp32s3", "esp-idf", Some("esp32")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let out = embedded_scaffold("Weather", &["esp32c6".into(), "esp32s3".into()])
            .expect("both variants are servable");
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();

        // The name is normalized, and there is one backend crate, not one per variant.
        assert!(
            paths.contains(&"crates/weather-hal-esp32/src/lib.rs"),
            "{paths:?}"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.contains("esp32c6") || p.contains("esp32s3")),
            "a variant is a platform entry, not a crate: {paths:?}"
        );
        assert_eq!(out.structure, ProjectStructure::Embedded);
        assert!(out.embedded, "the wizard sets embedded for this type");

        // The README describes *this* project: the esp32 family's own build command, with the crate
        // name filled in — and not the other family's toolchain, which this project does not have.
        // (It used to name both families whichever board was chosen, which is a README that lies.)
        let readme = out
            .files
            .iter()
            .find(|f| f.path == "README.md")
            .expect("the scaffold writes a README")
            .content
            .clone();
        assert!(
            readme.contains("MCU=esp32 cargo build --target xtensa-esp32-espidf"),
            "{readme}"
        );
        assert!(
            readme.contains("-Zbuild-std=std,panic_abort -p weather-hal-esp32"),
            "the crate name is completed, not left as a placeholder:\n{readme}"
        );
        assert!(
            !readme.contains("thumbv6m-none-eabi"),
            "another family's toolchain has no business here:\n{readme}"
        );
        assert!(
            !readme.contains("__BUILDS__") && !readme.contains("__HAL__"),
            "no template placeholder survives:\n{readme}"
        );
    }

    /// Both families: the marker the analyzer reads, one backend each, and — the part that is
    /// easiest to get wrong — which executor each uses.
    #[test]
    fn both_families_get_a_backend_and_the_marker_the_analyzer_reads() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[
            ("esp32c6", "esp-idf", Some("esp32")),
            ("rp2040", "rp2040", Some("rp2040")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let out = embedded_scaffold("weather", &["esp32c6".into(), "rp2040".into()]).expect("both");
        let file = |path: &str| {
            out.files
                .iter()
                .find(|f| f.path == path)
                .unwrap_or_else(|| panic!("no {path} among the scaffolded files"))
        };

        // The workspace declares the type, and the backends are not default members: a host
        // `cargo test` must not need a cross toolchain or a vendor SDK.
        let ws = &file("Cargo.toml").content;
        assert!(ws.contains("structure = \"embedded\""), "{ws}");
        assert!(ws.contains("\"crates/weather-hal-esp32\","), "{ws}");
        assert!(ws.contains("\"crates/weather-hal-rp2040\","), "{ws}");
        assert!(
            ws.contains("default-members = [\"crates/weather-hal\", \"crates/weather-hal-std\"]"),
            "{ws}"
        );

        // The std family reuses the shared executor; the no_std family owns its own.
        let esp = &file("crates/weather-hal-esp32/src/lib.rs").content;
        assert!(esp.contains("weather_hal_std::StdSpawner"), "{esp}");
        assert!(
            !esp.contains("#![no_std]"),
            "an esp-idf backend must not declare no_std — `std::thread` *is* its executor: {esp}"
        );
        assert!(file("crates/weather-hal-esp32/Cargo.toml")
            .content
            .contains("esp-idf-hal = \"0.47\""));
        let rp = &file("crates/weather-hal-rp2040/src/lib.rs").content;
        assert!(rp.contains("StaticSpawner"), "{rp}");
        // The attribute the compiler demanded: without it the crate wants `std`, which a thumbv6m
        // target does not have ("can't find crate for `std`" — found by building one).
        assert!(
            rp.contains("#![no_std]"),
            "a no_std family's backend must declare it: {rp}"
        );
        // And the crates its HAL's methods actually need in scope — at the versions `rp2040-hal`
        // 0.10.2 itself depends on (`embedded-hal` 1.0.0, `cortex-m` 0.7.2), because the *other*
        // major's `OutputPin` is a different trait (so the right import from the wrong version still
        // cannot call `set_high`) and a crate that is only a transitive dependency cannot be named.
        let rp_manifest = &file("crates/weather-hal-rp2040/Cargo.toml").content;
        for dep in [
            "rp2040-hal = \"0.10\"",
            "embedded-hal = \"1\"",
            "cortex-m = \"0.7\"",
        ] {
            assert!(rp_manifest.contains(dep), "missing {dep}: {rp_manifest}");
        }
        assert!(
            !file("crates/weather-hal-rp2040/Cargo.toml")
                .content
                .contains("weather-hal-std"),
            "a no_std family cannot use the std executor"
        );

        // Manifests are locked; the implementation is the fillable half.
        assert!(file("crates/weather-hal-esp32/Cargo.toml").structural);
        assert!(!file("crates/weather-hal-esp32/src/lib.rs").structural);
        assert_eq!(
            file("crates/weather-hal-esp32/src/lib.rs").fill_role,
            Some(spire_core::build_types::SourceRole::HalImplementation)
        );
        assert_eq!(out.fill_roots.len(), 4, "{:?}", out.fill_roots);
        assert_eq!(
            out.dependency_sections,
            vec![
                "crates/weather-hal-esp32/Cargo.toml".to_string(),
                "crates/weather-hal-rp2040/Cargo.toml".to_string()
            ]
        );
    }

    /// A platform this scaffold cannot serve is refused **by name**. A backend crate that cannot
    /// build is worse than no crate, because it fails long after the choice was made.
    #[test]
    fn unknown_and_non_embedded_platforms_are_refused() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[
            ("rpi5", "linux", None),
            ("esp32h2", "esp-idf", Some("esp32h2")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let err = embedded_scaffold("x", &["rpi5".into()]).unwrap_err();
        assert!(err.contains("not an embedded platform"), "{err}");

        let err = embedded_scaffold("x", &["nosuch".into()]).unwrap_err();
        assert!(err.contains("unknown platform"), "{err}");

        // Embedded, but a family no backend is known for: the scaffold must not invent one.
        let err = embedded_scaffold("x", &["esp32h2".into()]).unwrap_err();
        assert!(
            err.contains("no backend is known for family 'esp32h2'"),
            "{err}"
        );
    }

    /// Not a test of the content but of its **compilability**: write the scaffold somewhere a
    /// real `cargo` can be pointed at, so the emitted contract and executor are built as code
    /// rather than trusted as text.
    ///
    /// Ignored by default because it writes outside the target directory, which a test should
    /// not do unasked:
    ///
    /// ```sh
    /// cargo test -p spire-code --lib dump_scaffold -- --ignored --nocapture
    /// cd "$(cargo test … | tail -1)" && cargo test && cargo build -p <hal>-std
    /// ```
    #[test]
    #[ignore = "writes the scaffold to a real directory for a real cargo build"]
    fn dump_scaffold_for_a_real_build() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[
            ("esp32c6", "esp-idf", Some("esp32")),
            ("rp2040", "rp2040", Some("rp2040")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let out = embedded_scaffold("spire-demo", &["esp32c6".into(), "rp2040".into()])
            .expect("both families");
        let root = std::env::temp_dir().join("spire-embedded-hal-scaffold");
        let _ = std::fs::remove_dir_all(&root);
        for f in &out.files {
            let path = root.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
        }
        println!("{}", root.display());
    }

    /// A container on disk: a workspace manifest with a members list, one member per line.
    fn container_on_disk(root: &Path) -> PathBuf {
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nresolver = \"2\"\n# One crate, and it is host-checkable.\nmembers = [\n    \"crates/spire-embedded\",\n]\n\
             \n[workspace.metadata.spire]\nstructure = \"embedded\"\n",
        )
        .unwrap();
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
        std::fs::write(
            root.join("Cargo.toml"),
            "# A container with its crate set on one line.\n[workspace]\nresolver = \"2\"\n\
             members = [\"crates/spire-embedded\"]\n\n[workspace.package]\nversion = \"0.1.0\"\n\
             edition = \"2021\"\n",
        )
        .unwrap();
        root.join("Cargo.toml")
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
        for expected in [
            "name = \"spire-bsp-esp32c3\"",
            "spire-embedded = { path = \"../spire-embedded\" }",
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
                "crates/spire-embedded".to_string(),
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
                "crates/spire-embedded".to_string(),
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
        let library = root.join("crates").join(EMBEDDED_CRATE);
        std::fs::create_dir_all(library.join("src/drivers")).unwrap();
        std::fs::write(
            library.join("Cargo.toml"),
            "[package]\nname = \"spire-embedded\"\n",
        )
        .unwrap();
        std::fs::write(
            library.join("src/drivers/mod.rs"),
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

        let module = std::fs::read_to_string(
            root.path()
                .join("crates/spire-embedded/src/drivers/bme280.rs"),
        )
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
        let test =
            std::fs::read_to_string(root.path().join("crates/spire-embedded/tests/bme280.rs"))
                .expect("the driver's test");
        assert!(
            test.contains("#[ignore = \"the bme280 protocol is not written yet"),
            "{test}"
        );

        // Registered beside the existing module, modules before re-exports.
        let list =
            std::fs::read_to_string(root.path().join("crates/spire-embedded/src/drivers/mod.rs"))
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
        // A directory that is not a container: no library crate to add to.
        let empty = tempfile::tempdir().unwrap();
        container_on_disk(empty.path());
        let err = add_driver(empty.path(), "bme280", "i2c").unwrap_err();
        assert!(err.contains("no `spire-embedded` library at"), "{err}");

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
                .join("crates/spire-embedded/src/drivers/bme280.rs")
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

        let work = tempfile::tempdir().unwrap();
        copy_sources(&source, work.path());
        add_bsp(work.path(), "esp32c3").expect("a BSP for the pilot board");

        let toolchain = stable_toolchain();
        let output = cargo_in(
            work.path(),
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
        let artifact = work
            .path()
            .join("target/riscv32imc-unknown-none-elf/debug/libspire_bsp_esp32c3.rlib");
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

        let work = tempfile::tempdir().unwrap();
        copy_sources(&source, work.path());
        add_driver(work.path(), "bme280", "i2c").expect("a driver for an i2c device");

        let toolchain = stable_toolchain();
        // The module the emitter registered is a module the compiler actually reads — including the
        // `pub use` line beside it, which names the type it re-exports.
        let chip = cargo_in(
            work.path(),
            toolchain.as_ref(),
            &[
                "build",
                "-p",
                "spire-embedded",
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
            work.path(),
            toolchain.as_ref(),
            &["test", "-p", "spire-embedded", "--no-run"],
        );
        assert!(
            host.status.success(),
            "the emitted driver test must compile on the host.\n{}",
            output_of(&host)
        );

        // Exit status is not evidence: cargo can succeed without building anything, and a test that only
        // checks a status cannot tell that from a real compile. The artifacts can.
        let rlib = work
            .path()
            .join("target/riscv32imc-unknown-none-elf/debug/libspire_embedded.rlib");
        let size = std::fs::metadata(&rlib)
            .map(|meta| meta.len())
            .unwrap_or_default();
        assert!(
            size > 0,
            "no rlib at {} (the chip build compiled nothing)",
            rlib.display()
        );

        // The driver's test binary: the file `cargo build` never reads, compiled because it is a test.
        let deps = work.path().join("target/debug/deps");
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

    /// Copy a container's sources, skipping what a build regenerates or git owns.
    fn copy_sources(from: &Path, to: &Path) {
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
        if !source.join("crates/spire-embedded/Cargo.toml").is_file() {
            println!(
                "no spire-embedded container at {} — set SPIRE_EMBEDDED_ROOT to build this",
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
