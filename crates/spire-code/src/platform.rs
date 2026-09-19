// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Platform registry — cross-compilation targets (rpi5, rock3c, a7s, …).
//!
//! A platform is a declarative description of one cross-compilation target: its
//! CPU architecture, toolchain, target sysroot, and (optionally) the board it
//! runs on. This module owns both the data types and the registry behavior:
//!
//! - YAML seed loading (`~/.spire/platforms/*.yaml`, `$SPIRE_PLATFORM_DIR` override)
//! - Meson/Cargo cross-file generation from a [`Platform`]
//! - The MCP config for a platform's board (`device.mcp`)
//!
//! The **graph is the canonical store** for platforms (each field stored as an
//! individual typed property on a Platform node); the YAML files are only the
//! seed used on startup. The startup phase reads the graph back into
//! [`set_registry`], and [`Platform::from_registry`] — the build path's lookup —
//! resolves from that view, so a build cannot disagree with what the graph holds.
//! The generic MCP client stays in `spire-core` — this module just builds a
//! [`spire_core::mcp::client::McpServerConfig`] from it.
//!
//! These types live here rather than in `spire-core` because the platform
//! concept is Spire's own: cross-compilation targets, their registry and their
//! boards have no consumer outside this crate.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The in-process view of the registry, populated **from the graph** at startup.
///
/// [`Platform::from_registry`] is synchronous and is called from deep inside the
/// build modules — a Cargo target's triple, a Meson cross file, a board's MCP
/// endpoint — so it cannot query the graph per call. The startup phase seeds this
/// once per process from the graph's own `Platform` nodes, and the graph is the
/// source of truth from then on. A process that never seeded it (a test, a bare
/// tool) falls back to reading the YAML seed directly.
static REGISTRY: OnceLock<RwLock<Vec<Platform>>> = OnceLock::new();

/// Replace the in-process registry with the platforms read back from the graph.
pub fn set_registry(platforms: Vec<Platform>) {
    let lock = REGISTRY.get_or_init(|| RwLock::new(Vec::new()));
    *lock
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = platforms;
}

/// The platforms the graph holds, when it has been seeded with any.
fn graph_registry() -> Option<Vec<Platform>> {
    let lock = REGISTRY.get()?;
    let platforms = lock
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    (!platforms.is_empty()).then_some(platforms)
}

// ============================================================================
// Platform definitions — cross-compilation targets (rpi5, rock3c, …)
// ============================================================================

/// A cross-compilation platform target (e.g. Raspberry Pi 5, Rock 3C).
///
/// The graph is the canonical store for these; the YAML files under
/// `~/.spire/platforms/*.yaml` are only the seed used on startup. Every
/// property is stored as an individual typed key on a `Platform` graph node
/// (never a JSON fragment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    /// Stable identifier, also the graph node name (e.g. "rpi5", "rock3c").
    pub id: String,
    pub name: String,
    /// Discriminator: "linux" today; "esp-idf", "rp2040", "none", … later.
    pub os: String,
    pub architecture: PlatformArchitecture,
    /// The C cross-toolchain.
    ///
    /// Optional in the YAML on purpose: a **Rust** target (`os: "esp-idf"`) has no C
    /// toolchain, and forcing one to carry dummy compilers would be a trap for the model
    /// filling the file in. Defaults to the `clang`/`llvm-*` set. A C target that omits it
    /// is still gated by [`Platform::sysroot_ok`], so this adds permission, not risk.
    #[serde(default)]
    pub toolchain: PlatformToolchain,
    /// Target sysroot — the root file system of the target machine.
    ///
    /// Optional for the same reason, and *gated* rather than trusted: an empty or
    /// unpopulated root is refused for a cross-build by [`Platform::sysroot_ok`].
    #[serde(default)]
    pub sysroot: PlatformSysroot,
    /// Optional on-hardware access for this target: the board's MCP endpoint
    /// (the `spire-target-mcp` server) and where artifacts land on it. Absent
    /// for host-only targets, which have no board to talk to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<PlatformDevice>,
    /// Board **family** this variant belongs to (`esp32`, `rp2040`, …).
    ///
    /// Grouping only. One backend crate serves a whole family (`spire-hal-esp32` builds
    /// for esp32/esp32s3/esp32c6 through cargo features), but the unit of *compilation* is
    /// always the **variant**, because the target triple is: esp32c6 ≠ esp32 in exactly the
    /// way rpi5 ≠ rock3c.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// The **chip** this board carries (`esp32s3`, `rp2040`, `esp32-pico-d4`).
    ///
    /// Absent for a Linux SBC, whose entry *is* its board. Present on a bare-metal
    /// board, where it selects the chip's build facts — the stock triple, the vendor
    /// HAL, the flash tool. The registry holds **boards**, never generic silicon: this
    /// is how a board says which silicon it is, and what `add_bsp` resolves instead of
    /// a chip-name table in Rust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chip: Option<String>,
    /// Rust toolchain, for targets whose build is not a C cross-compile (`os: "esp-idf"`).
    ///
    /// Absent for the C platforms, whose toolchain is [`PlatformToolchain`]. Kept separate
    /// rather than as more optional fields on that struct: an ESP32 target has no `c`/`cpp`/
    /// `ar`, and pretending it does would put dummy compilers in a YAML for the model to
    /// trip over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust: Option<PlatformRust>,
    /// Free-text notes about this platform's SDKs, drivers and constraints, used to **focus**
    /// a generated implementation.
    ///
    /// Already present in the registry YAML (`library_hints:`) and already fed to the C++ HAL
    /// implementation prompt. Typed here because the other two consumers need it *from the
    /// platform* rather than from a second parse of the YAML: the create-project wizard shows
    /// it while a platform is chosen, and the Rust HAL fill prompt injects it to constrain
    /// what an `impl` is allowed to use. Free-form on purpose — it is guidance for a model,
    /// not a schema, and pinning prose to a shape would only make it worse to write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_hints: Option<String>,
}

/// How Spire reaches a board to run things on it (`device:` in the platform
/// YAML). Every part is optional, so a platform declares only what it has.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformDevice {
    /// MCP endpoint served by the board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<PlatformDeviceMcp>,
    /// Where build artifacts (e.g. test binaries) are deployed on the board.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<PlatformDeploy>,
}

impl PlatformDevice {
    /// True when this device declares a usable MCP endpoint.
    pub fn has_mcp(&self) -> bool {
        self.mcp
            .as_ref()
            .map(|mcp| !mcp.url.trim().is_empty())
            .unwrap_or(false)
    }
}

/// The Rust toolchain for a non-C target (`os: "esp-idf"`).
///
/// Every field here is a **compile-time** fact, which is why a variant is a distinct
/// platform: the triple selects the rustup target, and `esp32` vs `esp32c6` also differ in
/// architecture family (Xtensa vs RISC-V), so this is not a runtime switch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformRust {
    /// The rustup target triple, e.g. `riscv32imac-esp-espidf`.
    ///
    /// Espressif spells this differently from the IDF target, so both are carried rather
    /// than one being derived from the other.
    pub target: String,
    /// The vendor build target, e.g. `esp32c6` — which is both `IDF_TARGET` and the
    /// `esp-idf-hal` cargo feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idf_target: Option<String>,
    /// How the artifact reaches the board **over USB** (`espflash`, `idf.py`).
    ///
    /// A host-side step on purpose: the network MCP leg is not viable for a board that is
    /// not yet running anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flash: Option<String>,
}

/// The board's MCP endpoint (`device.mcp`) — a `spire-target-mcp` server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformDeviceMcp {
    /// Streamable HTTP URL, e.g. `http://rpi5.local:8737/mcp`.
    pub url: String,
    /// Optional bearer token, sent as `Authorization: Bearer <token>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Where artifacts are deployed on the board (`device.deploy`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformDeploy {
    /// Destination directory on the board, e.g. `/home/pi/ai-traps`.
    pub dest: String,
}

/// CPU architecture of the target platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformArchitecture {
    /// Meson cpu_family: arm | aarch64 | x86_64 …
    pub cpu_family: String,
    /// Meson cpu: armv8 | armv8-a …
    pub cpu: String,
    pub endian: String,
    /// clang/gcc triple, e.g. aarch64-linux-gnu.
    pub target_triple: String,
    /// Optional -march= value (e.g. "armv8.2-a+crc"); appended to c/cpp/link.
    #[serde(default)]
    pub march: Option<String>,
}

/// Toolchain binaries and extra flags for the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformToolchain {
    pub c: String,
    pub cpp: String,
    pub ar: String,
    pub strip: String,
    /// Optional linker (ld) — when present enables -fuse-ld=lld and [binaries] ld.
    #[serde(default)]
    pub ld: Option<String>,
    #[serde(default)]
    pub pkgconfig: Option<String>,
    /// Verbatim extra C compiler flags (${SYSROOT} is substituted).
    #[serde(default)]
    pub c_args_extra: Vec<String>,
    /// Verbatim extra C++ compiler flags (${SYSROOT} is substituted).
    #[serde(default)]
    pub cpp_args_extra: Vec<String>,
    /// Verbatim extra linker flags (${SYSROOT} is substituted).
    #[serde(default)]
    pub linker_args_extra: Vec<String>,
    #[serde(default)]
    pub needs_exe_wrapper: bool,
}

impl Default for PlatformToolchain {
    fn default() -> Self {
        Self {
            c: "clang".into(),
            cpp: "clang++".into(),
            ar: "llvm-ar".into(),
            strip: "llvm-strip".into(),
            ld: None,
            pkgconfig: None,
            c_args_extra: Vec::new(),
            cpp_args_extra: Vec::new(),
            linker_args_extra: Vec::new(),
            needs_exe_wrapper: false,
        }
    }
}

/// Target sysroot — the root file system of the target machine used for
/// cross-linking headers/libraries.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlatformSysroot {
    pub root: String,
    #[serde(default)]
    pub lib_dirs: Vec<String>,
    #[serde(default)]
    pub include_dirs: Vec<String>,
    #[serde(default)]
    pub pkg_config_libdir: Vec<String>,
}
/// The ${SYSROOT} placeholder substituted with `sysroot.root` in arg lists.
const SYSROOT_TOKEN: &str = "${SYSROOT}";

/// Whether a registry entry names a **board** or a **chip**.
///
/// The two are not interchangeable: a Linux SBC entry names the *board* it is
/// built for (the Pi 5's arch and sysroot), while a bare-metal entry names the
/// *processor* (`esp32c3`, `rp2040`) and currently carries that board's facts as
/// `library_hints`. Showing them as one flat list is what makes the platforms
/// screen read as a mixture of two different kinds of thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    Board,
    Chip,
}

impl Platform {
    /// True for a board a firmware project targets; false for a host or a Linux cross-target.
    ///
    /// Keyed on `os` rather than a new field, because `os` is what the *build* already keys on
    /// (an esp-idf build is not a C cross-compile; an rp2040 one has no OS at all), and a
    /// second taxonomy could only disagree with the first. It is what the create-project
    /// wizard filters on to offer embedded platforms for the embedded-HAL project type.
    pub fn is_embedded(&self) -> bool {
        // The `os` is the *runtime*, which is what the build keys on — and there are three of those
        // here: ESP-IDF (`std`, one FreeRTOS task per thread), `esp-hal` (bare metal, `no_std`, the
        // embassy executor), and `rp2040` (bare metal, ARM). The two bare-metal spellings are named
        // separately because they are different toolchains and different crate sets, which is exactly
        // what a build routing decision needs to tell apart.
        matches!(self.os.as_str(), "esp-idf" | "esp-hal" | "rp2040")
    }

    /// The build facts for a chip id, from the chip store.
    ///
    /// `None` when the chip is unknown, which is a **refusal, not a default**: a board
    /// naming a chip nobody has described cannot be built, and inventing a triple is
    /// how you build for the wrong silicon.
    pub fn chip_facts(id: &str) -> Option<Platform> {
        Self::chip_facts_in(Self::default_chip_dir(), id)
    }

    /// [`Self::chip_facts`] against an explicit directory, so the lookup is testable
    /// without the process-global `SPIRE_CHIP_DIR`.
    pub fn chip_facts_in(dir: impl AsRef<Path>, id: &str) -> Option<Platform> {
        Self::load_directory(dir)
            .ok()?
            .into_iter()
            .find(|chip| chip.id == id)
    }

    /// Which kind of thing this entry names — the axis the platforms list groups by.
    ///
    /// The inverse of [`Self::is_embedded`], and deliberately the *only* rule: an
    /// embedded entry (`esp-idf`/`esp-hal`/`rp2040`) names the **processor**, while
    /// everything else — a Linux SBC's arch+sysroot, or the host — is a **board**
    /// entry. A second predicate here would be a second taxonomy, free to disagree
    /// with the one the build keys on. The store split (`boards/` + `targets/`, a
    /// board naming its chip) turns this into a declared field, and only this method
    /// changes when it does.
    pub fn kind(&self) -> PlatformKind {
        if self.is_embedded() {
            PlatformKind::Chip
        } else {
            PlatformKind::Board
        }
    }

    /// The chip this entry is for: the one it declares, or — for an entry that still
    /// names silicon rather than a board — its own id, so a chip-shaped seed keeps
    /// resolving while the store converts.
    pub fn chip_id(&self) -> &str {
        self.chip.as_deref().unwrap_or(&self.id)
    }

    /// Load a platform definition from a YAML file.
    pub fn load(path: impl AsRef<Path>) -> Result<Platform> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read platform YAML: {}", path.display()))?;
        let platform: Platform = serde_yaml::from_str(&raw)
            .with_context(|| format!("failed to parse platform YAML: {}", path.display()))?;
        Ok(platform)
    }

    /// Load every platform definition from a directory of `.yaml` files.
    pub fn load_directory(dir: impl AsRef<Path>) -> Result<Vec<Platform>> {
        let dir = dir.as_ref();
        let mut out = Vec::new();
        let mut entries = fs::read_dir(dir)
            .with_context(|| format!("failed to read platform dir: {}", dir.display()))?
            .collect::<Result<Vec<_>, _>>()
            .context("read_dir iterator failed")?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let is_yaml = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "yaml" || e == "yml")
                .unwrap_or(false);
            if !is_yaml {
                continue;
            }
            match Platform::load(&path) {
                Ok(p) => out.push(p),
                Err(e) => {
                    tracing::warn!("skipping invalid platform file {}: {}", path.display(), e);
                }
            }
        }
        Ok(out)
    }

    /// True/OK when no cross-compilation is needed (empty sysroot root = native
    /// host platform) OR the configured `sysroot.root` exists on disk as a
    /// populated target root (contains `usr/`). Used to fail fast in
    /// Meson/Cargo cross-build setup instead of emitting `--sysroot=<missing>`
    /// and letting the tool fail late with a confusing error.
    pub fn sysroot_ok(&self) -> (bool, String) {
        let root = self.sysroot.root.trim();
        if root.is_empty() {
            // Host / non-linux platform: no cross sysroot required.
            return (true, String::new());
        }
        let p = Path::new(root);
        if !p.is_dir() {
            return (false, format!("sysroot root is not a directory: {root}"));
        }
        if !p.join("usr").is_dir() {
            return (
                false,
                format!(
                    "sysroot at {root} is not populated (missing {}/usr)",
                    p.display()
                ),
            );
        }
        (true, String::new())
    }

    /// Discover the **chip-facts** directory: `$SPIRE_CHIP_DIR` or the
    /// application-scoped `~/.spire/<app>/chips`.
    ///
    /// A board declares `chip:`; this is where that id's build facts live — the stock
    /// triple, the vendor HAL, the flash tool. Separate from the board list because a
    /// chip is not a thing a picker offers: it is what a board *is*.
    pub fn default_chip_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("SPIRE_CHIP_DIR") {
            if !dir.trim().is_empty() {
                return PathBuf::from(dir);
            }
        }
        spire_core::config::config_dir().join("chips")
    }

    /// Discover the seed platform directory: `$SPIRE_PLATFORM_DIR` (for
    /// tests/CI/containers) or the application-scoped
    /// `~/.spire/<app>/platforms`.
    ///
    /// The scope is the same one `spire-core::config` uses, so one application's
    /// board catalogue cannot surface in another's.
    pub fn default_platform_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("SPIRE_PLATFORM_DIR") {
            if !dir.trim().is_empty() {
                return PathBuf::from(dir);
            }
        }
        spire_core::config::config_dir().join("platforms")
    }

    /// Load a single platform by id.
    ///
    /// The **graph** is the registry: the startup phase seeds this process from
    /// the graph's `Platform` nodes and that view wins. The YAML seed
    /// (`$SPIRE_PLATFORM_DIR` or `~/.spire/<app>/platforms/*.yaml`) is only the
    /// fallback for a process that has not seeded a graph.
    pub fn from_registry(id: &str) -> Option<Platform> {
        Self::resolve(id, graph_registry().as_deref())
    }

    /// [`Self::from_registry`] with the graph-derived view passed in, so the
    /// selection is testable without a process-global.
    ///
    /// An id the graph does *not* hold resolves to `None` rather than being
    /// looked for in the seed: once the graph exists it is authoritative, and
    /// silently falling through would hide a platform the graph dropped.
    fn resolve(id: &str, from_graph: Option<&[Platform]>) -> Option<Platform> {
        if let Some(platforms) = from_graph {
            return platforms.iter().find(|p| p.id == id).cloned();
        }
        let dir = Self::default_platform_dir();
        let platforms = Self::load_directory(&dir).ok()?;
        platforms.into_iter().find(|p| p.id == id)
    }

    /// Graph/MCP name of this platform's device server, e.g. `device-rpi5`.
    ///
    /// One stable name per platform keeps the MCP server list (and the tools it
    /// contributes) greppable. Platforms with no device endpoint have no name.
    pub fn device_server_name(&self) -> Option<String> {
        self.device
            .as_ref()?
            .has_mcp()
            .then(|| format!("device-{}", self.id))
    }

    /// MCP client config for this platform's board, when it declares one.
    ///
    /// `autostart: false` on purpose. Boards are frequently powered off, and the
    /// MCP client's `ConnectAll` blocks for up to 15s per unreachable server —
    /// so an absent board must never stall startup. The host registers the
    /// server cheaply and connects it deliberately instead (project open spawns
    /// a bounded background connect; see the coordinator's `device/` handlers).
    pub fn device_mcp_config(&self) -> Option<spire_core::mcp::client::McpServerConfig> {
        let mcp = self.device.as_ref()?.mcp.as_ref()?;
        let url = mcp.url.trim();
        if url.is_empty() {
            return None;
        }

        let mut headers = std::collections::HashMap::new();
        if let Some(token) = mcp
            .token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }

        Some(spire_core::mcp::client::McpServerConfig {
            name: self.device_server_name()?,
            transport: spire_core::mcp::client::TransportConfig::Http {
                url: url.to_string(),
                headers,
            },
            autostart: false,
            build_type: None,
        })
    }

    /// Every registered platform that declares a device MCP endpoint
    /// (`device.mcp.url`), in registry order.
    pub fn device_platforms() -> Vec<Platform> {
        Self::device_platforms_in(Self::default_platform_dir())
    }

    /// Every platform in `dir` that declares a device MCP endpoint.
    pub fn device_platforms_in(dir: impl AsRef<Path>) -> Vec<Platform> {
        Self::load_directory(dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|platform| platform.device_server_name().is_some())
            .collect()
    }

    /// Substitute `${SYSROOT}` in a single arg with the sysroot root.
    fn substitute(&self, arg: &str) -> String {
        arg.replace(SYSROOT_TOKEN, &self.sysroot.root)
    }

    fn substituted(&self, args: &[String]) -> Vec<String> {
        args.iter().map(|a| self.substitute(a)).collect()
    }

    /// Render Cargo's `.cargo/config.toml` for cross-compiling a pure-Rust
    /// project to this platform. Returns `None` for non-`linux` platforms.
    pub fn cargo_config(&self) -> Option<String> {
        use std::fmt::Write;
        if self.os != "linux" {
            return None;
        }
        let triple = &self.architecture.target_triple;
        let sysroot = &self.sysroot.root;

        let mut s = String::new();
        let _ = writeln!(s, "[target.{}]", triple);
        let _ = writeln!(s, "linker = \"{}\"", self.toolchain.c);
        let _ = writeln!(
            s,
            "rustflags = [\"-C\", \"link-arg=--target={triple}\",\n  \"-C\", \"link-arg=--sysroot={sysroot}\"]",
        );
        let _ = writeln!(s);
        let _ = writeln!(s, "[env]");
        let _ = writeln!(s, "PKG_CONFIG_SYSROOT_DIR = \"{sysroot}\"");
        if !self.sysroot.pkg_config_libdir.is_empty() {
            let joined = self
                .sysroot
                .pkg_config_libdir
                .iter()
                .map(|p| self.substitute(p))
                .collect::<Vec<_>>()
                .join(":");
            let _ = writeln!(s, "PKG_CONFIG_LIBDIR = \"{joined}\"");
        }
        let _ = writeln!(s, "CC_{} = \"{}\"", triple.to_uppercase(), self.toolchain.c);
        let _ = writeln!(
            s,
            "CXX_{} = \"{}\"",
            triple.to_uppercase(),
            self.toolchain.cpp
        );
        let _ = writeln!(
            s,
            "AR_{} = \"{}\"",
            triple.to_uppercase(),
            self.toolchain.ar
        );
        Some(s)
    }

    /// Render this platform as a Meson cross file. Returns `None` for
    /// non-`linux` platforms (e.g. esp-idf/rp2040, which use a different
    /// toolchain model) — those are handled by `cmake_toolchain_args` later.
    pub fn meson_cross_file(&self) -> Option<String> {
        if self.os != "linux" {
            return None;
        }
        let triple = &self.architecture.target_triple;
        let sysroot = &self.sysroot.root;
        let sysroot_arg = format!("--sysroot={}", sysroot);

        // Implicit target args: -target <triple> + --sysroot + optional march.
        let mut target_args = vec!["-target".to_string(), triple.clone(), sysroot_arg.clone()];
        if let Some(march) = &self.architecture.march {
            target_args.push(format!("-march={}", march));
        }

        let mut c_args = target_args.clone();
        c_args.extend(self.substituted(&self.toolchain.c_args_extra));
        let mut cpp_args = target_args.clone();
        cpp_args.extend(self.substituted(&self.toolchain.cpp_args_extra));

        let mut link_args = vec!["-target".to_string(), triple.clone(), sysroot_arg.clone()];
        if let Some(ld) = &self.toolchain.ld {
            if ld.ends_with("lld") {
                link_args.push("-fuse-ld=lld".to_string());
            }
        }
        if let Some(march) = &self.architecture.march {
            link_args.push(format!("-march={}", march));
        }
        link_args.extend(self.substituted(&self.toolchain.linker_args_extra));

        let quote = |s: &str| format!("'{}'", s);
        let fmt_list = |paths: &[String]| {
            let items = paths
                .iter()
                .map(|p| self.substitute(p))
                .map(|p| format!("'{}'", p))
                .collect::<Vec<_>>()
                .join(", ");
            if items.is_empty() {
                "[]".to_string()
            } else {
                format!("[{}]", items)
            }
        };

        let mut s = String::new();
        s.push_str("[host_machine]\n");
        s.push_str(&format!("system = '{}'\n", self.os));
        s.push_str(&format!(
            "cpu_family = '{}'\n",
            self.architecture.cpu_family
        ));
        s.push_str(&format!("cpu = '{}'\n", self.architecture.cpu));
        s.push_str(&format!("endian = '{}'\n", self.architecture.endian));
        s.push('\n');
        s.push_str("[target_machine]\n");
        s.push_str(&format!("system = '{}'\n", self.os));
        s.push_str(&format!(
            "cpu_family = '{}'\n",
            self.architecture.cpu_family
        ));
        s.push_str(&format!("cpu = '{}'\n", self.architecture.cpu));
        s.push_str(&format!("endian = '{}'\n", self.architecture.endian));
        s.push('\n');
        s.push_str("[binaries]\n");
        s.push_str(&format!("c = {}\n", quote(&self.toolchain.c)));
        s.push_str(&format!("cpp = {}\n", quote(&self.toolchain.cpp)));
        s.push_str(&format!("ar = {}\n", quote(&self.toolchain.ar)));
        s.push_str(&format!("strip = {}\n", quote(&self.toolchain.strip)));
        if let Some(ld) = &self.toolchain.ld {
            s.push_str(&format!("ld = {}\n", quote(ld)));
        }
        if let Some(pkg) = &self.toolchain.pkgconfig {
            s.push_str(&format!("pkgconfig = {}\n", quote(pkg)));
        }
        s.push('\n');
        s.push_str("[built-in options]\n");
        s.push_str(&format!("c_args = {}\n", fmt_list(&c_args)));
        s.push_str(&format!("c_link_args = {}\n", fmt_list(&link_args)));
        s.push_str(&format!("cpp_args = {}\n", fmt_list(&cpp_args)));
        s.push_str(&format!("cpp_link_args = {}\n", fmt_list(&link_args)));
        s.push('\n');
        s.push_str("[properties]\n");
        s.push_str(&format!("sys_root = '{}'\n", sysroot));
        s.push_str(&format!(
            "needs_exe_wrapper = {}\n",
            self.toolchain.needs_exe_wrapper
        ));
        if !self.sysroot.lib_dirs.is_empty() {
            s.push_str(&format!(
                "lib_dirs = {}\n",
                fmt_list(&self.sysroot.lib_dirs)
            ));
        }
        if !self.sysroot.pkg_config_libdir.is_empty() {
            s.push_str(&format!(
                "pkg_config_libdir = {}\n",
                fmt_list(&self.sysroot.pkg_config_libdir)
            ));
        }
        Some(s)
    }
}

impl From<&Platform> for PlatformToolchain {
    fn from(p: &Platform) -> Self {
        p.toolchain.clone()
    }
}

/// Pre-rendered cross-compilation settings for one platform id, computed once
/// so Cargo/Meson/Swift modules share a single lookup instead of each calling
/// `Platform::from_registry` + re-deriving the triple / config fragments.
#[derive(Debug, Clone, Default)]
pub struct CrossSpec {
    /// Rendered `.cargo/config.toml` content (None for host/unknown/non-linux).
    pub cargo_config: Option<String>,
    /// Rendered Meson cross file content (None for host/unknown/non-linux).
    pub meson_cross_file: Option<String>,
    /// The target triple (e.g. "aarch64-linux-gnu"). Empty for host/unknown.
    pub target_triple: String,
}

impl CrossSpec {
    /// Resolve a platform id (e.g. "rpi5", "rock3c") to its pre-rendered
    /// cross-compilation spec. Returns `None` for "host", unknown ids, or
    /// platforms without a rendered config (non-linux).
    pub fn for_platform(id: &str) -> Option<CrossSpec> {
        let platform = Platform::from_registry(id)?;
        Some(CrossSpec {
            cargo_config: platform.cargo_config(),
            meson_cross_file: platform.meson_cross_file(),
            target_triple: platform.architecture.target_triple.clone(),
        })
    }
}
/// Set `SPIRE_PLATFORM_DIR` for a test and restore it on drop.
///
/// The variable is process-global, so a test using this must hold
/// [`crate::PLATFORM_DIR_TEST_LOCK`] for its whole body; the guard only owns the value, which is
/// the part that is easy to forget to restore. A hermetic platform registry is how a test can
/// name a family the machine does not have (an rp2040 today) without depending on the user's
/// `~/.spire/platforms`.
#[cfg(test)]
pub(crate) struct PlatformDirGuard {
    previous: Option<String>,
}

#[cfg(test)]
impl PlatformDirGuard {
    pub(crate) fn set(dir: impl AsRef<Path>) -> Self {
        let previous = std::env::var("SPIRE_PLATFORM_DIR").ok();
        std::env::set_var("SPIRE_PLATFORM_DIR", dir.as_ref());
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for PlatformDirGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(prev) => std::env::set_var("SPIRE_PLATFORM_DIR", prev),
            None => std::env::remove_var("SPIRE_PLATFORM_DIR"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_yaml(dir: &Path, name: &str, content: &str) {
        fs::create_dir_all(dir).unwrap();
        let mut f = fs::File::create(dir.join(name)).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    /// A **complete** platform YAML. `Platform` has required `architecture`,
    /// `toolchain` and `sysroot`, and `load_directory` skips a file that does not
    /// parse — so a bare `id`/`name`/`os` fixture loads as *nothing*.
    fn platform_yaml(id: &str, name: &str) -> String {
        format!(
            "id: {id}\nname: {name}\nos: linux\n\
             architecture:\n  cpu_family: aarch64\n  cpu: armv8-a\n  endian: little\n  \
             target_triple: aarch64-linux-gnu\n\
             toolchain:\n  c: clang\n  cpp: clang++\n  ar: ar\n  strip: strip\n  ld: ld.lld\n  \
             pkgconfig: pkg-config\n\
             sysroot:\n  root: /tmp/sysroot/{id}\n"
        )
    }

    /// The grouping axis: a Linux SBC entry **is** its board, a bare-metal entry
    /// names the processor. Kept here rather than re-derived in the client, so the
    /// screen and the wizard cannot disagree about which is which.
    #[test]
    fn kind_separates_linux_boards_from_bare_metal_chips() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(tmp.path(), "rpi5.yaml", &platform_yaml("rpi5", "Pi 5"));
        write_yaml(
            tmp.path(),
            "esp32c3.yaml",
            &platform_yaml("esp32c3", "ESP32-C3").replace("os: linux", "os: esp-hal"),
        );
        let by_id: std::collections::HashMap<String, Platform> =
            Platform::load_directory(tmp.path())
                .unwrap()
                .into_iter()
                .map(|p| (p.id.clone(), p))
                .collect();
        assert_eq!(by_id.len(), 2, "both fixtures must load");

        assert_eq!(by_id["rpi5"].kind(), PlatformKind::Board);
        assert_eq!(by_id["esp32c3"].kind(), PlatformKind::Chip);
    }

    /// A board's `chip:` resolves to that chip's facts, and an unknown chip is a
    ///**refusal** rather than a guess.
    ///
    /// `chip_facts_in` takes the store explicitly, so this needs no `SPIRE_CHIP_DIR`
    /// guard and cannot leak into a parallel test.
    #[test]
    fn a_chips_facts_resolve_by_id_and_an_unknown_chip_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "esp32s3.yaml",
            &platform_yaml("esp32s3", "ESP32-S3").replace("os: linux", "os: esp-hal"),
        );

        let facts = Platform::chip_facts_in(tmp.path(), "esp32s3").expect("the chip resolves");
        assert_eq!(facts.id, "esp32s3");
        assert!(facts.is_embedded(), "a chip is bare-metal");
        assert_eq!(facts.chip_id(), "esp32s3", "a chip resolves to itself");
        assert!(
            Platform::chip_facts_in(tmp.path(), "esp32c9").is_none(),
            "an unknown chip is a refusal, not a guessed triple"
        );
    }

    /// A board declares the chip it carries, and the declaration is what resolves.
    ///
    /// The fallback to the entry's own id is what lets the store convert one entry at
    /// a time: a chip-shaped seed (`esp32c3`, no `chip:`) keeps resolving to itself
    /// until its board names it.
    #[test]
    fn a_board_declares_the_chip_it_carries() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "m5stack-core-s3.yaml",
            &platform_yaml("m5stack-core-s3", "M5Stack Core S3")
                .replace("os: linux", "os: esp-hal\nchip: esp32s3"),
        );
        write_yaml(
            tmp.path(),
            "esp32c3.yaml",
            &platform_yaml("esp32c3", "ESP32-C3"),
        );
        let all = Platform::load_directory(tmp.path()).unwrap();
        let board = all.iter().find(|p| p.id == "m5stack-core-s3").unwrap();
        let chip_shaped = all.iter().find(|p| p.id == "esp32c3").unwrap();

        assert_eq!(board.chip_id(), "esp32s3", "the declaration wins");
        assert_eq!(
            chip_shaped.chip_id(),
            "esp32c3",
            "an entry that names silicon resolves to itself"
        );
    }

    /// The graph-held view wins, and an id it does not hold is *not* looked for in
    /// the seed — once the graph exists it is authoritative.
    ///
    /// `resolve` takes the view as an argument rather than reading the
    /// process-global on purpose: a test that called `set_registry` would leak into
    /// every other test's `from_registry`, and the runner is parallel.
    #[test]
    fn resolve_prefers_the_graph_view_over_the_yaml_seed() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "graph-only.yaml",
            &platform_yaml("graph-only", "From the graph"),
        );
        let graph = Platform::load_directory(tmp.path()).unwrap();
        assert_eq!(graph.len(), 1, "the fixture must load as one platform");

        assert_eq!(
            Platform::resolve("graph-only", Some(&graph)).map(|p| p.name),
            Some("From the graph".to_string())
        );
        assert!(
            Platform::resolve("rpi5", Some(&graph)).is_none(),
            "a graph that exists is authoritative, not a fallback to the seed"
        );
    }

    /// Without a graph, the YAML seed is the registry — a bare tool, or a test.
    #[test]
    fn resolve_falls_back_to_the_yaml_seed_without_a_graph() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(tmp.path(), "rpi5.yaml", &platform_yaml("rpi5", "Rasp Pi"));
        let _guard = crate::platform::PlatformDirGuard::set(tmp.path());

        assert_eq!(
            Platform::resolve("rpi5", None).map(|p| p.name),
            Some("Rasp Pi".to_string())
        );
    }

    #[test]
    fn load_cubie_a7s_platform_from_user_registry() {
        // The real user-level seed (write by the platform tooling). Kept as a
        // smoke test so an invalid a7s.yaml fails loudly at test time.
        let path = std::path::Path::new(std::env::var("HOME").unwrap_or_default().as_str())
            .join(".spire")
            .join("platforms")
            .join("a7s.yaml");
        if !path.exists() {
            eprintln!("skipping: {} not present", path.display());
            return;
        }
        let p = Platform::load(&path).expect("a7s platform YAML must parse");
        assert_eq!(p.id, "a7s");
        assert_eq!(p.name, "Cubie A7S");
        assert_eq!(p.os, "linux");
        assert_eq!(p.architecture.cpu_family, "aarch64");
        assert_eq!(p.architecture.target_triple, "aarch64-linux-gnu");
        assert_eq!(p.architecture.march.as_deref(), Some("armv8.2-a+crc"));
        // The linker path is machine-specific (Homebrew vs cross-toolchain);
        // only assert that a linker is declared.
        assert!(p.toolchain.ld.is_some(), "a7s must declare a linker");
        // The sysroot path is machine-specific ("sysroots/a7s" or
        // "/opt/cross/sysroot/cubie-a7s"); assert it references the platform.
        assert!(p.sysroot.root.contains("a7s"), "sysroot must reference a7s");
        // The SYSROOT token must be substituted when rendering the meson cross
        // file. The C++ standard-library version is machine-specific (GCC 10 vs
        // 12 depending on which sysroot is installed), so assert the
        // substitution itself rather than a pinned version.
        let cross = p.meson_cross_file().expect("linux cross file");
        assert!(
            cross.contains(&format!("-I{}/usr/include/c++/", p.sysroot.root)),
            "cpp include must substitute the SYSROOT token: {cross}"
        );
    }

    #[test]
    fn load_rock3c_platform_from_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "rock3c.yaml",
            r#"
id: rock3c
name: Rock 3C
os: linux
architecture:
  cpu_family: aarch64
  cpu: armv8-a
  endian: little
  target_triple: aarch64-linux-gnu
  march: armv8.2-a+crc
toolchain:
  c: clang
  cpp: clang++
  ar: /opt/homebrew/opt/llvm/bin/llvm-ar
  strip: /opt/homebrew/opt/llvm/bin/llvm-strip
  ld: /opt/homebrew/bin/ld.lld
  pkgconfig: /Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/bin/aarch64-pkg-config
sysroot:
  root: /Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c
  lib_dirs:
    - ${SYSROOT}/usr/lib/aarch64-linux-gnu
  pkg_config_libdir:
    - ${SYSROOT}/usr/lib/aarch64-linux-gnu/pkgconfig
    - ${SYSROOT}/usr/share/pkgconfig
"#,
        );

        let p = Platform::load(tmp.path().join("rock3c.yaml")).unwrap();
        assert_eq!(p.id, "rock3c");
        assert_eq!(p.os, "linux");
        assert_eq!(p.architecture.target_triple, "aarch64-linux-gnu");
        assert_eq!(p.architecture.march.as_deref(), Some("armv8.2-a+crc"));
        assert_eq!(p.toolchain.ld.as_deref(), Some("/opt/homebrew/bin/ld.lld"));
        assert_eq!(
            p.sysroot.lib_dirs,
            vec!["${SYSROOT}/usr/lib/aarch64-linux-gnu".to_string()]
        );
    }

    #[test]
    fn meson_cross_file_for_linux() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "rock3c.yaml",
            r#"
id: rock3c
name: Rock 3C
os: linux
architecture:
  cpu_family: aarch64
  cpu: armv8-a
  endian: little
  target_triple: aarch64-linux-gnu
  march: armv8.2-a+crc
toolchain:
  c: clang
  cpp: clang++
  ar: /opt/homebrew/opt/llvm/bin/llvm-ar
  strip: /opt/homebrew/opt/llvm/bin/llvm-strip
  ld: /opt/homebrew/bin/ld.lld
  pkgconfig: /Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/bin/aarch64-pkg-config
  cpp_args_extra:
    - -I${SYSROOT}/usr/include/c++/12
    - -I${SYSROOT}/usr/include/aarch64-linux-gnu/c++/12
    - -I${SYSROOT}/usr/include
sysroot:
  root: /Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c
  lib_dirs:
    - ${SYSROOT}/usr/lib/aarch64-linux-gnu
  pkg_config_libdir:
    - ${SYSROOT}/usr/lib/aarch64-linux-gnu/pkgconfig
    - ${SYSROOT}/usr/share/pkgconfig
"#,
        );

        let p = Platform::load(tmp.path().join("rock3c.yaml")).unwrap();
        let cross = p.meson_cross_file().expect("linux platform cross file");

        // Target args with -target + sysroot + march on c_args.
        assert!(
            cross.contains("cpu_family = 'aarch64'"),
            "missing cpu_family"
        );
        assert!(cross.contains("system = 'linux'"), "missing os");
        assert!(cross.contains("-target"), "missing -target");
        assert!(
            cross.contains(
                "--sysroot=/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c"
            ),
            "missing sysroot"
        );
        assert!(cross.contains("-march=armv8.2-a+crc"), "missing march");
        assert!(cross.contains("-fuse-ld=lld"), "missing lld fuse");
        assert!(cross.contains("pkgconfig = '/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/bin/aarch64-pkg-config'"), "missing pkgconfig");
        // ${SYSROOT} substitution in cpp_args_extra.
        assert!(
            cross.contains("-I/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/usr/include/c++/12"),
            "missing substituted cpp include"
        );
        assert!(
            cross.contains(
                "sys_root = '/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c'"
            ),
            "missing sys_root"
        );
        assert!(
            cross.contains("pkg_config_libdir = ['/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/usr/lib/aarch64-linux-gnu/pkgconfig', '/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/usr/share/pkgconfig']"),
            "missing pkg_config_libdir"
        );
    }

    #[test]
    fn cargo_config_for_rock3c() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "rock3c.yaml",
            r#"
id: rock3c
name: Rock 3C
os: linux
architecture:
  cpu_family: aarch64
  cpu: armv8-a
  endian: little
  target_triple: aarch64-linux-gnu
toolchain:
  c: clang
  cpp: clang++
  ar: /opt/homebrew/opt/llvm/bin/llvm-ar
  strip: /opt/homebrew/opt/llvm/bin/llvm-strip
sysroot:
  root: /Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c
  lib_dirs:
    - ${SYSROOT}/usr/lib/aarch64-linux-gnu
  pkg_config_libdir:
    - ${SYSROOT}/usr/lib/aarch64-linux-gnu/pkgconfig
    - ${SYSROOT}/usr/share/pkgconfig
"#,
        );

        let p = Platform::load(tmp.path().join("rock3c.yaml")).unwrap();
        let cfg = p.cargo_config().expect("linux cargo config");

        assert!(
            cfg.contains("[target.aarch64-linux-gnu]"),
            "missing target section"
        );
        assert!(cfg.contains("linker = \"clang\""), "missing linker");
        assert!(
            cfg.contains("link-arg=--target=aarch64-linux-gnu"),
            "missing target link-arg"
        );
        assert!(
            cfg.contains(
                "link-arg=--sysroot=/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c"
            ),
            "missing sysroot link-arg"
        );
        assert!(
            cfg.contains("PKG_CONFIG_SYSROOT_DIR = \"/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c\""),
            "missing pkg-config sysroot env"
        );
        assert!(
            cfg.contains("PKG_CONFIG_LIBDIR = \"/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/usr/lib/aarch64-linux-gnu/pkgconfig:/Users/steve/naturesense/ai-traps/tools/native/sysroots/rock3c/usr/share/pkgconfig\""),
            "missing joined pkg-config libdir"
        );
        // Toolchain env vars use the UPPERCASED triple.
        assert!(
            cfg.contains("CC_AARCH64-LINUX-GNU = \"clang\""),
            "missing CC env"
        );
        assert!(
            cfg.contains("CXX_AARCH64-LINUX-GNU = \"clang++\""),
            "missing CXX env"
        );
        assert!(
            cfg.contains("AR_AARCH64-LINUX-GNU = \"/opt/homebrew/opt/llvm/bin/llvm-ar\""),
            "missing AR env"
        );
    }

    /// "Is this an embedded platform" is what the create-project wizard filters on to offer
    /// platforms for the **embedded-HAL** project type, so it must answer for both families we
    /// ship and must not claim a Linux cross-target.
    #[test]
    fn is_embedded_keys_on_os() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "esp32c6.yaml",
            "id: esp32c6\nname: ESP32-C6\nos: esp-idf\narchitecture:\n  cpu_family: riscv\n  \
             cpu: esp32c6\n  endian: little\n  target_triple: riscv32imac-esp-espidf\n",
        );
        write_yaml(
            tmp.path(),
            "rp2040.yaml",
            "id: rp2040\nname: RP2040\nos: rp2040\narchitecture:\n  cpu_family: arm\n  \
             cpu: cortex-m0plus\n  endian: little\n  target_triple: thumbv6m-none-eabi\n",
        );
        write_yaml(
            tmp.path(),
            "rpi5.yaml",
            "id: rpi5\nname: Raspberry Pi 5\nos: linux\narchitecture:\n  cpu_family: aarch64\n  \
             cpu: armv8-a\n  endian: little\n  target_triple: aarch64-linux-gnu\n",
        );

        let embedded = |name: &str| Platform::load(tmp.path().join(name)).unwrap().is_embedded();
        assert!(embedded("esp32c6.yaml"));
        assert!(embedded("rp2040.yaml"), "the second family is embedded too");
        assert!(
            !embedded("rpi5.yaml"),
            "a Linux cross-target is a board we ssh into, not one we flash"
        );
    }

    #[test]
    fn non_linux_returns_none() {
        let platform = Platform {
            id: "esp32".into(),
            name: "ESP32-S3".into(),
            os: "esp-idf".into(),
            architecture: PlatformArchitecture {
                cpu_family: "xtensa".into(),
                cpu: "esp32s3".into(),
                endian: "little".into(),
                target_triple: "xtensa-esp32s3-elf".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            family: None,
            chip: None,
            rust: None,
            device: None,
            library_hints: None,
        };
        assert!(platform.meson_cross_file().is_none());
    }

    /// An embedded **variant** parses, and carries the facts that make it a distinct
    /// compilation target: its family (what a single backend crate keys off), the rustup
    /// triple, the IDF target (which is also the cargo feature), the USB flash command, and
    /// the free-text hints that *focus* a generated implementation.
    ///
    /// Two variants of one family are loaded together on purpose: same `family: esp32`, but
    /// `xtensa-esp32s3-espidf` vs `riscv32imac-esp-espidf` — Xtensa and RISC-V are different
    /// toolchains, so these are two platforms, not one with a runtime switch. That is why
    /// the unit of `Platform` is the variant.
    #[test]
    fn load_esp32_variants_from_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "esp32c6.yaml",
            r#"
id: esp32c6
name: ESP32-C6
os: esp-idf
family: esp32
chip: None,
architecture:
  cpu_family: riscv
  cpu: esp32c6
  endian: little
  target_triple: riscv32imac-esp-espidf
rust:
  target: riscv32imac-esp-espidf
  idf_target: esp32c6
  flash: espflash
# Free-text guidance for a generated implementation. Inline, so the scalar is exactly the
# string (a block scalar would keep its trailing newline).
library_hints: RISC-V RV32IMAC; no std-vs-no_std choice on this family.
"#,
        );
        write_yaml(
            tmp.path(),
            "esp32s3.yaml",
            r#"
id: esp32s3
name: ESP32-S3
os: esp-idf
family: esp32
chip: None,
architecture:
  cpu_family: xtensa
  cpu: esp32s3
  endian: little
  target_triple: xtensa-esp32s3-espidf
rust:
  target: xtensa-esp32s3-espidf
  idf_target: esp32s3
  flash: espflash
"#,
        );

        let c6 = Platform::load(tmp.path().join("esp32c6.yaml")).unwrap();
        assert_eq!(c6.os, "esp-idf");
        assert_eq!(c6.family.as_deref(), Some("esp32"));
        let rust = c6
            .rust
            .as_ref()
            .expect("an esp-idf platform carries a rust toolchain");
        assert_eq!(rust.target, "riscv32imac-esp-espidf");
        assert_eq!(rust.idf_target.as_deref(), Some("esp32c6"));
        assert_eq!(rust.flash.as_deref(), Some("espflash"));
        assert_eq!(
            c6.library_hints.as_deref(),
            Some("RISC-V RV32IMAC; no std-vs-no_std choice on this family."),
            "the hints the wizard shows and the fill prompt injects come from the YAML"
        );
        // Not a C cross-compile: the C toolchain block is absent from the YAML and the
        // cross-file path must not invent one.
        assert!(c6.meson_cross_file().is_none());

        // One family, two compilation targets.
        let all = Platform::load_directory(tmp.path()).unwrap();
        assert_eq!(all.len(), 2);
        assert!(
            all.iter().all(|p| p.family.as_deref() == Some("esp32")),
            "both variants share the family: {all:?}"
        );
        assert!(
            all.iter().all(|p| p.rust.is_some()),
            "and each carries its own toolchain"
        );
        let triples: Vec<&str> = all
            .iter()
            .map(|p| p.architecture.target_triple.as_str())
            .collect();
        assert!(
            triples.contains(&"riscv32imac-esp-espidf")
                && triples.contains(&"xtensa-esp32s3-espidf"),
            "different architectures are different platforms: {triples:?}"
        );
    }

    /// Target-level sysroot sanity: a nonexistent or unpopulated sysroot must be
    /// flagged (the cross-build gate), while a populated one and host platforms
    /// (empty root) pass.
    #[test]
    fn sysroot_ok_detects_missing_and_populated_roots() {
        let tmp = tempfile::tempdir().unwrap();

        // 1. Missing path → blocked.
        let missing = Platform {
            id: "rpi5".into(),
            name: "Raspberry Pi 5".into(),
            os: "linux".into(),
            architecture: PlatformArchitecture {
                cpu_family: "arm".into(),
                cpu: "armv8".into(),
                endian: "little".into(),
                target_triple: "arm-linux-gnueabihf".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot {
                root: tmp
                    .path()
                    .join("no-such-sysroot")
                    .to_string_lossy()
                    .to_string(),
                lib_dirs: Vec::new(),
                include_dirs: Vec::new(),
                pkg_config_libdir: Vec::new(),
            },
            family: None,
            chip: None,
            rust: None,
            device: None,
            library_hints: None,
        };
        let (ok, reason) = missing.sysroot_ok();
        assert!(!ok, "missing root must be blocked");
        assert!(reason.contains("not a directory"), "reason: {reason}");

        // 2. Empty placeholder root (no usr/) → blocked.
        let placeholder_dir = tmp.path().join("empty-sysroot");
        std::fs::create_dir_all(&placeholder_dir).unwrap();
        let placeholder = Platform {
            id: "rpi5".into(),
            name: "Raspberry Pi 5".into(),
            os: "linux".into(),
            architecture: PlatformArchitecture {
                cpu_family: "arm".into(),
                cpu: "armv8".into(),
                endian: "little".into(),
                target_triple: "arm-linux-gnueabihf".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot {
                root: placeholder_dir.to_string_lossy().to_string(),
                lib_dirs: Vec::new(),
                include_dirs: Vec::new(),
                pkg_config_libdir: Vec::new(),
            },
            family: None,
            chip: None,
            rust: None,
            device: None,
            library_hints: None,
        };
        let (ok, reason) = placeholder.sysroot_ok();
        assert!(!ok, "empty root must be blocked");
        assert!(reason.contains("not populated"), "reason: {reason}");

        // 3. Populated root (usr/) → passes.
        let populated_dir = tmp.path().join("ok-sysroot");
        std::fs::create_dir_all(populated_dir.join("usr")).unwrap();
        let populated = Platform {
            id: "rpi5".into(),
            name: "Raspberry Pi 5".into(),
            os: "linux".into(),
            architecture: PlatformArchitecture {
                cpu_family: "arm".into(),
                cpu: "armv8".into(),
                endian: "little".into(),
                target_triple: "arm-linux-gnueabihf".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot {
                root: populated_dir.to_string_lossy().to_string(),
                lib_dirs: Vec::new(),
                include_dirs: Vec::new(),
                pkg_config_libdir: Vec::new(),
            },
            family: None,
            chip: None,
            rust: None,
            device: None,
            library_hints: None,
        };
        assert!(populated.sysroot_ok().0, "populated root must pass");

        // 4. Host platform (empty root) always passes — native builds need no
        // cross sysroot.
        let host = Platform {
            id: "host".into(),
            name: "Host".into(),
            os: "linux".into(),
            architecture: PlatformArchitecture {
                cpu_family: "x86_64".into(),
                cpu: "x86_64".into(),
                endian: "little".into(),
                target_triple: "x86_64-linux-gnu".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            family: None,
            chip: None,
            rust: None,
            device: None,
            library_hints: None,
        };
        assert!(host.sysroot_ok().0, "host must pass");
    }

    #[test]
    fn discover_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "rpi5.yaml",
            r#"
id: rpi5
name: Raspberry Pi 5
os: linux
architecture:
  cpu_family: arm
  cpu: armv8
  endian: little
  target_triple: arm-linux-gnueabihf
toolchain:
  c: clang
  cpp: clang++
  ar: llvm-ar
  strip: llvm-strip
  pkgconfig: /usr/bin/pkg-config
sysroot:
  root: /opt/rpi5-sysroot
  lib_dirs:
    - ${SYSROOT}/usr/lib/arm-linux-gnueabihf
"#,
        );
        // A non-yaml file must be ignored.
        write_yaml(tmp.path(), "README.txt", "not a platform");

        let platforms = Platform::load_directory(tmp.path()).unwrap();
        assert_eq!(platforms.len(), 1);
        assert_eq!(platforms[0].id, "rpi5");
    }

    /// A platform's `device:` block is what turns a target into something Spire
    /// can talk to: an MCP endpoint on the board plus a deploy destination.
    #[test]
    fn device_block_maps_to_device_mcp_config() {
        let platform: Platform = serde_yaml::from_str(
            r#"
id: rpi5
name: Raspberry Pi 5
os: linux
architecture:
  cpu_family: aarch64
  cpu: armv8-a
  endian: little
  target_triple: aarch64-linux-gnu
toolchain:
  c: clang
  cpp: clang++
  ar: llvm-ar
  strip: llvm-strip
sysroot:
  root: /opt/cross/sysroot/rpi5
device:
  mcp:
    url: http://rpi5.local:8737/mcp
    token: board-secret
  deploy:
    dest: /home/pi/ai-traps
"#,
        )
        .unwrap();

        assert_eq!(
            platform.device_server_name().as_deref(),
            Some("device-rpi5")
        );

        let config = platform.device_mcp_config().expect("device MCP config");
        assert_eq!(config.name, "device-rpi5");
        assert!(
            !config.autostart,
            "a board must not be contacted by the client's ConnectAll"
        );
        match config.transport {
            spire_core::mcp::client::TransportConfig::Http { url, headers } => {
                assert_eq!(url, "http://rpi5.local:8737/mcp");
                assert_eq!(
                    headers.get("Authorization").map(String::as_str),
                    Some("Bearer board-secret")
                );
            }
            other => panic!("expected an HTTP transport, got {other:?}"),
        }

        let deploy = platform
            .device
            .as_ref()
            .and_then(|device| device.deploy.as_ref())
            .expect("deploy block");
        assert_eq!(deploy.dest, "/home/pi/ai-traps");
    }

    /// A platform with no `device:` is host-only: no server name, no config, and
    /// no `Authorization` header invented.
    #[test]
    fn platform_without_device_has_no_device_config() {
        let platform: Platform = serde_yaml::from_str(
            r#"
id: host
name: Host
os: linux
architecture:
  cpu_family: x86_64
  cpu: x86_64
  endian: little
  target_triple: x86_64-linux-gnu
toolchain:
  c: clang
  cpp: clang++
  ar: llvm-ar
  strip: llvm-strip
sysroot:
  root: ""
"#,
        )
        .unwrap();

        assert!(platform.device.is_none());
        assert_eq!(platform.device_server_name(), None);
        assert!(platform.device_mcp_config().is_none());
    }

    /// An endpoint without a token still yields a config — just no auth header —
    /// and a blank URL is not a device at all.
    #[test]
    fn device_config_handles_missing_and_blank_tokens() {
        // Built by hand (not from the registry) so the test is hermetic.
        let mut platform = Platform {
            id: "rpi5".into(),
            name: "Raspberry Pi 5".into(),
            os: "linux".into(),
            architecture: PlatformArchitecture {
                cpu_family: "aarch64".into(),
                cpu: "armv8-a".into(),
                endian: "little".into(),
                target_triple: "aarch64-linux-gnu".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            family: None,
            chip: None,
            rust: None,
            device: None,
            library_hints: None,
        };

        platform.device = Some(PlatformDevice {
            mcp: Some(PlatformDeviceMcp {
                url: "http://rock3c.local:8737/mcp".into(),
                token: Some("   ".into()),
            }),
            deploy: None,
        });
        let config = platform
            .device_mcp_config()
            .expect("blank token still configures");
        match config.transport {
            spire_core::mcp::client::TransportConfig::Http { headers, .. } => {
                assert!(headers.is_empty(), "blank token must not produce a header");
            }
            other => panic!("expected an HTTP transport, got {other:?}"),
        }

        platform.device = Some(PlatformDevice {
            mcp: Some(PlatformDeviceMcp {
                url: "  ".into(),
                token: None,
            }),
            deploy: None,
        });
        assert_eq!(platform.device_server_name(), None);
        assert!(platform.device_mcp_config().is_none());
    }

    /// `device_platforms_in()` is the coordinator's view of the registry: only
    /// platforms with a board. Takes an explicit directory so the test never
    /// touches the process-global `SPIRE_PLATFORM_DIR`.
    #[test]
    fn device_platforms_filters_the_registry() {
        let tmp = tempfile::tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "rpi5.yaml",
            r#"
id: rpi5
name: Raspberry Pi 5
os: linux
architecture:
  cpu_family: aarch64
  cpu: armv8-a
  endian: little
  target_triple: aarch64-linux-gnu
toolchain:
  c: clang
  cpp: clang++
  ar: llvm-ar
  strip: llvm-strip
sysroot:
  root: /opt/cross/sysroot/rpi5
device:
  mcp:
    url: http://rpi5.local:8737/mcp
"#,
        );
        write_yaml(
            tmp.path(),
            "host.yaml",
            r#"
id: host
name: Host
os: linux
architecture:
  cpu_family: x86_64
  cpu: x86_64
  endian: little
  target_triple: x86_64-linux-gnu
toolchain:
  c: clang
  cpp: clang++
  ar: llvm-ar
  strip: llvm-strip
sysroot:
  root: ""
"#,
        );

        let devices = Platform::device_platforms_in(tmp.path());

        assert_eq!(devices.len(), 1, "host has no board: {devices:?}");
        assert_eq!(devices[0].id, "rpi5");
        assert_eq!(
            devices[0].device_server_name().as_deref(),
            Some("device-rpi5")
        );
    }
}
