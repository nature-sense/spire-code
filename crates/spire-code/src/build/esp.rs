// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The esp-idf build plan — what an ESP32 project's build actually *is*.
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

use std::path::{Path, PathBuf};

use crate::build::{BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability};
use crate::platform::Platform;
use spire_actor::Actor;
use spire_core::build_types::BuildSpec;

/// The rustup toolchain `espup` installs. A *custom* toolchain, so `rustup target add`
/// cannot add to it; it carries `rust-src` (hence `-Zbuild-std`) instead.
pub const ESP_TOOLCHAIN: &str = "esp";

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
}

/// The esp-idf plan for `platform`, or `None` when this is not an esp-idf platform.
///
/// Returning `None` is what stops the ESP32 module from claiming a plain Rust project: a
/// `linux` platform with a `Cargo.toml` must go to `CargoBuildModule`, exactly as before.
pub fn esp_plan(platform: &Platform, opts: &BuildOptions) -> Option<EspPlan> {
    let rust = platform.rust.as_ref()?;
    if platform.os != "esp-idf" {
        return None;
    }
    // `cpu` and `idf_target` say the same thing; prefer the explicit one, and fall back so a
    // hand-written YAML with only `cpu` still works.
    let chip = match rust
        .idf_target
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(platform.architecture.cpu.as_str())
    {
        "" => return None,
        chip => chip.to_string(),
    };

    let mut args = vec![
        "build".to_string(),
        "--target".to_string(),
        rust.target.clone(),
        // These targets are tier-3 with no prebuilt std: without this the build dies with
        // "can't find crate for `core`", which reads like a missing toolchain.
        "-Zbuild-std=std,panic_abort".to_string(),
    ];
    if opts.mode.eq_ignore_ascii_case("release") {
        args.push("--release".to_string());
    }
    if let Some(package) = opts.package.as_deref().filter(|p| !p.trim().is_empty()) {
        args.push("-p".to_string());
        args.push(package.to_string());
    }

    Some(EspPlan {
        chip: chip.clone(),
        target: rust.target.clone(),
        args,
        env: vec![("MCU".to_string(), chip)],
    })
}

/// The host-side flash command for `artifact` (`espflash`, from `rust.flash`), or `None`.
///
/// Deliberately a **host** command, not a device-MCP one: a board that has never been
/// flashed has nothing running to talk to, so the network leg cannot bootstrap itself.
pub fn esp_flash_command(platform: &Platform, artifact: &Path) -> Option<Vec<String>> {
    let rust = platform.rust.as_ref()?;
    let tool = rust.flash.as_deref().filter(|t| !t.trim().is_empty())?;
    let plan = esp_plan(
        platform,
        &BuildOptions {
            mode: "release".to_string(),
            ..Default::default()
        },
    )?;
    Some(vec![
        tool.to_string(),
        "flash".to_string(),
        "--chip".to_string(),
        plan.chip,
        artifact.to_string_lossy().to_string(),
    ])
}

/// `~/.rustup/toolchains/esp/bin` — where `espup` puts the toolchain whose `cargo` *and*
/// `rustc` must both be used.
///
/// Both matter: leaving cargo to find its own `rustc` silently picks the stable one, and
/// `-Zbuild-std` then fails with "the option `Z` is only accepted on the nightly compiler" —
/// an error about a flag, never about the toolchain that should have accepted it.
pub fn esp_toolchain_bin() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    esp_toolchain_bin_in(&PathBuf::from(home).join(".rustup").join("toolchains"))
}

/// [`esp_toolchain_bin`] against an explicit toolchains root, so it can be tested without
/// depending on the machine having espup installed (the same reason `device_platforms_in`
/// takes a directory).
pub fn esp_toolchain_bin_in(toolchains_root: &Path) -> Option<PathBuf> {
    let bin = toolchains_root.join(ESP_TOOLCHAIN).join("bin");
    bin.is_dir().then_some(bin)
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
#[derive(Debug, Default, Clone, Copy)]
pub struct EspBuildModule;

impl EspBuildModule {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Actor for EspBuildModule {
    type Message = BuildModuleMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "esp-idf".to_string(),
                    // Empty on purpose: routing is by platform, not by config file.
                    config_files: Vec::new(),
                    build_system: "Cargo (esp-idf)".to_string(),
                    language: "Rust".to_string(),
                    source_extensions: vec!["rs".to_string()],
                    mcp_servers: Vec::new(),
                    // Declared false so the manager refuses these *before* routing to us: a
                    // clean or lint here would run against the wrong target, which is worse
                    // than refusing.
                    supports_clean: false,
                    supports_lint: false,
                    supports_format: false,
                    supports_fix: false,
                });
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

/// Run an esp-idf build for `opts.platform`, through the **shared** process runner.
///
/// Reusing `run_build_spec` rather than spawning a `Command` here means environment
/// handling, duration measurement and exit-code reporting behave exactly as they do for
/// every other build module.
pub async fn run_esp_build(path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
    let platform_id = opts
        .platform
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "an esp-idf build needs a platform, e.g. \"esp32c6\"".to_string())?;
    let platform = Platform::from_registry(platform_id)
        .ok_or_else(|| format!("unknown platform '{platform_id}'"))?;
    let plan = esp_plan(&platform, opts)
        .ok_or_else(|| format!("platform '{platform_id}' is not an esp-idf platform"))?;

    crate::build::generic_helpers::run_build_spec(path, &spec_from_plan(plan)).await
}

/// The [`BuildSpec`] an [`EspPlan`] becomes, with the two environment requirements added.
///
/// Both are easy to miss and neither failure names its real cause, which is why they live
/// here rather than in a shell script someone has to remember to source.
pub(crate) fn spec_from_plan(plan: EspPlan) -> BuildSpec {
    spec_from_parts(
        plan,
        esp_toolchain_bin().as_deref(),
        libclang_path().as_deref(),
        std::env::var("PATH").ok().as_deref(),
    )
}

/// The conversion itself, with the three environment lookups passed in.
///
/// Split out so it can be tested deterministically: `esp_toolchain_bin` and `libclang_path`
/// read `$HOME`, so a test calling them would pass or fail on whether *this* machine happens
/// to have espup — failing for a reason unrelated to the logic under test.
pub(crate) fn spec_from_parts(
    plan: EspPlan,
    toolchain_bin: Option<&Path>,
    libclang: Option<&Path>,
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

    // Without this, bindgen inside esp-idf-sys fails with an error that never mentions bindgen.
    if let Some(lib) = libclang {
        env.push((
            "LIBCLANG_PATH".to_string(),
            lib.to_string_lossy().to_string(),
        ));
    }

    BuildSpec {
        command: "cargo".to_string(),
        arguments: plan.args,
        working_dir: String::new(),
        env,
    }
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
    #[test]
    fn flash_is_a_host_command_naming_the_chip() {
        let cmd = esp_flash_command(&c6(), Path::new("target/riscv32imac-esp-espidf/release/fw"))
            .expect("an esp platform with `flash` set flashes");
        assert_eq!(cmd[0], "espflash");
        assert_eq!(cmd[1], "flash");
        // espflash cannot always infer the chip, so it is named explicitly.
        assert_eq!(cmd[2], "--chip");
        assert_eq!(cmd[3], "esp32c6");
        assert!(cmd[4].ends_with("release/fw"), "{cmd:?}");
    }

    #[test]
    fn a_platform_without_a_flash_tool_has_no_flash_command() {
        let mut c6 = c6();
        c6.rust.as_mut().expect("rust").flash = None;
        assert!(esp_flash_command(&c6, Path::new("fw")).is_none());
    }

    /// The toolchain probes are filesystem checks, so the *logic* is tested against a
    /// directory rather than the machine's `$HOME` — which may not have espup at all, and a
    /// test that needed it would fail for the wrong reason.
    #[test]
    fn the_toolchain_and_libclang_are_discovered_from_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Not installed yet: no bin dir, no clang.
        assert!(esp_toolchain_bin_in(root).is_none());
        assert!(libclang_path_in(root).is_none());

        // The toolchain's bin dir is what makes it usable...
        std::fs::create_dir_all(root.join(ESP_TOOLCHAIN).join("bin")).unwrap();
        assert_eq!(
            esp_toolchain_bin_in(root),
            Some(root.join(ESP_TOOLCHAIN).join("bin"))
        );

        // ...while libclang stays unknown until the versioned clang dir exists: that version
        // is exactly what must not be hardcoded.
        let toolchain = root.join(ESP_TOOLCHAIN);
        assert!(libclang_path_in(&toolchain).is_none());
        let lib = toolchain
            .join("xtensa-esp32-elf-clang")
            .join("esp-20.1.1_20250829")
            .join("esp-clang")
            .join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        assert_eq!(libclang_path_in(&toolchain), Some(lib));
    }

    /// The invocation is only correct if these two are present, and *both* failures are
    /// silent about their cause — so they are pinned here rather than discovered on a board.
    #[test]
    fn the_spec_carries_mcu_the_toolchain_and_libclang() {
        let plan = esp_plan(&c6(), &BuildOptions::default()).expect("plan");
        let spec = spec_from_parts(
            plan,
            Some(Path::new("/home/x/.rustup/toolchains/esp/bin")),
            Some(Path::new("/home/x/clang/lib")),
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
    }

    /// And when they cannot be resolved, nothing is emitted for them: an empty `PATH` entry
    /// or a blank `LIBCLANG_PATH` would be worse than absent, because it would look
    /// deliberate and send the next reader hunting for a configuration mistake.
    #[test]
    fn the_spec_omits_environment_it_could_not_resolve() {
        let plan = esp_plan(&c6(), &BuildOptions::default()).expect("plan");
        let spec = spec_from_parts(plan, None, None, None);

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
    /// otherwise run against the wrong chip.
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
    }

    /// `run_esp_build`'s three refusals — the whole function minus the invocation.
    ///
    /// These are the paths that matter most for safety: each one declines *before* anything
    /// could be built for the wrong target, and each names what was wrong. No toolchain and no
    /// board is needed to prove them, which is exactly why they are worth proving.
    #[tokio::test]
    async fn run_esp_build_refuses_before_it_could_build_for_the_wrong_chip() {
        let _guard = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
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

        // 2. A real platform that is not esp-idf: refuse rather than build it as if it were.
        let linux = BuildOptions {
            platform: Some("rpi5".to_string()),
            ..Default::default()
        };
        let err = run_esp_build(Path::new("/tmp/does-not-matter"), &linux)
            .await
            .expect_err("a linux platform must not be built by this module");
        assert!(err.contains("not an esp-idf platform"), "{err}");

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
