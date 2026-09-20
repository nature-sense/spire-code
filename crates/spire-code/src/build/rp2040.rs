// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **rp2040** build module: a plain `cargo` project for a `thumbv6m` target, with no vendor C
//! SDK behind it.
//!
//! What makes it a module of its own rather than cargo's job is the same thing that makes the
//! esp-idf one: the *invocation*. Three differences, none of which cargo can guess:
//!
//! 1. **The target is a platform fact**, not a project one: `thumbv6m-none-eabi` comes from the
//!    registry entry, and the toolchain that will run the build must have it. That is checked
//!    before anything is built, against the **sysroot `rustc --print sysroot` reports** — the
//!    question cargo is about to ask — because cargo's own failure names the triple and, when the
//!    toolchain on `PATH` is not rustup's, suggests installing a target that *is* installed.
//! 2. **No `-Zbuild-std`.** Unlike the esp targets this one has a prebuilt `core`, so a plain
//!    `cargo build --target` works on stable — which is why an rp2040 needs no custom toolchain
//!    and why nothing here touches `PATH`, `LIBCLANG_PATH` or an SDK install directory.
//! 3. **The flash is a host tool** that a platform names in `rust.flash`, and it is not
//!    `espflash`: see [`rp2040_flash_command`] for the three the boards are actually flashed with.
//!
//! Registered with `AddPlatformModule { os: "rp2040" }` and **never** with a config file: an
//! rp2040 project is *also* a `Cargo.toml` project, so claiming that file would replace the cargo
//! module for every Rust project in Spire (see `AddPlatformModule`).

use crate::build::{BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability};
use crate::platform::Platform;
use async_trait::async_trait;
use spire_actor::Actor;
use spire_core::build_types::BuildSpec;
use std::path::{Path, PathBuf};

/// The platform `os` this module answers for.
pub const RP2040_OS: &str = "rp2040";

/// The triple every rp2040 build uses. The platform entry carries it (`rust.target`); this is the
/// name of the fact, used in refusals so the message can say what to install.
pub const RP2040_TARGET: &str = "thumbv6m-none-eabi";

/// One rp2040 build: the program to run, its arguments, and its environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rp2040Plan {
    /// The chip, as the platform spells it — `probe-rs --chip` needs a name it recognises.
    pub chip: String,
    /// The rustup target triple, also passed as `--target`.
    pub target: String,
    /// Arguments after the program name.
    pub args: Vec<String>,
    /// Environment additions. Empty on purpose: an rp2040 build needs nothing the shell does not
    /// already have, which is the difference from esp-idf that makes this module small.
    pub env: Vec<(String, String)>,
}

/// The chip a platform targets — `rp2040` — or `None` when this is not an rp2040 platform.
///
/// `rust.idf_target` is preferred over `architecture.cpu` for the same reason the esp module
/// prefers it: it is the *vendor's* spelling, and a flash tool may need it spelled its way
/// (`probe-rs --chip RP2040`). The fallback keeps a hand-written entry with only the architecture
/// working.
pub fn rp2040_chip(platform: &Platform) -> Option<String> {
    if platform.os != RP2040_OS {
        return None;
    }
    let chip = platform
        .rust
        .as_ref()
        .and_then(|rust| rust.idf_target.as_deref())
        .map(str::trim)
        .filter(|chip| !chip.is_empty())
        .unwrap_or_else(|| platform.architecture.cpu.trim());
    (!chip.is_empty()).then(|| chip.to_string())
}

/// The rp2040 plan for `platform`, or `None` when this is not an rp2040 platform.
///
/// Returning `None` is what stops this module from claiming a plain Rust project: a `linux`
/// platform with a `Cargo.toml` must go to `CargoBuildModule`, exactly as before. An `os: rp2040`
/// entry without a `rust.target`, or with an empty one, is refused for the same reason — a build
/// with no `--target` would compile for the host and then fail at the link with an error about
/// every symbol.
pub fn rp2040_plan(platform: &Platform, opts: &BuildOptions) -> Option<Rp2040Plan> {
    if platform.os != RP2040_OS {
        return None;
    }
    let rust = platform.rust.as_ref()?;
    let target = rust.target.trim();
    if target.is_empty() {
        return None;
    }

    let mut args = vec![
        "build".to_string(),
        "--target".to_string(),
        target.to_string(),
    ];
    if opts.mode.eq_ignore_ascii_case("release") {
        args.push("--release".to_string());
    }
    if let Some(package) = opts.package.as_deref().filter(|p| !p.trim().is_empty()) {
        args.push("-p".to_string());
        args.push(package.to_string());
    }

    Some(Rp2040Plan {
        chip: rp2040_chip(platform).unwrap_or_default(),
        target: target.to_string(),
        args,
        env: Vec::new(),
    })
}

/// The host-side flash tool a platform declares (`rust.flash`), or `None`.
pub fn rp2040_flash_tool(platform: &Platform) -> Option<&str> {
    platform
        .rust
        .as_ref()?
        .flash
        .as_deref()
        .filter(|tool| !tool.trim().is_empty())
}

/// The host-side flash command for `artifact`, or `None` when the platform names no tool this
/// module knows.
///
/// Three tools, because there are three ways to get code onto an rp2040 and each needs something
/// different:
///
/// * **`picotool`** — the official tool, talking to the ROM bootloader over USB while the board is
///   in BOOTSEL mode. No probe, no mount, nothing to select: `picotool load -x <elf>` downloads and
///   reboots into the program.
/// * **`probe-rs`** — a debug probe (a second Pico running `debugprobe`, a J-Link, …). `--chip` is
///   required and comes from the platform, because probe-rs cannot infer it from a bare ELF.
/// * **`elf2uf2-rs`** — a UF2 written to the board's mounted `RPI-RP2` volume: the classic
///   drag-and-drop route without the drag.
///
/// A tool this module does not know is *refused* rather than passed through: a typo in the platform
/// entry would otherwise run a command that does not exist, and the failure would name the shell
/// rather than the platform.
///
/// `device` is the one argument whose meaning is per-tool, and it is **optional for all three**: a
/// probe-rs selector, or the volume to write a UF2 to. Nothing is auto-discovered here — unlike
/// espflash, none of these needs to be told which port to use, so there is nothing to guess.
pub fn rp2040_flash_command(
    platform: &Platform,
    artifact: &Path,
    device: Option<&Path>,
) -> Option<Vec<String>> {
    let tool = rp2040_flash_tool(platform)?;
    let elf = artifact.to_string_lossy().to_string();
    match tool {
        "picotool" => Some(vec![
            tool.to_string(),
            "load".to_string(),
            "-x".to_string(),
            elf,
        ]),
        "probe-rs" => {
            let chip = rp2040_chip(platform)?;
            let mut command = vec![
                tool.to_string(),
                "run".to_string(),
                "--chip".to_string(),
                chip,
            ];
            if let Some(device) = device {
                command.push("--probe".to_string());
                command.push(device.to_string_lossy().to_string());
            }
            command.push(elf);
            Some(command)
        }
        "elf2uf2" | "elf2uf2-rs" => {
            let mut command = vec!["elf2uf2-rs".to_string()];
            match device {
                // The tool's own INPUT OUTPUT form, with the UF2 named after the binary: the user
                // named the volume, so the copy is theirs to make (or to watch land).
                Some(device) => {
                    let stem = artifact
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("firmware");
                    command.push(elf);
                    command.push(
                        device
                            .join(format!("{stem}.uf2"))
                            .to_string_lossy()
                            .to_string(),
                    );
                }
                // `-d` is the deploy flag: it finds the mounted volume itself, copies the UF2 and
                // reboots the board.
                None => {
                    command.push("-d".to_string());
                    command.push(elf);
                }
            }
            Some(command)
        }
        _ => None,
    }
}

/// Whether the toolchain at `sysroot` has the standard library for `target`.
///
/// A sysroot's layout is how rustc finds a target's `core`: `lib/rustlib/<triple>/lib`. Asking the
/// **sysroot** rather than rustup's installed-target list is deliberate — it is the question cargo
/// is about to ask, and on a machine whose `cargo` is not rustup's shim the two disagree (measured
/// on this one: Homebrew's `cargo` has no `thumbv6m` while rustup's toolchain has it installed and
/// cannot be seen from there).
pub fn target_available_in_sysroot(sysroot: &Path, target: &str) -> bool {
    sysroot
        .join("lib")
        .join("rustlib")
        .join(target.trim())
        .join("lib")
        .is_dir()
}

/// Whether `target` appears in the output of `rustup target list --installed`.
///
/// Input rather than a `rustup` call so the check is provable on a string: what is under test is
/// "does this output contain that triple", and the machine running the test has its own targets.
pub fn target_installed_in(installed: &str, target: &str) -> bool {
    installed
        .lines()
        .map(str::trim)
        .any(|line| line == target.trim())
}

/// Output of a command run in the project, or `None` when it could not be run at all.
///
/// Run in the project so a `rust-toolchain.toml` is honoured — the toolchain `cargo` uses is not
/// always the default one, and both questions below are about *that* toolchain.
fn run_in_project(program: &str, args: &[&str], root: &Path) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

/// The sysroot of the `rustc` the build will use, or `None` when it cannot be asked.
pub(crate) fn rustc_sysroot(root: &Path) -> Option<PathBuf> {
    let sysroot = run_in_project("rustc", &["--print", "sysroot"], root)?;
    let trimmed = sysroot.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// The targets rustup's toolchain has installed, or `None` when `rustup` could not be asked.
///
/// Only used to tell the two missing-target cases apart in the refusal — see
/// [`spec_from_target`].
pub(crate) fn rustup_installed_targets(root: &Path) -> Option<String> {
    run_in_project("rustup", &["target", "list", "--installed"], root)
}

/// The platform and plan a build *or* a flash both need, or the refusal naming what is missing.
///
/// One function so the two paths cannot drift in what they accept — while the message names the
/// operation, because "an rp2040 build needs a platform" is a confusing thing to read when you
/// asked for a flash. `rp2040_plan` is what decides "is this rp2040".
fn rp2040_platform_plan(op: &str, opts: &BuildOptions) -> Result<(Platform, Rp2040Plan), String> {
    let platform_id = opts
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| format!("an rp2040 {op} needs a platform, e.g. \"rp2040\""))?;
    let platform = Platform::from_registry(platform_id)
        .ok_or_else(|| format!("unknown platform '{platform_id}'"))?;
    let plan = rp2040_plan(&platform, opts).ok_or_else(|| {
        format!("platform '{platform_id}' is not an rp2040 platform with a rust.target")
    })?;
    Ok((platform, plan))
}

/// Run an rp2040 build for `opts.platform`, through the **shared** process runner.
///
/// Reusing `run_build_spec` rather than spawning a `Command` here means duration measurement and
/// exit-code reporting behave exactly as they do for every other build module.
pub async fn run_rp2040_build(path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
    let (_, plan) = rp2040_platform_plan("build", opts)?;
    let spec = spec_from_target(
        plan,
        rustc_sysroot(path).as_deref(),
        rustup_installed_targets(path).as_deref(),
    )?;
    crate::build::generic_helpers::run_build_spec(path, &spec).await
}

/// The [`BuildSpec`] an [`Rp2040Plan`] becomes, with the one requirement an rp2040 build has.
///
/// The sysroot and rustup's list are passed in for the same reason esp's `spec_from_parts` takes
/// its toolchain paths: fetching them runs `rustc`/`rustup` and reads the environment, so a test
/// calling them directly would pass or fail on whether *this* machine happens to have the target —
/// failing for a reason unrelated to the logic under test.
///
/// **Two** missing-target cases, because they need different fixes and cargo's own message
/// describes neither: the target is not installed at all (`rustup target add`), or it is installed
/// for rustup's toolchain while the `cargo` about to run is not rustup's (`cargo` on `PATH` is not
/// the shim). The second is not hypothetical — it is what this machine does, and cargo's message
/// there suggests installing a target that is already installed.
///
/// `sysroot == None` (no `rustc`) is **not** a refusal: cargo's own error is better than a guess
/// about a toolchain this module cannot see.
pub(crate) fn spec_from_target(
    plan: Rp2040Plan,
    sysroot: Option<&Path>,
    rustup_targets: Option<&str>,
) -> Result<BuildSpec, String> {
    spec_for_target(plan.args, plan.env, &plan.target, sysroot, rustup_targets)
}

/// The same check for any **bare-metal** plan: an argument list, an environment, and a stock target.
///
/// Shared with the esp module's `esp-hal` flavour, whose build has the same shape — a stock rustup
/// target with no vendor SDK — and whose failure on a machine whose `cargo` is not rustup's is the
/// same one this was written for.
pub(crate) fn spec_for_target(
    args: Vec<String>,
    env: Vec<(String, String)>,
    target: &str,
    sysroot: Option<&Path>,
    rustup_targets: Option<&str>,
) -> Result<BuildSpec, String> {
    if let Some(sysroot) = sysroot {
        if !target_available_in_sysroot(sysroot, target) {
            let known_to_rustup = rustup_targets
                .map(|targets| target_installed_in(targets, target))
                .unwrap_or(false);
            return Err(if known_to_rustup {
                format!(
                    "the target '{}' is installed for rustup's toolchain but not for the toolchain \
                     this build will run (sysroot {}): `cargo` and `rustc` on PATH are not rustup's \
                     — put the directory `rustup which cargo` prints first on PATH, or build \
                     through `rustup run <toolchain> cargo`",
                    target,
                    sysroot.display()
                )
            } else {
                format!(
                    "the target '{}' is not installed for the toolchain at {}: run `rustup target add {}`",
                    target,
                    sysroot.display(),
                    target
                )
            });
        }
    }
    let mut env = env;

    // `rust-lld` links dynamically against the toolchain's own `libLLVM`, and on macOS nothing finds it
    // by default: every link dies with `dyld: Library not loaded: @rpath/libLLVM.dylib`, naming neither
    // the target nor the build. Measured here, and the reason the live tests set this by hand — a build
    // module that does not set it links nothing on this host. The toolchain's `lib` is where the
    // library lives, and the sysroot `rustc --print sysroot` reports *is* the toolchain root.
    if let Some(sysroot) = sysroot {
        env.push((
            "DYLD_FALLBACK_LIBRARY_PATH".to_string(),
            sysroot.join("lib").to_string_lossy().to_string(),
        ));
    }

    Ok(BuildSpec {
        command: "cargo".to_string(),
        arguments: args,
        working_dir: String::new(),
        env,
    })
}

/// Flash the artifact for `opts.platform` onto the board, over USB.
///
/// Every failure mode is a *refusal*, never an attempt: no platform, an unknown one, one that is not
/// rp2040, one that declares no flash tool, one that names a tool this module does not know, an
/// artifact that cannot be identified, and an artifact that is not there. Each would otherwise flash
/// the wrong binary, or the right binary onto the wrong chip — and all of them happen before the
/// tool runs.
///
/// There is no port discovery here, unlike esp: none of the three tools needs to be told where the
/// board is (`picotool` talks to the bootloader, `probe-rs` finds a single probe, `elf2uf2-rs` finds
/// the mounted volume), so there is nothing to guess and nothing to refuse over.
pub async fn run_rp2040_flash(
    path: &Path,
    opts: &BuildOptions,
    artifact: Option<&Path>,
    device: Option<&Path>,
) -> Result<BuildOutput, String> {
    let (platform, plan) = rp2040_platform_plan("flash", opts)?;

    let tool = rp2040_flash_tool(&platform).ok_or_else(|| {
        format!(
            "platform '{}' declares no flash tool (rust.flash), so nothing can flash it",
            platform.id
        )
    })?;

    let artifact = crate::build::generic_helpers::cargo_artifact_path(
        path,
        &plan.target,
        &opts.mode,
        opts.package.as_deref(),
        artifact,
    )
    .ok_or_else(|| {
        "cannot tell which binary to flash: Cargo.toml has no [package] name and no package was \
         given — pass artifact=<path> or package=<name>"
            .to_string()
    })?;
    if !artifact.is_file() {
        return Err(format!(
            "no artifact at {}; build for '{}' first, or pass artifact=<path> / mode=<profile>",
            artifact.display(),
            platform.id
        ));
    }

    let command = rp2040_flash_command(&platform, &artifact, device).ok_or_else(|| {
        format!(
            "platform '{}' names a flash tool this module does not know ('{tool}'): use picotool, \
             probe-rs or elf2uf2-rs",
            platform.id
        )
    })?;
    crate::build::generic_helpers::run_build_spec(
        path,
        &crate::build::generic_helpers::build_spec_from_command(command),
    )
    .await
}

// ─────────────────────────────────────────────────────────────────────────────────────
// The module
// ─────────────────────────────────────────────────────────────────────────────────────

/// The rp2040 build module.
///
/// Registered with `AddPlatformModule { os: "rp2040" }` and **never** with a config file — see the
/// module docs. Analysis is not its job either: a `Cargo.toml` analyses the same way whatever the
/// target is, so the cargo module keeps that and only the invocation differs here.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rp2040BuildModule;

impl Rp2040BuildModule {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Actor for Rp2040BuildModule {
    type Message = BuildModuleMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: RP2040_OS.to_string(),
                    // Empty on purpose: routing is by platform, not by config file.
                    config_files: Vec::new(),
                    build_system: "Cargo (rp2040)".to_string(),
                    language: "Rust".to_string(),
                    source_extensions: vec!["rs".to_string()],
                    mcp_servers: Vec::new(),
                    // The artifact exists, the chip is a platform fact, and the tools are host
                    // binaries — the same reasoning the esp module's flag carries.
                    supports_flash: true,
                    // Declared false so the manager refuses these *before* routing here: a clean
                    // or a lint would run against the host toolchain, which is worse than
                    // refusing.
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
                // `port` is esp's name for "the device"; here it is whatever selector the
                // platform's flash tool takes (see `rp2040_flash_command`).
                let _ = reply_to.send(
                    run_rp2040_flash(&path, &opts, artifact.as_deref(), port.as_deref()).await,
                );
            }

            BuildModuleMessage::Build {
                path,
                opts,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(run_rp2040_build(&path, &opts).await);
            }

            BuildModuleMessage::BuildStreaming {
                path,
                opts,
                event_tx,
                reply_to,
                ..
            } => {
                let result = run_rp2040_build(&path, &opts).await;
                // One synthetic finished line, as the other modules fall back to when they cannot
                // stream per-line.
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
                // As in the esp module: everything else either has its capability declared false
                // above — so the manager refuses it before routing — or belongs to cargo. Warn
                // rather than reply, so a future routing change surfaces here.
                tracing::warn!(
                    "Rp2040BuildModule: a message it does not implement was routed here; \
                     the build manager should have refused or routed it elsewhere"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{PlatformArchitecture, PlatformRust, PlatformSysroot, PlatformToolchain};

    /// An rp2040 platform entry, or one with the fields under test blanked.
    fn platform(os: &str, triple: &str, flash: Option<&str>) -> Platform {
        Platform {
            id: "rp2040".into(),
            name: "Raspberry Pi Pico".into(),
            os: os.into(),
            architecture: PlatformArchitecture {
                cpu_family: "arm".into(),
                cpu: "rp2040".into(),
                endian: "little".into(),
                target_triple: triple.into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            device: None,
            family: Some("rp2040".into()),
            chip: None,
            hal: None,
            rust: Some(PlatformRust {
                target: triple.to_string(),
                idf_target: Some("RP2040".to_string()),
                flash: flash.map(str::to_string),
            }),
            library_hints: None,
        }
    }

    fn opts(platform_id: Option<&str>, mode: &str) -> BuildOptions {
        BuildOptions {
            mode: mode.to_string(),
            platform: platform_id.map(str::to_string),
            ..Default::default()
        }
    }

    fn pico() -> Platform {
        platform(RP2040_OS, RP2040_TARGET, Some("picotool"))
    }

    /// The plan is the invocation, and the two things that make it an rp2040 build are the target
    /// and the absence of everything esp needs.
    #[test]
    fn the_plan_is_a_cargo_build_for_the_platforms_target() {
        let plan = rp2040_plan(&pico(), &opts(Some("rp2040"), "release")).expect("a plan");
        assert_eq!(
            plan.args,
            vec!["build", "--target", RP2040_TARGET, "--release"],
            "{plan:?}"
        );
        assert_eq!(plan.target, RP2040_TARGET);
        assert_eq!(plan.chip, "RP2040", "the vendor's spelling, for probe-rs");
        assert!(
            plan.env.is_empty(),
            "an rp2040 build needs no environment: {plan:?}"
        );
        assert!(
            !plan.args.iter().any(|arg| arg.contains("build-std")),
            "this target has a prebuilt core, so -Zbuild-std would need a nightly toolchain for \
             nothing: {plan:?}"
        );
    }

    /// A platform that is not rp2040 is not this module's business — `None` is what keeps a plain
    /// Rust project (or a Linux cross-target) on the cargo module.
    #[test]
    fn only_an_rp2040_platform_gets_a_plan() {
        for other in ["linux", "esp-idf", "none"] {
            assert!(
                rp2040_plan(
                    &platform(other, "aarch64-unknown-linux-gnu", None),
                    &opts(None, "")
                )
                .is_none(),
                "os '{other}' must not be planned by this module"
            );
        }
        // An `os: rp2040` entry with no usable target cannot be built either, and saying so here
        // is what makes the refusal name the missing field.
        assert!(rp2040_plan(&platform(RP2040_OS, "", None), &opts(None, "")).is_none());
        let no_rust = Platform {
            rust: None,
            ..pico()
        };
        assert!(rp2040_plan(&no_rust, &opts(None, "")).is_none());
    }

    /// `--package` is forwarded when asked for, because a workspace's default members may not
    /// include the binary the user means.
    #[test]
    fn the_package_reaches_cargo() {
        let mut options = opts(Some("rp2040"), "");
        options.package = Some("blink-rp2040".to_string());
        let plan = rp2040_plan(&pico(), &options).expect("a plan");
        assert_eq!(
            plan.args,
            vec!["build", "--target", RP2040_TARGET, "-p", "blink-rp2040"]
        );
    }

    /// The three tools the boards are flashed with, and the one thing each needs that the others do
    /// not: nothing for picotool, a chip for probe-rs, a deploy flag for elf2uf2-rs.
    #[test]
    fn each_flash_tool_gets_its_own_command() {
        let artifact = Path::new("/tmp/target/thumbv6m-none-eabi/release/blink");

        let picotool = rp2040_flash_command(
            &platform(RP2040_OS, RP2040_TARGET, Some("picotool")),
            artifact,
            None,
        )
        .expect("picotool is known");
        assert_eq!(
            picotool,
            vec!["picotool", "load", "-x", artifact.to_str().unwrap()]
        );

        let probe = rp2040_flash_command(
            &platform(RP2040_OS, RP2040_TARGET, Some("probe-rs")),
            artifact,
            Some(Path::new("0d28:0204")),
        )
        .expect("probe-rs is known");
        assert_eq!(
            probe,
            vec![
                "probe-rs",
                "run",
                "--chip",
                "RP2040",
                "--probe",
                "0d28:0204",
                artifact.to_str().unwrap()
            ]
        );

        // Without a device elf2uf2-rs deploys: it finds the mounted volume itself.
        let uf2 = rp2040_flash_command(
            &platform(RP2040_OS, RP2040_TARGET, Some("elf2uf2-rs")),
            artifact,
            None,
        )
        .expect("elf2uf2-rs is known");
        assert_eq!(uf2, vec!["elf2uf2-rs", "-d", artifact.to_str().unwrap()]);

        // With one, it uses the tool's INPUT OUTPUT form, the UF2 named after the binary.
        let uf2_at = rp2040_flash_command(
            &platform(RP2040_OS, RP2040_TARGET, Some("elf2uf2")),
            artifact,
            Some(Path::new("/Volumes/RPI-RP2")),
        )
        .expect("the short alias is known too");
        assert_eq!(
            uf2_at,
            vec![
                "elf2uf2-rs",
                artifact.to_str().unwrap(),
                "/Volumes/RPI-RP2/blink.uf2"
            ]
        );
    }

    /// A platform whose `rust.flash` is absent or a typo is refused rather than run: the failure
    /// would otherwise name the shell, not the platform entry that is wrong.
    #[test]
    fn an_unknown_flash_tool_is_refused_rather_than_run() {
        let artifact = Path::new("/tmp/blink");
        for flash in [None, Some("openocd"), Some("espflash")] {
            assert!(
                rp2040_flash_command(&platform(RP2040_OS, RP2040_TARGET, flash), artifact, None)
                    .is_none(),
                "flash={flash:?} must not produce a command"
            );
        }
    }

    /// The build refuses before running anything when the target is not there — and the two ways it
    /// can be missing get different messages, because they need different fixes.
    ///
    /// The second is the one cargo cannot describe: it says "may not be installed" and suggests
    /// installing a target that *is* installed, because the toolchain that has it is not the one
    /// about to run. Measured on this machine (Homebrew's `cargo`, rustup's toolchain).
    #[test]
    fn a_missing_target_is_refused_with_the_reason_it_is_missing() {
        let plan = rp2040_plan(&pico(), &opts(Some("rp2040"), "")).expect("a plan");
        let sysroot = tempfile::tempdir().expect("sysroot dir");
        // A toolchain that can build for itself and nothing else.
        std::fs::create_dir_all(sysroot.path().join("lib/rustlib/aarch64-apple-darwin/lib"))
            .unwrap();

        // Case 1: nothing has it.
        let err = spec_from_target(
            plan.clone(),
            Some(sysroot.path()),
            Some("aarch64-apple-darwin\n"),
        )
        .expect_err("the target is missing from that list");
        assert!(err.contains(RP2040_TARGET), "{err}");
        assert!(
            err.contains("rustup target add"),
            "the refusal says what to run: {err}"
        );

        // Case 2: rustup's toolchain has it, the toolchain we will run does not.
        let err = spec_from_target(
            plan.clone(),
            Some(sysroot.path()),
            Some("aarch64-apple-darwin\nthumbv6m-none-eabi\n"),
        )
        .expect_err("rustup knowing about it does not make this toolchain have it");
        assert!(
            err.contains("not rustup's"),
            "the refusal names the real cause: {err}"
        );
        assert!(
            !err.contains("run `rustup target add"),
            "and does not suggest installing what is installed: {err}"
        );

        // Case 3: the toolchain we will run has it.
        std::fs::create_dir_all(
            sysroot
                .path()
                .join("lib/rustlib")
                .join(RP2040_TARGET)
                .join("lib"),
        )
        .unwrap();
        let spec = spec_from_target(plan, Some(sysroot.path()), Some(""))
            .expect("the target is installed");
        assert_eq!(spec.command, "cargo");
        assert_eq!(spec.arguments, vec!["build", "--target", RP2040_TARGET]);
        // The one environment a bare-metal build needs on this host: `rust-lld` cannot find the
        // toolchain's own `libLLVM` without it, and every link then fails with a `dyld` error that names
        // neither the target nor the build.
        assert_eq!(
            spec.env,
            vec![(
                "DYLD_FALLBACK_LIBRARY_PATH".to_string(),
                sysroot.path().join("lib").to_string_lossy().to_string()
            )],
            "{spec:?}"
        );
    }

    /// A toolchain that cannot be asked is not a refusal: cargo's own error is better than a guess
    /// about a toolchain this module cannot see.
    #[test]
    fn an_unanswerable_toolchain_does_not_block_the_build() {
        let plan = rp2040_plan(&pico(), &opts(Some("rp2040"), "")).expect("a plan");
        assert!(spec_from_target(plan, None, None).is_ok());
    }

    /// Both questions are asked of the right place: the sysroot is a *directory* test, and rustup's
    /// list is matched exactly — a triple that merely *contains* the wanted one is a different
    /// target, and treating it as present would start a build that cannot link.
    #[test]
    fn the_target_is_looked_for_where_the_toolchain_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!target_available_in_sysroot(tmp.path(), RP2040_TARGET));
        std::fs::create_dir_all(tmp.path().join("lib/rustlib/thumbv6m-none-eabi/lib")).unwrap();
        assert!(target_available_in_sysroot(tmp.path(), RP2040_TARGET));
        // The directory alone is not enough: rustc wants the `lib` inside it.
        let bare = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(bare.path().join("lib/rustlib/thumbv6m-none-eabi")).unwrap();
        assert!(!target_available_in_sysroot(bare.path(), RP2040_TARGET));

        assert!(target_installed_in("thumbv6m-none-eabi\n", RP2040_TARGET));
        assert!(target_installed_in(
            "  thumbv6m-none-eabi  \n",
            RP2040_TARGET
        ));
        assert!(!target_installed_in(
            "thumbv6m-none-eabihf\n",
            RP2040_TARGET
        ));
        assert!(!target_installed_in("", RP2040_TARGET));
    }

    /// Read the **whole registry** (`~/.spire/platforms/*.yaml`) through the real loader, so a
    /// hand-edited entry is known to parse before anything depends on it — and so the hints the
    /// wizard shows and the fill prompt injects can be read back as one list.
    ///
    /// Ignored by default because it reads outside the target directory:
    ///
    /// ```sh
    /// cargo test -p spire-code --lib dump_registry -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "reads the platform stores from ~/.spire/<app>/{boards,platforms,chips}"]
    fn dump_registry() {
        let platforms = Platform::load_registry().expect("the registry loads");
        println!("{} entries across the stores", platforms.len());
        for platform in platforms {
            println!(
                "  {:<10} os={:<8} embedded={:<5} family={:<8} flash={:<9} hints={} chars",
                platform.id,
                platform.os,
                platform.is_embedded(),
                platform.family.as_deref().unwrap_or("-"),
                platform
                    .rust
                    .as_ref()
                    .and_then(|r| r.flash.as_deref())
                    .unwrap_or("-"),
                platform.library_hints.as_deref().unwrap_or("").trim().len()
            );
        }
    }

    /// Read the **real** registry entry (`~/.spire/platforms/rp2040.yaml`) through the real code,
    /// so the platform definition and the module cannot disagree about what an rp2040 build is —
    /// the triple, the chip spelling a tool needs, the flasher, and that it counts as embedded.
    ///
    /// Ignored by default because it reads outside the target directory, which is the caller's
    /// decision rather than a test's:
    ///
    /// ```sh
    /// cargo test -p spire-code --lib dump_rp2040_registry_entry -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "reads the platform registry from ~/.spire/platforms (or SPIRE_PLATFORM_FILE)"]
    fn dump_rp2040_registry_entry() {
        let path = std::env::var("SPIRE_PLATFORM_FILE").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.spire/platforms/rp2040.yaml")
        });
        let platform = Platform::load(&path).expect("the registry entry parses");
        let plan = rp2040_plan(&platform, &BuildOptions::default()).expect("it plans a build");

        println!("entry:       {path}");
        println!("is_embedded: {}", platform.is_embedded());
        println!("family:      {:?}", platform.family);
        println!("hints:       {:?}", platform.library_hints.as_deref());
        println!("build args:  {:?}", plan.args);
        println!("chip:        {}", plan.chip);
        println!("env:         {:?}", plan.env);
        let sysroot = rustc_sysroot(Path::new("."));
        println!("sysroot:     {:?}", sysroot);
        println!(
            "target ok:   {:?}",
            sysroot
                .as_deref()
                .map(|s| target_available_in_sysroot(s, RP2040_TARGET))
        );
        println!(
            "rustup has:  {:?}",
            rustup_installed_targets(Path::new("."))
                .map(|targets| target_installed_in(&targets, RP2040_TARGET))
        );
        println!(
            "verdict:     {:?}",
            spec_from_target(
                rp2040_plan(&platform, &BuildOptions::default()).expect("it plans a build"),
                sysroot.as_deref(),
                rustup_installed_targets(Path::new(".")).as_deref(),
            )
            .map(|spec| spec.arguments)
        );
    }
}
