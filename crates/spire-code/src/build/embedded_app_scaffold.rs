// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Embedded application** scaffold — a firmware binary for one board that *depends on* an
//! embedded-HAL project.
//!
//! ```text
//! <hal>/                     the HAL: a library (contract + one backend per family)
//!   crates/<hal>/            the contract: the traits firmware programs against
//!   crates/<hal>-<family>/   the backend: the family's implementation + its executor
//! <app>/                     this scaffold: one binary
//!   Cargo.toml               path-deps the two crates above
//!   .cargo/config.toml       target, build-std, MCU, linker, runner
//!   build.rs                 re-emits the ESP-IDF link args for this binary
//!   sdkconfig.defaults       the two settings a std firmware needs (measured on a board)
//!   src/main.rs              an actor over the contract's traits  ← the fill writes this
//! ```
//!
//! Four things decide this shape, and three of them were measured on real hardware:
//!
//! 1. **Two projects, not one project type.** The HAL is a workspace of libraries; an application
//!    is a crate that path-deps it. So an app names the HAL it builds against and the manifest
//!    records it — a dependency is a fact, not a directory convention.
//! 2. **The board is chosen here, and only one.** A `main` names a pin and a vendor's peripheral
//!    type; two boards in one binary would need `#[cfg]`s the wizard cannot express.
//! 3. **`build.rs` is not boilerplate.** `esp-idf-sys` propagates the IDF link args as *metadata*
//!    for its own targets and `esp-idf-hal` relays them for the examples; neither emits them for
//!    this binary. Without the one line below the final link reaches `ldproxy` with no
//!    `--ldproxy-linker` and dies in a way that names neither the crate nor the missing file.
//! 4. **`sdkconfig.defaults` carries two measured settings**, and both are the difference between a
//!    firmware that runs and one that panics before its first line of output: the pthread stack
//!    (3 KB default, too small for a std thread's mailbox path) and the partition table (1 MB
//!    default, too small for a debug std image).

use spire_core::build_types::ProjectStructure;
use std::path::Path;

/// The app-side facts for one board family — the half the *HAL* scaffold has no use for: how a
/// `main` for this family is compiled and put on a board.
///
/// The rustup **target** and the **chip** are deliberately not here: one family spans chips whose
/// triples differ (`xtensa-esp32-espidf` for the classic, `riscv32imac-esp-espidf` for the C6), so
/// both are read from the chosen *platform*'s own `rust:` block. A family may share the linker, the
/// runner and the vendor crate; it does not share a triple.
struct AppSpec {
    /// The linker. `ldproxy` for esp-idf, because IDF's flags travel in a response file.
    linker: Option<&'static str>,
    /// How `cargo run` reaches the board.
    runner: &'static str,
    /// The vendor HAL the *application* needs itself: it takes the peripherals and links the
    /// patches, neither of which the backend crate could do on the app's behalf.
    vendor_dep: &'static str,
    /// The `use` lines that dependency brings.
    vendor_use: &'static str,
    /// The call that must run before anything of ours does (empty when the family has none).
    link_patches: &'static str,
    /// The executor the app hires: a std family's is the shared `StdSpawner`.
    spawner: &'static str,
    /// `build.rs` for this family, when one is required.
    build_rs: Option<&'static str>,
}

/// The two settings a std firmware needs, with the failure each one prevents.
///
/// Both were measured on the attached ESP32 (ISSUES.md, "on the board again"): without the first,
/// `Guru Meditation (LoadProhibited)` inside `pthread_mutex_unlock` before `app_main` printed
/// anything; without the second, the build's own partition table cannot hold the build's own image.
const ESP_SDKCONFIG_DEFAULTS: &str = r#"# This board's own defaults, applied on top of ESP-IDF's.
#
# `CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT` is 3072 bytes in IDF's defaults, and Rust's
# `std::thread` **is** a pthread — so every actor gets a 3 KB stack. That is not enough for
# `std::sync::mpsc`'s (mpmc) receive path in a debug build: the thread overflows, and the first
# casualty is a bogus pointer inside `pthread_mutex_unlock`, reported as `Guru Meditation
# (LoadProhibited)` before any application output appears.
CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=16384

# IDF's default partition table gives the `factory` app partition 1 MB, and a **debug** build of a
# std image is ~1.18 MB: the build's own table cannot hold the build's own image, and flashing it
# fails with "Supplied ELF image ... is too big". `SINGLE_APP_LARGE` is the same layout with a
# 1500K app partition, which holds this image at ~77%.
CONFIG_PARTITION_TABLE_SINGLE_APP_LARGE=y
"#;

/// The ESP-IDF `build.rs`, whose absence fails in a way that names neither the crate nor the file.
const ESP_BUILD_RS: &str = r#"// The one link the esp-idf crates cannot make from where they sit.
//
// `esp-idf-sys` does the real ESP-IDF build and propagates what it learned as `DEP_ESP_IDF_*`
// **metadata**; `esp-idf-hal`'s build script relays that onward as `DEP_ESP_IDF_HAL_*`. Neither
// emits it for *this* crate's binary, because a `rustc-link-arg` from a build script applies to
// the emitting package's own targets. Without this line the final link reaches `ldproxy` without
// `--ldproxy-linker` and panics with `Cannot locate argument '--ldproxy-linker <linker>'`.
fn main() {
    embuild::espidf::sysenv::output();
}
"#;

/// The app-side wiring for a board family, or `None` when this scaffold does not know it.
///
/// Returning `None` rather than defaulting is what stops the scaffold from emitting a manifest
/// whose dependencies cannot resolve, or a `.cargo/config.toml` pinning a triple the family does
/// not use — the same rule `embedded_hal_scaffold::family_spec` follows.
///
/// `esp32` only, for now, and for a reason worth writing down: a `no_std` family's application
/// cannot be written until its **backend supplies the executor** the app spawns on (the HAL
/// scaffold leaves that to the fill, because it is the backend's obligation). An esp32 app has
/// `StdSpawner` — a `std::thread`, which under esp-idf *is* a FreeRTOS task — so it is writable,
/// and it is the wiring that was measured on the board.
fn app_spec(family: &str) -> Option<AppSpec> {
    match family {
        "esp32" => Some(AppSpec {
            linker: Some("ldproxy"),
            runner: "espflash flash --monitor",
            vendor_dep: "esp-idf-hal = \"0.47\"",
            vendor_use:
                "use esp_idf_hal::peripherals::Peripherals;\nuse esp_idf_hal::sys::link_patches;",
            link_patches: "    link_patches();",
            spawner: "StdSpawner",
            build_rs: Some(ESP_BUILD_RS),
        }),
        _ => None,
    }
}

/// The crates a HAL workspace offers an application: the contract, and the backend for `family`.
struct HalCrates {
    /// The contract crate's directory, as written in the HAL's `members`.
    contract_dir: String,
    /// The family's backend crate directory.
    backend_dir: String,
}

/// True when a manifest declares the embedded-HAL shape (the marker `embedded_hal_scaffold` writes).
///
/// Read by hand, like the rest of this crate's manifest parsing: a TOML dependency for one key would
/// be the tail wagging the dog, and the value is written by our own scaffold.
fn declares_embedded_hal(manifest: &str) -> bool {
    let mut in_spire_metadata = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_spire_metadata = trimmed == "[workspace.metadata.spire]";
            continue;
        }
        if !in_spire_metadata {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("structure") {
            if let Some(value) = rest.trim_start().strip_prefix('=') {
                return value
                    .trim()
                    .trim_matches('"')
                    .eq_ignore_ascii_case(ProjectStructure::EmbeddedHal.as_str());
            }
        }
    }
    false
}

/// Every double-quoted run in a line.
fn quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        match after.find('"') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// The `members` of a workspace manifest, as directory paths.
///
/// Both forms the scaffold can produce are read: `members = ["a", "b"]` on one line, and the
/// multi-line array. A quoted entry is preferred over whitespace splitting, so a path with a space
/// in it survives.
fn workspace_members(manifest: &str) -> Vec<String> {
    let mut members = Vec::new();
    let mut in_members = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_members = false;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("members") {
            if rest.trim_start().starts_with('=') {
                members.extend(quoted(rest));
                in_members = !rest.contains(']');
                continue;
            }
        }
        if in_members {
            if trimmed.starts_with(']') {
                in_members = false;
                continue;
            }
            members.extend(quoted(trimmed));
        }
    }
    members
}

/// Locate the contract and the family's backend inside a HAL workspace.
///
/// By **suffix**, because that is the HAL scaffold's own naming rule (`<name>-hal`,
/// `<name>-hal-<family>`) and it is what lets one backend serve every chip of a family. The backend
/// is matched first so the contract can exclude it — a family literally named `hal` would otherwise
/// make `<name>-hal` look like both.
fn hal_crates(members: &[String], family: &str) -> Option<HalCrates> {
    let backend_suffix = format!("-{family}");
    let backend_dir = members
        .iter()
        .find(|m| m.ends_with(&backend_suffix) && !m.ends_with("-std"))
        .cloned()?;
    let contract_dir = members
        .iter()
        .find(|m| {
            let name = m.rsplit('/').next().unwrap_or(m.as_str());
            name.ends_with("-hal") && **m != backend_dir
        })
        .cloned()?;
    Some(HalCrates {
        contract_dir,
        backend_dir,
    })
}

/// The `[package.metadata.spire]` marker for an application.
///
/// Records the structure *and* the HAL it depends on, so a later reader — the analyzer, a rebuild, a
/// human — does not have to infer the dependency from a `path = "../.."` in the manifest.
fn marker(hal_path: &str) -> String {
    format!(
        "[package.metadata.spire]\n\
         structure = \"{}\"\n\
         # The embedded-HAL project this application builds against. The path dependencies above\n\
         # are resolved from it, so moving one without the other is a broken build rather than a\n\
         # mystery.\n\
         hal_path = \"{}\"\n",
        ProjectStructure::EmbeddedApp.as_str(),
        hal_path
    )
}

/// Why a board list cannot be used for an application: none, or more than one.
///
/// Separate from the entry point so the `match` there has one arm body — a shape rustfmt settles
/// on — and because the two refusals are the part a caller reads.
fn application_board_refusal(platforms: &[String]) -> String {
    match platforms {
        [] => "an embedded application needs a board: the board decides the toolchain, the SDK \
               and the backend crate this project depends on"
            .to_string(),
        many => format!(
            "an embedded application targets **one** board, and {} were given ({}); an app is a \
             `main` for a specific chip, so split it or pick one",
            many.len(),
            many.join(", ")
        ),
    }
}

/// Emit an embedded **application**: a firmware binary for one board, depending on a HAL project.
///
/// `hal_path` is the path as it should appear in the manifest and the marker (relative when the two
/// projects are near each other, absolute otherwise); `hal_root` is where the HAL actually is, so
/// this can read its members and refuse a HAL with no backend for the chosen board.
pub(crate) fn embedded_app_scaffold(
    project_name: &str,
    platforms: &[String],
    hal_path: &str,
    hal_root: &Path,
) -> Result<super::ScaffoldOutput, String> {
    let name = project_name.trim().to_lowercase().replace(' ', "-");

    // One board. A `main` names a pin and a vendor's peripheral type; two boards would need
    // `#[cfg]`s the wizard cannot express, and a binary that half-runs on each.
    let platform_id = match platforms {
        [one] => one,
        [] | [_, _, ..] => return Err(application_board_refusal(platforms)),
    };
    let platform = crate::platform::Platform::from_registry(platform_id)
        .ok_or_else(|| format!("unknown platform '{platform_id}' (see ~/.spire/platforms)"))?;
    if !platform.is_embedded() {
        return Err(format!(
            "platform '{platform_id}' is not an embedded platform (os '{}'); an application needs a \
             board it can flash",
            platform.os
        ));
    }
    let family = platform.family.clone().ok_or_else(|| {
        format!("platform '{platform_id}' names no `family`, so no backend applies")
    })?;
    let spec = app_spec(&family).ok_or_else(|| {
        format!(
            "no application wiring is known for family '{family}' yet (platform \
             '{platform_id}'). The esp32 wiring was measured on a board; a `no_std` family's is not \
             writable until its backend supplies the executor an application spawns on"
        )
    })?;

    // The HAL this app depends on, read rather than assumed. A wrong directory, a directory that is
    // not a HAL, and a HAL with no backend for this board are three different refusals — and all
    // three are cheaper here than at the first `cargo build`.
    let manifest_path = hal_root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("no embedded-HAL project at {}: {e}", hal_root.display()))?;
    if !declares_embedded_hal(&manifest) {
        return Err(format!(
            "{} is not an embedded-HAL project (its manifest does not declare `{}`), so there is \
             nothing for an application to depend on",
            hal_root.display(),
            ProjectStructure::EmbeddedHal.as_str()
        ));
    }
    let members = workspace_members(&manifest);
    let crates = hal_crates(&members, &family).ok_or_else(|| {
        format!(
            "{} has no '{family}' backend, so this application has nothing to link against — add \
             that board to the HAL project first (its members: {})",
            hal_root.display(),
            if members.is_empty() {
                "none".to_string()
            } else {
                members.join(", ")
            }
        )
    })?;

    // Crate *names* from each manifest: the directory says where a crate is, the name says what a
    // `use` statement needs, and the two differ whenever the project name has a dash in it.
    let contract_name =
        crate::build::generic_helpers::cargo_package_name(&hal_root.join(&crates.contract_dir))
            .ok_or_else(|| {
                format!(
                    "{}/Cargo.toml has no [package] name",
                    hal_root.join(&crates.contract_dir).display()
                )
            })?;
    let backend_name =
        crate::build::generic_helpers::cargo_package_name(&hal_root.join(&crates.backend_dir))
            .ok_or_else(|| {
                format!(
                    "{}/Cargo.toml has no [package] name",
                    hal_root.join(&crates.backend_dir).display()
                )
            })?;

    let main_rs = app_main(
        &contract_name.replace('-', "_"),
        &backend_name.replace('-', "_"),
        &spec,
    );
    let manifest_rs = app_manifest(
        &name,
        hal_path,
        &crates,
        &contract_name,
        &backend_name,
        &spec,
    );
    // The **platform's** own facts, not the family's: one family spans chips whose triples differ
    // (the classic esp32 and the C6), and the chip is what esp-idf-sys reads as `MCU`.
    let rust = platform.rust.as_ref().ok_or_else(|| {
        format!(
            "platform '{platform_id}' declares no `rust:` block (a target triple), so an \
             application cannot be compiled for it"
        )
    })?;
    let target = rust.target.clone();
    // `MCU` is esp-idf's own variable, so it is written for an esp-idf platform — not for a
    // platform whose `idf_target` happens to exist for a different tool (`probe-rs --chip`).
    let mcu = if platform.os.eq_ignore_ascii_case("esp-idf") {
        rust.idf_target.clone()
    } else {
        None
    };

    let mut files = vec![
        super::ScaffoldFile {
            path: "Cargo.toml".to_string(),
            content: manifest_rs.clone(),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: ".cargo/config.toml".to_string(),
            content: cargo_config(&spec, &target, mcu.as_deref()),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "README.md".to_string(),
            content: readme(&name, &backend_name, &spec),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "src/main.rs".to_string(),
            content: main_rs.clone(),
            // The fill's file. The actor is written out in full, but the one line that cannot be
            // known here — the board's LED constructor — belongs to the backend crate, which the
            // *HAL project's* fill writes. Everything that must be right (deps, target, MCU, the
            // SDK settings) is structural.
            structural: false,
            ..Default::default()
        },
    ];
    if let Some(build_rs) = spec.build_rs {
        files.push(super::ScaffoldFile {
            path: "build.rs".to_string(),
            content: build_rs.to_string(),
            structural: true,
            ..Default::default()
        });
    }
    if mcu.is_some() {
        files.push(super::ScaffoldFile {
            path: "sdkconfig.defaults".to_string(),
            content: ESP_SDKCONFIG_DEFAULTS.to_string(),
            structural: true,
            ..Default::default()
        });
    }

    Ok(super::ScaffoldOutput {
        build_file: "Cargo.toml".to_string(),
        build_content: manifest_rs,
        source_dir: "src".to_string(),
        source_file: "src/main.rs".to_string(),
        source_content: main_rs,
        files,
        platform_targets: vec![platform_id.clone()],
        // The application's own source is the only fillable half: the contract and the backends
        // live in the HAL project, which is the point of the split.
        fill_roots: vec!["src".to_string()],
        dependency_sections: vec!["Cargo.toml".to_string()],
        structure: ProjectStructure::EmbeddedApp,
        embedded: true,
    })
}

/// The application's manifest: the two path dependencies, the board's vendor HAL, and the marker.
fn app_manifest(
    name: &str,
    hal_path: &str,
    crates: &HalCrates,
    contract_name: &str,
    backend_name: &str,
    spec: &AppSpec,
) -> String {
    let hal = hal_path.trim_end_matches('/');
    APP_MANIFEST
        .replace("__NAME__", name)
        .replace("__HAL__", hal)
        .replace("__CONTRACT_DIR__", &crates.contract_dir)
        .replace("__CONTRACT__", contract_name)
        .replace("__BACKEND_DIR__", &crates.backend_dir)
        .replace("__BACKEND__", backend_name)
        .replace("__VENDOR_DEP__", spec.vendor_dep)
        .replace("__MARKER__", &marker(hal_path))
}

/// The `.cargo/config.toml`: the platform's target and chip, the family's linker and runner.
fn cargo_config(spec: &AppSpec, target: &str, mcu: Option<&str>) -> String {
    let mcu = mcu
        .map(|mcu| format!("\n[env]\nMCU = \"{mcu}\"\n"))
        .unwrap_or_default();
    let linker = spec
        .linker
        .map(|linker| {
            format!(
                "# IDF's flags travel in a response file that `ldproxy` reads. Without this, cargo\n\
                 # calls the C compiler directly, the IDF libraries are never added, and *every*\n\
                 # symbol comes back undefined.\n\
                 linker = \"{linker}\"\n"
            )
        })
        .unwrap_or_default();
    APP_CARGO_CONFIG
        .replace("__TARGET__", target)
        .replace("__MCU_BLOCK__", &mcu)
        .replace("__LINKER_BLOCK__", &linker)
        .replace("__RUNNER__", spec.runner)
}

/// The application's source: the actor in full, with the board's own wiring left to the fill.
fn app_main(contract_id: &str, backend_id: &str, spec: &AppSpec) -> String {
    APP_MAIN_RS
        .replace("__CONTRACT_ID__", contract_id)
        .replace("__BACKEND_ID__", backend_id)
        .replace("__VENDOR_USE__", spec.vendor_use)
        .replace("__SPAWNER__", spec.spawner)
        .replace("__LINK_PATCHES__", spec.link_patches)
}

/// The application's README: what it depends on, and the one command that puts it on a board.
fn readme(name: &str, backend_name: &str, spec: &AppSpec) -> String {
    README_MD
        .replace("__NAME__", name)
        .replace("__BACKEND__", backend_name)
        .replace("__RUNNER__", spec.runner)
}

/// The application's manifest template. `__HAL__` is where the user placed the HAL project.
const APP_MANIFEST: &str = r#"# __NAME__ — an embedded application: one board, one binary, built against a HAL project.
#
# The two path dependencies below are that HAL. They are *dependencies* rather than a nested
# layout: the HAL is a project of its own and can serve several applications.
[package]
name = "__NAME__"
version = "0.1.0"
edition = "2021"

[dependencies]
# The contract: the traits this application's actor is written against.
__CONTRACT__ = { path = "__HAL__/__CONTRACT_DIR__" }
# The backend: the board's implementation, and the executor it re-exports.
__BACKEND__ = { path = "__HAL__/__BACKEND_DIR__" }
# The application's own use of the vendor HAL — it takes the peripherals and links the patches,
# neither of which the backend crate could do on the app's behalf.
__VENDOR_DEP__

[build-dependencies]
# Emits the ESP-IDF link args for *this* binary (see build.rs).
embuild = "0.33"

__MARKER__"#;

/// The `.cargo/config.toml` template — the board's compile-time facts.
const APP_CARGO_CONFIG: &str = r#"# The facts a `cargo build` for this board needs, declared by the project rather than injected per
# command line: the target triple, and — for esp-idf — the chip esp-idf-sys reads as `MCU`.
[build]
target = "__TARGET__"

[unstable]
# A tier-3 target with no prebuilt std: the standard library is compiled from source, which is why
# the toolchain installs `rust-src`.
build-std = ["std", "panic_abort"]
__MCU_BLOCK__
[target.__TARGET__]
__LINKER_BLOCK__runner = "__RUNNER__"
"#;

/// The application's source template.
///
/// The actor is written out in full, because it is the portable part and the part a user is meant
/// to change: it holds the contract's traits and no vendor type. What is left as a `todo!()` is the
/// one line that cannot be written here — the backend's constructor for this board's LED. That
/// belongs to the backend crate, so this file names it as a call site rather than guessing its
/// shape; and because `todo!()` type-checks, the wiring *around* it (deps, target, SDK) can be built
/// and checked before any fill has run.
const APP_MAIN_RS: &str = r#"//! The application: one actor, one board, written against the HAL's traits.
//!
//! Note what this file does *not* contain: no vendor type in the actor, no chip name, no `unsafe`.
//! The actor below would be the same on any board the HAL has a backend for — which is what makes
//! the HAL a dependency rather than a copy.

__VENDOR_USE__

use __CONTRACT_ID__::actor::{Actor, Mailbox, Spawner};
use __CONTRACT_ID__::hal::{DelayMs, Led};
use __BACKEND_ID__::{FamilyDelay, GpioLed, __SPAWNER__};

/// Half a second on, half a second off.
const PERIOD_MS: u32 = 500;

/// What the actor understands.
///
/// `Debug` because `try_send` hands a message back inside a `SendError`, and
/// `SendError<M>: Debug` needs `M: Debug` — which is what makes `.expect()` on a send work.
#[derive(Debug)]
enum Blink {
    Toggle,
    Wait(u32),
    /// Blink forever: one instruction, then the component owns it, which is how real firmware is
    /// shaped.
    Repeat { period_ms: u32 },
}

/// The actor — the part that looks the same on any board family.
struct Blinker<L, D> {
    led: L,
    delay: D,
    on: bool,
}

impl<L: Led, D: DelayMs> Actor for Blinker<L, D> {
    type Message = Blink;

    fn handle(&mut self, msg: Self::Message) {
        match msg {
            Blink::Toggle => {
                self.on = !self.on;
                self.led.set(self.on);
                // Observable with nothing wired: the console is the first proof that the firmware
                // runs and the actor handles messages; the LED is the second.
                println!("blink: {}", if self.on { "on" } else { "off" });
            }
            Blink::Wait(ms) => self.delay.delay_ms(ms),
            // Never returns, so this actor processes no further messages. For a blink that is the
            // point; an actor that must stay responsive would self-schedule instead.
            Blink::Repeat { period_ms } => loop {
                self.handle(Blink::Toggle);
                self.handle(Blink::Wait(period_ms));
            },
        }
    }
}

fn main() {
__LINK_PATCHES__
    let led = board_led();
    let spawner = __SPAWNER__;
    let mailbox = spawner.spawn(Blinker { led, delay: FamilyDelay, on: false });

    // One instruction, then supervise. `try_send` hands the message back rather than dropping it,
    // so the expect is a real assertion that the task started.
    mailbox
        .try_send(Blink::Repeat { period_ms: PERIOD_MS })
        .expect("the blink task should be running");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// TODO: this board's LED, from the backend crate.
///
/// The pin is a board fact. Take it from `Peripherals::take()`, hand it to the constructor
/// `__BACKEND_ID__::GpioLed` documents, and return the driver — a wrong pin here is the most
/// likely reason nothing blinks, and it is the one line this scaffold cannot know.
fn board_led() -> GpioLed {
    let _peripherals = Peripherals::take().expect("peripherals are taken once, and here");
    todo!("construct __BACKEND_ID__::GpioLed from this board's pin")
}
"#;

/// The application's README template.
const README_MD: &str = r#"# __NAME__ — an embedded application

One binary for one board, built against the HAL project named in `Cargo.toml`
(`[package.metadata.spire] hal_path`). The HAL owns the traits and the board's implementation;
this project owns the `main`.

```text
src/main.rs        the actor — board-agnostic, written against the HAL's traits
Cargo.toml         the HAL path dependencies, and the board's vendor HAL
.cargo/config.toml target, chip, linker, runner
sdkconfig.defaults the two settings a std firmware needs (see the comments)
build.rs           re-emits the ESP-IDF link args for this binary
```

The board's LED is the one thing left to write: `board_led()` in `src/main.rs` returns
`__BACKEND__::GpioLed`, whose constructor the HAL's backend crate provides.

## Build and run

```sh
cargo build
cargo run   # builds, flashes, and opens the monitor: __RUNNER__
```

The target and the runner come from `.cargo/config.toml`, so no flags are needed — but the
*toolchain* is not this project's: an esp-idf build needs the `esp` rustup toolchain
(`espup install`) and the SDK, which `esp-idf-sys` downloads on its first build. See the HAL
project's README for the environment.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A hermetic registry, so the tests can name families the machine may not have and prove the
    /// refusals without touching `~/.spire/platforms`.
    ///
    /// `(id, os, family, rust target)` — the target is what `.cargo/config.toml` pins, and the chip
    /// (`idf_target`) is the platform's own id for the esp families, which is what it is in the real
    /// registry. **Per platform, not per family**: two chips of one family differ here, which is
    /// exactly what the emitter has to get right.
    fn registry(entries: &[(&str, &str, Option<&str>, Option<&str>)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (id, os, family, target) in entries {
            let family_line = family.map(|f| format!("family: {f}\n")).unwrap_or_default();
            let rust_block = match target {
                Some(target) => {
                    format!("rust:\n  target: {target}\n  idf_target: {id}\n  flash: espflash\n")
                }
                None => String::new(),
            };
            std::fs::write(
                dir.path().join(format!("{id}.yaml")),
                format!(
                    "id: {id}\nname: {id}\nos: {os}\n{family_line}architecture:\n  cpu_family: x\n  \
                     cpu: x\n  endian: little\n  target_triple: x\n{rust_block}"
                ),
            )
            .unwrap();
        }
        dir
    }

    /// A **real** HAL on disk: the workspace `embedded_hal_scaffold` emits, written out.
    ///
    /// Written from our own emitter rather than hand-rolled, so the test proves the two scaffolds
    /// agree — an app reads exactly the members and crate names the HAL writes.
    fn hal_on_disk(dir: &Path, name: &str, platforms: &[&str]) {
        let ids: Vec<String> = platforms.iter().map(|p| p.to_string()).collect();
        let out =
            crate::build::embedded_hal_scaffold::embedded_hal_scaffold(name, &ids).expect("a HAL");
        for file in &out.files {
            let path = dir.join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &file.content).unwrap();
        }
    }

    /// The whole point of the structure: an app is a **separate project** whose manifest depends on
    /// the HAL's contract and its board's backend, by name and by path.
    #[test]
    fn an_app_path_deps_the_hals_contract_and_its_backend() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // The board is a **C6**: same family as the classic esp32, different triple and chip. The
        // config must carry this platform's facts, not the family's — which is the whole reason
        // target and `MCU` are read per platform.
        let reg = registry(&[(
            "esp32c6",
            "esp-idf",
            Some("esp32"),
            Some("riscv32imac-esp-espidf"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let hal_dir = tempfile::tempdir().unwrap();
        hal_on_disk(hal_dir.path(), "weather", &["esp32c6"]);
        let hal_path = hal_dir.path().to_string_lossy().to_string();

        let out = embedded_app_scaffold(
            "Weather Node",
            &["esp32c6".into()],
            &hal_path,
            hal_dir.path(),
        )
        .expect("an esp32c6 app against an esp32-family HAL");

        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        for expected in [
            "Cargo.toml",
            ".cargo/config.toml",
            "build.rs",
            "sdkconfig.defaults",
            "src/main.rs",
            "README.md",
        ] {
            assert!(paths.contains(&expected), "missing {expected}: {paths:?}");
        }

        let manifest = out
            .files
            .iter()
            .find(|f| f.path == "Cargo.toml")
            .unwrap()
            .content
            .clone();
        // The two dependencies, by crate name and by the HAL's own directory layout.
        assert!(
            manifest.contains("weather-hal = { path = ")
                && manifest.contains("/crates/weather-hal"),
            "{manifest}"
        );
        assert!(
            manifest.contains("weather-hal-esp32")
                && manifest.contains("/crates/weather-hal-esp32"),
            "{manifest}"
        );
        // The vendor HAL is the app's own dependency: it takes the peripherals.
        assert!(manifest.contains("esp-idf-hal"), "{manifest}");
        // And the marker says what this project is *and* what it depends on.
        assert!(manifest.contains("[package.metadata.spire]"), "{manifest}");
        assert!(
            manifest.contains("structure = \"embedded_app\""),
            "{manifest}"
        );
        assert!(
            manifest.contains(&format!("hal_path = \"{hal_path}\"")),
            "{manifest}"
        );

        // The wiring that was measured on a board, not guessed — and the **platform's** target and
        // chip, which is why a C6 gets the RISC-V triple while the family still names esp32.
        let config = out
            .files
            .iter()
            .find(|f| f.path == ".cargo/config.toml")
            .unwrap();
        assert!(
            config
                .content
                .contains("target = \"riscv32imac-esp-espidf\""),
            "{}",
            config.content
        );
        assert!(
            config.content.contains("MCU = \"esp32c6\""),
            "{}",
            config.content
        );
        assert!(
            config.content.contains("linker = \"ldproxy\""),
            "{}",
            config.content
        );
        assert!(config.content.contains("espflash"), "{}", config.content);
        assert_eq!(out.structure, ProjectStructure::EmbeddedApp);
        assert!(out.embedded);

        let build_rs = out.files.iter().find(|f| f.path == "build.rs").unwrap();
        assert!(
            build_rs
                .content
                .contains("embuild::espidf::sysenv::output()"),
            "{}",
            build_rs.content
        );
        let sdkconfig = out
            .files
            .iter()
            .find(|f| f.path == "sdkconfig.defaults")
            .unwrap();
        assert!(sdkconfig
            .content
            .contains("CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=16384"));
        assert!(sdkconfig
            .content
            .contains("CONFIG_PARTITION_TABLE_SINGLE_APP_LARGE=y"));

        // The app's own source is the fillable half; everything that must be *right* is structural —
        // a model is not allowed to edit a target triple or a partition table.
        let fillable: Vec<&str> = out
            .files
            .iter()
            .filter(|f| !f.structural)
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(
            fillable,
            vec!["src/main.rs"],
            "only the app's source is fillable"
        );
        assert_eq!(out.fill_roots, vec!["src".to_string()]);

        // The actor holds the contract's traits; the one line that belongs to the backend is named,
        // not guessed.
        let main_rs = out.files.iter().find(|f| f.path == "src/main.rs").unwrap();
        assert!(
            main_rs
                .content
                .contains("use weather_hal::hal::{DelayMs, Led};"),
            "{}",
            main_rs.content
        );
        assert!(main_rs
            .content
            .contains("use weather_hal_esp32::{FamilyDelay, GpioLed, StdSpawner};"));
        assert!(main_rs.content.contains("fn board_led() -> GpioLed"));
    }

    /// One board, and the refusal names why: a `main` is written for one chip.
    #[test]
    fn an_app_needs_exactly_one_board() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[
            (
                "esp32",
                "esp-idf",
                Some("esp32"),
                Some("xtensa-esp32-espidf"),
            ),
            (
                "esp32c6",
                "esp-idf",
                Some("esp32"),
                Some("riscv32imac-esp-espidf"),
            ),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());
        let hal_dir = tempfile::tempdir().unwrap();
        hal_on_disk(hal_dir.path(), "weather", &["esp32"]);
        let hal_path = hal_dir.path().to_string_lossy().to_string();

        let none = embedded_app_scaffold("App", &[], &hal_path, hal_dir.path()).unwrap_err();
        assert!(none.contains("needs a board"), "{none}");

        let two = embedded_app_scaffold(
            "App",
            &["esp32".into(), "esp32c6".into()],
            &hal_path,
            hal_dir.path(),
        )
        .unwrap_err();
        assert!(two.contains("targets **one** board"), "{two}");
        assert!(
            two.contains("esp32, esp32c6"),
            "the refusal names them: {two}"
        );
    }

    /// What the app depends on is **read**, so every way of naming the wrong thing is a refusal
    /// here rather than a build failure two steps later.
    #[test]
    fn an_app_refuses_a_directory_that_is_not_a_hal_with_a_backend_for_the_board() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[
            (
                "esp32",
                "esp-idf",
                Some("esp32"),
                Some("xtensa-esp32-espidf"),
            ),
            (
                "rp2040",
                "rp2040",
                Some("rp2040"),
                Some("thumbv6m-none-eabi"),
            ),
        ]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        // A directory with no manifest at all.
        let empty = tempfile::tempdir().unwrap();
        let missing =
            embedded_app_scaffold("App", &["esp32".into()], "…", empty.path()).unwrap_err();
        assert!(missing.contains("no embedded-HAL project at"), "{missing}");

        // A Cargo project, but not a HAL: the marker is what makes it one.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(
            other.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let not_a_hal =
            embedded_app_scaffold("App", &["esp32".into()], "…", other.path()).unwrap_err();
        assert!(
            not_a_hal.contains("is not an embedded-HAL project"),
            "{not_a_hal}"
        );

        // A real HAL with no backend for the chosen board: the app would have nothing to link.
        let hal_dir = tempfile::tempdir().unwrap();
        hal_on_disk(hal_dir.path(), "weather", &["rp2040"]);
        let no_backend =
            embedded_app_scaffold("App", &["esp32".into()], "…", hal_dir.path()).unwrap_err();
        assert!(
            no_backend.contains("has no 'esp32' backend"),
            "{no_backend}"
        );
        assert!(
            no_backend.contains("crates/weather-hal-rp2040"),
            "the members are listed: {no_backend}"
        );
    }

    /// A family whose app wiring has not been measured is refused by name, with the reason — rather
    /// than emitted from a template that would fail on the first board it met.
    #[test]
    fn a_family_without_app_wiring_is_refused_by_name() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[(
            "rp2040",
            "rp2040",
            Some("rp2040"),
            Some("thumbv6m-none-eabi"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());
        let hal_dir = tempfile::tempdir().unwrap();
        hal_on_disk(hal_dir.path(), "weather", &["rp2040"]);

        let refused =
            embedded_app_scaffold("App", &["rp2040".into()], "…", hal_dir.path()).unwrap_err();
        assert!(
            refused.contains("no application wiring is known for family 'rp2040'"),
            "{refused}"
        );
        // The reason is the useful half: the executor belongs to the backend, and a no_std backend
        // has not written it yet.
        assert!(refused.contains("executor"), "{refused}");
    }

    /// A host platform is not a board: an application needs something it can flash.
    #[test]
    fn a_host_platform_is_not_an_application_target() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[("rpi5", "linux", None, None)]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());
        let hal_dir = tempfile::tempdir().unwrap();

        let refused =
            embedded_app_scaffold("App", &["rpi5".into()], "…", hal_dir.path()).unwrap_err();
        assert!(refused.contains("is not an embedded platform"), "{refused}");
    }

    /// The wiring, **built**: a real HAL and a real app on disk, cross-compiled for the board.
    ///
    /// This is the check the unit tests cannot make. They pin what the files *say*; this proves the
    /// project those files describe actually builds — the path dependencies resolve to the crates
    /// the HAL wrote, the target and `MCU` reach esp-idf-sys, `build.rs` supplies the link args, and
    /// the `sdkconfig.defaults` are enough for a standard image.
    ///
    /// `#[ignore]`d because it needs the `esp` rustup toolchain and the ESP-IDF SDK, and because the
    /// first build for a chip compiles IDF itself (minutes). Run it with:
    ///
    ///     cargo test -p spire-code --lib build::embedded_app -- --ignored
    ///
    /// The environment comes from the **build module's own helpers** (`esp_toolchain_bin`,
    /// `libclang_path`, `esp_idf_tools_install_dir`), so the test builds with exactly the environment
    /// a real esp build gets — a second copy of that resolution could pass here and fail there.
    #[ignore = "live cross-build: needs the esp toolchain and the ESP-IDF SDK"]
    #[test]
    fn an_app_cross_compiles_against_a_real_hal() {
        // The real registry: the point of this test is the toolchain and the platform as they are.
        let Some(toolchain_bin) = crate::build::esp::esp_toolchain_bin() else {
            eprintln!("skipping: no `esp` rustup toolchain (run `espup install`)");
            return;
        };
        let Some(libclang) = crate::build::esp::libclang_path() else {
            eprintln!("skipping: no esp clang inside the esp toolchain");
            return;
        };

        let hal_dir = tempfile::tempdir().unwrap();
        hal_on_disk(hal_dir.path(), "weather", &["esp32"]);
        let app_dir = tempfile::tempdir().unwrap();
        let out = embedded_app_scaffold(
            "weather-node",
            &["esp32".into()],
            &hal_dir.path().to_string_lossy(),
            hal_dir.path(),
        )
        .expect("an esp32 app against an esp32 HAL");
        for file in &out.files {
            let path = app_dir.path().join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &file.content).unwrap();
        }

        // The app's LED is the fill's line, and `todo!()` is what makes the project checkable before
        // any fill has run: it type-checks, so everything around it compiles and the link is real.
        let path_var = format!(
            "{}:{}",
            toolchain_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let build = std::process::Command::new("cargo")
            .arg("build")
            .current_dir(app_dir.path())
            .env("PATH", path_var)
            .env("LIBCLANG_PATH", &libclang)
            .env(
                "ESP_IDF_TOOLS_INSTALL_DIR",
                crate::build::esp::esp_idf_tools_install_dir(),
            )
            .output()
            .expect("run cargo");
        let stdout = String::from_utf8_lossy(&build.stdout);
        let stderr = String::from_utf8_lossy(&build.stderr);
        assert!(
            build.status.success(),
            "the scaffolded app must build against the HAL it names.\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
        );
    }
}
