// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **Embedded application** scaffold — a firmware binary for one board, on `esp-hal` + embassy.
//!
//! ```text
//! <app>/
//!   Cargo.toml            the board's crate set, and `spire-embedded` by path
//!   .cargo/config.toml    the target, the runner, and the linker script esp-hal wants
//!   src/main.rs           the actor and the board's facts   ← the fill writes this
//! ```
//!
//! Five things decide this shape, and every one of them was measured on a board — the pilot in
//! `spire-embedded/examples/c3-blink`, where each of these cost a build or a flash to find.
//!
//! 1. **There is no HAL project to depend on.** The peripheral traits are `embedded-hal`'s, the HAL is
//!    the *vendor's* crate (`esp-hal`), and the actor system is `spire-embedded`. So an application is
//!    an ordinary crate again: what it needs from a board is a target, a runner and some pins, and all
//!    three are its own business. What it path-deps is `spire-embedded` — from the user's checkout,
//!    because that crate is not published.
//! 2. **The board is chosen here, and only one.** A `main` names pins and a chip feature; two boards in
//!    one binary would need `#[cfg]`s the wizard cannot express.
//! 3. **No `build.rs`, no `sdkconfig.defaults`, no `MCU` variable.** The C3's target
//!    (`riscv32imc-unknown-none-elf`) is a **stock rustup target**, so there is no `-Zbuild-std`, no
//!    vendor SDK and no linker wrapper. Every piece of the esp-idf friction is simply gone.
//! 4. **The image still needs an ESP-IDF application descriptor**, or `espflash` refuses it *after* a
//!    clean build. The reason IDF appears at all on a bare-metal app is that the board already carries
//!    an IDF bootloader and the image rides it.
//! 5. **`embassy-executor` must not enable a `platform-*` feature.** esp-rtos exports the `__pender`
//!    wake function itself; embassy's own platform defines a second one and the link fails with
//!    `duplicate symbol: __pender`.
//!
//! What is left as a `todo!()` is the board's LED — the one fact no scaffold can know — and it is
//! *typed*, so everything around it compiles, links and flashes before any fill has run.

use spire_core::build_types::ProjectStructure;
use std::path::Path;
use toml_edit::DocumentMut;

/// The app-side facts for one **chip** — the board half of a build.
///
/// Keyed by platform id rather than by family, because under this shape the chip is what the crate
/// features name: `esp-hal` takes `esp32c3`/`esp32s3`/… and so does every other Espressif crate here.
/// A family row could not tell a C3 from an S3, which is exactly the distinction that matters.
struct AppSpec {
    /// The chip feature every Espressif crate in this app takes.
    chip: &'static str,
    /// The rustup target. Stock for RISC-V; Xtensa would need the `esp` toolchain.
    target: &'static str,
    /// How `cargo run` reaches the board.
    runner: &'static str,
    /// Linker arguments esp-hal needs. `linkall.x` is its linker script — it places `.rodata`, keeps
    /// the boot ROM's stack, and so on — and esp-hal adds its directory to the search path.
    rustflags: &'static [&'static str],
}

/// The app-side wiring for one chip, or `None` when this scaffold does not know it.
///
/// Returning `None` rather than defaulting is what stops the scaffold from emitting a manifest whose
/// dependencies cannot resolve or a config pinning a target the chip does not use.
///
/// **`esp32c3` only, and that is deliberate.** It is the one row measured on hardware: built, flashed,
/// actor running, LED cycling (see the pilot's README). The other ESP32 chips are a row each — the C6
/// and the RISC-V family differ only in `chip` and `target`, and the S2/S3 need the Xtensa toolchain —
/// but adding a row nobody has flashed is how a scaffold starts lying.
fn app_spec(platform_id: &str) -> Option<AppSpec> {
    match platform_id {
        "esp32c3" => Some(AppSpec {
            chip: "esp32c3",
            target: "riscv32imc-unknown-none-elf",
            runner: "espflash flash --monitor",
            rustflags: &["-C", "link-arg=-Tlinkall.x"],
        }),
        _ => None,
    }
}
// CHUNK-1-END

/// Why a board list cannot be used for an application: none, or more than one.
fn application_board_refusal(platforms: &[String]) -> String {
    match platforms {
        [] => {
            "an embedded application needs a board: the board decides the target, the crates and \
               the pins"
                .to_string()
        }
        many => format!(
            "an embedded application targets **one** board, and {} were given ({}); an app is a \
             `main` for a specific chip, so split it or pick one",
            many.len(),
            many.join(", ")
        ),
    }
}

/// The `[package.metadata.spire]` marker for an application.
///
/// Records the structure *and* where `spire-embedded` was taken from, so a later reader — the
/// analyzer, a rebuild, a human — does not have to infer the dependency from a `path = "../.."`.
fn marker(embedded_path: &str) -> String {
    format!(
        "[package.metadata.spire]\n\
         structure = \"{}\"\n\
         # The checkout this application builds against. The path dependency above is resolved from\n\
         # it, so moving one without the other is a broken build rather than a mystery.\n\
         embedded_path = \"{}\"\n",
        ProjectStructure::EmbeddedApp.as_str(),
        embedded_path
    )
}

/// The container's crate, from a checkout root — or from the crate itself.
///
/// Returns the crate's **name** and the directory to path-depend on *relative to the checkout*, which is
/// empty when the caller pointed straight at the crate. Both are accepted because both are things a user
/// reasonably points at: the repository they cloned, or the crate inside it.
///
/// The name is **read, not assumed**. The container's library crate is named after its project —
/// `spire-embedded/` holds `crates/spire-embedded`, and a container called `weather-embedded` holds
/// `crates/weather-embedded` — so `spire-embedded` is the name of one project rather than a crate id
/// every container shares. An application that path-deps a crate that is not there builds nowhere.
fn embedded_crate(root: &Path) -> Result<(String, String), String> {
    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let doc: DocumentMut = manifest
        .parse()
        .map_err(|e| format!("{} does not parse: {e}", manifest_path.display()))?;

    // A package, not a workspace: the crate itself was pointed at, so its directory name is its name.
    if doc.get("package").is_some() {
        let name = doc
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(|name| name.as_str())
            .ok_or_else(|| {
                format!(
                    "{} has a `[package]` with no `name`",
                    manifest_path.display()
                )
            })?;
        return Ok((name.to_string(), String::new()));
    }

    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("cannot read a project name from {}", root.display()))?;
    let dir = format!("crates/{name}");
    if !root.join(&dir).join("Cargo.toml").is_file() {
        return Err(format!(
            "no container library at {} — the container's crate is named after its project, so '{dir}' \
             is the crate this application path-deps (point at the workspace, or at the crate itself)",
            root.join(&dir).display()
        ));
    }
    Ok((name, dir))
}
// CHUNK-2-END

/// Emit an embedded **application**: a firmware binary for one board.
///
/// `embedded_path` is the path as it should appear in the manifest and the marker (relative when the
/// two projects are near each other, absolute otherwise); `embedded_root` is where that checkout
/// actually is, so this can refuse a directory that is not one before a `cargo build` has to.
pub(crate) fn embedded_app_scaffold(
    project_name: &str,
    platforms: &[String],
    embedded_path: &str,
    embedded_root: &Path,
) -> Result<super::ScaffoldOutput, String> {
    let name = project_name.trim().to_lowercase().replace(' ', "-");

    // One board. A `main` names pins and a chip feature; two would need `#[cfg]`s the wizard cannot
    // express, and a binary that half-runs on each.
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
    let spec = app_spec(platform_id).ok_or_else(|| {
        format!(
            "no application wiring is known for '{platform_id}' yet. `esp32c3` is the one row measured \
             on a board — built, flashed, LED cycling — and another chip is a row here plus a platform \
             file, not a guess"
        )
    })?;

    // The container, read rather than assumed: a wrong directory is cheaper to refuse here than to
    // discover at the first `cargo build`.
    let (container, crate_dir) = embedded_crate(embedded_root)?;
    // The crate's *identifier* form, for the `use` paths in `main.rs`: a crate is named with dashes and
    // referred to with underscores.
    let container_id = container.replace('-', "_");

    // If the platform names a target, it must be the one this scaffold would emit. A platform file
    // carrying esp-idf's triple while the wiring says bare-metal is a conflict rather than a
    // preference — and choosing silently is how a build fails with an error about `core`.
    if let Some(rust) = platform.rust.as_ref() {
        if !rust.target.is_empty() && rust.target != spec.target {
            return Err(format!(
                "platform '{platform_id}' declares target '{}', but this application's wiring builds \
                 for '{}' ({}) — fix the platform file, or drop its `rust.target` and let this decide",
                rust.target, spec.target, spec.chip
            ));
        }
    }

    let embedded_dep = if crate_dir.is_empty() {
        embedded_path.trim_end_matches('/').to_string()
    } else {
        format!("{}/{}", embedded_path.trim_end_matches('/'), crate_dir)
    };

    let bsp_dep = bsp_dep_line(embedded_root, platform_id, embedded_path);
    let manifest_rs = app_manifest(
        &name,
        embedded_path,
        &embedded_dep,
        &bsp_dep,
        &container,
        &spec,
    );
    let main_rs = APP_MAIN_RS.replace("__CONTAINER_ID__", &container_id);
    let files = vec![
        super::ScaffoldFile {
            path: "Cargo.toml".to_string(),
            content: manifest_rs.clone(),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: ".cargo/config.toml".to_string(),
            content: cargo_config(&spec),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "README.md".to_string(),
            content: readme(&name, &container, &spec),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "src/main.rs".to_string(),
            content: main_rs.clone(),
            // The fill's file, and the only one: the actor is written out in full, and the board's LED
            // is the single `todo!()` this scaffold cannot know.
            structural: false,
            ..Default::default()
        },
    ];

    Ok(super::ScaffoldOutput {
        build_file: "Cargo.toml".to_string(),
        build_content: manifest_rs,
        source_dir: "src".to_string(),
        source_file: "src/main.rs".to_string(),
        source_content: main_rs,
        files,
        platform_targets: vec![platform_id.clone()],
        // The application's own source is the only fillable half. There is nothing else to fill: no
        // contract to implement, no backend, and no BSP unless a board needs one generated.
        fill_roots: vec!["src".to_string()],
        dependency_sections: vec!["Cargo.toml".to_string()],
        structure: ProjectStructure::EmbeddedApp,
        embedded: true,
    })
}
// CHUNK-3-END

/// The BSP dependency line for an application, when the container has a BSP for its board.
///
/// A BSP is per board, so its absence is ordinary — an upstream BSP may cover the board, or the board
/// may need none. What matters is that the application *names* it when it exists: the board's facts
/// live there, and a `main.rs` that repeats them is a `main.rs` that can drift from them.
fn bsp_dep_line(container: &Path, platform_id: &str, embedded_path: &str) -> String {
    let crate_name = format!("spire-bsp-{platform_id}");
    let dir = format!("crates/{crate_name}");
    if !container.join(&dir).join("Cargo.toml").is_file() {
        return String::new();
    }
    format!(
        "# This board's BSP: its facts — which pin the LED is on, whether it is active-low — so that\n\
         # this file does not repeat them and cannot drift from them.\n\
         {crate_name} = {{ path = \"{}/{dir}\" }}\n\n",
        embedded_path.trim_end_matches('/')
    )
}

/// The application's manifest: the board's crate set, the container's crate, and the marker.
///
/// Two paths, deliberately: `embedded_path` is the *checkout* the user pointed at (what the marker
/// records, and what a human would move), while `dep_path` is the crate inside it that a manifest has to
/// name. They differ by `crates/<project>` — the container's crate is named after its project.
fn app_manifest(
    name: &str,
    embedded_path: &str,
    dep_path: &str,
    bsp_dep: &str,
    container: &str,
    spec: &AppSpec,
) -> String {
    APP_MANIFEST
        .replace("__NAME__", name)
        .replace("__EMBEDDED__", dep_path)
        .replace("__CHIP__", spec.chip)
        .replace("__BSP_DEP__", bsp_dep)
        .replace("__CONTAINER__", container)
        .replace("__MARKER__", &marker(embedded_path))
}

/// `.cargo/config.toml`: the target, the runner, and esp-hal's linker script.
fn cargo_config(spec: &AppSpec) -> String {
    let flags = spec
        .rustflags
        .iter()
        .map(|flag| format!("\"{flag}\""))
        .collect::<Vec<_>>()
        .join(", ");
    APP_CARGO_CONFIG
        .replace("__TARGET__", spec.target)
        .replace("__RUNNER__", spec.runner)
        .replace("__RUSTFLAGS__", &flags)
}

/// The application's README: what it depends on, and the one command that puts it on a board.
fn readme(name: &str, container: &str, spec: &AppSpec) -> String {
    README_MD
        .replace("__NAME__", name)
        .replace("__CHIP__", spec.chip)
        .replace("__TARGET__", spec.target)
        .replace("__RUNNER__", spec.runner)
        // The container by name and by identifier: prose says `weather-embedded`, code says
        // `weather_embedded`, and the README has to read right in both.
        .replace("__CONTAINER__", container)
        .replace("__CONTAINER_ID__", &container.replace('-', "_"))
}

/// The application's manifest template. `__EMBEDDED__` is where the user's container is.
const APP_MANIFEST: &str = r#"# __NAME__ — an embedded application: one board, one binary.
#
# There is no HAL project to depend on. The peripheral traits are `embedded-hal`'s, the HAL is
# `esp-hal`, and the actor system is `__CONTAINER__` — so this is an ordinary crate, and what it needs
# from its board is a target, a runner and some pins.
[package]
name = "__NAME__"
version = "0.1.0"
edition = "2021"

[dependencies]
# The actor system, with the embassy runtime wired in.
__CONTAINER__ = { path = "__EMBEDDED__", features = ["embassy"] }

# The HAL, and the RTOS half of it. `esp-rtos` — not `esp-hal-embassy`, which is no longer maintained
# — is where the embassy executor for esp-hal lives. Both take the chip feature.
esp-hal = { version = "1.2", features = ["__CHIP__", "unstable"] }
esp-rtos = { version = "0.4", features = ["embassy", "__CHIP__"] }

# The task machinery — and deliberately **no** `platform-*` feature. esp-rtos exports the `__pender`
# wake function itself, and embassy's own platform defines a second one; both together fail to link
# with `duplicate symbol: __pender`, which reads like an application bug and is not.
embassy-executor = { version = "0.10", features = ["executor-thread"] }
# The mailbox's channel: the application owns its storage, so it names the crate.
embassy-sync = "0.8"

# The console on UART0. `default-features = false` because esp-println's default is its own `auto`
# probe, and it refuses to coexist with an explicit transport.
esp-println = { version = "0.18", default-features = false, features = ["__CHIP__", "uart"] }
# The panic handler. An unreferenced crate is not linked, so `main.rs` carries
# `use esp_backtrace as _;` — without which the build asks for a `#[panic_handler]`.
esp-backtrace = { version = "0.20", features = ["__CHIP__", "panic-handler", "println"] }
# The ESP-IDF application descriptor: `espflash` refuses an image without one, and refuses it at
# *flash* time — after a clean build, so green build output says nothing about it.
esp-bootloader-esp-idf = { version = "0.5", features = ["__CHIP__"] }

__BSP_DEP____MARKER__"#;

/// The `.cargo/config.toml` template — the board's compile-time facts.
const APP_CARGO_CONFIG: &str = r#"# What a build for this board needs, declared by the project rather than injected per command line.
[build]
target = "__TARGET__"

[target.__TARGET__]
runner = "__RUNNER__"
# esp-hal's linker script. It places `.rodata`, keeps the boot ROM's stack where the ROM left it, and
# so on; esp-hal ships it and adds its directory to the linker's search path.
rustflags = [__RUSTFLAGS__]
"#;
// CHUNK-4-END

/// The application's source template.
///
/// The actor is written out in full, because it is the portable part and the part a user is meant to
/// change: it holds `embedded-hal`'s traits and no vendor type. What is left as a `todo!()` is the one
/// fact no scaffold can know — this board's LED — and it is *typed*, so everything around it compiles,
/// links and flashes before any fill has run.
const APP_MAIN_RS: &str = r#"//! The application: one actor, one board.
//!
//! Note what this file does *not* contain: no vendor type in the actor, no chip name, no `unsafe`. The
//! actor below would be the same on any board that can hand it an output pin and a delay — which is
//! what makes it *our* code rather than an interface to somebody else's.
//!
//! Embassy tasks cannot be generic, so the task names the *concrete* type its board produces. That is
//! embassy's constraint, not ours, and it is why `spire-embedded` ships `embassy::run`: the loop is
//! generic, the task is not, and the difference is one line.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

// Linked, not called: the panic handler lives here, and an unreferenced crate is not linked — which
// reads as "`#[panic_handler]` function required, but not found".
use esp_backtrace as _;

use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_println::println;

use __CONTAINER_ID__::actor::{Actor, Mailbox};
use __CONTAINER_ID__::embedded_hal::delay::DelayNs;
use __CONTAINER_ID__::embedded_hal::digital::OutputPin;
use __CONTAINER_ID__::embassy::{run, EmbassyMailbox};

// The ESP-IDF application descriptor, which the image must carry or `espflash` refuses it at flash
// time. Module level, not inside `main`: the macro emits the static the bootloader reads.
esp_bootloader_esp_idf::esp_app_desc!();

/// Half a second on, half a second off.
const PERIOD_MS: u32 = 500;

/// What the actor understands.
#[derive(Debug, Clone, Copy)]
enum Blink {
    Toggle,
    Wait(u32),
    /// Blink forever: one instruction, then the actor owns it — which is how real firmware is shaped,
    /// and what makes the mailbox a control surface rather than a command stream.
    Repeat { period_ms: u32 },
}

/// The actor. Generic over the traits, so this exact code is what runs on any board.
struct Blinker<L, D> {
    led: L,
    delay: D,
    on: bool,
}

impl<L: OutputPin, D: DelayNs> Actor for Blinker<L, D> {
    type Message = Blink;

    fn handle(&mut self, msg: Blink) {
        match msg {
            Blink::Toggle => {
                self.on = !self.on;
                // A chip pin's level write cannot fail; the `Result` exists for expanders and buses.
                let _ = if self.on {
                    self.led.set_high()
                } else {
                    self.led.set_low()
                };
            }
            Blink::Wait(ms) => self.delay.delay_ms(ms),
            Blink::Repeat { period_ms } => loop {
                self.handle(Blink::Toggle);
                self.handle(Blink::Wait(period_ms));
            },
        }
    }
}

/// The mailbox's storage, and the application's to own: `no_std` has no allocator, and a channel's
/// capacity belongs in its type.
static CHANNEL: Channel<CriticalSectionRawMutex, Blink, 4> = Channel::new();

/// Embassy demands a concrete task per actor, so this function plus `run` is the entire wiring.
#[embassy_executor::task]
async fn blink(led: Output<'static>, pause: Delay) {
    run(
        Blinker {
            led,
            delay: pause,
            on: false,
        },
        CHANNEL.receiver(),
    )
    .await
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    // TODO: this board's LED — the only board fact in this file, and the reason it is the fill's file.
    //
    // A board with a plain LED:
    //     Output::new(peripherals.GPIO8, esp_hal::gpio::Level::Low, esp_hal::gpio::OutputConfig::default())
    // A board whose LED is *addressable* needs no output pin at all: it takes an `spi::master::Spi` and
    // `__CONTAINER_ID__::drivers::Ws2812`, because a level is not a colour. Either way it is one line,
    // and it is typed so that the rest of this file compiles, links and flashes without it.
    let led: Output<'static> = todo!("this board's LED, from this board's pins");

    spawner.spawn(blink(led, Delay::new()).expect("one task, one pool slot"));

    let mailbox = EmbassyMailbox::new(CHANNEL.sender());
    mailbox
        .try_send(Blink::Repeat {
            period_ms: PERIOD_MS,
        })
        .expect("a fresh mailbox has room");
    println!("app: alive, actor spawned");

    // The actor owns the blinking; this task is done. Park rather than spin — the executor has other
    // things to do, and a busy main is the classic first firmware bug.
    core::future::pending::<()>().await
}
"#;
// CHUNK-5-END

/// The application's README: what it is made of, and the two commands that matter.
const README_MD: &str = r#"# __NAME__ — an embedded application

One binary for one board (`__CHIP__`), built on `esp-hal` with the `__CONTAINER__` actor system.

```text
src/main.rs         the actor, and this board's facts — the only file a fill should touch
Cargo.toml          the board's crates, and `__CONTAINER__` by path
.cargo/config.toml  the target (`__TARGET__`) and the runner
```

There is no HAL project to link against and no BSP to write: the peripheral traits are `embedded-hal`'s,
the HAL is the vendor's crate, and the actor system is `__CONTAINER__`. What is left for this project
is its own `main` — and the board's pins, which are in it.

## Build and run

The target is a **stock rustup target**, so there is no `-Zbuild-std`, no vendor SDK, no `sdkconfig` and
no `MCU` variable. Two things about this machine are worth knowing anyway, because each fails in a way
that does not name its cause:

- **use rustup's toolchain explicitly** if `cargo`/`rustc` on your `PATH` belong to another install
  (Homebrew's, for instance). Otherwise the build dies with "can't find crate for `core`", which reads
  like a missing *target* rather than a missing *toolchain*.
- **`rust-lld` may not find `libLLVM.dylib`** through its own rpath, so the link aborts inside dyld with
  "Library not loaded: @rpath/libLLVM.dylib". The file is there; the fallback path is what finds it.

```sh
T=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin

PATH="$T/bin:$PATH" DYLD_FALLBACK_LIBRARY_PATH="$T/lib" cargo build --release
PATH="$T/bin:$PATH" DYLD_FALLBACK_LIBRARY_PATH="$T/lib" cargo run --release
```

`cargo run` hands over to the runner in `.cargo/config.toml`: __RUNNER__.

## The one thing left to write

The LED in `src/main.rs` is a `todo!()` — this board's pin, which no scaffold can know. It is *typed*,
so everything around it compiles, links and flashes while it stays unwritten, and `cargo build` is a
real check of the wiring rather than a partial one.

One note from the pilot this scaffold was generated from: if the board's LED is **addressable** (an
WS2812-family part), a GPIO level will not light it — it wants `__CONTAINER_ID__::drivers::Ws2812` over an
`Spi`, because a level is not a colour.
"#;
// CHUNK-6-END

#[cfg(test)]
mod tests {
    use super::*;

    /// A hermetic registry, so the tests can name chips the machine may not have and prove the
    /// refusals without touching `~/.spire/platforms`.
    ///
    /// `(id, os, family, rust target)` — the target is the one the scaffold checks its own wiring
    /// against, which is what makes a platform file that disagrees a refusal rather than a surprise.
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

    /// A `spire-embedded` checkout on disk: the workspace, with the crate inside it.
    /// The container's crate, relative to `root`, as the derivation names it: after the project.
    ///
    /// A temporary directory's own name here — deliberately **not** `spire-embedded`. That is the name
    /// of one project, and a fixture that reused it would pass while every other container's crate was
    /// looked for in the wrong place.
    fn container_dir(root: &Path) -> String {
        format!("crates/{}", root.file_name().unwrap().to_string_lossy())
    }

    /// A container on disk: the workspace manifest, and the library crate inside it.
    ///
    /// Both, because a container *is* the workspace — its crate is a member named after the project, and
    /// the root manifest is what says so.
    fn embedded_on_disk(root: &Path) {
        let member = container_dir(root);
        let name = member.trim_start_matches("crates/");
        std::fs::write(
            root.join("Cargo.toml"),
            format!("[workspace]\nresolver = \"2\"\nmembers = [\"{member}\"]\n"),
        )
        .unwrap();
        let crate_dir = root.join(&member);
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\n"),
        )
        .unwrap();
        std::fs::write(crate_dir.join("src/actor.rs"), "// the actor system\n").unwrap();
    }
    // CHUNK-7-END

    /// The whole point of the shape: an application is an **ordinary crate**, and nothing has to exist
    /// beside it. The traits are `embedded-hal`'s, the HAL is `esp-hal`, the actor system is
    /// `spire-embedded` — so there is no HAL project to scaffold, and no BSP to write.
    #[test]
    fn an_app_is_a_normal_crate_that_path_deps_spire_embedded() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[(
            "esp32c3",
            "esp-hal",
            Some("esp32"),
            Some("riscv32imc-unknown-none-elf"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let embedded = tempfile::tempdir().unwrap();
        embedded_on_disk(embedded.path());
        let path = embedded.path().to_string_lossy().to_string();

        let out =
            embedded_app_scaffold("Weather Node", &["esp32c3".into()], &path, embedded.path())
                .expect("an esp32c3 application");

        // The files — and, just as much the point, the ones that are *gone*.
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        for expected in [
            "Cargo.toml",
            ".cargo/config.toml",
            "src/main.rs",
            "README.md",
        ] {
            assert!(paths.contains(&expected), "missing {expected}: {paths:?}");
        }
        for gone in ["build.rs", "sdkconfig.defaults"] {
            assert!(
                !paths.contains(&gone),
                "`{gone}` belongs to the esp-idf shape, which this is not: {paths:?}"
            );
        }

        let manifest = out
            .files
            .iter()
            .find(|f| f.path == "Cargo.toml")
            .unwrap()
            .content
            .clone();
        // The one dependency that is ours: by path, into the *crate* rather than the workspace, and named
        // after the container's project rather than after ours.
        let container = embedded
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            manifest.contains(&format!(
                "{container} = {{ path = \"{path}/{}\"",
                container_dir(embedded.path())
            )),
            "{manifest}"
        );
        // The measured crate set, including the ones that are easy to get wrong.
        for expected in [
            "esp-hal = { version = \"1.2\", features = [\"esp32c3\", \"unstable\"] }",
            "esp-rtos = { version = \"0.4\", features = [\"embassy\", \"esp32c3\"] }",
            "embassy-executor = { version = \"0.10\", features = [\"executor-thread\"] }",
            "esp-println = { version = \"0.18\", default-features = false, features = [\"esp32c3\", \"uart\"] }",
            "esp-bootloader-esp-idf = { version = \"0.5\", features = [\"esp32c3\"] }",
        ] {
            assert!(
                manifest.contains(expected),
                "missing `{expected}`:\n{manifest}"
            );
        }
        // No build script, because there is no SDK to link.
        assert!(!manifest.contains("build-dependencies"), "{manifest}");
        // The marker: what this is, and where the dependency came from.
        assert!(
            manifest.contains("structure = \"embedded_app\""),
            "{manifest}"
        );
        assert!(
            manifest.contains(&format!("embedded_path = \"{path}\"")),
            "{manifest}"
        );
        assert_eq!(out.structure, ProjectStructure::EmbeddedApp);
        assert!(out.embedded);

        // The config: a stock target, the runner, and esp-hal's linker script.
        let config = out
            .files
            .iter()
            .find(|f| f.path == ".cargo/config.toml")
            .unwrap();
        assert!(
            config
                .content
                .contains("target = \"riscv32imc-unknown-none-elf\""),
            "{}",
            config.content
        );
        assert!(config.content.contains("espflash"), "{}", config.content);
        assert!(config.content.contains("-Tlinkall.x"), "{}", config.content);

        // The app's source is the only fillable half — and the board's LED is the one `todo!()`, typed
        // so that everything around it builds.
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
        let main_rs = out
            .files
            .iter()
            .find(|f| f.path == "src/main.rs")
            .unwrap()
            .content
            .clone();
        for expected in [
            "todo!(\"this board's LED",
            "esp_app_desc!();",
            "use esp_backtrace as _;",
            "#[esp_rtos::main]",
            "embassy::run",
        ] {
            assert!(
                main_rs.contains(expected),
                "missing `{expected}`:\n{main_rs}"
            );
        }
    }
    // CHUNK-8-END

    /// One board, and the refusal names why: a `main` is written for one chip.
    #[test]
    fn an_app_needs_exactly_one_board() {
        let embedded = tempfile::tempdir().unwrap();
        embedded_on_disk(embedded.path());

        // Both refusals come before the registry is consulted, so neither needs a platform dir.
        let none = embedded_app_scaffold("App", &[], "…", embedded.path()).unwrap_err();
        assert!(none.contains("needs a board"), "{none}");

        let two = embedded_app_scaffold(
            "App",
            &["esp32c3".into(), "esp32c6".into()],
            "…",
            embedded.path(),
        )
        .unwrap_err();
        assert!(two.contains("targets **one** board"), "{two}");
    }

    /// A directory that is not a container is refused here, not at the first build.
    #[test]
    fn an_app_refuses_a_directory_that_is_not_a_container() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[(
            "esp32c3",
            "esp-hal",
            Some("esp32"),
            Some("riscv32imc-unknown-none-elf"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let empty = tempfile::tempdir().unwrap();
        let err = embedded_app_scaffold("App", &["esp32c3".into()], "…", empty.path()).unwrap_err();
        assert!(err.contains("Cargo.toml"), "{err}");

        // A directory that *is* a project — the HAL workspace this replaced — is refused as well, by
        // the same check rather than by a later, more confusing failure.
        let hal = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(hal.path().join("crates/weather-hal/src")).unwrap();
        std::fs::write(
            hal.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/weather-hal\"]\n",
        )
        .unwrap();
        let err = embedded_app_scaffold("App", &["esp32c3".into()], "…", hal.path()).unwrap_err();
        assert!(err.contains("no container library at"), "{err}");
    }

    /// A chip with no wiring row is refused by name, and the refusal says what *is* known.
    ///
    /// This is the rule that keeps the scaffold honest: a row is added when a board has been flashed,
    /// not when its datasheet has been read.
    #[test]
    fn a_chip_without_wiring_is_refused_by_name() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[(
            "esp32s3",
            "esp-hal",
            Some("esp32"),
            Some("xtensa-esp32s3-none-elf"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let embedded = tempfile::tempdir().unwrap();
        embedded_on_disk(embedded.path());

        let err =
            embedded_app_scaffold("App", &["esp32s3".into()], "…", embedded.path()).unwrap_err();
        assert!(
            err.contains("no application wiring is known for 'esp32s3'"),
            "{err}"
        );
        assert!(
            err.contains("esp32c3"),
            "the refusal names what is known: {err}"
        );
    }

    /// A host platform is not something to flash.
    #[test]
    fn a_host_platform_is_not_an_application_target() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[("x86-64", "linux", None, None)]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let err =
            embedded_app_scaffold("App", &["x86-64".into()], "…", Path::new("…")).unwrap_err();
        assert!(err.contains("not an embedded platform"), "{err}");
    }

    /// A platform whose declared target disagrees with this scaffold's wiring is a **conflict**, not a
    /// preference. Choosing silently is how a build dies with an error about `core`.
    #[test]
    fn a_platform_that_disagrees_with_the_wiring_is_refused_rather_than_guessed() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // An `esp32c3` platform file written for the *old* esp-idf shape.
        let reg = registry(&[(
            "esp32c3",
            "esp-idf",
            Some("esp32"),
            Some("riscv32imac-esp-espidf"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let embedded = tempfile::tempdir().unwrap();
        embedded_on_disk(embedded.path());

        let err =
            embedded_app_scaffold("App", &["esp32c3".into()], "…", embedded.path()).unwrap_err();
        assert!(
            err.contains("declares target 'riscv32imac-esp-espidf'"),
            "{err}"
        );
        assert!(
            err.contains("riscv32imc-unknown-none-elf"),
            "the refusal names both sides: {err}"
        );
    }

    /// The live test: scaffold an application and **build** it for the chip.
    ///
    /// This is the check that catches scaffold drift — versions that no longer resolve, an import the
    /// crate no longer exports, a template that no longer compiles — and nothing else here would
    /// notice, because the other tests assert *strings*, and a string is not a build.
    ///
    /// Ignored by default: it needs the RISC-V target, a `spire-embedded` checkout and a network.
    ///
    /// ```sh
    /// SPIRE_EMBEDDED_ROOT=../spire-embedded cargo test -p spire-code --lib \
    ///     a_scaffolded_app_builds_for_its_chip -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "live build: needs the target, a spire-embedded checkout and a network"]
    fn a_scaffolded_app_builds_for_its_chip() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[(
            "esp32c3",
            "esp-hal",
            Some("esp32"),
            Some("riscv32imc-unknown-none-elf"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        // The real checkout: beside this repo, or wherever the developer says it is. Its crate is named
        // after the project — `spire-embedded/crates/spire-embedded` here — so it is read, by the same
        // derivation the scaffold uses, rather than assumed.
        let root = std::env::var("SPIRE_EMBEDDED_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spire-embedded")
            });
        if embedded_crate(&root).is_err() {
            println!(
                "no container checkout at {} — set SPIRE_EMBEDDED_ROOT to build this",
                root.display()
            );
            return;
        }
        let path = root.to_string_lossy().to_string();
        let out = embedded_app_scaffold("Build Check", &["esp32c3".into()], &path, &root)
            .expect("an esp32c3 application");

        // Write it out the way the project writer does, then build it the way a user would.
        let work = tempfile::tempdir().unwrap();
        for file in &out.files {
            let target = work.path().join(&file.path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, &file.content).unwrap();
        }

        // The *toolchain*, not just `cargo` from `PATH`: a bare-metal target needs the `core` that
        // belongs to it, and the `cargo` a machine happens to have first may know nothing about it —
        // which fails as "can't find crate for `core`" and reads like a missing target.
        let toolchains = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".rustup/toolchains");
        let mut stable: Vec<std::path::PathBuf> = std::fs::read_dir(&toolchains)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().starts_with("stable-"))
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        stable.sort();
        let toolchain = stable.pop();

        let mut build = std::process::Command::new(
            toolchain
                .as_ref()
                .map(|t| t.join("bin/cargo"))
                .unwrap_or_else(|| std::path::PathBuf::from("cargo")),
        );
        build.args(["build", "--release"]).current_dir(work.path());
        if let Some(toolchain) = &toolchain {
            build
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        toolchain.join("bin").display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                // `rust-lld` may not find `libLLVM.dylib` through its own rpath; the file is in the
                // toolchain's `lib/`, and this is what reaches it.
                .env("DYLD_FALLBACK_LIBRARY_PATH", toolchain.join("lib"));
        }

        let output = build.output().expect("cargo runs");
        assert!(
            output.status.success(),
            "the scaffolded application must build.\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        // And it must have produced a *binary*. A green exit with no artifact would be a build that
        // did nothing — which is the kind of thing a test asserting only a status code would miss.
        let elf = work
            .path()
            .join("target/riscv32imc-unknown-none-elf/release/build-check");
        let size = std::fs::metadata(&elf)
            .map(|meta| meta.len())
            .unwrap_or_default();
        assert!(
            size > 0,
            "no ELF at {} (the scaffolded app built nothing)",
            elf.display()
        );
    }

    /// When the container has a BSP for this board, the application **names it**.
    ///
    /// The board's facts live in the BSP, so an application that repeats them in `main.rs` is an
    /// application that can drift from them. And when there is no BSP — the ordinary case, since an
    /// upstream one may cover the board — the manifest must not invent a dependency on one.
    #[test]
    fn an_app_names_the_containers_bsp_when_there_is_one() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry(&[(
            "esp32c3",
            "esp-hal",
            Some("esp32"),
            Some("riscv32imc-unknown-none-elf"),
        )]);
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        // A container with a BSP for this board, as `add_bsp` writes one.
        let with_bsp = tempfile::tempdir().unwrap();
        embedded_on_disk(with_bsp.path());
        let bsp = with_bsp.path().join("crates/spire-bsp-esp32c3");
        std::fs::create_dir_all(&bsp).unwrap();
        std::fs::write(
            bsp.join("Cargo.toml"),
            "[package]\nname = \"spire-bsp-esp32c3\"\n",
        )
        .unwrap();

        let path = with_bsp.path().to_string_lossy().to_string();
        let out =
            embedded_app_scaffold("Weather Node", &["esp32c3".into()], &path, with_bsp.path())
                .expect("an esp32c3 application");
        let manifest = out
            .files
            .iter()
            .find(|f| f.path == "Cargo.toml")
            .unwrap()
            .content
            .clone();
        assert!(
            manifest.contains(&format!(
                "spire-bsp-esp32c3 = {{ path = \"{path}/crates/spire-bsp-esp32c3\" }}"
            )),
            "the BSP is named by path:\n{manifest}"
        );
        // The vendor crate stays: the application still calls `esp_hal::init` and hands the
        // peripherals to the BSP.
        assert!(manifest.contains("esp-hal = "), "{manifest}");

        // A container without one: no such dependency, and nothing invented.
        let without = tempfile::tempdir().unwrap();
        embedded_on_disk(without.path());
        let path = without.path().to_string_lossy().to_string();
        let out = embedded_app_scaffold("Weather Node", &["esp32c3".into()], &path, without.path())
            .expect("an esp32c3 application");
        let manifest = out
            .files
            .iter()
            .find(|f| f.path == "Cargo.toml")
            .unwrap()
            .content
            .clone();
        assert!(
            !manifest.contains("spire-bsp-"),
            "no BSP in the container means no BSP in the manifest:\n{manifest}"
        );
    }
}
