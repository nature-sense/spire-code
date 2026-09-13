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

## 7. Target-hardware testing: device MCP server + prompt→generate→verify

A device-side MCP server runs on each board, controls the trap binary and runs
tests over the network; Spire talks to it as an MCP server over Streamable HTTP.
This is the first place the "prompt → generate → verify" workflow lands (generate
a test → cross-build → deploy → run on hardware → feed failures back to the LLM
fix loop). The server itself lives in the separate `spire-target-mcp` repo; this
entry tracks the work end to end.

- [x] 1. M1 — done (2026-09-13, `spire-target-mcp @ fd00719`, separate repo).
      Rebuilt on `rust-mcp-sdk` + `rust-mcp-axum` — the same crates and version as
      the host client (`spire-core`'s `ClientStreamableTransport`), so both ends
      share one protocol implementation and one version (2025-11-25, negotiated
      down for older clients). The scaffold did not compile (`tokio_stream` /
      `async_stream` used but undeclared) and spoke the legacy HTTP+SSE split
      (`POST /mcp` + `GET /sse`, 2024-11-05); it now serves Streamable HTTP on
      `POST /mcp` (+ `/health`), with `BIND_HOST` / `BIND_PORT` and an `info`
      tool. `tests/roundtrip.rs` spawns the real binary and drives it with the
      same client Spire uses — `initialize → tools/list → tools/call("info")`
      passes — and a curl-driven check of the same sequence passes too.
- [x] 2. M2 — done (2026-09-13). `Platform` gained an optional
      `device: { mcp: { url, token }, deploy: { dest } }` block, parsed from the
      YAML seed and round-tripped through the graph as individual typed
      properties (`device_mcp_url` / `device_mcp_token` / `device_deploy_dest`).
      `Platform::device_mcp_config()` turns it into an MCP client config named
      `device-<id>`, with the token as a bearer header and `autostart: false` so
      a powered-off board can never stall `ConnectAll`. Every path that reloads
      MCP config from the graph re-adds the device servers (otherwise they would
      silently vanish from the server list), `device/status` reports them with
      the token withheld, and project open background-connects the boards the
      project actually builds for. `Connect` is now bounded by
      `CONNECT_TIMEOUT_SECS` so one dead endpoint can't park the MCP client's
      mailbox. Covered by `platform::tests` (mapping, blank token, host-only) and
      `test_device_servers_come_from_the_platform_registry` (actor level).
      Interop was also proven against the real board server (M1) over Streamable
      HTTP: a registry `device:` block connects and lists the board's `info` tool.
      That check surfaced — and fixed — a bug on the host side: Spire's HTTP MCP
      client used `standalone = true`, i.e. it opened a session-less GET SSE
      stream and treated any status for it as fatal, so *any* spec-compliant
      Streamable HTTP server (the GET endpoint is optional; 400/405 is allowed)
      was unusable with `HTTP error: 400 Bad Request`.
- [ ] 3. M3 — Test action over the device. Cross-build the test binary → upload
      it over HTTP → MCP `run_test` → report exit code + output. No `meson`/
      `test()` machinery is needed on the device — it just runs the ELF. The host
      side, the test executable and the cross toolchain (below) are what remain.
  - [x] 3a. Device side — done (2026-09-13, `spire-target-mcp @ af8980f`).
        `PUT /upload/<name>` stores a binary in the work directory (single name
        component, `.part`+rename, executable bit) and the `run_test` tool runs
        it, returning exit code, both streams and duration; failures come back
        with `isError` and the output intact. A killed run stops the clock at the
        kill and drains the pipes for at most 2s, so a test that leaves children
        behind can't hang the call (found by the new end-to-end test: a 1s timeout
        used to take 31s). 11 unit + 5 end-to-end tests; clippy/fmt clean.
  - [x] 3b. Host side — done (2026-09-13, `spire-code`). `device/test
        { platform, path, args?, timeout_secs?, name? }` resolves the platform's
        `device.mcp`, resolves `path` against the project root, ensures the server
        is connected, PUTs the binary to `/upload/<name>` (bearer token from the
        YAML when set) and calls `run_test`, returning the board's result
        verbatim. `device/status` reports the upload endpoint too. The build
        itself stays with the build tools; this picks the artifact up. Covered by
        the `device` module's unit tests plus the `device/test` validation paths
        in `test_device_servers_come_from_the_platform_registry`.
  - [x] 3c. ai-traps — done (2026-09-13). The platform-independent harness moved
        to `tests/` (shared, not host-only) and `tests/meson.build` exports
        `platform_test_sources`; `host/meson.build` and each board now build it.
        rpi5 / rock3c / a7s produce `ai-traps-<plat>-tests` with their own
        toolchains (ELF aarch64), `install: false` and no `test()` entry — a cross
        binary has nothing an `exe_wrapper` could wrap, so the board is the
        runner. `meson test` on host is unchanged (1/1 OK).
        So the full invocation becomes:
        `device/test { platform: "rpi5", path: "build-rpi5/rpi5/ai-traps-rpi5-tests",
        timeout_secs: 120 }` (a7s → `build-a7s/a7s/ai-traps-a7s-tests`,
        rock3c → `build-rock3c/rock3c/ai-traps-rock3c-tests`).
  - [x] 3d. Toolchain — done (2026-09-13, `spire-target-mcp @ d0b9a3c`). The board
        binary is a **static musl** aarch64 ELF (9.0 MB): `make device-toolchain`
        (rustup target + the cross toolchain into `~/toolchains`, checksum-verified)
        and `make device-binary` reproduce it. Two environment gotchas are
        documented in the README: `brew install` refuses when the Command Line
        Tools are outdated, so the toolchain comes straight from the upstream
        release; and the build must run through the **rustup** toolchain, because
        the ambient `cargo` is Homebrew's Rust, which ships only the host std
        (`can't find crate for core`).
  - [ ] 3e. Follow-up — `tests/device_tools.rs` can hang when run with the
        default (high) test parallelism: 5/5 passes with `--test-threads=2`
        (5.7s) and again in the same form, but a full run showed 4/5 with
        `passes_arguments_and_kills_a_hung_binary` never finishing. Suspect the
        orphaned grandchild holding the capture pipes together with several
        concurrent device-server processes; needs a proper diagnosis (and
        possibly `--test-threads` for that file) before the M3.4 hardware run.
- [ ] 4. M4 — run→fix loop. Feed a failing `run_test` back into the LLM fix
      loop (rebuild → redeploy → re-run), bounded and revert-safe — the
      first-class prompt→generate→verify slice.
- [ ] 5. M5 — trap control + all boards. Tools `run` / `start` / `stop` /
      `status` / `logs`; generalize across rpi5 / rock3c / a7s.
- [ ] 6. Backlog — first-class prompt→generate→verify everywhere. Wire the
      generate tools (`createProject/*`, `hal_*`) into the verify spine so new
      code is compile-verified as it is generated; covers brand-new HAL
      contracts, new toolkits, and from-scratch projects (not yet exercised —
      all work so far has been on the pre-existing ai-traps project).

