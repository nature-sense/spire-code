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

use crate::build::BuildOptions;
use crate::platform::Platform;

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
}
