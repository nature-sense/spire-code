// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **SpireApp** scaffold — the Rust/SwiftUI monorepo shape.
//!
//! Emits a complete `spire-<name>` project: a Cargo workspace with one
//! `crates/spire-<name>` crate (rlib + cdylib) built on `spire-actor` +
//! `spire-core` (sibling path deps), a minimum-launchable SwiftUI app in
//! `ui/swift` that embeds the dylib over the JSON FFI, and the
//! `build/assemble-app.sh` / `Makefile` glue that assembles a `.app` bundle.
//!
//! `[profile.release] strip = "none"` is baked in so the Rust dylib stays
//! loadable at `dlopen` time (rust-lang/rust#157750).

use spire_core::build_types::{ProjectStructure, SourceRole};

/// Emit the full SpireApp monorepo `ScaffoldOutput`. `project_name` is the
/// crate + bundle name (convention: `spire-<name>`); the crate id is
/// lowercased and space-normalized so any wizard-typed name yields a valid
/// Cargo package id.
pub(crate) fn spire_app_scaffold(project_name: &str) -> super::ScaffoldOutput {
    // Crate/dylib name: lowercase, spaces → hyphens (a valid Rust crate id).
    let name = project_name.trim().to_lowercase().replace(' ', "-");
    let display = project_name.trim();

    let workspace_cargo = r#"# __DISPLAY__ — Spire application monorepo (Rust core + SwiftUI app).
#
# Layout:
#   crates/__NAME__/   Rust core (rlib + cdylib) on spire-actor + spire-core
#   ui/swift/          host SwiftUI app embedding the core via the JSON FFI
#   build/             app assembly (build/assemble-app.sh → <App>.app)

[workspace]
resolver = "2"
members = ["crates/__NAME__"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "GPL-3.0-or-later"

# Spire framework — sibling repos, resolved by path (must sit next to this repo).
[workspace.dependencies]
spire-actor = { path = "../spire-actor" }
spire-core = { path = "../spire-core" }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Do NOT strip in release builds (rust-lang/rust#157750): rustc's
# `-C strip=debuginfo` post-link strip step misaligns the LINKEDIT string pool
# for cdylibs that use chained fixups, and dyld then rejects the dylib at
# dlopen time. `strip = "none"` keeps the FFI dylib loadable.
[profile.release]
strip = "none"
"#;

    let crate_cargo = r#"[package]
name = "__NAME__"
version.workspace = true
edition.workspace = true
description = "__NAME__ — Spire-based application core"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
spire-actor = { workspace = true }
spire-core = { workspace = true }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
"#;

    let lib_rs = r##"//! __NAME__ — Spire-based application crate (the Rust core the SwiftUI app
//! embeds as `lib__NAME__.dylib`).
//!
//! Built on spire-actor (the actor runtime) and spire-core (embedding/RAG,
//! build metadata, LLM subsystems, config). The SwiftUI app talks JSON over
//! the `spire_send_json` FFI entry point below.
#![allow(dead_code)]

use std::ffi::{CStr, CString};

/// Minimal JSON FFI entry point (mirrors spire-code's `spire_send_json`).
/// The fill phase replaces this stub with the real application logic built on
/// spire-actor + spire-core.
#[no_mangle]
pub extern "C" fn spire_send_json(
    request: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    let _req = unsafe { CStr::from_ptr(request) }.to_string_lossy().to_string();
    let reply = r#"{"ok":true,"result":{"status":"scaffold","core":"__NAME__"}}"#;
    CString::new(reply)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Free a string previously returned by [`spire_send_json`].
#[no_mangle]
pub extern "C" fn spire_free_string(p: *mut std::os::raw::c_char) {
    if !p.is_null() {
        unsafe { drop(CString::from_raw(p)) };
    }
}
"##;

    let main_rs = r#"//! __NAME__ — command-line entry point (development).
fn main() {
    println!("__NAME__: Spire app core built. The SwiftUI app (ui/swift) embeds this crate.");
}
"#;

    let package_swift = r#"// swift-tools-version: 5.10
import PackageDescription

let package = Package(
    name: "SpireUI",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(name: "SpireUI", targets: ["SpireUI"]),
    ],
    targets: [
        .executableTarget(name: "SpireUI", path: "Sources/SpireUI"),
    ]
)
"#;

    let app_swift = r#"import SwiftUI
import AppKit

/// __DISPLAY__ — minimal launchable SwiftUI shell. The fill phase replaces
/// this with the real application UI (chat/RAG panels, project views, …).
@main
struct SpireApp: App {
    @State private var core = CoreBridge()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environment(core)
                .frame(minWidth: 800, minHeight: 500)
        }
        .windowStyle(.titleBar)
    }
}
"#;

    let content_swift = r##"import SwiftUI

/// __DISPLAY__ — placeholder shell that pings the Rust core over the FFI.
struct ContentView: View {
    @Environment(CoreBridge.self) private var core
    @State private var output = "not sent yet"

    var body: some View {
        VStack(spacing: 16) {
            Text("__DISPLAY__")
                .font(.largeTitle.weight(.semibold))
            Text(core.statusText)
                .foregroundStyle(.secondary)
            Button("Ping Rust core") {
                output = core.send(#"{"method":"status"}"#) ?? "no core"
            }
            Text(output)
                .font(.body.monospaced())
                .textSelection(.enabled)
        }
        .padding(32)
    }
}
"##;

    let core_bridge_swift = r#"import Foundation
import Observation

/// Loads the Rust core (`lib__NAME__.dylib`) and calls it over the JSON FFI.
/// The dylib lives in Contents/Frameworks when bundled, or in
/// <repo-root>/target/release during development — same convention as
/// spire-code's SpireFFI.
@Observable
final class CoreBridge {
    private(set) var statusText = "Rust core: not loaded"
    private var handle: UnsafeMutableRawPointer?

    init() { load() }

    private func load() {
        var candidates: [String] = []
        candidates.append(
            Bundle.main.bundleURL
                .appendingPathComponent("Contents")
                .appendingPathComponent("Frameworks")
                .appendingPathComponent("lib__NAME__.dylib")
                .path
        )
        candidates.append(
            URL(fileURLWithPath: #filePath)
                .deletingLastPathComponent()  // Bridge/
                .deletingLastPathComponent()  // SpireUI/
                .deletingLastPathComponent()  // Sources/
                .deletingLastPathComponent()  // swift/
                .deletingLastPathComponent()  // ui/
                .deletingLastPathComponent()  // <repo root>/
                .appendingPathComponent("target")
                .appendingPathComponent("release")
                .appendingPathComponent("lib__NAME__.dylib")
                .path
        )
        for p in candidates {
            if let h = dlopen(p, RTLD_NOW | RTLD_LOCAL) {
                handle = h
                statusText = "Rust core: loaded"
                return
            }
        }
    }

    /// Send a JSON request to the Rust core and return the JSON reply string.
    func send(_ request: String) -> String? {
        guard let h = handle,
              let sendSym = dlsym(h, "spire_send_json"),
              let freeSym = dlsym(h, "spire_free_string")
        else { return nil }
        let sendFn = unsafeBitCast(
            sendSym,
            to: (@convention(c) (UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?).self
        )
        let freeFn = unsafeBitCast(
            freeSym,
            to: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void).self
        )
        let response = request.withCString { sendFn($0) }
        defer { freeFn(response) }
        return response.map { String(cString: $0) }
    }

    deinit {
        if let h = handle { dlclose(h) }
    }
}
"#;

    let assemble_sh = r#"#!/bin/bash
# assemble-app.sh — Builds the Rust core cdylib + Swift UI and assembles them
# into a double-clickable macOS app bundle.
#
#   build/__DISPLAY__.app
#     Contents/
#       Info.plist
#       MacOS/SpireUI                    (the SwiftUI executable)
#       Frameworks/lib__NAME__.dylib     (the Rust core, loaded via dlopen)
#
# Dev-runnable only — unsigned, not notarized.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="__DISPLAY__"
APP_DIR="$ROOT/build/$APP_NAME.app"
EXEC_NAME="SpireUI"
DYLIB="lib__NAME__.dylib"
BUNDLE_ID="com.naturesense.__NAME__"
MARKETING_VERSION="0.1.0"

echo "=== 1/3 Building Rust core (cdylib) ==="
cargo build --release -p __NAME__

echo "=== 2/3 Building Swift UI ==="
(cd "$ROOT/ui/swift" && swift build -c release)

echo "=== 3/3 Assembling $APP_NAME.app ==="
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Frameworks" "$APP_DIR/Contents/Resources"

cp "$ROOT/ui/swift/.build/release/$EXEC_NAME" "$APP_DIR/Contents/MacOS/$EXEC_NAME"
cp "$ROOT/target/release/$DYLIB" "$APP_DIR/Contents/Frameworks/$DYLIB"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$EXEC_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$MARKETING_VERSION</string>
    <key>CFBundleVersion</key>
    <string>$MARKETING_VERSION</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
PLIST

chmod +x "$APP_DIR/Contents/MacOS/$EXEC_NAME"

echo ""
echo "=== Done ==="
echo "  App: $APP_DIR"
echo "  Run: open $APP_DIR   (or $APP_DIR/Contents/MacOS/$EXEC_NAME)"
"#;

    let makefile = r#"# __DISPLAY__ — Rust core (crates/__NAME__) + SwiftUI app (ui/swift).
#
#   make rust   — build the Rust core (lib__NAME__.dylib)
#   make swift  — build the SwiftUI executable
#   make app    — build everything + assemble build/__DISPLAY__.app
#   make run    — assemble + launch the app
#   make clean  — remove build artifacts

.PHONY: rust swift app run clean

rust:
	cargo build --release -p __NAME__

swift:
	cd ui/swift && swift build

app:
	@./build/assemble-app.sh

run: app
	@open ./build/__DISPLAY__.app

clean:
	cargo clean
	rm -rf build/__DISPLAY__.app
	cd ui/swift && swift package clean || true
"#;

    let gitignore = "/target/\n/build/\n.DS_Store\nui/swift/.build/\n";

    let sub = |s: &str| -> String { s.replace("__NAME__", &name).replace("__DISPLAY__", display) };

    let crate_root = format!("crates/{name}");
    let files = vec![
        super::ScaffoldFile {
            path: "Cargo.toml".to_string(),
            content: sub(workspace_cargo),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: format!("{crate_root}/Cargo.toml"),
            content: sub(crate_cargo),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: format!("{crate_root}/src/lib.rs"),
            content: sub(lib_rs),
            structural: false,
            fill_role: Some(SourceRole::App),
        },
        super::ScaffoldFile {
            path: format!("{crate_root}/src/main.rs"),
            content: sub(main_rs),
            structural: false,
            fill_role: Some(SourceRole::App),
        },
        super::ScaffoldFile {
            path: "ui/swift/Package.swift".to_string(),
            content: sub(package_swift),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "ui/swift/Sources/SpireUI/App.swift".to_string(),
            content: sub(app_swift),
            structural: false,
            fill_role: Some(SourceRole::App),
        },
        super::ScaffoldFile {
            path: "ui/swift/Sources/SpireUI/ContentView.swift".to_string(),
            content: sub(content_swift),
            structural: false,
            fill_role: Some(SourceRole::App),
        },
        super::ScaffoldFile {
            path: "ui/swift/Sources/SpireUI/Bridge/CoreBridge.swift".to_string(),
            content: sub(core_bridge_swift),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "build/assemble-app.sh".to_string(),
            content: sub(assemble_sh),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: "Makefile".to_string(),
            content: sub(makefile),
            structural: true,
            ..Default::default()
        },
        super::ScaffoldFile {
            path: ".gitignore".to_string(),
            content: gitignore.to_string(),
            structural: true,
            ..Default::default()
        },
    ];

    let build_content = files
        .iter()
        .find(|f| f.path == "Cargo.toml")
        .map(|f| f.content.clone())
        .unwrap_or_default();

    super::ScaffoldOutput {
        build_file: "Cargo.toml".to_string(),
        build_content,
        source_dir: format!("{crate_root}/src"),
        source_file: "main.rs".to_string(),
        source_content: files
            .iter()
            .find(|f| f.path == format!("{crate_root}/src/main.rs"))
            .map(|f| f.content.clone())
            .unwrap_or_default(),
        files,
        platform_targets: vec!["host".to_string()],
        fill_roots: vec![format!("{crate_root}/src"), "ui/swift/Sources".to_string()],
        dependency_sections: vec![format!("{crate_root}/Cargo.toml")],
        structure: ProjectStructure::SpireApp,
        ..Default::default()
    }
}
