import Testing
@testable import SpireUI

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
