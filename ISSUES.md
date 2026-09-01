# Open Issues

Local follow-up tracker for outstanding cleanup/feature work in `spire-code`.
Each entry is a task identified during the 2026 codebase review that has not
yet been scheduled.

## 1. Remove never-instantiated subsystem group structs

The `PlanningSubsystem`, `BuildSubsystem`, and `ProjectSubsystem` structs (and
their `*Handles`/`*Deps` types + `impl Subsystem` blocks) are dead code: the
composition roots (`src/main.rs`, `src/ffi.rs`) spawn actors manually rather
than through these groups.

- `crates/spire-code/src/subsystems/planning/mod.rs`
- `crates/spire-code/src/subsystems/build/mod.rs`
- `crates/spire-code/src/subsystems/project/mod.rs`

Note: the `pub use` re-exports in these files are still used and must be kept.

## 2. Implement `clean` / `lint` / `format` / `fix` for 8 build modules

These modules return `"… not implemented for this module"` for
`Clean` / `Lint` / `Format` / `Fix` (+ streaming variants) — 48 stubs total.
After remediation they declare `supports_clean/lint/format/fix: false` in
`ModuleCapability` and are gated by `BuildManager::check_capability`, so the
failure is now an up-front clear error. Implement the operations (or explicitly
document them as out of scope).

- `crates/spire-code/src/build/node.rs`
- `crates/spire-code/src/build/maven.rs`
- `crates/spire-code/src/build/make.rs`
- `crates/spire-code/src/build/cmake.rs`
- `crates/spire-code/src/build/go.rs`
- `crates/spire-code/src/build/gradle.rs`
- `crates/spire-code/src/build/python.rs`
- `crates/spire-code/src/build/ruby.rs`

## 3. Wire `project/build|test|lint|install` tools in the FFI

These four project meta-tools are intentionally unregistered in the FFI tool
registry because `ProjectBuildActor` takes a fixed `project_root` at
construction while the FFI opens projects dynamically. To wire them in, add a
per-project (re)spawn or a root setter, then pass real channels to
`build_default_registry` instead of `None`.

- `crates/spire-code/src/ffi.rs`
- `crates/spire-code/src/actors/tool_providers/builder.rs`

## 4. Remove unused `send_raw` test helper

Pre-existing dead-code warning: the `send_raw` method in the integration tests
is never called.

- `crates/spire-code/tests/integration_tests.rs`
