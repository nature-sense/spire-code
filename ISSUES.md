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
- [x] 3. M3 — Test action over the device (done 2026-09-17). Cross-build the test binary → upload
      it over HTTP → MCP `run_test` → report exit code + output. No `meson`/
      `test()` machinery is needed on the device — it just runs the ELF. 3a/3b/3c were already done;
      the **cross-build leg** is now closed too: `device/test` (and `device/deploy`, which shares the
      same front half) takes `build: true` and produces the artifact itself, through the *build
      tools*, so it is the one this platform's module writes for this triple — with its SDK
      environment and flags — rather than whatever a hand-run command left in the tree. It analyses
      on demand first (a build routes on a stored analysis, and requiring the caller to have done it
      was the same hidden ordering requirement the fill leg dropped), and a failed build returns the
      build's own output tail, because a cross-build fails for a reason the user can act on ("no
      target installed") that paraphrasing would hide. `device/test` also names that option in its
      missing-artifact error — "build it for <platform> first, or pass `\"build\": true`" — and the
      UI's "Run tests on board" passes it, so the action builds, deploys and runs in one gesture.
      Covered by the existing `device/test` validation test, which now pins the ordering (a build
      request with no project open stops before any network) and the hint.
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
      first-class prompt→generate→verify slice. Depends on M3's cross-build leg
      (the loop has to rebuild) and reuses the **bounded compile→fix spine** that
      the embedded-HAL fill leg now runs on (see 9f), so the two are one
      mechanism rather than two.
- [ ] 5. M5 — trap control + all boards. Tools `run` / `start` / `stop` /
      `status` / `logs`; generalize across rpi5 / rock3c / a7s. The **board side
      is the blocker**: `spire-target-mcp` currently exposes only `run_test` and
      `deploy`, so those tools have to exist there (a separate repo) before the
      host can pass them through.
- [ ] 6. Backlog — first-class prompt→generate→verify everywhere. Wire the
      generate tools (`createProject/*`, `hal_*`) into the verify spine so new
      code is compile-verified as it is generated; covers brand-new HAL
      contracts, new toolkits, and from-scratch projects (not yet exercised —
      all work so far has been on the pre-existing ai-traps project). The spine
      itself now exists (`build/verify_spine.rs`): gate → build → hand the
      compiler's errors back to the generator → rebuild, bounded. What remains is
      wiring the generators that still write without one.

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
- [x] 8b. `spire-hal-esp32` — the backend, wrapping `esp-idf-hal` 0.47 (`std`), FreeRTOS actor
      executor via `spire-hal-std`. **Compiles for two RISC-V variants**: esp32c6
      (`riscv32imac-esp-espidf`, 1.6 s incremental) and esp32p4 (`riscv32imafc-esp-espidf`,
      2 m 06 s the first time, because ESP-IDF is built per MCU) — both with no errors and no
      warnings, on top of a fully built ESP-IDF. The two files written unverified were both
      wrong in exactly the ways predicted, and are fixed: `PinDriver<'d, MODE>` carries only
      the MODE in 0.47, so `GpioLed` has **no pin type parameter** and the pin appears only on
      `new` (the abstraction finally doing what it claims); and `FreeRtos::delay_ms` is an
      inherent method, not a trait one, so the `Delay` import was unused.
      **Correction to the line above: there is no per-chip cargo feature.** The chip is the
      **`MCU` environment variable** that esp-idf-sys reads, which is exactly why a variant is
      a platform entry (8c) rather than a feature flag. The working command is
      `MCU=<chip> cargo build -Zbuild-std=std,panic_abort --target <triple>`, with espup's
      `esp` toolchain supplying **both** cargo and rustc on PATH (letting cargo find its own
      rustc silently uses stable, and `-Z` then fails with an error about a *flag*), plus
      `source ~/export-esp.sh` for LIBCLANG_PATH. Rationale and the three failed attempts are
      in that repo's README, since each failure names something other than its real cause.
- [x] 8c. Platform + build. **The variant is a compile-time property, so: one YAML per
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
      **Done (2026-09-16): both halves.** Platform: `platform.rs` + codec + seeds for esp32,
      esp32s3, esp32c6, esp32p4 — all validated, and the p4 entry compiles through to the
      backend. Build: `build/esp.rs` (11 tests) emits the invocation above — espup's `esp`
      toolchain prepended to `PATH` (cargo finding its own `rustc` silently picks stable, and
      `-Zbuild-std` then fails complaining about a *flag*), `LIBCLANG_PATH` globbed out of the
      versioned esp-clang dir, `MCU` from the platform — and registers **by platform**
      (`AddPlatformModule { os: "esp-idf" }`) so it cannot shadow cargo for ordinary Rust
      projects. The flash leg is reachable end to end: `build_flash { path, platform, … }` →
      `BuildManager::flash_project` → `Flash` → `run_esp_flash` → host
      `espflash flash --chip <chip> <artifact>`. The artifact is **derived, not guessed**
      (`target/<triple>/<profile>/<package>`, the package from `opts.package` else
      `[package] name`), and a refusal for every gap — missing or unknown platform, not
      esp-idf, no flash tool, unnamed binary, artifact not built — fires *before* any device is
      touched. `supports_flash` on
      `ModuleCapability` applies the same up-front-refusal rule as clean/lint/format/fix, so a
      `build_flash` for a board with no flash step is refused by name instead of being routed to
      a module whose silence would surface as a lost channel.
      Note: **ESP-IDF is built per MCU**, so the first build for a new chip costs minutes while
      later ones are seconds.
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
      **Wired into `hal_missing_impls` as a sibling, not a rewrite**: the C++
      `hal_platform_coverage_map` is untouched and `build_manager.rs` merges
      `rust_platform_coverage_map` into the same map, because a project mid-migration
      legitimately has both. Same conventions deliberately — contracts in `hal/api` plus the
      toolkit mirror, the file **stem** as the interface key (which is what lets `led.rs` and
      `camera.hpp` land in one map), canonical and legacy platform dirs, and the
      `has_impl`/`implemented` split the fill flow branches on. `missing_sigs` is empty on
      purpose: no Rust fill path exists yet, and blank signatures would look like a contract
      that had been read and found complete.
      This is what makes the embedded work HAL-*based* rather than merely Rust: with it, the
      same cascade (contract → drift → fill → cross-build → flash → run) closes on firmware.

### 8, on hardware (2026-09-16): the cascade ran against a real ESP32

Board: **ESP32 rev v3.0** (M5Stack Core2), 16 MB flash, on `/dev/cu.usbserial-569C0028661`;
firmware: `spire-hal/examples/blink-esp32`; platform: the `esp32` registry entry
(`xtensa-esp32-espidf`). `build_flash` was driven through the app's own tool path
(`spire_rpc.py`-style → FFI → `tools/call`) and flashed the board: `espflash flash --chip
esp32 --non-interactive --port /dev/cu.usbserial-569C0028661 <elf>` → exit 0, then the serial
console showed the actor loop (`blink: on` / `blink: off`). Two real bugs fell out of doing
it, both fixed:

- **`build_build` ignored the platform when routing.** `build_project_with_events` — the path
  a UI Build click and a `build_build` tool call share — called `module_tx_for(config, None)`,
  so an ESP32 build went to the **cargo** module while the batch `build_project` sent the same
  request to the esp module. Two paths, two answers. Pinned by
  `build_build_routes_by_platform_not_to_the_config_owners_module`: two fake modules stamp a
  distinct `command`, and the stored analysis is faked at the `GetConfig` boundary — so it
  needs no memory graph, toolchain or board, and it was verified to *fail* (`cargo-build`) with
  the bug reintroduced.
- **espflash cannot see a plain USB-serial adapter.** `espflash list-ports` reports *no known
  serial ports* for this board while `--port /dev/cu.usbserial-…` connects fine, and its
  fallback is an interactive prompt — which a `tools/call` cannot answer, so the un-flagged
  command dies with `IO error: not a terminal`. The flash command now always passes
  `--non-interactive` and names the port, discovered as the single USB-serial device
  (`serial_port_in`, refusing when there are none or several), with `port=` / `$ESPFLASH_PORT`
  as escape hatches.

**Resolved (2026-09-16): the example was missing its `build.rs`.** The link died in `ldproxy`
with *Cannot locate argument '--ldproxy-linker <linker>'* because **no crate re-emitted the
link args for the binary**. `esp-idf-sys` does the real ESP-IDF build and *propagates* what it
learned (`--ldproxy-linker`, the IDF linker script, the lib dirs, kconfig as `#[cfg]`) as
`DEP_ESP_IDF_*` **metadata**; `esp-idf-hal`'s build script relays that onward as
`DEP_ESP_IDF_HAL_*` — but a `rustc-link-arg` from a build script applies to the *emitting*
package's own targets, which is why esp-idf-hal's `output()` is documented as "only necessary
for building the examples". The documented one-liner for a binary crate that depends on
esp-idf-sys/-hal/-svc is a `build.rs` containing `embuild::espidf::sysenv::output()`, and the
blink example did not have one. Added, with `[build-dependencies] embuild = "0.33"` (the
version esp-idf-sys already pulls in).

Measured after the fix, cold and through the app: `build_build` → `cargo build --target
xtensa-esp32-espidf -Zbuild-std=std,panic_abort --release`, exit 0, then `build_flash` → exit 0
→ the serial console showing the actor loop. No workaround, no injected RUSTFLAGS.

Two notes for the next person: an earlier version of this entry blamed a cargo propagation
quirk — that was wrong, the mechanism above is the whole story, and the "minimal repro" it
cited did not measure what it claimed. And the args being absent is *invisible* until the
link, so a project like this fails with an error that names neither the crate nor the missing
file; anyone scaffolding an esp project should emit this `build.rs` (spire-code has no esp
project scaffold today — the example is hand-written).

**Disk: one ESP-IDF install per machine, not per project** (2026-09-16). `esp-idf-sys` defaults
to `<workspace>/.embuild/espressif`, which had installed the framework, its build tools, a
Python env and an archive cache **twice** on this machine: ~5.3 GB for the `spire-hal` workspace
and ~6.6 GB again for the detached `blink-esp32` example — because the example is its own
workspace. `EspBuildModule` now sets `ESP_IDF_TOOLS_INSTALL_DIR` on every esp build — to
esp-idf-sys's keyword `global` (its standard `~/.espressif`) unless the environment already names
something, in which case that value is forwarded verbatim. It is a **keyword, not a path**:
esp-idf-sys splits the value on `:` and matches `global` / `workspace` / `out` / `fromenv` /
`custom:<dir>`, so passing an absolute path fails with `Matching variant not found` — an error
naming neither the variable nor the reason, and exactly what the first version of this code did.
`custom:<dir>` is how a machine points it at a shared cache or CI layer; `workspace` takes back
the per-project behaviour. Switching is a one-time cost, and it
can be made offline: seeding `<dir>/dist` with the tool archives (`*.tar.xz` from a previous
`.embuild/espressif/dist`) turns the install into an extract rather than a download, and
`idf_path`/`$IDF_PATH` covers the ESP-IDF clone itself. The espup toolchain (~1.6 GB: rustc for
`-Zbuild-std` plus its own Xtensa C compiler and clang) is already per-user and unaffected.

Measured after the switch: both project-local trees deleted (**11.9 GB reclaimed**), the shared
`~/.espressif` 7.0 GB, and a **cold** rebuild driven through the app — `cargo clean` first, so std,
every dependency and the esp32 ESP-IDF build all ran from scratch — succeeded in **84 s**, leaving
`<workspace>/.embuild` empty: nothing re-downloaded, nothing re-installed per project. That same
build was then flashed onto the board through `build_flash` and the console showed the actor loop
(`blink: on` / `blink: off`), so the shared install costs nothing at runtime either.

Two gotchas worth knowing. (1) The IDF cmake cache under
`target/<triple>/<profile>/build/esp-idf-sys-*/out` records the IDF **source path**, so the first
build after changing the directory fails with *"The source … does not match the source … used to
generate cache"* until that build-script output is removed (`rm -rf
target/<triple>/<profile>/build/esp-idf-sys-*`). (2) The setting is a keyword, not a path — see
above; an absolute path is the obvious wrong answer and its error names neither variable nor value.

## 9. The `embedded-hal` project type — one contract, N board families

What this is: a **new create-project type** in the wizard that interactively builds a
firmware HAL the way the existing *cross-platform with HAL* type builds a C++ one — contract
first, then a scaffold and a fill per platform — but for Rust, with the contract a set of
`trait`s and each platform a **backend crate**. The target repo shape is the `spire-hal`
sibling (`spire-hal` contract + `spire-hal-std` executor + `spire-hal-esp32` / `spire-hal-rp2040`
backends), and the multi-platform claim is true from day one: **esp32 family + rp2040**.

The analogy, so the shape is obvious: contract ⇄ `hal/api/*.hpp`; impl ⇄ a backend crate;
`meson.build` wiring ⇄ `Cargo.toml` + `.cargo/config.toml` + `build.rs`; and the drift measure
already exists on both sides (`extract_contract_methods_cpp`, `missing_trait_methods_rust`,
merged in `hal_missing_impls`). Reused as-is: the `createProject/*` wizard pipeline, the
`ScaffoldSpec`/`ScaffoldFile` structural-vs-fillable contract, and `EspBuildModule` for the esp
build/flash leg.

- [x] 9a. **Platform model carries the HAL hints** (done 2026-09-17). `Platform` gained
      `library_hints` — the free-text "use this SDK / these peripherals / no radio on this
      variant" block the YAML already had and the C++ impl prompt already consumed — so the
      wizard can *show* it while a platform is chosen and the Rust fill prompt can inject it.
      It is persisted as its own typed property (`platform_codec`) and re-seeded on every
      startup, so existing graphs pick it up without a migration. `Platform::is_embedded()`
      (`os` ∈ esp-idf, rp2040) is the filter the picker needs; `os` rather than a new field
      because `os` is what the *build* already keys on. `hal_platform_library_hints` reads the
      typed field first and the raw YAML second, so a hand-written partial YAML still yields its
      hint. Verified through the app: `platforms/list` returns all seven seeds with their hints,
      and the four esp variants report `embedded=true` while the Linux cross-targets do not.
- [x] 9b. **Type + recognition + scaffold** (done 2026-09-17). `ProjectStructure::EmbeddedHal`
      (`"embedded_hal"`) is recognized by a **declared** marker rather than a layout guess:
      `[workspace.metadata.spire] structure = "embedded_hal"` in the workspace manifest, read by
      `cargo.rs::declares_embedded_hal` — hand-parsed like the rest of that module, since a TOML
      dependency for one key is the tail wagging the dog. The emitter
      (`build/embedded_hal_scaffold.rs`) writes the workspace, the contract crate
      (`actor::{Actor, Mailbox, SendError, Spawner}`, `HalError`, `Led`, `DelayMs`), the std
      executor (a real `QueueMailbox`/`StdSpawner` over `std::sync::mpsc`), and **one backend
      crate per family** with its manifest plus a fillable `unimplemented!()` stub. It refuses by
      name on an unknown platform, a platform that is not embedded, and a family no backend is
      known for (today: esp32, rp2040) — a backend crate that cannot build is worse than none.
      Two invariants are encoded *and tested*: **variants collapse to families** (esp32c6 +
      esp32s3 → one `-esp32` crate, because the chip is `MCU` + `--target`, not a feature), and a
      `std` family reuses the shared executor while a `no_std` one is told to supply its own
      `Spawner`. Verified: the emitted scaffold's contract + executor build with **zero warnings**
      and no cross toolchain (`default-members` excludes the backends), and a test runs the
      emitter and then `analyze` to prove the marker written in one module is the marker read by
      another. Backend *builds* wait on 9c/9d (the fill, and the rp2040 platform).
- [x] 9c. **Rust contract authoring + fill, hint-injected.** The Rust analogues of the `hal_*`
      tools (`_validate/_write_contract`, `_add_platform`, `_fill_plan/_fill_apply`,
      `_missing_impls`) whose prompts read the platform's `library_hints` (+ a Rust hardware
      profile) so a generated `impl` uses that board's real peripherals. Done in four parts — the
      measure, the plan, the apply leg and the authoring trio:
      - **The measure reads the new layout, and a stub is not coverage** (done 2026-09-17).
        `rust_platform_coverage_map` now knows the embedded-HAL tree as well as the C++ one:
        contracts from `crates/<prefix>-hal/src/hal/*.rs` (the same stem-keyed interface
        convention, so both layouts merge into one map) and backends from
        `crates/<prefix>-hal-<family>/src`, keyed by **family**. `-std` is excluded by convention
        — it is the shared executor crate, not a board family. Without this the measure found
        *nothing* in a scaffolded project, which is the one project it most needed to see.
        Placeholders are recognized **per `impl`** by `unimplemented!()` in the body
        (`placeholder_impls_rust`): the scaffold declares every required method, so syntax alone
        reported a fresh backend as `implemented` — the fill queue was empty exactly when it
        should be full. `implemented` is now `missing.is_empty() && !is_stub`, so
        `hal_missing_impls` reports `stub` and the Swift maturity label (which reads `is_stub`)
        shows it without a UI change. Four tests pin the scaffold's own stub, the same file once
        filled, `-std` not becoming a platform, and `hal/mod.rs` not becoming an interface.
        Running the plan against the **real `spire-hal` workspace** then caught what tests shaped
        like the scaffold could not: that workspace's `time.rs` declares `DelayMs`, so matching the
        stem alone reported a written implementation as `none` — the one failure that makes a fill
        rewrite code that is already there. The match is now **trait-name first, stem second** (a
        contract keeps its traits beside its stem key, in both layouts), and the plan names the file
        that **holds** each impl rather than assuming `lib.rs`, because the reference workspace
        splits its backends into `led.rs`/`time.rs`. Re-run against that workspace: one injected
        `unimplemented!()` yields exactly one item, on that file, with the platform's real hints.
      - **`embedded_hal_fill_plan`** (done 2026-09-17). One item per backend **file** that still
        owes something: family, crate, file, the pending traits with status
        (`none`/`stub`/`partial`)
        and the constrained prompt — plus a `refused[]` entry for a family with no vendor facts,
        the same refusal the scaffold makes rather than a prompt that would invent an API. The
        prompt injects the platform's `library_hints`, a hardware profile (board/chip/target/os/
        runtime/vendor crate), each pending contract's source, the file's current source, and the
        rules that keep the answer inside this file and this vendor's API. Read-only: the plan is
        the reviewed artefact, and the same measure the UI reads feeds it, so the two cannot
        disagree. The authoring tools that pair with it are below; the UI reaches Add board and is
        yet to reach contract authoring.
      - **`embedded_hal_fill_apply`** (done 2026-09-17). One model call per item (role `Coding`)
        with the same two retries the C++ path uses: once on truncation with a "be concise"
        instruction, twice on a parse error with the errors appended. Between the answer and the
        file sits the **gate** — every pending trait implemented by name, every pending method
        present, and no `unimplemented!()` left in its `impl` — and a generation failing any of
        those is reported with the reason while **nothing is written**. Unlike the C++ path it never
        writes source that does not parse: there the verdict is advisory for a human, here the file
        lands in a crate the host build compiles. The item's path is checked (an existing file under
        `crates/`) rather than trusted, and each write is followed by a re-measure, so
        `interfaces_still_pending` is the honest verdict rather than a claim.
        The answer's fences come off through a Rust-aware `code_block`: the shared
        `strip_code_fences` handles only a fence at the very start of the answer and only a `cpp`
        tag, so a preamble or a ```rust tag would have failed the parse gate on *every* answer —
        which is how it was caught, by the two end-to-end tests. Both ends of the loop are pinned
        with a **fake LLM** (a channel that answers with a canned string): one drives plan →
        generate → gate → write and asserts `hal_missing_impls` then reports `implemented` with
        `is_stub: false`; the other answers with the placeholder kept and asserts the refusal plus a
        byte-identical file.
      - **`_validate`/`_write_contract`/`_add_platform`** (done 2026-09-17). The Rust trio that
        authors the two halves of a project that are not a backend — its contracts and its boards
        (`build/embedded_hal_contract.rs`). The C++ pair's safety rule carries over: validate first,
        and an invalid contract never touches disk. The rules are not style — each is a way the file
        would be invisible to the drift measure: it must parse (same tree-sitter parser the measure
        uses), declare at least one trait with at least one **required** method (a trait whose
        methods are all defaulted is skipped by the measure, so accepting it hands back a contract
        that behaves as absent), declare no `impl` (an implementation in the interface file would be
        reported as *every* board's), and not declare one name twice (ambiguous at the
        `use hal::{…}` every backend writes).
        Where the C++ tools edit `meson.build`, these edit the two files the Rust layout depends on
        for visibility, and that is the part worth having: `write_contract` adds
        `pub mod <stem>;` **and** `pub use <stem>::<Trait>;` to `hal/mod.rs` (beside the existing
        groups rather than appended), and `add_platform` adds the **workspace member** line. Without
        those, a written contract and a new backend crate are both invisible — the project would look
        *finished* rather than broken. `add_platform` also refuses what the scaffold refuses (a
        family with no vendor facts, a non-embedded `os`, an unknown id) and one more: a family that
        already has a backend crate, so a second call cannot fork it. The backend crate itself comes
        from the **scaffold's own emitter** — one code path, so a platform added later gets exactly
        the pinned deps (`embedded-hal = "1"`, `cortex-m = "0.7"`) and the `unimplemented!()` stubs
        that took a compiler to discover.
        Two refusals on the write are deliberate: an existing file with **different** content is not
        replaced (a contract is what every backend implements — editing it is a deliberate act), and
        an **identical** file is a no-op with the module list still checked, so a retry after an
        interrupted authoring converges instead of erroring.
        Verified end to end, no model: `a_new_contract_and_a_new_board_both_become_fill_work` drives
        the real route — scaffold (rp2040) → plan (1 item) → validate + write `sensor.rs` →
        add `esp32c6` → plan again (**2 items**) — and asserts the new backend owes the interface
        just authored (`stub`), not merely that files exist; the unit tests assert the same through
        `embedded_hal_layout`, the function the measure uses. Unit tests there also pin every refusal
        above, the idempotent re-write, and that a project without a contract crate is refused by
        name rather than half-written.
        Two test-infrastructure findings came out of it. The shared `PLATFORM_DIR_TEST_LOCK` is now
        taken **tolerantly** (`unwrap_or_else(|poisoned| poisoned.into_inner())`): a plain `unwrap`
        let one failing test poison it, and every later taker then panicked with "PoisonError" — one
        real failure turned into ten unrelated ones, which is exactly how it presented. And the
        fixture discipline (a registry *and* the lock *and* an env guard) is what makes these tests
        pass together, not just alone.
        In the UI: **"Add board"** (the "Rust backends" section) calls `embedded_hal_add_platform`
        and re-reads the coverage, so the rows gain the family and the menu drops it in the same
        gesture. The menu offers only families the project does not have — the tool refuses a
        duplicate, and a menu that offered what it will refuse would be a lie — and that rule is a
        static, tested function (`addableBoards`) rather than a second copy of the refusal. Still
        open: `_validate`/`_write_contract` are tool-only, so authoring a contract is a tool call
        rather than a window; the validate-then-wire behaviour they pair up is what such a window
        would show. The C++ `hal_*` pair is untouched beside them.
- [x] 9d. **rp2040 platform + build/flash** (done 2026-09-17). A registry entry
      (`~/.spire/platforms/rp2040.yaml`: `os: rp2040`, `family: rp2040`,
      `target: thumbv6m-none-eabi`, `rust.idf_target: RP2040` for `probe-rs --chip`,
      `rust.flash: picotool`, its own `library_hints`) and `build/rp2040.rs` — a module registered
      with `AddPlatformModule { os: "rp2040" }` beside the esp one, in both the app path (`ffi.rs`)
      and the standalone binary, claiming no config file so a plain Rust project still reaches
      cargo. The plan is `cargo build --target <triple>` and **nothing else**: no `-Zbuild-std` (the
      target has a prebuilt `core`), no vendor SDK, no environment at all — which is why the module
      is a fraction of the esp one's size. Flash supports the three tools boards are actually
      flashed with, each from `rust.flash`: `picotool` (the BOOTSEL USB interface, no probe and no
      mount — the default), `probe-rs` (a debug probe, with `--chip` from the platform because it
      cannot infer one from an ELF), and `elf2uf2-rs` (a UF2 to a mounted volume, `-d` to deploy or
      an explicit `INPUT OUTPUT`); a fourth name is refused rather than run, so a YAML typo fails as
      "this module does not know that tool" instead of as a shell error.
      The extraction this needed went into `generic_helpers` (`cargo_package_name`,
      `cargo_artifact_path`, `build_spec_from_command`) with esp's functions kept as thin delegates,
      so the proven esp module's tests did not move.
      Verified live: the real registry entry parses through the real code (`is_embedded: true`, the
      plan, the chip spelling, the hints), and the planned invocation
      (`cargo build --target thumbv6m-none-eabi`, no flags) **builds a `no_std` crate on stable**
      once the toolchain is right. That check found a real environment trap: this machine's `cargo`
      *and* `rustc` on `PATH` are Homebrew's (`/opt/homebrew/Cellar/rust/1.98.0`, whose sysroot has
      only the host target) while rustup's stable toolchain has `thumbv6m` installed — so cargo fails
      with "can't find crate for `core` … may not be installed" and suggests installing a target that
      is already installed. The module therefore checks the **sysroot `rustc --print sysroot`
      reports** rather than rustup's target list, and the refusal distinguishes the two cases: not
      installed anywhere (`rustup target add …`), or installed for rustup's toolchain but not for the
      one about to run (name the sysroot, and say to put `rustup which cargo`'s directory first on
      `PATH` — the fix, verified on this machine). A toolchain that cannot be asked is not a refusal:
      cargo's own error is better than a guess.
- [x] 9e. **UI** (done 2026-09-17). "Embedded HAL (Rust)" is the third structure in the wizard's
      Cargo row, and the platform picker narrows to **boards** when it is selected: a Linux
      cross-target has no backend crate to fill, and the scaffold refuses one by name. Each board's
      `library_hints` are shown under its checkbox — the same text the fill prompt injects, so what
      the user reads while choosing is what the implementation will be constrained by — and Plan
      stays disabled until a board is chosen, because the project *is* its backends. The rule for
      "is this a board" is **not** re-implemented in Swift: `platforms/list` sends `embedded`
      (`Platform::is_embedded()`) beside the platform, and both ends are pinned by tests (the
      payload in Rust, the decode in Swift). The Swift `Platform` gained the fields the picker needs
      (`family`, `rust`, `library_hints`, `embedded`), which it had been silently dropping.
      Two things came with it, both needed for the wizard's flow to be *true* rather than merely to
      compile:
      - **The Plan route is deterministic for this structure**, as SpireApp's is: a shared
        `scaffold_plan_from_spec` turns the emitter's files into write steps and adds a parse + host
        build gate, so "OK — scaffold and run" never hands the contract to an LLM plan. (The
        `embedded` flag on the creation messages was already carried and ignored; the dispatch is on
        the structure, which is what actually differs.)
      - The verification window grew a **Rust backends** section: one row per family with each
        interface's maturity and its missing methods, read from the same `hal_missing_impls`
        coverage the maturity chips and the build gate use — so a backend shown `stub` there is
        exactly what keeps that platform's build disabled, and filling it flips both. "Plan fill"
        runs `embedded_hal_fill_plan` (read-only, one item per backend file) and shows the pending
        traits *before* anything happens; "Fill" runs `embedded_hal_fill_apply`, which gates the
        model's answer (every pending trait implemented by name, no `unimplemented!()` left) and
        re-measures, after which the UI re-reads coverage. The C++ pair-file flow is untouched
        beside it.
      Verified: `swift build` and `swift test` pass with the new decode test, and 306 lib tests pass.
      **Driven end to end at last** — `tests/embedded_hal_creation_tests.rs` walks the wizard's own
      route (`createProject/Plan` → `createProject/Scaffold` → `hal_missing_impls` →
      `embedded_hal_fill_plan`) through the real coordinator, the real build manager with its real
      cargo module, the real filesystem module and a real platform registry, with **no model
      wired at all**. It asserts the deterministic plan (is_template, contract + both backends
      written, build gate last), the workspace on disk (the declared marker, six files), the fresh
      backends measuring as `stub`/`is_stub`, and the fill plan naming one file per family with the
      board's own hints and the contract source in the prompt.
      Driving it found two things a compile could not:
      - **The wizard's Plan button does not use `createProject/GeneratePlan`** — it calls
        `createProject/Plan` (`PlanScaffold`), which went straight to the **LLM fill plan** for every
        structure, so 9e's "the plan is deterministic" was true of a route the UI never calls. Worse
        for this structure, that fill plan's roots include the *contract crate*: a model writing
        there edits the one thing every backend depends on. `PlanScaffold` now dispatches on the
        structure and returns the deterministic scaffold plan for the embedded HAL — so the plan
        needs no model, and the contract is never handed to one. (SpireApp keeps its LLM fill plan
        deliberately: there the fill *is* the goal.)
      - `hal_missing_impls` reported `kind: "partial"` for a backend that is a fresh stub, because
        it tested `implemented` before `is_stub`. `kind` now says `stub` when a placeholder is
        present — the same word the maturity chips use, so the payload and the UI agree.
      **And the fill leg now runs against a real model** (`#[ignore]`d live test in the same file,
      gated on `load_global_llm_config()` like the crate's other live test). Both backends filled,
      the gate accepted both answers, and the measure reported `led` *and* `time` implemented for
      `esp32` and `rp2040` — the end of the chain, on a real model, unattended. Getting there was
      worth the three earlier live runs, which failed for reasons worth keeping:
      - The prompt said "write the pending methods and nothing else", which a model reads as *return
        only those methods*; the gate then refuses the fragment ("no `impl Led for …`"). Rule 1 now
        says **return the COMPLETE file**, with the reason spelled out — the output contract was
        implicit and had to be explicit.
      - A gate refusal was terminal, so those two runs ended with a refusal a user could not act on.
        `generate` now retries **once on a gate refusal**, feeding the reason back ("the answer's
        impls: …" — a new excerpt, since the answer itself is dropped) — the same courtesy the parse
        error already got. Two refusals in a row still end as a refusal: the gate is the authority.
      - Both of my own first assertions in the live test were wrong in the same way, and neither was
        a product bug: `source.contains("unimplemented!")` matched the scaffold's **doc header**,
        which mentions the macro; `source.contains("impl Led")` missed `impl<'d> Led for
        GpioLed<'d>`, which is how a lifetime-carrying GPIO type must be written. Text is the wrong
        tool for both questions; the measure reads impl blocks, and it is what the test asserts on.

Design notes worth keeping: the contract stays **`no_std`** and **synchronous** (`Spawner` is
the backend's only obligation, so the rp2040 backend supplies its own synchronous scheduler and
does not depend on `spire-hal-std`); backends are per **family**, not per chip (the chip is the
`MCU` env var, the triple is the variant); and the contract grows a trait only when a *second*
family needs it.

**9f — building what the model wrote** (closed). `SPIRE_LIVE_FILL_DIR=<dir>` keeps the live fill
test's project on disk so its backends can be built by hand; doing that found three scaffold gaps
(all fixed) and the limit that mattered, which is now closed by a repair round:

- **Gap 1, fixed**: the emitted `no_std` backend had no `#![no_std]`, so `cargo build --target
  thumbv6m-none-eabi` failed with "can't find crate for `std`". Nothing had ever built a backend —
  the host `cargo test` deliberately excludes them. Emitted per family now (`uses_std_executor`
  decides; an esp-idf backend must *not* have it, since `std::thread` is its executor) and pinned by
  the scaffold test.
- **Gap 2, fixed**: `rp2040-hal` does not re-export `embedded-hal`/`nb`, yet its GPIO and delay
  methods *are* those traits — so the family carries its own `deps` and the prompt quotes the
  manifest's `[dependencies]` verbatim (the list the compiler enforces) rather than asserting a
  count. "One dependency, not two" was a fact about esp-idf-hal generalised into a rule.
- **Gap 3, fixed — and the one that hid the real failure**: the version, not just the name. The
  scaffold pinned `embedded-hal = "0.2"` while `rp2040-hal` 0.10.2 depends on `embedded-hal =
  "1.0.0"` (checked in its manifest: `[dependencies.embedded-hal] version = "1.0.0"`, plus
  `embedded_hal_0_2` as a *renamed* compatibility dep). A backend importing
  `embedded_hal::digital::OutputPin` then has the *other* crate's trait in scope, and
  `set_high` does not resolve however right the import looks — the compiler's own words were
  "the following traits which provide `set_high` are implemented but not in scope", naming a trait
  that does not provide it for that `Pin`. The scaffold test now pins the version, so this cannot
  regress silently.
- **The limit, closed**: the vendor API's *shape* cannot be guaranteed by a prompt. rp2040:
  `gpio::Output` does not exist (the type is `FunctionSio<SioOutput>`, and `Pin` takes three
  parameters). esp32: `PinDriver<'d, MODE>` takes **one** generic since 0.47 (`PinDriver::output`
  erases the pin) where the model wrote two, and `AnyOutput` does not exist. `spire-hal`'s
  hand-written esp32 backend documents that trap in a comment — the knowledge was in the repo and
  never reached the prompt.
  Two legs, both landed:
  - **(a) the data**: `library_hints` name the shapes for all five embedded entries (`rp2040` and the
    four esp ones; the prompt already injects the field). The rp2040 entry's claim was *wrong* —
    it said `embedded_hal::digital::v2::OutputPin` — so the compiler's output, not memory, is what
    the text now repeats.
  - **(c) the loop**: `embedded_hal_fill_apply` builds each backend it wrote through the *platform
    module* (the same `Build` message the UI sends, `package` naming the crate), and on failure
    makes **one repair call** whose prompt appends the compiler's errors verbatim, then rebuilds.
    One round only, and the file keeps what the compiler saw if the repair is refused — a failed
    repair cannot leave a backend in a state nobody has built.
- **Proven, not asserted**: `a_scaffolded_backend_builds_after_one_repair_round` drives the real
  route (scaffold → plan → apply) with a **scripted** model that answers rp2040's wrong API first and
  the corrected one second. The build is real (rustup toolchain, `thumbv6m-none-eabi`, `rp2040-hal`
  from the registry), so the test asserts `repaired: true` and `built: true`, and that the file on
  disk is the repaired one. No API key, no flakiness, and it is the failing case: swap the second
  answer for a wrong one and it fails.
- **And with the real model** (`a_real_model_fills_and_the_backend_builds`, now passing): the hints
  did their job — the first live answer since them declared
  `Pin<Gpio25, FunctionSio<SioOutput>, PullDown>` and `use embedded_hal::digital::OutputPin;`
  correctly, and the IO traits resolved. The failure moved to the *crate list*: the model wrote
  `cortex_m::asm::delay(...)` for `DelayMs` — idiomatic for a Cortex-M0, and unavailable, because
  rp2040-hal *uses* `cortex-m` 0.7 without re-exporting anything from it, so a backend can only name
  it by declaring it. The repair ran, quoted rustc's "use `cargo add cortex_m`", and the model kept
  the crate it could not have — one round is not enough when the error asks for a dependency the
  manifest will not grant. Fixing the *data* instead was the honest move: `cortex-m = "0.7"` (the
  version 0.10.2 itself depends on, same rule as `embedded-hal`) is now one of rp2040's `deps`, and
  the model's own answer builds unchanged.
- **Two things driving it taught, now in the code**: the tool receives the plan as the *items array*
  (the UI hands back `plan`), which the verification read as `plan["plan"]` — it silently checked
  nothing; and requiring a prior `build_analyze` was a hidden ordering requirement (the analysis store
  is best-effort: an analyze can succeed and the lookup still miss), so the verification now analyzes
  on demand and every skip names its reason — "written" and "verified to build" stay different claims.
- **Still open (smaller)**: the esp32 leg's build needs the ESP-IDF SDK, so its verification runs only
  where that is installed; the deterministic tests cover rp2040 only. (The one-round limit this entry
  used to carry is closed — see below.)
- **The loop is now bounded at three rounds, not one** (done 2026-09-17, and the first piece of the
  verify spine). `Repair` stopped being a special case of the fill: the shape gate → build → hand the
  compiler's errors back → rebuild now lives in `build/verify_spine.rs`, and the fill leg implements
  it (`FillArtifact`) instead of repeating it. Two things came out of writing it once:
  - **A setup failure is not a compiler failure.** No module for the platform's `os`, a closed
    channel, a toolchain that is missing — a model cannot fix any of that, so the spine stops and
    reports `built: null` + `not_built: <reason>` rather than spending a call to be told nothing. The
    fill's three-valued `built` (`null`/`true`/`false`) finally has a name for what it meant.
  - **`rounds` is reported, not just `repaired`.** "Built after one repair" and "built after three"
    are different facts about the same success.
  `MAX_REPAIR_ROUNDS = 3` is a cost ceiling chosen from evidence: a live run's first answer fixed the
  GPIO type and its second still needed the trait in scope, so one round was measurably short.
  Verified without a key: `a_second_wrong_answer_still_gets_a_third_round` scripts exactly that
  sequence through the real route (scaffold → plan → apply) with a real compiler and asserts
  `rounds: 2` and `built: true`; the spine itself has six unit tests (a fake that fails twice, a
  refused repair, a gate refusal, a setup failure, `max_rounds: 0`) that need no model, no toolchain
  and no files.


