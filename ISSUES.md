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
        concurrent device-server processes. Not diagnosed yet, but the
        `--test-threads=2` workaround held through the M3.4 work (6/6 in 6.2s,
        including the new 3 MB upload test).
  - [x] 3f. M3.4 — done (2026-09-13). The loop runs on real hardware: the static
        musl server deployed to `trap@rpi5.local`, `device.mcp.url` set in the
        real registry, then
        `device/test { platform: "rpi5", path: "build-rpi5/rpi5/ai-traps-rpi5-tests" }`
        → `device-rpi5` → PUT 2298336 B → `run_test` → **passed**, exit 0, 3 ms,
        `stdout: "ai-traps host tests: OK (30 checks)"`. Two real bugs surfaced,
        which is exactly what running on a board is for:
        - **Every real upload was rejected with 413.** Axum caps request bodies
          at 2 MB, so `handle_upload` never ran and the intended
          `work::MAX_UPLOAD_BYTES` (128 MB) check was dead code. The unit test
          missed it by calling `store_upload_in` directly, never crossing axum.
          Fixed in `spire-target-mcp @ f10a5ce`, with a regression test that
          uploads 3 MB over real HTTP.
        - **The board was missing `libyaml-cpp0.8`**, so the cross-built test
          binary died at the loader (exit 127, `cannot open shared object
          file`) — the sysroot has the dev package, the board did not have the
          runtime one. Installed with apt. Any board meant to run these binaries
          needs the app's runtime dependencies, not just the sysroot's.
        The proof is repeatable and opt-in (CI still needs no board):
        `SPIRE_LIVE_DEVICE_BINARY=<path> cargo test --test actor_tests
        live_device_test -- --ignored --nocapture` → 1 passed in 0.56 s. M4 can
        drive that same path.
  - [x] 3g. Deploy + UI — done (2026-09-13). The board has two roles, and they now
        exist as two commands that share one front half (resolve → connect →
        upload, `stage_device_artifact`) and differ only in the last step:
        - `device/test` → `run_test`: run an artifact from the scratch work dir.
        - `device/deploy` → the new server-side `deploy` tool
          (`spire-target-mcp @ 16a81d6`): install it at an absolute destination,
          executable, replaced atomically. Destination from the request `dest`,
          else `device.deploy.dest` — which is what finally gives that registry
          field a purpose.
        The UI catches up (`593b98f`): `Platform` gained `device` (the Swift model
        had drifted; the wire already carried it), `SpireBridge` gained
        `deviceOnline` / `connectDevice` / `runDeviceTests` / `deployDeviceBinary`,
        and the action pane has a **Device group** at the top for the selected
        target — Connect, then Run tests on board / Deploy binary, gated on the
        connection with a result line under them. It appears only when the
        selection maps to a platform that declares a board.
        Also (in `593b98f`): the left pane showed targets twice — the layout tree's
        `Targets` section listed the same per-platform executables the "Platform
        builds" list above it selects and reports state for — so the tree now
        skips that section and the top list is the single target selector.
        And the project-open background connect is gone (see below): connecting is
        explicit now, which is what the "one active target" model implies.
        Known gap: the test binary's path is a convention
        (`build-<plat>/<plat>/<project>-<plat>-tests`). A project that names its
        tests differently gets a "not found" naming the full path. A registry
        field would make it explicit. M5 (start/stop/status/logs) is what makes a
        deployed binary actually run, and a rollback possible.
  - [x] 3h. Board service + the Connect fix — done (2026-09-13).
        - **`spire-code @ f299509`**: `project/open` only registered the device MCP
          servers when the graph already carried MCP config
          (`if !servers.is_empty()`), so a project with none of its own (ai-traps)
          never got a board registered — and the UI had no Connect button
          anywhere. `mcp/loadConfig` never had that guard, which is exactly why
          the actor test passed while the app did not: they exercise different
          paths. The guard is gone.
        - The board runs `spire-target-mcp` as a **systemd unit**
          (`spire-target-mcp/deploy/`, `enabled`), verified by an actual reboot:
          the board came back serving `/health` in ~15 s with nobody starting it.
          A one-off `setsid` start dies with the board and looks like a board
          fault; `systemctl is-enabled` is the thing to check, not `is-active`.
        - The device binary on the board is current: it exposes 3 tools
          (`info` / `run_test` / `deploy`). Deploying over a *running* binary
          fails with `Text file busy` (ETXTBSY), so the service must be stopped
          first — the restart then drops any client connection.
        - Verified live from the app: `device-rpi5` loads, `mcp/servers` reports
          `status=offline tools=0` before Connect, and the Connect button drives
          `device/connect` → `connected to 'spire-target-mcp' (tools: true)` →
          `status=online`. This is the first time the whole chain has worked from
          the UI rather than from a test harness.
- [x] 3i. M4a — the modify spine (`crates/spire-code/src/modify.rs`). One
      propose → apply → verify → keep-or-roll-back loop, so "modify existing
      code" is written once rather than beside each caller (autofix, free-text
      modify, the HAL contract cascade). The **round** is the unit of change: it
      measures, writes every pending target, measures again, then keeps what the
      driver accepted — so `reject_round` can judge the *project* before any
      per-file verdict is trusted, and a verification costs one compile per
      round rather than one per target. A driver owns the two domain rules
      (`accept` for one change, `reject_round` for the whole round, off by
      default); a target that failed is never retried, one that improved stays
      eligible so a partial fix converges. autofix now runs BOTH its phases on it as
      an `AutofixAdapter` — errors and warnings, the latter with a per-round gate
      rather than a per-file one, which is what `reject_round` is for — with **every
      existing test unchanged** as the evidence that behaviour did not move. Still
      open: `modify/code`; and the HAL `modify-contract` cascade.
- [x] 3j. M4b — `modify/code`. Change existing code from the user's own words: one plan
      call (which files, then a rewrite each, through the same single-file path and
      structural check the compile-fix loop already trusts), then the spine applies and
      verifies. Verification is layered and says which layer it reached — build + host
      tests are the gate, target tests run only when the board's MCP server is online,
      and a run that never reached hardware carries a caveat rather than a claim.
      Reachable as `modify/code` / the `modify_code` tool. Not yet in the build
      manager's tool list, so the UI cannot offer it (that is M4d). The target-test leg
      has not been exercised against a board. Its plan path has not itself run against a
      live model, but that is no longer an unknown: `system_flow_tests` proves the whole
      path against a real compiler, and `a_real_model_fixes_a_real_defect` drives Fix &
      Verify with the REAL `LlmConfig` (via `load_global_llm_config()`) and a live
      DeepSeek call — gated by `#[ignore]` and a runtime key check, asserting structure
      (a change that builds) and never exact text, since a real model may solve the same
      defect differently every run.
- [x] 3k. M4c — HAL `modify-contract` cascade. Built: every piece was already present,
      which makes it composition rather than invention: `hal_missing_impls` IS the drift
      measure (per platform × interface: implemented / partial / missing),
      `hal_fill::plan` + `hal_fill_apply` already do the gap fill, and
      `hal_diff_contracts` gives the contract diff the "up" direction needs. So: Obs =
      gaps + build errors, targets = the missing/partial pairs, apply = hand the plan to
      `hal_fill_apply` (backing up the files it names first), and any compile errors left
      over go through the existing `AutofixAdapter`. Acceptance is the criterion the
      drift analysis already measures: no missing, no drift, builds. Proven at system
      level up to the honest failure: `modify_contract_reports_a_real_gap_it_could_not_close`
      runs it against a real contract with no implementation and checks that the run
      reports the drift it could NOT close (`success: false`, the interface still listed)
      rather than claiming a fill it never performed. The successful generation path still
      needs a live model — `hal_generate_impl` is what it calls.
- [x] 3l. System-level flow tests — `crates/spire-code/tests/system_flow_tests.rs`. The
      flow unit tests prove each flow's decisions; `modify_code_llm_tests` proves one
      flow's LLM plumbing. Neither proves the WIRING or the real toolchain. This harness
      spawns the real actors as `ffi.rs` does — knowledge graph, build manager, tool
      registry, plus the three deps attached by MESSAGE rather than at construction
      (`SetFfiDeps`, the build-module handshake, `SetLlm`) — and replaces only the model's
      TEXT. Nine tests: the registry, the real build manager, dispatch, a real Meson
      project building, Fix & Verify fixing a real defect, `modify/code` keeping a prompted
      change, `modify/code` rolling one back, the cascade reporting drift it could not
      close, and the gated live-model run. It found three defects the fakes could not: a
      double-counted diagnostic stub, a wrong-reply bug, and `success: true` on a
      rolled-back change.
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

## 8. Embedded targets — ESP32 first, via a reusable HAL + actor framework

Unlike ai-traps (one project), the HAL and actor framework here are **reusable across
projects**: a project depends on `spire-hal` and never on a vendor SDK.

- [x] 8a. `spire-hal` core — done (2026-09-16, `spire-hal @ d4b7455`, new sibling repo).
      The seam, before any board. `no_std` and dependency-free on purpose: the first
      backend (ESP32 via `esp-idf-hal`) is `std`, but the next family (RP2040 via
      `embassy-rp`) is not, so the abstraction has to hold without std or it is an ESP32
      API with a nicer name. A `std` backend may implement `no_std` traits; the reverse is
      impossible. `actor::{Actor, Mailbox, SendError, Spawner}` is deliberately the same
      SHAPE as the host's `spire_actor::Actor` (a `Message` type and a `handle`), differing
      only in the runtime — `async`/tokio on the host, synchronous/**FreeRTOS** on device.
      `Spawner` is the executor seam and a backend's only obligation. Contracts start with
      `hal::{Led, DelayMs}` plus `HalError` (ours, not `std::io::Error`, because a trait
      signature is a promise to every family). `cargo check --lib` proves the no_std
      property and 4 tests prove the claim: a `Blink` actor that borrows the *traits* runs
      unchanged against two independently written HAL implementations, driven only through
      a `core`-only mailbox ring (so an allocator-free backend is demonstrably possible).
- [ ] 8b. `spire-hal-esp32` — the backend, wrapping `esp-idf-hal` (`std`), one cargo feature
      per chip variant, FreeRTOS actor executor. **Blocked on the toolchain**: neither
      `espup` nor `idf.py` nor any xtensa/riscv rustup target is installed on this machine,
      so it cannot be compiled or verified here — unlike the core, which is host-checkable.
      Installing espup + the ESP-IDF toolchain is the prerequisite to starting it.
- [ ] 8c. Platform + build. **The variant is a compile-time property, so: one YAML per
      chip, not one per family.** esp32c6 ≠ esp32 — different target triple, different
      `IDF_TARGET`, different cargo feature, and Xtensa vs RISC-V are different toolchains
      entirely, so a variant is a distinct cross-compilation target in exactly the way rpi5
      differs from rock3c. The model already carries the two essentials:
      `architecture.target_triple` (`riscv32imac-esp-espidf`) and `architecture.cpu` (which
      *is* the IDF target). What is genuinely missing: a `family` field for grouping (one
      backend crate serves c3/c6/s3/…), the flash command, and the fact that `toolchain` /
      `sysroot` are **required** today — so a Rust target needs dummy C values. Measured
      cost of adding the fields: 7 construction sites, including the graph codec
      (`actors/platform_codec.rs:123`), which must persist each new field as an individual
      typed property per that module's rule. Flash is host-side over **USB**
      (`espflash` / `idf.py`), deliberately not the network MCP leg (the MCP path is not
      viable for a fresh board). Build maps `{cpu, triple}` → `MCU=<cpu>` + `--target <triple>`
      — **not** `--features`: verified against esp-idf-hal 0.47 / esp-idf-sys 0.38, the
      esp-idf crates have NO per-chip features, and the chip is the `MCU` env var that
      `esp-idf-sys` reads. The same source confirms every triple used here: esp32 →
      `xtensa-esp32-espidf`, esp32s3 → `xtensa-esp32s3-espidf`, esp32c6 →
      `riscv32imac-esp-espidf`, and esp32p4 → `riscv32imafc-esp-espidf` (note the `f`).
- [x] 8d. The Rust drift measure — `build/hal_rust_contract.rs`. tree-sitter-rust was already
      a dependency and `trait_item`/`impl_item` already mapped in `rust_language_config`, so
      this was wiring rather than new machinery: `missing_trait_methods_rust(contract, impl)`
      asks the same question `extract_contract_methods_cpp` asks for C++ — which contract
      methods has this implementation not provided? Three semantics worth naming, each with a
      test, because each is a way to be wrong: a method with a **default body** is not owed
      (reporting it would be a false positive the fill path would then generate code for); an
      inherent `impl Foo` is not a contract and does not satisfy one; and
      `impl hal::Led for X` must match `trait Led` by its last path segment. A trait with no
      impl at all reports every required method — the cascade's starting state. 7 tests.
      **Not yet wired into `hal_missing_impls`**: that path is proven against C++ and gets the
      Rust branch *beside* it rather than a rewrite around it.
      This is what makes the embedded work HAL-*based* rather than merely Rust: with it, the
      same cascade (contract → drift → fill → cross-build → flash → run) closes on firmware.


