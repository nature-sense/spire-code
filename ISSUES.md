# Open Issues

Local follow-up tracker for outstanding cleanup/feature work in `spire-code`.
Each entry is a task identified during the 2026 codebase review that has not
yet been scheduled.

## 1. Implement `clean` / `lint` / `format` / `fix` for 8 build modules

Done (2026-09-01): `clean` is now implemented for all 8 modules; `lint` /
`format` / `fix` (+ streaming variants) where a canonical ecosystem tool exists:

| Module | clean | lint | format | fix |
| --- | --- | --- | --- | --- |
| cmake | `cmake --build build --target clean` | — | — | — |
| make | `make clean` | — | — | — |
| maven | `mvn clean` | — | — | — |
| gradle | `gradle clean` | — | — | — |
| go | `go clean` | `go vet ./...` | `gofmt -l .` | `go fix ./...` |
| node | `run clean` / rm dist,build,coverage,.next | `eslint .` / `run lint` | `prettier --check .` / `run format` | `eslint --fix .` / `run fix` |
| python | rm build,dist,`__pycache__`,.pytest_cache,*.egg-info | `ruff check .` (flake8 fallback) | `ruff format --check .` | `ruff check --fix .` |
| ruby | `rake clean` / rm tmp,coverage | `bundle exec rubocop` | — | `bundle exec rubocop -A` |

`supports_clean/lint/format/fix` in `ModuleCapability` now match, and
`BuildManager::check_capability` gates the unsupported ops up-front. Streaming
variants run the batch op + emit a synthetic finished event (same pattern as
the pre-existing modules). Covered by `build::python::tests` (deterministic
artifact removal) and `test_build_module_operation_capabilities` in
`actor_tests.rs`.

## 2. Wire `project/build|test|lint|install` tools in the FFI

Done (2026-09-01): the four project meta-tool actors are now spawned and
registered at FFI startup (`project.build` / `project.test` / `project.lint` /
`project.install` in the shared registry) and real channels are passed to
`build_default_registry` instead of `None`, so `tools/call` reaches them.

`ProjectBuildActor` gained a `SetProjectRoot` message; the coordinator's
`project/open` and `AnalyzeProject` handlers re-point it on every open (the
FFI opens projects dynamically, so a fixed construction-time root no longer
applies). An empty/unset root makes `project/build` fail with a clear error.
The other three actors route through ProjectQuery + BuildManager, which are
already initialized per-project. Covered by
`test_build_default_registry_registers_project_meta_tools` and
`test_project_build_root_gating_and_set` in `actor_tests.rs`.

## 3. Make the integration test harness hermetic

Done (2026-09-01): `CoreProcess::spawn()` now sets `SPIRE_DATA_DIR`,
`SPIRE_PROJECT_ROOT`, and `SPIRE_LOG_DIR` to a per-test `tempfile::tempdir()`
so the spawned `spire-core` never reads/writes shared locations
(`temp_dir()/spire-core-data`, the test cwd) — fixing the flaky
`test_system_status` failure from a stale/truncated WAL.

## 4. Consolidate the two dispatch paths (FFI-inline + Coordinator)

Done (2026-09-01): the FFI-inline RPC handlers (`project/open`,
`AnalyzeProject`, `project/getBuildTarget|buildStatus|diagnostics`,
`createProject/*`, `rag/*`, `plan/create` root injection) were moved into
`CoordinatorActor::route_request`. The FFI now attaches its app-only deps once
via `CoordinatorMessage::SetFfiDeps` (a shared `ServiceRegistry` + `FfiSharedState`
holding project root / analysis / RAG domain / watcher output), and
`ffi.rs::process_json_request` is a thin parse-and-forward wrapper with no
method branching. The standalone binary never sends `SetFfiDeps`, so the moved
methods return a clear "FFI dispatch deps not attached" error there (its
extension flow uses the tools/ methods). Regression coverage added in
`actor_tests.rs` (route-through + error-without-deps).

Also done: aligned `rust-mcp-schema`/`rust-mcp-transport` to 1.0 in both
`spire-code` and `spire-core` so the graph holds a single `rust-mcp-schema`
1.0.0 (previously 0.10.3 + 1.0.0 coexisted).

## 5. Metal embedding crashed the app at load (fixed)

Done (2026-09-02): enabling candle's `metal`/`accelerate` features made the
release `libspire_code.dylib` unloadable in the app — the FFI never came up and
the UI showed "Rust core not available". `dlopen` failed with:

```
dlopen(...) (mis-aligned LINKEDIT string pool, fileOffset=0x...)
```

Root cause: rustc's `-C strip=debuginfo` post-link strip step (a Cargo release
default) rewrites `__LINKEDIT` and lands the symbol string table on a 4-byte
(not 8-byte) file offset for cdylibs that use chained fixups; dyld enforces
8-byte alignment and rejects the dylib. Known upstream bug:
rust-lang/rust#157750 ("Stripping debuginfo on macOS produces misaligned
dylibs").

Fix: `[profile.release] strip = "none"` in the workspace `Cargo.toml` (see the
comment there). The Metal dylib then loads fine with the default linker; the
app now boots with:

```
Using Metal GPU acceleration (SPIRE_USE_METAL=1)
Embedding model loaded from Hugging Face Hub ... on Metal(MetalDevice(DeviceId(1)))
```

(The standalone `spire-core` binary was unaffected because executables don't
carry the same chained-fixup export layout.) Re-verified with a `strip=none`
build of the cdylib and a fresh `make app` + launch. A `tools/check_dylib.py`
helper dlopens the dylib to catch regressions.

## 6. SpireApp project shape (Rust + SwiftUI monorepo)

Done (2026-09-02): generalized the project-shape concept beyond HAL with a new
`ProjectStructure::SpireApp`. The wizard (Rust toolchain → "Spire app") now
scaffolds a complete `spire-<name>` monorepo: a Cargo workspace with a
`crates/spire-<name>` crate (rlib + cdylib) on `spire-actor`/`spire-core`
(sibling path deps), a minimum-launchable SwiftUI app in `ui/swift` embedding
the dylib over the JSON FFI, and `build/assemble-app.sh`/`Makefile` glue.

- `ProjectStructure::SpireApp` + `as_str`/`from_str` keys in `spire-core`
  `build_types` (serde "spire_app").
- `build/spire_app_scaffold.rs` — the monorepo scaffold (structural vs fill
  roots; workspace `Cargo.toml` bakes in `[profile.release] strip = "none"`).
- Structure/embedded now thread through the wizard → coordinator
  (`createProject/Plan|GeneratePlan|Scaffold`) → `PlanScaffold`/`GeneratePlan`/
  `ScaffoldProject` messages → `scaffold_spec_in_memory` → `ScaffoldBuildConfig`
  (previously hardcoded `structure: None`).
- The legacy `GeneratePlan` path emits a deterministic scaffold plan for
  SpireApp (no LLM needed to propose the structure).
- Fill: `generic_helpers::spire_framework_hints()` (curated spire-actor/
  spire-core API surface) is injected into the `FillProject` prompt.
- Analysis: `cargo::analyze` detects the shape (workspace + `ui/swift/`
  + spire deps) → `structure = spire_app`, `project_type = "spire_app"`, and
  `core`/`ui` `ProjectDomain`s.
- UI: New Project wizard gains the Spire app choice; project analysis shows a
  "Spire app" badge.
- Templates: `templates/spire-app/` (best practices + minimal example).
- Tests: scaffold emits the monorepo, analyze detects the shape, structure
  keys roundtrip, framework hints cover the API. E2E: materialized a
  `spire-quicknotes` scaffold, `cargo build --release` + `swift build` +
  dlopen + `assemble-app.sh` + app launch all succeeded.

