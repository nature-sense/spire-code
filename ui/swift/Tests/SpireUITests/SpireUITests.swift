import Testing
import Foundation
import SwiftUI
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

/// The `hal:` / `bsp:` blocks, as the configuration screen consumes them.
///
/// Two scopes, one shape: `hal` rides on the **chip** (so a board reads the one its `chip:` names)
/// and `bsp` on the **board**. Both were invisible to the screen until the model decoded them — a
/// dropped key shows nothing rather than an error, the same quiet failure this suite pins
/// elsewhere — so both are fixed here, along with the `crate` rename and the absent case that means
/// "generate our own backend".
@Test("A platform decodes its HAL and BSP crates")
func platformDecodesHalAndBsp() throws {
    let json = """
    [{
      "id": "rp2040",
      "name": "Raspberry Pi Pico",
      "os": "rp2040",
      "architecture": {
        "cpu_family": "arm", "cpu": "rp2040", "endian": "little",
        "target_triple": "thumbv6m-none-eabi"
      },
      "toolchain": {
        "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
        "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
        "needs_exe_wrapper": false
      },
      "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
      "hal": { "crate": "rp2040-hal", "version": "0.10", "features": [] },
      "embedded": true
    }, {
      "id": "m5stack-core-s3",
      "name": "M5Stack Core S3",
      "os": "esp-idf",
      "architecture": {
        "cpu_family": "xtensa", "cpu": "esp32s3", "endian": "little",
        "target_triple": "xtensa-esp32s3-espidf"
      },
      "toolchain": {
        "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
        "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
        "needs_exe_wrapper": false
      },
      "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
      "chip": "esp32s3",
      "bsp": "m5stack_core_s3",
      "embedded": true
    }]
    """

    let platforms: [Platform] = try MessageSerializer.decode(Data(json.utf8))
    #expect(platforms.count == 2)

    // The chip-scoped HAL: `crate` is renamed on the wire (a Rust keyword), version and features
    // survive, and a board with no BSP of its own says so by absence.
    let pico = platforms[0]
    #expect(pico.hal?.crateName == "rp2040-hal")
    #expect(pico.hal?.version == "0.10")
    #expect(pico.bsp == nil, "the Pico names no BSP — Spire generates its backend")

    // The board-scoped BSP beside the chip it names: a vendor component name, and the HAL stays on
    // the chip rather than being copied onto the board.
    let core = platforms[1]
    #expect(core.chip == "esp32s3")
    #expect(core.bsp == "m5stack_core_s3")
    #expect(core.hal == nil, "the HAL is the chip's; a board reads it through `chip`")
}


/// The `device/procs` payload, as the processes panel renders it.
///
/// Pinned against the tool's exact JSON, because this view's failure mode is quiet: a field that
/// stopped decoding would show *"Nothing started by Spire is running"* — which reads as a fact about
/// the board rather than as a bug in the panel. The uptime formatting is pinned too, since "up 0s"
/// for a process started two minutes ago would be equally believable.
@Test("A board process listing describes what is running")
func deviceProcessListingDescribesWhatIsRunning() throws {
    let reply: [String: Any] = [
        "platform": "rpi5",
        "result": [
            "count": 2,
            "running": [
                [
                    "name": "trap", "pid": 4242, "alive": true, "uptime_ms": 125_000,
                    "path": "/home/pi/ai-traps/trap-rpi5", "args": ["--verbose"],
                    "log": "/home/pi/spire-target-work/logs/trap.log",
                ],
                [
                    "name": "old-run", "pid": 4100, "alive": false, "uptime_ms": 0,
                    "log": "/home/pi/spire-target-work/logs/old-run.log",
                ],
            ],
        ],
    ]
    let processes = DeviceProcessesSection.DeviceProcess.from(reply)
    #expect(processes.count == 2)

    let running = processes[0]
    #expect(running.name == "trap")
    #expect(running.pid == 4242)
    #expect(running.alive)
    #expect(running.label == "trap · pid 4242 · up 2m 5s", "\(running.label)")

    // A process that exited is shown as exited, not dropped and not shown as running.
    let exited = processes[1]
    #expect(!exited.alive)
    #expect(exited.label == "old-run · pid 4100 · exited", "\(exited.label)")

    // Uptime, at the boundaries where a naive division reads wrong.
    #expect(DeviceProcessesSection.DeviceProcess.humanUptime(0) == "0s")
    #expect(DeviceProcessesSection.DeviceProcess.humanUptime(59_000) == "59s")
    #expect(DeviceProcessesSection.DeviceProcess.humanUptime(60_000) == "1m 0s")
    #expect(DeviceProcessesSection.DeviceProcess.humanUptime(3_600_000) == "1h 0m")
    #expect(DeviceProcessesSection.DeviceProcess.humanUptime(7_500_000) == "2h 5m")

    // An empty board, and a flat reply (no `result` wrapper) — both shapes are answers, not bugs.
    #expect(DeviceProcessesSection.DeviceProcess.from(["result": ["running": []]]).isEmpty)
    #expect(DeviceProcessesSection.DeviceProcess.from(nil).isEmpty)
    let flat = DeviceProcessesSection.DeviceProcess.from([
        "running": [["name": "solo", "pid": 1, "alive": true, "uptime_ms": 1_000]],
    ])
    #expect(flat.map(\.name) == ["solo"])

    // An entry with no name cannot be addressed by logs or stop, so it is not offered.
    let nameless = DeviceProcessesSection.DeviceProcess.from([
        "result": ["running": [["pid": 9, "alive": true]]],
    ])
    #expect(nameless.isEmpty, "an unnamed process is not addressable")
}

@Test("Bridge initialises without crashing")
func bridgeInit() {
    let bridge = SpireBridge()
    #expect(bridge != nil)
}

/// The history sheet has to carry its own size, because macOS sizes a sheet to the fitting size of
/// its content *at presentation* — and this one's records arrive asynchronously. Sized by its
/// content it was laid out while the view still said "Reading the record…", so it opened at about a
/// third of its height (560×146) and grew into place afterwards. Assert the sheet's size, which must
/// not depend on what has loaded.
@Test("The history sheet is a fixed size, not the height of its loading state")
@MainActor
func componentEditHistorySheetIsFixedSize() {
    let host = NSHostingController(
        rootView: ComponentEditHistorySheet(root: "/tmp/unused", component: "demo")
            .environment(SpireBridge())
            .environment(AppTheme())
    )
    host.view.layoutSubtreeIfNeeded()
    let size = host.view.fittingSize
    #expect(size.width == 560)
    #expect(size.height == 550)
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
// MARK: - A component's kind, and the key that joins the analyzer and the UI

/// The kind a component **states** about itself travels as `componentKind`, beside `kind: "component"`.
///
/// Pinned because the name is the whole join between two languages: `serialize_analysis` in Rust writes
/// it and `SubprojectInfo` reads it, so a drift — `component_kind`, or a nested object — decodes to
/// `nil`, and `nil` is a *meaningful* answer here ("this component states no kind"). That is exactly
/// the class of mistake worth a test: it would show up as a hand-written-looking row for a component
/// spire-code itself created, rather than as an error anywhere.
@Test("a component's kind decodes from the analyzer's own key")
func componentKindDecodesFromAnalyzersKey() throws {
    let stated = """
    {"name":"sps30","kind":"component","componentKind":"driver",
     "buildSystem":"CMake","path":"components/sps30","language":"Other"}
    """
    let driver = try JSONDecoder().decode(SubprojectInfo.self, from: Data(stated.utf8))
    #expect(driver.kind == .component)
    #expect(driver.componentKind == "driver")

    let pure = """
    {"name":"moving_average","kind":"component","componentKind":"library",
     "buildSystem":"CMake","path":"components/moving_average","language":"Other"}
    """
    let library = try JSONDecoder().decode(SubprojectInfo.self, from: Data(pure.utf8))
    #expect(library.componentKind == "library")

    // A **framework** component states the *same kind* as a generated library component — which is the
    // whole reason `componentFramework` exists: the kind alone cannot tell a shipped component from one
    // this library produced, and the row offers a Write button only for the second.
    let shipped = """
    {"name":"actors","kind":"component","componentKind":"library","componentFramework":"actors",
     "buildSystem":"CMake","path":"components/actors","language":"Other"}
    """
    let framework = try JSONDecoder().decode(SubprojectInfo.self, from: Data(shipped.utf8))
    #expect(framework.componentKind == "library")
    #expect(framework.componentFramework == "actors")

    // A component that states nothing — one written by hand — is `nil` rather than a default: nothing
    // guesses, and the sheet's description box reflects that.
    let unstated = """
    {"name":"handwritten","kind":"component","componentKind":null,
     "buildSystem":"CMake","path":"components/handwritten","language":"Other"}
    """
    let other = try JSONDecoder().decode(SubprojectInfo.self, from: Data(unstated.utf8))
    #expect(other.componentKind == nil)
    // …and it is the library's own work, so its row keeps the Write button.
    #expect(other.componentFramework == nil)

    // The other half of the join: a component that is *not* the framework carries `null`, and a
    // subproject that is not a component at all carries neither.
    #expect(driver.componentFramework == nil)
    #expect(library.componentFramework == nil)

    let plain = """
    {"name":"hal","kind":"directory","buildSystem":"Meson","path":"hal","language":"C/C++"}
    """
    let directory = try JSONDecoder().decode(SubprojectInfo.self, from: Data(plain.utf8))
    #expect(directory.componentKind == nil)
    #expect(directory.componentFramework == nil)
}



/// A subproject's **identity** is its path, not its name.
///
/// Two directories in different places can share a leaf name (`components/ramen/test` and
/// `components/actors/test` are both `test`), and the UI identifies rows — and compares selections —
/// by this. With the name as identity, two such subprojects were one row to SwiftUI: a duplicate
/// `ForEach` id, and a click on one that highlighted both. The analyzer no longer reports either of
/// them as a subproject; this pins the other half, that identity cannot collide even if it did.
@Test("two subprojects with the same name in different places are different rows")
func sameNameDifferentPathsAreDistinct() {
    let ramenTest = SubprojectInfo(name: "test", kind: .component, buildSystem: "CMake",
                                   path: "components/ramen/test", language: "Other")
    let actorsTest = SubprojectInfo(name: "test", kind: .component, buildSystem: "CMake",
                                    path: "components/actors/test", language: "Other")
    #expect(ramenTest.name == actorsTest.name)
    #expect(ramenTest.id != actorsTest.id)
    #expect(Set([ramenTest.id, actorsTest.id]).count == 2)

    // And identity is the path, so two rows of the same size are never the same row by accident: the
    // root subproject's path is empty, and it is the only one.
    let root = SubprojectInfo(name: "sensors", kind: .project, buildSystem: "CMake",
                              path: "", language: "Other")
    let component = SubprojectInfo(name: "sensors", kind: .component, buildSystem: "CMake",
                                   path: "components/sensors", language: "Other")
    #expect(root.id != component.id)
}

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


// MARK: - The project types

/// Every project type that can scaffold names a `ProjectStructure` key the core knows.
///
/// Pinned because an unknown key falls back to `native` **silently** (`ProjectStructure::from_str`),
/// so a typo here would scaffold a host crate for a firmware choice and report nothing.
@Test("A project type that scaffolds names a structure the core scaffolds")
func projectTypeStructuresAreKeysTheToolScaffolds() {
    // What the tool can create today. `embedded` and `embedded_app` are deliberately absent: their
    // scaffolds were retired, and a row naming one would not fail — `from_str` would hand back a
    // *different* structure and the core would build the wrong project without comment.
    let scaffolding: Set<String> = [
        "native", "single_source", "hal", "spire_app", "idf_library", "idf_application",
    ]
    for type in ProjectTypeGroup.all.flatMap(\.types) {
        guard let structure = type.structure else { continue }
        #expect(scaffolding.contains(structure), "\(type.title) → \(structure)")
    }
}

/// The rows that cannot scaffold are exactly the rows with no structure key.
///
/// The key is the whole gate — `ProjectTypePicker` refuses to open the name sheet without one — so a
/// row that gained a key it cannot honour would start scaffolding, and a row that lost one would go
/// quiet in a way nothing else reports.
@Test("Only the wired project types carry a structure key")
func onlyWiredProjectTypesCarryAStructure() {
    let rows = ProjectTypeGroup.all.flatMap(\.types)

    #expect(rows.compactMap(\.structure).sorted()
            == ["idf_application", "idf_library", "spire_app"])
    #expect(rows.filter { $0.structure == nil }.map(\.id).sorted()
            == ["linux_sbc_cpp", "linux_sbc_rust"])
}

/// Every row is its own type, and the embedded ones sit under one heading in the order they are
/// shown.
///
/// Pinned because the ids are the contract with the `ForEach` that draws the list: two rows sharing
/// one would render one row, and the list would be quietly short.
@Test("Every project type is distinct and grouped")
func projectTypesAreDistinctAndGrouped() {
    let rows = ProjectTypeGroup.all.flatMap(\.types)
    #expect(Set(rows.map(\.id)).count == rows.count)
    #expect(Set(ProjectTypeGroup.all.map(\.id)).count == ProjectTypeGroup.all.count)

    // `SectionHeading` draws the group's title, so an empty one is a sub-heading over nothing.
    #expect(ProjectTypeGroup.all.allSatisfy { !$0.title.isEmpty && !$0.types.isEmpty })

    let embedded = ProjectTypeGroup.all.first { $0.id == "embedded" }
    #expect(embedded?.types.map(\.id) == [
        "linux_sbc_cpp", "linux_sbc_rust", "idf_library", "idf_application",
    ])
}

/// The **design phase's** reply, as a reviewer reads it.
///
/// This is the contract between the two halves of the wizard's review step: the Rust side decides the
/// decomposition (`createProject/DesignApplication`) and this shows it — the framework with the line it
/// becomes, the units with what each one *is*, the wiring and the board facts. A field that quietly
/// stopped decoding would review an empty application and approve it, which is the one outcome the
/// design phase exists to prevent, so it is pinned against the exact JSON the core produces.
@Test("A design reply decodes into what a person reviews")
func designReplyDecodes() throws {
    let json = """
    {
      "framework": "actors",
      "justification": "~1 Hz reading, touch-driven recalibration, state held across messages.",
      "board": { "chip": "esp32s3", "bsp": "waveshare/esp32_s3" },
      "board_facts": [
        { "bus": "i2c", "device": "sps30", "address": "0x69" },
        { "bus": "i2c", "device": "sht20", "address": "0x40" }
      ],
      "units": [
        { "id": "sps30", "kind": "component", "role": "driver", "source": "stub",
          "provides": "PM1/2.5/4/10 readings", "bus": "i2c" },
        { "id": "rolling_average", "kind": "component", "role": "library", "source": "existing",
          "provides": "windowed mean" },
        { "id": "esp_dl", "kind": "component", "role": "library", "source": "published",
          "registry": "espressif/esp-dl", "provides": "the detector runtime" },
        { "id": "sampler", "kind": "actor", "message": "Tick", "state": "both sensors",
          "uses": ["sps30", "sht20"], "sends_to": ["air_quality"] },
        { "id": "air_quality", "kind": "actor", "message": "Reading", "state": "the window",
          "uses": ["rolling_average"], "sends_to": ["view"] }
      ],
      "wiring": ["sampler -> air_quality", "air_quality -> view"]
    }
    """

    let design: ApplicationDesign = try MessageSerializer.decode(Data(json.utf8))

    #expect(design.framework == "actors")
    // The choice reaches the tree as one stated line, so the review shows that line rather than a
    // summary of it.
    #expect(design.marker == "set(SPIRE_APPLICATION_FRAMEWORK actors)")
    #expect(design.justification?.contains("~1 Hz") == true)
    #expect(design.board.chip == "esp32s3")
    #expect(design.board.json["hal"] == nil, "an empty hal is left out, not sent blank")

    // The split is the one a reader needs: what the library must have, and what the application is.
    #expect(design.components.map(\.id) == ["sps30", "rolling_average", "esp_dl"])
    #expect(design.composition.map(\.id) == ["sampler", "air_quality"])
    #expect(design.componentsToWrite.map(\.id) == ["sps30"],
            "a component the library has — and a published one — is not something to write")
    // A `published` component is a **managed dependency**: the application depends on it by its
    // registry name, and nothing is written into the library for it.
    #expect(design.componentsFromRegistry.map(\.id) == ["esp_dl"])
    #expect(design.componentsFromRegistry.first?.registry == "espressif/esp-dl")
    #expect(design.facts.map(\.summary) == ["sps30 on i2c at 0x69", "sht20 on i2c at 0x40"])
    #expect(design.edges.count == 2)

    // A unit described only by its name is a unit the person cannot disagree with, so each line says
    // what the unit *is* — including whether a component is already in the library.
    #expect(design.components[0].summary == "driver on i2c — to be written: PM1/2.5/4/10 readings")
    #expect(design.components[1].summary == "library — the library has it: windowed mean")
    // A published component says *which* registry component it is, not merely that it is one.
    #expect(design.components[2].summary
            == "library — from the registry (espressif/esp-dl): the detector runtime")
    #expect(design.composition[0].summary
            == "actor on Tick, holding both sensors, uses sps30, sht20, sends to air_quality")
    // A stage says what it pulls and pushes; "nothing" is a fact about the design, not a gap in the UI.
    let stage = ApplicationDesign.Unit(
        id: "preprocess", kind: "stage", pulls: "frames", pushes: "tensors"
    )
    #expect(stage.summary == "stage: pulls frames, pushes tensors")
}

/// The **composition panel's** reply, as the viewer reads it — a design read back *off a tree* rather
/// than one a model has just chosen.
///
/// It is the same `ApplicationDesign` the review decodes, shown beneath the file it came from, so this
/// pins the shape the panel adds: `spec` (the design), `yaml` (the composition as the file holds it), and
/// `file` (which file to open to change it). The design a tree states is the one a unit's Write is written
/// from, and its line names the drivers a unit `uses` — the wording a person reads here is the wording the
/// code is written under.
@Test("A design read back off a tree decodes with the file it came from")
func readBackDesignDecodes() throws {
    let json = """
    {
      "file": "composition.spire",
      "spec": {
        "framework": "actors",
        "board": { "chip": "esp32s3", "bsp": "waveshare/esp32_s3" },
        "board_facts": [ { "bus": "i2c", "device": "sps30", "address": "0x69" } ],
        "units": [
          { "id": "sps30", "kind": "component", "role": "driver", "source": "stub",
            "provides": "PM1/2.5/4/10 readings", "bus": "i2c" },
          { "id": "sampler", "kind": "actor", "message": "Tick", "state": "both sensors",
            "uses": ["sps30"], "sends_to": ["air_quality"] }
        ],
        "wiring": ["sampler -> air_quality"]
      },
      "yaml": "framework: actors\\nboard:\\n  chip: esp32s3\\n"
    }
    """

    let reply = (try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]) ?? [:]
    let spec = (reply["spec"] as? [String: Any]) ?? [:]
    let design: ApplicationDesign = try MessageSerializer.decode(
        try JSONSerialization.data(withJSONObject: spec)
    )

    // The file names the thing a person opens to change the design, rather than the step that read it.
    #expect(reply["file"] as? String == "composition.spire")
    #expect(design.components.map(\.id) == ["sps30"])
    // The line a reviewer reads is the one a Write is written under: the drivers the actor wraps are
    // named (`uses`), not left to the code.
    #expect(design.composition.first?.summary
            == "actor on Tick, holding both sensors, uses sps30, sends to air_quality")
    // The composition as the file holds it travels beside the reading, for the panel's lower half.
    #expect((reply["yaml"] as? String)?.contains("framework: actors") == true)
}

/// The design form's six answers become the `description` the design request is built from.
///
/// The labels are load-bearing: the request asks the model to say which question went unanswered rather
/// than invent a device, an address or a protocol to fill the gap, and a model handed six labelled
/// paragraphs can see the gap. An empty answer is left out — a blank line under a heading would make an
/// unanswered question look answered.
@Test("The design form labels its answers and leaves the blanks out")
func designFormDescription() {
    var form = ApplicationDesignForm()
    #expect(!form.isUsable, "an empty form is not something to design from")
    #expect(form.description.isEmpty)

    form.purpose = "  A PM2.5 meter with a small screen.  "
    form.senses = "SPS30 and SHT20 on i2c, about 1 Hz"
    // `actsOn`, `reactsTo`, `timing` and `missing` are left unanswered on purpose.

    #expect(form.isUsable)
    #expect(form.description.hasPrefix("What it does: A PM2.5 meter"))
    #expect(form.description.contains("\n\nWhat it senses: SPS30 and SHT20"))
    #expect(!form.description.contains("What it acts on"), "a blank answer is not sent as one")
    #expect(form.answers.count == 6, "the six questions, in the order the request asks them")
    #expect(form.answers[4].answer.isEmpty)
}

/// A board is required rather than defaulted, and the rule is the core's: a guessed chip is a build that
/// fails on hardware. Checked here so the button is disabled rather than the design refused. A BSP is
/// **not** required — the catalogue has one on one board in ten — so a chip alone is a board.
@Test("A board needs a chip, and a BSP only when the board has one")
func boardChoiceRule() {
    var board = BoardChoice()
    #expect(!board.isComplete)
    #expect(board.json["chip"] as? String == "")

    board.chip = " esp32s3 "
    #expect(board.isComplete, "the chip is what decides the build; a BSP is the board's to offer")
    #expect(board.json["chip"] as? String == "esp32s3", "the field is trimmed, not sent as typed")
    #expect(board.json["bsp"] as? String == "", "no BSP is an answer, not a missing one")

    board.bsp = "espressif/m5stack_core_s3"
    #expect(board.json["bsp"] as? String == "espressif/m5stack_core_s3")
    #expect(board.json["hal"] == nil)

    board.hal = "m5unified"
    #expect(board.json["hal"] as? String == "m5unified")
}

/// A **repair** reply: the rewrites to run, and the two lists that say what a repair would not touch.
///
/// This is the contract between the core's `createProject/RepairFromBuild` and the wizard's build step.
/// The steps must decode as the same `CreationStep` the fill's plan uses — they are executed by the same
/// `ExecutePlan`, which is where the structural guard lives — and the refusals and the unrepaired
/// diagnostics must survive, because they are the part a person has to act on: a rewrite that would not
/// parse was not written, and an error in a file no repair may touch is not a model's to fix.
@Test("A repair reply decodes into steps and what it refused")
func repairReplyDecodes() throws {
    let json = """
    {
      "steps": [
        {
          "id": "repair-1",
          "stepType": "write_source_file",
          "description": "Repair main/main.cpp (3 compiler lines)",
          "status": "pending",
          "parameters": {
            "path": "/tmp/app/main/main.cpp",
            "content": "#include <actor.hpp>\\n"
          },
          "result": null
        }
      ],
      "diagnostics": 3,
      "refused": [
        { "path": "/tmp/app/main/touch.hpp", "reason": "the rewrite did not parse, so it was not written" }
      ],
      "unrepaired": [
        "/tmp/app/library/components/actors/include/scheduler.hpp:63:23: error: no matching function"
      ],
      "next": "execute these steps and build again"
    }
    """

    let repair: ApplicationRepair = try MessageSerializer.decode(Data(json.utf8))

    #expect(repair.diagnostics == 3)
    #expect(repair.hasWork)
    #expect(repair.steps.count == 1)
    #expect(repair.steps[0].id == "repair-1")
    #expect(repair.steps[0].stepType == .writeSourceFile, "the same step type the fill's plan uses")
    #expect(repair.steps[0].description == "Repair main/main.cpp (3 compiler lines)")
    #expect(repair.refused.first?.path == "/tmp/app/main/touch.hpp")
    #expect(repair.unrepaired.count == 1, "the library's error is reported, not fixed")
    #expect(repair.next?.contains("build again") == true)
    #expect(
        repair.summary == "1 rewrite for 3 errors, 1 refused, 1 not ours to fix",
        "the line the sheet shows while it works: \(repair.summary)"
    )

    // A repair that proposes nothing ends the loop: asking the same question about the same log has no
    // exit, and a caller that rebuilt anyway would report the same failure twice.
    let nothing = """
    { "steps": [], "diagnostics": 2, "refused": [], "unrepaired": ["a", "b"] }
    """
    let empty: ApplicationRepair = try MessageSerializer.decode(Data(nothing.utf8))
    #expect(!empty.hasWork)
    #expect(empty.summary == "0 rewrites for 2 errors, 2 not ours to fix")
    #expect(empty.next == nil)
}

/// **The core never answers a repair with a key missing, and this is the Swift side of that promise.**
///
/// A build whose output named no `error:` line once came back as `{steps, diagnostics, note}` — no
/// `refused`, no `unrepaired` — and the decode failed: "the repair could not run: Key 'refused' not
/// found", which took the whole build→repair→rebuild verify down with it. The core builds both answers
/// through one function now, so the empty one is decoded here exactly as it is sent.
@Test("A build with nothing to repair still decodes as a repair")
func anEmptyRepairDecodes() throws {
    let json = """
    {
      "steps": [],
      "diagnostics": 0,
      "refused": [],
      "unrepaired": [],
      "next": "nothing to repair: the build's output names no `error:` line, so there is no compiler diagnostic to act on — a warning is not a broken build"
    }
    """

    let repair: ApplicationRepair = try MessageSerializer.decode(Data(json.utf8))

    #expect(!repair.hasWork, "no rewrites is not work")
    #expect(repair.steps.isEmpty)
    #expect(repair.diagnostics == 0)
    #expect(repair.refused.isEmpty)
    #expect(repair.unrepaired.isEmpty)
    #expect(repair.next?.contains("no `error:` line") == true)
}

/// A build that failed because the **machine** has no ESP-IDF, told apart from a build that failed
/// because the application is wrong.
///
/// This is the one failure with a documented fix (`idf_env_check` / `idf_env_fix`, and `make run-idf` to
/// build in an environment a person chose), and the fix is useless if the failure reads like every other
/// one: twenty lines of shell output ending in a `cmake` error. The phrases are listed rather than guessed
/// at, and the negative case is the common one — a compiler's error must not be blamed on the environment.
@Test("A missing toolchain is named, a compiler error is not")
func buildEnvironmentHint() {
    let spawnFailure = """
    Command failed: idf.py -B build build
    /bin/bash: line 1: idf.py: command not found
    """
    let hint = BuildEnvironmentHint.forFailure(output: spawnFailure)
    #expect(hint?.contains("make run-idf") == true, "the hint names the fix: \(hint ?? "nil")")
    #expect(hint?.contains("idf_env_check") == true)

    // The process runner's own way of saying the same thing, with no output at all.
    #expect(
        BuildEnvironmentHint.forFailure(output: "", error: "spawn failed: No such file or directory (os error 2)")
            != nil
    )

    // A compiler error is the application's, and must not be excused as environmental.
    let compileError = """
    /Users/me/app/main/sampler.hpp:25:9: error: no matching function for call to 'app::Sampler::Sampler(…)'
    ninja: build stopped: subcommand failed.
    """
    #expect(
        BuildEnvironmentHint.forFailure(output: compileError) == nil,
        "a compile error is not an environment problem"
    )
    #expect(BuildEnvironmentHint.forFailure(output: "", error: nil) == nil)
}

/// An ESP-IDF **application** starts with the shared component library already chosen; every other
/// project type has no library field, so it starts with none.
///
/// This is the one thing that makes "the library + the drivers are always available" true without a
/// click: an application built on `spire-idf` is the ordinary case, and an application that names no
/// library has to grow its own copy of the framework and the drivers — the outcome the field exists to
/// avoid. It stays editable; only the default is decided here.
@Test("An application defaults to the shared library, every other type to none")
func applicationDefaultsToTheSharedLibrary() {
    let expected = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("naturesense/spire/spire-idf").path
    #expect(expected.hasSuffix("naturesense/spire/spire-idf"), "\(expected)")
    #expect(CreateProjectSheet.defaultLibrary(for: "idf_application") == expected)
    #expect(CreateProjectSheet.defaultLibrary(for: "idf_library") == nil,
            "a library is built against nothing")
    #expect(CreateProjectSheet.defaultLibrary(for: "native") == nil)
}

/// The primary button's rule — and the half it used to be missing: the **form** needs somewhere to put
/// the project, not only a board and the six answers.
///
/// It was the pairing that failed. A design ran happily with no location chosen, and the *review*
/// screen — which has no location field — then offered a Create button that could never enable. So the
/// rule is pinned as a whole: form ⇒ root **and** board **and** answers; review ⇒ root; and neither
/// while a creation is in flight.
@Test("The form cannot be designed without a place to put the project")
func theFormCannotBeDesignedWithoutARoot() {
    func enabled(
        _ stage: CreateProjectSheet.Stage,
        application: Bool = true,
        creating: Bool = false,
        board: Bool = true,
        form: Bool = true,
        root: Bool = true
    ) -> Bool {
        CreateProjectSheet.primaryEnabled(
            stage: stage, isApplication: application, isCreating: creating,
            boardComplete: board, formUsable: form, canCreate: root
        )
    }

    #expect(enabled(.form))
    // The bug: a board and the answers, but nowhere to put the result.
    #expect(!enabled(.form, root: false),
            "a design with no location reaches a review screen whose Create button cannot enable")
    #expect(!enabled(.form, board: false))
    #expect(!enabled(.form, form: false))

    // At review the root is all that is left, and a project is never created twice.
    #expect(enabled(.review, board: false, form: false))
    #expect(!enabled(.review, root: false))
    #expect(!enabled(.review, creating: true))
    #expect(!enabled(.designing))

    // A non-application has no design stage: the button simply follows the root.
    #expect(enabled(.form, application: false, board: false, form: false))
    #expect(!enabled(.form, application: false, root: false))
}

/// The line the sheet shows while a build runs — the compiler's own text, bounded.
///
/// It exists because a first build is minutes long and a still spinner for minutes reads as a hang:
/// during that time the only evidence of progress is the last line the build streamed. That line is
/// not a caption-sized thing — a managed-component path plus an error plus the code context runs to
/// hundreds of characters — so it is bounded before it is held in view state. The bound is what is
/// pinned here, on all three sides of it: short lines untouched, the boundary exact, long lines cut
/// from the **front** (the tail is what names the file being compiled).
@Test("The streamed build line is bounded, and it keeps the tail")
func theStreamedBuildLineIsBounded() {
    let short = "Compiling main.cpp"
    #expect(CreateProjectSheet.shortBuildLine(short) == short)

    // Whitespace is not content — the consumer skips blank lines by the same rule.
    #expect(CreateProjectSheet.shortBuildLine("   \n  ") == "")

    // At the bound, nothing is cut; one past it, the ellipsis replaces exactly one character.
    let atBound = String(repeating: "y", count: 160)
    #expect(CreateProjectSheet.shortBuildLine(atBound) == atBound)
    let justOver = String(repeating: "y", count: 161)
    #expect(CreateProjectSheet.shortBuildLine(justOver).count == 160,
            "160 including the ellipsis, so the caption's width does not depend on the line")

    let long = "START-" + String(repeating: "x", count: 400) + "-app_main.cpp:42: error"
    let bounded = CreateProjectSheet.shortBuildLine(long)
    #expect(bounded.count == 160)
    #expect(bounded.hasPrefix("…"))
    #expect(bounded.hasSuffix("-app_main.cpp:42: error"), "the tail is what the caption shows")
    #expect(!bounded.contains("START-"), "the head is what is dropped")
}

/// The creation log's numbers, pinned where a reader would notice them: the log exists to answer
/// "where did the minutes go?", and a duration rendered as `0s` answers it wrongly.
///
/// The phases worth telling apart are a fast one (the scaffold) and a slow one (a model call or a
/// first build), so the fast end keeps a decimal and the slow end becomes minutes.
@Test("A phase duration is rendered so a fast phase does not read as no phase")
func phaseDurationsAreRendered() {
    #expect(CreateProjectSheet.durationText(0.42) == "0.4s")
    #expect(CreateProjectSheet.durationText(9.4) == "9.4s")
    #expect(CreateProjectSheet.durationText(12.3) == "12s")
    #expect(CreateProjectSheet.durationText(59.4) == "59s")
    #expect(CreateProjectSheet.durationText(60) == "1m")
    #expect(CreateProjectSheet.durationText(75) == "1m 15s")
    #expect(CreateProjectSheet.durationText(120) == "2m")
    // Nothing reads as a rounded-away zero, and nothing is negative — clock skew is not a phase.
    #expect(CreateProjectSheet.durationText(0) == "0.0s")
    #expect(CreateProjectSheet.durationText(-1) == "0.0s")
}

/// The stamp is fixed and locale-independent — a log line read next to another log line (or next to
/// `spire-scaffold.log`) has to line up, and every field is zero-padded so two lines never shift the
/// text beside them.
@Test("A log stamp is fixed-format wall-clock time")
func logStampsAreFixedFormat() {
    // 2026-09-29T14:09:37Z and 2026-09-29T04:05:06Z, as seconds since the epoch — the calendar is not
    // what is under test, so it is not what is constructed.
    #expect(CreateProjectSheet.logStamp(Date(timeIntervalSince1970: 1_790_690_977), timeZone: .gmt)
        == "14:09:37")
    #expect(CreateProjectSheet.logStamp(Date(timeIntervalSince1970: 1_790_654_706), timeZone: .gmt)
        == "04:05:06")
}

/// A log line is the stamp, then the duration when there is one, then the text — and the line that
/// opens a phase has no duration, because none has elapsed yet.
@Test("A creation log line renders stamp, duration, then text")
func creationLogLinesRender() {
    let moment = Date(timeIntervalSince1970: 1_790_690_977)

    let opening = CreateProjectSheet.CreationLogLine(
        stamp: moment, text: "Scaffolding…", elapsed: nil, kind: .phase
    )
    #expect(opening.rendered == CreateProjectSheet.logStamp(moment) + "  Scaffolding…")

    let closing = CreateProjectSheet.CreationLogLine(
        stamp: moment, text: "Scaffolded", elapsed: 0.42, kind: .done
    )
    #expect(closing.rendered == CreateProjectSheet.logStamp(moment) + "  0.4s  Scaffolded")
}

/// The note that explains a slow first build follows the **finalized** dependency list, not the
/// scaffold's board guess.
///
/// It used to say "the ESP-IDF framework and this board's BSP" about every build. That was true of
/// the scaffold's unconditional BSP pin and is false of a sensor-only composition, whose stubs pull
/// nothing managed: naming a BSP that build never fetched is the kind of note a person stops
/// believing. So the note is a function of what the manifest settled.
@Test("The first-build note follows what the manifest actually pinned")
func theFirstBuildNoteFollowsTheManifest() {
    let nothing = CreateProjectSheet.firstBuildNote(dependencies: [])
    #expect(nothing.contains("ESP-IDF framework"))
    #expect(!nothing.lowercased().contains("bsp"),
            "no board stack was pinned, so none may be named")

    let one = CreateProjectSheet.firstBuildNote(dependencies: ["espressif/m5stack_core_s3"])
    #expect(one.contains("1 managed component"))
    #expect(!one.contains("1 managed components"))
    #expect(one.lowercased().contains("bsp"))

    let two = CreateProjectSheet.firstBuildNote(dependencies: ["a/b", "c/d"])
    #expect(two.contains("2 managed components"))
}

/// A build that failed **stays in front of its owner**, and leaves its own record.
///
/// The failure used to be written and then thrown away: `create()` dismissed the sheet the instant
/// `buildAndRepair` returned, whether or not the build had succeeded, so a run whose build failed
/// handed over a project tree and a message nobody could read — "it produced errors I did not see".
/// Only the *environment* failure has a documented fix (`make run-idf`), and the fix is useless if
/// the message that names it is dismissed with the sheet, so both halves are pinned together here:
/// only a nil failure closes the sheet, and the log entry that outlives it carries the whole thing.
@Test("A failed build keeps the sheet and leaves a record")
func aFailedBuildKeepsTheSheetAndLeavesARecord() {
    #expect(CreateProjectSheet.canCloseAfterBuild(failure: nil),
            "a build that worked hands the person over to the dashboard")
    #expect(!CreateProjectSheet.canCloseAfterBuild(failure: "The project is created but does not build."),
            "a project that does not build is a fact its owner has to be shown")

    // The record: the sheet's message — which names the fix when the app's own environment is the
    // cause — and then the build's own text, which only some failures carry in their message.
    let hint = BuildEnvironmentHint.forFailure(output: "idf.py: command not found") ?? ""
    let line = CreateProjectSheet.buildFailureLogLine(
        message: "The project is created but does not build.\n\n" + hint,
        buildOutput: "idf.py: command not found\n"
    )
    #expect(line.hasPrefix("createProject/Build FAILED:"))
    #expect(line.contains("make run-idf"), "the record names the fix: \(line)")
    #expect(line.contains("build output (last 4000 chars):"))
    #expect(line.contains("idf.py: command not found"))
}

/// The build output in the record is bounded, the way the sheet's copy of it is.
///
/// A first ESP-IDF build can fail with thousands of lines, and unlike the sheet this file is not
/// scrolled away — it is read, and read later, so one failure must not bury the next one.
@Test("The build record's output is bounded")
func theBuildRecordsOutputIsBounded() {
    let huge = String(repeating: "x", count: 9000)
    let line = CreateProjectSheet.buildFailureLogLine(message: "failed", buildOutput: huge)
    #expect(line.contains("(last 4000 chars)"))
    #expect(line.contains(String(repeating: "x", count: 4000)))
    #expect(!line.contains(String(repeating: "x", count: 4001)),
            "4000 characters at most, so one failure cannot bury the file")

    // A runner that said nothing adds nothing: the message alone is the record.
    let bare = CreateProjectSheet.buildFailureLogLine(message: "failed")
    #expect(!bare.contains("build output"))
    #expect(!CreateProjectSheet.buildFailureLogLine(message: "failed", buildOutput: "  \n ")
        .contains("build output"))
}

/// The build runs for the **design's** chip, not the form's picker — which a loaded composition left
/// empty.
///
/// Opening a composition fills no form, so `board.chip` is empty while the design's own `board.chip`
/// carries the file's board. Reading the form first handed the build no platform, and no platform
/// routes a `CMakeLists.txt` tree to the CMake module (`cmake --build build`) instead of the ESP-IDF
/// one (`idf.py build`) — which is how a real, never-configured ESP-IDF project failed with "`build`
/// is not a directory" instead of building. The scaffold is told the same chip, so the recorded
/// targets and the build's module cannot disagree.
@Test("The build runs for the design's chip, even when the form's picker is empty")
func theBuildRunsForTheDesignsChip() {
    // The load path: the form was never filled, so the design is the only board there is.
    #expect(CreateProjectSheet.buildPlatform(designChip: "esp32s3", formChip: "") == "esp32s3",
            "a loaded composition builds for the file's own board")

    // The designed path: the form *is* the design, so the two agree and the answer is the same one.
    #expect(CreateProjectSheet.buildPlatform(designChip: "esp32s3", formChip: "esp32s3") == "esp32s3")

    // With no design at all the form still decides, which is the shape a scaffold without one has.
    #expect(CreateProjectSheet.buildPlatform(designChip: nil, formChip: "rpi5") == "rpi5")

    // Neither names a chip: a host build, which is what a scaffold without a board means.
    #expect(CreateProjectSheet.buildPlatform(designChip: nil, formChip: "") == nil)
    #expect(CreateProjectSheet.buildPlatform(designChip: "", formChip: "") == nil,
            "an empty design chip is not a chip")
}

/// The platform decision is written down — both inputs and what they resolved to — so a run inside a
/// process launched before a fix was linked is distinguishable from a broken rule.
///
/// The failure this pins is a puzzle that cost a session: the same `cmake --build build` error after
/// the rule was fixed, because the app doing the create had been launched *before* the fixed binary
/// was assembled. The line reading `design.chip=nil form.chip=nil` for a composition whose tree names
/// a chip is the tell — the inputs were empty, so the old rule's answer was reproduced by the old
/// binary, not by this one.
@Test("The platform decision is logged with both inputs and the platform it resolved to")
func thePlatformDecisionIsLogged() {
    // The load path: the design's chip decides, the form's empty picker does not, and the line says so.
    #expect(CreateProjectSheet.buildPlatformLogLine(designChip: "esp32s3", formChip: "")
            == "CreateProjectSheet.create() platform: design.chip=esp32s3 "
                + "form.chip=nil -> esp32s3 platforms=[\"esp32s3\"]",
            "the line names the input that decided and the platforms the scaffold will record")

    // The shape a stale binary produces, and the reason the line exists: both inputs empty, no
    // platform — which is correct for this input and wrong for a tree that names a chip.
    #expect(CreateProjectSheet.buildPlatformLogLine(designChip: nil, formChip: "")
            == "CreateProjectSheet.create() platform: design.chip=nil "
                + "form.chip=nil -> nil platforms=[]")

    // The designed path: the form is the design, so the line's two inputs agree.
    #expect(CreateProjectSheet.buildPlatformLogLine(designChip: "esp32s3", formChip: "esp32s3")
            == "CreateProjectSheet.create() platform: design.chip=esp32s3 "
                + "form.chip=esp32s3 -> esp32s3 platforms=[\"esp32s3\"]")
}

/// **A component the design says is already here and is not** stops the run before a file is written,
/// with the reason rather than the symptom.
///
/// `idf_apply_design` reports the disagreement in `problems`, not `error`: it writes the components it
/// *can* write, and the scaffold that follows names the missing one in the application's `REQUIRES`. So
/// the run used to continue — library applied, tree scaffolded, tree filled — and fail minutes later at
/// CMake with `Failed to resolve component 'moving_average' required by component 'main'`: a line that
/// names neither the design that asked for it nor the way out. The core had already said the useful
/// thing ("the design says the library already has 'moving_average' and it does not"), and the sheet
/// was dropping it. The message is the whole point of stopping, so it is pinned here.
@Test("The design and the library disagreeing is stated, not left to the build")
func designLibraryDisagreementIsStated() {
    // Agreeing: every component the design named is in the library, so there is nothing to say and
    // nothing to stop for.
    #expect(CreateProjectSheet.designLibraryDisagreement([]) == nil)

    // The failure this pins, in the core's own words: `moving_average` marked `existing` against a
    // library whose component is `rolling_average`.
    let one = CreateProjectSheet.designLibraryDisagreement([
        "the design says the library already has 'moving_average' and it does not — add it "
            + "(marking it `stub`, as one the design asks to be written) or design it out",
    ])
    #expect(one?.contains("already has 'moving_average' and it does not") == true,
            "the core's reason survives into the sheet's message")
    #expect(one?.contains("Fix the composition or the library and create it again") == true,
            "and so does what to do about it")

    // Several at once: the count is in the finish line, and each reason is its own bullet.
    let two = CreateProjectSheet.designLibraryDisagreement(["one component", "another component"])
    #expect(two?.contains("\n• one component\n• another component") == true,
            "each problem is its own bullet")
}


/// The fill's plan is split: the writes the sheet executes, and the build steps it defers to its own
/// build at the end.
///
/// The plan's trailing `Build` step used to be executed along with the writes, and it failed on a
/// build directory the build itself had not created yet — a `✗ Build` in the log for work that had
/// not happened. The split is the rule that keeps the log honest, so it is pinned here.
@Test("The fill plan's build steps are deferred to the build at the end")
func fillPlanBuildStepsAreDeferred() {
    func write(_ id: String) -> CreationStep {
        CreationStep(id: id, stepType: .writeSourceFile, description: id,
                     status: .pending, parameters: nil, result: nil)
    }
    let build = CreationStep(id: "fill-8", stepType: .build, description: "build",
                             status: .pending, parameters: nil, result: nil)

    let (writes, deferred) = CreateProjectSheet.writableSteps(from: [write("fill-1"), write("fill-2"), build])
    #expect(writes.map(\.id) == ["fill-1", "fill-2"])
    #expect(deferred == 1)

    let (none, zero) = CreateProjectSheet.writableSteps(from: [write("fill-1")])
    #expect(none.map(\.id) == ["fill-1"])
    #expect(zero == 0)

    let (empty, nothing) = CreateProjectSheet.writableSteps(from: [])
    #expect(empty.isEmpty)
    #expect(nothing == 0)
}

/// **The core's report when a scaffold did not use the design it was handed**, as the wizard receives
/// it — and the shape it has when there is nothing to say.
///
/// The rule is the core's (`idf_projects::design_for_scaffold`): a tree that already states a design in
/// `composition.spire` decides, because a design changes by editing that file. So the decomposition the
/// wizard reviewed can be dropped, and the core says so in the spec it returns. It is optional in both
/// directions, and that is the part worth pinning: a field that stopped decoding would be `nil`, and a
/// `nil` warning is the *ordinary* case — the notice would never appear, with nothing to point at.
@Test("A dropped design arrives as the core's own sentence, and its absence as nothing")
func aScaffoldReportDecodesAndReEncodes() throws {
    let reported = """
    {
      "structural_files": ["main/CMakeLists.txt"],
      "fill_roots": ["main"],
      "dependency_sections": ["main/idf_component.yml"],
      "platform_targets": ["esp32c6"],
      "build_system": "ESP-IDF",
      "files": [],
      "structure": "idf_application",
      "embedded": true,
      "design_warning": "the design in composition.spire decides — the design passed in was not used"
    }
    """
    let spec = try JSONDecoder().decode(ScaffoldSpec.self, from: Data(reported.utf8))
    #expect(spec.designWarning?.contains("composition.spire") == true,
            "the core's wording is shown verbatim: \(spec.designWarning ?? "nil")")
    #expect(spec.structure == "idf_application", "and the fields around it still decode")
    #expect(spec.fillRoots == ["main"])

    // Nothing to say ⇒ no key at all: an older core sends exactly this, and it is the ordinary case
    // rather than a report with no words.
    let quiet = """
    {
      "structural_files": ["main/CMakeLists.txt"],
      "fill_roots": ["main"],
      "dependency_sections": ["main/idf_component.yml"],
      "platform_targets": ["esp32c6"],
      "build_system": "ESP-IDF",
      "structure": "idf_application",
      "embedded": true
    }
    """
    let quietSpec = try JSONDecoder().decode(ScaffoldSpec.self, from: Data(quiet.utf8))
    #expect(quietSpec.designWarning == nil)
    #expect(quietSpec.files.isEmpty, "and the fields with defaults still default")

    // What the bridge hands back to the core carries it only when there is one — the round trip the
    // fill and a repair make, where a warning turned into `null` would say something the core never did.
    let back = try JSONDecoder().decode(ScaffoldSpec.self, from: try JSONEncoder().encode(spec))
    #expect(back.designWarning == spec.designWarning)
    let quietJSON = String(decoding: try JSONEncoder().encode(quietSpec), as: UTF8.self)
    #expect(!quietJSON.contains("design_warning"),
            "nothing to say is sent as nothing: \(quietJSON)")
}

/// The edit-history record, as `component_edit_history` emits it.
///
/// The pane exists to show the **whole** run — what the chip gate did, what could not be checked, and
/// the files that were reverted or left alone — so every one of those keys is pinned here against the
/// exact JSON `record_to_json` produces. A key that silently stopped decoding would drop a fact the
/// user opened the log to read, and would do it without an error.
///
/// The second half is the reason the decoder is lenient: records written before those properties
/// existed are still in the graph, and a history that refuses to decode is worse than one with a gap.
@Test("An edit record decodes the whole run, and a legacy one still opens")
func editRecordsDecode() throws {
    let json = """
    {
      "instruction": "Read PM2.5 from the SPS30.",
      "success": false,
      "verified": "HostOnly",
      "up_to_date": false,
      "files_changed": ["src/sps30.cpp"],
      "files_reverted": ["src/sps30_test.cpp"],
      "files_skipped": [],
      "chip_build": "not run: no chip named",
      "caveats": ["no board: the chip build did not run"],
      "component": "sps30",
      "library": "spire-idf",
      "at": "2026-10-03T14:09:37+00:00"
    }
    """
    let record = try JSONDecoder().decode(ComponentEditRecord.self, from: Data(json.utf8))
    #expect(record.component == "sps30")
    #expect(record.library == "spire-idf", "what the row reads as when several histories share a list")
    #expect(!record.success, "the gate refused, and the log says so")
    #expect(record.verified == "HostOnly")
    #expect(record.filesChanged == ["src/sps30.cpp"])
    #expect(record.filesReverted == ["src/sps30_test.cpp"])
    #expect(record.filesSkipped.isEmpty, "an empty list is a fact — nothing was skipped")
    #expect(record.chipBuild == "not run: no chip named")
    #expect(record.caveats == ["no board: the chip build did not run"])
    #expect(record.at == "2026-10-03T14:09:37+00:00")

    // A record from before the extended keys: the six it always had, and nothing else.
    let legacy = """
    {
      "instruction": "Add a temperature read.",
      "success": true,
      "verified": "WithTarget",
      "up_to_date": true,
      "files_changed": [],
      "at": "2026-09-01T08:00:00+00:00"
    }
    """
    let old = try JSONDecoder().decode(ComponentEditRecord.self, from: Data(legacy.utf8))
    #expect(old.instruction == "Add a temperature read.", "the record is not lost")
    #expect(old.upToDate, "`up_to_date` is not `success` — nothing moved because it need not")
    #expect(old.chipBuild.isEmpty, "a missing key reads as absent, not as a refusal")
    #expect(old.caveats.isEmpty)
    #expect(old.component.isEmpty)
}

/// The two knowledge-graph payloads, as the graph pane consumes them.
///
/// `queryGraph` and `getRelationships` answer the same node shape with one difference that matters to
/// the pane: a query returns a node's edges **inside** the node (`relationships`), while a traversal
/// returns them as one flat list beside the nodes. Both are pinned here — including the camelCase
/// `fromId`/`toId`, which is what the Rust side actually sends (`e.from_id` / `e.to_id` in
/// `project_query.rs`) and the one thing a hand-written model gets wrong. Snake_case here would not
/// throw; it would decode every node and then draw no edges at all.
@Test("A graph query and a traversal decode, edges and all")
func knowledgeGraphsDecode() throws {
    let query = """
    {
      "total": 2,
      "nodes": [
        {
          "id": "n-project", "name": "Spire", "type": "Project", "subtype": null,
          "description": "The project.", "properties": { "root": "/x" },
          "relationships": [
            { "id": "r1", "type": "BelongsTo", "fromId": "n-entity", "toId": "n-project" },
            { "id": "r2", "type": "HasEdit", "fromId": "n-project", "toId": "n-entity" }
          ]
        },
        {
          "id": "n-entity", "name": "SPS30", "type": "Entity", "subtype": "Class",
          "description": null, "properties": {},
          "relationships": [
            { "id": "r1", "type": "BelongsTo", "fromId": "n-entity", "toId": "n-project" }
          ]
        }
      ]
    }
    """
    let graph = try JSONDecoder().decode(KnowledgeGraphResult.self, from: Data(query.utf8))
    #expect(graph.total == 2)
    #expect(graph.nodes.count == 2)
    #expect(graph.nodes[0].type == "Project", "the type is what the ring layout colours by")
    #expect(graph.nodes[0].subtype == nil, "absent, not empty")
    #expect(graph.nodes[1].subtype == "Class", "the finer label survives")
    #expect(graph.nodes[1].relationships?.count == 1)
    #expect(graph.nodes[1].relationships?.first?.fromId == "n-entity",
            "camelCase is what the core sends; snake_case would decode to an empty pane")
    #expect(graph.nodes[1].relationships?.first?.toId == "n-project")
    #expect(graph.nodes[1].relationships?.first?.type == "BelongsTo")
    #expect(graph.nodes[0].relationships?.first?.weight == nil,
            "a query edge carries no weight; it is optional, not required")
    #expect(graph.nodes[0].description == "The project.", "a description is carried when present")

    // A traversal: the same nodes without `description`/`relationships`, edges in one list beside them.
    let traversal = """
    {
      "startNodeId": "n-project",
      "nodes": [
        { "id": "n-project", "name": "Spire", "type": "Project", "subtype": null },
        { "id": "n-entity", "name": "SPS30", "type": "Entity", "subtype": "Class" }
      ],
      "relationships": [
        { "id": "r1", "type": "BelongsTo", "fromId": "n-entity", "toId": "n-project", "weight": 1.0 }
      ],
      "totalNodes": 2,
      "totalRelationships": 1
    }
    """
    let edges = try JSONDecoder().decode(GraphRelationshipsResult.self, from: Data(traversal.utf8))
    #expect(edges.startNodeId == "n-project", "the traversal is anchored, and the pane needs the anchor")
    #expect(edges.nodes.count == 2)
    #expect(edges.nodes[0].relationships == nil, "a traversal node carries no nested edges")
    #expect(edges.relationships.count == 1)
    #expect(edges.relationships[0].weight == 1.0)
    #expect(edges.totalNodes == 2 && edges.totalRelationships == 1)
}

/// The contract-drift report, as `modify-contract` returns it.
///
/// The pane's whole point is the **arithmetic**: `drift_before` against `drift_after`, with the
/// interfaces that closed, that were rolled back, and that are still open. A count that failed to
/// decode would not throw — it would read as `0`, which is the one number the report must not invent.
/// The refusal shape (`{"success": false, "error": …}`) is the second half: that is what "no platform
/// selected" comes back as, and it must open as a refusal rather than as an unreadable error.
@Test("A drift report decodes its counts, and a refusal decodes as a refusal")
func driftReportsDecode() throws {
    let json = """
    {
      "success": true,
      "drift_before": 4,
      "drift_after": 1,
      "gaps_closed": ["rpi5/ISensor", "rpi5/IGpio"],
      "gaps_reverted": ["rock3c/IUart"],
      "gaps_remaining": ["rpi5/IClock"],
      "output": "closed:\\n  rpi5/ISensor\\n  rpi5/IGpio\\nreverted:\\n  rock3c/IUart\\nstill open:\\n  rpi5/IClock"
    }
    """
    let report = try JSONDecoder().decode(ModifyContractReport.self, from: Data(json.utf8))
    #expect(report.success)
    #expect(report.driftBefore == 4, "a count is a number, not a string the pane parses")
    #expect(report.driftAfter == 1)
    #expect(report.gapsClosed == ["rpi5/ISensor", "rpi5/IGpio"], "`platform/interface` is the unit")
    #expect(report.gapsReverted == ["rock3c/IUart"],
            "a rolled-back round is a fact the report keeps, not one it hides")
    #expect(report.gapsRemaining == ["rpi5/IClock"])
    #expect(report.output.contains("still open:\n  rpi5/IClock"),
            "the coordinator's own summary arrives verbatim, newlines and all")
    #expect(report.error == nil, "a run carries no error, even when it left drift behind")

    // The refusal: the shape the coordinator sends when there is no path, and before that when there
    // is no platform. The cascade never ran, so there is nothing to count — and `error` is the reason.
    let refusal = """
    {
      "success": false,
      "error": "modify-contract needs a project path",
      "output": "Modify contract: needs a project path; nothing was changed."
    }
    """
    let refused = try JSONDecoder().decode(ModifyContractReport.self, from: Data(refusal.utf8))
    #expect(!refused.success)
    #expect(refused.error == "modify-contract needs a project path", "the reason survives as the reason")
    #expect(refused.output.contains("nothing was changed"), "so does what the refusal says happened")
    #expect(refused.driftBefore == 0 && refused.driftAfter == 0)
    #expect(refused.gapsClosed.isEmpty && refused.gapsReverted.isEmpty && refused.gapsRemaining.isEmpty)
}



// MARK: - The experimental board/resource flow

/// A **board profile** decodes its declared parts, and a part's own values are read by name.
///
/// The `devices` array is the read the flow seeds its resource list from: the name is what a
/// resource is called, the role is the capability it *is*, and `bus`/`address` are what a spec
/// carries. A dropped key here would show a resource with no wiring rather than an error — the same
/// quiet failure this suite pins elsewhere — so each is asserted.
@Test("A board profile decodes its declared parts")
func boardProfileDecodesDevices() throws {
    let profile = try JSONDecoder().decode(BoardProfile.self, from: Data(profileFixture.utf8))

    #expect(profile.board == "m5stack-core-s3")
    #expect(profile.devices.count == 12, "the board's whole `devices:` block comes back")
    #expect(
        profile.devices.map(\.name) == [
            "audio_in", "audio_out", "camera", "display", "expander", "imu",
            "magnetometer", "pmu", "proximity", "rtc", "sd", "touch",
        ],
        "the core sends them in name order, and the flow shows them in that order")

    let display = try #require(profile.devices.first { $0.name == "display" })
    #expect(display.role == "media.display")
    #expect(display.part == "ILI9342C")
    #expect(display.bus == "display")
    #expect(display.address == "", "a part the board gave no address is empty, not \"\" written by the core")

    let rtc = try #require(profile.devices.first { $0.name == "rtc" })
    #expect(rtc.address == "0x51", "an address is a property read by name")
    #expect(rtc.bus == "i2c_internal")

    // `resolved_profile` sends `via`/`provides` on a `realizes` entry; the shared `CapabilityEdge`
    // ignores unknown keys rather than failing, so the profile still decodes whole.
    #expect(profile.realizes.count == 7)
    #expect(profile.realizes.first?.capability == "io.usb")
    #expect(profile.pins.map(\.function).contains("i2c_internal"), "the wiring reads back too")
}

/// The **capability vocabulary** decodes as categories of paths — the catalogue a resource's role is
/// picked from, and the same one the core validates against.
@Test("The capability vocabulary decodes as categories")
func vocabularyDecodesCategories() throws {
    // The live `capabilities/vocabulary` reply, verbatim — categories in the vocabulary's own order,
    // paths sorted within each.
    let json = """
    { "categories": [
        { "capabilities": ["compute.cpu", "compute.gpu", "compute.ml"], "category": "compute" },
        { "capabilities": ["io.buses", "io.can", "io.ethernet", "io.expander", "io.gpio", "io.pcie", "io.usb"], "category": "io" },
        { "capabilities": ["media.audio", "media.camera", "media.display", "media.touch", "media.video.decode", "media.video.encode"], "category": "media" },
        { "capabilities": ["power.battery", "power.pmu", "power.poe"], "category": "power" },
        { "capabilities": ["radio.bluetooth", "radio.ieee802154.matter", "radio.ieee802154.thread", "radio.ieee802154.zigbee", "radio.wifi"], "category": "radio" },
        { "capabilities": ["sensing.imu", "sensing.magnetometer", "sensing.particulate", "sensing.proximity", "sensing.temperature"], "category": "sensing" },
        { "capabilities": ["storage.emmc", "storage.nvme", "storage.sd"], "category": "storage" },
        { "capabilities": ["timekeeping.rtc"], "category": "timekeeping" }
    ] }
    """
    let reply = try JSONDecoder().decode(VocabularyReply.self, from: Data(json.utf8))
    #expect(
        reply.categories.map(\.category) == [
            "compute", "io", "media", "power", "radio", "sensing", "storage", "timekeeping",
        ])
    #expect(reply.categories[2].capabilities.contains("media.display"))

    // The paths a board actually declares a part for are all in the catalogue — the picker and the
    // writer's validator are one vocabulary, so anything offered can be written.
    let offered = Set(reply.categories.flatMap(\.capabilities))
    for role in ["media.display", "timekeeping.rtc", "io.expander", "sensing.particulate"] {
        #expect(offered.contains(role), "\(role) is a capability the flow may offer")
    }

    // A role's own default resource name is its last segment — the same rule
    // `toggleCatalogueResource` applies.
    var state = BoardResourceFlowState()
    state.toggleCatalogueResource(role: "sensing.particulate")
    #expect(state.resources.map(\.name) == ["particulate"], "the catalogue's word becomes the resource's name")
}

/// `CapabilityBlocks` decodes **tolerantly**: a `platforms/config` reply from a core older than the
/// device layer omits `devices`, and a required field would fail the whole reply — an empty screen
/// rather than a missing group.
@Test("Capability blocks decode without a devices key")
func capabilityBlocksTolerateMissingDevices() throws {
    let older = """
    { "realizes": [], "provides": [], "carries": [], "pins": [] }
    """
    let blocks = try JSONDecoder().decode(CapabilityBlocks.self, from: Data(older.utf8))
    #expect(blocks.devices.isEmpty, "absent devices is `no parts`, not a decode failure")
    #expect(blocks.isEmpty)

    let current = """
    {
      "realizes": [
        { "capability": "io.usb", "properties": { "ports": 1, "role": "otg" } },
        { "capability": "media.display", "properties": { "interface": "spi" } }
      ],
      "provides": [], "carries": [],
      "pins": [ { "function": "i2c_internal", "properties": { "scl": "GPIO11", "sda": "GPIO12" } } ],
      "devices": [
        { "name": "display", "properties": { "bus": "display", "part": "ILI9342C" }, "role": "media.display" },
        { "name": "rtc", "properties": { "address": "0x51", "bus": "i2c_internal", "part": "BM8563" }, "role": "timekeeping.rtc" }
      ]
    }
    """
    let withDevice = try JSONDecoder().decode(CapabilityBlocks.self, from: Data(current.utf8))
    #expect(withDevice.devices.count == 2)
    // A `platforms/config` block sends `capability` + `properties` and no `via`/`provides` — the
    // other `realizes` shape the shared `CapabilityEdge` must accept (the profile sends both keys,
    // null). Either way the block decodes whole.
    #expect(withDevice.realizes.map(\.capability) == ["io.usb", "media.display"])
    #expect(withDevice.pins.map(\.function) == ["i2c_internal"], "the wiring is read even when nothing realizes it")
    #expect(withDevice.devices.map(\.name) == ["display", "rtc"])
    #expect(!withDevice.isEmpty, "a declared part is something, so the block is not empty")
}


/// The flow's state, without a view: a chip is not offered as a board, choosing a board offers its
/// parts **unselected** — the board *can* supply each, the application decides it *uses* it — a part
/// toggles `via: board`, an added role becomes a `library` resource named by its last segment, a
/// duplicate is refused, a board part displaces an added resource that took its name, and a role the
/// board carries is the board's to toggle alone — the catalogue never adds it.
@Test("The board/resource flow toggles board parts and gates Continue")
func boardResourceFlowTogglesParts() throws {
    let platformsJSON = "[\(platformFixture(id: "m5stack-core-s3", kind: "board", embedded: true)),"
        + "\(platformFixture(id: "esp32p4", kind: "chip", embedded: true)),"
        + "\(platformFixture(id: "rpi5", kind: nil, embedded: false))]"
    let platforms: [Platform] = try MessageSerializer.decode(Data(platformsJSON.utf8))

    #expect(
        BoardResourceFlowState.boards(from: platforms).map(\.id) == ["m5stack-core-s3"],
        "a chip is not a board an application is built for")

    let profile = try JSONDecoder().decode(BoardProfile.self, from: Data(profileFixture.utf8))
    var state = BoardResourceFlowState()
    state.choose(boardID: "m5stack-core-s3", profile: profile)

    #expect(state.selectedBoardID == "m5stack-core-s3")
    #expect(state.resources.isEmpty, "a board's parts are offered, not chosen for the application")
    #expect(!state.canContinue, "a board alone declares no resource to build against")

    // A part the board declares is selected by name: it enters the set reached `via: board`.
    state.toggleBoardPart(name: "display")
    let display = try #require(state.resources.first { $0.name == "display" })
    #expect(display.via == .board, "a board's part is reached through the board")
    #expect(display.bus == "display", "the board's bus rides into the draft")
    #expect(display.json["address"] == nil, "an empty address is omitted, not written blank")
    #expect(state.isBoardPartSelected("display"))
    #expect(state.canContinue, "a board and one well-formed resource is enough")

    // Selecting the same part again takes it back out — the toggle is symmetric.
    state.toggleBoardPart(name: "display")
    #expect(state.resources.isEmpty)
    #expect(!state.canContinue)

    // A name the board does not declare is ignored: the palette is the board's facts, not free text.
    state.toggleBoardPart(name: "lidar")
    #expect(state.resources.isEmpty)

    // Selecting the board's `rtc` carries its wiring and role, as `display` did.
    state.toggleBoardPart(name: "rtc")
    let rtc = try #require(state.resources.first { $0.name == "rtc" })
    #expect(rtc.json["address"] as? String == "0x51")
    #expect(rtc.role == "timekeeping.rtc")
    #expect(
        state.resources.contains { $0.role == "timekeeping.rtc" },
        "the selected part covers its capability — what the catalogue reads to disable a twin")

    // A catalogue path: named by the role's last segment, reached through the library in v1.
    state.toggleCatalogueResource(role: "sensing.particulate")
    #expect(state.resources.last?.name == "particulate")
    #expect(state.resources.last?.via == .library, "the forward-compat hook, for now")
    #expect(state.declaresRole("sensing.particulate"), "an addition covers its capability")
    #expect(state.declaredVia(role: "sensing.particulate") == .library, "an addition is the application's")
    #expect(state.declaredVia(role: "timekeeping.rtc") == .board, "the board's part is the board's")
    #expect(state.declaredVia(role: "radio.wifi") == nil, "a path nothing carries is open")

    // The toggle is symmetric: choosing the same path again takes the addition back out.
    state.toggleCatalogueResource(role: "sensing.particulate")
    #expect(!state.resources.contains { $0.name == "particulate" })
    #expect(!state.declaresRole("sensing.particulate"))

    // An addition made before a board is chosen survives the choice (only the board's own selected
    // parts are dropped); selecting the board's same-named part then displaces it — one name is one
    // resource, and the board's is the real one.
    var collided = BoardResourceFlowState()
    collided.toggleCatalogueResource(role: "timekeeping.rtc")  // name "rtc", no board yet
    #expect(collided.resources.map(\.name) == ["rtc"])
    collided.choose(boardID: "m5stack-core-s3", profile: profile)
    #expect(
        collided.resources.map(\.name) == ["rtc"],
        "choosing a board drops only its own selected parts, not the application's additions")
    collided.toggleBoardPart(name: "rtc")
    #expect(collided.resources.count == 1, "one name is one resource")
    #expect(
        collided.resources.first?.via == .board,
        "the board's `rtc` is the real one, so the added draft is dropped")

    // A role the board carries as a part is board-owned: the catalogue leaves it alone even before the
    // part is selected, because the part row is its only control. This is what keeps the board part and
    // the category in step — a `power.pmu` ticked in the category can never be a twin of the board's
    // `pmu`.
    var boardOwned = BoardResourceFlowState()
    boardOwned.choose(boardID: "m5stack-core-s3", profile: profile)
    #expect(boardOwned.boardCarriesRole("power.pmu"), "the board's `pmu` device carries the role")
    boardOwned.toggleCatalogueResource(role: "power.pmu")
    #expect(boardOwned.resources.isEmpty, "a carried role is the board's, so the catalogue cannot add it")
    #expect(!boardOwned.declaresRole("power.pmu"), "nothing is declared until the part row is chosen")
    boardOwned.toggleBoardPart(name: "pmu")
    #expect(boardOwned.declaredVia(role: "power.pmu") == .board, "the part row is the only control")
    #expect(boardOwned.resources.map(\.name) == ["pmu"], "one resource, reached through the board")
    boardOwned.toggleCatalogueResource(role: "power.pmu")
    #expect(boardOwned.declaredVia(role: "power.pmu") == .board, "nor can the catalogue take it back")

    // Switching boards drops the previous board's selected parts but keeps the application's additions.
    var switched = BoardResourceFlowState()
    switched.choose(boardID: "m5stack-core-s3", profile: profile)
    switched.toggleBoardPart(name: "display")
    switched.toggleCatalogueResource(role: "sensing.particulate")
    switched.choose(boardID: "m5stack-core-s3", profile: profile)
    #expect(
        switched.resources.map(\.name) == ["particulate"],
        "the new board's palette arrives unselected; the application's addition stays")

    #expect(!BoardResourceFlowState().canContinue, "no board, nothing to continue with")
}

/// A `platforms/list`-shaped fixture — the fields a `Platform` needs, with `kind` present only when
/// the caller passes one (the absent case is a payload from an older core).
private func platformFixture(id: String, kind: String?, embedded: Bool) -> String {
    let kindLine = kind.map { "\"kind\": \"\($0)\"," } ?? ""
    return """
    {
      "id": "\(id)", "name": "\(id)", "os": "esp-idf",
      "architecture": {
        "cpu_family": "xtensa", "cpu": "\(id)", "endian": "little", "target_triple": "t"
      },
      "toolchain": {
        "c": "c", "cpp": "cpp", "ar": "ar", "strip": "strip",
        "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [], "needs_exe_wrapper": false
      },
      "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
      \(kindLine) "embedded": \(embedded)
    }
    """
}

/// A `capabilities/profile` reply **copied from the core** — the live reply for `m5stack-core-s3`,
/// which is the board this suite exercises. Every key, every value and every ordering below is the
/// released core's, so the fixture cannot drift into a shape the core does not send.
///
/// The `devices` block is verbatim and complete (all twelve parts, in the core's own name order) and
/// `realizes` is verbatim (all seven, including the `via`/`provides` nulls); `pins` is abridged to
/// the three functions the assertions name — the core's reply carries all nine, and the shape is the
/// reply's.
private let profileFixture = """
{
  "board": "m5stack-core-s3",
  "realizes": [
    { "capability": "io.usb", "properties": { "ports": 1, "role": "otg" }, "provides": null, "via": null },
    { "capability": "media.audio", "properties": { "in": true, "out": true }, "provides": null, "via": null },
    { "capability": "media.camera", "properties": { "interface": "parallel" }, "provides": null, "via": null },
    { "capability": "media.display", "properties": { "interface": "spi" }, "provides": null, "via": null },
    { "capability": "power.battery", "properties": { "charger": true }, "provides": null, "via": null },
    { "capability": "sensing.imu", "properties": {}, "provides": null, "via": null },
    { "capability": "storage.sd", "properties": { "interface": "spi" }, "provides": null, "via": null }
  ],
  "carries": [],
  "pins": [
    { "function": "display", "properties": { "cs": "GPIO3", "dc": "GPIO35", "mosi": "GPIO37", "sck": "GPIO36" } },
    { "function": "i2c_internal", "properties": { "scl": "GPIO11", "sda": "GPIO12" } },
    { "function": "touch", "properties": { "int": "P1_2", "reset": "P0_0", "via": "aw9523b" } }
  ],
  "devices": [
    { "name": "audio_in", "properties": { "address": "0x40", "bus": "audio", "part": "ES7210" }, "role": "media.audio" },
    { "name": "audio_out", "properties": { "address": "0x36", "bus": "audio", "part": "AW88298" }, "role": "media.audio" },
    { "name": "camera", "properties": { "address": "0x21", "bus": "camera", "part": "GC0308" }, "role": "media.camera" },
    { "name": "display", "properties": { "bus": "display", "part": "ILI9342C" }, "role": "media.display" },
    { "name": "expander", "properties": { "address": "0x58", "bus": "i2c_internal", "part": "AW9523B" }, "role": "io.expander" },
    { "name": "imu", "properties": { "address": "0x69", "bus": "i2c_internal", "part": "BMI270" }, "role": "sensing.imu" },
    { "name": "magnetometer", "properties": { "address": "0x10", "bus": "i2c_internal", "part": "BMM150" }, "role": "sensing.magnetometer" },
    { "name": "pmu", "properties": { "address": "0x34", "bus": "i2c_internal", "part": "AXP2101" }, "role": "power.pmu" },
    { "name": "proximity", "properties": { "address": "0x23", "bus": "i2c_internal", "part": "LTR553" }, "role": "sensing.proximity" },
    { "name": "rtc", "properties": { "address": "0x51", "bus": "i2c_internal", "part": "BM8563" }, "role": "timekeeping.rtc" },
    { "name": "sd", "properties": { "bus": "sd", "part": "microSD" }, "role": "storage.sd" },
    { "name": "touch", "properties": { "address": "0x38", "bus": "i2c_internal", "part": "FT6336" }, "role": "media.touch" }
  ]
}
"""

