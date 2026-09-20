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
- [x] 4. M4 — run→fix loop (done 2026-09-17). Feed a failing `run_test` back into the LLM fix
      loop (rebuild → redeploy → re-run), bounded and revert-safe. `device/fix_test` runs the board's
      tests (through M3's cross-build leg), and on failure hands the board's own output to the
      **existing** code-modification path (`modify/code`) rather than growing a second fix mechanism,
      then runs again. Two disciplines make it safe to automate rather than a loop that quietly
      rewrites a project:
  - **Revertible** — the project must be a git repository (the wizard's scaffolds make every project
    one, with a committed baseline, precisely so LLM changes are reviewable as a diff), so the loop
    refuses with that reason rather than editing a tree nobody can undo. It reports what it touched
    and how to undo it, and does **not** run the undo: discarding a user's changes automatically is a
    bigger act than the one they asked for.
  - **Bounded** — `max_rounds` counts *fixes* (default 2, capped at 4). A passing run ends the loop;
    a refused fix ends it with the reason, because a model that cannot produce a change on one round
    will not on the next. A run that could not happen (no board, a failed build) is reported as such
    instead of being turned into a fix prompt — the same setup-versus-content split the verify spine
    made for compile failures.
  The prompt is a pure function with its own test: the board's words verbatim, a bounded tail (a
  failing harness can print thousands of lines), and the rule that keeps a "fix" from being a deleted
  test. Writing the revert report caught a real defect in the first attempt — `git diff --name-only`
  does not mention **untracked** files, so a fix that *created* a file would have reported "nothing
  changed"; it reads `git status --porcelain` and separates modified from created, because the two
  are undone differently (`git checkout --` versus deleting). A board is needed for the whole loop,
  so what is pinned here is the prompt, the git refusal, the two change lists, and the preconditions;
  the loop's rounds are exercised by the same shape as the fill's (the verify spine).
- [x] 5. M5 — trap control + all boards (done 2026-09-17). Tools `run` / `start` / `stop` /
      `status` / `logs`, generalized across boards. The board side lands first (`spire-target-mcp @
      4dad5c7`), because it is the machine holding the processes: `procs.rs` is a name → pid table for
      the life of the server, with `run` (start and wait — `run_test` under the name a non-test binary
      deserves, one implementation behind both), `start` (background, stdout *and* stderr appended to
      `<work>/logs/<name>.log`), `status`, `logs` (bounded tail) and `stop`. Three limits keep it from
      becoming a board full of orphans: **named, one per name** (starting a running name is refused, so
      `stop` always means what the caller started), **logged from the first byte**, and **TERM → 2s →
      KILL**, killed by pid so it still works for a child that outlived a restarted server. A process
      that exited on its own stays listed as `alive: false` — "it crashed" and "it never started" are
      different answers. Tested against real processes: start → status → logs → stop, the duplicate
      refusal, a self-exiting process, and the path-traversal refusal on the name.
      The host side is a **thin pass-through** (`device/run|start|stop|logs|procs`), deliberately: the
      board owns the state, and mirroring it in Spire would give two answers to "what is running" with
      the stale one on screen. It resolves the platform's `device.mcp`, connects, calls the tool and
      folds the board's `isError` into `error` — keeping the board's own words, which name the pid, the
      signal and the log path. The host name for the board's `status` is **`device/procs`**: `device/
      status` is Spire's own listing of the *boards* it knows, and a name that answered both questions
      would be the confusing one — pinned by a test. Validation paths (no platform, unknown platform,
      a platform with no board) match the existing `device/*` family.
      The UI followed (Batch B): the Device group gained a **Board processes** panel — the list from
      `device/procs`, per-row Logs and Stop, and a Start button for the artifact this project built for
      that platform. Still open: a board to exercise it against (connect, start, read, stop), and a
      per-process name field, since the panel starts everything under the platform's name.
- [x] 6. Backlog — first-class prompt→generate→verify everywhere (2026-09-17: the spine exists and
      every generator that writes code now reports through it; what remains is the from-scratch
      non-HAL route, which has not been exercised). Wire the
      generate tools (`createProject/*`, `hal_*`) into the verify spine so new
      code is compile-verified as it is generated; covers brand-new HAL
      contracts, new toolkits, and from-scratch projects. The spine itself landed
      (`build/verify_spine.rs`): gate → build → hand the compiler's errors back to the generator →
      rebuild, bounded. Its implementations, which is what justified extracting it:
      the fill leg (`FillArtifact`, three repair rounds), `embedded_hal_write_contract`
      (`ContractArtifact`, zero rounds — the source is the user's own, so there is no generator to
      hand errors to, and the result carries the compiler's words for the caller to act on), and the
      C++ placeholder writers (`CppStubArtifact`). An authored contract needed no cross toolchain:
      the contract crate is `no_std` but dependency-free and host-testable by design, so it is
      verified everywhere.
  - **The scaffold's own backends** (done 2026-09-17). `createProject/Scaffold` now reports
    `backend_verification: [{family, platform, crate, built, errors?, not_built?}]` — one entry per
    family, three-valued like everything else on the spine: **built**, **broken** with the compiler's
    words, or **not built** with the reason (this machine's missing target or SDK). The scaffold never
    fails for a missing toolchain — the project exists either way — and `verifyBackends: false` opts
    out, because each entry is a real cross-build (minutes on a cold cache).
    This is the check that previously happened **by hand** and found the scaffold's own gaps
    (`#![no_std]` missing, `embedded-hal = "0.2"` where the HAL implements 1.0, `cortex-m`
    undeclared) — so the next such gap surfaces when a project is created rather than the first time
    someone builds a board. Verified end to end: the wizard's route scaffolds an rp2040 project and
    the result says `built: true`; on a machine without the target the test asserts the other honest
    outcome (a non-empty reason, never `false`).
    It also uncovered a **third instance of the same hidden ordering trap** and fixed it at the
    source: every build path did `get_analysis(..).ok_or_else("run AnalyzeProject first")`, and the
    store is best-effort — so an analyse could report success and the build then demand one. Build,
    test, format and lint now go through `analysis_for(path)`, which uses the metadata
    `analyze_project` *returns* when the store is empty instead of discarding it and looking for it.
  - **The from-scratch route** (measured 2026-09-17; one half still open). Exercising it settled what
    was previously an assumption: planning a project from a goal **requires a model** — with none
    wired, `createProject/Plan` refuses by name ("LLM unavailable — the project creator is not
    connected to the LLM service"), which is a deliberate property (`generate_plan`: *"nothing may
    ever be scaffolded without a real plan"*) and now pinned by a test, together with the fact that a
    refused plan writes nothing. So the route is not "unverified"; it is *model-planned*, and the
    verify spine is inside the plan itself (the prompt asks for `parse_and_validate` and a final
    `build` step, and `ExecutePlan` runs them).
    **Still open**: the scripted-model half — drive that route with a fake endpoint and assert the
    executor's writes, parse gate and build gate against a real filesystem and cargo. It needs the
    test harness to wire the *project creator* to a model, which that harness deliberately does not
    do (a harness that gave the embedded route a model could not prove that route needs none), so it
    is a small, explicit harness change rather than a half-wired one.
  - **The C++ writers** (done 2026-09-17). `hal_add_target` used to write a `<stem>_stub.cpp` per
    interface, wire `hal/meson.build` and report — nothing between the write and the claim. It now
    reports `compile: {built, refused, not_built, errors}` from the spine, where the gate parses
    every stub that landed (with the same tree-sitter CST the HAL analyzer uses: a stub that does
    not parse is one the coverage map reads as an *absent* interface, not as a placeholder) and the
    build is the platform's own `meson compile`. The `meson compile` gate is now **one** helper
    (`cpp_compile_gate`) shared with `hal_generate_apply`, which had its own inline copy — two copies
    would drift exactly where it matters. Writing it taught the distinction the hard way: a
    `build-rpi5` *directory* is not a build directory (meson writes `build.ninja` at setup, and its
    own answer for a bare directory is "Current directory is not a meson build directory"), so the
    gate checks for a configured build dir and reports the rest as `not_built` with the `meson setup`
    command that would fix it — a fabricated build directory must not yield a fabricated verdict.

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

> The *flash* in that sentence is verified; the *blink* is qualified by the 2026-09-18 section
> below, where the same firmware panicked in the actor's mailbox until the pthread stack was
> raised. Read the two together: the flash leg worked here, and the loop is what the config
> below makes true.

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

### 8, on the board again (2026-09-18): the flash leg verified to the byte, and the actor loop that stayed silent

Same board (ESP32 rev v3.0 / M5Stack Core2), same firmware, same platform. This run added the
step the earlier one did not have: **the check that the chip is running the bytes we built.**
IDF prints `ELF file SHA256` at app startup, and it matched our ELF's own prefix — `shasum -a
256 …/blink-esp32` → `00f1a099d`, device → `ELF file SHA256: 00f1a099d…`. An exit-0 flash says
the tool believed it worked; a hash match says the *silicon* is running this build. The leg's
shape is now pinned by `a_real_board_is_flashed_through_the_flash_leg` (`#[ignore]`d, skipped
unless `SPIRE_FLASH_PROJECT` names a project with a binary — an embedded-HAL project is a library,
so there is nothing in *it* to flash). It asserts the command too, not just the exit code, because
`--chip <platform>` and `--port` are where the leg's decisions show up.

What that run turned up, in the order it bit:

- **`espflash flash <elf>` writes only the app.** The board was left with a bootloader from a
  *different* IDF (`v6.1-beta1`) while the app was `v5.5.5`, and nothing said so — the flash
  succeeded either way. Flashing our own bootloader (`--bootloader <build>/bootloader/bootloader.bin`)
  is possible, and the panic below was *not* caused by the mismatch (it survived the fix), but a
  leg that claims "the board runs this build" should carry the bootloader and partition table
  from the same build. Ours passes neither.
- **The build's own partition table cannot hold the app.** IDF's default single-app layout gives
  the app 1 MB; a std **debug** Rust app is 1.13 MB (`App/part. size: 1,181,776/16,384,000` — and
  16,384,000 is the *stale* table's factory partition, not ours). Flashing our table fails with
  *"Supplied ELF image of 1181792B is too big"*. The board only ran the app because it happened to
  carry a 16 MB factory partition from an earlier life. A firmware project needs a `partitions.csv`
  (this is a *project* fact, not a platform one — but it is the reason "it flashed fine" proves less
  than it looks).
- **The actor loop printed nothing: `Guru Meditation (LoadProhibited)` at `app_main`.** Symbolized
  with the toolchain's `xtensa-esp32-elf-addr2line`: `pthread_mutex_unlock` ← std's
  `MutexGuard<Waker>::drop` ← `SyncWaker::register` ← `std::sync::mpmc::array::Channel::recv` —
  i.e. the *actor's mailbox*, before a single message was handled. The cause is a default, not a
  bug in the code: `CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=3072` in IDF, and Rust's `std::thread`
  **is** a pthread — so `spire-hal-std`'s executor gave the actor a 3 KB stack, which std's mpmc
  receive path (deep frames in a debug build) overflows into a wild pointer. Adding
  `examples/blink-esp32/sdkconfig.defaults` with `CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=16384`
  flipped it from panic to `blink: on` / `blink: off`, 15 lines in 16 seconds, no panics. The
  earlier note that "the console showed the actor loop" was written from a flash that had
  certainly worked; with *today's* toolchain the loop is only true with this config, so that line
  now means "the flash was verified then, the loop is verified now" — and the difference between
  the two is exactly the kind of thing a hash match and a monitor capture settle.
- **The flash leg now carries the build's own bootloader and partition table** (the change after
  `e739c70`). `esp_flash_command` gained `--bootloader` / `--partition-table`, filled from
  `esp_bootloader_path` / `esp_partition_table_path` — a glob of
  `target/<triple>/<profile>/build/esp-idf-sys-*/out/build/`, newest match wins, `None` when the
  project is not esp-idf-sys's (so a non-IDF esp artifact still flashes app-only). Both are
  arguments, never environment, which the spec test pins. The behaviour change is deliberate: a
  project whose table does not fit its own image now **fails at flash time** instead of silently
  writing into whatever partition an earlier firmware left on the chip.
- **The partition table that does fit is a project fact, and it is one line.** IDF's default
  single-app layout gives `factory` 1 MB; `SINGLE_APP_LARGE` gives it **1500K**
  (`partitions_singleapp_large.csv`), which holds a 1.18 MB debug std image at 76.94%
  (`App/part. size: 1,181,776/1,536,000`). That is where the fix went — into the firmware's
  `sdkconfig.defaults`, beside the stack size — because a *library* (what the embedded-HAL scaffold
  produces) has no partition table at all.

- **Spire's esp leg passes no IDF configuration at all** (no `ESP_IDF_SDKCONFIG_DEFAULTS`, and the
  registry entries carry none), so a project's `sdkconfig.defaults` is read only by esp-idf-sys's
  own convention, at *configure* time — adding the file after a first build changed nothing until
  `cargo clean -p esp-idf-sys` forced a re-configure. Worth knowing before the next "why is the
  config ignored".

**Resolved on hardware, same day.** The flash completed through Spire's own leg, which now emits the
full set itself — the command it ran was `espflash flash --chip esp32 --non-interactive --port …
--bootloader …/out/build/bootloader/bootloader.bin --partition-table
…/out/build/partition_table/partition-table.bin …/debug/blink-esp32` → exit 0, 66.6 s,
`App/part. size: 1,181,776/1,536,000 bytes, 76.94%`. The chip's boot log then said what the build
said: `I (13) boot: ESP-IDF v5.5.5-dirty 2nd stage bootloader` (not the `v6.1-beta1` it had been
running) and `2 factory  factory app  00 00 00010000 00177000` — the build's **1500 KiB** factory
partition, where it used to read `00fa0000` from a firmware nobody could name. Then `blink: on` /
`blink: off`, 13 lines in 15 s, no panics. So both halves are now measured on the board rather than
argued: the leg writes the set this build produced, and the table is one this build's image fits.

The wedge that delayed it is worth keeping as an operational fact. An interrupted transfer left the
adapter **readable but unprogrammable** — `stty -a` printed the settings while every `tcsetattr`
returned `Invalid argument`, and a plain `cat` opened fine — and the driver instance survived a
*quick* re-plug (`ioreg` showed `USB Single Serial`, `busy 0`, same node). What cleared it was
**a different USB port**, i.e. a real re-enumeration, not just re-seating. espflash is invoked with
no `--baud`, so it uses the adapter's default (460800 here); if this recurs on a long transfer, a
conservative `--baud 115200` is the knob — and one the leg does not currently expose.

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

**Where the toolchain and SDK come from is not Spire's business** (2026-09-18). The rule, stated
where the wizard shows it: `espup` installs the Rust toolchain (and the clang bindgen needs),
`esp-idf-sys` downloads ESP-IDF on the first build, and Spire **installs nothing** — it references an
existing install through environment variables. Four of them are now *read* rather than assumed:
`ESP_TOOLCHAIN_BIN` (a toolchain installed anywhere), `RUSTUP_HOME` (a relocated rustup), `$HOME/.rustup`
(the default), and `LIBCLANG_PATH` (an exported clang). Two were previously the wrong way round: the
toolchain was only ever looked for at `~/.rustup/toolchains/esp` with no way to say otherwise, and a
*discovered* clang was written **over** an exported `LIBCLANG_PATH` — Spire overriding the one setting
the user had been explicit about. The order is now explicit → standard variable → default, pinned by
`build/esp.rs`'s tests, and a set-but-wrong `ESP_TOOLCHAIN_BIN` falls through rather than failing a
build that would otherwise have worked (a typo must not lose a discovery). The story is also in every
esp platform's `library_hints` and in the scaffold's per-family README block, because "run `espup
install` first" previously existed only in code comments — the one place a user never reads.

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
- **And the esp32 family, live** (2026-09-18). The fill leg had only ever been run against rp2040,
  because the esp32 leg was believed to need an SDK this machine lacked. With that corrected (the SDK
  was installed; see below), the same loop runs for esp32c6 and **passed on the first answer**:
  `{"built":true,"repaired":false,"rounds":0}` — no repair needed. What the model wrote is worth
  recording, because it is the shape the platform hints describe: `PinDriver<'static, Output>` with the
  pin **erased** by `PinDriver::output(pin)` and a generic `P: OutputPin + 'static` on `new`, plus
  `set_level(Level::High/Low)` and `active_low` inverted once in `new`.
  That also corrects the note in this entry that said "esp32: `AnyOutput` does not exist and
  `PinDriver<'d, MODE>` takes **one** generic where the model wrote two". The generic count is right —
  one — and `esp_idf_hal::gpio::Output` *does* exist as the driver's mode type; what was wrong in the
  earlier answer was `AnyOutput` and a second generic, and the hints added for it are evidently enough
  for a model to get it right unaided. The two live tests now share one `live_fill` body (scaffold →
  plan → apply → build) so the families cannot drift in the part that matters — the assertions.
- **And with the real model, before that** (`a_real_model_fills_and_the_rp2040_backend_builds`): the
  hints
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
- **The esp32 leg is verified, not assumed** (2026-09-18). The note here used to say its verification
  needed an ESP-IDF SDK this machine did not have. That was **wrong**, and worth recording as wrong:
  the SDK was installed all along (`~/espressif` at 7.0 GB, with the `esp-idf` v5.5.5 clone and the
  `riscv32-esp-elf` / `xtensa-esp-elf` / `esp-clang` tools), and the thing that was missing was
  `idf.py` *on `PATH`* — which esp-idf-sys never uses, because it manages the install itself through
  `ESP_IDF_TOOLS_INSTALL_DIR`. Reading "not installed" off a missing human-facing CLI is exactly the
  kind of inference this repo keeps catching in itself.
  So it is measured now: `an_esp32_backend_builds_where_the_sdk_is_installed` scaffolds an esp32c6
  project through the wizard's route and asserts the result —
  `{"built":true,"crate":"blink-esp-hal-esp32","family":"esp32","platform":"esp32c6"}`, **110 s** for
  a cold build of std and the SDK components for that chip. It is `#[ignore]`d (minutes, even warm)
  and it needed one harness change: the integration harness registered only the rp2040 platform
  module, so an esp `Build` had nowhere to route. Both modules are registered now, which is harmless
  because routing keys on the platform's `os`.
  That also means the env-resolution change above is exercised in production, not just in unit tests:
  the same run finds the esp toolchain, sets `LIBCLANG_PATH` and pins
  `ESP_IDF_TOOLS_INSTALL_DIR=global`.
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

### 9, the wizard becomes a tree (2026-09-18): two projects, not one project type

The new-project wizard asked its questions in a **fixed order** — Environment → Targets → Toolchain →
Structure → Details — which meant every project walked past every question, including the ones its own
shape had already answered. It is now a tree, and the branch decides the steps:

    Environment ─┬─ Native ────┬─ Spire UI App         → spire_app
                 │             └─ CLI                  → native
                 └─ Embedded ─┬─ Linux SBC (C++) ───── Targets → Structure
                              │                          → single_source | hal   (unchanged)
                              └─ Controller (Rust) ─┬─ HAL + Frameworks → embedded_hal
                                                   └─ Application      → embedded_app

Why it is data rather than an enum's order: a Native CLI is three steps, a Linux SBC five, a
Controller five, and the two Embedded branches never offer each other's platforms. The split falls out
of the registry's own flags — boards are embedded platforms with a `family`, Linux SBCs are embedded
platforms without one — so the wizard filters on what `platforms/list` already says instead of
re-deriving "is this a board" a second time.

**The decision worth recording: the HAL and the application are two projects, not two shapes of one.**
The embedded-HAL project stays a **library** (contract + one backend per board family + the executors)
— what the drift cascade fills. An application is a **separate project** that *depends on* it: its
Cargo.toml path-deps the HAL's contract crate and the chosen board's backend crate, exactly the
`blink-esp32 → spire-hal` relationship that has been hand-written until now. That is why Application is
its own structure (`embedded_app`) rather than a flag on `embedded_hal`, and why the wizard will ask
for the **HAL project directory** (a path picker) as well as the board: the board names the backend it
depends on, the directory names the HAL it belongs to.

Landed so far (this change): the tree, the branch filtering, and the leaf → `ProjectStructure` mapping,
with three tests pinning it — the steps per branch, the key each leaf becomes (an unknown key falls
back to `native` **silently**, so a typo would scaffold a host crate for a firmware choice), and the
board/SBC split. The **Application leaf is shown but not offered** (`enabled: false`), because
`embedded_app` does not exist in the core yet and a selectable card would scaffold that host crate.
Turning it on is one flag, in the change that lands the structure and its scaffold:

- `spire-core` — `ProjectStructure::EmbeddedApp` (`"embedded_app"`), recognized by a declaration
  (`[workspace.metadata.spire] structure = "embedded_app"` plus the HAL path it depends on), not by a
  layout guess — the same rule `EmbeddedHal` follows.
- the scaffold — a Cargo **binary** crate (`main.rs`, an actor driving the HAL's traits), `[dependencies]`
  path-deps read from the chosen HAL's workspace manifest, and the board's build wiring: for esp,
  `.cargo/config.toml` (target / `build-std` / `MCU` / `ldproxy`), `sdkconfig.defaults` (the 16 KB
  pthread stack **and** the 1500K partition table — both learned on the board) and a `build.rs` with
  `embuild::espidf::sysenv::output()`; for rp2040, `memory.x` and the probe-rs/picotool flash step.
- the plan — the LLM writes `main.rs` inside that scaffold. The HAL cascade is *not* part of an app's
  plan; it belongs to the HAL project, which is the point of splitting them.
**Landed (2026-09-18): the application structure and its scaffold.** `ProjectStructure::EmbeddedApp`
(`"embedded_app"`) exists in `spire-core`, and `build/embedded_app_scaffold.rs` emits the project:
a package whose `Cargo.toml` path-deps the HAL's **contract crate and the board's backend crate**
(read from that HAL's `members` and each crate's own `[package] name`, so the `use` lines are right
whenever the project name has a dash in it), a `.cargo/config.toml` with the target / `MCU` /
`ldproxy` / runner, the `build.rs` whose absence fails in a place that names nothing, and the
`sdkconfig.defaults` carrying **both** measured settings. The marker records
`structure = "embedded_app"` *and* `hal_path`, so the dependency is a fact rather than something to
infer from `../..`.

Two things about it are worth keeping:

- **Only `src/main.rs` is fillable.** The actor is written out in full — it holds the contract's
  traits and no vendor type, which is what makes it the same file on any board — but the one line
  that cannot be known there is the board's LED constructor, which belongs to the *backend* crate and
  so is written by the **HAL project's** fill. It is a `todo!()` rather than a guess, and because
  `todo!()` type-checks, the wiring around it is compiled and linked before any fill has run.
- **The scaffold is emitted from `project_creation`, not from a build module.** It is not a
  per-language layout: it is one fixed emission whose *input* is another project's directory, and the
  module layer never sees a path (it gets a name, a goal, platforms and a structure). `cargo.rs`
  therefore **refuses** `EmbeddedApp` by name rather than falling through to its Cargo layout — a
  silent fall-through would emit a plain host crate for a firmware choice and report nothing.

Verified by a real cross-build, not by reading: `an_app_cross_compiles_against_a_real_hal`
(`#[ignore]`d) writes a HAL *and* an app to a temp dir and builds the app with the esp toolchain and
the SDK, using the build module's own environment helpers so the test cannot pass with a second copy
of that resolution — **ok in 92 s**. Five unit tests pin the refusals (no board, two boards, a
directory that is not a HAL, a HAL with no backend for the board, a host platform) and the emitted
files (both path dependencies, the marker, the config's target/`MCU`/linker, `build.rs`, the two
sdkconfig settings, and that only `src/main.rs` is fillable).

Still to come, and the reason the wizard's Application card is still off: the **HAL directory has to
reach `project_creation` from the FFI** (a `hal_root` argument on `createProject/Scaffold`, plus the
wizard's picker), after which flipping `enabled: true` on that card is the whole UI change. Two
smaller follow-ons: no `embedded_app_template_plan` yet (an app's plan falls through to the LLM path,
which is arguably right — the app's one job is writing `main.rs`), and no `no_std` app wiring, which
is not writable until a backend supplies the executor an app spawns on.


**Landed: the last hop, so the wizard can actually do it.** The HAL directory now travels from the
FFI (`halRoot` on `createProject/GeneratePlan`, `/Plan` and `/Scaffold`, parsed into a
`hal_root: Option<PathBuf>` on the three `ProjectCreationMessage` variants), and the wizard's
Application leaf is a real choice: the card is enabled, the board step becomes single-select, and a
new **HAL Project** step asks which HAL the app builds against — a directory picker, with the
backend crate it implies (`crates/<hal>-<family>`) shown as a hint. The structure key decides whether
`halRoot` is sent, so no other leaf's request changes.

**The plan had to be built too, and that is what makes the flow work at all.** `PlanView`
materializes a project by *executing the plan's steps*, so a structure with no template plan falls
through to the LLM path and writes nothing of its shape. `embedded_app_template_plan` is therefore
the app's scaffold in step form — the same split `SpireApp`/`EmbeddedHal` use, where the structure is
fixed before the goal is read and the goal only shapes the code inside it.

**A bug the tests caught by being asked the right question:** the target triple and `MCU` were
family-level, but one *family* spans chips with different triples (`xtensa-esp32-espidf` for the
classic, `riscv32imac-esp-espidf` for the C6). They now come from the platform's own `rust:` block —
`cargo_config` takes them as arguments, and `MCU` is written only for an esp-idf platform, since
`idf_target` on an rp2040 is `probe-rs`'s spelling, not IDF's. The unit test now scaffolds for
**esp32c6** and asserts the RISC-V triple and `MCU = "esp32c6"` while the *backend crate* is still
`-esp32` — one family, one backend, per-chip compilation facts. The live cross-build (still on the
classic esp32, the board on this desk) passes after the change: 130 s.

Worth knowing for the next person: the wizard's own plan path (`createProject/GeneratePlan`) is what
`NewProjectView` calls, and `PlanView` executes it step by step — there is no separate `Scaffold` call
on that route (`ProjectWizardView`, the only caller of `scaffoldProject`, is not presented by
anything). So "the plan is the scaffold" is not a shortcut; it is how this flow has always worked.



## 10. The embedded corpora — and the retrieval bug that filling them exposed

The RAG had two corpora about *spire itself* (`spire-core`, `spire-actor`) and nothing about the thing a
generated firmware crate is actually written against. Four manifests were added to close that, and the
exercise found a retrieval bug that made every one of them invisible.

### The corpora, and why each one

| corpus | source | what it is for |
| --- | --- | --- |
| `esp-rs-book` | github `esp-rs/book` | the *decisions*: toolchain, `esp-idf-sys` + the build env, std-vs-`no_std`, flashing. Prose, so the parser is at its best here |
| `esp-idf-hal` | github `esp-rs/esp-idf-hal` | the API a generated backend calls: `src/` (doc comments *are* the reference) **and** `examples/` (real, compiling usage — a call shape is usually visible there and nowhere else) |
| `esp-idf` | github `espressif/esp-idf` | the layer underneath: Kconfig settings, partition tables, the C API the bindings expose as `esp_idf_sys`. Scoped to `docs/en` — the repo is 22,010 files and almost none of the rest is documentation |
| `rust` | **local** `~/.rustup/toolchains/esp/lib/rustlib/src/rust/library` | `core`/`std`/`alloc`, as the board toolchain compiles them: `-Zbuild-std` builds std from exactly this tree, so a generated crate calls the API it will link against |

Board **facts** (pin numbers, target triple) did **not** become a corpus: they are a two-line table in
`esp32.yaml`'s `library_hints`, where the prompt already reads them. A corpus is for what is too big to
inline; a fact is not.

The std corpus is the one source with a **local path**, and it has to be: `espup` installs the toolchain
outside any project, and the ingest has no variable expansion. It is called out in the manifest, and a
missing path is reported as a skipped source rather than as a failure. The three others clone (cached
under `~/.spire/knowledge/.cache/<id>`) so the bundle stays portable.

### One list, three readers

`actors::rag_bundle` now owns `BUNDLE` (the six manifests, `include_str!`-embedded) and
`install_into(dir)`, which writes `<corpus>/ingest.yaml` into the KnowledgeStore's scan dirs. The
coordinator's `rag/install-bundle-manifests` calls it, the manifest tests parse it, and the live fill
ingests it — so "the bundle" cannot come to mean three different sets, and a manifest that is not in
`BUNDLE` is not shipped at all.

### The pattern trap, measured

`**/src/**/*.rs` does not match `src/gpio.rs`. `glob_to_regex` expands `**` to `.*` and `*` to `[^/]*`,
so the pattern becomes `.*/src/.*/[^/]*\.rs` — the `.*` after `src/` must be followed by a `/`, i.e.
nested-only. The first fill therefore ingested **16 files** of esp-idf-hal where the tree holds **84**,
and 21 of the book's 25 pages. Nothing failed: an ingest that matches a third of its tree reports a
smaller chunk count, which reads exactly like a small corpus.

The fix is to list each directory at **both** depths (`**/src/*.rs` *and* `**/src/**/*.rs`), and the
thing that keeps it fixed is a test that asserts each manifest against *paths from the tree it points
at* rather than against its own count:

- `include_patterns_match_the_real_files_and_not_the_neighbours` claims `/…/esp-idf-hal/src/gpio.rs`,
  `/…/esp-idf/docs/en/index.rst`, `/…/library/core/src/lib.rs` (all of which the nested-only form
  missed) and asserts it does *not* claim `/…/esp-idf-hal/tests/hil.rs`,
  `/…/esp-idf/docs/zh_CN/index.rst`, `/…/library/portable-simd/…`;
- `path_matches` was made `pub` for it — a pure predicate over (path, manifest), and the only way to
  test patterns where the patterns are written, since they live in another crate.

Chunk counts after the correction, measured on the fill: book 47 (was 37), esp-idf-hal **829** (was 177),
ESP-IDF 3,732 from 508 files, std 8,952 from 1,014 files — each equal to the file count in the tree it
points at, which is the check that matters.


### The bug filling them exposed: retrieval could not see a small corpus

`semantic_retrieve` used to read **the first 500 `rag_chunk` nodes in the whole store** and drop the
other domains in-process:

```
QueryAttrNodes { subtype: "rag_chunk", limit: 500 }   // store-wide
for n in nodes { if n["domain"] == domain { … } }     // then filter
```

While a store holds one corpus that is the same as filtering in the graph. This store holds eight —
swift 11,206 chunks, swiftui 7,244, rpi5 573, raspberry-pi-5 570, a7s 90, spire-core 77, spire-actor 2
— so **19,799 chunks**, and the window was filled long before it reached a freshly ingested corpus.
The measured symptom, on a corpus that had just ingested cleanly:

```
esp-rs-book: 37 chunks, 21 sources        (ListDomains — counted straight from the graph)
esp-rs-book: 0 hits                       (every query, for every one of its chunks)
```

That is the worst shape a bug can have: the ingestion was right, the count was right, and search
returned nothing — which reads as "the embedding is poor" or "the corpus is thin", not as a one-line
query bug.

**Fixed** by scoping the read to the domain in the graph:

- `MemoryGraphMessage::QueryAttrNodesWhere { subtype, props: Vec<(String, String)>, limit }` — an
  **added** variant (the existing `QueryAttrNodes` would have needed a new field at all 62 call sites,
  most of them in other products). It builds the same GQL with the property equalities ANDed into the
  `WHERE`, so only the domain's rows are resolved.
- `semantic_retrieve` asks for `subtype = "rag_chunk"` **and** `domain = <domain>`, and takes up to
  `MAX_SCORED_CHUNKS` (500) of them.

Same corpus, same store, after the fix — `3 hits, best 0.679` from
`esp-rs-book/src/getting-started/index.md`. `rag/search` and `rag/find-interfaces` are the same path
(`semantic_retrieve`), so this was every RAG tool, including the RagView search box.

All four corpora after the fill, queried with `"toggle a GPIO output pin"`:

| corpus | chunks | hits | best |
| --- | --- | --- | --- |
| `esp-rs-book` | 47 | 3 | 0.679 — `src/getting-started/index.md` |
| `esp-idf-hal` | 829 | 3 | 0.819 — `src/reset.rs` |
| `esp-idf` | 3,732 | 3 | 0.477 — `docs/en/migration-guides/…/peripherals.rst` |
| `rust` | 8,952 | 3 | 0.489 — `library/alloc/src/lib.miri.rs` |

(`esp-idf-hal` scoring only 500 of its 829 chunks is the truncation below, visible in the result: the
best hit comes from the first 500. The other three are under the budget and fully scored.)

**What is still limited** (deliberately, and worth knowing before reading a poor result as a bad
corpus):

- There is no vector index. Scoring **re-embeds every candidate** on every query, because a chunk
  stores its `embedding_id` (a text hash) but not its vector, and the `Embedder` trait has no
  lookup-by-hash. `MAX_SCORED_CHUNKS` is therefore a *latency* budget: a domain with more than 500
  chunks is truncated for scoring, deterministically but arbitrarily. Fixing that properly means
  persisting vectors (and a cosine scan or an ANN index) — a real change, not this one.
- The 500 is now only ever spent *inside* one corpus, which is what makes a 47-chunk corpus searchable
  next to an 11,000-chunk one. The truncation of a *big* domain is the remaining debt.

Worth noticing: `count_type` and `delete_domain_nodes` already read with `limit: 1_000_000` and filter
in-process, and that is why the chunk counts stayed right while search was not. The 500 was the odd one
out.
### Filling and refreshing (the part that is a command, not a button)

Install writes six manifests into `~/.spire/knowledge/<corpus>/ingest.yaml`; ingesting is a separate,
expensive step. RagView does both from the UI, and the same thing is available headlessly for filling a
store in bulk — which is how these four were filled:

```sh
cd spire-code
# one corpus (fast, while writing a manifest)
SPIRE_FILL_CORPUS=esp-idf-hal cargo test --release -p spire-code --test rag_fill_tests -- --ignored --nocapture
# all six
cargo test --release -p spire-code --test rag_fill_tests -- --ignored --nocapture
# a manifest changed -> REPLACE the corpus instead of merging into it
SPIRE_FILL_REINGEST=1 SPIRE_FILL_CORPUS=esp-rs-book cargo test --release -p spire-code --test rag_fill_tests -- --ignored --nocapture
```

**`--release` is not a preference.** A debug build embeds roughly an order of magnitude slower (candle
unoptimised): the std corpus alone took over eight minutes to get through a fraction of its files in
debug and 9.5 minutes for *all four* corpora, start to finish, in release.

`tests/rag_fill_tests.rs` builds the app's RAG wiring in miniature (a KnowledgeStore graph at
`knowledge_dir`, a provenance graph, an embedder registered as a service, a `RagActor` from the
registry) and then asserts what matters at the end: `chunks > 0` per corpus, and **a query that
returns hits** — the second assertion is the one that would have caught the retrieval bug on day one.

Two things to know before running it:

- **Quit the app first.** It writes the same store (`~/.spire/knowledge`, or `$SPIRE_KNOWLEDGE_DIR`);
  one writer at a time.
- The output is the evidence: per-source file/chunk counts, the store's per-domain table
  (`ListDomains`), and the best hit for `"toggle a GPIO output pin"` per corpus. Read it, not the exit
  code — a corpus that matched a fraction of its tree passes `chunks > 0` (see the pattern trap above;
  the count is the tell).

To ask the *store* rather than the fill process what it holds — which is the only way to find out
whether a fill survived, since the store is taken read-write and the app discards any WAL it finds on
startup — `spire-core` has a read-only example for it:

```sh
cd spire-core && cargo run --release --example rag_domain_check -p spire-core   # [domain…] to filter
```

It is what confirmed the four corpora were really in the store and not only in the process that wrote
them (33,322 chunks over 11 domains), and it is the fastest way to see a suspicious count.

Store cost, measured: `~/.spire/knowledge` was 628 MB with eight domains and 19,799 chunks; after the
fill it is 1.1 GB with eleven domains and **33,322 chunks** — the four embedded corpora are 13,560 of
them (std 8,952, ESP-IDF 3,732, esp-idf-hal 829, book 47). Disk, not minutes, is the thing to watch when
widening a corpus: a reingest rewrites the domain's nodes, and the older snapshots stay until SeleneDB
prunes them.

The four manifests are the whole change on the data side; the rest of it was one query bug and one glob
semantic. Neither was in the plan, and both would have made the fill look like it had not worked.

## 11. The HAL re-centers on `embedded-hal` — the actor stays ours

A review of what `spire-hal` had become, and the decision that came out of it: **the peripheral traits
are the ecosystem's, the actor contract stays ours, and drivers become the growth area.**

### The finding

`spire-hal` was two unrelated layers wearing one name:

| layer | what was there | what it is |
| --- | --- | --- |
| hardware abstraction | `hal::{Led, DelayMs}`, `HalError` | a re-invention of `embedded-hal`'s `OutputPin` / `DelayNs` and its error philosophy |
| firmware architecture | `actor::{Actor, Mailbox, Spawner}` | genuinely ours — no ecosystem crate has one |

The first layer was costing the most and buying the least. Every vendor HAL (`esp-idf-hal`,
`rp2040-hal`, `stm32-hal`) implements `embedded-hal`'s traits, and so does (almost) every driver crate
in existence — so a trait of our own stood between a generated project and the entire driver ecosystem,
and asked every backend to implement the same `set_high`/`delay_ms` twice: once as ours, once as the
vendor's (which it already had). The scaffold's own manifests already said as much — the rp2040
backend's has pinned `embedded-hal = "1"` since the day it was written, because `rp2040-hal`'s methods
*are* `embedded-hal` methods and a model that imports the wrong major cannot call `set_high`.

The second layer is not a HAL trait at all. It is the concurrency/architecture model the rest of Spire
leans on: a firmware unit is a `Message` type plus a `handle`, the same shape as the host's
`spire_actor::Actor`, and a message with no handler is a missing implementation — which is what the
drift measure reads. `embedded-hal` has no opinion about any of that.

### The decision

- **Adopt `embedded-hal` 1.0 as the trait layer** and re-export it from `spire-hal`, so a project names
  one version of it and gets the one its backends were compiled against.
- **Keep the actor trio** (`Actor`, `Mailbox`, `Spawner`) as the only trait this workspace owns.
- **Backends supply constructors, not implementations.** A board's facts — which pin, which polarity,
  which executor — live in a `Board` in the backend crate. A `Board` **type**, deliberately not a
  `trait Board`: a trait would be a contract of our invention again, one level up, and would need the
  drift machinery this change removes.
- **Drivers are the growth area.** They are written against `embedded-hal` traits (`SpiDevice`, `I2c`,
  `DelayNs`), which is what makes them board-agnostic *and* host-testable; prefer an upstream crate and
  wrap it in an actor only when it must participate in the actor model.


### Landed (spire-hal, this change)

- `spire-hal`: `hal/{led,time}.rs` and `error.rs` **deleted**; `embedded-hal = "1.0"` added and
  re-exported. The crate is now the actor contract plus that re-export.
- `spire-hal-esp32`: `GpioLed` and `FreeRtosDelay` **deleted** — wrappers whose only purpose was
  implementing our traits, though both types they wrapped already implement the ecosystem's
  (`PinDriver: OutputPin + StatefulOutputPin`, `FreeRtos: DelayNs`). Replaced by `Board::led(pin)` and
  `Board::delay()`, which return those vendor types directly. The crate now has no `impl` block of our
  own in it.
- `contract.rs` (the portability test that justifies the seam): `Blink` is generic over
  `OutputPin`/`DelayNs` and the fakes implement `embedded-hal` — the *same* test with the trait
  swapped. `the_same_actor_runs_against_a_different_backend` still passes, which is the claim that had
  to survive: the actor does not change when the board does.
- `examples/blink-esp32` (the on-hardware proof): `Board::led` / `Board::delay` / `StdSpawner`, actor
  unchanged in shape.

Verified: `cargo test` in spire-hal (4 contract + 3 executor tests, zero warnings);
`MCU=esp32c6 cargo check -p spire-hal-esp32 --target riscv32imac-esp-espidf` (the RISC-V variant); and
`cargo build --release` of `examples/blink-esp32` for the classic ESP32 on the desk.

### Two things worth knowing for the next person

- **`embedded-hal` 1.0 splits the error out of the pin trait**: `OutputPin: ErrorType`, so an
  implementation is two `impl` blocks (`impl ErrorType { type Error = … }`, then `impl OutputPin`). The
  compiler reports the missing `ErrorType`, which reads like a typo in a trait name if you know only
  0.2 or only the pin traits of 1.0.
- **Polarity is the one thing the swap moved, and it moved to the right place.** `Led::set(on: bool)`
  was *logical* (`on` = lit; an active-low board inverted in the backend); `OutputPin::set_high` is
  *physical*. So the actor now drives levels and polarity is a fact of the backend's `Board::led` —
  where board facts already live (`library_hints`, `sdkconfig`). The pilot board is active-high, so it
  needs nothing; a backend with an active-low LED inverts in `Board::led` and the actor above it does
  not change. Smaller surface than the old logical trait, and it says what it means.

### Landed (spire-code, this change) — steps 1–3

They were coupled (the fill reads the measure, the scaffold's output is the measure's input), so doing
one alone would have left the suite red. As one change:

- **The scaffold** (`embedded_hal_scaffold.rs`) emits the new shape. The contract crate is `lib.rs` +
  `actor.rs` with `embedded-hal = "1.0"` re-exported and **no authored traits** — nothing in it is
  fillable. Each backend is a `Board` whose constructors are `unimplemented!()`. `hal/{led,time}.rs`,
  `error.rs` and `HalError` are gone from the emission entirely.
- **The measure** (`hal_rust_contract.rs`) knows what is owed when the traits are the ecosystem's:
  `BOARD_INTERFACE`/`BOARD_TYPE`/`BOARD_METHODS`, a `board_coverage` measure, and
  `extract_inherent_methods_rust` + `placeholder_methods_rust` — because a board's constructors are
  **inherent** methods (`impl Board`), which `impl Trait for Type` never sees. The coverage map reports
  `board` for a backend that declares one and *only* for those, so a hand-written HAL without a board
  is never asked for a constructor it never had. The early return in `rust_platform_coverage_map` no
  longer fires on an empty contract set: a scaffolded project has no contract traits **by design**.
- **The fill** (`embedded_hal_fill.rs`) plans the board: its source is the contract's `lib.rs` (the
  seam that re-exports what a constructor must return), its methods are the constructors, and the gate
  reads both impl forms. The prompt's rules were re-aimed — return `embedded-hal` types, keep vendor
  types inside the crate, and give a constructor the right *signature* for its vendor.
- `write_contract` now creates `src/hal` and `hal/mod.rs`: a scaffolded project has no `src/hal` until
  a project authors its first trait, and requiring the directory made the crate invisible to the very
  tool that writes it. Its refusal message was corrected to "`crates/*-hal` with a `src`".

**The stub's first shape did not compile, and only a real build found it.**
`pub fn led() -> impl OutputPin { unimplemented!() }` fails with *"the trait bound `(): OutputPin` is
not satisfied"* — for a body that never returns the compiler infers the opaque type as `()`. The stub
returns a concrete `UnimplementedLed`/`UnimplementedDelay` instead, so a scaffolded project builds
while still measuring as a stub.

Also found by the fixtures: a "corrected" answer that only *looks* right can be unparsable — the test's
first rp2040 answer lost the third parameter of `Pin<DynPinId, FunctionSio<SioOutput>, PullDown>` and
the gate refused it as "does not parse as Rust" with an **empty reason**, which is a message worth
fixing some day (a parse refusal should name the first error, as the C++ side does).

Verified: the full spire-code suite, including the integration tests that run **real cargo builds** —
`the_wizard_creates_an_embedded_hal_project_and_the_loop_closes`,
`a_scaffolded_backend_builds_after_one_repair_round` (plan → generate → gate → build → repair → build,
for rp2040) and `a_second_wrong_answer_still_gets_a_third_round` (three answers, two repairs).

### Step 4 landed — the application

`embedded_app_scaffold.rs` emits the app's `main.rs` against the new shape: the actor is generic over
`OutputPin`/`DelayNs` (the traits the HAL re-exports), `main` takes its delay from `Board::delay()`,
and the one line that belongs to the board — `board_led()` — is named as `Board::led` rather than
guessed. The app's own placeholder is `UnimplementedLed` in `main.rs`, and it **implements**
`OutputPin` for real (panicking bodies), because the invariant "the project type-checks, links and
flashes before the fill has run" has to survive.

That invariant is what failed first, and the fix was in the *backend*: its stub returned bare
`UnimplementedLed`/`UnimplementedDelay` types that implemented nothing, so a scaffolded app could not
construct the actor (`UnimplementedDelay: DelayNs` not satisfied). The backend's stand-ins now
implement their traits with panicking bodies — unreachable in practice, since the only way to obtain
one is the constructor that panics — which keeps the measure honest (`is_stub` is still true: the
board's own methods are the placeholders) and lets the app build, link and flash beforehand.

Verified by the test that was written for exactly this: `an_app_cross_compiles_against_a_real_hal`
(`--ignored`, live) scaffolds a HAL plus an application and cross-builds the application for the
classic ESP32 with the `esp` toolchain and the real SDK — 114 s, green.

Also tidied: the fill's unit tests had one fixture described as "the scaffold's output" while writing
the *authored-trait* shape. The two are now separate (`project` = a project that authors a trait of
its own, still a supported path and still measured; `scaffolded_project` = what the scaffold emits),
and `a_scaffolded_backend_is_one_pending_plan_item` asserts against the latter — `board`, `Board`,
`led`/`delay`.

### Step 5 landed — one driver, and the policy that decides when to write one

The decision the plan asked to settle is: **upstream crates are the default.** A sensor, a display or
a strip has a crate written against these traits, and it drops onto a `Board` bus without an adapter —
which is the whole reason the traits are re-exported rather than re-invented. That is now stated where
a model reads it: point 3 of the scaffolded contract's `lib.rs`, which the HAL fill prompt injects
verbatim as "the contract itself, not a paraphrase".

Writing a driver is the fallback, for the two cases where the default is not the answer: no crate for
the device, or a driver that has to be *actor-shaped*. `crates/spire-hal-drivers` is the first one —
`Ws2812`, generic over `SpiBus<u8>` and `DelayNs`, with no vendor type, no `#[cfg(board)]` and no chip
name — and it is host-tested against fakes (`cargo test -p spire-hal-drivers`). It is a `default-member`
of the workspace *because* it is host-checkable, which is the point of writing drivers against traits.

The test earned its keep immediately. Three things an eye cannot check on a bench are byte
assertions: three SPI bits per data bit (`100` for a zero, `110` for a one), GRB channel order rather
than the caller's RGB, and the latch *after* the frame. It failed first on a real bug: an unset pixel
left as zero bytes sends 30 µs of low **inside** a frame — outside the protocol — so a new frame is
now encoded black rather than cleared. That is exactly the class of mistake a driver is worth having a
test for, and none of it needs hardware.

### Still to do

1. **The board's buses.** `Board` has `led` and `delay`; a bus driver needs `Board::spi`/`Board::i2c`
   (esp-idf-hal's `SpiDriver`/`I2cDriver` already implement `SpiBus`/`I2c`, so they are constructor
   wrappers like `led`). Until that lands, `Ws2812` is provable on a host fake but cannot be wired to
   the pilot board — the driver is done, the board owes it a bus.
2. Then the second driver, and the app's fill prompt reaching for an upstream crate by name.

The whole spiral — a bespoke trait per board family, a scaffold that generates it, a fill that
implements it, a drift measure that scores it — existed to make a *contract* out of something the
ecosystem had already standardised. Adopting the standard deletes the bottom layer and leaves the two
things that were actually ours: the actor model, and the board.


**Renamed, and re-shaped: the "HAL" an application depends on is a *container*.** The
`EmbeddedHal` structure became `Embedded` (the `spire-embedded` container: the actor framework, the
peripheral drivers, a BSP crate per board), so the argument that names it was renamed with it. The
wire field is **`embeddedRoot`** (was `halRoot`) and the Rust parameter `embedded_root` (was
`hal_root`), on the same three tools (`createProject/GeneratePlan`, `/Plan` and `/Scaffold`). The
refusal an application without it gets now says so: *"an embedded application needs the container it
depends on: pass `embeddedRoot`"* — so a UI still sending `halRoot` fails by name rather than being
scaffolded against nothing.

The same rename dropped the phrase "embedded-HAL project" from the docs: it named a shape that no
longer exists (a contract crate implementing our own traits). What an application path-deps is the
container's library crate, `crates/<project>`.

## Rationalising the platform/board/HAL structure

**The trigger.** The Platforms screen listed every registry entry flat, and reading it made the
underlying mistake visible: the Linux SBC entries (`rpi5`, `rock3c`, `a7s`) are **boards** — the
Pi 5's arch and sysroot are facts about *that board* — while the bare-metal entries (`esp32c3`,
`rp2040`, …) are **processors**, and `esp32c3.yaml` is named `"ESP32-C3 (DevKitM-1)"` because the
board facts had nowhere else to go. Its `library_hints` describe a *DevKitM-1* ("the on-board LED is
on **GPIO8** and is addressable (WS2812-family)"), not the C3 silicon. So one list held two kinds,
and one entry held two concepts.

**The model (settled before any code).** Four things, not one:

| concept | what it is | owns |
|---|---|---|
| **HAL** | the platform abstraction | *never us* — the Linux kernel, or `embedded-hal` + the vendor HAL |
| **Chip** | the silicon (`esp32c3`, `rp2040`, `bcm2712`) | its vendor HAL, its compile target (triple, flash tool) |
| **Board** | the physical thing (`DevKitM-1`, `Pi 5`, `Rock 3C`) | names a chip; its **BSP** (pin facts); optionally a device endpoint |
| **Driver library** | the reusable project an application depends on | device drivers (Ws2812/IMU · camera/NPU/codec); the actor framework bare-metal-side |

Two consequences worth stating, because both correct an earlier assumption:

- **A BSP is not bare-metal-only.** It is *board facts* (GPIO pinout, LED, active-low). Every board
  has them; bare-metal needs them **now** (nothing else knows which pin is the LED), while on Linux
  the kernel already owns the low level, so a BSP there would only name the header mapping — useful,
  not yet needed. One concept, two timelines.
- **The two HAL shapes were always the same idea.** ai-traps' `hal/api` + `hal/implementations/<plat>`
  (camera, NPU, h264) is a *driver library* that has been living **inside** each project; the
  container is a driver library that already lives beside it. Converging means moving the first out,
  not inventing a third thing.

**Stage 1 — name the two kinds, and section the screen (done, `683ef22`).** `Platform::kind()`
returns `PlatformKind::{Board, Chip}`, and `platforms/list` sends it as `"kind"` — **sent**, like
`embedded` already was, because the rule is `os` and a second copy of it in Swift could only ever
disagree with the first. The screen sections by it, boards first. The rule is deliberately the
**inverse of `is_embedded()`** rather than a second predicate: a second taxonomy is free to disagree
with the one the build keys on. It is derived *today*; the split below turns it into a declared
field, and only this method changes when it does.

**Stage 2 — every entry is a board (specified, not yet built).** The registry holds **boards only** —
specific manufacturers' boards, never generic silicon: `m5stack-core3`, `raspberry-pi-pico`,
`raspberry-pi-5`, `rock-3c`, … The chip is a **property** of a board (`chip: esp32s3`), not a thing
in the list, so there is no `esp32c3` entry to pick.

That changes two things stage 1 assumed:

- **`kind` stops being a picker distinction.** With every entry a board, there are no chips to
  contrast with — so stage 1's sections become "Linux boards" / "bare-metal boards" (what genuinely
  differs is the toolchain world) or disappear entirely. `PlatformKind` survives only as long as the
  mixture does.
- ~~**There is no `chips/` store.**~~ **Superseded — see "The state it landed in" below.** There *is*
  a `chips/` store, and what a chip contributes — the vendor HAL, the stock triple, the flash tool,
  the sysroot and toolchain — is an **entry** in it. That is what let `vendor_for`'s chip→crate match
  be *deleted* rather than moved into a second table in Rust. *(What was right in the original: a chip
  is not something a picker offers.)*

Mechanically: a board declares `chip:`; the load path reads **`boards/`** and **`chips/`** — the two
stores the old `platforms/` directory turned out to be a mixture of — and `platform_codec` carries `chip` so the graph
holds the declaration rather than re-deriving it.

The conversion of the nine entries we have:

| today | becomes | carries |
|---|---|---|
| `rpi5`, `rock3c`, `a7s` | boards, as they already are | the SBC's arch + sysroot; its SoC |
| `esp32c3` | a board (M5Stack …) | `chip: esp32c3` + the LED/WS2812 pin facts |
| `esp32s3` | a board (`m5stack-core3`) | `chip: esp32s3` + its own board facts |
| `rp2040` | a board (`raspberry-pi-pico`) | `chip: rp2040` + its own board facts |
| `esp32`, `esp32c6`, `esp32p4` | named M5Stack boards | `chip:` + their own board facts |

**The boards (named 2026-09-19).** The list, and the chip each implies **where the name carries it**:

| board | chip | proposed id | basis |
|---|---|---|---|
| M5Stack Core S3 | `esp32s3` | `m5stack-core-s3` | the name |
| M5Stack Atom S3 Lite | `esp32s3` | `m5stack-atom-s3-lite` | the name |
| Waveshare ESP32-P4-Nano | `esp32p4` | `waveshare-esp32-p4-nano` | the name |
| M5Stack Core Ink | `esp32-pico-d4` | `m5stack-core-ink` | named 2026-09-19 |
| M5Stack Station | `esp32-d0wdq6-v3` | `m5stack-station` | named 2026-09-19 |
| Raspberry Pi Pico | `rp2040` | `raspberry-pi-pico` | named |

(Ids are proposed, in kebab-case. They are longer than the current `esp32c3`/`rpi5`, which were
short because they named silicon; a board id has to distinguish *M5Stack Core S3* from a bare
ESP32-S3, so it cannot be shorter than the board's name.)

**The two classics are distinct chips sharing a family, which is what the lookup must model.**
`esp32-pico-d4` and `esp32-d0wdq6-v3` are both ESP32 *classic* silicon: they share the family's build
facts (the `xtensa-esp32-espidf` triple, its toolchain) while being separate chips. So the chip facts
are keyed **per chip**, with a family grouping for what is genuinely shared — not one `esp32` entry
standing in for both, which is what the registry has today.

One thing is still open:

- **`esp32c3` and `esp32c6` have no board.** Either a board names each, or those entries go. They must
  not survive as entries, though: a chip entry kept as a *board* would reintroduce the generic chip
  name this stage exists to remove.

Every other entry now has a board — the three Linux SBCs, the two ESP32-S3 boards, the P4-Nano, the
Pico, and the two classics — so the store conversion can proceed for all of them, with c3/c6 the only
entries still unresolved.

**Stage 3 — HAL → a referenced driver library.** Extract ai-traps' `hal/` so `Hal` and `Embedded`
are the same concept: a project of drivers an application depends on, with a board's BSP as that
library's per-board piece. **Stage 4 — names last**: rename `Platform`/`Hal`/`Embedded` in the type
system only after 1–3 hold, since the names are the least of it.

## The state it landed in (2026-09-20)

Stages 1 and 2 above are done, and the shape is the one that spec was reaching for. Recorded here
because two entries it supersedes still sit above, and a reader should meet this first.

- **Two stores, not one.** `~/.spire/<app>/boards/` holds **boards** — 11: the six bare-metal boards,
  Espressif's `esp32-c3-devkitm-1`, and the four Linux boards (`a7s`, `a7z`, `rock3c`, `rpi5`).
  `~/.spire/<app>/chips/` holds **chips** — 8: `esp32`, `esp32c3`, `esp32p4`, `esp32s3`, `rp2040`, and
  the Linux SoCs `allwinner-a733`, `rockchip-rk3566`, `broadcom-bcm2712`. The old `platforms/`
  directory is gone; `esp32c6` was retired.
- **Every board declares `chip:`**, and no board states a triple, sysroot or toolchain. `a7s` and
  `a7z` are the case that proves the model: one SoC, two boards, differing only in I/O.
- **`kind()` is one test** — *a board declares the chip it carries; a chip is what is left.* The
  `os`/`is_embedded` rule it replaced is the one the spec above predicted would go, and it went for
  the reason predicted: a Linux SoC entry is a chip and is **not** bare-metal, which finally retired
  the `is_embedded` clause. `embedded` survives as the separate axis it always was ("can a firmware
  project target this").
- **`Platform::build_facts()`** resolves a board to its chip's architecture, toolchain, sysroot and
  Rust target while keeping the board's identity — so the link is real at compile time, not only in
  the data. `resolve()` returns the resolved entry; the listing stays raw, because "what is there"
  and "what do I compile this with" are different questions.
- **`vendor_for` is deleted.** The pilot's vendor HAL — esp-hal 1.2 + `unstable`, and *why*
  `unstable` — is a `hal:` block in `chips/esp32c3.yaml`, and `add_bsp` reads it from there.
- **The screen shows the link.** `chip` is in the Swift model, a board's row carries it, and the
  detail panel reads the chip's facts out of the list it already holds rather than showing a board
  three empty groups.
- **Where the measured hints are, honestly.** Still on the chips — except the one board-specific fact
  ever measured, the DevKitM-1's addressable LED on GPIO8, which now lives on the
  `esp32-c3-devkitm-1` board. Splitting the rest (SDK/constraint notes → chip, I/O notes → board) is
  unfinished. `a7z` carries no hints because its I/O has not been measured: empty is honest there,
  a7s's would not be.

Stages 3 and 4 are untouched by this and stand as written above. Two smaller notes worth keeping: a
hint-length compared across Rust and Python will differ, because Rust counts UTF-8 bytes and Python
counts characters (22 bytes of em dashes and ellipses in `esp32c3`) and serde_yaml clips a trailing
newline that Python's parser keeps. And the app's test binary scopes its own `~/.spire/<app>` — the
app name comes from the process — so an ignored test run without `SPIRE_BOARD_DIR` / `SPIRE_CHIP_DIR`
reads an *empty* scope rather than the real registry.

## The capability model (2026-09-20) — design settled, step 2 in progress

The next thing after the board/chip split, and the reason for that split: **the generator defaults
to software because it does not know hardware exists.** Software is the only *safe* answer when the
target is unknown — it always compiles, it has no wrong pins — so the model writes a software
inference path for a board that has an accelerator, and nothing flags it. Everything below exists to
make the hardware **known and authoritative**, both before generation and after it.

### The shape

- **YAML is the seed; the graph is the source of truth.** Facts are authored in `boards/` and
  `chips/` because that is easy to create and diff, and seeded into the graph because a graph can
  hold *relations* — which a per-entry file cannot. Resolution reads the graph; `resolve()` already
  prefers it, and the startup phase already seeds it.
- **A capability is not a boolean.** Presence carries its details, and **absence is omission** —
  which is what keeps "has an NPU" and "has no NPU" from being the same silence.
- **Two layers of fact, one vocabulary.** A **chip** declares what its silicon *can* do
  (`capabilities:`); a **board** declares what it *realizes*, and how (`realized:`, `pins:`). The
  vocabulary (`schema/capabilities.yaml`) is shared, so `media.video.encode.h264` means the same
  thing on a bare-metal board and on a Linux SBC — only the glue differs.
- **A board is not one chip.** `companions:` lists silicon a board carries *beside* its host: the
  Stamp-P4 has no radio and gets WiFi/BT/ZigBee/Thread from an on-board ESP32-C6. What *can* be
  attached is a board fact; what *is* attached is a configuration, i.e. a graph edge.
- **Strict on shape, open on vocabulary.** A misshapen capability is refused; a new name under a
  category is legal, so adding a capability is one line in the vocabulary and never a code change.
- **The schema carries facts; the drivers carry shape.** The generator gets the resolved profile
  plus the trait, never a wall of prose.
- **Pin granularity:** author *function → pin* (`led: GPIO48`), name *function → interface*
  (`camera: { via: esp32p4, connector: csi0 }`) and let the chip's mux derive the rest. The full
  pinout is the chip's devicetree and stays out.

### The plan

1. **Vocabulary** — done (`a7b3b57`): `schema/capabilities.yaml`, 7 categories, 20 names. The
   validator (strict shape / open vocabulary) is still to write.
2. **Schema + plumbing** — in progress; see below.
3. **Seed the facts** — chip capabilities, board realization/pins/companions; `esp32c6` returns as
   companion silicon. Gated on reading the physical boards, and it must not block step 2.
4. **The resolved profile** — board → chip → capabilities → companions → wiring, as a graph walk.
5. **Ground the codegen** — the profile into the fill prompt.
6. **The verification oracle** — flag a software reimplementation where hardware is declared.
   Inference first: the software path (`tflite`, `ndarray`, hand-rolled loops) is easy to recognise.
7. **Deterministic fills** — `pins:` → the BSP's `led()` and the Linux impl's line, by template.
8. **Rename** — HAL → drivers for our side; the vendor HAL stays `hal:` on the chip. Last.

### Step 2, exactly — and its two traps

The four fields (`capabilities`, `realized`, `pins`, `companions` — optional generic trees, because
the vocabulary is open and a new capability must not be a code change) break **twelve**
`Platform { … }` literals: `platform.rs` ×6 · `platform_codec.rs` 159, 239, 268 ·
`coordinator.rs:5688` (the `platform()` test helper, whose callers then follow) · `esp.rs:809` ·
`rp2040.rs:551`.

- **Trap one — the codec reader.** `platform_codec.rs:159` sits inside `platform_json_to_spire`
  (line 132), which builds a `Platform` **from graph props**. It must *read* the four props, and
  `platform_to_registry_json` (line 16; props at 105/110 to model on) must *write* them. Putting
  `None` there compiles and **silently drops capabilities on every graph round-trip** — so the
  round-trip test is what defines "done" for this step, not the compile.
- **Trap two — do it by hand.** A regex/brace-matching script was attempted three times and is the
  wrong tool: it cannot determine a struct literal's extent, and it produced duplicates (`E0062`)
  and misses (`E0063`). Twelve hand edits, or an AST pass. Not a script.

Then: capability nodes and `realizes` / `via` / `carries` edges seeded beside the platform nodes —
**no `spire-core` change needed**, because `AttrNode.node_type` is a free-form string and
`RelationshipType` already carries `Custom(String)`.

### Correction (same session): step 2 needs no `Platform` fields at all

Three attempts to add `capabilities` / `realized` / `pins` / `companions` to `Platform` failed on the
same rock: four new fields break twelve `Platform { … }` literals, and the change is atomic, so it is
all-or-nothing across eleven fixtures. The attempts above are recorded as a lesson about scripts. They
are better read as **evidence that the fields were the wrong shape.**

They were. Those fields came from the **blob-prop** design — option (a), data living *on the platform
node* — and this file already chose **(b): capabilities are nodes and edges.** Under (b) nothing needs
the blocks on `Platform`:

- the resolved profile (step 4) is a graph **walk** — board → realizes → capability → via → chip;
- the oracle (step 6) reads the same walk, so it cannot disagree with what the generator was told;
- the codegen prompt (step 5) is built from that walk.

So the fields were only ever needed to *carry* the data from YAML into the graph through the typed
`Platform` — and that trip is unnecessary. **The bootstrap can seed capability nodes straight from the
YAML**, which is how the platform nodes are already seeded.

**Step 2, corrected:**

1. **Read the blocks raw**, the way `generic_helpers` already reads `library_hints` out of a YAML
   document rather than through the typed `Platform`. That is a precedent in this codebase, not a new
   trick.
2. **Seed them as nodes and edges** beside the platform nodes: `(:capability {name:
   "media.video.encode.h264", …})`, `(board)-[:realizes]->(capability)`,
   `(capability)-[:via]->(chip)`, `(board)-[:carries]->(chip)`.
3. **Test it end to end** — a chip YAML with `capabilities:` seeds capability nodes; a board's
   `realized:` creates the edge to them; and nothing is read from `Platform`.

No `Platform` change, no codec change, no fixtures, and no twelve edits. The lesson generalises:
**when the source of truth is the graph, do not launder its data through a typed struct that exists
only to be serialised.**


### Two constraints found before the seeder gets written (2026-09-20)

Both would have been wrong assumptions in the seeder, and both are cheap now.

1. **`spire-core` cannot call `capability_paths` — or anything else in `spire-code`.** The dependency
   runs one way: `spire-code` depends on `spire-core`, so the seeder (in spire-core's graph subsystem)
   can only ever be a **dumb writer**. The tree must therefore be **flattened on this side of the
   message**, in spire-code, and the payload should carry the result — the capability paths, and the
   edges a realization implies — rather than the raw tree. That also puts the naming rule
   (`capability_paths`) and its only consumer in the same crate, which is its right home.

2. **`via:` and `firmware:` are not the only things a realization carries, and the vocabulary was
   wrong to say so.** `schema/capabilities.yaml` states "every capability may carry two realization
   keys, and nothing else", while the worked example gives `camera: { via: esp32p4, connector: csi0 }`.
   The example is right and the rule was too strict: a real board says *which* connector, *which* bus,
   *which* pins.

   Resolved by keeping the two apart cleanly: **`via:` and `firmware:` say what *realizes* a
   capability; `pins:` says how it is *wired*.** So the connector belongs under `pins:` beside the
   LED's pin, not inside the capability block — the vocabulary's rule stands as written, and the
   example in the notes above is the thing that needed correcting.

### The seeder, specified (2026-09-20)

Step 2's last piece, and the only cross-repo one. `spire-code` now hands over the payload; the writer
lives in `spire-core` and must stay **dumb**, because the dependency runs one way.

**What arrives** — per platform node, under `capability_blocks`, from
`capability_vocabulary::seeder_input()`:

```yaml
capabilities: ["media.camera", "media.video.encode"]     # node names
realizes:     [ { capability: "media.camera", properties: { via: "esp32p4" } } ]
carries:      [ { chip: "esp32c6", properties: { role: "radio", firmware: "esp-hosted" } } ]
pins:         { led: { pin: "GPIO48" } }                 # wiring, not an edge - ignored here
```

A key is absent when it is empty, and the whole block is `null` when the entry declares nothing -
which is most entries today. So the writer must treat *absent* as the common case, not an error.

**What the writer does**, beside the `BootstrapPlatforms` handler in
`spire-core/src/subsystems/graph/memory_graph.rs`:

1. **Delete first, like the platform nodes do** - `MATCH (n:SpireNode) WHERE n.node_type =
   'capability' DETACH DELETE n` - so the graph mirrors the registry on every startup instead of
   accumulating. Without it an edited board leaves its old capabilities behind, and the oracle would
   check generated code against a graph that no longer matches the board.
2. **One node per name** - `node_type: 'capability'`, `name` = the path (`media.video.encode`), with
   the entry's properties when it has them.
3. **The edges** - `(board)-[:realizes {properties}]->(capability)`,
   `(capability)-[:via]->(chip)` where a realization names one, `(board)-[:carries {properties}]->(chip)`.
   `RelationshipType::Custom` already allows these names, so no enum change is needed.

**The test that defines done:** a fixture payload carrying the block above seeds two capability nodes
and the edges; a *second* bootstrap with the block removed leaves none. The second half is the one
that catches a missing delete, and it is the half that would otherwise go unwritten.

**What the handler already gives the seeder** (read from it, not assumed): the node write is an
`AttrNode { id, node_type, name, description, properties, .. }` stored with
`actor.store_attr_node_via_gql(&attr, None)`, so a capability node is an `AttrNode` with
`node_type: "Capability"`, `name`/`id` = the path, and the block's properties; the delete is
`execute_gql_write("MATCH (n:SpireNode) WHERE n.node_type = '..' DETACH DELETE n")`, and node types
are plain strings, so none of this needs an enum change; and `schedule_snapshot()` runs once after
the loop. **Edges are not created in this handler**, so the `realizes`/`via`/`carries` writes need
the edge API read from wherever the `ast_*` relationships are written - that is the one unknown left.

**And a bug found while reading it.** The platform delete matches `node_type = 'platform'` while the
nodes are created with `node_type: "Platform"`. Cypher compares *values*, and a property value is
case-sensitive - so that delete matches **nothing**, and the "graph mirrors the registry exactly on
every startup" it exists for is not happening: a platform removed from the registry stays in the
graph. The seeder's own delete must match the case it writes, and the existing one should be fixed
in the same commit, because it is precisely the failure the spec above warns about - a stale truth,
which is worse than no truth.

**The edge API - the last unknown, resolved.** `store_edge_via_gql(..)` writes an edge and
`delete_edge_via_gql(uuid)` removes one; the GQL label comes from
`relationship_type_to_gql_label(&RelationshipType)`, and `RelationshipType` carries `Custom(String)`,
so `realizes` / `via` / `carries` need **no enum change** - they are `Custom`, exactly as the model
check predicted before any of this was written. So the seeder is entirely: `store_attr_node_via_gql`
for one node per capability path, `store_edge_via_gql` for the edges, labels from `Custom(..)`.

**And a gap to expect before starting.** `memory_graph.rs` has **no handler tests** - no `#[test]`
in that file at all. So the seeder's test needs a graph fixture that does not exist yet, and building
that harness is a bigger piece than the seeder itself. The first task is therefore the harness: bring
up a `MemoryGraph` over a temp store and read a node back. The seeder test, and the delete-first
assertion that makes it meaningful, follow from it.
