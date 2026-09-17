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
        }),
        "rp2040" => Some(FamilySpec {
            vendor_crate: "rp2040-hal",
            // Three, not one, and measured rather than assumed: `rp2040-hal` does **not** re-export
            // `embedded-hal` or `nb` (checked against 0.10.2's source — it re-exports `fugit`,
            // `paste` and `rp2040-pac`), and its GPIO and timer APIs are those traits:
            // `Pin::set_high` is `embedded_hal::digital::v2::OutputPin::set_high`, and
            // `Timer::count_down` returns an `nb`-based `CountDown`. Without these two the backend
            // cannot call the HAL's own methods — the first generated backend failed to compile on
            // exactly this.
            deps: &[
                "rp2040-hal = \"0.10\"",
                "embedded-hal = \"0.2\"",
                "nb = \"1\"",
            ],
            vendor_note:
                "# The blocking HAL: this backend's actor executor is synchronous, so it\n\
                          # needs no async runtime. (An embassy-rp backend is the async\n\
                          # alternative, and would still drive the same synchronous `handle`.)\n\
                          # `embedded-hal` and `nb` are here because rp2040-hal's GPIO and timer\n\
                          # methods come from them and it does not re-export them.",
            uses_std_executor: false,
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
pub(crate) fn embedded_hal_scaffold(
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
        structure: ProjectStructure::EmbeddedHal,
        embedded: true,
    })
}

/// The workspace manifest — and the **marker** that makes this project recognizable.
///
/// `[workspace.metadata.spire] structure = "embedded_hal"` is what `cargo.rs::analyze` reads:
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

# Declares this project's type to Spire. The value is `ProjectStructure::EmbeddedHal`'s own key.
[workspace.metadata.spire]
structure = "embedded_hal"
"#
    .replace("__DISPLAY__", display)
    .replace("__BACKENDS__", &backends)
    .replace("__MEMBERS__", &members)
    .replace("__HAL__", hal)
}

/// The README: what the shape is, and how each family is actually built.
///
/// The commands are the ones that were made to work, because each fails in a way that does not
/// name its real cause — a README that just says "build it" is how that knowledge gets lost.
fn readme(display: &str, hal: &str, families: &[(String, FamilySpec)]) -> String {
    let rows: String = families
        .iter()
        .map(|(family, _)| format!("- `crates/{hal}-{family}` — the {family} backend\n"))
        .collect();
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
# esp32 family (std, esp-idf). Needs the espup `esp` toolchain and ESP-IDF, and LIBCLANG_PATH
# (Spire sets it; by hand, `source ~/export-esp.sh` first).
MCU=esp32 cargo build --target xtensa-esp32-espidf -Zbuild-std=std,panic_abort -p __HAL__-esp32

# rp2040 family (no_std).
cargo build --target thumbv6m-none-eabi -p __HAL__-rp2040
```

The FIRST esp32 build compiles ESP-IDF itself and takes minutes; later builds are seconds.
"#
    .replace("__DISPLAY__", display)
    .replace("__ROWS__", &rows)
    .replace("__HAL__", hal)
}

/// The contract crate: `no_std`, dependency-free, host-testable.
///
/// `lib.rs`, `actor.rs`, `hal/mod.rs` and the error type are **structural** — they are the seam,
/// and a hand edit there is how a family quietly stops being portable. `hal/led.rs` and
/// `hal/time.rs` are **fillable** with `fill_role: HalInterface`: authoring the contract *is* the
/// work of this project type, so those are the files the fill phase may write.
fn contract_files(hal: &str, hal_id: &str, display: &str) -> Vec<super::ScaffoldFile> {
    let structural = |path: String, content: String| super::ScaffoldFile {
        path,
        content,
        structural: true,
        ..Default::default()
    };
    let contract_trait = |path: String, content: &str| super::ScaffoldFile {
        path,
        content: content.to_string(),
        structural: false,
        fill_role: Some(spire_core::build_types::SourceRole::HalInterface),
    };

    vec![
        structural(
            format!("crates/{hal}/Cargo.toml"),
            format!(
                "[package]\n\
                 name = \"{hal}\"\n\
                 description = \"The board-family-agnostic HAL contract for {display} firmware.\"\n\
                 version.workspace = true\n\
                 edition.workspace = true\n\
                 license.workspace = true\n\
                 \n\
                 # Intentionally dependency-free.\n\
                 #\n\
                 # Every dependency is one more thing that can drag `std`, a vendor SDK or an\n\
                 # allocator into the abstraction, which is exactly what this crate exists to\n\
                 # prevent. A backend is where dependencies belong.\n\
                 [dependencies]\n"
            ),
        ),
        structural(format!("crates/{hal}/src/lib.rs"), contract_lib(hal_id)),
        structural(format!("crates/{hal}/src/error.rs"), ERROR_RS.to_string()),
        structural(format!("crates/{hal}/src/actor.rs"), ACTOR_RS.to_string()),
        structural(
            format!("crates/{hal}/src/hal/mod.rs"),
            HAL_MOD_RS.to_string(),
        ),
        contract_trait(format!("crates/{hal}/src/hal/led.rs"), LED_RS),
        contract_trait(format!("crates/{hal}/src/hal/time.rs"), TIME_RS),
    ]
}

/// The contract crate's `lib.rs`. `no_std` outside tests, so the host can still test it — which
/// is what makes the abstraction verifiable without hardware.
fn contract_lib(hal_id: &str) -> String {
    format!(
        "//! `{hal_id}` — the board-family-agnostic HAL contract.\n\
         //!\n\
         //! A firmware project depends on this crate and nothing else; supporting a board family\n\
         //! adds a **backend** crate that implements these traits. Nothing here knows about a\n\
         //! vendor SDK, an OS or an executor — the executor is injected (see `actor::Spawner`).\n\
         \n\
         #![cfg_attr(not(test), no_std)]\n\
         \n\
         pub mod actor;\n\
         pub mod error;\n\
         pub mod hal;\n\
         \n\
         pub use error::HalError;\n"
    )
}

/// The error type: ours, not `std::io::Error` — a trait signature is a promise to every family.
const ERROR_RS: &str = r#"/// The contract's error type.
///
/// Ours rather than `std::io::Error`, because a trait signature is a promise to every board
/// family and `std` is not available on all of them. Deliberately small: add a variant when a
/// family needs one, not in anticipation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HalError {
    /// The hardware cannot do this at all — a peripheral this variant does not have.
    Unsupported,
    /// It could, but the request was invalid: range, mode, timing.
    Invalid,
    /// Another owner holds it.
    Busy,
}
"#;

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

/// The `hal` module: the traits, and the rule for adding them.
const HAL_MOD_RS: &str = r#"//! The HAL traits a firmware project programs against.
//!
//! A project depends on the contract and sees only these; which board it runs on is decided by
//! which backend crate is linked in. So project code never contains a `#[cfg(board)]`.
//!
//! Traits are added here when a *second* board family needs them, not in anticipation: an
//! abstraction built speculatively is a guess about what the vendor HALs have in common, and the
//! guess is cheapest to make once the second backend exists.

pub mod led;
pub mod time;

pub use led::Led;
pub use time::DelayMs;
"#;

/// The first contract trait — the smallest one that exercises the whole stack.
const LED_RS: &str = r#"/// A single binary output.
///
/// The smallest contract that exercises the whole stack — trait, backend, actor, drift — and the
/// one thing every board family has, which is why it is first.
pub trait Led {
    /// Drive the output. `on` is the **logical** state: an active-low board inverts here, in its
    /// backend, so the inversion is written once instead of leaking into every caller.
    fn set(&mut self, on: bool);
}
"#;

/// The embedded primitive every family has, and the reason an actor is testable off-hardware.
const TIME_RS: &str = r#"/// Blocking delay — the embedded primitive.
///
/// A millisecond delay rather than a timestamped clock: an actor needs "wait", and a clock API
/// would drag an associated `Instant` type — a different one per family — into the abstraction
/// for no gain.
pub trait DelayMs {
    /// Block the current task for `ms` milliseconds. Deliberately `&mut self`: on a single-core
    /// part, who waits is a real decision, and two actors must not share one time source.
    fn delay_ms(&mut self, ms: u32);
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
fn backend_files(
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

/// The backend template. Stubs are `unimplemented!()` on purpose — see the header.
///
/// `__NO_STD__` is the crate-level attribute a `no_std` family needs and a `std` one must not have:
/// a `thumbv6m` target has no `std` to link, and an esp-idf backend that declared `no_std` could not
/// call `std::thread` — which *is* its executor. It sits after the `//!` block because inner
/// attributes may follow inner doc comments, and before `use` because that is where an inner
/// attribute must be.
const BACKEND_LIB_RS: &str = r#"//! The __FAMILY__ backend: the contract, implemented with `__VENDOR__`.
//!
//! Types from `__VENDOR__` live here and nowhere else: a firmware actor holds the *traits*, so
//! it does not change when the board does.
//!
//! The stubs below are `unimplemented!()` on purpose. This is the file the contract's drift
//! measure reports as not implemented and the fill phase writes; a stub that quietly did nothing
//! would look implemented, which is worse than one that fails loudly.
__NO_STD__
use __HAL_ID__::hal::{DelayMs, Led};

__EXECUTOR__
/// An LED on this family's GPIO.
///
/// The pin is handed to the struct and never named by an actor; an active-low board inverts in
/// `new` (see the `Led` contract).
pub struct GpioLed {
    // TODO: the pin type from __VENDOR__.
}

impl Led for GpioLed {
    fn set(&mut self, _on: bool) {
        unimplemented!("GpioLed::set")
    }
}

/// This family's blocking delay.
pub struct FamilyDelay;

impl DelayMs for FamilyDelay {
    fn delay_ms(&mut self, _ms: u32) {
        unimplemented!("FamilyDelay::delay_ms")
    }
}"#;

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

#[cfg(test)]
mod tests {
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
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry(&[
            ("esp32c6", "esp-idf", Some("esp32")),
            ("esp32s3", "esp-idf", Some("esp32")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let out = embedded_hal_scaffold("Weather", &["esp32c6".into(), "esp32s3".into()])
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
        assert_eq!(out.structure, ProjectStructure::EmbeddedHal);
        assert!(out.embedded, "the wizard sets embedded for this type");
    }

    /// Both families: the marker the analyzer reads, one backend each, and — the part that is
    /// easiest to get wrong — which executor each uses.
    #[test]
    fn both_families_get_a_backend_and_the_marker_the_analyzer_reads() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry(&[
            ("esp32c6", "esp-idf", Some("esp32")),
            ("rp2040", "rp2040", Some("rp2040")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let out =
            embedded_hal_scaffold("weather", &["esp32c6".into(), "rp2040".into()]).expect("both");
        let file = |path: &str| {
            out.files
                .iter()
                .find(|f| f.path == path)
                .unwrap_or_else(|| panic!("no {path} among the scaffolded files"))
        };

        // The workspace declares the type, and the backends are not default members: a host
        // `cargo test` must not need a cross toolchain or a vendor SDK.
        let ws = &file("Cargo.toml").content;
        assert!(ws.contains("structure = \"embedded_hal\""), "{ws}");
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
        // And the crates its HAL's methods actually need in scope: rp2040-hal's GPIO and timer APIs
        // are `embedded-hal`/`nb` traits it does not re-export, which a generated backend found out
        // the hard way ("no method named `set_high`").
        let rp_manifest = &file("crates/weather-hal-rp2040/Cargo.toml").content;
        for dep in [
            "rp2040-hal = \"0.10\"",
            "embedded-hal = \"0.2\"",
            "nb = \"1\"",
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
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry(&[
            ("rpi5", "linux", None),
            ("esp32h2", "esp-idf", Some("esp32h2")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let err = embedded_hal_scaffold("x", &["rpi5".into()]).unwrap_err();
        assert!(err.contains("not an embedded platform"), "{err}");

        let err = embedded_hal_scaffold("x", &["nosuch".into()]).unwrap_err();
        assert!(err.contains("unknown platform"), "{err}");

        // Embedded, but a family no backend is known for: the scaffold must not invent one.
        let err = embedded_hal_scaffold("x", &["esp32h2".into()]).unwrap_err();
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
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry(&[
            ("esp32c6", "esp-idf", Some("esp32")),
            ("rp2040", "rp2040", Some("rp2040")),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let out = embedded_hal_scaffold("spire-demo", &["esp32c6".into(), "rp2040".into()])
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
}
