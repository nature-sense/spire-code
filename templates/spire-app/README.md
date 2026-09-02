# Spire App — project shape & best practices

A **spire-app** is a Rust/SwiftUI monorepo built on the Spire framework
(`spire-actor` + `spire-core`). Scaffold it from the New Project wizard
(Rust toolchain → **Spire app**) — the shape selector emits this layout:

```
spire-<name>/
├── Cargo.toml                    # [workspace] + [workspace.dependencies]
│                                 #   spire-actor/spire-core (sibling path deps)
│                                 #   [profile.release] strip = "none"  ← REQUIRED
├── crates/spire-<name>/          # the Rust core (rlib + cdylib)
│   ├── Cargo.toml
│   ├── src/lib.rs                # spire_send_json FFI + application logic
│   └── src/main.rs               # CLI entry point (development)
├── ui/swift/                     # host SwiftUI app (macOS 14+)
│   ├── Package.swift
│   └── Sources/SpireUI/
│       ├── App.swift
│       ├── ContentView.swift
│       └── Bridge/CoreBridge.swift   # dlopens libspire_<name>.dylib
├── build/assemble-app.sh         # cargo build + swift build → <Name>.app
└── Makefile                      # make rust / swift / app / run / clean
```

## Non-negotiables

- **`[profile.release] strip = "none"`** — rustc's default `strip=debuginfo`
  post-link step misaligns the LINKEDIT string pool of the FFI dylib and dyld
  rejects it at `dlopen` (rust-lang/rust#157750). Never remove this.
- **Path deps** `../spire-actor` / `../spire-core` — a spire-app always lives
  inside the spire tree next to those repos.
- **FFI symbols** — the crate must export `spire_send_json` (JSON in → JSON
  out) and `spire_free_string` as `#[no_mangle] extern "C"`; the Swift bridge
  is written against exactly those two symbols.
- **Structural files are locked** — workspace `Cargo.toml`, `Package.swift`,
  `CoreBridge.swift`, `assemble-app.sh`, `Makefile`, `.gitignore`. The fill
  LLM may only write under `crates/<name>/src` and `ui/swift/Sources`.

## How to fill (LLM rules)

- App logic lives in the Rust crate (actors on `spire_actor::Actor`, using
  `spire_core` for embedding/RAG/LLM/config) and SwiftUI views under
  `ui/swift/Sources`. Both are fill roots.
- Declare each dependency exactly once via `declare_dependencies` against
  `crates/<name>/Cargo.toml` (the crate manifest), never the workspace root.
- Keep the FFI surface stable: route JSON methods inside `spire_send_json`
  to your actor messages; never change the two exported symbols.
- Run `make app` after filling to verify the Rust core compiles, the Swift
  app builds, and the assembled `.app` launches with the core loaded.

## Examples

See `templates/spire-app/example/` for a minimal reference crate + SwiftUI
shell (the exact shape the scaffold emits, before filling).
