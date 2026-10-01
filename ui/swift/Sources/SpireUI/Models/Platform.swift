import Foundation

/// A cross-compilation platform definition (mirror of `spire_modules::Platform`).
struct Platform: Identifiable, Codable, Hashable {
    let id: String
    let name: String
    let os: String
    let architecture: PlatformArchitecture
    let toolchain: PlatformToolchain
    let sysroot: PlatformSysroot
    /// Board **family** this variant belongs to (`esp32`, `rp2040`).
    ///
    /// Grouping only: one backend crate serves a whole family, while the unit of compilation stays
    /// the variant (the triple differs between chips).
    let family: String?
    /// The Rust toolchain, for a platform whose build is not a C cross-compile.
    ///
    /// Absent for the C platforms, which build with `toolchain` above.
    let rust: PlatformRust?
    /// Free-text notes about this platform's SDKs, drivers and constraints.
    ///
    /// Shown while a board is chosen — "how to reach the LED on this chip" is exactly what makes a
    /// board choice informed — and injected into the Rust fill prompt so a generated
    /// implementation uses that board's peripherals.
    let libraryHints: String?
    /// True for a board a firmware project targets; false for a host or a Linux cross-target.
    ///
    /// **Sent by `platforms/list`, not derived here.** The rule is `os`, which is what the build
    /// itself keys on, and a second copy of it in Swift could only ever disagree with the first —
    /// the wizard filters its platform list on this flag.
    let embedded: Bool
    /// `"board"` or `"chip"` — which kind of thing this entry names.
    ///
    /// **Sent by `platforms/list`, not derived here**, for the same reason `embedded` is. Board and
    /// chip are not interchangeable — a Linux SBC entry names the board it is built for, a bare-metal
    /// entry names the processor — so the platforms screen sections by this instead of showing one
    /// undifferentiated list. Optional only so a payload from an older core still decodes.
    let kind: String?
    /// The chip this board carries (`chip:` in the registry YAML).
    ///
    /// A board declares its silicon and states none of the build facts — the triple, the
    /// toolchain, the sysroot are the chip's — so this is the link the detail panel follows to
    /// show what a board is built with, rather than three empty groups. Absent on a chip (it *is*
    /// the chip) and on a Linux SBC that states its own config.
    let chip: String?
    /// The vendor HAL a **chip's** BSPs are written against (`hal:` in the chip YAML).
    ///
    /// Chip-scoped, so it is read off the chip a board names rather than the board itself.
    /// Absent for a chip whose HAL has not been pinned and for every Linux target.
    let hal: PlatformHal?
    /// The board's BSP, as its SDK names it (`bsp:` in the board YAML) — under ESP-IDF a managed
    /// component, e.g. `m5stack_core_s3`.
    ///
    /// Board-scoped where `hal` is chip-scoped: one chip serves several boards, while the BSP brings
    /// up *this* board's display, touch and power. Absent means Spire generates its own backend, so a
    /// board pointing at a vendor BSP stays distinguishable from one that has none.
    let bsp: String?
    /// The board this platform can reach, when it declares `device:`. Absent for
    /// the host and for platforms with no board — which is what tells the UI
    /// whether "Run tests on board" / "Deploy binary" can apply.
    let device: PlatformDevice?
    /// The capability blocks the graph holds for this entry — `realizes` / `provides` / `carries` /
    /// `pins`, each with its values.
    ///
    /// **Sent by `platforms/config` only**: `platforms/list` omits it (the wizard needs the build
    /// facts, not the wiring), so it is optional and nil there.
    let capabilityBlocks: CapabilityBlocks?

    enum CodingKeys: String, CodingKey {
        case id, name, os, architecture, toolchain, sysroot, family, rust, embedded, device, kind, chip
        case hal, bsp
        case libraryHints = "library_hints"
        case capabilityBlocks = "capability_blocks"
    }
}

/// The vendor HAL a **chip's** BSPs are written against (`hal:` in the chip YAML).
///
/// Chip-scoped: it travels on the silicon's entry, so a board reads the one its `chip:` names.
/// `crate` is a Rust keyword, hence the rename on the wire.
struct PlatformHal: Codable, Hashable {
    /// The crate name, e.g. `esp-hal`, `rp2040-hal`.
    let crateName: String
    /// The version the HAL has been proven at; empty when none is pinned.
    let version: String
    /// The features this chip needs from the HAL, e.g. `["esp32c3", "unstable"]`.
    let features: [String]

    enum CodingKeys: String, CodingKey {
        case version, features
        case crateName = "crate"
    }
}

/// The **Rust** toolchain for a platform whose build is not a C cross-compile (`rust:` in the
/// registry YAML).
struct PlatformRust: Codable, Hashable {
    /// The rustup target triple, e.g. `riscv32imac-esp-espidf` or `thumbv6m-none-eabi`.
    let target: String
    /// The vendor's name for the chip: `IDF_TARGET` for the esp targets, and the spelling
    /// `probe-rs --chip` expects for the rp2040.
    let idfTarget: String?
    /// The host tool that writes the artifact to the board (`espflash`, `picotool`, `probe-rs`,
    /// `elf2uf2`). Absent means this platform has no USB flash step.
    let flash: String?

    enum CodingKeys: String, CodingKey {
        case target, flash
        case idfTarget = "idf_target"
    }
}

/// A platform's board (`device:` in `~/.spire/platforms/<id>.yaml`).
struct PlatformDevice: Codable, Hashable {
    let mcp: PlatformDeviceMcp?
    let deploy: PlatformDeploy?

    /// True when this platform declares a reachable MCP endpoint.
    var hasMcp: Bool {
        !(mcp?.url.trimmingCharacters(in: .whitespaces) ?? "").isEmpty
    }
}

/// The board's MCP endpoint — a `spire-target-mcp` server over Streamable HTTP.
struct PlatformDeviceMcp: Codable, Hashable {
    let url: String
    /// Optional bearer token; the field is never displayed.
    let token: String?
}

/// Where deployed binaries are installed on the board (`device.deploy`).
struct PlatformDeploy: Codable, Hashable {
    let dest: String
}

struct PlatformArchitecture: Codable, Hashable {
    let cpuFamily: String
    let cpu: String
    let endian: String
    let targetTriple: String
    let march: String?

    enum CodingKeys: String, CodingKey {
        case cpuFamily = "cpu_family"
        case cpu
        case endian
        case targetTriple = "target_triple"
        case march
    }
}

struct PlatformToolchain: Codable, Hashable {
    let c: String
    let cpp: String
    let ar: String
    let strip: String
    let ld: String?
    let pkgconfig: String?
    let cArgsExtra: [String]
    let cppArgsExtra: [String]
    let linkerArgsExtra: [String]
    let needsExeWrapper: Bool

    enum CodingKeys: String, CodingKey {
        case c, cpp, ar, strip, ld, pkgconfig
        case cArgsExtra = "c_args_extra"
        case cppArgsExtra = "cpp_args_extra"
        case linkerArgsExtra = "linker_args_extra"
        case needsExeWrapper = "needs_exe_wrapper"
    }
}

struct PlatformSysroot: Codable, Hashable {
    let root: String
    let libDirs: [String]
    let includeDirs: [String]
    let pkgConfigLibdir: [String]

    enum CodingKeys: String, CodingKey {
        case root
        case libDirs = "lib_dirs"
        case includeDirs = "include_dirs"
        case pkgConfigLibdir = "pkg_config_libdir"
    }
}