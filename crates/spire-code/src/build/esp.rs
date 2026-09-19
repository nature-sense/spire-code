// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The esp-idf build *and flash* plan — what an ESP32 project's build actually *is*.
//!
//! Kept separate from the actor that runs it ([`crate::build::esp`]'s module) because this
//! is the part worth testing: turning a [`Platform`] into a command is the knowledge, and it
//! is host-testable, while the actor around it needs a toolchain and a board.
//!
//! Three things here are non-obvious and were each found by a failed build:
//!
//! 1. **The chip is not a cargo feature.** esp-idf-hal 0.47 has no per-chip features; the
//!    chip is the `MCU` environment variable that `esp-idf-sys` reads at build time. That is
//!    why a variant is a platform entry rather than a flag.
//! 2. **The std is built from source.** These targets are tier-3 with no prebuilt std, so
//!    `-Zbuild-std=std,panic_abort` is mandatory — and it needs a *nightly* cargo, which is
//!    why the esp toolchain must supply `rustc` as well (see [`esp_toolchain_bin`]).
//! 3. **`LIBCLANG_PATH` must be set** or bindgen fails inside `esp-idf-sys`, with an error
//!    that does not mention bindgen.
//!
//! The flash step is the same knowledge pointed at a board: the chip the build puts in `MCU`
//! is the chip `espflash` is told with `--chip`, and the artifact is where that build wrote it
//! — so both come from [`esp_chip`] and [`esp_artifact_path`] rather than from a second guess.

use std::path::{Path, PathBuf};

use crate::build::{BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability};
use crate::platform::Platform;
use spire_actor::Actor;
use spire_core::build_types::BuildSpec;

/// The rustup toolchain `espup` installs. A *custom* toolchain, so `rustup target add`
/// cannot add to it; it carries `rust-src` (hence `-Zbuild-std`) instead.
pub const ESP_TOOLCHAIN: &str = "esp";

/// The `os` values this module answers for — **one chip family, two flavours.**
///
/// `esp-idf` is the std flavour: `esp-idf-hal` over ESP-IDF, on a target with no prebuilt `core`, so
/// `-Zbuild-std` and the custom `esp` toolchain. `esp-hal` is the bare-metal one: a **stock** rustup
/// target, no vendor SDK, no `MCU`, no `sdkconfig` — the flavour the project scaffold emits, and the
/// one the pilot flew.
///
/// One module rather than two, because what the two share is what the routing is *about*: the same
/// chip, the same platform facts (`rust.target`, `rust.idf_target`, `rust.flash`) and the same flash
/// tool. What differs is the invocation, and that is a branch inside the plan.
pub const ESP_OSES: &[&str] = &["esp-idf", "esp-hal"];

/// The **bare-metal** flavour's `os` — the one that builds on stock toolchains.
pub const ESP_HAL_OS: &str = "esp-hal";

/// True for a platform this module answers for, either flavour.
pub fn is_esp(platform: &Platform) -> bool {
    ESP_OSES.contains(&platform.os.as_str())
}

/// True for the flavour that needs a vendor SDK and `-Zbuild-std`.
pub fn is_esp_idf(platform: &Platform) -> bool {
    platform.os != ESP_HAL_OS
}

/// One esp-idf build: the program to run, its arguments, and the environment it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspPlan {
    /// The chip — `MCU`, and the thing a user recognises.
    pub chip: String,
    /// The rustup target triple, also passed as `--target`.
    pub target: String,
    /// Arguments after the program name.
    pub args: Vec<String>,
    /// Environment additions (on top of the inherited environment).
    pub env: Vec<(String, String)>,
    /// True for the `esp-idf` flavour — the one that needs `MCU`, `LIBCLANG_PATH`, `-Zbuild-std` and
    /// the custom `esp` toolchain. The bare-metal flavour adds **no** environment at all: a stock
    /// target builds with the stock toolchain, so forcing the esp one onto it would be wrong.
    pub esp_idf: bool,
}

/// The chip a platform targets — `IDF_TARGET`, and the `MCU` environment variable — or `None` when
/// this is not an ESP platform at all.
///
/// One function because three things key off the chip and they must agree: the build (as `MCU`, for
/// the esp-idf flavour), the flash tool (as `--chip`, for either flavour — `espflash` wants the same
/// spelling the vendor uses), and the triple's directory under `target/`.
pub fn esp_chip(platform: &Platform) -> Option<String> {
    if !is_esp(platform) {
        return None;
    }
    let rust = platform.rust.as_ref()?;
    // `cpu` and `idf_target` say the same thing; prefer the explicit one, and fall back so a
    // hand-written YAML with only `cpu` still works.
    let chip = rust
        .idf_target
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(platform.architecture.cpu.as_str());
    (!chip.is_empty()).then(|| chip.to_string())
}

/// The esp-idf plan for `platform`, or `None` when this is not an esp-idf platform.
///
/// Returning `None` is what stops the ESP32 module from claiming a plain Rust project: a
/// `linux` platform with a `Cargo.toml` must go to `CargoBuildModule`, exactly as before.
pub fn esp_plan(platform: &Platform, opts: &BuildOptions) -> Option<EspPlan> {
    let rust = platform.rust.as_ref()?;
    let chip = esp_chip(platform)?;
    let esp_idf = is_esp_idf(platform);

    let mut args = vec![
        "build".to_string(),
        "--target".to_string(),
        rust.target.clone(),
    ];
    if esp_idf {
        // These targets are tier-3 with no prebuilt std: without this the build dies with
        // "can't find crate for `core`", which reads like a missing toolchain. The bare-metal
        // flavour's target is a **stock** one, so it needs nothing here — and asking for
        // `-Zbuild-std` would put a nightly-only flag on a stable build.
        args.push("-Zbuild-std=std,panic_abort".to_string());
    }
    if opts.mode.eq_ignore_ascii_case("release") {
        args.push("--release".to_string());
    }
    if let Some(package) = opts.package.as_deref().filter(|p| !p.trim().is_empty()) {
        args.push("-p".to_string());
        args.push(package.to_string());
    }

    Some(EspPlan {
        chip: chip.clone(),
        // `MCU` is a build-time fact only for esp-idf: `esp-idf-sys` reads it, and the crates have no
        // per-chip cargo feature. In the bare-metal flavour the chip *is* a cargo feature, named in
        // the project's own manifest, so there is nothing to pass.
        env: if esp_idf {
            vec![("MCU".to_string(), chip)]
        } else {
            Vec::new()
        },
        esp_idf,
        target: rust.target.clone(),
        args,
    })
}

/// The host-side flash tool a platform declares (`rust.flash`), or `None`.
///
/// Its own function because two callers ask the same question for different reasons: the
/// command builder needs the tool to run, and `run_esp_flash` needs it to *refuse* a platform
/// that has no flash step before it looks for an artifact.
pub fn esp_flash_tool(platform: &Platform) -> Option<&str> {
    platform
        .rust
        .as_ref()?
        .flash
        .as_deref()
        .filter(|t| !t.trim().is_empty())
}

/// The host-side flash command for `artifact` (`espflash`, from `rust.flash`), or `None`.
///
/// Deliberately a **host** command, not a device-MCP one: a board that has never been
/// flashed has nothing running to talk to, so the network leg cannot bootstrap itself.
///
/// `--non-interactive` is not decoration. espflash's fallback for a device it cannot
/// auto-detect is a **prompt**, and a `tools/call` has no terminal: measured on the attached
/// ESP32, the un-flagged command dies with `espflash::dialoguer_error / IO error: not a
/// terminal` — a failure that names neither the board nor the port.
///
/// `bootloader` and `partition_table` are the ones **this build** produced ([`esp_bootloader_path`],
/// [`esp_partition_table_path`]). Without them espflash writes the app and nothing else, leaving the
/// board's existing bootloader and table in place — which is how a `v5.5.5` app came to run under a
/// `v6.1` bootloader, inside a partition from a firmware nobody could name. `None` keeps the
/// app-only behaviour, which is still right for an esp artifact that is not esp-idf-sys's.
pub fn esp_flash_command(
    platform: &Platform,
    artifact: &Path,
    bootloader: Option<&Path>,
    partition_table: Option<&Path>,
    port: Option<&Path>,
) -> Option<Vec<String>> {
    let tool = esp_flash_tool(platform)?;
    let chip = esp_chip(platform)?;
    let mut command = vec![
        tool.to_string(),
        "flash".to_string(),
        "--chip".to_string(),
        chip,
        "--non-interactive".to_string(),
    ];
    if let Some(port) = port {
        command.push("--port".to_string());
        command.push(port.to_string_lossy().to_string());
    }
    if let Some(bootloader) = bootloader {
        command.push("--bootloader".to_string());
        command.push(bootloader.to_string_lossy().to_string());
    }
    if let Some(partition_table) = partition_table {
        command.push("--partition-table".to_string());
        command.push(partition_table.to_string_lossy().to_string());
    }
    command.push(artifact.to_string_lossy().to_string());
    Some(command)
}

/// Filename fragments that identify a **USB-serial adapter** on macOS and Linux.
///
/// espflash matches specific Espressif VID/PIDs and does not know a plain adapter (CP210x,
/// CH34x, FTDI). Measured on the attached ESP32: `espflash list-ports` prints *No known serial
/// ports found* while `--port /dev/cu.usbserial-…` connects and reads the chip. So the port is
/// found here, where it can be explained, instead of left to a prompt.
const USB_SERIAL_MARKERS: &[&str] = &["usbserial", "usbmodem", "wchusbserial", "SLAB_USBtoUART"];

/// The **single** USB-serial device in `dir`, or `None` when there is none or more than one.
///
/// A directory parameter so this is provable on a temp dir — the same reason `libclang_path_in`
/// takes one. Ambiguity returns `None` rather than the first match: two adapters means two
/// boards, and picking one would flash the wrong silicon.
pub fn serial_port_in(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name,
                None => return false,
            };
            let is_device =
                name.starts_with("cu.") || name.starts_with("ttyUSB") || name.starts_with("ttyACM");
            is_device
                && USB_SERIAL_MARKERS
                    .iter()
                    .any(|marker| name.contains(marker))
        })
        .collect();
    found.sort();
    match found.len() {
        1 => found.pop(),
        _ => None,
    }
}

/// The attached board's USB-serial port, or `None` when that is not unambiguous.
pub fn esp_serial_port() -> Option<PathBuf> {
    serial_port_in(Path::new("/dev"))
}

/// The binary a `cargo build` for `platform` writes, or `None` when its name cannot be known.
///
/// `target/<triple>/<profile>/<name>`: the triple is a **subdirectory** because `--target` was
/// passed (a host build puts it straight in `target/<profile>/`), and the profile is `release`
/// only for a release build — flashing a debug artifact from the release path, or the reverse,
/// is a wrong-binary flash that no tool would catch.
///
/// `explicit` wins when given (a `[[bin]]` name, or an artifact some other step produced);
/// otherwise the name is `package`, falling back to the `[package] name` in `Cargo.toml`.
pub fn esp_artifact_path(
    root: &Path,
    platform: &Platform,
    mode: &str,
    package: Option<&str>,
    explicit: Option<&Path>,
) -> Option<PathBuf> {
    // The derivation itself is shared with the other platform modules; what is esp-specific is
    // where the triple comes from (this platform's `rust.target`).
    let rust = platform.rust.as_ref()?;
    crate::build::generic_helpers::cargo_artifact_path(root, &rust.target, mode, package, explicit)
}

/// A file `esp-idf-sys` left under a profile dir's `build/esp-idf-sys-*/out/build/`, or `None`.
///
/// The build directory is **globbed**, not named, because its hash is esp-idf-sys's own — and
/// several can accumulate (a changed `sdkconfig.defaults` re-configures into a new one while the
/// old stays behind). The **newest** is the one this build used; an older one still describes the
/// previous partition table, which is the whole hazard this exists to avoid.
///
/// A directory parameter so the glob is provable on a temp dir, the same reason `serial_port_in`
/// and `libclang_path_in` take one.
fn esp_idf_build_file_in(profile_dir: &Path, relative: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(profile_dir.join("build")).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("esp-idf-sys-"))
        })
        .map(|entry| entry.path().join("out").join("build").join(relative))
        .filter(|file| file.is_file())
        .collect();
    candidates.sort_by_key(|file| std::fs::metadata(file).and_then(|m| m.modified()).ok());
    candidates.pop()
}

/// The bootloader **this build** produced, or `None` when the project is not an esp-idf one.
///
/// `espflash flash <elf>` deliberately writes only the app, so a board flashed that way keeps
/// whatever bootloader it already had — measured: a `v6.1-beta1` bootloader under a `v5.5.5` app,
/// reported by neither the tool nor the build. Carrying the one the build made is what makes
/// "flash this project" mean what it says.
pub fn esp_bootloader_path(artifact: &Path) -> Option<PathBuf> {
    esp_idf_build_file_in(artifact.parent()?, "bootloader/bootloader.bin")
}

/// The partition table **this build** produced, or `None`.
///
/// Flashed with the app because the two must agree: the app's size has to fit the partition it is
/// written to, and a board's *existing* table is whatever some earlier firmware left there. Passing
/// the build's own table turns "the image does not fit" into a failure at flash time instead of a
/// silent write into someone else's layout.
pub fn esp_partition_table_path(artifact: &Path) -> Option<PathBuf> {
    esp_idf_build_file_in(artifact.parent()?, "partition_table/partition-table.bin")
}

/// The esp toolchain's `bin` — the directory that must come first on the child build's `PATH`.
///
/// Resolved in the order a user would expect, each step a *reference* to an install this machine
/// already has rather than anything Spire creates:
///
/// 1. **`ESP_TOOLCHAIN_BIN`** — an explicit answer, for a toolchain installed somewhere else (a
///    second rustup home, a CI image, a vendored toolchain). The same courtesy
///    `ESP_IDF_TOOLS_INSTALL_DIR` already extends to the SDK.
/// 2. **`$RUSTUP_HOME/toolchains/esp/bin`** — the standard variable for a relocated rustup install.
/// 3. **`$HOME/.rustup/toolchains/esp/bin`** — the default rustup home, which is where `espup` puts
///    it on a normal machine.
///
/// A path that is set but does not exist is **not** fatal: the next step is tried, and if nothing is
/// found the build fails with the toolchain's own error. Deliberate — an explicit typo losing a
/// discovery that would have worked is a worse failure than a missing directory.
///
/// Both `cargo` *and* `rustc` must come from here: leaving cargo to find its own `rustc` silently
/// picks the stable one, and `-Zbuild-std` then fails with "the option `Z` is only accepted on the
/// nightly compiler" — an error about a flag, never about the toolchain that should have accepted it.
pub fn esp_toolchain_bin() -> Option<PathBuf> {
    esp_toolchain_bin_in(
        non_empty_env("ESP_TOOLCHAIN_BIN").as_deref(),
        non_empty_env("RUSTUP_HOME").as_deref(),
        non_empty_env("HOME").as_deref(),
    )
}

/// [`esp_toolchain_bin`] with the environment passed in, so the order is testable without espup
/// installed (the same reason `libclang_path_in` takes a directory).
pub fn esp_toolchain_bin_in(
    explicit: Option<&str>,
    rustup_home: Option<&str>,
    home: Option<&str>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        let bin = PathBuf::from(explicit);
        if bin.is_dir() {
            return Some(bin);
        }
    }
    let toolchains = match rustup_home {
        Some(root) => PathBuf::from(root).join("toolchains"),
        None => PathBuf::from(home?).join(".rustup").join("toolchains"),
    };
    let bin = toolchains.join(ESP_TOOLCHAIN).join("bin");
    bin.is_dir().then_some(bin)
}

/// An environment value that is set and not blank, else `None`.
///
/// Blank counts as unset: `ESP_TOOLCHAIN_BIN=""` in a shell profile would otherwise resolve to the
/// current directory — the kind of deliberate-looking nonsense this module refuses elsewhere.
pub(crate) fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The `LIBCLANG_PATH` bindgen needs, discovered under the esp toolchain.
///
/// `espup` writes this into `~/export-esp.sh` with a *versioned* directory name
/// (`xtensa-esp32-elf-clang/esp-20.1.1_20250829/esp-clang/lib`), so globbing is the way to
/// find it without pinning a version that will change.
pub fn libclang_path() -> Option<PathBuf> {
    libclang_path_in(esp_toolchain_bin()?.parent()?)
}

/// [`libclang_path`] against an explicit toolchain directory (see [`esp_toolchain_bin_in`]).
pub fn libclang_path_in(toolchain_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(toolchain_dir.join("xtensa-esp32-elf-clang")).ok()?;
    for entry in entries.flatten() {
        let lib = entry.path().join("esp-clang").join("lib");
        if lib.is_dir() {
            return Some(lib);
        }
    }
    None
}

/// The value esp-idf-sys's `esp_idf_tools_install_dir` must be given, so every project on the
/// machine shares one ESP-IDF install instead of installing its own.
///
/// **One installation per machine, not one per project.** With the setting unset, esp-idf-sys
/// defaults to `workspace` — `<workspace>/.embuild/espressif`, a complete copy of ESP-IDF, its
/// build tools, a Python env and a download cache *per workspace*: measured on this machine,
/// ~5.3 GB for the `spire-hal` workspace and ~6.6 GB again for the detached `blink-esp32`
/// example. `global` is esp-idf-sys's own keyword for the standard `~/.espressif`, and that
/// directory is keyed by tool and version, so an Xtensa `esp32` build and a RISC-V `esp32c6`
/// build share it happily.
///
/// The environment wins and is forwarded **verbatim**, because this value is not a path: it is
/// one of `global` / `workspace` / `out` / `fromenv` / `custom:<dir>`, and esp-idf-sys parses it
/// by splitting on `:` and matching the first part. Handing it a bare path — the obvious first
/// attempt, and what an earlier version of this function did — fails with
/// `Matching variant not found`, an error that names neither the variable nor the reason. So a
/// machine points it at a shared cache, a CI layer or an air-gapped mirror with `custom:<dir>`,
/// or takes back the per-project behaviour with `workspace`, and no code changes.
pub fn esp_idf_tools_install_dir() -> String {
    esp_idf_tools_install_dir_in(std::env::var("ESP_IDF_TOOLS_INSTALL_DIR").ok().as_deref())
}

/// The setting used when the environment names nothing: esp-idf-sys's keyword for the shared
/// `~/.espressif`. Its own default is `workspace`, which is one install per project — see above.
pub const DEFAULT_ESP_IDF_TOOLS_INSTALL_DIR: &str = "global";

/// [`esp_idf_tools_install_dir`] with the environment passed in (see [`esp_toolchain_bin_in`]).
pub fn esp_idf_tools_install_dir_in(explicit: Option<&str>) -> String {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ESP_IDF_TOOLS_INSTALL_DIR)
        .to_string()
}

// ─────────────────────────────────────────────────────────────────────────────────────
// The module
// ─────────────────────────────────────────────────────────────────────────────────────

/// The ESP32 build module.
///
/// Registered with `AddPlatformModule { os: "esp-idf" }` and **never** with a config file:
/// an esp-idf project is *also* a `Cargo.toml` project, so claiming that file would replace
/// the cargo module for every Rust project in Spire (see `AddPlatformModule`).
///
/// Analysis is deliberately not its job either — a `Cargo.toml` analyses the same way for
/// either target, so the cargo module keeps that, and only the *invocation* differs here.
#[derive(Debug, Clone, Copy)]
pub struct EspBuildModule {
    /// The flavour this instance answers for — the `os` it was registered under. Carried so the
    /// capability names what it *is*: two registrations of the same module, one per flavour, must not
    /// both introduce themselves as "esp-idf".
    os: &'static str,
}

impl Default for EspBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

impl EspBuildModule {
    /// The **std** flavour — what every existing caller means by "the esp module".
    pub fn new() -> Self {
        Self { os: "esp-idf" }
    }

    /// The module as it answers for one flavour (see [`ESP_OSES`]).
    pub fn for_os(os: &'static str) -> Self {
        Self { os }
    }
}

#[async_trait::async_trait]
impl Actor for EspBuildModule {
    type Message = BuildModuleMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: self.os.to_string(),
                    // Empty on purpose: routing is by platform, not by config file.
                    config_files: Vec::new(),
                    build_system: format!("Cargo ({})", self.os),
                    language: "Rust".to_string(),
                    source_extensions: vec!["rs".to_string()],
                    mcp_servers: Vec::new(),
                    // The one operation this module *is* for: the artifact exists, the chip is
                    // a platform fact, and the tool (`espflash`) is a host binary.
                    supports_flash: true,
                    // Declared false so the manager refuses these *before* routing to us: a
                    // clean or lint here would run against the wrong target, which is worse
                    // than refusing.
                    supports_clean: false,
                    supports_lint: false,
                    supports_format: false,
                    supports_fix: false,
                });
            }

            BuildModuleMessage::Flash {
                path,
                opts,
                artifact,
                port,
                reply_to,
                ..
            } => {
                let _ = reply_to
                    .send(run_esp_flash(&path, &opts, artifact.as_deref(), port.as_deref()).await);
            }

            BuildModuleMessage::Build {
                path,
                opts,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(run_esp_build(&path, &opts).await);
            }

            BuildModuleMessage::BuildStreaming {
                path,
                opts,
                event_tx,
                reply_to,
                ..
            } => {
                let result = run_esp_build(&path, &opts).await;
                // One synthetic finished line, mirroring how the other modules fall back when
                // they cannot stream per-line. Worth knowing: the first build for a chip
                // downloads and compiles ESP-IDF itself, so this can take minutes.
                let _ = event_tx.send(crate::build::BuildEvent {
                    line: format!(
                        "Finished {} in {:?}s",
                        path.display(),
                        result.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)
                    ),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
                let _ = reply_to.send(result);
            }

            _ => {
                // Everything else (test/lint/clean/fix/parse/analyze/scaffold) either has its
                // capability declared false above — so the manager refuses it before routing —
                // or belongs to the cargo module. Warn rather than reply, so a future routing
                // change surfaces here instead of being silently swallowed.
                tracing::warn!(
                    "EspBuildModule: a message it does not implement was routed here; \
                     the build manager should have refused or routed it elsewhere"
                );
            }
        }
    }
}

/// The platform and plan a build *or* a flash both need, or the refusal naming what is missing.
///
/// One function so the two paths cannot drift in what they accept — while the message names the
/// operation, because "an ESP build needs a platform" is a confusing thing to read when you asked for
/// a flash. `esp_plan` is what decides "is this an ESP platform": a platform with no `rust:` block can
/// neither be built nor flashed, so it is refused here rather than producing a command with an empty
/// triple.
fn esp_platform_plan(op: &str, opts: &BuildOptions) -> Result<(Platform, EspPlan), String> {
    let platform_id = opts
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| format!("an ESP {op} needs a platform, e.g. \"esp32c3\""))?;
    let platform = Platform::from_registry(platform_id)
        .ok_or_else(|| format!("unknown platform '{platform_id}'"))?;
    let plan = esp_plan(&platform, opts).ok_or_else(|| {
        format!(
            "platform '{platform_id}' is not an ESP platform (os '{}'): this module builds esp-idf \
             and esp-hal projects",
            platform.os
        )
    })?;
    Ok((platform, plan))
}

/// Run an esp-idf build for `opts.platform`, through the **shared** process runner.
///
/// Reusing `run_build_spec` rather than spawning a `Command` here means environment
/// handling, duration measurement and exit-code reporting behave exactly as they do for
/// every other build module.
pub async fn run_esp_build(path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
    let (_, plan) = esp_platform_plan("build", opts)?;
    let spec = if plan.esp_idf {
        spec_from_plan(plan)
    } else {
        // The bare-metal flavour: a stock target, so there is no SDK and no custom toolchain to force
        // — but the toolchain about to run must have that target's `core`, and on a machine whose
        // `cargo` is not rustup's it does not (see `rp2040::spec_from_target`, which measured it).
        // Checked here rather than left to cargo, whose message names the triple and then suggests a
        // fix that is already applied.
        bare_metal_spec(
            plan,
            crate::build::rp2040::rustc_sysroot(path).as_deref(),
            crate::build::rp2040::rustup_installed_targets(path).as_deref(),
        )?
    };
    crate::build::generic_helpers::run_build_spec(path, &spec).await
}

/// The [`BuildSpec`] the **bare-metal** flavour becomes, with the one requirement it has.
///
/// Nothing here needs the esp toolchain, `LIBCLANG_PATH` or an IDF install directory: a stock target
/// builds with the stock toolchain, and injecting that environment would be this module deciding
/// something the platform never said. What it does carry is the target check and the host-side linker
/// fix that comes with it — see `rp2040::spec_for_target`, which is where both live.
pub(crate) fn bare_metal_spec(
    plan: EspPlan,
    sysroot: Option<&Path>,
    rustup_targets: Option<&str>,
) -> Result<BuildSpec, String> {
    crate::build::rp2040::spec_for_target(
        plan.args,
        plan.env,
        &plan.target,
        sysroot,
        rustup_targets,
    )
}

/// Flash the artifact for `opts.platform` onto the board, over USB.
///
/// Every failure mode is a *refusal*, never an attempt: no platform, an unknown one, one that
/// is not esp-idf, one that declares no flash tool, an artifact that cannot be identified, an
/// artifact that is not there, and a board with no identifiable serial port. Each would
/// otherwise flash the wrong binary — or the right binary onto the wrong chip. All of them
/// happen before the tool runs; the invocation itself is [`esp_flash_command`]'s.
///
/// The port resolves explicit → `$ESPFLASH_PORT` → the single USB-serial device → refusal. It is
/// named rather than left to espflash because espflash cannot see this adapter at all (see
/// [`USB_SERIAL_MARKERS`]), and its fallback would be a prompt with no terminal to answer it.
///
/// The bootloader and partition table travel with the artifact when the build produced them
/// ([`esp_bootloader_path`], [`esp_partition_table_path`]) — a board's existing ones belong to
/// whatever firmware was flashed before, and those are not the ones this app was built against.
pub async fn run_esp_flash(
    path: &Path,
    opts: &BuildOptions,
    artifact: Option<&Path>,
    port: Option<&Path>,
) -> Result<BuildOutput, String> {
    let (platform, _) = esp_platform_plan("flash", opts)?;

    if esp_flash_tool(&platform).is_none() {
        return Err(format!(
            "platform '{}' declares no flash tool (rust.flash), so nothing can flash it",
            platform.id
        ));
    }

    let artifact = esp_artifact_path(
        path,
        &platform,
        &opts.mode,
        opts.package.as_deref(),
        artifact,
    )
    .ok_or_else(|| {
        "cannot tell which binary to flash: Cargo.toml has no [package] name and no package \
             was given — pass artifact=<path> or package=<name>"
            .to_string()
    })?;
    if !artifact.is_file() {
        return Err(format!(
            "no artifact at {}; build for '{}' first, or pass artifact=<path> / mode=<profile>",
            artifact.display(),
            platform.id
        ));
    }

    // Explicit, then a convention, then discovery — and otherwise refuse. A refusal is right
    // here because espflash's own fallback is a prompt, which a `tools/call` cannot answer.
    let port = port
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var("ESPFLASH_PORT")
                .ok()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
        })
        .or_else(esp_serial_port)
        .ok_or_else(|| {
            "no serial port to flash over: pass port=<dev>, or set $ESPFLASH_PORT — a single \
             USB serial device was not found (espflash does not recognise plain adapters, so it \
             cannot pick one itself)"
                .to_string()
        })?;

    let command = esp_flash_command(
        &platform,
        &artifact,
        esp_bootloader_path(&artifact).as_deref(),
        esp_partition_table_path(&artifact).as_deref(),
        Some(&port),
    )
    .ok_or_else(|| format!("platform '{}' has no flash command", platform.id))?;
    crate::build::generic_helpers::run_build_spec(path, &spec_from_command(command)).await
}

/// The [`BuildSpec`] an [`EspPlan`] becomes, with the environment requirements added.
///
/// Each is easy to miss and no failure names its real cause, which is why they live here rather
/// than in a shell script someone has to remember to source. The last one is not a correctness
/// requirement but a disk one: without it every project installs its own copy of ESP-IDF.
pub(crate) fn spec_from_plan(plan: EspPlan) -> BuildSpec {
    spec_from_parts(
        plan,
        esp_toolchain_bin().as_deref(),
        libclang_path().as_deref(),
        &esp_idf_tools_install_dir(),
        non_empty_env("LIBCLANG_PATH").as_deref(),
        std::env::var("PATH").ok().as_deref(),
    )
}

/// The conversion itself, with the environment lookups passed in.
///
/// Split out so it can be tested deterministically: `esp_toolchain_bin`, `libclang_path` and
/// `esp_idf_tools_install_dir` read `$HOME`, so a test calling them would pass or fail on whether
/// *this* machine happens to have espup — failing for a reason unrelated to the logic under test.
pub(crate) fn spec_from_parts(
    plan: EspPlan,
    toolchain_bin: Option<&Path>,
    libclang: Option<&Path>,
    idf_tools_dir: &str,
    inherited_libclang: Option<&str>,
    inherited_path: Option<&str>,
) -> BuildSpec {
    let mut env = plan.env;

    // The esp toolchain's `cargo` **and** `rustc` must both win, and the bin must come FIRST:
    // letting cargo find its own rustc silently picks the stable one, and `-Zbuild-std` then
    // fails with an error about a *flag* rather than about the toolchain that should have
    // accepted it.
    if let Some(bin) = toolchain_bin {
        let mut path = bin.to_string_lossy().to_string();
        if let Some(existing) = inherited_path {
            path.push(':');
            path.push_str(existing);
        }
        env.push(("PATH".to_string(), path));
    }

    // Without `LIBCLANG_PATH`, bindgen inside esp-idf-sys fails with an error that never mentions
    // bindgen. **An inherited value wins**: it is the canonical name for this setting, so a user who
    // exported it did so about their own clang, and overwriting their choice with the one we found
    // under the esp toolchain would be exactly the "Spire knows better" this module avoids
    // everywhere else. Discovery is the fallback, not the authority.
    match (inherited_libclang, libclang) {
        (Some(explicit), _) => env.push(("LIBCLANG_PATH".to_string(), explicit.to_string())),
        (None, Some(found)) => env.push((
            "LIBCLANG_PATH".to_string(),
            found.to_string_lossy().to_string(),
        )),
        (None, None) => {}
    }

    // Where esp-idf-sys installs ESP-IDF and its tools. Left to itself it uses `workspace` —
    // `<workspace>/.embuild/espressif` — so every project on the machine pays for its own copy
    // (measured: ~5.3 GB and ~6.6 GB for two projects here). This is always set, never optional:
    // the value is a keyword, not a discovered path, so there is nothing to fail to resolve.
    env.push((
        "ESP_IDF_TOOLS_INSTALL_DIR".to_string(),
        idf_tools_dir.to_string(),
    ));

    BuildSpec {
        command: "cargo".to_string(),
        arguments: plan.args,
        working_dir: String::new(),
        env,
    }
}

/// A host `[program, args…]` as a [`BuildSpec`] — the flash step's counterpart to
/// [`spec_from_plan`].
///
/// **No environment**, deliberately: `espflash` is a host binary (cargo-installed), so unlike
/// the build it needs neither the esp toolchain on `PATH` nor `LIBCLANG_PATH`. Setting them
/// here would be cargo-cult, and a `PATH` that hides the user's own tools is a real failure
/// mode, not a theoretical one.
pub(crate) fn spec_from_command(command: Vec<String>) -> BuildSpec {
    // Delegates: the conversion is shared with the other platform modules, and this name is kept
    // because esp's tests and flash path ask for it in esp's terms.
    crate::build::generic_helpers::build_spec_from_command(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{PlatformArchitecture, PlatformRust, PlatformSysroot, PlatformToolchain};

    fn platform(
        id: &str,
        os: &str,
        cpu: &str,
        triple: &str,
        rust: Option<(&str, &str)>,
    ) -> Platform {
        Platform {
            id: id.into(),
            name: id.into(),
            os: os.into(),
            architecture: PlatformArchitecture {
                cpu_family: "riscv".into(),
                cpu: cpu.into(),
                endian: "little".into(),
                target_triple: triple.into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            device: None,
            family: Some("esp32".into()),
            rust: rust.map(|(target, idf)| PlatformRust {
                target: target.into(),
                idf_target: Some(idf.into()),
                flash: Some("espflash".into()),
            }),
            library_hints: None,
        }
    }

    fn c6() -> Platform {
        platform(
            "esp32c6",
            "esp-idf",
            "esp32c6",
            "riscv32imac-esp-espidf",
            Some(("riscv32imac-esp-espidf", "esp32c6")),
        )
    }

    /// The pilot's board, in the **bare-metal** flavour: a stock rustup target, espflash as the tool.
    fn c3() -> Platform {
        platform(
            "esp32c3",
            ESP_HAL_OS,
            "esp32c3",
            "riscv32imc-unknown-none-elf",
            Some(("riscv32imc-unknown-none-elf", "esp32c3")),
        )
    }

    /// A plain Rust project on a normal platform must NOT be claimed by the ESP32 path: it
    /// belongs to `CargoBuildModule`, exactly as before. This is the test that keeps the new
    /// module from hijacking every existing cargo project.
    #[test]
    fn a_non_esp_platform_has_no_esp_plan() {
        let linux = platform("rpi5", "linux", "armv8-a", "aarch64-linux-gnu", None);
        assert!(esp_plan(&linux, &BuildOptions::default()).is_none());
    }

    /// An `os: esp-idf` platform with no `rust` block is malformed, not ESP32 — the toolchain
    /// is what makes it buildable.
    #[test]
    fn an_esp_platform_without_a_rust_toolchain_is_not_esp() {
        let broken = platform(
            "esp32c6",
            "esp-idf",
            "esp32c6",
            "riscv32imac-esp-espidf",
            None,
        );
        assert!(esp_plan(&broken, &BuildOptions::default()).is_none());
    }

    /// The chip is `MCU` — an environment variable, **not** a cargo feature. And the std is
    /// built from source, because these targets have no prebuilt one.
    #[test]
    fn esp32c6_gets_the_chip_the_triple_and_build_std() {
        let plan = esp_plan(&c6(), &BuildOptions::default()).expect("an esp-idf platform plans");

        assert_eq!(plan.chip, "esp32c6");
        assert_eq!(plan.env, vec![("MCU".to_string(), "esp32c6".to_string())]);
        assert_eq!(plan.target, "riscv32imac-esp-espidf");
        assert!(
            plan.args.contains(&"--target".to_string()),
            "{:?}",
            plan.args
        );
        assert!(
            plan.args
                .contains(&"-Zbuild-std=std,panic_abort".to_string()),
            "without build-std these targets fail with `can't find crate for core`: {:?}",
            plan.args
        );
    }

    /// The **bare-metal flavour** plans the same chip and triple and none of what an esp-idf build
    /// needs: no `MCU` (the chip *is* a cargo feature, named in the project's own manifest), and no
    /// `-Zbuild-std` — its target is a **stock** one, so the flag would be a nightly-only request on a
    /// stable build.
    #[test]
    fn esp_hal_plans_a_stock_target_with_no_idf_environment() {
        let plan = esp_plan(&c3(), &BuildOptions::default()).expect("an esp-hal platform plans");

        assert_eq!(
            plan.chip, "esp32c3",
            "espflash --chip wants the vendor spelling"
        );
        assert_eq!(plan.target, "riscv32imc-unknown-none-elf");
        assert!(!plan.esp_idf);
        assert!(plan.env.is_empty(), "nothing to pass: {:?}", plan.env);
        assert_eq!(
            plan.args,
            vec!["build", "--target", "riscv32imc-unknown-none-elf"]
        );
    }

    /// Its [`BuildSpec`] carries **no** environment: the esp toolchain, `LIBCLANG_PATH` and the shared
    /// IDF directory belong to the other flavour, and injecting them here would be this module deciding
    /// something the platform never said.
    #[test]
    fn the_bare_metal_spec_adds_no_environment() {
        let plan = esp_plan(&c3(), &BuildOptions::default()).expect("a plan");
        let spec =
            bare_metal_spec(plan, None, None).expect("no sysroot to check, so nothing is refused");

        assert_eq!(spec.command, "cargo");
        assert!(spec.env.is_empty(), "{:?}", spec.env);
        assert_eq!(
            spec.arguments,
            vec!["build", "--target", "riscv32imc-unknown-none-elf"]
        );
    }

    /// The sysroot check applies to this flavour too, and it is the one that matters on a machine whose
    /// `cargo` is not rustup's: the target is installed, and the toolchain about to run cannot see it.
    #[test]
    fn the_bare_metal_spec_refuses_a_target_the_running_toolchain_lacks() {
        let plan = esp_plan(&c3(), &BuildOptions::default()).expect("a plan");
        let empty = std::env::temp_dir().join("spire-esp-hal-no-such-sysroot");
        std::fs::create_dir_all(&empty).unwrap();

        let err = bare_metal_spec(plan, Some(&empty), Some("riscv32imc-unknown-none-elf\n"))
            .expect_err("that directory has no such target");
        assert!(
            err.contains("not for the toolchain this build will run"),
            "{err}"
        );
        assert!(
            err.contains("rustup which cargo"),
            "the refusal must say what to do: {err}"
        );
    }

    /// Two instances of this module are registered — one per flavour — so the capability has to name
    /// the flavour it answers for. Both introducing themselves as "esp-idf" would be a UI that cannot
    /// tell them apart, and a route the user cannot reason about.
    #[tokio::test]
    async fn the_capability_names_the_flavour() {
        let mut idf = EspBuildModule::new();
        let mut hal = EspBuildModule::for_os(ESP_HAL_OS);

        let (t, r) = tokio::sync::oneshot::channel();
        idf.handle(BuildModuleMessage::DescribeCapabilities { reply_to: t })
            .await;
        assert_eq!(r.await.unwrap().name, "esp-idf");

        let (t, r) = tokio::sync::oneshot::channel();
        hal.handle(BuildModuleMessage::DescribeCapabilities { reply_to: t })
            .await;
        let cap = r.await.unwrap();
        assert_eq!(cap.name, ESP_HAL_OS);
        assert_eq!(cap.build_system, "Cargo (esp-hal)");
        assert!(cap.supports_flash, "either flavour flashes with espflash");
        assert!(
            cap.config_files.is_empty(),
            "routing is by platform, never by file"
        );
    }

    /// The live test: **this module's own build path**, against a real scaffolded application.
    ///
    /// The unit tests pin the plan and the spec; this drives [`run_esp_build`] itself — the routing
    /// decision, the plan, the sysroot preflight and the shared process runner — which is the part no
    /// unit test reaches.
    ///
    /// It needs a `spire-embedded` checkout (`$SPIRE_EMBEDDED_ROOT`, else the sibling) and, on a machine
    /// whose `cargo` is not rustup's, a `PATH` with rustup's toolchain first. That is precisely what the
    /// preflight refuses without, so running it by hand is the point:
    ///
    /// ```sh
    /// PATH="$HOME/.rustup/toolchains/stable-$(uname -m | sed s/x86_64/x86_64/)/bin:$PATH" \
    ///   SPIRE_EMBEDDED_ROOT=../spire-embedded \
    ///   cargo test -p spire-code --lib the_module_builds_a_scaffolded_app -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "live build: needs a container checkout, a Rust target and a network"]
    async fn the_module_builds_a_scaffolded_app() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("platforms");
        std::fs::create_dir_all(&reg).unwrap();
        std::fs::write(
            reg.join("esp32c3.yaml"),
            "id: esp32c3\nname: ESP32-C3\nos: esp-hal\narchitecture:\n  cpu_family: riscv\n  \
             cpu: esp32c3\n  endian: little\n  target_triple: riscv32imc-unknown-none-elf\n\
             rust:\n  target: riscv32imc-unknown-none-elf\n  idf_target: esp32c3\n  flash: espflash\n",
        )
        .unwrap();
        let _env = crate::platform::PlatformDirGuard::set(&reg);

        // The container by its *workspace*, not by a crate name: which crate is inside is the
        // scaffold's business, and it derives it.
        let source = std::env::var("SPIRE_EMBEDDED_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spire-embedded")
            });
        if !source.join("Cargo.toml").is_file() {
            println!(
                "no container checkout at {} — set SPIRE_EMBEDDED_ROOT to build this",
                source.display()
            );
            return;
        }

        let app = crate::build::embedded_app_scaffold::embedded_app_scaffold(
            "module-build-check",
            &["esp32c3".to_string()],
            &source.to_string_lossy(),
            &source,
        )
        .expect("an esp32c3 application");

        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("module-build-check");
        for file in &app.files {
            let target = root.join(&file.path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, &file.content).unwrap();
        }

        let opts = BuildOptions {
            platform: Some("esp32c3".to_string()),
            ..Default::default()
        };
        let output = run_esp_build(&root, &opts)
            .await
            .expect("the module must build a scaffolded application");
        assert!(output.success, "{}", output.output);
        assert!(
            output.output.contains("Compiling") || output.output.contains("Finished"),
            "the build must have run: {}",
            output.output
        );
    }

    /// The P4 is a different target in the same family — `imafc`, not `imac`. Getting that
    /// wrong would build for the wrong CPU rather than fail, which is why it is pinned.
    #[test]
    fn esp32p4_uses_its_own_triple() {
        let p4 = platform(
            "esp32p4",
            "esp-idf",
            "esp32p4",
            "riscv32imafc-esp-espidf",
            Some(("riscv32imafc-esp-espidf", "esp32p4")),
        );
        let plan = esp_plan(&p4, &BuildOptions::default()).expect("an esp-idf platform plans");
        assert_eq!(plan.chip, "esp32p4");
        assert_eq!(plan.target, "riscv32imafc-esp-espidf");
    }

    #[test]
    fn release_and_package_become_flags() {
        let opts = BuildOptions {
            mode: "release".into(),
            package: Some("firmware".into()),
            ..Default::default()
        };
        let plan = esp_plan(&c6(), &opts).expect("plan");
        assert!(
            plan.args.contains(&"--release".to_string()),
            "{:?}",
            plan.args
        );
        // `-p` and its value, in that order — a package flag with no value would be worse
        // than absent.
        let at = plan.args.iter().position(|a| a == "-p").expect("-p");
        assert_eq!(plan.args.get(at + 1).map(String::as_str), Some("firmware"));
    }

    /// Flashing is a **host** command: a board that has never been flashed has nothing
    /// running to talk to, so the network MCP leg cannot be how the first flash happens.
    ///
    /// The port and `--non-interactive` are pinned because both were learned from the board:
    /// espflash does not recognise this adapter (`list-ports` finds none) and without a port it
    /// **prompts** — which a `tools/call` cannot answer, so it fails as `IO error: not a
    /// terminal`. Naming the port is what makes the flash unattended.
    #[test]
    fn flash_is_a_host_command_naming_the_chip_and_the_port() {
        let cmd = esp_flash_command(
            &c6(),
            Path::new("target/riscv32imac-esp-espidf/release/fw"),
            None,
            None,
            Some(Path::new("/dev/cu.usbserial-569C0028661")),
        )
        .expect("an esp platform with `flash` set flashes");
        assert_eq!(cmd[0], "espflash");
        assert_eq!(cmd[1], "flash");
        // espflash cannot always infer the chip, so it is named explicitly.
        assert_eq!(cmd[2], "--chip");
        assert_eq!(cmd[3], "esp32c6");
        assert!(
            cmd.contains(&"--non-interactive".to_string()),
            "without this a prompt with no terminal is the failure mode: {cmd:?}"
        );
        let at = cmd
            .iter()
            .position(|a| a == "--port")
            .expect("the port is named");
        assert_eq!(
            cmd.get(at + 1).map(String::as_str),
            Some("/dev/cu.usbserial-569C0028661")
        );
        assert!(
            cmd.last().expect("artifact").ends_with("release/fw"),
            "{cmd:?}"
        );
        // Nothing was produced by a build here, so nothing is invented to flash with it: the
        // app-only invocation is the fallback, not the shipping path.
        assert!(!cmd.contains(&"--bootloader".to_string()), "{cmd:?}");
        assert!(!cmd.contains(&"--partition-table".to_string()), "{cmd:?}");
    }

    /// The build's own bootloader and partition table are flashed **with** the app, because
    /// espflash otherwise writes only the app and leaves the board's existing ones — measured: a
    /// `v6.1-beta1` bootloader left under a `v5.5.5` app, inside a partition from an earlier
    /// firmware. Both are named as flags before the artifact, which is the position espflash
    /// expects them in.
    #[test]
    fn the_flash_command_carries_the_builds_own_bootloader_and_partition_table() {
        let cmd = esp_flash_command(
            &c6(),
            Path::new("target/riscv32imac-esp-espidf/debug/fw"),
            Some(Path::new("out/build/bootloader/bootloader.bin")),
            Some(Path::new("out/build/partition_table/partition-table.bin")),
            Some(Path::new("/dev/cu.usbserial-1")),
        )
        .expect("command");

        for (flag, value) in [
            ("--bootloader", "out/build/bootloader/bootloader.bin"),
            (
                "--partition-table",
                "out/build/partition_table/partition-table.bin",
            ),
        ] {
            let at = cmd
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} is part of the command: {cmd:?}"));
            assert_eq!(cmd.get(at + 1).map(String::as_str), Some(value), "{cmd:?}");
        }
        assert!(
            cmd.last().expect("artifact").starts_with("target/"),
            "the artifact stays last, after every flag: {cmd:?}"
        );
    }

    /// The discovery is a **glob with a newest-wins rule**, and both halves are pinned: several
    /// `esp-idf-sys-*` build dirs accumulate across re-configurations, and the older one still
    /// describes the *previous* partition table — flashing that would write the app into a layout
    /// this build was not built for.
    #[test]
    fn the_bootloader_and_table_are_the_newest_esp_idf_sys_build() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("target/xtensa-esp32-espidf/debug");
        let artifact = profile.join("blink");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(&artifact, b"app").unwrap();

        // The old build, written first so the newer one is unambiguously newer.
        let old = profile.join("build/esp-idf-sys-aaaa/out/build");
        std::fs::create_dir_all(old.join("bootloader")).unwrap();
        std::fs::create_dir_all(old.join("partition_table")).unwrap();
        std::fs::write(old.join("bootloader/bootloader.bin"), b"old").unwrap();
        std::fs::write(old.join("partition_table/partition-table.bin"), b"old").unwrap();

        // Not an esp-idf-sys dir at all: globbed past rather than guessed at.
        let noise = profile.join("build/some-other-crate-1234/out/build/bootloader");
        std::fs::create_dir_all(&noise).unwrap();
        std::fs::write(noise.join("bootloader.bin"), b"noise").unwrap();

        let new = profile.join("build/esp-idf-sys-bbbb/out/build");
        std::fs::create_dir_all(new.join("bootloader")).unwrap();
        std::fs::create_dir_all(new.join("partition_table")).unwrap();
        std::fs::write(new.join("bootloader/bootloader.bin"), b"new").unwrap();
        std::fs::write(new.join("partition_table/partition-table.bin"), b"new").unwrap();

        let bootloader = esp_bootloader_path(&artifact).expect("the newest bootloader");
        assert_eq!(
            std::fs::read(&bootloader).unwrap(),
            b"new",
            "{bootloader:?}"
        );
        let table = esp_partition_table_path(&artifact).expect("the newest table");
        assert_eq!(std::fs::read(&table).unwrap(), b"new", "{table:?}");

        // An artifact with no esp-idf-sys build beside it answers `None` — the app-only flash —
        // rather than pointing at some path that is not there.
        let bare = tmp.path().join("target/host/debug/tool");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        std::fs::write(&bare, b"host binary").unwrap();
        assert!(esp_bootloader_path(&bare).is_none());
        assert!(esp_partition_table_path(&bare).is_none());
    }

    #[test]
    fn a_platform_without_a_flash_tool_has_no_flash_command() {
        let mut c6 = c6();
        c6.rust.as_mut().expect("rust").flash = None;
        assert!(esp_flash_command(&c6, Path::new("fw"), None, None, None).is_none());
    }

    /// The port is the **single** USB-serial device, and ambiguity is `None` rather than a
    /// pick: two adapters means two boards, and choosing one would flash the wrong silicon.
    ///
    /// The non-adapter devices are here on purpose — they are what the attached machine
    /// actually lists (`cu.Bluetooth-Incoming-Port`, `cu.debug-console`, a headset), and a
    /// filter that did not exclude them would find four devices and refuse forever.
    #[test]
    fn the_serial_port_is_the_single_usb_adapter() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for noise in [
            "cu.Bluetooth-Incoming-Port",
            "cu.debug-console",
            "cu.StevesBeatsStudioBuds",
        ] {
            std::fs::write(dir.join(noise), "").unwrap();
        }
        assert!(
            serial_port_in(dir).is_none(),
            "non-adapter devices are not candidates"
        );

        std::fs::write(dir.join("cu.usbserial-569C0028661"), "").unwrap();
        assert_eq!(
            serial_port_in(dir),
            Some(dir.join("cu.usbserial-569C0028661")),
            "one adapter is the board"
        );

        // A second adapter makes it a guess, and a guess can flash the wrong board.
        std::fs::write(dir.join("cu.wchusbserial1420"), "").unwrap();
        assert!(serial_port_in(dir).is_none(), "two adapters is ambiguous");

        // A directory with no serial devices at all: also None, not an error.
        let empty = tempfile::tempdir().unwrap();
        assert!(serial_port_in(empty.path()).is_none());
    }

    /// The artifact is where *that* build wrote it: the triple is a **subdirectory** (because
    /// `--target` was passed) and the profile directory follows the requested mode. Getting the
    /// profile wrong would flash a binary from an earlier build, which no tool would notice.
    #[test]
    fn the_flash_artifact_is_where_that_build_wrote_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fw\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let debug =
            esp_artifact_path(root, &c6(), "", None, None).expect("the name comes from Cargo.toml");
        assert_eq!(
            debug,
            root.join("target/riscv32imac-esp-espidf/debug/fw"),
            "the triple is a directory under target/, and debug is the default profile"
        );

        let release = esp_artifact_path(root, &c6(), "release", None, None).expect("path");
        assert_eq!(
            release,
            root.join("target/riscv32imac-esp-espidf/release/fw")
        );
    }

    /// The two escape hatches a `[[bin]]` name or a workspace member needs: an explicit
    /// artifact (relative to the project, unless absolute) and an explicit package.
    #[test]
    fn an_explicit_artifact_or_package_overrides_the_derived_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        assert_eq!(
            esp_artifact_path(root, &c6(), "", None, Some(Path::new("out/fw.elf"))),
            Some(root.join("out/fw.elf")),
            "a relative artifact is relative to the project, like every build path"
        );
        assert_eq!(
            esp_artifact_path(root, &c6(), "", None, Some(Path::new("/abs/fw.elf"))),
            Some(PathBuf::from("/abs/fw.elf")),
            "an absolute artifact is not joined to the project"
        );
        assert_eq!(
            esp_artifact_path(root, &c6(), "release", Some("member"), None),
            Some(root.join("target/riscv32imac-esp-espidf/release/member")),
            "a named package wins even when there is no Cargo.toml to read"
        );
    }

    /// With nothing to derive a name from the path is *unknown*, not guessed: an invented
    /// binary name would be flashed (or fail) with no explanation of where it came from.
    #[test]
    fn a_project_without_a_package_name_cannot_say_which_binary() {
        let tmp = tempfile::tempdir().unwrap();
        // A workspace root: `[workspace]` with members, no `[package]`.
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"fw\"]\n",
        )
        .unwrap();
        assert!(esp_artifact_path(tmp.path(), &c6(), "", None, None).is_none());

        // No Cargo.toml at all is the same answer.
        let empty = tempfile::tempdir().unwrap();
        assert!(esp_artifact_path(empty.path(), &c6(), "", None, None).is_none());
    }

    /// The flash spec carries **no environment**: `espflash` is a host binary, so neither the
    /// esp toolchain's `PATH` entry nor `LIBCLANG_PATH` belongs on it. Pinned because copying
    /// the build's env here would look harmless while silently shadowing the user's own tools.
    #[test]
    fn the_flash_spec_runs_the_host_tool_with_no_build_environment() {
        let cmd = esp_flash_command(
            &c6(),
            Path::new("target/riscv32imac-esp-espidf/release/fw"),
            Some(Path::new("/tmp/out/build/bootloader/bootloader.bin")),
            Some(Path::new(
                "/tmp/out/build/partition_table/partition-table.bin",
            )),
            Some(Path::new("/dev/cu.usbserial-569C0028661")),
        )
        .expect("command");
        let spec = spec_from_command(cmd);

        assert_eq!(spec.command, "espflash");
        assert_eq!(
            &spec.arguments[..3],
            ["flash", "--chip", "esp32c6"],
            "{:?}",
            spec.arguments
        );
        assert!(
            spec.arguments
                .contains(&"/dev/cu.usbserial-569C0028661".to_string()),
            "the port travels in the command, not in the environment: {:?}",
            spec.arguments
        );
        // The bootloader and table are arguments too: an env var would be a second interface for
        // the same two paths, and one this host tool never reads.
        for path in [
            "/tmp/out/build/bootloader/bootloader.bin",
            "/tmp/out/build/partition_table/partition-table.bin",
        ] {
            assert!(
                spec.arguments.contains(&path.to_string()),
                "{path} travels in the command: {:?}",
                spec.arguments
            );
        }
        assert!(
            spec.env.is_empty(),
            "espflash needs none of the build's environment: {:?}",
            spec.env
        );
        assert!(
            spec.working_dir.is_empty(),
            "the project root is the working dir"
        );
    }

    /// `run_esp_flash`'s refusals — everything about a flash except running the tool.
    ///
    /// Ordered by what a reader has to fix first: which platform, whether it can be flashed at
    /// all, which binary, and whether that binary exists. Proven without `espflash` and without a
    /// board, which is the point: every one of these declines *before* a device is touched.
    #[tokio::test]
    async fn run_esp_flash_refuses_before_it_could_flash_the_wrong_thing() {
        let _guard = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("platform dir");
        std::fs::write(
            dir.path().join("esp32c6.yaml"),
            "id: esp32c6\nname: ESP32-C6\nos: esp-idf\narchitecture:\n  \
             cpu_family: riscv\n  cpu: esp32c6\n  endian: little\n  \
             target_triple: riscv32imac-esp-espidf\nrust:\n  \
             target: riscv32imac-esp-espidf\n  idf_target: esp32c6\n  flash: espflash\n",
        )
        .unwrap();
        // Same chip, but no USB flash step declared.
        std::fs::write(
            dir.path().join("esp32c6-nousb.yaml"),
            "id: esp32c6-nousb\nname: ESP32-C6 (no USB)\nos: esp-idf\narchitecture:\n  \
             cpu_family: riscv\n  cpu: esp32c6\n  endian: little\n  \
             target_triple: riscv32imac-esp-espidf\nrust:\n  \
             target: riscv32imac-esp-espidf\n  idf_target: esp32c6\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rpi5.yaml"),
            "id: rpi5\nname: Raspberry Pi 5\nos: linux\narchitecture:\n  \
             cpu_family: aarch64\n  cpu: armv8-a\n  endian: little\n  \
             target_triple: aarch64-linux-gnu\n",
        )
        .unwrap();
        let _env = PlatformDir::set(dir.path());

        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"fw\"\n").unwrap();

        // 1. No platform: there is no default chip, and the host is not a board.
        let err = run_esp_flash(root, &BuildOptions::default(), None, None)
            .await
            .expect_err("a flash without a platform must refuse");
        assert!(err.contains("platform"), "{err}");

        // 2. A real platform that is not an ESP platform of either flavour.
        let linux = BuildOptions {
            platform: Some("rpi5".to_string()),
            ..Default::default()
        };
        let err = run_esp_flash(root, &linux, None, None)
            .await
            .expect_err("a linux platform has no flash step");
        assert!(err.contains("not an ESP platform"), "{err}");

        // 3. An esp platform with no flash tool: refused *before* the artifact is looked for,
        //    because no artifact would make it flashable.
        let no_usb = BuildOptions {
            platform: Some("esp32c6-nousb".to_string()),
            ..Default::default()
        };
        let err = run_esp_flash(root, &no_usb, None, None)
            .await
            .expect_err("no tool, no flash");
        assert!(err.contains("no flash tool"), "{err}");

        // 4. A flashable platform whose artifact was never built — the message names the path it
        //    expected, which is what makes "build first" actionable.
        let c6_opts = BuildOptions {
            platform: Some("esp32c6".to_string()),
            ..Default::default()
        };
        let err = run_esp_flash(root, &c6_opts, None, None)
            .await
            .expect_err("nothing has been built yet");
        assert!(
            err.contains("no artifact at") && err.contains("debug/fw"),
            "{err}"
        );

        // 5. A project that cannot name its binary (a workspace root, no `[package]`): the
        //    refusal points at the flag that answers it.
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let err = run_esp_flash(workspace.path(), &c6_opts, None, None)
            .await
            .expect_err("an unnamed binary cannot be flashed");
        assert!(err.contains("artifact=<path>"), "{err}");

        // 6. An id that is not in the registry at all.
        let unknown = BuildOptions {
            platform: Some("no-such-board".to_string()),
            ..Default::default()
        };
        let err = run_esp_flash(root, &unknown, None, None)
            .await
            .expect_err("an unknown platform must refuse");
        assert!(err.contains("unknown platform"), "{err}");
    }

    /// The toolchain probes are filesystem checks, so the *logic* is tested against directories
    /// rather than the machine's `$HOME` — which may not have espup at all, and a test that needed
    /// it would fail for the wrong reason.
    ///
    /// The order is the point: an explicit answer, then a relocated rustup home, then the default
    /// one. Each is a *reference* to an install this machine already has (see `esp_toolchain_bin`).
    #[test]
    fn the_esp_toolchain_is_resolved_explicit_then_rustup_home_then_home() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Nothing installed anywhere: no toolchain, and no clang under one either.
        assert!(esp_toolchain_bin_in(None, None, None).is_none());
        assert!(libclang_path_in(root).is_none());

        // The default rustup home — where espup puts it on a normal machine.
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".rustup/toolchains/esp/bin")).unwrap();
        let from_home = home.join(".rustup/toolchains/esp/bin");
        assert_eq!(
            esp_toolchain_bin_in(None, None, home.to_str()),
            Some(from_home.clone())
        );

        // `RUSTUP_HOME` relocates it, and wins over `$HOME`: that is what the variable is for.
        let moved = root.join("elsewhere/rustup");
        std::fs::create_dir_all(moved.join("toolchains/esp/bin")).unwrap();
        let from_moved = moved.join("toolchains/esp/bin");
        assert_eq!(
            esp_toolchain_bin_in(None, moved.to_str(), home.to_str()),
            Some(from_moved.clone()),
            "a relocated rustup home must be honoured"
        );

        // An explicit `ESP_TOOLCHAIN_BIN` wins over both.
        let explicit = root.join("opt/esp/bin");
        std::fs::create_dir_all(&explicit).unwrap();
        assert_eq!(
            esp_toolchain_bin_in(explicit.to_str(), moved.to_str(), home.to_str()),
            Some(explicit.clone()),
            "an explicit toolchain is an answer, not a hint"
        );

        // A set-but-wrong explicit path falls *through* rather than failing the build: a typo must
        // not lose a discovery that would have worked.
        assert_eq!(
            esp_toolchain_bin_in(Some("/definitely/not/here"), moved.to_str(), home.to_str()),
            Some(from_moved)
        );
        assert_eq!(
            esp_toolchain_bin_in(Some("   "), None, home.to_str()),
            Some(from_home),
            "blank is not an answer"
        );

        // The versioned clang directory is what must not be hardcoded, so it is discovered from
        // whichever toolchain won.
        let toolchain = explicit.parent().unwrap();
        assert!(libclang_path_in(toolchain).is_none());
        let lib = toolchain
            .join("xtensa-esp32-elf-clang")
            .join("esp-20.1.1_20250829")
            .join("esp-clang")
            .join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        assert_eq!(libclang_path_in(toolchain), Some(lib));
    }

    /// The invocation is only correct if these are present, and *each* failure is silent about
    /// its cause — so they are pinned here rather than discovered on a board.
    #[test]
    fn the_spec_carries_mcu_the_toolchain_libclang_and_the_shared_idf_dir() {
        let plan = esp_plan(&c6(), &BuildOptions::default()).expect("plan");
        let spec = spec_from_parts(
            plan,
            Some(Path::new("/home/x/.rustup/toolchains/esp/bin")),
            Some(Path::new("/home/x/clang/lib")),
            "global",
            None,
            Some("/usr/bin:/bin"),
        );

        assert_eq!(spec.command, "cargo");
        assert!(
            spec.arguments
                .contains(&"-Zbuild-std=std,panic_abort".to_string()),
            "{:?}",
            spec.arguments
        );

        let env = |key: &str| {
            spec.env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(env("MCU").as_deref(), Some("esp32c6"), "the chip is MCU");
        // FIRST, not merely present: letting cargo find its own rustc picks stable, and
        // -Zbuild-std then fails complaining about a flag.
        assert_eq!(
            env("PATH").as_deref(),
            Some("/home/x/.rustup/toolchains/esp/bin:/usr/bin:/bin"),
            "the toolchain bin must be prepended to the inherited PATH"
        );
        assert_eq!(env("LIBCLANG_PATH").as_deref(), Some("/home/x/clang/lib"));
        assert_eq!(
            env("ESP_IDF_TOOLS_INSTALL_DIR").as_deref(),
            Some("global"),
            "one ESP-IDF install per machine, not one per project"
        );
    }

    /// An inherited `LIBCLANG_PATH` wins over the one discovered under the esp toolchain.
    ///
    /// This is the "reference, don't own" rule applied to the one variable where it was previously
    /// backwards: the discovered clang was written *over* whatever the user had exported. A user who
    /// set `LIBCLANG_PATH` did so about their own clang — Spire's job is to supply it when the
    /// machine has not, not to overrule it when it has.
    #[test]
    fn an_inherited_libclang_path_wins_over_the_discovered_one() {
        let plan = esp_plan(&c6(), &BuildOptions::default()).expect("plan");
        let env = |spec: &BuildSpec, key: &str| {
            spec.env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };

        // Both present: the caller's wins.
        let spec = spec_from_parts(
            plan.clone(),
            None,
            Some(Path::new("/esp/toolchain/clang/lib")),
            "global",
            Some("/my/own/clang/lib"),
            None,
        );
        assert_eq!(
            env(&spec, "LIBCLANG_PATH").as_deref(),
            Some("/my/own/clang/lib"),
            "an explicit LIBCLANG_PATH is an instruction"
        );

        // Only the caller's: still theirs, and nothing is invented for it.
        let spec = spec_from_parts(
            plan.clone(),
            None,
            None,
            "global",
            Some("/my/own/clang/lib"),
            None,
        );
        assert_eq!(
            env(&spec, "LIBCLANG_PATH").as_deref(),
            Some("/my/own/clang/lib")
        );

        // Only the discovered one: that is what discovery is for.
        let spec = spec_from_parts(
            plan,
            None,
            Some(Path::new("/esp/toolchain/clang/lib")),
            "global",
            None,
            None,
        );
        assert_eq!(
            env(&spec, "LIBCLANG_PATH").as_deref(),
            Some("/esp/toolchain/clang/lib"),
            "bindgen fails with an error that never mentions bindgen, so this must be filled in"
        );
    }

    /// And when they cannot be resolved, nothing is emitted for them: an empty `PATH` entry
    /// or a blank `LIBCLANG_PATH` would be worse than absent, because it would look
    /// deliberate and send the next reader hunting for a configuration mistake.
    #[test]
    fn the_spec_omits_environment_it_could_not_resolve() {
        let plan = esp_plan(&c6(), &BuildOptions::default()).expect("plan");
        let spec = spec_from_parts(plan, None, None, "global", None, None);

        let env = |key: &str| {
            spec.env
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            env("MCU").as_deref(),
            Some("esp32c6"),
            "MCU comes from the platform, not from discovery — it is never optional"
        );
        assert_eq!(
            env("PATH"),
            None,
            "an empty PATH entry would look deliberate"
        );
        assert_eq!(env("LIBCLANG_PATH"), None);
        assert_eq!(
            env("ESP_IDF_TOOLS_INSTALL_DIR").as_deref(),
            Some("global"),
            "not discovered, so it cannot fail to resolve: the shared install survives even \
             when nothing else can be found"
        );
    }

    /// The shared ESP-IDF install setting: the environment wins, and the default is
    /// esp-idf-sys's own `global` keyword — the disk argument being that its default
    /// (`workspace`) is one install per *project* (~5.3 GB here, then ~6.6 GB again for the
    /// second one).
    ///
    /// The **keyword** form is pinned deliberately, because this is not a path: esp-idf-sys
    /// splits the value on `:` and matches the first part against
    /// `global`/`workspace`/`out`/`fromenv`/`custom:<dir>`. Passing an absolute path — the
    /// obvious first attempt, and what this function did in its first version — dies with
    /// `Matching variant not found`, an error that names neither the variable nor the reason.
    #[test]
    fn the_idf_install_dir_defaults_to_global_and_passes_an_override_through() {
        assert_eq!(
            esp_idf_tools_install_dir_in(None),
            "global",
            "unset: the shared `~/.espressif`, not esp-idf-sys's per-project `workspace`"
        );
        assert_eq!(
            esp_idf_tools_install_dir_in(Some("   ")),
            "global",
            "whitespace is not an override"
        );
        assert_eq!(
            esp_idf_tools_install_dir_in(Some("workspace")),
            "workspace",
            "a project can take back the per-project behaviour"
        );
        assert_eq!(
            esp_idf_tools_install_dir_in(Some("custom:/cache/espressif")),
            "custom:/cache/espressif",
            "a shared cache, CI layer or air-gapped mirror: forwarded verbatim, and the only \
             form that names a directory to esp-idf-sys"
        );
    }

    /// Sets `SPIRE_PLATFORM_DIR` for the duration and restores it on drop.
    ///
    /// The var is process-global, so a stale value pointing at a deleted fixture would break
    /// any registry-reading test that ran later. `crate::PLATFORM_DIR_TEST_LOCK` serializes
    /// these against the other writers/readers in the crate.
    struct PlatformDir(Option<String>);

    impl PlatformDir {
        fn set(dir: &Path) -> Self {
            let previous = std::env::var("SPIRE_PLATFORM_DIR").ok();
            std::env::set_var("SPIRE_PLATFORM_DIR", dir);
            Self(previous)
        }
    }

    impl Drop for PlatformDir {
        fn drop(&mut self) {
            match &self.0 {
                Some(prev) => std::env::set_var("SPIRE_PLATFORM_DIR", prev),
                None => std::env::remove_var("SPIRE_PLATFORM_DIR"),
            }
        }
    }

    /// `EspBuildModule` executed for the first time — on the part that needs no toolchain.
    ///
    /// It has never run a real build (that needs espup plus ESP-IDF and minutes), so what is
    /// pinned here is the capability itself, because two of its fields are safety properties:
    /// **no config files** is what stops it shadowing cargo, and the `supports_*` flags are
    /// what make the manager refuse lint/clean/fix *before* routing — each of which would
    /// otherwise run against the wrong chip. `supports_flash` is the mirror image: the one
    /// operation this module *is* for, and the flag that lets the manager route it here.
    #[tokio::test]
    async fn the_esp_module_claims_no_config_file_and_refuses_the_operations_it_lacks() {
        let mut module = EspBuildModule::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        module
            .handle(BuildModuleMessage::DescribeCapabilities { reply_to: tx })
            .await;

        let cap = rx.await.expect("the module answers DescribeCapabilities");
        assert_eq!(cap.name, "esp-idf");
        assert!(
            cap.config_files.is_empty(),
            "claiming Cargo.toml would replace the cargo module for every Rust project: {:?}",
            cap.config_files
        );
        for (operation, supported) in [
            ("clean", cap.supports_clean),
            ("lint", cap.supports_lint),
            ("format", cap.supports_format),
            ("fix", cap.supports_fix),
        ] {
            assert!(
                !supported,
                "'{operation}' must be refused up front, not performed for the wrong chip"
            );
        }
        assert!(
            cap.supports_flash,
            "flash is what this module is for; without the flag the manager refuses every \
             build_flash before it can route it here"
        );
    }

    /// The `Flash` arm answers on its channel with the reason, rather than falling silent.
    ///
    /// A silent arm would reach the manager as a *lost channel* — indistinguishable from a
    /// crash — so the reason has to come back as an error. Proven with no platform, which is the
    /// first refusal and needs neither a toolchain nor a board.
    #[tokio::test]
    async fn the_esp_module_answers_a_flash_it_cannot_perform() {
        let mut module = EspBuildModule::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        module
            .handle(BuildModuleMessage::Flash {
                path: PathBuf::from("/tmp/does-not-matter"),
                metadata: spire_core::build_types::BuildMetadata::default(),
                opts: BuildOptions::default(),
                artifact: None,
                port: None,
                reply_to: tx,
            })
            .await;

        let result = rx.await.expect("the module replies to Flash");
        let err = result.expect_err("a flash without a platform must refuse");
        assert!(err.contains("platform"), "{err}");
    }

    /// `run_esp_build`'s three refusals — the whole function minus the invocation.
    ///
    /// These are the paths that matter most for safety: each one declines *before* anything
    /// could be built for the wrong target, and each names what was wrong. No toolchain and no
    /// board is needed to prove them, which is exactly why they are worth proving.
    #[tokio::test]
    async fn run_esp_build_refuses_before_it_could_build_for_the_wrong_chip() {
        let _guard = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("platform dir");
        std::fs::write(
            dir.path().join("rpi5.yaml"),
            "id: rpi5\nname: Raspberry Pi 5\nos: linux\narchitecture:\n  \
             cpu_family: aarch64\n  cpu: armv8-a\n  endian: little\n  \
             target_triple: aarch64-linux-gnu\n",
        )
        .unwrap();
        let _env = PlatformDir::set(dir.path());

        // 1. No platform at all: the caller has to say which chip, because there is no
        //    sensible default and guessing one would target the wrong silicon.
        let err = run_esp_build(Path::new("/tmp/does-not-matter"), &BuildOptions::default())
            .await
            .expect_err("a build without a platform must refuse");
        assert!(
            err.contains("platform"),
            "the error should name the gap: {err}"
        );

        // 2. A real platform that is not an ESP platform of either flavour: refuse rather than build it
        //    as if it were.
        let linux = BuildOptions {
            platform: Some("rpi5".to_string()),
            ..Default::default()
        };
        let err = run_esp_build(Path::new("/tmp/does-not-matter"), &linux)
            .await
            .expect_err("a linux platform must not be built by this module");
        assert!(err.contains("not an ESP platform"), "{err}");

        // 3. An id that is not in the registry at all.
        let unknown = BuildOptions {
            platform: Some("no-such-board".to_string()),
            ..Default::default()
        };
        let err = run_esp_build(Path::new("/tmp/does-not-matter"), &unknown)
            .await
            .expect_err("an unknown platform must refuse");
        assert!(err.contains("unknown platform"), "{err}");
    }
}
