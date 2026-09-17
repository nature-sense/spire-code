import Testing
import Foundation
@testable import SpireUI

/// The `platforms/list` payload, as the wizard consumes it.
///
/// This is the contract between the two halves of the wizard's board picker: the Rust side sends
/// `embedded` (its `os` rule) rather than letting Swift re-derive it, and the hints are shown where
/// the choice is made. A field that silently stopped decoding would show an empty list rather than
/// an error, so it is pinned here against the exact JSON `platforms/list` produces.
@Test("A platform listing decodes with the wizard's fields")
func platformListingDecodes() throws {
    let json = """
    [{
      "id": "esp32c6",
      "name": "ESP32-C6",
      "os": "esp-idf",
      "architecture": {
        "cpu_family": "riscv", "cpu": "esp32c6", "endian": "little",
        "target_triple": "riscv32imac-esp-espidf"
      },
      "toolchain": {
        "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
        "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
        "needs_exe_wrapper": false
      },
      "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
      "family": "esp32",
      "rust": {
        "target": "riscv32imac-esp-espidf", "idf_target": "esp32c6", "flash": "espflash"
      },
      "library_hints": "RISC-V RV32IMAC via esp-idf-hal; no std-vs-no_std choice to make.",
      "embedded": true
    }, {
      "id": "rpi5",
      "name": "Raspberry Pi 5",
      "os": "linux",
      "architecture": {
        "cpu_family": "aarch64", "cpu": "armv8-a", "endian": "little",
        "target_triple": "aarch64-linux-gnu"
      },
      "toolchain": {
        "c": "aarch64-linux-gnu-gcc", "cpp": "aarch64-linux-gnu-g++",
        "ar": "aarch64-linux-gnu-ar", "strip": "aarch64-linux-gnu-strip",
        "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
        "needs_exe_wrapper": false
      },
      "sysroot": { "root": "/usr/aarch64-linux-gnu", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
      "embedded": false
    }]
    """

    let platforms: [Platform] = try MessageSerializer.decode(Data(json.utf8))
    #expect(platforms.count == 2)

    let board = platforms[0]
    #expect(board.embedded, "a firmware board is what the embedded picker offers")
    #expect(board.family == "esp32")
    #expect(board.rust?.target == "riscv32imac-esp-espidf")
    #expect(board.rust?.idfTarget == "esp32c6")
    #expect(board.rust?.flash == "espflash")
    #expect(board.libraryHints?.contains("RV32IMAC") == true)

    let host = platforms[1]
    #expect(!host.embedded, "a Linux cross-target has no backend crate to fill")
    #expect(host.family == nil, "family is absent, not empty, for a C platform")
    #expect(host.rust == nil)
    #expect(host.libraryHints == nil)
}


/// Which boards the "Add board" menu offers.
///
/// The rule the *view* applies, pinned here because the tool refuses what it would offer: a family
/// that already has a backend crate, a platform that is not a board, and — since one backend serves
/// every variant of a family — a second variant of a family already present.
@Test("Add board offers one entry per absent family, and only boards")
func addableBoardsOffersOneEntryPerAbsentFamily() throws {
    // Shaped like the real `platforms/list` payload: the model's `architecture`/`toolchain`/`sysroot`
    // are required keys, so a fixture that omitted them would be testing a payload the core never
    // sends.
    let platforms: [Platform] = try MessageSerializer.decode(Data("""
    [{ "id": "esp32c6", "name": "ESP32-C6", "os": "esp-idf", "family": "esp32", "embedded": true,
       "library_hints": "RISC-V RV32IMAC via esp-idf-hal.",
       "architecture": { "cpu_family": "riscv", "cpu": "esp32c6", "endian": "little",
                         "target_triple": "riscv32imac-esp-espidf" },
       "toolchain": { "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
                      "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
                      "needs_exe_wrapper": false },
       "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
       "rust": { "target": "riscv32imac-esp-espidf", "idf_target": "esp32c6", "flash": "espflash" }
    }, { "id": "esp32s3", "name": "ESP32-S3", "os": "esp-idf", "family": "esp32", "embedded": true,
       "architecture": { "cpu_family": "xtensa", "cpu": "esp32s3", "endian": "little",
                         "target_triple": "xtensa-esp32s3-espidf" },
       "toolchain": { "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
                      "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
                      "needs_exe_wrapper": false },
       "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
       "rust": { "target": "xtensa-esp32s3-espidf", "idf_target": "esp32s3", "flash": "espflash" }
    }, { "id": "rp2040", "name": "Raspberry Pi Pico", "os": "rp2040", "family": "rp2040", "embedded": true,
       "architecture": { "cpu_family": "arm", "cpu": "rp2040", "endian": "little",
                         "target_triple": "thumbv6m-none-eabi" },
       "toolchain": { "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
                      "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
                      "needs_exe_wrapper": false },
       "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
       "rust": { "target": "thumbv6m-none-eabi", "idf_target": "RP2040", "flash": "picotool" }
    }, { "id": "rpi5", "name": "Raspberry Pi 5", "os": "linux", "embedded": false,
       "architecture": { "cpu_family": "aarch64", "cpu": "armv8-a", "endian": "little",
                         "target_triple": "aarch64-linux-gnu" },
       "toolchain": { "c": "aarch64-linux-gnu-gcc", "cpp": "aarch64-linux-gnu-g++",
                      "ar": "aarch64-linux-gnu-ar", "strip": "aarch64-linux-gnu-strip",
                      "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
                      "needs_exe_wrapper": false },
       "sysroot": { "root": "/usr/aarch64-linux-gnu", "lib_dirs": [], "include_dirs": [],
                    "pkg_config_libdir": [] }
    }]
    """.utf8))

    // A project with an rp2040 backend: the two esp variants are one offer, and rp2040 is not offered
    // again (the tool would refuse it — a second click must not fork the crate).
    let addable = EmbeddedHalBackendsSection.addableBoards(
        platforms: platforms,
        presentFamilies: ["rp2040"]
    )
    #expect(addable.map(\.id) == ["esp32c6"], "one entry per family, boards only: \(addable.map(\.id))")

    // A project with no backends yet: every board family, and still not the Linux host.
    let fresh = EmbeddedHalBackendsSection.addableBoards(platforms: platforms, presentFamilies: [])
    #expect(fresh.map(\.id) == ["esp32c6", "rp2040"], "\(fresh.map(\.id))")
}

@Test("Bridge initialises without crashing")
func bridgeInit() {
    let bridge = SpireBridge()
    #expect(bridge != nil)
}

// MARK: - FileTree watcher-event regression tests

/// Tree must NOT contain the repeated-folder phantoms (rpi/hal/rpi,
/// toolkit/include/toolkit) that stemmed from mutatePath assigning the
/// full remaining segment as a node's path/id.
@Test("watcher event creates a correctly nested tree (no repeated dirs)")
func watcherEventCreatesCorrectTree() {
    var tree = FileTreeDirectory(name: ".", path: ".", role: "")
    tree.apply(event: SpireBridge.FileChangeEvent(kind: "created", path: "rpi/hal/source.c", isDirectory: false))
    tree.apply(event: SpireBridge.FileChangeEvent(kind: "created", path: "rpi/hal/board.h", isDirectory: false))
    tree.apply(event: SpireBridge.FileChangeEvent(kind: "created", path: "toolkit/include/toolkit.h", isDirectory: false))

    #expect(tree.directories.count == 2, "expected rpi + toolkit at top level")

    let rpi = tree.directories.first { $0.name == "rpi" }
    #expect(rpi?.path == "rpi")
    let hal = rpi?.directories.first { $0.name == "hal" }
    #expect(hal?.path == "rpi/hal")
    #expect(hal?.files.count == 2)

    let toolkit = tree.directories.first { $0.name == "toolkit" }
    #expect(toolkit?.path == "toolkit")
    let include = toolkit?.directories.first { $0.name == "include" }
    #expect(include?.path == "toolkit/include")
    #expect(include?.files.count == 1)
}

/// A deep directory created in one watcher event must build the full chain
/// with correct identities ("platforms/radxa/rock/hal", not repeats).
@Test("watcher event with deeply nested dir builds full chain")
func watcherEventDeepNestedChain() {
    var tree = FileTreeDirectory(name: ".", path: ".", role: "")
    tree.apply(event: SpireBridge.FileChangeEvent(kind: "created", path: "platforms/radxa/rock/hal/i2c.c", isDirectory: false))

    var node = tree
    let expected = ["platforms", "radxa", "rock", "hal"]
    for (i, name) in expected.enumerated() {
        let child = node.directories.first { $0.name == name }
        #expect(child != nil, "expected dir \(name) at depth \(i)")
        #expect(child?.path == expected[0...i].joined(separator: "/"), "wrong path for \(name): \(child?.path ?? "nil")")
        node = child!
    }
    #expect(node.files.count == 1)
}

/// Extension-less files must be files, never phantom empty directories.
@Test("extension-less files are not phantom directories")
func extensionlessFilesAreFilesNotDirs() {
    var tree = FileTreeDirectory(name: ".", path: ".", role: "")
    tree.apply(event: SpireBridge.FileChangeEvent(kind: "created", path: "Makefile", isDirectory: false))
    tree.apply(event: SpireBridge.FileChangeEvent(kind: "created", path: "LICENSE", isDirectory: false))

    #expect(tree.files.count == 2)
    #expect(tree.directories.count == 0)
}

/// Deleted events must never re-add nodes.
@Test("deleted watcher events never re-add nodes")
func deletedEventsDoNotAddNodes() {
    var tree = FileTreeDirectory(name: ".", path: ".", role: "")
    tree.apply(event: SpireBridge.FileChangeEvent(kind: "deleted", path: "rpi/hal/board.h", isDirectory: false))

    #expect(tree.directories.isEmpty)
    #expect(tree.files.isEmpty)
}

// MARK: - Subproject absolute-path resolution (build-status graph key)

/// Build status is persisted under `build.last.<absoluteSubprojectPath>.<target>`.
/// If the READER resolves a different string than the WRITER, every platform
/// shows "never built" forever. The empty-path case (single-root projects like
/// ai-traps, whose HAL subproject path is "") is the one that bit us: it must
/// resolve to the project root with NO trailing slash.
@Test("empty subproject path resolves to the project root without a trailing slash")
func emptySubprojectPathResolvesToRoot() {
    let sub = SubprojectInfo(name: "ai-traps", kind: .project, buildSystem: "Meson",
                             path: "", language: "C/C++")
    let abs = sub.absolutePath(in: "/Users/me/ai-traps")
    #expect(abs == "/Users/me/ai-traps")
    #expect(!abs.hasSuffix("/"), "a trailing slash breaks the build.last.<path> key")
}

@Test("relative subproject path is joined with exactly one slash")
func relativeSubprojectPathJoinsRoot() {
    let sub = SubprojectInfo(name: "hal", kind: .directory, buildSystem: "Meson",
                             path: "hal", language: "C/C++")
    #expect(sub.absolutePath(in: "/Users/me/ai-traps") == "/Users/me/ai-traps/hal")
}

@Test("absolute subproject path is used as-is")
func absoluteSubprojectPathUnchanged() {
    let sub = SubprojectInfo(name: "x", kind: .directory, buildSystem: "Meson",
                             path: "/opt/elsewhere", language: "C/C++")
    #expect(sub.absolutePath(in: "/Users/me/ai-traps") == "/opt/elsewhere")
}

@Test("trailing slashes on root and path are normalised away")
func trailingSlashesNormalised() {
    let sub = SubprojectInfo(name: "hal", kind: .directory, buildSystem: "Meson",
                             path: "hal/", language: "C/C++")
    #expect(sub.absolutePath(in: "/Users/me/ai-traps/") == "/Users/me/ai-traps/hal")

    let empty = SubprojectInfo(name: "p", kind: .project, buildSystem: "Meson",
                               path: "", language: "C/C++")
    #expect(empty.absolutePath(in: "/Users/me/ai-traps/") == "/Users/me/ai-traps")
}


// MARK: - Git status parsing (uncommitted-changes indicator)

/// The uncommitted-changes badge counts `git status --short` entries and
/// separates untracked (`??`) ones. Getting this wrong would either hide a
/// destructive change or cry wolf on a clean tree.
@Test("git status porcelain is counted, untracked separated")
func gitStatusCountsPorcelainLines() {
    let porcelain = """
     M Sources/App.swift
    ?? new-file.txt
    M  crates/spire-code/src/lib.rs
    ?? another.txt
    """
    let s = SpireBridge.parseGitStatus(porcelain)
    #expect(s.changedCount == 4)
    #expect(s.untrackedCount == 2)
    #expect(s.isDirty)
    #expect(s.lines.count == 4)
}

@Test("clean working tree is not dirty")
func gitStatusCleanTree() {
    let s = SpireBridge.parseGitStatus("\n \n")
    #expect(s.changedCount == 0)
    #expect(s.untrackedCount == 0)
    #expect(!s.isDirty)
}

