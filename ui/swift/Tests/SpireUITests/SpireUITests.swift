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


// MARK: - The new-project wizard's tree

/// The platforms the branch filter is pinned against: a board of each family, a Linux SBC (embedded,
/// **no** `family`), and the host.
///
/// Shaped like the real `platforms/list` payload — the model's `architecture`/`toolchain`/`sysroot`
/// are required keys, so a fixture that omitted them would be testing a payload the core never sends.
private func wizardPlatforms() throws -> [Platform] {
    try MessageSerializer.decode(Data("""
    [{ "id": "esp32c6", "name": "ESP32-C6", "os": "esp-idf", "family": "esp32", "embedded": true,
       "architecture": { "cpu_family": "riscv", "cpu": "esp32c6", "endian": "little",
                         "target_triple": "riscv32imac-esp-espidf" },
       "toolchain": { "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
                      "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
                      "needs_exe_wrapper": false },
       "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
       "rust": { "target": "riscv32imac-esp-espidf", "idf_target": "esp32c6", "flash": "espflash" }
    }, { "id": "rp2040", "name": "Raspberry Pi Pico", "os": "rp2040", "family": "rp2040", "embedded": true,
       "architecture": { "cpu_family": "arm", "cpu": "rp2040", "endian": "little",
                         "target_triple": "thumbv6m-none-eabi" },
       "toolchain": { "c": "clang", "cpp": "clang++", "ar": "llvm-ar", "strip": "llvm-strip",
                      "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
                      "needs_exe_wrapper": false },
       "sysroot": { "root": "", "lib_dirs": [], "include_dirs": [], "pkg_config_libdir": [] },
       "rust": { "target": "thumbv6m-none-eabi", "idf_target": "RP2040", "flash": "picotool" }
    }, { "id": "rock3c", "name": "Radxa Rock 3C", "os": "linux", "embedded": true,
       "architecture": { "cpu_family": "aarch64", "cpu": "armv8-a", "endian": "little",
                         "target_triple": "aarch64-linux-gnu" },
       "toolchain": { "c": "aarch64-linux-gnu-gcc", "cpp": "aarch64-linux-gnu-g++",
                      "ar": "aarch64-linux-gnu-ar", "strip": "aarch64-linux-gnu-strip",
                      "c_args_extra": [], "cpp_args_extra": [], "linker_args_extra": [],
                      "needs_exe_wrapper": false },
       "sysroot": { "root": "/usr/aarch64-linux-gnu", "lib_dirs": [], "include_dirs": [],
                    "pkg_config_libdir": [] }
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
}

/// The wizard is a **tree**, so which questions get asked is a property of the branch.
///
/// Pinned because the step count is now data rather than an enum's order: a branch that gained a step
/// it does not need (or lost one it does) would still compile, and would show up only as a user being
/// asked something irrelevant — or never being asked their board.
@Test("The wizard's tree decides its own steps")
func wizardTreeDecidesItsOwnSteps() {
    // Native: host only — the shape, then the details.
    #expect(NewProjectView.path(environment: .native, deviceClass: .linuxSbc)
            == [.environment, .nativeKind, .details])

    // Linux SBC: the Meson shape, unchanged — targets, then single-source vs HAL.
    #expect(NewProjectView.path(environment: .embedded, deviceClass: .linuxSbc)
            == [.environment, .deviceClass, .targets, .structure, .details])

    // Controller: container vs application comes *before* the board, because the role decides what a
    // board selection means — a BSP to write, or one to depend on.
    let controller = NewProjectView.path(environment: .embedded, deviceClass: .controller)
    #expect(controller == [.environment, .deviceClass, .controllerRole, .targets, .details])
    #expect(controller.firstIndex(of: .controllerRole)! < controller.firstIndex(of: .targets)!)

    // An application is asked one more question than a container: which container it builds against.
    // Nothing else in the wizard implies it, and the core refuses an app without it.
    let application = NewProjectView.path(
        environment: .embedded, deviceClass: .controller, controllerRole: .application
    )
    #expect(application == [.environment, .deviceClass, .controllerRole, .targets, .containerProject, .details])
    #expect(application.last == .details, "the container is chosen before the name, not after")
}


/// Every leaf is exactly one `ProjectStructure` key — the whole contract with the core.
///
/// Pinned because an unknown key falls back to `native` **silently** (`ProjectStructure::from_str`),
/// so a typo here would scaffold a host crate for a firmware choice and report nothing.
@Test("Each leaf of the wizard becomes one project structure")
func wizardLeavesBecomeProjectStructures() {
    func key(environment: NewProjectView.ProjectEnvironment,
             nativeKind: NewProjectView.NativeKind = .spireApp,
             deviceClass: NewProjectView.DeviceClass = .linuxSbc,
             controllerRole: NewProjectView.ControllerRole = .container,
             useHal: Bool = true) -> String {
        NewProjectView.structureKey(environment: environment, nativeKind: nativeKind,
                                    deviceClass: deviceClass, controllerRole: controllerRole,
                                    useHal: useHal)
    }

    // Native: the Spire UI App, or the CLI.
    #expect(key(environment: .native, nativeKind: .spireApp) == "spire_app")
    #expect(key(environment: .native, nativeKind: .cli) == "native")

    // Linux SBC: the two Meson shapes, chosen by the structure step.
    #expect(key(environment: .embedded, deviceClass: .linuxSbc, useHal: true) == "hal")
    #expect(key(environment: .embedded, deviceClass: .linuxSbc, useHal: false) == "single_source")

    // Controller: the container — the framework, the drivers and the BSPs, scaffolded by the core.
    #expect(key(environment: .embedded, deviceClass: .controller,
                controllerRole: .container) == "embedded")

    // Controller: the application, which path-deps a container rather than containing it.
    #expect(key(environment: .embedded, deviceClass: .controller,
                controllerRole: .application) == "embedded_app")

    // Nothing in the tree can produce a key the core would not understand — both embedded keys exist
    // now (`embedded` for the container, `embedded_app` for the application), so this set is the whole
    // contract with `ProjectStructure`.
    let known: Set<String> = ["native", "single_source", "hal", "spire_app", "embedded", "embedded_app"]
    for environment in NewProjectView.ProjectEnvironment.allCases {
        for kind in NewProjectView.NativeKind.allCases {
            for device in NewProjectView.DeviceClass.allCases {
                for role in NewProjectView.ControllerRole.allCases {
                    for useHal in [true, false] {
                        let value = NewProjectView.structureKey(
                            environment: environment, nativeKind: kind, deviceClass: device,
                            controllerRole: role, useHal: useHal)
                        #expect(known.contains(value), "\(environment)/\(device)/\(role) → \(value)")
                    }
                }
            }
        }
    }
}

/// The two embedded branches never offer each other's targets.
///
/// Pinned because the split is the registry's `family` flag, and offering a Linux SBC to a Controller
/// (or a board to the Meson path) is a mistake the *build* refuses much later — after the plan.
@Test("Boards and Linux SBCs are offered to their own branch only")
func wizardBranchTargetsDoNotOverlap() throws {
    let platforms = try wizardPlatforms()

    let boards = NewProjectView.offeredTargets(platforms, environment: .embedded, deviceClass: .controller)
    #expect(boards.map(\.id) == ["esp32c6", "rp2040"], "boards: \(boards.map(\.id))")

    let sbcs = NewProjectView.offeredTargets(platforms, environment: .embedded, deviceClass: .linuxSbc)
    #expect(sbcs.map(\.id) == ["rock3c"], "an embedded target with no family is an SBC: \(sbcs.map(\.id))")

    // Native has no targets at all, and the host is never one: `embedded` is the registry's answer,
    // not a rule this view re-derives.
    #expect(NewProjectView.offeredTargets(platforms, environment: .native, deviceClass: .linuxSbc).isEmpty)
    #expect(!boards.contains { !$0.embedded } && !sbcs.contains { !$0.embedded })
}

