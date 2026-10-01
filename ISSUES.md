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

**Correction to the gap above: the harness mostly exists.** `create_test_graph() -> GraphDb` is
already a test helper in `spire-core/src/graph.rs` (around line 670), and
`tests/spatial_query_tests.rs` and `tests/tile_actor_tests.rs` are integration tests that bring an
actor up over a test store. So the pattern for "a `MemoryGraph` over a temp store" is written twice
already, and the seeder's test is a copy of an existing shape rather than new infrastructure. The one
thing to check first is whether `create_test_graph()` is visible outside `src/graph.rs`'s test module
(it looks module-scoped), which decides between reusing it and lifting it into a shared test helper -
a small decision, not a piece of work. Lesson repeated: the gap I named was found by grep, not by
reading, and grepping one file was one file too few.

**The last signatures, so the seeder is a transcription.** `store_edge_via_gql(&self, from_uuid,
predicate, to_uuid, properties: &[(&str, &str)])` - and `predicate` is a **free string**, so
`realizes` / `via` / `carries` go straight in with no `RelationshipType` construction at all. Node
ids **are** the graph uuids (the platform node's id is deliberately the registry id, for exactly that
reason), so the edges join on ids the seeder already holds. And a test graph is one line:
`GraphDb::new_in_memory()` - which is all `create_test_graph()` in `src/graph.rs` is, module-scoped,
so a new test writes the same line rather than needing it lifted. The one remaining shape to copy is
bringing the `MemoryGraphActor` up over that store, which `tests/spatial_query_tests.rs` already
demonstrates.

So the seeder is: delete `node_type = 'Capability'`; per payload carrying `capability_blocks`, one
`AttrNode { node_type: "Capability", id: path, name: path, .. }` through `store_attr_node_via_gql` per
name, then `store_edge_via_gql(board_id, "realizes", path, &[("via", ..)])` and
`store_edge_via_gql(board_id, "carries", chip_id, ..)` from the block's edge lists - with the case of
the delete matching the case of the create, which the platform delete currently does not.

**And the last one, so nothing is left to look up.** `GraphDb` has both calls a free function needs:
`create_node(labels, props) -> id` (line 129) and **`create_edge(label, from, to, props)`** (line 209,
used in its own tests as `db.create_edge("knows", a, b, vec![])`). So:

    fn seed_capabilities(graph_db: &GraphDb, platforms: &[serde_json::Value]) -> Result<()>

- deletes `node_type = 'Capability'` through `execute_gql_write`;
- creates one node per path, `labels: ["SpireNode"]`, with `node_type: "Capability"` and
  `name: <path>` - the label and props the delete's own GQL implies (`MATCH (n:SpireNode) WHERE
  n.node_type = ...`);
- `create_edge("realizes", board_id, path, ..)` and `create_edge("carries", board_id, chip_id, ..)`
  from the block's edge lists.

The handler calls it with its `graph_db`; the test calls it with `GraphDb::new_in_memory()` and reads
nodes and edges back. **No actor, no message loop, no harness to copy** - the blocker I named two
entries ago was an artefact of keeping the rule inline in the handler. The rule belongs in a function
and the handler belongs to the plumbing, which is how every other piece of this work landed.

**Correction: the seeder DOES need the actor's store, and the previous entry was wrong.** Reading
`GraphDb::create_node(labels, props) -> NodeId` and `create_edge(label, subject, object, props)` showed
both exist, and I concluded the seeder could be a free function over `&GraphDb` with no actor at all.
That skips something plainly visible in the handler: the platform nodes are written through
`actor.store_attr_node_via_gql`, which maps an `AttrNode` - labels, `node_type`, timestamps, version -
onto the graph, while `create_node` takes a raw `Vec<(String, selene Value)>`. Writing capability nodes
with `create_node` directly would either duplicate that mapping or, worse, produce nodes shaped
differently from the platform nodes they sit beside. So: the seeder uses the actor's own paths
(`store_attr_node_via_gql`, `store_edge_via_gql`), and the test **does** need the actor up - which is
what `tests/spatial_query_tests.rs` already demonstrates. The free-function idea was right about
extracting the rule and wrong about the harness.

## The seeder, verified (2026-09-21) — and it had been writing nothing

Phase 1 done: `spire-core` now has the test its spec named (`dc77d7c`), and writing it earned its
place — **the seeder had never written a single edge**, through three independent failures, none of
which any existing test could see:

1. **Invalid GQL for empty properties.** `gql_props(&[])` returns the literal `{}`, so
   `store_edge_via_gql` emitted `INSERT (a)-[e:realizes {}]->(b)`, which SeleneDB's parser rejects
   (`parse failed: expected prop_ident`). That aborted the **whole bootstrap on the first edge**.
   The one long-standing caller always passes properties, so the empty case was never exercised.
2. **No dedup by path.** A path declared by a chip *and* by the board built on it (`media.display`
   on the P4 and on the P4-Nano) wrote **two nodes** — the store does not dedup by id — leaving
   every edge to that capability ambiguous. Now collected into a `BTreeSet` before writing.
3. **No edge metadata.** `parse_edge_from_row` treats `uuid` and `edge_type` as mandatory (`?` on
   each) while the seeder passed `&[]`, so edges were stored and then **silently dropped by every
   read**. They now carry what the working caller carries (`store_capability_edge_via_gql`), and
   `RelationshipType` gained `Realizes` / `Via` / `Carries`, so the predicates read back **named**
   rather than as `Unknown` — an edge findable only by shape is not one you can query.

This is the lesson this file had already reached two entries above and not yet acted on: a writer
that never reads back is green while being wrong. The second half of the test is the part that
matters — re-bootstrap with the blocks removed must leave nothing behind.

**Consequence for the running app: restart it.** Its graph holds platform and capability nodes and
no capability edges, so every read would answer "nothing provides this".

Two of the test's own assertions were wrong and were corrected, not worked around: a `via` naming
the board's *chip* is a real edge (the chip is a different node from the board), and the tautology
the seeder skips is `via == board_id`, not "any `via` that happens to name a chip".

**Vocabulary grew**: `compute.cpu { cores, clock }` and `compute.gpu { model }` (`63cae20`) — the
three Linux SoCs had no way to state a CPU at all, while every other fact they need (`io.*`,
`storage.*`, `media.video`, `power.*`, `sensing.*`) was already covered. No loader change was
required: the vocabulary is embedded with `include_str!`, so the file *is* the source of truth —
which is what its header always claimed, now confirmed by reading the loader instead of trusting it.
Worth knowing the consequence: editing that file recompiles all of spire-code.

## The migration to C++/ESP-IDF — the data, and the build module (2026-09-21)

**The decision, and why it is not a language argument.** The Rust embedded side is retired: the C++
APIs the product actually needs (NPU, video codec, camera/ISP, the ESP-IDF media stack) have no Rust
equivalents and are not going to get them, `ai-traps` is C++ and stays C++, and for the controller
boards the C++ world has what Rust does not — a first-party BSP layer (`esp-bsp`, and M5Stack's own
`M5Unified`/`M5GFX`, which cover their whole line; `core-s3` was the lone Rust exception). Rust was
one language for one SDK; this is one language for both, at the cost of a second build system
(`idf.py` beside Meson).

**Phase 1 — the data.** Every ESP32 entry is `os: esp-idf` now, and it means *C++* ESP-IDF rather
than Rust over it. The Rust facts went with the Rust: `hal:` (the vendor HAL crate) and `rust:` (the
rustup target, the vendor's idf spelling, the flasher) are gone from every chip, and the BSP is a
name as its SDK writes it — `bsp: m5stack_core_s3` — so `PlatformBsp` collapsed from a struct into
`Option<String>`. The chip's `IDF_TARGET` is `architecture.cpu`, which a *resolved* board already
carries through `build_facts()`, so a board and its chip name the same target with nothing typed
twice. `rp2040` and `raspberry-pi-pico` are deleted with the decision to be ESP32-only: in Rust the
Pico was nearly free (shared `embedded-hal` and cargo), and in C++ it would have been an entire
second SDK for a board that fills a role the ESP32 family already covers with a radio on die.

**Phase 2 — the build module.** `build/esp.rs` and `build/rp2040.rs` are deleted; `build/idf.rs`
replaces them. The old esp module was ~1900 lines of `esp-idf-sys` knowledge — `MCU`, `LIBCLANG_PATH`,
`-Zbuild-std`, the custom `esp` toolchain, and a glob into `build/esp-idf-sys-*/out/` for the
bootloader and partition table — all of which `idf.py` owns itself. What is left is the invocation:
`idf.py build` with `IDF_TARGET` from the chip, and `idf.py -p <port> flash` over USB. No MCP and no
network leg, which is the only honest answer for a board that may be running nothing at all.

The subtlety worth keeping: **no `PATH` surgery and no SDK variables.** The ESP-IDF environment is
what `export.sh` sets, and the platform model already says Spire *references* an install this machine
has rather than creating one — so the module contributes the target and nothing else, and a build
that fails because the environment was never exported fails with IDF's own message, which names it.

*(Superseded in part, 2026-10-01: this holds — and is still the first choice — whenever the process has
an environment of its own, and `install_for_build` supplies one when it has none. See §6f of
`docs/esp-idf-architecture.md` and the later `idf_env_check` entry in this file.)*

Registered `os: "esp-idf"` and **never** by config file, for the reason the old module stated and
this one inherits: an ESP-IDF project is *also* a CMake project, so claiming `CMakeLists.txt` would
replace the cmake module and capture every CMake project in Spire.

**Retired with it**: `embedded_creation_tests.rs` (1252 lines) tested the Rust embedded path
end-to-end — the wizard's HAL-project loop, the Rust contract crate, the rp2040/esp backends — and
with rp2040 gone and the Rust modules deleted, four of its ten tests failed for the right reason. Its
subject is what Phase 3 removes, so it went with the feature rather than being propped up.

**Verified**: `cargo test -p spire-code` green (330 lib + every integration suite), the live
`platforms/config` returns 18 entries with `esp32s3`/`esp32c3` on `esp-idf` and
`bsp: m5stack_core_s3` on the CoreS3, and the app rebuilds and reseeds at 18.

**Phase 3 — the container.** `build/container_scaffold.rs` emits an ESP-IDF project from the
templates vendored under `crates/spire-code/templates/esp-container/`: the root `CMakeLists.txt`,
`sdkconfig.defaults`, `main/{CMakeLists.txt,main.cpp}`, and three components — `ramen` (the vendored
`ramen.hpp`), `toolkit` (the `Container` plus the FreeRTOS `Task` an actor's loop runs on), and
`spire_hal` (the contract side). The framework is fixed and structural; the growth surfaces
(`main/main.cpp`'s wiring, `hal/types.hpp`) are empty **and say so** rather than carrying a
placeholder actor that the first real one would have to delete.

Three things were measured rather than assumed:

1. **`ramen.hpp` is ours, not a third-party drop.** The header's own copyright is Nature Sense's,
   Apache-2.0, and it documents its port algebra (Pushable/Pusher/Pullable/Puller, `Latch`, `Lift`)
   in full. It is vendored verbatim, the way the Rust container vendors its framework — the point
   of one header is that there is one of it. (An earlier note here called it Zubax's; the file says
   otherwise.)
2. **`components/hal/` does not work.** ESP-IDF ships a component named `hal`, and a project-level
   component of the same name *replaces* it — after which `esp-idf/mbedtls` cannot find
   `hal/sha_types.h` and the build dies naming neither this directory nor the collision. The
   component is `spire_hal`; the include path `hal/…` and the namespace `hal::` are unchanged,
   because only component *names* collide. Found by building it.
3. **There is no central event loop to pump.** Ramen's push model is synchronous — a producer calls
   its `Pusher` and the wired behaviours run inline on the producer's stack — so the "pump" the plan
   called for is each producing actor's own loop. `Task` therefore provides what an MCU loop needs
   from the platform and `std::thread` cannot say: a stack size, a priority and a core.

**Reachability, which is where this could have quietly failed.** `BuildManager` routes `Build`,
`BuildStreaming` and `Flash` by **platform** (`opts.platform`), but `scaffold_build_config` routes by
**config file** — so `IdfBuildModule` claims `sdkconfig.defaults`: the one file in an ESP-IDF tree
that is ESP-IDF's alone, since claiming `CMakeLists.txt` would capture every CMake project in Spire.
Analysis is unaffected and correctly still the cmake module's, because `analyze_project` routes with
no platform at all.

Claiming the file was **not enough**, and the gap was found by reading `add_platform_module` rather
than by testing: it registered the capability in the *platform* router only, so a module's
`config_files` never reached the config router at all. A module could therefore register, describe
itself, answer builds and flashes — and be unreachable for the one operation that creates a project
of its kind. `add_platform_module` now registers the claimed files too, and *keeps the existing
owner* when a file already has one, since whoever owns a file is the module that analyses it (in
`ffi.rs` this module is registered before `node` and `cmake`, so order alone would have let it
shadow either). `tests/container_scaffold_routing_tests.rs` holds the pair: the request reaches the
module that claims the file, and with no module claiming it the refusal names the file.


**Verified by building it, not by reading it.** ESP-IDF 5.5.5 with the xtensa/riscv toolchains is
installed here, so the scaffold was emitted (`dump_container_scaffold`, an `--ignored` test, so the
tree built is the *emitter's* output and not the templates') and built for `esp32s3`:
`pm25-meter.elf` (3.8 MB) and `pm25-meter.bin` (191 KB), with `pm25_meter::` present in the ELF —
the rename reached the namespace and the image. `cargo test -p spire-code` is green at 340 lib tests.

**Worth knowing for the next person:** `export.sh` on this machine *fails*, because the debug tools
(`xtensa-esp-elf-gdb`, `riscv32-esp-elf-gdb`, `openocd-esp32`) are not installed and activation
aborts on them — none of which a build or a USB flash needs. The build above ran with a hand-built
environment (`IDF_PATH`, the toolchain/CMake/ninja bin dirs, `ESP_ROM_ELF_DIR`). Until those tools
are installed (`idf_tools.py install`), `idf.py` is not on `PATH`, and the Phase-2 plan that names
`idf.py` will not find it. **Superseded** by `idf_env_check` / `idf_env_fix` (see the entry
"The ESP-IDF environment is the app's to test and fix"): the diagnosis and the install are the app's
now, and running them here fixed this machine — including the `openocd-esp32` half that
`idf_tools.py check` alone reports as present.

**Phase 4 — the first vertical slice.** `build/container_device.rs` installs the SPS30 particulate
slice into a container, exposed as the tool `container_add_pm_sensor`. Five files plus a sink: the
contract `hal::PmSensor` and its value type `PmReading` (in `spire_hal`, beside the contract that
produces it rather than in the shared `types.hpp`), the board's implementation `hal::Sps30I2c` (in
`implementations/m5stack_core_s3/`, because the pins are that board's facts), the actor
`Sps30Actor` (a push port, a `Task` loop, the `init`/`shutdown` pair) and `ReadingLoggerActor` (a
consumer with no task at all — Ramen runs it inline on the producer's stack).

**It edits files, not only writes them**, which is the part that makes it an operation rather than a
file drop: a new `.cpp` is not compiled until a `CMakeLists.txt` names it, and an actor nothing
links is a struct that never runs. So it rewrites `idf_component_register()` — *creating* `SRCS` for
a component that was header-only — and the two **markers** the scaffold writes into `main.cpp`
(`// spire:include`, `// spire:wire`), refusing when a marker is gone rather than appending the
wiring at the end of the file. Wiring after `container.start()` would drop every reading pushed
before the link, which is a bug that looks exactly like a sensor that never reports.

**Two things the build caught that reading could not:**

1. **RTTI cannot be off.** ESP-IDF disables it by default, and `ramen.hpp`'s `send()` — the
   name-based dispatch — goes through `typeid`, so `-fno-rtti` does not compile the header at all
   (`error: cannot use 'typeid' with '-fno-rtti'`). `sdkconfig.defaults` now sets
   `CONFIG_COMPILER_CXX_RTTI=y`, with the error text in the comment so the next reader does not
   turn it back off. `ai-traps` never hit this because it builds under Meson on Linux, where RTTI is
   on — which is what a second toolchain is for. Exceptions stay off; nothing needed them.
2. **The sink was not renamed.** `add_pm_sensor` applied the project namespace to `pm_sensor_files()`
   and then wrote the logger verbatim, so `main.cpp` called `pm25_meter::ReadingLoggerActor` against
   a header that still said `spire_container::`. The unit test had the same blind spot — it checked
   `pm_sensor_files()` and not the file written beside them — so the assertion now covers both.

**Verified end to end.** The container was generated *through spire-code's own code*
(`dump_pm25_container`, an `--ignored` test that scaffolds and installs), then built for `esp32s3`:
`pm25-meter.elf` (4.7 MB) and `pm25-meter.bin` (227 KB), with `pm25_meter::Sps30Actor::{init, loop,
poll, shutdown}` in the symbol table and `"no SPS30 answered at 0x69 on Grove A"` in the image.
`cargo test -p spire-code` green at 346 lib tests.

## The embedded rethink — two project types, and tooling rather than contents (2026-09-22)

**The correction.** Everything above was still modelling the Rust world: a container plus
applications plus HAL contracts plus BSPs, with a `PmSensor` interface and a `PowerSwitchable` and a
driver-versus-actor split. What broke it was the simplest question — *the SPS30 is I²C and the I²C
API is ESP-IDF's, so why is there a HAL?* — and the answer is that there should not be one. On
ESP-IDF the HAL **is** IDF, a BSP is a component like any other, and a container is just a library.

So the embedded side is **two project types**, and only two:

- **`ProjectStructure::IdfLibrary`** — a project whose product is `components/*`: protocol drivers,
  board support packages, the framework they share. No actors and no application entry point; `main/`
  is a **build harness**, present so `idf.py build` compile-checks the components.
- **`ProjectStructure::IdfApplication`** — `main/`, the actors, the wiring, and the board facts
  (pins, bus, addresses), built against one or more libraries.

**What was deleted.** `build/container_scaffold.rs`, `build/container_device.rs`, and the vendored
SPS30 slice (`templates/esp-container-devices/` — the whole protocol and both actors) are gone. That
last part is the correction that matters: **the SPS30 driver and its actors were container
*contents*, which is the model's to write, not the tooling's.** A scaffold that ships a finished
driver is a scaffold that only ever worked for that driver.

`build/idf_projects.rs` replaces them with the two skeletons and one generic operation —
`idf_add_component(root, name, bus)`, the C++ counterpart of the Rust `embedded_add_driver`. It emits
`components/<name>/{CMakeLists.txt, include/<name>.hpp, src/<name>.cpp}` as a **typed stub**: the bus
decides the handle type and the IDF component that provides it, and the protocol is a `TODO` in the
owner's words. The bus is an input rather than a guess, for the reason the Rust version takes one — a
driver written for I²C when the part is on SPI never compiles.

**One thing deliberately not configurable: the namespace.** The framework is `namespace spire`, fixed.
The retired scaffold renamed the project's name into the namespace, so the two halves of a project had
to agree about a rename — and where they did not, `main.cpp` called `pm25_meter::ReadingLoggerActor`
against a header still saying `spire_container::`, which only a real build caught. Now only
`project()` and the log tag carry a project's own name.

**Verified by building both types.** `dump_idf_projects` emits a library (`sensors`, 9 files) and an
application (`pm25-meter`, 6 files) that names it as a sibling through `EXTRA_COMPONENT_DIRS`, then
installs an `sps30` component stub into the library. Both built for `esp32s3`:

- `sensors.bin` (190 KB), with `__idf_sps30.dir/src/sps30.cpp.obj` proving the **generated stub**
  compiles — and `ramen`, `toolkit` and `sps30` all visible in the component list;
- `pm25-meter.elf` (3.8 MB), with `spire::Container::{start, stop}` linked in and the build log
  showing it took `ramen`, `toolkit` and `sps30` out of the library's `components/`.

`cargo test -p spire-code` is green at 340 lib tests plus three new routing tests that run the
scaffolds through `BuildManager` rather than calling them directly.

## The generic ESP32 tool, a self-describing library, and `spire-idf` (2026-09-22)

**One more step back, and it is the right one.** The two project types above were still carrying an
architecture: `library_scaffold` shipped RAMEN and the toolkit baked in, and `application_scaffold`
shipped a `Container` and a wiring seam. That is the tool *mandating* an architecture — the thing the
whole migration has been trying not to do. What replaced it:

- **A component library starts empty.** `components/*`, and none of them. The tool has no opinion
  about whether what arrives is a protocol driver, a board's support package, a framework, or all
  three.
- **An application is a blank `app_main`.** No actor owner, no wiring, no lifecycle. It still has a
  `main/` component because ESP-IDF requires one to build at all — "empty" means *assumes nothing*,
  not *has no files*.
- **The architecture is a library, and a library describes itself.** `SPIRE.md` is emitted at the
  root of every library: what it provides, how it is meant to be used, what it deliberately leaves to
  the application. That file is the contract with the model, and it is the only place an architecture
  is written down — the tool has no vocabulary for actors, tasks or containers at all.
- **RAMEN moved out of the product path** into `tests/fixtures/ramen-framework/`: ramen, the toolkit's
  `Task`, the `Container` and the example `main.cpp`, kept as a *fixture*, never shipped as a seed.
  The RTTI requirement went with them — a framework's build needs belong in that framework's hints,
  not in the `sdkconfig.defaults` every project starts from.

**The gap it exposed, and the duplicate behind it.** Driving `createProject/Scaffold` over RPC with
`{ structure: "idf_library" }` returned a **Meson** project with `embedded: false` — my structure
branch had gone into `scaffold_spec_in_memory` and `ScaffoldProject` has its *own* copy of the
language→build-file mapping, so only the Plan path was fixed. Both now call one
`scaffold_routing()`, which resolves the config file, the library, the build-system label and the
embedded flag together, plus a `build_system_for()` that lets the **structure** decide where the
language cannot (an ESP-IDF library and an ESP-IDF application are both "ESP-IDF", neither is "C++").
That is the codebase's own recurring lesson — a decision point copied to nine call sites is a
decision point that will be applied to eight of them — and this was the ninth.

**Verified end to end, interactively.** Through `tools/spire_rpc.py` against the built dylib:

```
createProject/Scaffold {"projectName":"spire-idf","rootDir":"/tmp/spire-ws",
                        "structure":"idf_library","language":"cpp"}

→ {"build_system":"ESP-IDF","embedded":true,"structure":"idf_library",
   "files":[CMakeLists.txt, sdkconfig.defaults, main/CMakeLists.txt,
            main/build_harness.cpp, SPIRE.md, README.md]}
```

The six files landed on disk, spire-code initialised a git repo and committed them, and
`idf.py set-target esp32s3 build` produced `spire-idf.bin` (190 KB) — **EXIT:0.** A component library
project, created by name, from spire-code, and it builds. The empty scaffolds keep building too, and
`idf_add_component(root, name, bus)` installs a protocol stub into one (`__idf_sps30.dir/…sps30.cpp.obj`
compiled in the same library).

**The open thread.** `SPIRE.md` is emitted and empty. Nothing yet *reads* it: when a component is
added or an application is built against a library, the hints need to reach the model as prompt
context — that is the next piece of the workflow, and the point of writing them down.

## The wired welcome screen, `SPIRE.md` reaching the model, and the container retired (2026-09-22)

**The rows do what they say.** The welcome screen's project-type list was a picture; it is now the
way a project gets made. `ProjectTypePicker` is one view, used in three places that all mean the
same thing — the welcome screen, the actions for a folder with no project in it, and the dashboard's
empty state — and `CreateProjectSheet` is the whole of what a type needs: **a name, and somewhere to
put it**. Not a wizard. The type was decided by the row that opened the sheet, and asking again would
be the tree of questions the flat list replaced.

The location field is not even asked when the folder is already open: an empty project being given a
shape knows where it lives, so the sheet opens with the folder's own name already in the name field.
An **application** gets one more *optional* field — the component library it is built against —
because it is the one type whose product is a composition, and naming it is what makes item 2 below
reachable at all.

`type.structure == nil` is the whole gate: a row with a key scaffolds through `createProject/Scaffold`
and one without is inert (it says so on hover). Four rows are still inert — the two Linux SBC types,
which have no structure key written yet — and three are real: `spire_app`, `idf_library`,
`idf_application`.

**`SPIRE.md` reaches the model.** `idf_projects::library_hints(root)` reads a library's root
`SPIRE.md`, or `None` when there is no such file **or nothing in it** — the scaffold emits the file
with a template's headings and no answers, and a heading with nothing under it is an instruction to
the model to guess. `generate_fill_plan` injects it as a `LIBRARY HINTS` block, and each structure
reads exactly one file: an application reads the library it named, a library reads its own (present
only when it is being filled *after* it was created, since the plan is asked for before anything is
written — a brand-new library has written down nothing yet, and that is not a gap to paper over).

The library travels to the plan the same way it travels to the scaffold: `embeddedRoot` on the
request, which is the field `scaffold_routing` already maps to the application's library. `FillProject`
gained the same field, because a fill without the hints is a fill against an architecture the model
was never shown. `GeneratePlan` — the legacy goal→skeleton generator — deliberately does *not* get
them: it has nowhere to put an ESP-IDF project's library, and a hints block over a prompt that then
writes a `Cargo.toml` would be worse than silence.

Verified model-free, on the real `LlmMessage` channel: a stub responder captures the prompt and
replies with a parseable plan, so the test fails on the *prompt* — `the_librarys_hints_reach_the_fill_prompt`
asserts the hints are there when a library is named and absent when none is.

**The container is retired from creation.** The Rust embedded **container** and **application** were
already unreachable from the UI — the wizard that reached them is gone — but they were still reachable
over RPC, and they were ~3,000 lines of emitters, plans and tools. Deleted: `embedded_scaffold.rs`
(1,835 lines), `embedded_app_scaffold.rs` (1,100), the `embedded_add_bsp` / `embedded_add_driver` tool
handlers and their registrations, `embedded_container_template_plan` / `embedded_app_template_plan` /
`embedded_app_fallback_spec`, the `EmbeddedApp` branches in `scaffold_spec_in_memory` and
`ScaffoldProject`, and the Swift `EmbeddedContainerSheet` + its two bridge methods + the "Container"
action card.

What was **kept** is the part that is not creation: `ProjectStructure::Embedded` and `EmbeddedApp`
still *recognize* a project that declares one (the analyzer reads the marker, and a subproject still
reports its structure), and both are now **refused by name** in `cargo.rs` — one branch covering the
pair, with the reason and the replacement in the error. That refusal is the guard the deletion needed:
without it, `createProject/Scaffold { structure: "embedded" }` would fall through to the Cargo layout
and emit a plain host crate for a firmware choice, reporting nothing wrong.

**Cleanup.** `ActionRailView` (never instantiated anywhere, and carrying its own duplicate copy of the
welcome screen's actions) is deleted. All three "Structure Project…" buttons — the right pane, the
dashboard's empty state, the action panel — now show the same project-type list with **the open folder
as the location**, so the folder is the answer to the question the wizard used to ask. The one that
landed on a "Starting a new project…" placeholder pad is gone with the wizard. Cmd-N ("New project")
closes what is open and shows the welcome screen, which is now the whole new-project flow.
`NewProjectView`'s four wizard tests are replaced by three that pin the contract the list now has:
every structure key is one the tool scaffolds, the rows without a key are exactly the two
unimplemented ones, and the ids are distinct so the `ForEach` cannot silently drop a row.

**Verified.** `cargo check -p spire-code --all-targets` clean (no warnings), `cargo test -p spire-code`
green (322 lib tests + every suite; `test_mcp_servers_loaded` failed once on a cold binary — the 120s
readiness timeout while the core was rebuilt — and passes in 3s warm). `swift build` clean,
`swift test` 17 passed. Scaffolds re-verified over RPC against the built dylib:
`{"structure":"idf_library"}` → `build_system: "ESP-IDF"`, `embedded: true`; `{"structure":"spire_app"}`
→ `build_system: "Cargo"`, a Cargo+SwiftUI monorepo. **`{"structure":"embedded"}` now returns the
retirement message instead of a project.**

## The project that opened as `main`, and the harness that was not a subproject (2026-09-22)

**Two symptoms, one line.** Creating an "ESP32 Components" project called `spire-idf` produced a
project called **`main`**, whose `CMakeLists.txt` registered a build harness. The scaffold was right —
the file on disk says `project(spire-idf)` at its root. What was wrong was where the app *opened* it.

`resolve_project_root` unwraps wrapper folders: a directory with no build file of its own and exactly
one subdirectory is descended into, so scaffolding `<name>` into a folder already named `<name>` does
not leave you in the wrapper. The "is this the project?" test was `Cargo.toml`, and **only**
`Cargo.toml`. A component library's root holds exactly one non-hidden subdirectory — `main/`, the
build harness — so the descent went straight into it, `project/open` reported
`root = …/spire-idf/main`, and the project name is that directory's own name.

Fixed by asking the question at the right scope: a directory with a **build file** is a project,
whatever the build system is. `PROJECT_ROOT_MARKERS` mirrors the modules' own registered
`config_files`, because "is this directory a project?" and "which module claims it?" should not be
two different answers.

**The harness is not a subproject.** With the root right, the library still listed `main/` as one —
the cmake module analyses every `CMakeLists.txt`, and the harness has one. It is not a component of
the product: it exists so `idf.py build` has something to compile, and it starts nothing, wires
nothing and names no device. Listing it makes a fresh library look like it already contains an
application, which is the one thing the library type is for.

Suppressing it needed the analyzer to know *which* ESP-IDF project it was looking at, and it didn't —
an ESP-IDF project was opening as a generic CMake one. So `idf_projects::structure_from` now reads the
`set(SPIRE_PROJECT_STRUCTURE …)` marker the scaffold already writes, from the root `CMakeLists.txt`,
inside the cmake module's `analyze` (analysis of an ESP-IDF project belongs there: its
`CMakeLists.txt` *is* a CMake project, which is the split `IdfBuildModule` already states).
`serialize_analysis` then drops the subproject whose path is `main` when the project is an
`IdfLibrary`. An **application**'s `main/` is its product and stays — same directory, opposite
meaning, and the difference is the root's own declaration rather than the directory's name.

Two smaller things the fix exposed. The library's one remaining subproject was labelled `cmake`, from
the generic fallback for a root config with no name (written for a bare Makefile beside a Cargo
workspace); the root config of a project that *is* its root is now named after the project — the rule
`SpireApp` already followed, generalised to both ESP-IDF types. And `buildSystems` read
`CMake · CMake` — the project's `CMakeLists.txt` and the component's — which the UI renders as a
`ForEach` keyed by the string itself, where a duplicate id is a dropped row. Deduped, order preserved.

**Verified.** `cargo test -p spire-code`: 328 lib tests + every suite green. Then over RPC against the
built dylib, on a fresh scaffold:

```
createProject/Scaffold {"projectName":"spire-idf","structure":"idf_library","language":"cpp"}
project/open          {"root":"/tmp/spire-verify/spire-idf"}

→ root: /tmp/spire-verify/spire-idf
  name: spire-idf
  subprojects: [ path=""  name="spire-idf"  buildSystem="CMake"
                 structure="idf_library"  kind="project" ]
```

One subproject, named after the project, structured `idf_library`, and no `main`. Regression tests
pin all three parts — the descent (an IDF-shaped tree resolves to itself, a Cargo project still does,
a wrapper still descends), the marker read, and the suppression, including that an application's
`main` survives it.

## Components: the seam, the fake bus, and the first host test (2026-09-22)

**A component is now a thing the tool knows about.** The analyzer labels a subproject under
`components/` of an `idf_library` project `kind: "component"` and names it by its own directory —
`components/sps30` is `sps30`, not `components`, which is what the generic path-derived name gave it.
That is the list the right pane will draw.

**The seam.** A protocol needs a bus, and a component that calls IDF's driver API directly cannot be
compiled on a host — so `add_component` now writes the component's **whole** interface to the bus as
one tool-owned header, `<name>_bus.hpp`:

```cpp
using BusHandle = i2c_master_dev_handle_t;   // the fake says `void*`
bool bus_write(BusHandle, const uint8_t* out, std::size_t out_len);
bool bus_read(BusHandle, uint8_t* in, std::size_t in_len);
bool bus_write_read(BusHandle, const uint8_t* out, std::size_t out_len, uint8_t* in, std::size_t in_len);
```

Three calls, not two, and the third is the one that matters: **a register read is one exchange with
the device.** On I²C the halves are separated by a STOP — a different transaction that plenty of
devices do not answer; on SPI the reply is clocked out while the command goes in. A seam without
`bus_write_read` would push every protocol author into an exchange its device may not understand, and
a test that only counts bytes would never see it. The per-bus bodies live in the tool, one set each,
and the component names no IDF type: it says `BusHandle`, so one header compiles both into firmware
and into the host test.

Two latent bugs fell out of writing it. The stub's `probe()` returned `device_ != nullptr` — which
does not compile for `uart_port_t`, an `int` — and its member was `= nullptr` for the same reason; and
the public header included `<driver/…>`, which is exactly what the host compiler cannot read. A stub
that returned `true` for a handle it never used was also lying about the device being there; it
returns `false` now, and says why.

**The harness.** `add_component` writes a host test beside the protocol — `test/CMakeLists.txt`,
`test/<name>_bus.hpp` (the fake) and `test/<name>_test.cpp` (the cases). The fake keeps a **script**
of frames the device answers with and a **recording** of every byte the protocol wrote, and the build
puts `test/` ahead of the component's `include/`, so the seam resolves to the fake.

The seam is included with **angle brackets**, and that is load-bearing rather than style: a quoted
include is resolved beside the *including* file, so `include/<name>.hpp` asking for
`"<name>_bus.hpp"` would always find the real one next to it, and the "host test" would be a firmware
build with a host compiler. Found by building it — the error was
`'driver/i2c_master.h' file not found`, which reads like a missing file rather than a design error.

**Removing one.** `idf_remove_component(root, name)` takes the directory and nothing else — ESP-IDF
finds a component by its directory, so there is no registry line to unpick. What it will not do is
edit another component on the caller's behalf: if something `REQUIRES` it, the removal stops and
names what depends on it, read out of the staying components' own `CMakeLists.txt`. A component
others are built on is a design decision, and a delete command is not where design decisions get made.

**Verified, both ways round.** The three components are written to a scratch library
(`SPIRE_SCAFFOLD_OUT=/tmp/idf-demo3 cargo test -p spire-code --lib dump_idf_projects -- --ignored`),
and then:

- **`idf.py set-target esp32s3 build` → EXIT:0** (`sensors.bin`, 190 KB) with
  `__idf_sps30.dir/src/sps30.cpp.obj`, `__idf_bme280…` and `__idf_gps…` all compiled — so all three
  seams and all three stubs are real IDF code, on ESP-IDF v5.5.5;
- **the host test builds and runs for all three buses** — `cmake -S test -B test/build && cmake
  --build test/build && ctest --test-dir test/build` → `1/1 Test #1: sps30 ... Passed`, the same for
  `bme280` and `gps`. No board, no chip, no IDF.

`cargo test -p spire-code`: 333 lib tests + every suite.

**Still to come.** `idf_component_edit` — the one LLM surface, whose acceptance gate is the host test
that now exists (and whose chip build is an *additional* gate, run only when a chip is known, and
reported as skipped when it is not) — and the right pane's Components section: the list, an Add sheet
that asks for the device and its description, and Edit/Delete per row.

## Writing a protocol: the context, and the gate that proves it (2026-09-22)

**The context is the feature.** `idf_projects::component_edit_request` assembles everything a model
needs to write one device's protocol, in six parts: what the library says about how its components are
used (`SPIRE.md`), the **seam** and why a register read is the combined call, the component's **public
header as it stands** (the contract its sources and its test have to match), the invariants of the
shape, **what the user knows about the device**, and how the answer will be checked. It deliberately
carries neither the source nor the test: `modify_code_prompt` puts each file's current contents in
front of the model on that file's turn, and the same file twice is a prompt the model has to reconcile
with itself.

It is the `request` of the existing `modify/code` spine, so the plan→apply→verify→rollback machinery
came for free — including the two-step rewrite (which files, then each one whole) whose details are
worth not copying: the answer is filtered to the *scope*, and a rewrite that does not parse is skipped
rather than written. That body moved to `CoordinatorActor::plan_rewrites`, shared by both callers.

**The gate is the host test.** `CoordinatorComponentModify` is a second `CodeModifyBackend`: the
"build" it measures is the component's **host test** compiled by CMake, and the "host tests" leg is
`ctest` over that binary. Nothing here needs a board, a chip or IDF — which is what makes writing a
protocol seconds rather than minutes. The chip build rides on `target_tests`, the leg the spine already
treats as optional: naming a chip adds it, naming none leaves it out **and the report says so**
(`chip_build`), rather than letting "verified" imply a chip was involved.

Two details that had to be right rather than plausible. The gate must be **runnable before anything is
planned** — a machine without `cmake` would otherwise produce an empty error list, which reads as a
clean build, so the handler refuses up front with that named. And `ctest` never runs a binary the last
build did not replace: a failed compile means `None` for that leg, not a stale pass.

**Driven end to end, with a real model.** A fresh library (`/tmp/llm-demo/sensors`) and one component
(`sps30`, i2c), then:

```
tools/call {"tool":"idf_component_edit","args":{"root":"…/sensors","name":"sps30",
  "instruction":"An I2C device at 7-bit address 0x69. Commands are a single byte. The ID command is
   0xD0 and the device replies with exactly 3 bytes: a two-byte id, most significant byte first, then
   one byte of 0x00. The device is present only if the id is not 0x0000."}}

→ success:true, verified:"HostOnly", files_changed:[sps30.hpp, sps30.cpp, sps30_test.cpp],
  chip_build:"not run: no chip named, so nothing here was checked against the real IDF driver"
  verify: build 0 → 0 error(s), host tests passed → passed
```

The model's `probe()` is `bus_write_read(device_, &cmd, 1, reply, sizeof(reply))` — the seam's combined
exchange, which is the one thing the seam exists to steer a protocol author towards. Its test scripts
the device's own three-byte frame, asserts **what was sent** (`fake_written() == {0xD0}`) as well as
what came back, and covers all three malformed cases the context asked for: a short reply, an all-zero
id, and no reply at all. Re-run independently: the host test passes, and
`idf.py set-target esp32s3 build` on the library ends in **Project build complete** (EXIT:0) — so the
model's protocol is real IDF code as well as host-tested code.

**The right pane grew the section it needed.** For a project whose declared structure is `idf_library`,
`ComponentSection` lists the analyzer's `kind: "component"` subprojects with a Write and a Remove per
row, and an Add that opens `ComponentSheet` — device, bus, and the description box, whose placeholder
text says where those facts come from. Add and edit are one sheet because they end in the same place.
A "Stub only" action exists for a component whose facts are not in hand yet. The run reports what was
checked, including the chip-build line, and a change the gate refused is shown as the result it is
rather than as a tool failure.

One integration bug caught by *adding the kind*: `SubprojectKind` is a `String`-backed `Codable` enum,
and such an enum **throws** on a raw value it does not know — so a `"component"` kind the Swift side
had never heard of would have taken the whole `ProjectInfo` decode with it and a library would have
failed to open, not merely shown an odd row. The case is declared, with that reason written next to it.

## A component is kind-stated: `driver` or `library` (2026-09-22)

**The correction.** "Component = a device on a bus" was baked in at every layer, and the evidence was
not in one place but in six: `add_component` *required* a `bus`; the stub was always `class Name {
explicit Name(BusHandle); bool probe(); }`; the seam and the fake bus were always emitted; `REQUIRES
esp_driver_i2c/spi/uart` always followed from the bus; the edit request's invariants ("owns its bus
handle, returns false") were meaningless for CPU code; and the Add sheet had a mandatory bus picker. A
component library is *supposed* to hold a filter, a codec or a DSP block — code with no device — and
there was no way to say so.

The gate itself was the one part that needed nothing: `run_host_test` builds `test/CMakeLists.txt` and
runs `ctest`, and it does not care what the component is. So this was skeleton emission, prompt and UI —
**not** the spine. That is why the two kinds can share a path at all, and it is worth stating as the
reason the change stayed small.

**The kinds, split on one question — has it a bus?** `ComponentKind { Driver, Library }`, and the two
skeletons have nothing in common. A `driver` is today's seven files: header, source, `<name>_bus.hpp`,
`test/CMakeLists.txt`, `test/<name>_bus.hpp`, `test/<name>_test.cpp`, and a `CMakeLists.txt` whose
`REQUIRES` follows from the bus. A `library` is five: `CMakeLists.txt` with **no** `REQUIRES`,
`include/<name>.hpp`, `src/<name>.cpp`, and a `test/` that is an ordinary unit test — no seam, no fake,
and none of the include-path ordering a driver's harness needs, because there is nothing to stand in
for. `component_files(name, kind, bus)` branches, and `bus` is *ignored* by a library: a bus parameter
that means nothing on half the calls would be a contract that lies, so the tool schema requires `kind`
and documents `bus` as driver-only. The templates moved to `templates/esp-idf/component/{driver,library}/`
for the same reason — the directory states the kind.

**The kind is stated, not inferred — and then it is a fact of the component.** `set(SPIRE_COMPONENT_KIND
driver|library)` is written into the component's own `CMakeLists.txt` (the same mechanism as the
project's `SPIRE_PROJECT_STRUCTURE`), and `component_kind(root, name)` reads it back. Nothing infers it:
`idf_add_component` refuses an unknown `kind` by name rather than defaulting, because the kind decides
the skeleton and is not recoverable from it afterwards. The edit path then *branches on the component's
own statement*, which is what makes a driver's request and a library's request different documents; for
a component that states none — one written by hand — the fallback is what its **files** show (a seam
present means a driver, which is a fact on disk rather than a guess), and the UI says "states no kind"
rather than picking one.

The `componentKind` travels to the UI in the analyzer's subproject JSON, beside `kind: "component"`:
read in Rust from the same `CMakeLists.txt`, never re-derived in Swift, so the row's label cannot
disagree with what the prompt will be told.

**The prompt is per-kind, and the *absences* are the point.** `component_edit_request` keeps its shared
frame — `SPIRE.md`, the header as it stands, what the user knows, "kept only if the host test compiles
and passes" — and moves everything device-shaped into `driver_edit_request`: the seam, the register-read
paragraph, "owns its bus handle / takes no board facts / `probe()` answers whether the device is there".
`library_edit_request` says the opposite: *pure*, the caller's facts rather than a global, no IDF type
and no peripheral, returns false, small — and its gate paragraph is a table of inputs and expected
outputs, "an ordinary unit test, with no fake and nothing stood in for", edges and refusals included. An
empty instruction is answered per kind too: a driver must not invent a register map, a library must not
invent an algorithm. The tests assert the absences (`!library.contains("bus_write")`,
`!driver.contains("it is **pure**")`) because that is the half a regression would quietly restore.

**The chip picker is now a first-class option, in the sheet for both kinds.** `ComponentSheet` gained
"Verify against": *Host only* (default) or a chip from the platform registry (`os: esp-idf`, `kind:
chip`), wired through `idfComponentEdit(platform:)` — the leg that was already optional. Host-only stays
the default because it is the honest default: the report names what did not run. The kind picker and the
bus picker appear only when adding, and the bus only for a driver.

**Verified.** 23 unit tests over `idf_projects` — the skeleton per kind, the marker round-trip, an
unknown kind refused rather than defaulted, the per-kind request, and the per-kind "invent nothing" —
plus one over `serialize_analysis` that pins `componentKind` **from the side that writes it** (a drift
in that key would decode as `nil` in Swift, and `nil` is a meaningful value there: "states no kind", so
the two halves are pinned separately). The whole `cargo test -p spire-code` suite is green, `swift test`
is 18, and `cargo fmt --check` passes.

The generated library (`SPIRE_SCAFFOLD_OUT`) now carries one `moving_average` **library** component
beside its three drivers, and both gates were run against it for real. Its emitted stub's host test:
`cmake -S . -B build && ctest` → **1/1 passed**. Then the same component *filled in* the way the library
prompt asks — a header, a pure implementation, and a test that is a table of inputs and expected means,
with the edges (a window of one, samples at the top of the range) and the refusals (no storage, a window
of zero, a null out-parameter) — and the gate run again: **1/1 passed**, with `-Wall -Wextra` and no
warning. The second gate is the one that settles the argument the kind split is about: `idf.py
set-target esp32s3 build` on the library ends in `Project build complete` (EXIT:0), having compiled
`esp-idf/moving_average/CMakeFiles/__idf_moving_average.dir/src/moving_average.cpp.obj` and linked
`libmoving_average.a` — so "no IDF type, no `REQUIRES`, the same code on the host and on the chip" is
demonstrated rather than asserted. (Building the IDF environment by hand was needed this time: the
installed venv is `idf5.5_py3.14_env` while `export.sh` derives `idf5.5_py3.12_env` from whatever
`python3` is first on `PATH`, so `IDF_PYTHON_ENV_PATH` has to be set and the missing gdb/openocd
packages skipped.)

## ~~A component can be *someone else's* code: the `source` axis and `idf_vendor_component`~~ **Superseded (2026-09-23) — see "The framework is what a library *ships*" at the end.** There is no `source` axis and no `idf_vendor_component` any more: the one upstream this library needs is hard-coded as a framework component, and the protection vendoring gave is now stated by `FRAMEWORK_COMPONENTS`. Kept for the reasoning, which is what the next upstream will need.

**The question that opened it.** "Should we package `ramen.hpp` as a component?" — RAMEN being Zubax's
single-header C++20 dataflow actor library — followed immediately by the observation that reframed it:
*"we will hit the same problem again, not just for h files, but pieces of software repackaged as a
component."* That is the real shape of it. What recurs is not "header-only"; it is **code whose origin
is not this tree**.

**Two axes, not one.** `kind` (`driver` | `library`) answers *what it is* — has it a bus. `source`
(`generated` | `vendored`) answers *where the code came from* — written here, or placed from upstream.
They are independent, so there are four cells, and **"header-only" is not one of them**: it is simply a
vendored component with no sources, which is what most single-header software is. A `header_only` flag
would have fixed RAMEN and nothing that comes after it, which is why this is an axis instead.

**Where it is stated.** `set(SPIRE_COMPONENT_SOURCE generated|vendored)` beside
`set(SPIRE_COMPONENT_KIND …)` in the component's own `CMakeLists.txt`, with the pin in the same file:
`SPIRE_COMPONENT_UPSTREAM`, `SPIRE_COMPONENT_REVISION`, `SPIRE_COMPONENT_LICENSE`. Read back by
`component_source()`; *absent* means `generated`, which is what every component scaffolded before this
axis existed is. The generated scaffolds now write the line too, so the axis is stated in both
directions rather than inferred from a missing line.

**A separate tool, because it is a different operation.** `idf_vendor_component(root, name, kind,
files[], upstream, revision, license, cpp_standard)`: it is handed **files** instead of a description,
and each is a `(from, to)` pair — a local path to read, a path under the component's `include/` or
`src/` to write. **Fetching is the caller's job and reading is the tool's**, so nothing here reaches the
network and the operation is reproducible on a bench without wifi. The `to` path is the one
caller-supplied thing the tool acts on directly, so it is confined to `include/`/`src/` — no `..`, no
absolute, no climbing out of the component.

**Lopsided on purpose: everything placed is structural, and exactly one file is fillable.** Upstream's
code goes in byte-for-byte and is out of `component_scope`, so `idf_component_edit` can never rewrite
it; `test/<name>_test.cpp` is the whole of the scope. Vendoring without a test is a copy — the test is
what makes it a component, and the gate (host test, optional chip build) means exactly what it means
everywhere else. The request is inverted to match: no "invariants of the shape" to keep, because the
shape is upstream's; the code is handed over so the API can be *read* (inlined up to 64 KB, and named as
too large rather than silently truncated past it); and the job is the cases.

**`library` only, and the review of RAMEN is the reason.** The tool refuses `kind: driver` by name. The
driver skeleton exists to stand a fake bus in for a device that a model writes a protocol for; upstream
code reaches for IDF itself and fits none of that.

**`cpp_standard` is now a stated attribute** (on both add and vendor; default 17; 11/14/17/20/23/26
accepted). A fact of the code rather than a preference: RAMEN is C++20 and the library harness was
hard-coded to 17, and a C++20 header built at 17 fails as a wall of concept errors that names the
concepts and not the standard. The half a component cannot state itself — that its *consumers* must also
build C++20 — is the caller's to write in `SPIRE.md`, which is exactly what that section exists for.

**What the fixture is, and what upstream is.** The `ramen-framework` fixture's `ramen.hpp` carries a
block labelled *"a custom extension to RAMEN for the AI Camera Trap framework"*: an `Actor` base class, a
global `ActorRegistry`, and name-based `send<T>()` over `typeid`, using `std::mutex`, `std::string` and
RTTI. Upstream has none of it. Upstream is 976 lines, MIT, `#pragma once`, standard-library-only and
allocation-free (its `Function` keeps its target in a `std::array<std::byte, fp>`), with four ports
(`Pusher`/`Pushable`/`Puller`/`Pullable`), `>>` and `^`, topics, and
`Latch`/`Lift`/`PushUnary`/`PullUnary`/`PullNary`/`PushCast`/`PullCast`/`Ctor`/`Finalizer` — note that
`PushNary` is still a TODO upstream: the pull side is n-ary, the push side is not. So the fixture stays
what it was, a *test artefact for the tool's side of the contract*, and what gets vendored is upstream's
own file, pinned.

**A gap this exposed in header-only components.** A component with `INCLUDE_DIRS` and no `SRCS` is never
compiled by `idf.py build` — the include path is registered for its consumers and nothing else happens,
so a chip build *looks* like verification and is not. Rather than paper over it, the generated
`CMakeLists.txt` now says which it is: with sources, `idf.py build` compiles them with the chip's
compiler; header-only, it checks that the registration resolves and the header itself is compiled by the
host test.

**Verified end to end.**
`SPIRE_SCAFFOLD_OUT=/tmp/spire-idf-vendor SPIRE_VENDOR_RAMEN=/tmp/ramen_upstream.hpp cargo test -p
spire-code --lib dump_idf_projects -- --ignored` vendored the real upstream header, 51.8 KB, as
`components/ramen/` — `CMakeLists.txt` (kind, source, pin, no `SRCS`), `include/ramen/ramen.hpp`
byte-for-byte, and the two test files. Its host test then built and passed on this machine (**1/1
passed**, `-Wall -Wextra`, no warning), which is the vendored header compiling at **C++20**. The
cross-compiler agreed independently: `xtensa-esp32s3-elf-g++ -std=gnu++20 -fsyntax-only` on a TU that
includes it is clean. And `idf.py set-target esp32s3 build` on the library ends in **Project build
complete** (EXIT:0), with `ramen` in the component list and `sensors.bin` written — alongside the three
drivers and the generated `moving_average`, all unchanged.

29 tests now cover `idf_projects` (six of them this axis: the layout both ways, the path confinement,
the scope and the request, the over-budget prompt, and the refused vendored driver), `cargo test -p
spire-code` is green, `swift test` is 18, and `cargo fmt --check` passes.

## ~~A component can be *linked* to its upstream, not only a copy of it~~ (2026-09-22) — DESIGNED, NOT IMPLEMENTED **Superseded (2026-09-23) as machinery — see the entry at the end.** There is no `source` axis for `linked` to be a third value of, and the one upstream that motivated it is now a hard-coded framework component with its provenance in a comment. The design below is still the place to start if a *second* multi-file upstream ever arrives.

**The gap a copied file leaves.** `idf_vendor_component` places bytes and pins them, but that pin is
*static metadata*: it answers "where did this come from" and cannot `git log`, `git diff` or `git pull`.
For RAMEN — one self-contained header — that is the right end state, because for a single-file project
the file *is* the repository. For anything with more than one file, its own history and its own tests,
the repository is the natural unit, and copying loses real value: history, upstream's tests, the ability
to take a fix, attribution at the file level.

**Why this is written down now and built later.** RAMEN is the only concrete case, and a submodule for
it would be ceremony for no benefit. The decisions are cheap to state now and expensive to rediscover
later, and they are these: what the pin *is*, who owns the layout of the code, and what happens when
somebody wants to patch upstream.

**The axis gains a third value.** `source` becomes `generated` | `vendored` | `linked` — still one axis
(where the code came from), with three answers that differ in one respect each:

| `source` | the code is | the pin's source of truth | who owns the layout |
|---|---|---|---|
| `generated` | written here (a stub a model fills) | — | **spire** (`include/<ns>/…`) |
| `vendored` | a copy of a file | our markers (`SPIRE_COMPONENT_UPSTREAM` …) | spire (upstream's path under `include/`) |
| `linked` | a **git submodule** | **git itself** — the recorded commit | **upstream**, verbatim |

**The shape: a shim we own, around a checkout we do not.** Upstream cannot carry our markers — writing
`SPIRE_COMPONENT_KIND` into someone else's repository is a fork, and a fork that has to be rebased. So
the statement of what the component *is* lives beside the code rather than inside it:

```
components/<name>/
  CMakeLists.txt   ours: kind, source=linked, INCLUDE_DIRS "upstream", REQUIRES
  upstream/        the submodule — byte-for-byte, git-managed
  test/            ours: the host test, the whole of our work here (as for a vendored component)
```

`INCLUDE_DIRS "upstream"` maps upstream's layout onto our include path, so with a submodule at
`components/ramen` upstream's own `ramen/ramen.hpp` is included as `#include <ramen/ramen.hpp>` without
upstream being reorganised into our `include/` convention. That is the rule: **upstream keeps its own
structure, spire-written components keep ours** — and a *copied* file is the one case where we normalise,
because there we own the layout and can place it under `include/`. IDF auto-discovers components only one
level below `components/`, so a submodule that ships its own `CMakeLists.txt` is not registered twice.

**The pin moves to git, which makes it stronger.** The submodule's recorded commit is the pin, with the
remote and branch recorded beside it for a reader. And "no local patches" stops being a promise the tool
has to keep and becomes a property of the tree: an edit under `upstream/` shows as a dirty submodule in
`git status`. Changing upstream behaviour means a pull request, or a fork the submodule re-points at —
which was the agreed price of a live connection.

**The network rule is unchanged, and it is the whole reason for a submodule over a component-manager
dependency.** `idf_component.yml` with a `git:` source is the idiomatic ESP-IDF answer, and it resolves at
*build* time — which is precisely the failure this library already writes down ("a build that reaches the
network is a build that fails on a bench with no wifi"). A submodule is fetched at *development* time
(`git submodule update --init`), and the only other network call this design introduces is an explicit
"check for updates" on request, never during a build.

**Designed, not implemented — none of this exists today.** `ComponentSource::Linked`; a `link_component`
operation (init the submodule, write the shim, scaffold the test); the shim template; the analyzer's
submodule detection and `.git` / `.gitmodules` skipping (a grep for `submodule`, `.gitmodules`,
`idf_component.yml` or "component manager" finds nothing anywhere in the tree); and the row's "check for
updates" affordance. When the first upstream with more than one file arrives, this is the entry to start
from. The test-writing path is already the right shape: a linked component's request inlines upstream's
headers up to the same budget a vendored one uses, and the scope is `test/` only.

**What does not change.** RAMEN stays **`vendored`**: one header, MIT, changed rarely — and a copy that
compiles hermetically on a bench with no wifi is the better answer there than a submodule. The gate is
untouched as well: a linked component would still compile from `test/` on the host and could still be
chip-built, because the gate never cared where the code came from.

## The framework is what a library *ships*: two application frameworks, and the `source` axis retired (2026-09-23)

**The reversal, stated plainly.** A component library used to start **empty**: the tool had no opinion
about what belonged in `components/`, and the first decision was the author's. It now starts with a
**framework** — three components that are already there — because a framework *is* the architecture, and
shipping it as something the model has to write is asking the model to invent the architecture. That is
the one thing `SPIRE.md` exists to state rather than leave to inference. A library that adopts neither
framework is still a perfectly good library; what the framework removes is the *gap*.

**What survives from the old rule is the part that mattered: provided, not chosen.** The scaffold emits
the frameworks without picking one for the product, so neither the library's own `CMakeLists.txt` nor the
build harness — the shape an application would otherwise inherit — names a framework. There is a test for
exactly that (`declares_no_framework` over the harness and the project file), because it is the property
that would rot silently.

**Why *two* frameworks, and why they share one class.** The question "should RAMEN be a component?" ended
by dissolving the premise: RAMEN is **not** the actor pattern. It has no identities, no mailboxes and no
scheduler — it is synchronous, inline dataflow, and a "RAMEN actor" is a struct with ports plus a task
that pushes. So the **actor is the application's composition**, not a library object, and the frameworks
are two different bets on how to make one:

| | `ramen` — dataflow | `actors` — classical |
|---|---|---|
| a unit is | a struct with typed ports | a class with a mailbox and `on_message` |
| dispatch | **inline**: the pusher runs every wired behaviour before returning | **posted**: a queue, taken when the actor is ready |
| ordering | link order, on one stack | queue order, across tasks |
| reach for it when | **streaming** — frames, samples, readings, a pipeline | **many interacting agents** — commands, timers, events from several sources |

They share exactly one thing, and it is the *only* thing they may share: **`toolkit`'s `spire::Task`** —
a loop, a stack, a priority, a core. `actors` `REQUIRES toolkit`; `ramen` requires nothing; neither
requires the other, and a component requires neither. `toolkit`'s comment-level `REQUIRES ramen` had to go
for this to be true — a task seam that depended on one framework would force the other to depend on it too.

**Which framework is chosen is an application's business, and application-side tooling for the choice is
deferred** until a real classical-actor app exists. The choice is written down in the library's `SPIRE.md`
(`Choosing a framework`) and expressed once, in the application.

**RAMEN is upstream's, byte for byte, with a documented pin instead of machinery.** `include/ramen/` is
`Zubax/ramen` @ `94886680bd48901121350c4542116abeda1eb746` (sha256 `f7c6b036…`), MIT, with URL, revision,
hash and licence on the component's own `CMakeLists.txt` — as **comments a human reads**, not markers a
tool reads. It is upgraded by replacing it. The old `SPIRE_COMPONENT_UPSTREAM`/`_REVISION`/`_LICENSE`
markers and their read-back are gone with the axis; a pin nobody parses is a pin at the wrong altitude for
a file that ships in a template.

**~~Header-only components are never compiled by `idf.py build` — so each gets a compile-check TU.~~** **Superseded (2026-09-23): only `ramen` keeps one — see "The actor framework is *complete*" at the end.** The rule below is right about header-only components, and `ramen` is one (it is upstream's template library). `toolkit` and `actors` no longer are: they have real `src/*.cpp` now, so the translation unit *is* the compile check.

`src/<name>_compile_check.cpp` includes the header, asserts the standard, and instantiates the templates,
because otherwise a header-only component registers an include path, compiles nothing, and passes whether
or not the header works. It earned its keep immediately: the build log shows
`Building CXX object esp-idf/{toolkit,actors,ramen}/…compile_check.cpp.obj` and then `libtoolkit.a`,
`libactors.a`, `libramen.a` — which is the evidence that the mailbox code and upstream's header both build
on xtensa, rather than merely parse in an editor.

**C++20 costs nothing, because ESP-IDF already compiles past it.** For chip targets IDF passes
`-std=gnu++2b` (`tools/cmake/build.cmake`), which is past C++20 — so a project-level `-std=` would be a
second, conflicting answer to a question IDF has answered. What still needs the standard stated is
anything built **outside** IDF: the framework's host test sets `CMAKE_CXX_STANDARD 20` itself, and the
component test harnesses are now hard-coded to 20 rather than handed a number. The **per-component
`cpp_standard` parameter is therefore deleted** — from `component_files`, `add_component`, the tool
schema and the Swift bridge — along with `CPP_STANDARDS` and its validation. It was a parameter that
stated what was already true of the library.

**The `source` axis is retired, and vendoring's protection is kept by a better rule.** The axis
(`generated` | `vendored`), `ComponentSource`, the pin markers, `VendorPin`, `vendor_files`,
`vendor_component`, `idf_vendor_component` (handler and schema), `component_facts`, the Swift
`VendorComponentSheet` and the pin row are all gone. What they protected — *upstream's code is not this
tree's to rewrite* — is now one list and three guards on it:

- `FRAMEWORK_COMPONENTS` = `toolkit`, `ramen`, `actors`, kept in step with `FRAMEWORK_FILES` by a test;
- `component_scope` returns **nothing** for one of them: no file in the framework is fillable;
- `component_edit_request` **refuses** one by name, saying it is shipped and upgraded by replacement;
- `add_component` **refuses** one too, because scaffolding `ramen` again would replace upstream's header
  with an empty stub.

That is stronger than the vendored promise (it covers the whole component, not one directory) and it is
made in one place instead of per component. The **`linked` design is superseded as machinery** — there is
no axis for it to join — but its reasoning still stands if a second multi-file upstream ever arrives, and
its entry above says where to start.

**The rule that fell out of the whole exercise: abstract the protocol, not the actor.** A component holds
a device or an algorithm and knows no pins, no board, no `Task` and no framework; an *application* holds
the composition, the board facts, and the actors. A `SPS30Actor` therefore belongs in the consuming
application, never in the library — which is what makes a component reusable across products that made
different framework choices.

**Where it all lives.** `templates/esp-idf/framework/<name>/` (the components), `hints.md` (the `SPIRE.md`
seed: what is here, which to reach for, the lifecycle, the standard), the library README, and
`library_scaffold`, which emits all eleven framework files as **structural** — no tool edits an
architecture.

**Verified the same day.** `cargo test -p spire-code` green (343 lib + all integration tests), `swift test`
18, RAMEN's framework host test **4/4 at C++20** — inline push, link order, a combinator, and a `Latch`
bridging push to pull, which is the first time the framework's *semantics* have been pinned rather than
assumed — and a real `idf.py set-target esp32s3 build` **EXIT 0** with the three framework components
compiled and linked beside `sps30`, `bme280`, `gps` and `moving_average`.

## The actor framework is *complete*: a scheduler, typed refs, and a real `.cpp` (2026-09-23)

**The complaint that reframed it:** *"I expected them to be complete, not missing their
implementations."* It was correct, and worth recording exactly how. `actors` was an **abstract base** —
`Actor<Message>` with an embedded mailbox and a receive loop — and nothing else: no concrete actor, no
`spawn`, no refs, no behavioural test. And all three components were header-only, so every `src/`
held a compile check and nothing that ran. The first cut was a *skeleton*, and shipping a skeleton as
the framework an application is written against is the same mistake as making the model invent the
architecture, one level down.

**The split is C++'s answer, not a preference — and saying so is half the work.** What is a template
on the message type *cannot* live in a `.cpp`; what is not a template has no business in a header. So:

| | where the implementation lives | why |
|---|---|---|
| `toolkit` | `src/task.cpp` | `Task` is not a template. Every line in it is a FreeRTOS call. |
| `actors` | `src/scheduler.cpp` | the queue, the task and the type-erased `Entry` are not templates |
| `actors` | `include/actors/*.hpp` | `Actor<Message>`, `ActorRef<Message>` and `spawn` must be templates, so they must be here |
| `ramen` | header only, compile check kept | it is upstream's **template** library: a `.cpp` would mean forking it |

That also removed the compile-check TUs from `toolkit` and `actors` — a real translation unit compiles
the headers for you. `ramen` keeps its, because otherwise `idf.py build` would register an include path
and compile nothing.

**What the framework gained.** A `Scheduler` that owns each actor's object, mailbox and task, starts
them in spawn order and stops them in reverse, with an **all-or-nothing** start that rolls back what
already started; typed **`ActorRef<Message>`** handles, so a sender never sees the actor's type; and a
**ref-counted `Mailbox`**, which is a lifetime decision rather than a style one — a ref outliving the
scheduler is normal (an application keeps one past `stop()`, an actor hands one to a peer), and a
queue deleted underneath a live handle is a use-after-free found during shutdown, which is the worst
time to find one. A stale ref now *refuses* instead of crashing.

**One real bug, found by the test rather than by reading.** The mailbox was opened at `start()`, so a
freshly-spawned `ActorRef` was **invalid** — while the header documented the opposite ("spawn
everything, wire it, then start", with early messages waiting in the queue). The test caught it on the
first run. Fixed: the queue opens at **spawn**; an `init()` refusal *closes* it, so a refused actor's
ref is invalid rather than a queue nobody will read; and `shutdown()` pairs with `started_` (the loop
actually ran) rather than with the mailbox, because a spawned-but-never-started actor has nothing to
shut down.

**The host test is the drivers' fake-bus trick applied to the platform.** `test/freertos/{FreeRTOS,
queue,task}.h` + `test/freertos_fake.cpp` stand a fake FreeRTOS in front of the chip's, and it is
*deliberately not* a mock: the queue is a real deque under a mutex and a condition variable that can
fill up and blocks and wakes, and a task is a real `std::thread`. So "it dispatches" is observed. Five
cases: a message reaches the actor; a refused `init()` starts nothing; an actor sends to another
(two message types, through a ref it was constructed with); a full mailbox refuses instead of blocking
its sender; and an actor posts to itself through `self()`. 10/10 runs, no warnings.

**Concrete actors: the worked ones are in the test and the docs, not shipped as a toy set.** The
documented `Counter` example is in `actor.hpp` and `SPIRE.md`; the test carries a `Counter`, a
`Sampler`→`Sink` pair with two message types, and a self-posting `Tickler`. Nothing was added to
`components/actors/` that every consuming library would then carry unused — an actor is the
*application's* composition, and a framework that shipped a `Counter` would be one step from shipping
an architecture. If a shipped example actor is wanted later, this is the place to say which one.

**One C++ rule worth writing down, since it cost a round trip.** A friend of a base class *cannot*
call a protected override through `Derived*`: the override declares the member in `Derived`, and
`Derived` grants nobody friendship, so the access check is about the wrong class. Naming it through the
**base** pointer (`Actor* actor = derived; actor->on_message(m);`) is both legal and still virtual —
which is how the type-erased `dispatch` hook reaches a subclass's handler without making `on_message`
public.

**Verified.** `cargo test -p spire-code` (24 in the IDF module), and on a freshly generated library:
`actors` **1/1** on the host against fake FreeRTOS, `ramen` 1/1, `moving_average` 1/1, `sps30` 1/1, then
a real `idf.py set-target esp32s3 build` with `toolkit/src/task.cpp` and `actors/src/scheduler.cpp`
compiled for xtensa.

## The design phase: a spec a person reviews and a tool checks (2026-09-26)

**The gap it closes.** The library is self-describing, the components arrive with host tests and the
framework ships complete — and the *application* was still made by asking a model an open question. An
application is a **composition**: a few units of work (drivers and pure algorithms, which are
framework-agnostic) arranged into loops, messages and wiring, which is the application's own. Nobody can
review a paragraph that describes that arrangement — "read the SPS30 on a timer and show the average" is
silent about who owns the timer, what the average is computed over, and which address the sensor is at —
and nobody can check it either. So the design phase's artifact is a **typed spec**, and
`build/application_spec.rs` is it: `parse_spec` reads what a model writes, `validate` is the six rules a
tool can *check*, and `design_request` is what the model is asked.

**The framework is decided first, derived rather than picked.** It shapes everything downstream — the
`main/` template, the decomposition idiom, the prompts — and the *decomposition is the framework*, so the
two cannot be chosen separately. The rule, as the reason rather than the vocabulary:

- **human timescale, interactive, command/event-driven → `actors`** — a touch screen, a menu, a reading a
  second, a device reacting to commands and timers while holding state;
- **machine timescale, high-rate data flowing through stages → `ramen`** — frames, kHz samples, an
  inference pipeline, where a value is *moved and transformed* rather than *waited on*.

A stream is a **component's** job either way (a Klipper status channel and a sensor bus are both
drivers), so "streaming vs agents" is the *reason* and not the test. The model returns its choice with a
one-line justification for a person to confirm or override, and the choice is recorded in the
application's own `CMakeLists.txt` as `set(SPIRE_APPLICATION_FRAMEWORK actors|ramen)` — so no later reader
has to deduce it from the shape of `main/`.

**Three forks settled before writing it:** the framework enum stays **extensible** (adding one is a
variant *plus* a `main/` template, a decomposition idiom and a host test — which is why an unknown
framework is a *refusal* and not a guess); **no mixed applications yet**; and the spec is
**review-and-approve** rather than a fire-and-forget generation.

**The schema: three kinds of unit, and the framework is what decides which two may appear.** A unit is a
`component` (a **driver** on one bus, or a **library** of pure code — the framework-agnostic kind, shared
by both frameworks), an `actor` (a `Message`, the `state` behind it, the components it `uses`, and the
units it `sends_to`), or a `stage` (what it `pulls`, what it `pushes`). Around them: `board {chip, bsp,
hal}`, the `board_facts` (pins and addresses, which are the *application's* — the rule the whole
component library rests on), and the `wiring`, stated rather than inferred. One struct with the
kind-unused fields empty, because a spec is read by a person and a component must not be able to quietly
grow a `message`.

**The six rules, each one a failure that would otherwise reach the compiler or a board:**

1. one framework per application — an `actors` application has `component` and `actor` units, a `ramen`
   application has `component` and `stage` units;
2. every `actor` states a `message`; every `stage` states at least one port;
3. every reference resolves — wiring ends, `sends_to` and `uses` — the **wiring connects only actors and
   stages** (never a component), **`uses` names only components**, and a ramen dataflow has no cycles;
4. every component has a `role`; a `driver` states a `bus`, a `library` states none, and no component
   carries composition;
5. every board fact's `device` is a **driver component in this spec** (an onboard display or gauge is the
   board's, not a fact);
6. nothing invented and unused: every unit is referenced by the wiring, a `sends_to`, a `uses`, or a board
   fact.

Rule 3's second half is the one that earns its keep: wire a driver and it needs ports, and ports are the
framework's — which is exactly how a "component" stops being reusable. Rule 4's mirror image forbids a
`library` a `bus`, because a component that names no device has none.

**Writing the worked examples changed the schema twice, which is the point of pinning them.** App 1
(the M5Stack CoreS3 PM2.5 meter: `sps30` @ 0x69 and `sht20` @ 0x40 on I²C, `moving_average`, and the
`sampler`/`air_quality`/`view`/`touch`/`power` actors) validated first — except that `moving_average`, the
library `air_quality` *wraps*, was referenced by nothing at all. Hence the **`uses`** field, which is also
half of what a reviewer reads ("`air_quality` wraps `moving_average`"). App 3 (the ESP32-P4-NANO insect
trap: `camera` → `capture` → `preprocess` → `detector` → `classifier` → `trigger` → `trap`) then showed a
stage at each **end** of the chain — a frame pump *reads a device* rather than being handed a value, so it
has only `pushes`; a trigger only drives one, so it only `pulls`. Two schema changes, both found by trying
to write a real application down rather than by reasoning about the schema in the abstract.

**The description form is six questions, and the sixth is the one everybody forgets.** What it does; what
it **senses** (and roughly how fast); what it **acts on**; what it **reacts to over time** (periodic,
on-command, on-threshold, on-event); **timing** (what is parallel, what is human-rate, what is
machine-rate); and what happens **when something is missing** — a sensor absent, WiFi down, battery low.
Four or five of them decide the framework; the last one decides the behaviour nobody writes unless asked.
All six are in `design_request`, so a model handed a thin description can see *which* question went
unanswered and say so rather than filling the gap — the same rule as everywhere else: nothing is
invented, and a gap left open is reported.

**What validation deliberately does not promise.** A spec names a message; whether that name is a type a
mailbox can hold — trivially copyable, default-constructible — is not a spec's business. That half is the
**compile's**: `spire::Actor`'s `static_assert`s and the `actors` component's host test, which runs on
this machine. Six rules a tool can check plus a compile that checks the rest is a better division than a
validator pretending to type-check C++.

**Verified.** 17 tests in `build::application_spec`: both worked decompositions validate clean, one
negative per rule (a mixed application, a message-less actor, a port-less stage, an edge to nothing,
wiring a driver, a dataflow cycle, a busless driver, a bussed library, a component carrying composition, a
board fact for a non-driver, an invented unit nobody uses), an unknown framework refused rather than
guessed, a fenced answer parsed, the request carrying the board + the form + the rule + the schema, and
the marker line. `cargo fmt -p spire-code --check` clean.

**Next, and not yet built.** The tool and the UI: the form, then the model call, then the review screen,
then the scaffold — plus the live `registry/*` tool the second and third applications already need
(`esp_lcd_st7701`, `esp_lcd_touch_gt911`), an `esp-dl` corpus before the insect trap's detector can be
written from documentation rather than assertion, and mixed-framework support, which waits for a second
actor system that genuinely wants to sit beside a pipeline.

## The design phase becomes reachable: one request, one tool, and a repair round (2026-09-26)

**The gap it closes.** A schema and a validator nobody calls is a library, not a feature. The design
phase now has two callers — and it is *one* implementation behind both, because a second copy of "ask
the model, check the answer" is how the two silently drift.

| caller | shape | why it exists |
|---|---|---|
| `createProject/DesignApplication` | `{ board:{chip,bsp,hal}, description, framework? }` | the **wizard**: a board and the form's answers in, a spec to review out |
| `idf_design_application` | `chip`, `bsp`, `hal`, `description`, `framework` (flat) | the **model**, in chat: "design this application for this board" |

The tool's arguments are flat because that is what a model writes; the request takes a `board` object
because that is what the wizard has. So the tool handler normalizes one into the other and delegates —
which is exactly how `idf_component_edit` already works (declared in `list_tools()`, intercepted in
`route_request` before the build manager sees it). Both responses carry `{ spec, marker }`: the spec to
review, and the exact line — `set(SPIRE_APPLICATION_FRAMEWORK actors)` — the application will state its
framework with, because that line *is* the whole of what the choice becomes in the tree.

**The repair round, borrowed from the AppSpec requirements pass rather than invented.** The design phase
is an LLM stage like `GenerateAppSpec`: the model is injected as a plain async `call` closure (so the
tests run on canned answers, no actor and no network), the answer is parsed and checked against the six
rules, and a *rejection is re-asked with the problems named* — up to `MAX_DESIGN_ATTEMPTS` (3). Two
decisions worth recording:

- **A parse failure is a problem like any other.** A model that answers in prose or YAML gets told so and
  answers in JSON, instead of the whole design phase failing on a formatting habit — the fence-stripping
  in `parse_spec` handles the common case, and this handles the rest.
- **A refusal is the whole answer, not a draft plus issues.** What a person reviews is a spec that
  *passed*, because a decomposition that does not hold together is not something to approve. The error
  names what the last answer got wrong, so the caller can say which rule the model could not satisfy
  rather than "invalid spec". (When an editing UI exists, "show the draft and the problems" is a small
  change to one return type — deliberately not built before then.)

**The leg lives in the coordinator, not the creation actor — the rule `idf_component_edit` already
follows.** The design phase needs the model and *nothing else*: no project, no files, no graph, and it
deliberately runs before the tree it describes exists. Putting it behind a `project_creation` message
would have meant reaching the actor through the FFI registry, which no test has — so the leg that decides
would have been the one leg that could not be tested end to end. In the coordinator it is exercised
against a real `LlmActor` over HTTP, and the creation actor keeps the thing that needs creation state
(`GenerateAppSpec`'s graph persistence).

**The request validation moved out of the handler, so it could be tested.** `board_from_json` refuses a
board with no `chip` or no `bsp` (a guess at either is a build that fails on hardware) and treats `hal`
as genuinely optional; `framework_from_name` refuses an unknown framework *by name*, because adding one
is adding a `main/` template, a decomposition idiom and a host test. Both live in `application_spec`
next to the schema they guard, and both are unit-tested — a handler that needs the whole FFI to run was
the wrong place for the contract the UI shows.

**The worked examples became public.** `application_spec::examples::{PM25_METER, INSECT_TRAP}` is now one
canonical copy of each decomposition, used by this module's tests *and* the creation flow's — so the
actor-level test proves the real wiring and the real prompt against the same app 1 the user will review,
rather than against a stub spec.

**Verified.** 24 tests in `build::application_spec` (the six rules, the request validation, the repair
round, giving up with the problems named, an unreachable LLM told apart from a bad spec); five
end-to-end tests in `tests/design_application_llm_tests.rs`, where the real `LlmActor` posts to a fake
OpenAI-compatible endpoint and only the model's *text* is scripted — a decomposition designed from the
form; a holed one **repaired over HTTP** with the problem named in the second request; the board and the
user's words asserted to be *in* the request that left (via a new `fake_llm_logging` in the shared
harness); the same design reached through `tools/call`, which is the path a model takes; and a chip-less
board refused *before* the model is asked, with a valid answer queued to prove it was never sent. Plus a
build-manager test pinning that `idf_design_application` is advertised with `chip`, `bsp` and
`description` required. `cargo test -p spire-code` green, `cargo fmt -p spire-code --check` clean.

**Still not built.** The wizard's *screen* — the six-field form, the framework presented for
confirmation with its justification, and the spec shown for approval — plus deriving the per-unit fills
from the spec. (What approval writes into the scaffold was the next seam, and the entry below is it.)

## The framework reaches the tree, and the fill model stops choosing (2026-09-26)

**The failure this closes was silent and expensive.** The design phase decided and a person reviewed the
framework — and then the **fill phase never learned it**, so the model filling `main/` read a library's
`SPIRE.md` (which describes *both* frameworks and how to choose between them), chose again, and could
choose differently on the next run. A decision nobody downstream can see is not a decision.

**It is stated in the application's own file**, which is the convention this tree already uses for the
project type and the component kind: the scaffold writes `set(SPIRE_APPLICATION_FRAMEWORK actors)` into
the application's `CMakeLists.txt`, and `application_spec::declared_framework` reads it back. A comment
stating a framework is deliberately **not** a statement (`# set(...)` is a note to a reader), and a value
that is not `actors` or `ramen` is **refused by name** rather than ignored — a file saying something
unreadable is a wrong fact, not a missing one.

**Two sources, in the order the tree exists.** At *plan* time nothing is written yet, so the framework
can only come from the request (`PlanScaffold`/`ScaffoldProject` carry it, alongside the `library` they
already carried, through the same message chain). At *fill* time the application exists, so
`generate_fill_plan` reads the choice back from its `CMakeLists.txt` — which is why `FillProject` needed
no new field at all, and why the fill leg works unattended after a confirm.

**What the model is told is short and specific** (`idf_projects::framework_prompt_block`): that the
choice is made and reviewed, the two or three sentences that make its idiom unmistakable (an actor is a
message type plus the state behind it, spawned on a `Scheduler`; a stage is ports and `>>`, pushing
synchronously, so the chain is a DAG), and — when no choice exists yet — that the library's rule applies
and *where* the answer belongs. It is only in an application's prompt: a library builds against nothing
and states no framework of its own, which the library scaffold already asserts.

**One guard had to be made more precise rather than weaker.** `carries_no_architecture` forbade the bare
token `"ramen"` in any application file, which the new "no framework stated yet" comment trips — it names
the two values a reader may write. The guard's real claim is *this shell adopts no framework*, so it now
checks that by **declaration** (`declared_framework(...) == Ok(None)`) instead of by the word: a comment
naming both legal values is documentation, while `set(SPIRE_APPLICATION_FRAMEWORK …)` is a choice.

**Verified.** The scaffold writes the marker and `declared_framework` reads it back (through the manager,
in `idf_project_routing_tests`, and directly in `idf.rs`'s own scaffold test); a *library* states none, in
both places; the read-back ignores a comment and refuses an unknown name; and the fill prompt is asserted
to carry the `FRAMEWORK: actors` block, the ramen idiom including the DAG rule, the "states none" wording when
there is no choice, the choice read **from the tree** when the caller does not pass one, and **nothing**
for a library. `cargo test -p spire-code` green, `cargo fmt -p spire-code --check` clean.

## The reviewed decomposition reaches the model that writes `main/` (2026-09-26)

**The same failure as the framework, one level down, and worse.** Fixing the framework line stopped the
fill model choosing an *idiom*; it still had no idea **what to build**. The prompt handed it a goal and a
library, and it invented the components, the units, their messages, the wiring and the board's addresses
— so what landed in `main/` was a different application from the one that was reviewed, and nothing in
the round trip said so.

**So the application carries its design.** The scaffold writes the reviewed spec beside its
`CMakeLists.txt` as `SPIRE.application.json` — `SPIRE.md` is a *library's* architecture in prose, this is
an *application's* in a form a tool can check and a model can follow — and the fill phase reads it back.
Which also made the earlier `framework` parameter redundant: the framework is a *projection* of the spec,
so the spec is what travels now (`ScaffoldBuildConfig.application`, `PlanScaffold`, `ScaffoldProject`).
Structural, because it is the reviewed artifact: changing the composition means designing again, not
editing the record of what was designed. The README names it, so a reader who wonders why `main/` has the
shape it has is one file away from the answer.

**What the model is given is rendered, not dumped.** `composition_block` turns the spec into the lines a
person would read:

```
THE DESIGN — reviewed and approved. Write this composition; do not invent another, and do not leave a unit out.
Components (framework-agnostic: a driver is one device on one bus, a library is pure code):
- `sps30` — driver on i2c: PM1/2.5/4/10 readings (to be written in the library)
- `moving_average` — library: a rolling window (already in the library)
Units (the composition itself — this is what `main/` holds):
- `air_quality` — actor on `Reading`, holding the rolling average, uses moving_average, sends to view
Wiring: sampler -> air_quality; air_quality -> view; …
Board facts (the application's own — write them into `main/`, never into a component):
- `sps30` on i2c at 0x69
```

`source` reaches it too ("already in the library" / "to be written in the library"), because a model that
cannot tell which parts exist will re-create them — and a design that names a component the library does
not have yet is useful *only* if the model is told to write it.

**Two sources, the same shape as the framework** — and the same order: the request while planning (the
tree does not exist yet), the tree afterwards. `FillProject` still carries nothing: the fill leg reads the
spec the scaffold wrote, which is what makes a fill after a confirm work unattended.

**A decomposition handed to a creation request is checked, not trusted.** `params_application` parses and
runs the *same six rules* the design phase ran, refusing with the problems listed — because the wizard
hands back whatever was reviewed and possibly edited, and because the project **writes the spec down**: a
tree carrying `SPIRE.application.json` is a tree claiming that composition, so one scaffolded from a spec
that does not hold together carries the lie in its own files.

**Verified.** The scaffold writes the spec, the read-back is the spec that was approved, the file is
structural, the README names it, and a *library* carries no application's decomposition (directly in
`idf.rs` and through the manager in `idf_project_routing_tests`); `composition_block` renders the worked
example whole, says "this application's components are its own" when there are none, and handles the
ramen **source** stage (one port, not two); the fill prompt is asserted to carry the design block, the
per-unit lines, the wiring and the board address, to read the design **from the tree** when the caller
passes none, to fall back to the stated framework line when there is no decomposition, to invent nothing
when there is neither, and to offer a *library* neither block; and an edited decomposition is refused with
its problems named. `cargo test -p spire-code` green, `cargo fmt -p spire-code --check` clean.

**Still not built.** The wizard's *screen* (the six-field form, the framework shown for confirmation, the
spec shown for approval, and the create call that hands the reviewed spec back). (Creating the components
the design names was the next seam, and the entry below is it.)

## The design stops being a document: it writes the library's components (2026-09-26)

**The prompt had been promising this since it was written.** It says a component the library has is
`"source": "existing"` and one that has to be written is `"source": "stub"` — *"and the design generates
the stub, so name it rather than avoiding it"*. Nothing generated anything. A design that names `sps30`
and `sht20` as stubs was a design whose two most important facts had no effect on the tree.

**The reconciliation is three lists, not a boolean.** `application_spec::component_plan` sets a design's
components against what the library has:

| | |
|---|---|
| `add` | the components the design names (as `stub`, or with `source` unstated) that the library does not have |
| `present` | already there — the ordinary case for `existing`, and the idempotent case for a second apply |
| `problems` | the two **disagree**: "the library already has this" when it does not |

The third list is the point, and it is why this is not a loop that fills gaps: a design and a library can
contradict each other, and *which of the two is wrong is a person's call*. Writing the component anyway
would be writing one the design never asked for; ignoring it would leave an application that cannot
build. `has` is a closure rather than a path, so the reconciliation is a pure function of the design and
one fact about the tree — testable with no library on disk.

**One kind, two vocabularies, and the compiler holding them together.** The design's `role`
(`driver`/`library`) decides the skeleton, and the kind is then a fact of the component's own
`CMakeLists.txt` — `idf_projects::ComponentKind`. Rather than casting between two strings, there is an
explicit `From<ComponentRole> for ComponentKind`: they are the same two words on purpose, a third variant
in one of them fails to compile here, and that is the whole reason to write it down.

**`idf_apply_design` is a tool, not a request** — and that is a decision about callers. The wizard already
calls tools (`idf_add_component` is how the UI grows a library today), it needs no LLM, and it needs no
registry hop, so one surface serves both the model and the UI. The decomposition is **checked again**
before a single file is written (the same rules the design phase ran), because the wizard may hand back an
edited one and a stub written from a design that does not hold together is a component nobody asked for.

**A hole this closed on the way.** Two units could share an `id` — and `unit(id)` answered with the first,
so the wiring, `uses`, `sends_to` and the board facts would silently mean the wrong one. Rule 1 now reads
"one framework, **one unit per id**", in the validator and in the prompt.

**Verified.** The reconciliation: nothing in the library (the two drivers, plus the disagreement about
the component the design says exists), everything in the library (nothing to do), and a **library**-role
stub (no bus, which is the difference the role makes). `component_names`: sorted, and a directory without
a manifest is not a component. And through the tool, end to end on a real scaffolded library: `sps30` and
`sht20` written with `SPIRE_COMPONENT_KIND driver` and `esp_driver_i2c` in their manifests, the
disagreement carried through without writing anything for it, a **second apply writing nothing**, and a
holed decomposition refused with no `components/ticker` on disk. `cargo test -p spire-code` green,
`cargo fmt -p spire-code --check` clean.

**Still not built.** The wizard's *screen*. (The one fact the design was missing — *which library* it
designs for — is the entry below.)

## The design is told which library it designs for (2026-09-26)

**An invented fact, one level up from the last two.** Every component in a spec says `"source":
"existing"` or `"source": "stub"`, and that is a claim *about a library* — which the design request
never mentioned. The model was asked to say which parts already exist without being told what the library
has, and the reconciliation added an hour earlier was built to report exactly that kind of disagreement.
A design that guesses is worse than one that refuses, because a guess looks like knowledge.

**The fix is to hand it the facts, read from the tree.** `LibraryFacts { root, components, hints }`, from
`idf_projects::library_facts`: the components the library has (`component_names`) with the kind each one's
own manifest states (`component_kind`), and its `SPIRE.md`. Rendered into the request as a block:

```
## The library this application is built against

`/work/sensors`, and the components it already has:
- `actors` — library
- `sps30` — driver
- `toolkit` — library

Those are `"source": "existing"` — they are here. Anything else you design has to be written, so mark it
`"source": "stub"`, and do not design a component this library already has.

LIBRARY HINTS — how the author of this library says it is meant to be used. It is the architecture, it is
not in the code, and the design has to respect it:
…
```

Two details worth recording:

- **A component that states no kind is reported as exactly that** ("no kind stated"), not as a driver or a
  library. The kind is what decides a skeleton, so a design that has to *know* whether a component is a
  device driver gets an honest unknown rather than an inherited default.
- **"No library named" is stated, not omitted.** It is not the same as an empty one: with no library
  there is nothing for a component to already be in, so *every* component has to be written. Silence there
  would be filled with "the library probably has this", and that application cannot build.

**A wrong directory is refused by name.** `libraryRoot` is read only if its `CMakeLists.txt` states
`idf_library`; anything else is refused rather than read as an empty library, because a design told "this
library has nothing" about the wrong path designs the wrong application *confidently*. The tool's flat
`library` argument is mapped to it by the adapter that already normalizes the tool's arguments into the
request's shape.

**Verified.** The request: the no-library wording, and with facts — the path, each component with its kind
(including "no kind stated"), the `existing` instruction, and the library's hints. `library_facts`: a
scaffolded library reads back its framework components as libraries and `sps30` as a driver, a library
that has written nothing down reports **no** hints rather than an empty string, and its components are
still its own. And end to end, on the wire: a real scaffolded library on disk, `createProject/
DesignApplication` with `libraryRoot`, and the captured HTTP request asserted to carry the path, the
components with their kinds, the instruction and the hints; plus a `libraryRoot` that is not a library
refused **with no request sent at all**. `cargo test -p spire-code` green, `cargo fmt -p spire-code
--check` clean.

**Still not built.** The wizard's *screen* — and it is now the only piece of the loop with no
implementation: design (a request and a tool) → scaffold (a request) → apply the design (a tool) → fill
(a request), each with tests, and no way to drive any of it from the app's UI yet.

## The loop ran, for real — and the live run found three things no test could (2026-09-26)

**The state before this**: design, scaffold, apply-the-design and fill were each implemented and each
tested, with the model scripted everywhere. Nothing had ever been *run* — and the point of a loop is that
its pieces meet.

**So it is now an `#[ignore]`d test**: `a_real_model_designs_builds_and_fills_an_application`, taking the
library to design against (`SPIRE_LIVE_LIBRARY`) and a workspace to create in (`SPIRE_LIVE_WORKSPACE`), and
gated exactly like the live fix run — `--ignored`, and a clean skip with no key configured. The library is
**copied, never written to**, so a run leaves the original alone and can be repeated. It reads the app's
own model config, which needed one thing fixed first: a *test* binary is not named `spire-code`, so
`config_dir()` resolved to `~/.spire/<test-binary>/llm-config.json` and the live tests had been **silently
skipping** — a green run that had done nothing. `SPIRE_APP_NAME=spire-code` is the fix, and both gates now
print the path they looked at.

**The harness was missing two actors, and that is the first thing a creation run finds.** `createProject/*`
resolved against a registry with no `project_creation` in it, so every request answered
`lost: channel closed` — the dummy sender swallowing it. The system harness now spawns the creation actor
and the filesystem it writes through, as `ffi.rs` does.

**Three real defects, each invisible to every scripted test:**

1. **The plan's key was rejected for its *name*.** The model returned
   `{"type": "write_source_file", "path": …, "content": …}` — the most natural spelling — and
   `step_from_value` accepted `step` / `action` / `step_type` / `stepType` but **not `type`**, and looked
   for parameters under `arguments`/`parameters` rather than beside the type. All eight steps of a
   perfectly good plan were discarded, and the error read "the model produced a truncated/unusable plan".
   Fixed: `type` is accepted, the step object is its own parameters when there is no wrapper, and the fill
   prompt now shows the shape instead of hoping for it.
2. **The fill model copied the library's components into `main/`** instead of using them — the same driver
   written twice, free to disagree. The prompt listed the components but never said they were already on
   the include path, which is what makes "use them" actionable. `composition_block` now says
   `` `#include <sps30.hpp>` — do **not** copy a component into `main/` ``.
3. **The application's product file was locked.** `application_scaffold` marked `main/main.cpp`
   structural and gave the application **no fill roots** — so the composition could never be written, and
   the guard refused the model's write with "a locked structural file". That is a stale decision from
   when an application was filled only by component operations. `main/main.cpp` is the product: it is now
   the one fillable file, `main/` is the fill root, and the scaffold test pins exactly that.

**What it produced** (a copy of `~/naturesense/spire/spire-idf`, in a temp workspace):

```
framework: actors — "Human-timescale interactive meter: a ~1 Hz reading, a touch-driven recalibration
                     command, and state held across messages — actors, not a dataflow pipeline."
board_facts: sps30 @ 0x69, sht20 @ 0x40 (i2c)
components:  sps30 (driver, stub), sht20 (driver, stub), rolling_average (library, stub)
units:       sampler → air_quality → view, touch → air_quality, …
```

The library gained exactly those three components as typed stubs; the application stated
`set(SPIRE_APPLICATION_FRAMEWORK actors)` and carried `SPIRE.application.json`; and the fill wrote
**one file per unit** into `main/` — `messages.hpp`, `sampler.hpp`, `air_quality.hpp`, `view.hpp`,
`touch.hpp`, `main.cpp` — with the board facts (both addresses, the pins, the sample period) in `main/`
where a component can never see them, and the composition wired in `app_main` with `spire::Scheduler`.

**Verified.** The live run passes end to end; the new parse case is a unit test
(`parse_fill_steps_handles_the_flat_type_shape`); the product-file/fill-root fix is pinned in the
scaffold's own test; the include-path instruction is pinned in the composition test; and the suite is
green with `cargo fmt -p spire-code --check` clean.

**Still not built.** The wizard's screen — and the first thing it should drive is exactly this run, which
is now proven to work.

## The scaffold had never been read by a builder (2026-09-26)

The loop ran, the suite was green, and every file it produced was asserted against *substrings*. Then a
real `idf.py build` read the generated `CMakeLists.txt` — and found five defects that no assertion in the
suite could see, two of them before a single line of C++ was compiled. The lesson is narrow and worth
keeping: a generated text file's contract is the tool that reads it, and a test that greps for a marker
is not that tool.

**What the build found, in the order cmake found it** (a copy of `spire-idf` and the application of a
live run in `/tmp/spire-app-one`; `idf.py set-target esp32s3 build`, IDF v5.5.5):

1. **A template token survived into the file.** `application_scaffold` *prepended* the framework block to
   the library substitution instead of substituting its own token, so `CMakeLists.txt` line 16 was the
   literal `__FRAMEWORK_BLOCK__`. cmake: `Parse error. Expected "(", got newline`. Every test passed,
   because the marker they asserted (`set(SPIRE_APPLICATION_FRAMEWORK actors)`) was in the file either
   way. Fixed by substituting by name (`FRAMEWORK_BLOCK_TOKEN`), and **guarded**: no scaffolded file may
   contain `__` at all — which is the assertion that would have caught it.
2. **An absolute library path was concatenated onto the application's own.** `EXTRA_COMPONENT_DIRS` was
   `${CMAKE_CURRENT_LIST_DIR}/${SPIRE_LIBRARY_DIR}/components`, correct only for a *relative* library —
   and the wizard, and the live run, name an **absolute** one. The result was
   `/tmp/spire-app-one/pm25-meter/tmp/spire-app-one/library/components`: not a directory, and impossible
   to mistake for one at a glance. Now `if(NOT IS_ABSOLUTE "${SPIRE_LIBRARY_DIR}") … endif()`: the path
   is used as given, and resolved against the application only when it needs to be.
3. **The application's own component declared no `REQUIRES`.** The manifest is structural, so the model
   could not add them, and every `#include <sps30.hpp>` in the composition was unresolvable. The
   scaffold now names them from the reviewed design — the framework's own components first, then the
   design's: `REQUIRES actors toolkit sps30 sht20 rolling_average`.
4. **`SRCS "main.cpp"` could not hold what the model writes.** The fill writes one file per unit; the
   first attempt, `SRCS "*.cpp"`, is taken by IDF as a *literal filename* (`Cannot find source file:
   …/main/*.cpp`); `SRC_DIRS "."` globs but stops one directory deep; and `CONFIGURE_DEPENDS` — the
   obvious way to make a glob stay current — is **invalid in script mode**, which is the mode IDF
   includes a component's `CMakeLists.txt` in to read its requirements (`CONFIGURE_DEPENDS is invalid
   for script and find package modes`). What works is a plain `file(GLOB_RECURSE …)`, with the comment
   saying that a file added *after* the first configure needs a reconfigure.
5. **The composition compiled against nothing but its own includes.** Two errors, both the same mistake:
   `main/actors.hpp: fatal error: spire/actor.hpp: No such file or directory`. The framework's namespace is
   `spire::`; its header is `<actors/actor.hpp>`. Nothing had ever told the model the *path* — the prompt
   named the namespace and the class and left the include to inference.

**The fix for the last one is the same mechanism as the one before it.** `component_apis` — which exists
because a live run wrote `Sps30(i2c_port, address)` and `init()` against a header that says
`Sps30(BusHandle device)` and `probe()` — now renders **the framework's own components first** (from the
library's `components/` tree, by walking `include/**/*.hpp`), then the components the design names, and
every section states the include line it belongs to: ``### `actors/actor.hpp` — include it as
`#include <actors/actor.hpp>` ``. The next live run wrote `<actors/actor.hpp>`, `<actors/scheduler.hpp>`,
`<sps30.hpp>`, `<sht20.hpp>`, `<rolling_average.hpp>` — the real paths — and put `0x69` and `0x40` in
`main/board.hpp`.

**The build then reached the application's own sources** (`[1078/1087]`, the last objects to compile) and
failed with five errors in three files, all of them calls into a **stub**: `RollingAverage` has no member
`push`, it was initialized with an argument it does not take, and a `uint8_t` address was passed where
`BusHandle` is wanted. That is the
boundary and not a defect: a stub's API is a `TODO` by construction ("one method per thing the device
actually does"), and the composition calling it cannot compile until the component's protocol is written —
which is the next piece of work for a real application, and a person's. One wording fix came out of it:
both the driver stub and the component-edit prompt said "pins, ports and addresses are the application's
to supply", which reads as *pass the address* beside a constructor that takes a **handle**. Both now say
the application opens the bus and adds the device at its address, and that what comes back from that is
what the constructor is given.

**How the trees were rebuilt without paying for a model call.**
`scratch_writes_an_application_tree_for_a_person_to_build` (`idf_project_routing_tests`, `#[ignore]`d)
writes a scaffold onto disk; with `SPIRE_SCRATCH_SPEC` pointing at a live run's `SPIRE.application.json`
it re-scaffolds *that* application's tree — same design, same library, fresh manifest — which is how each
cmake error above was chased to the next one.

**One environment note.** IDF's `export.sh` on this machine fails (an unsupported system `ninja` 1.13.2
and a venv activation error), so the build was run with `IDF_PATH`, the venv python and the toolchain
`PATH` set by hand — plus `ESP_ROM_ELF_DIR`, without which every configure prints a gdbinit warning.

## The design phase reaches a screen (2026-09-26)

The wizard's `CreateProjectSheet` created every project type the same way — name, location, library,
`createProject/Scaffold` — so the design phase the Rust side grew had no way in from the UI: an
application was scaffolded blank, and the `createProject/Fill` leg had no caller left at all (the old
two-phase `NewProjectView` is gone). An ESP-IDF **application** now goes through four stages, and the
live loop (`a_real_model_designs_builds_and_fills_an_application`) is the executable specification of
exactly what each one calls.

**The form.** The board (chip and BSP required — a guess at either is a build that fails on hardware —
with an optional abstraction) and the design request's six questions, each its own labelled field. The
labels are load-bearing: the request tells the model to say which question went unanswered instead of
inventing a device, an address or a protocol, and a model handed six labelled answers can see the gap;
blank answers are not sent, so an unanswered question stays visibly unanswered. The framework is
**deliberately not pinned here**: the model chooses it after reading the answers and has to justify it,
and the person confirms it — derived where it can be informed, overridable where it is read.

**The review.** `DesignReviewView` shows what is reviewable and nothing else: the framework **with its
justification and the marker line it will become** (`set(SPIRE_APPLICATION_FRAMEWORK actors)` — the whole
of what the choice turns into in the tree), the components split from the composition with what each unit
*is* (a driver on a bus, a library the library already has, an actor on a message holding state, a stage
that pulls and pushes), the wiring, and the board facts — which the application carries because no
component may. It writes nothing, and it says the one thing a build would say later when the design has
stubs to write and no library to write them into.

**The pipeline.** Approve and create runs the live loop's sequence: `idf_apply_design` (the library gains
the design's components as typed stubs, reported as *added / already there / refused*), then
`createProject/Scaffold` **with the spec**, then `createProject/Fill`, then `createProject/ExecutePlan` —
with each phase named in the sheet, because a scaffold is instant and a fill is a model call and a person
waiting deserves to know which one they are waiting on. Two failures are surfaced rather than swallowed: a
library that refused a component stops before the application is written (scaffolding against a component
nobody has is worse than not scaffolding), and a fill that returns no plan leaves the sheet **open**
saying so, instead of opening an unfinished project as though it were finished.

**What is approved is what is built.** The raw spec JSON that came back is what travels to the scaffold
and the apply tool; the decoded type is for reading only. The Swift model mirrors the Rust spec's fields
(`board_facts`, `sends_to`, `uses`, `pulls`/`pushes`) and keeps optionality, so a field that stopped
decoding cannot quietly review an empty application — pinned by three tests (`designReplyDecodes`,
designFormDescription, boardChoiceRule). `swift test` is 22 tests, green; the Rust suite is untouched at
523.

## The loop builds a chip image (2026-09-26)

The loop ended at "filled" — a tree nobody had compiled. It now ends at a build, and the four things
between the two were all rules the **compiler** asked for, each one a sentence that had never been
written down. `pm25-meter.bin` is 171 KB, and the last live run reports *no compiler errors*.

**What the compiler found, in the order it found it** (each error below is quoted from a real run):

1. **A stub is a boundary, not an API.** `RollingAverage has no member named 'push'`, and a `uint8_t`
   address passed where a `BusHandle` is wanted. A component the design marks `stub` has a header that is
   a shape and a `TODO` — nothing callable — so the composition must leave its calls open instead of
   inventing them. `composition_block` now says so, names which components those are, and states the
   shape that compiles: a `// TODO:` where the call belongs, with the rest of the unit real.
2. **A `TODO` may not stand where a value was needed.** The next run followed that rule too literally:
   `sps30::Sps30 sps30{/* TODO: sps30 bus handle */};` — an empty brace list where an argument belongs,
   which does not compile either. The rule now says what to do instead: declare a zero-initialised value
   of the type the constructor takes (`sps30::BusHandle sps30_handle{};`) and pass it, with the `TODO` on
   the line that will fill it in.
3. **An edge carries the receiver's message, not the sender's.** `no matching function for call to
   'app::TouchInput::TouchInput(spire::ActorRef<app::Reading>&)'` — the model gave an actor whose own
   message is `TouchEvent` a `ActorRef<TouchEvent>` to send to an actor that takes `Reading`. The rule is
   now in the design request (so the designer's edges are chosen knowingly) and in the actors framework
   block (which is what the compiler checks): for `a -> b`, `a` holds `ActorRef<b::Message>`.
4. **A driver's handle does not exist until `init()`.** `no matching function for call to
   'sps30::Sps30::Sps30(<brace-enclosed initializer list>)'` came from a **member** initialised with a
   placeholder; the fix that followed — `sps30_ = sps30::Sps30(handle);` — cannot work either, because a
   component is non-copyable. The framework block now says where a component with a runtime argument
   lives: `std::optional<T>` built with `emplace(...)` in `init()`, never a plain member and never an
   assignment.

**The gate is now in the loop.** The live test builds the application where the machine can:
`SPIRE_LIVE_BUILD_COMMAND` is a shell command run *in the application's directory* (a command rather than
`idf.py`, because this machine's `export.sh` is broken and a person's own invocation is the honest
contract), it prints the compiler's errors, and it asserts the one thing the scaffold owns — that the
application's own component reached the compiler at all. That assertion is the scaffold's contract: a
manifest that collects its sources, names the framework's and the design's components and resolves the
library, which is exactly what was broken while every substring assertion in the suite passed. And when
the gate cannot run it says *where it looked*, because a gate that skips silently looks like a gate that
passed.

**What is left is a repair turn, and the last two errors are its evidence.** The run before the successful
one failed with two mismatches *inside the model's own code* — `main.cpp` spawning `Sampler` and
`Calibration` with constructor arguments their classes do not take. No prompt rule prevents a model from
disagreeing with itself across two files it wrote; only compiling does. The gate prints those errors now;
the next step is to hand them back with the files they name and let the fill try again, bounded.

## A build that fails goes back to the model (2026-09-26)

The loop could build; when the build failed, it stopped and printed. It now repairs and rebuilds, twice at
most, and both paths are verified live: the ordinary run ends `THE BUILD — passed / no compiler errors`,
and a run with `SPIRE_LIVE_BREAK_BUILD=1` — which appends a failing `static_assert` to `main/main.cpp` —
ends `THE BUILD — failed` → `THE REPAIR (pass 1)` → `THE BUILD AFTER REPAIR 1 — passed`.

**Nothing new was written for the repair itself.** The app already had a compile-fix loop — `build/fixPropose`
→ `compile_fix_prompt` → `llm_rewrite`, which parses every proposal with tree-sitter and spends a second
attempt on one that does not parse — and it was built for exactly this. What was missing was the part that
turns *a build's output* into a repair: which file each error belongs to, and which of those a repair is
allowed to touch. That is `createProject/RepairFromBuild`, and it does not write anything itself: it returns
`write_source_file` steps, which the caller executes through `createProject/ExecutePlan` — the same path the
fill's steps take, which is where the structural guard lives.

The permission model is the scaffold's own: `spec.fill_roots`. A diagnostic whose every candidate is a
locked file or one the **library** owns comes back in `unrepaired`, and the gate prints it as "not ours to
fix" — that is a mistake for a person, and saying so is the point. The rewrite path refuses a proposal that
will not parse, so a repair cannot write what the fill could not have.

**Two things a real failing build taught the parser**, both in `group_diagnostics`:

1. **An error is not always in the file that carries the fix.** A live build reported `no matching function
   for call to 'app::Sampler::Sampler(…)'` inside the **library's** `actors/scheduler.hpp` — a header the
   application may not change — with `In file included from …/main/main.cpp:2:` above it and a `note:`
   naming `main/sampler.hpp:23` with the compiler's own explanation (`no known conversion for argument 2
   from 'ActorRef<Reading>' to 'ActorRef<Offsets>'`). So a diagnostic carries its include chain and its
   notes, and the file that *included* the blamed one is the fallback candidate.
2. **But the blamed file wins when it is one a repair may touch**, and the first version had that backwards:
   a syntax error in `main/touch.hpp` — the model wrote `explicit Touch(final spire::ActorRef<Reading> out)`,
   Rust's `final` in a C++ parameter list — arrived under an `In file included from …/main/main.cpp:8:`
   chain, so the repair rewrote `main.cpp` instead and left the broken file exactly as broken. `candidates()`
   now tries the blamed file first and the chain second; the guard test covers the fallback case (an error
   blamed on `scheduler.hpp` whose chain reaches `main/sampler.hpp`).

**Why the gate asserts a clean build, not a build attempt.** The loop's product claim is that a designed,
applied, scaffolded, filled application **builds** — with a repair turn to get there. A gate that stopped at
"it tried" would be the silent-skip failure again, one level up.

**Still to do.** The wizard does not build after its fill yet, so the creation flow ends before this seam:
`createProject/RepairFromBuild` is reachable from the live gate and from any caller with a build's output,
and the next UI step is to build after the fill (`project/build`), and on failure drive this loop behind the
review the app already has (`FixErrorsSheet`, which is built on the same `fixPropose`).

## The wizard builds, and repairs, before it hands the project over (2026-09-26)

The creation sheet ended at "filled" — the same place the loop did before this session — so a person was
handed a tree nobody had compiled, with the fill's own syntax errors still in it. It now builds after the
fill and, when the build fails, puts the compiler's output back to the model through the core's repair
seam, twice at most.

**The build is the app's own.** `BuildService.runTool("build_build", path:language:"cpp", platform:)` —
the same path the dashboard uses, routed to the same module — with the platform taken from the board the
design named: the design form's chip *is* a platform id (`esp32s3`), which is what `platform_os_of` maps to
the `esp-idf` module. Nothing new was built for the build.

**The repair is the core's, unchanged.** `createProject/RepairFromBuild` (added for the live gate) turns the
compiler's output into `write_source_file` steps, and those steps go through `createProject/ExecutePlan` — the
same executor the fill's steps use, which is where the structural guard lives. So a repair cannot write a
file the scaffold locked, whatever the compiler says about it. A rewrite that would not parse is refused
(the core's tree-sitter gate) and an error in a file no repair may touch comes back as `unrepaired`.

**Why two passes.** Errors cascade: a syntax error hides every type error in the file it broke, so the first
pass fixes what the compiler could see and the second fixes what it could see after that. A pass that
proposes nothing ends the loop — asking the same question about the same log has no exit — and what is left
over leaves the sheet **open**, saying the project is created but does not build, and pointing at the
dashboard, where the app's reviewed fix flow (`FixErrorsSheet`, on the same `fixPropose`) can take it. That
split is deliberate and it is the app's own convention: an automatic repair is for the loop's generated
code, and a person's own edits go through a review.

**One wire shape, not two.** `fillProject` and `repairProject` now share `scaffoldSpecJSON` — the
`ScaffoldSpec` as the Rust side reads it (snake_case keys, a `content` on every file). Two hand-built copies
of that dictionary was one edit away from two different contracts with the same guard.

**Verified.** `swift build --build-tests` and `swift test` — 23 tests, the new one
(`repairReplyDecodes`) pinning the reply: the steps decode as the same `CreationStep` the fill uses, and
`refused`/`unrepaired` survive, because they are the part a person acts on. **Not** verified here: the UI's
build-and-repair path cannot be run headlessly in this environment (it needs a running core and a toolchain
in the app's own environment); the core seam it calls is verified live, both ways.

## The `esp-dl` corpus (2026-09-26)

The gap the corpus list had left open: an application that runs a **model** had no corpus for the thing
that runs it. `esp-dl.ingest.yaml` is now in the bundle, with the two repositories a detector task needs
and nothing from either that a C++ application cannot use.

**What it takes, and why — every pattern checked against the real tree** (via the GitHub API, because the
rule in this directory is that a pattern which matches nothing ingests nothing and the failure *looks like
bad retrieval*):

* **the API**, `**/esp-dl/**/*.hpp` — this component has **no `include/`**: its headers sit beside the
  implementations (`esp-dl/dl/base/dl_base_*.hpp`, `esp-dl/vision/{detect,classification,recognition,image}/`,
  `esp-dl/fbs_loader/`), so the header is what a caller includes and the `.cpp` beside it is the one
  neighbour not worth retrieving;
* **how it is driven**, `examples/*/main/app_main.cpp` — the only place the call shape is written down:
  load a model, configure the detector, run it on a frame, read the boxes. The example's `README.md` says
  which model and which chip it is for;
* **the catalogue**, `models/README.md` and the `.md` under it — which prebuilt models exist and what each
  costs on which chip. That is the structured half of *"is there a model for this?"*, and a detector task
  starts there. The models themselves are binary and are not ingested: a chunk of quantised weights is not
  documentation;
* **the constraint**, `operator_support_state.md` — which operators the runtime implements per chip and per
  type. It decides whether a model can run at all and nothing else in the tree states it;
* **the training side** is a second repository in the same corpus, `espressif/esp-detection`: its README and
  `cfg/*.yaml` (what a model needs, how class names are declared), and not its Python — the application is
  C++, and the exporter is a tool a person runs.

**One latent gap in the test, found by adding a second source.** `include_patterns_match_the_real_files_…`
asked only the **first** enabled source of a corpus, which was fine while every corpus had one repository —
and silently would have pinned half of this one. It now asks *every* enabled source whether it claims the
path, which is what ingestion does. New cases use paths verified in the real tree
(`esp-dl/dl/base/dl_base_dotprod.hpp`, `examples/cat_detect/main/app_main.cpp`, `models/README.md`,
`operator_support_state.md`, and the negatives: the `.cpp`, `Kconfig`, `cat.jpg`, an example's
`sdkconfig.defaults.*`, `train.py`).

**Verified.** The four `rag_bundle_manifests` tests pass — the manifest parses, names its corpus, has an
enabled source, every include pattern can match an absolute path, and each case matches the manifest's
behaviour. The suite is 525 / 0 with `cargo fmt --check` clean. Ingest is a `--depth 1` clone, the same as
the esp-idf corpus, so the cost is one working tree rather than a history.

## The live `registry/*` tool (2026-09-26)

The third item on the design phase's list, and the one the next two applications need before they can be
designed at all: `esp_lcd_st7701` and `esp_lcd_touch_gt911` are **published** components, and a design
that cannot look one up will design a second copy of it.

Two tools, in `build/registry.rs`, registered on the shared registry beside `search/*` and reachable by
`tools/call`:

* **`registry/search {query, limit?}`** — components published on components.espressif.com, each described
  as a design reads it: the fully-qualified name, what it is, which chips it targets, its licence, and what
  it pulls in;
* **`registry/component {name, namespace?, versions?}`** — one component, newest versions first, with
  `versions_total` beside `versions_shown` so a caller knows the list was cut.

**Nothing is cached and nothing is vendored**, which is the difference between this and a corpus: a corpus
is documentation, read once and embedded, while a component's version, targets and dependencies **change**.
A design that named a version older than the registry's is a build that fails on the machine that has the
newer one, and no amount of retrieval prevents that.

**Three facts the real payload taught**, each now pinned by a fixture taken from it (`?q=st7701`, and
`/api/components/espressif/esp_lcd_st7701`):

1. an entry states its description, targets, licence and dependencies on its **latest version**, not on the
   entry — a parser reading the top level would describe every component as empty;
2. an `idf` dependency has a **null name** and a version range ("this needs IDF ≥ 5.4"). It is kept apart
   from a component dependency rather than flattened, because calling the IDF a component is a lie a design
   would act on;
3. a **bare name is not always an answer**: `esp_lcd_st7701` is published by Espressif *and* by
   Nicolaielectronics, and a search for `st7701` also returns components that merely mention it. So a name
   resolves only on an **exact** match, and both failures — none, and more than one — are refusals that say
   what they saw. That resolution is a pure function (`resolve`) precisely so those two paths are testable
   without a network.

**Verified.** Three tests offline (the payload shape, the resolution refusals, and that a missing argument
is refused before any network call), one **live gate** — run, not merely written:
`cargo test -p spire-code --lib build::registry -- --ignored` answered the real registry in 3.3 s, found
`espressif/esp_lcd_st7701` with its targets and description, described 3 of its 30 versions, and resolved
`esp_lcd_touch_gt911` by **name alone** — the path a design takes when it knows what the part is called.
The registration is pinned too (`test_build_default_registry_registers_project_meta_tools` now asks for
both tools): a tool that registers without being advertised is a tool the model never sees, which is the
failure that test family exists for. Suite 527 / 0, `cargo fmt --check` clean, no warnings.

**Not done, and deliberately.** The design phase does not *call* these yet: `design_request` is told the
**local** library's facts, and a registry lookup per design would put a second network call in that path
and a rate limit in its way. The next step is a caller that chooses to look — the tool is reachable by the
model now, which is what the applications needed.

## Making the app able to build a chip application (2026-09-26)

The loop builds a chip image, and the wizard builds after its fill — and neither reaches an ESP-IDF when
the app is launched the ordinary way. That was the one thing standing between "the code is right" and
"you can try it": the IDF module runs `idf.py` **by name**, deliberately expecting the environment
`export.sh` sets (idf.rs:103 at the time — *no PATH surgery and no SDK variables*), while `open` hands a
bundle a fresh environment with no IDF in it at all.

> **Superseded in part (2026-10-01).** The module now supplies that environment itself, and only when the
> process has none: `install_for_build` resolves the install and the spec names
> `<venv>/bin/python <IDF_PATH>/tools/idf.py` outright, so a `open`-launched chip build no longer needs
> this wrapper. What the wrapper is for is *choosing* an environment (`SPIRE_IDF_EXPORT`, a particular
> `export.sh`) and running a command in the very environment the app will use. The shim below is now a
> hand-built version of an arrangement the module states directly — see §6f of
> `docs/esp-idf-architecture.md` and the `idf_env_check` / `idf_env_fix` entry.

`build/run-with-idf.sh` and `make run-idf` close that, and nothing about the product changed:

* it resolves an ESP-IDF environment — `SPIRE_IDF_EXPORT` if set, then `$IDF_PATH/export.sh`, then the
  usual install locations, then `~/.espressif/esp-idf/<version>/export.sh`;
* **when no `export.sh` works it synthesises the same environment** from `IDF_PATH` and `IDF_TOOLS_PATH`:
  the venv, the toolchain directories, `ESP_ROM_ELF_DIR`. That is not a second environment, it is what the
  script writes, written by hand because on this machine the script itself refuses (an unsupported system
  `ninja` and a venv that will not activate);
* **when `idf.py` does not run it writes a shim** — one line, early on `PATH` — because the app invokes
  `idf.py` by name and this machine's `idf.py` script has an interpreter without the venv
  (`No module named 'esp_idf_monitor'`, the error the tool-less build was hitting). The shim is that script
  with the venv's python, which is what a working `export.sh` arranges by putting the venv first;
* it **execs the binary rather than `open`**, so the app inherits the environment. Running the executable
  from a terminal is supported (`SpireApp` sets `setActivationPolicy(.regular)` for exactly that), and
  without an IDF at all it still launches and says what it looked for — the module is dev-only, reversible,
  and all of it lives under `build/`.

Two ways to use the environment it resolves, because a wrapper that can only launch is a wrapper you
cannot debug with: `--check` proves it (`idf.py --version`, the chip compiler, and where each came from)
and `-- <command>` runs anything in it.

**Verified, including the part that matters.** `--check` resolves v5.5.5 through the synthesised path with
the shim and finds `xtensa-esp32s3-elf-gcc`. Then a **real build of the loop's own application**:
`run-with-idf.sh -- idf.py build` in `/tmp/spire-app-one/pm25-meter` ended `Project build complete. To
flash, run: …`. That is the same command, by name, in the same shape of environment the app will have.

**And the failure now says so.** The one environmental failure has a documented fix, which is useless if it
reads like twenty lines of shell output ending in a `cmake` error, so `BuildEnvironmentHint` recognises a
missing program (`idf.py: command not found`, the runner's `os error 2`) and appends *"launch it with
`make run-idf`"* to the sheet's failure — while a compiler error is left to be the application's, which is
the common case and pinned by the test (`buildEnvironmentHint`, Swift tests now 24).

## A component can be *published*: the registry is a third source, and the board is a dependency (2026-09-27)

**The gap, named by the applications themselves.** The first three applications do not differ in what they
*write* — each writes one or two own drivers, which is what the library is for — they differ in what they
*depend on*: an M5Stack CoreS3 (`espressif/m5stack_core_s3`, which carries the display, the touch panel, the
IMU and LVGL), a Waveshare ESP32-P4-NANO (`waveshare/esp32_p4_nano`, which carries MIPI-DSI + GT911 + camera
+ uSD + the ES8311 codec), and a YOLOv8n detector that is Espressif's (`espressif/esp-dl`). None of those is
a component this project writes, and until now the design phase had only two words for a component —
`existing` (the library has it) and `stub` (write it) — so a design that needed `esp-dl` either stubbed a
second copy of it or left it out. That is the failure the `registry/*` tools were built to prevent and could
not, because nothing in the spec could *say* a component was already published.

**The third source, and the field that names it.** `UnitSource` gains **`published`**, and `Unit` gains
**`registry`** — the fully-qualified `namespace/name` the registry knows the component by. A published
component is a **managed dependency**, not a component of the library: the scaffold writes it into the
application's manifest, and the library neither owns it nor stubs it. The rules that follow are the ones the
three sources imply and nothing more:

* `published` ⇒ a non-empty `registry`; `existing`/`stub` ⇒ no `registry` (a registry name there would
  promise a dependency that is not one);
* a `published` **driver** needs no `bus` — nothing is written from it, so the design is not asked for a
  fact that goes nowhere (a `stub` driver still needs one, and the test pins the contrast);
* a published component is still referenced like any other (`uses`), so rule 6 is unchanged.

`design_request` teaches the third source, tells the model to **look a component up** with the `registry/*`
tools rather than designing a second copy, and shows `registry` in the schema example. The **insect trap's**
`esp_dl` is the worked example — and its *only* unit: that application is a *video/AI pipeline only*, with no
actuator and no GPIO, and a pipeline's own code is its stages, which are the application's. Its camera is the
**board's** (the BSP carries MIPI-CSI), so by the board paragraph's own rule it is neither a unit nor a board
fact — `capture` reads it through the board.

**The manifest goes in `main/`, and that is the whole design.** `application_scaffold` now emits
**`main/idf_component.yml`** carrying the board's BSP and every published component, at `"*"`. It is *not* at
the project root, and it is not a `REQUIRES` — because the ESP-IDF component manager **injects** a
component's manifest dependencies into that component's `REQUIRES` (into `MANAGED_REQUIRES` first, so the
namespaced build names arrive correctly). So a manifest in `main/` is what lets `main.cpp` include the BSP's
headers *without* `main/CMakeLists.txt` naming them — which it may not, because it is structural. The
convention was read out of the installed IDF (v5.5.5) rather than guessed:
`idf_component_tools.build_system_tools.build_name` and `core.inject_requirements` (`ITERABLE_PROPS`), and
the managed component's directory is `namespace__name` (`sources/fetcher.py` — `build_name(component.name)`),
which is exactly the name `requires_block` has no business guessing. So `requires_block` now **skips**
published components — naming one there would be a `REQUIRES` that does not resolve — and the library is
still reached as a plain directory, deliberately: only the third-party board support is managed, because it
is the one part with no local copy to reach for.

**Nothing is stubbed that is not written.** `ComponentPlan` gains a `published` list beside
`add`/`present`/`problems`; `component_plan` handles `published` *before* `has`, so a local component of the
same name is a stated **conflict** rather than a reason to skip the dependency. `idf_apply_design` reports
them apart (`published` in its reply), and the composition prompt no longer calls a published component "no
API yet" — it has one, upstream, and is excluded from the stub list and from the include-path example.

**The wizard names the library by default.** `CreateProjectSheet` opens an ESP-IDF **application** with the
shared library already chosen (`~/naturesense/spire/spire-idf`, where this machine's framework and drivers
live), still editable, and `nil` for every other type — the ordinary application *does* build on that
library, and the outcome the field exists to avoid is an application growing its own copy of the framework
and the drivers.

**Verified.** `build::application_spec` and `build::idf_projects` extend by 9 tests: the three sources
validate; a `published` with no `registry` is refused; a local source carrying a `registry` name is refused;
a published driver needs no bus while a stub one does; `component_plan` puts published in its own list and
states the name collision as a problem; the design request names the third source, `registry` and
`components.espressif.com`; the scaffold's `main/idf_component.yml` carries the BSP + `espressif/esp-dl` and
*not* the library, while an application with no dependencies carries no manifest; `requires_block` leaves the
published component out; and `idf_apply_design`'s report puts it apart from the stubs. The two live-loop
filters now treat a published component as built-by-the-manifest rather than by the library. Swift: the
design decode test carries a `published` unit through `componentsFromRegistry` and its review line, and
`defaultLibrary(for:)` is pinned for an application versus every other type (25 tests).

**Corrected in passing: the insect trap has no actuator and no units of its own.** The worked example had
grown two fictions — a `trap` driver (a solenoid on `gpio`) and a `camera` component marked `existing`. It is
a **video/AI pipeline only**: a camera, the ESP-DL detector, the stages between them, and a last one that
**saves an image** of each detection. Nothing drives a pin, so the solenoid is gone; and the camera is the
**board's** (the BSP carries it), so the board paragraph's own rule applies — *neither a unit nor a board
fact*, with `capture` reading it through the board. What is left is one component (`esp_dl`) and five stages,
the last of them (`save_image`) a sink. (The GPIO gap the moment exposed is separate and still real:
`add_component` scaffolds `i2c`/`spi`/`uart` only, so a device that genuinely *is* on a pin — a relay, a
solenoid — still has no skeleton.)

## The loop ran again — the board, a warning, and a design the fill ignored (2026-09-28)

A real run — the wizard, `~/naturesense/spire/spire-idf` as the library, an M5Stack Core S3 — produced a
project. The pipeline itself was correct: the design phase returned a decomposition, `idf_apply_design` added
three components to the library, the scaffold wrote seven files, and the fill executed seven steps without an
error. Three things were wrong anyway, and only the last of them was the model's.

**The board is a given, and the model replaced it.** The form said `espressif/m5stack_core_s3` and
`m5unified`; the spec came back with `bsp: "m5stack/cores3"` and `hal: "esp-idf"`. That spec is written to
disk and read by the scaffold, so `main/idf_component.yml` carried a registry name nobody chose — a
dependency that does not resolve, in the one file that decides whether the application can be built at all.
The six rules are about the **composition**; the board is not part of what the model is being asked to
decide. So `design_application` now **overwrites** `spec.board` with the caller's board once the answer has
passed validation (`with_the_callers_board`), and the design request says so in words as well — "The board
above is **given**: echo it in `board` exactly. Substituting a different BSP or abstraction is not a design
choice". The answer's decomposition is still the answer's.

**A build whose output names no `error:` line answered in a shape the caller could not decode.**
`createProject/RepairFromBuild`'s early return — "there is nothing to repair" — answered
`{steps, diagnostics, note}`, while the far end decodes a repair as `{steps, diagnostics, refused,
unrepaired, next}`. So a build that produced only warnings became *"the repair could not run: Key 'refused'
not found"*, and that took the whole build→repair→rebuild verify down with it: the failure read like a
broken application and was a missing JSON key. Both answers are built by one function now,
`CoordinatorActor::repair_reply`, so the shape cannot drift again — and the "nothing to repair" reply keeps
`next`, which is where the reason belongs.

**And the fill wrote a flat application.** This is the one the report was about. Everything the model needed
was in the prompt — the reviewed composition, `FRAMEWORK: actors` with its idiom, the library's own headers
through `component_apis` — and it wrote one FreeRTOS poll loop with `xTaskCreate`, plus its own
`class Sps30`/`class Sht20` in `main/sensors.h`, instead of `spire::Actor<Message>` on a `spire::Scheduler`
and the library's `sps30.hpp`. Nothing downstream noticed, because **a flat loop compiles**.

Three changes, then, and none of them is a prompt alone:

* **The prompt says it twice, and where each rule has to live to be unconditional.** `composition_rules` is a
  new block in the fill prompt, beside the composition it applies to — "**THE COMPOSITION IS THE
  ARCHITECTURE** — implement it, do not approximate it", with the two things the run did named as
  prohibitions (no bare `while (true)` task and no `xTaskCreate` per unit; no class of your own for a
  component the design names). The same loop prohibition is in `framework_prompt_block(actors)`, because that
  block is what an application is told when **no design phase ran** — the wizard chose the framework and the
  line is in its `CMakeLists.txt` — so a rule that lived only beside the composition would be missing
  exactly there.
* **The composition is checked, not assumed.** `createProject/VerifyApplication` reads the application's
  `main/` and answers `{ok, gaps}`: the framework's own markers are absent, or a component the **library**
  owns is re-declared there (`class Sps30`, in three spellings — the id, its capitalisation and its upper
  case). It is deliberately not a compiler and never claims a composition is *right*, only that it is not
  obviously *absent*; an actor whose wiring is wrong is the build's business and the reader's.
* **And `CreateProjectSheet` stops on a gap.** The tree is on disk, the failure names what is missing, and
  the sheet stays at review instead of building, dismissing and handing over an application nobody designed.
  A second fill is *not* attempted automatically, deliberately: the first fill's extra files are not
  overwritten by the second, so `main/` would end up holding two copies of the same driver. The repair has to
  be a person's, or a fresh run's.

**Verified.** The board pin is pinned twice — at the unit level (`design_application` given an answer whose
board is `m5stack/cores3` returns the caller's `m5stack_core_s3`, with the answer's eight units intact) and
end to end through the real actor against a fake model (`design_application_llm_tests`: the model cannot
change the board it was given). The repair shape is pinned on **both sides of the wire**: `actor_tests` sends
`createProject/RepairFromBuild` a warning-only log through the real coordinator and asserts all five keys,
and the Swift side decodes the empty repair (`anEmptyRepairDecodes`; 27 UI tests). `composition_gaps` has its
own test — a flat `main/` (a poll loop plus `class Sps30`) reports the framework and the re-declared
component, a written one reports nothing, and a `main/` with no sources says so — and `verify_application` is
tested through the request it parses: `ok: false` with exactly one gap, and a missing design, a missing tree
and something that is not a spec are **errors rather than passes**. The fill prompt's new rules are pinned in
the test that already read the prompt, including that they are *absent* when there is no design, and
`framework_prompt_block`'s loop prohibition has its own test. And the gate was run against the real project:
`createProject/VerifyApplication` over `~/naturesense/spire/pm25-reader` — the tree that flat fill actually
produced — reports exactly its three gaps (no `spire::Actor`/`spire::Scheduler`, `class Sps30`,
`class Sht20`). The live loop (`a_real_model_designs_builds_and_fills_an_application`) now ends on the gate
rather than on the file having been written, because every assertion it already had passes for a `main/` that
names each component and then writes its own — which is what the run did.

**One test was asserting a race, and is fixed.** `integration_tests::test_multiple_sequential_requests`
accepted `running` or `initializing` for `system/status`; `SystemState` has never had a `Running` variant
(`initializing` → `ready`), so what it actually tested was whether this machine finished starting up before
the second request — and it failed on a loaded one. It accepts `ready` now, which is the state the enum
reaches.

## The fill was never shown the API — and that is why there were no actors (2026-09-28)

The prompt hardening and the composition gate from the entry above both landed, and the next live run produced
**the same flat application again**: `main/sensors/sensors.h` (`class SensorHub`), `main/ui/ui.h`
(`class Display`), and a `main/main.cpp` that is one `while (true)` with `vTaskDelay`. The gate caught it —
the run stopped at review instead of handing over the wrong tree — but catching it is not the fix.

**It was not the model's willingness, and not the prompt's wording.** `component_apis` is the block that
renders the library's **actual headers** — `components/actors/include/actors/actor.hpp`, `actor_ref.hpp`,
`mailbox.hpp`, `scheduler.hpp`, `toolkit/task.hpp`, and every component the design names — and
`actors/actor.hpp` carries a complete worked example:

```cpp
class Counter final : public spire::Actor<Add> {
protected:
    void on_message(const Add& message) override { total_ += message.amount; }
};
… 
spire::Scheduler scheduler;
auto counter = scheduler.spawn<Add, Counter>("counter", 4096, 5, tskNO_AFFINITY, total);
scheduler.start();
counter.send(Add{3});
scheduler.stop();
```

But that block is only built when the fill is **given the library**, and `SpireBridge.fillProject` never sent
it: `createProject/Scaffold` was passed `embeddedRoot` and the fill was not. So the core's `library_root` was
`None`, `component_apis` and `library_hints` came back empty, and the fill prompt was the reviewed composition
and the prose idiom with **no API at all** — `prompt_len=2981`, against the design prompt's 13741. A model that
has never seen `spire::Actor`/`spire::Scheduler` writes the only embedded code it knows. Everything downstream
was already wired: the core's `handle_create_project_fill` reads `embeddedRoot`, and `generate_fill_plan`
already spends it on `component_apis` and on the library's `SPIRE.md`.

**The fix is one call site.** `fillProject` now takes `libraryRoot` and sends it as `embeddedRoot` when it is
non-empty, and `CreateProjectSheet.create()` passes the library it already holds. The prompt the model is asked
with contains the framework's own headers, the worked actor example, and the components' constructors.

**And the test that would have caught it.** `the_librarys_hints_and_the_design_reach_the_fill_prompt` now
builds a library **with headers** (a component counts only with a manifest, the rule IDF applies) and asserts
that a fill given both a design *and* a library carries `#include <actors/actor.hpp>` with `spire`, and
`#include <sps30.hpp>` with the driver's own constructor — and that a fill with **no** library carries neither,
so nothing is invented to fill the gap. That is the contract the Swift caller has to honour, pinned at the
level a Rust test can reach. The bridge also logs the gaps themselves now, so the next time the sheet stops on
them the log says why without anything being copied out of the UI.

**Two things that were *not* bugs, and cost time to be sure of.** The board this run wrote down —
`"chip": "esp32c3"`, `"bsp": "m5stack/cores3"`, no `hal` — is **the form's own input written back verbatim**:
the board pin from the entry above is in the shipped dylib (its symbol is there, along with the design
request's "The board above is **given**" line), so the pipeline is doing exactly what it was fixed to do, and
the `Chip`/`BSP` fields simply held those values. And the "hung" UI is the sheet **stopping at review with the
failure** — the gate doing its job; the process was asleep at 0% CPU, not deadlocked.

**Verified.** The new assertions pass (the test above), the core suite is green, and the app is rebuilt and
relaunched with the library now reaching the fill.

**Open, and worth watching on the next run.** The fill's output budget is `max_tokens=4096` and its RULES cap
the plan at ~8 steps with ~1500-character contents. A four-actor composition — two drivers, the actor classes,
the scheduler wiring in `main.cpp` — is nearer that ceiling than the flat version was (3303 characters, one
file), so a truncated plan is the next thing to look for if the fill comes back with the composition only
partly written.

## The spec arrived without `structure`, and that is what emptied the prompt (2026-09-28)

The library fix above landed, and the next run's fill request **did** carry `embeddedRoot` — and the prompt was
still 2976 characters, and `main/` was still a flat FreeRTOS loop. Digging one layer further found the actual
cause, and it is a single dropped field.

**Every application block in the fill prompt is gated on one word.** `generate_fill_plan` reads the framework
block, the reviewed composition, the composition rules and the library's headers only when
`spec.structure == ProjectStructure::IdfApplication`; with anything else the prompt is the goal, the structural
contract and the generic RULES. `ScaffoldSpec.structure` carries `#[serde(default)]`, so a spec that arrives
**without** it deserializes as `native` — and the wizard's `scaffoldSpecJSON`, hand-built because the wire shape
is not the Swift shape, listed five keys and not six. It had never sent `structure`.

So the two live runs that asked for `actors` were handed a prompt with **no framework, no composition, no
composition rules and no API** — 2976 characters against the design request's ~13,800 — and the model did
exactly what such a prompt asks: it wrote a plausible freestanding application, `SensorHub` and `Display`
classes and one `while (true)`.

**Two fixes, because there are two ways to lose it.** The wizard sends `structure` (and `embedded`) now: the
field round-trips verbatim, and the value is the core's own key decoded from the scaffold response. And the
core no longer takes the field's word for it: `SPIRE.application.json` is written *by* the IDF application
scaffold, so a tree carrying one **is** an application whatever the spec says. `generate_fill_plan` computes
`planned_structure` from both — the file is the fact, the field is the claim — and every gate in the prompt
reads that. A `native` project with nothing in the tree stays native, which is the test's other half.

**Verified.** The fill-prompt test now pins all of it: a library built with real headers makes the prompt carry
`#include <actors/actor.hpp>` with `spire` and `#include <sps30.hpp>` with the driver's own constructor; a spec
whose `structure` was set to `native` still gets the framework, the design, the composition rules and the
headers **when the tree carries the design**; and a `native` field with nothing in the tree gets no composition
at all. The core suite is green, and the app is rebuilt and relaunched.

**What to look for now.** The fill's `prompt_len` in `~/.spire/spire-code/logs/spire-ui.log` should jump from
~3,000 to the tens of thousands — that is the composition and the library's headers arriving — and `main/`
should carry `spire::Actor` subclasses wired by a `spire::Scheduler`.

**The lesson, stated plainly.** Two of the three faults in this file's last three entries were *one field not
travelling*: `embeddedRoot` was not sent to the fill, and then `structure` was not sent at all. The prompt is
assembled from gates, and a gate that silently evaluates false looks exactly like a model that will not
cooperate — which is why the count of what was *in* the prompt (`prompt_len`, and now the gaps themselves in
the log) is worth reading before the model is blamed.

## The board is typed by hand, and a typo in it silenced the whole build (2026-09-28)

The first run that produced a real composition — `0 gap(s)`, a 32,013-character fill prompt — also produced
`main/sampler.cpp` calling `sps30_->read(...)`, `sht20_->read(...)` and `main/air_quality.cpp` calling
`pm25_average_.push()`: none of which the library declares. Its `sps30`, `sht20` and `rolling_average` are
**stubs** — `probe()`, `run()` and nothing else — so a fill told to *use* them had no read API to call and
invented one. That is a real prompt problem, and the composition rules now say the move: *use only the methods
the headers declare — a component whose header offers only what a stub offers has **no read API yet**, so
write the `TODO` where the call belongs, exactly as the bus opening is left open.* The header was the whole API,
and it was in the prompt.

**But that is not what the run showed, and this is the part worth keeping.** The build never reached the
compiler. `build/` held a component-manager counter and nothing else, `RepairFromBuild` answered *"0 rewrites
for 0 errors"*, and the sheet said the compiler's errors were "in files this repair may not rewrite" — naming a
cause and printing nothing. The cause was the board. The form had been typed by hand as
`"bsp": "espressif/m5stack_core_s3,"` — with a trailing comma — and the BSP is the *key* of a dependency in the
application's `main/idf_component.yml`:

```yaml
dependencies:
  espressif/m5stack_core_s3,: "*"
```

A key the component manager cannot resolve. So it failed the build, produced output with no `error:` line in
it, and every defect in `main/` stayed invisible behind "nothing to repair".

**Three fixes, and one of them is an observation still open.**

* **The board's names are validated where they are typed.** `board_from_json` refused a missing chip or BSP and
  nothing else; it now refuses what *cannot be a name* — a comma, a space, a stray slash, an empty half — for
  both the chip and the BSP, and the message carries the consequence ("this becomes a key in the application's
  `main/idf_component.yml`"). Deliberately **not** "must be namespaced": the board catalogue stores the BSP as
  its SDK writes it (`m5stack_core_s3`) and the registry form adds the namespace, so refusing either would
  refuse a board. And deliberately not a check that the board *exists* — `m5stack/cores3` is well formed and is
  not a component, and telling those apart needs the registry, which is a picker's job.
* **A build that fails before the compiler now shows its own output.** `buildAndRepair` printed the "files this
  repair may not rewrite" line whenever the repair found no work — which is exactly wrong when it found no work
  *because there were no diagnostics at all*. With `diagnostics == 0` the sheet shows the build's own text, so
  the manifest error is the message rather than a guess at something else.
* **And the observation stands as the real fix, deferred by choice.** Entering the board by hand is the defect:
  three free-text fields, no validation, and one error in each of three runs (`m5stack/cores3` from the model,
  `esp32c3`, and the trailing comma). All of it is typed where it could be *chosen*:
  `~/.spire/spire-code/boards/*.yaml` already carries `id`, `name`, `chip`, `bsp`, the pins and the library
  hints, the platform store already loads them, and the app already receives them as `Platform` models. So the
  change is a **picker** over that catalogue in the design form — filling chip/bsp/hal — with the free-text
  escape hatch kept for a board the catalogue does not have, behind the validation above. The one decision it
  forces: the catalogue's `bsp` is the SDK's name and the manifest needs the registry's, so the picker has to
  emit the namespaced form (or the manifest writer has to namespace it), otherwise "discovered" would reproduce
  the ambiguity instead of removing it.

**Verified.** `a_board_whose_names_are_not_names_is_refused` pins both directions: a comma, a space, a trailing
slash, an empty half and a malformed chip are each refused by name, while `m5stack_core_s3` and
`espressif/m5stack_core_s3` are both accepted — and `m5stack/cores3` is accepted too, because shape is not
existence. The fill-prompt test asserts the stub rule reaches the prompt. The app is rebuilt and relaunched.

## The board is *chosen* now, not typed (2026-09-29)

The observation above landed. The design form's three free-text fields are a cascade over the catalogue the app
already had: **Processor → Board**, and the board brings its chip and its BSP with it.

**What the data decided.** The processor list is `chips/*.yaml` filtered to `os == "esp-idf"` (a Linux SBC's
processor is not one an ESP-IDF application runs on); the boards are `boards/*.yaml` filtered by their own
`chip:`, which is the same spelling as the chip's `id` — so the filter *is* the join. The app already reads
both as `Platform`s over `platforms/list`, which carries `kind`, `os`, `chip` and `bsp`, so nothing new was
needed on the wire.

**And what it refused to invent: a HAL level.** There is no `hal:` in any chip or board YAML — the vendor-HAL
concept was retired in the C++/ESP-IDF migration, and under IDF the HAL *is* IDF — so a HAL pulldown would be
an empty set. The field is gone from the IDF form (it stays in the spec, unread, for a future non-IDF world).

**Two decisions worth naming.** The BSP became **optional** in both directions: `board_from_json` used to
refuse a board without one, and the Swift `isComplete` used to require one — which is exactly how a board that
has none ended up hand-typed. Nine boards in ten have no published BSP, an empty one means *Spire generates
its own backend*, and the manifest already skipped an empty BSP; now the form, the core and the manifest agree.
And the catalogue's one BSP is now written **namespaced** (`espressif/m5stack_core_s3`), because the value is
the *key* of a dependency in the application's `main/idf_component.yml` — the trailing-comma bug from the entry
above was the same string in the same place.

**The escape hatch.** "Custom board…" brings the three text fields back for a board the catalogue does not
have, and every value still passes `board_from_json`'s name check. If the catalogue fetch fails (an older core,
no platforms seeded) the text fields are what appear — a picker with nothing in it would be worse than a form.

**Verified.** 397 core tests pass (release — the debug cache no longer fits on this machine), including the
relaxed board rule; 27 UI tests pass, including the rewritten `boardChoiceRule` (a chip alone is now a board).
The board YAML change is live: startup logs `platform + capability graph seeded (18 entries)`, and
`platforms_listing` serialises the full `Platform`, so `platforms/list` hands the picker `kind`/`os`/`chip`/`bsp`.
App rebuilt and relaunched.

**Left open, deliberately.** Surfacing a board's `pins:` into the design and the fill prompt, so `board.hpp` is
written from the board's measured facts instead of the GPIO21/22 the model guessed — the data is already in
`platforms/config`'s capability blocks. And the ESP32-S3-Touch-LCD-2.8B still has no BSP to point a picker at.

## The disk was never Rust's fault — but Rust was making it worse (2026-09-29)

Every build of this session ended in `No space left on device`, and freeing space only bought a few minutes.
The cause is not in this repo: the machine's APFS container (494 GB) holds **23 Time Machine local snapshots**
— one per hour since the previous day — with the Time Machine destination a **network share**
(`smb://tmuser@10.0.10.201/TimeMachine`) that is evidently not draining them. A local snapshot is reported as
"free" while holding the blocks of everything changed since, so `df` swings between 300 MB and 12 GB depending
on what macOS is willing to release — which is exactly the oscillation that was killing rustc mid-link.

The cycle fed itself: the builds wrote and deleted multi-gigabyte `target/` trees *every hour*, and each hourly
snapshot pinned the blocks of what the builds deleted. So the artifacts were not really freed — they were kept
by a snapshot that would not be offloaded. (Thinning the snapshots needs `sudo tmutil thinlocalsnapshots / …`,
which is the operator's call, not the code's.)

What this repo *could* fix is how much space it asks for. A default `cargo test` was **4.6 GB** — Cargo's dev
profile emits full DWARF plus split debuginfo — so a build needed more headroom than the machine had, and the
failure left partial artifacts that made the next build redo more work. `[profile.dev] debug = false,
incremental = false` in the workspace manifest makes a debug build **1.8 GB, measured** — a 61% cut — which
fits beside the 2.3 GB release tree that `make app` needs. `line-tables-only` was measured in between (3.5 GB)
and rejected: `du` showed the bulk is the dependencies' rlibs, each carrying debuginfo of its own, so line
tables alone do not free enough. A test failure still prints the test's name, the assertion's values and a
symbolised backtrace; the source line is the thing given up, and on this machine that is the right trade.

**Verified.** 397 core tests pass in **both** profiles with the new settings — debug at 1.8 GB (from 4.6 GB)
and release, which is unaffected by `[profile.dev]`. Still reclaimable if wanted: the `esp` rustup toolchain
(1.6 GB, vestigial since the C++/ESP-IDF migration) and `spire-target-mcp/target` (540 MB).

## A fetched dependency tree was analyzed as the project — and the analyzer is in the wrong crate (2026-09-29)

**The hang.** A run that succeeded end to end — `0 gap(s)`, a build that finally fetched the M5Stack BSP — then
hung the UI at `project/open`. The cause was not the UI. The project analysis walked **`managed_components/`**,
the directory the ESP-IDF component manager writes fetched dependencies into, and a BSP brings a tree with it:
`esp_lvgl_port`, `esp_cam_sensor`, `usb_host_uvc`, `esp_video`, each with `examples/*/main` and `test_apps/*`,
every one carrying its own `CMakeLists.txt`. So a single open enumerated *hundreds* of "subprojects" and a file
tree to match, and handed all of it to the UI to render.

`spire-core`'s analyzer skips known non-project directories through `SKIP_DIRS` — `node_modules`, `target`,
`build`, `.venv` … — and `managed_components` was missing from it. It is the C++/ESP-IDF equivalent of
`node_modules`: downloaded, regenerable, not the user's source. Fixed by adding it: one line, and it covers both
the build-file walk (`discover_build_files`) and the file tree (`should_skip`), plus a classification entry in
`tree_builder` so it is *typed* as `dependencies` in the one place that labels directory kinds.

**And the analyzer should not be in `spire-core` at all.** It is there by accretion, not by design. `spire-core`
is the *original core-process* library — MCP client, JSON-RPC transport, knowledge graph, embeddings, "runs as
VS Code extension subprocess" — while `spire-code` is the desktop app. The `analyzer` module holds only the
generic halves (`scanner`: walk and classify; `tree_builder`: assemble a `DirectoryNode`), and the
project-specific orchestration, `ProjectAnalyzerActor`, already lives in `spire-code`. But **nothing in
`spire-core`'s own actor/MCP/graph code uses the analyzer** — its only consumers are `spire-code`'s `ffi.rs`,
`subsystems/project/project_analyzer.rs` and `project_sync.rs`. By usage it is app infrastructure wearing a core
crate's name.

To keep module separation clean it belongs in `spire-code`'s project subsystem (e.g.
`crates/spire-code/src/subsystems/project/analyzer/`). It is mechanical — `spire_core::analyzer::` is referenced
at about five sites across three files — and it is deliberately **not** bundled into the fix above: a refactor
mixed into a bug fix widens the blast radius for nothing. `spire-core` has accumulated a few other app-only
modules the same way (`build_types`, `config`, `models`), and they deserve the same question separately.

## The device's own bytes: a corpus keyed by part number, and a second lookup in the seam (2026-09-29)

**The failure this is for is not a missing answer but a wrong one.** `sht20` on I²C. `esp-idf-lib` carries
`sht3x` and `sht4x` and no SHT2x at all, so asking that corpus for the part returns the *nearest other
device* — `sht3x`, whose commands are `0x2400` to measure and `0xE000` to fetch, against SHT2x's
`0xF3`/`0xF5` and its bare read — with a score high enough to look like an answer. Depth fails the same
way: even for a device the library *does* carry, the bytes live in `components/*/*.c`, which the
`esp-idf-lib` manifest excludes on purpose (one driver's bit-twiddling ranked beside another's is the
near-miss the model would then have to sort out), and the header that *is* ingested gives the API shape
and the family, never the bytes.

**So the corpus is documents, not code**: one file per part number, named for it
(`resources/device-facts/sht20.md`), and asked for by it. A document carries the commands, the framing,
the reply shape, the checksum, what a bad reply looks like — and **its provenance**. `sht20.md` says it is
derived from `esp-idf-lib`'s `si7021.c` (BSD-3) and is "an implementation, not the datasheet", with a file
and line for every claim. A fact with a source can be checked; a fact without one is indistinguishable
from a guess, which is the one thing a protocol cannot be built on.

**One document is one chunk, deliberately.** `chunk_size: 3000` against documents of 2,975 (`sht20.md`)
and 2,968 bytes (`sht30.md`) — and the manifest says so in those words, "the budget, not a target". A
protocol
split in half is not a smaller answer but a wrong one — a model handed only the framing section writes a
driver with no command words, and one handed only the command table writes one with no checksum, neither
able to tell it was given half. So the manifest's budget and the shipped sizes are compared in a test
rather than left to the chunker: `tests/rag_bundle_manifests.rs` installs the bundle into a temp directory
and fails if a document outgrows the budget, if the file is not on disk byte-for-byte what the bundle
carries, if the manifest's `**/*.md` glob does not match where the installer *put* it, or if the document
never names the part its file name claims.

**There is no repository to clone, so the documents ship with the binary.** The manifest's source is
`type: local, path: docs` — **relative on purpose**, so it resolves against the manifest's own directory
(`<store>/device-facts/`) and the install stays relocatable. Which documents ship is
`rag_bundle::FACTS_DOCS`, an explicit `include_str!` list that `install_into` writes to
`<store>/device-facts/docs/`: a file dropped into the source directory without an entry there is not
shipped, so what the corpus can answer is reviewable in the tree at review time. The format is documented
in the manifest rather than in a `README.md` beside the documents, because everything matching the
patterns is ingested — and a format note retrievable as if it were a part is a part that does not exist.

**Two corpora, two questions, asked together.** `component_device_lookups` replaces the single
`component_knowledge_query` and returns both: the **part number alone** for `device-facts` (what *are* the
bytes) and the device in prose for `esp-idf-lib` (has somebody written it, and how is it driven). The
coordinator asks them in sequence and returns a `ComponentReference` — the markdown *and* which corpora
answered, because the failure this exists for was silent, so "the facts answered and the precedents did
not" has to be a different report from "neither did". Each section is introduced by a `ReferenceRole`
heading — "The device's own protocol, from the device-facts corpus" against "Somebody else's driver for
a comparable device — the shape, not the commands" — and a section that came back empty leaves **no
trace**: a heading with nothing under it does not read as "unavailable", it reads as "the facts are:
nothing", which is how a driver gets written from a label. A UI-selected corpus (`domain`) still replaces
the pair with that one corpus, and the query follows the corpus — `device-facts` is asked by part number,
anything else in prose.

**The prompt has to say which is which, and headings alone are not enough.** The reference block now tells
the model that the facts section is where the bytes come from and the precedent gives the shape, that a
comparable part's command word "looks right and is wrong", and what to do when only a precedent came
back: write no command word, no register address and no checksum, and say which fact was missing. That is
the same rule the user's silence already carried, applied to the case where retrieval *did* return
something — because a near-miss driver is more dangerous than an empty section, not less.

**Verified.** `cargo test -p spire-code --lib`: **404** tests pass (403 before), plus the five manifest
tests; fmt clean, no new clippy warnings. The live fill against a throwaway store
(`SPIRE_KNOWLEDGE_DIR=/tmp/spire-facts-fill SPIRE_FILL_CORPUS=device-facts`, so the app's store is not
touched) installed the bundle, ingested `device-facts [local] 1 files, 1 chunks` — the whole document as
**one** chunk, which is the design intent measured rather than assumed — and retrieval answered with
`docs/sht20.md`. The app's own store (`~/.spire/spire-code/knowledge`) gains the manifest on its next
`rag/install-bundle-manifests`, and the ingest after it is local: no clone, no network.

**The facts were audited against the file they cite, not trusted.** Every `si7021.c`/`si7021.h`
citation in `sht20.md` was resolved against upstream `master` — the same source the code itself is
derived from — and the four that did not point where they claimed now do: the reset and the
CRC-reject landed one to two lines short of the call, the raw value was cited at the checksum's line,
and the user register was described as CRC-checked when this driver reads one byte and checks
nothing. A corpus of *checkable* claims is only worth more than a model's guess if someone checks
them, and the doc now names the revision whose numbering it uses so the next reader can repeat it.

**Two follow-ups closed the same day.** The record above left two things untested and one wrong, and all
three are done:

* **A near-miss second document.** `resources/device-facts/sht30.md` (2,968 bytes, one chunk) is the same
  family, the same no-clock-stretching framing and the same CRC *polynomial* — and disagrees about the
  address (`0x44`/`0x45`, where SHT2x answers at `0x40`), the command words (`0x2400` measure, `0xE000`
  fetch), the framing (the fetch is a *command*, not a bare read) and the checksum's *seed* (`0xff`, where
  SHT2x starts at 0). Every citation was resolved against `components/sht3x/sht3x.c`/`.h` on upstream
  `master` — which is also what corrected this entry's own claim about `0x2C06`: that word is in the
  datasheet, not in this driver, so the driver's pair is what is stated now, in the entry above and in the
  manifest's comment.
  `tests/rag_fill_tests.rs` pins the keyed lookup against the **real** model: for `sht20` the scores are
  0.049 (its own document) against 0.047 (the near miss), and for `sht30` 0.093 against 0.051. Both queries
  answer with the right part — and the margin on one of them is the finding worth keeping: two documents
  this close are separated by the key *and by a hair*, which is why the seam labels every chunk with its
  source path instead of trusting the order. The test prints both scores on every run, so the margin is
  visible rather than inferred from a pass, and a change to either document has to be re-measured.
* **The seam on the wire.** `tests/idf_component_edit_tests.rs` drives `idf_component_edit` with a real
  coordinator, a real `RagActor` over a temp store with the corpus installed and ingested, a real
  `LlmActor` posting to a recording fake endpoint, and the real gate — the component under edit is
  scaffolded by this tool's own `add_component`, so `cmake` configures and builds its host test exactly as
  it would for a user (the model's answer is scripted as `NONE`, because the *question* is the subject).
  What only the wire can show is asserted: the prompt carries the part's own protocol (`0xF3`, and
  `0x988000` from the document's **last** section, so the whole document arrived and not a truncated
  half) under the facts heading, the part's document is presented *before* its near miss, and a `domain`
  override that cannot answer leaves **no trace** rather than an empty heading. It needs `cmake`, `ctest`
  and the embedding model, says so, and skips without them.
* **The examples were reading the wrong store.** `spire-core`'s `rag_domain_check` and `rag_ingest_check`
  built `~/.spire/knowledge` by hand — the *pre-scope* layout, which no current install has. Both now use
  `spire_core::config::knowledge_dir()`, so they follow `~/.spire/<app>/knowledge` and
  `SPIRE_KNOWLEDGE_DIR` like the app and the fill test do: the difference between a diagnostic that reads
  the store and one that reports "nothing found" about a directory nothing writes.

**Still open.** The app's own store (`~/.spire/spire-code/knowledge`) has the corpus from an earlier
install but not `sht30.md`, and not this session's ingest: the app was **running**, and its store is taken
read-write by one process at a time. So the second document reaches that store on the next
`rag/install-bundle-manifests` — or on a live fill with `SPIRE_FILL_CORPUS=device-facts` — and until then
the running app can answer a `sht20` edit from the facts but a `sht30` one only from `esp-idf-lib`'s
nearest device, which is the failure this whole entry exists to remove.




## The scaffold kept the tree's design, and the wizard told nobody (2026-09-30)

**A rule that discards a person's input has to say so, and this one said it to nobody.**
`design_for_scaffold` resolves a design the way `design_in_force` does: the tree's when
`composition.spire` is already there — a design changes by editing that file — and the caller's only
for a tree that states none. So a scaffold can write a project from a decomposition the wizard's
review step never displayed, and the only signal was a log line reading "Scaffolded". Nothing about
the tree is wrong when that happens, which is why no test could catch it: the failure is a *person*
told the wrong thing about their own project, and a wrong belief has no compile error.

**The resolver reports, and the report rides on the spec.** `design_for_scaffold` now returns
`ScaffoldDesign { spec, dropped }` rather than a bare spec: `dropped` is `Some` only when a
caller-supplied design was actually discarded, and its sentence names the file that decided
(`composition.spire`), so the notice is actionable rather than a shrug. `ScaffoldSpec` gained
`design_warning: Option<String>`, set at every site that resolves a design — the plan leg and the
write leg both, because both go through the one resolver and a plan and the tree it becomes must not
disagree about what was dropped. It is `#[serde(default, skip_serializing_if = "Option::is_none")]`,
so a run with nothing to say is byte-for-byte what it was before the field existed: a field bolted
onto an existing wire is optional in **both** directions or every older reader sees a change.

**The sheet shows the core's sentence, not one of its own.** `CreateProjectSheet` gained one
`@State designWarning`, set from `bridge.scaffoldSpec?.designWarning` when the scaffold returns and
cleared wherever the form is re-entered or the run restarted — a stale notice is worse than none. It
is drawn below the log in amber (*not* red: nothing failed and the pipeline carries on), selectable so
it can be quoted, and it is also a line in the creation log, which is the plain account of what the
run did. The wording is the core's, verbatim: the rule is the core's, so the sentence stating it is
too, and a sheet that re-worded it would be the second author of a decision it does not make.

**Verified.** `build_manager::tests::a_scaffold_report_crosses_the_wire_under_the_key_the_wizard_reads`
pins the wire, because a `decodeIfPresent` client fails *silently* on a renamed key — the banner would
simply never appear, which is the exact silence being removed: the key is `design_warning`, absent and
not `null` when there is nothing to say, and a payload from a core older than the field decodes as
`None`. `idf_projects::tests::a_scaffold_resolves_the_design_the_tree_carries` keeps the two answers
apart — the tree's spec *and* a report when the tree carries a composition, the caller's and `None`
when it carries none. `project_creation::tests::a_scaffold_leaves_an_authored_composition_alone_and_states_the_record_from_it` asserts it through the real write: the `REQUIRES` line comes from the tree's composition **and** the spec the wizard reads carries the sentence naming that file. Swift's `aScaffoldReportDecodesAndReEncodes` decodes the reported shape and the quiet one (no key at all) and re-encodes both, because the round trip a fill makes must not turn a warning into a `null` or a `null` into a warning. Suites green: `cargo test -p spire-code` and `swift test` (34 tests).

## The ESP-IDF environment is the app's to test and fix (2026-10-01)

**"Install the missing tools by hand" is not a product.** The build module contributes `IDF_TARGET`
and expects `export.sh` to have run — right for the *product*, since Spire references an install this
machine has rather than inventing one — but it left the app unable to answer the one question a failed
chip build actually raises: is the application wrong, or is there no working ESP-IDF here? Every note
in this file that said "install them by hand" (`xtensa-esp-elf-gdb`, `riscv32-esp-elf-gdb`,
`openocd-esp32`) was a manual step standing in for a feature. And the failure on this machine is the
awkward kind: not an absent install but one that is **present and unusable**. `~/.espressif/tools/` has
no debug tools, `export.sh` treats "no installed versions" as fatal, and so activation aborts on tools
that neither a build nor a USB flash ever invokes — a broken environment with a working toolchain.

**Two tools, and neither needs a model.** `build/idf_env.rs` is the app's own doctor, exposed as
`idf_env_check` (read-only) and `idf_env_fix` (installs what is missing, then tests again). They are
handled by `BuildManagerActor::call_tool` beside the `build_*` actions, so they reach a caller through
`tools/call` → `ToolRouter` exactly as those do; the dashed method names `idf-env/check` and
`idf-env/fix` forward to the same handlers, because unlike `idf-component/edit` and
`idf-design/application` there is no planning leg to give the LLM actor. A fact about the machine is not
a thing to ask a model about.

**Found by listing, not by deriving — which is the bug this machine had twice.** `resolve_idf_install`
mirrors `run-with-idf.sh`'s order (`SPIRE_IDF_EXPORT`, then `IDF_PATH`, then the newest
`<tools>/esp-idf/<version>`, with an `IDF_PATH` that is not an install skipped rather than trusted) and
finds the venv **by listing** `python_env/idf*_py*_env`: the derived name is what broke here
(`idf5.5_py3.14_env`, not the `py3.12` `export.sh` computes from whatever `python3` is first on `PATH`),
and a name that is *found* cannot drift from a name that is *guessed*. The environment is then
synthesised in Rust — venv python, the five toolchain bin dirs on `PATH`, `ESP_ROM_ELF_DIR` — the same
fallback the launch script writes by hand, because `idf.py --version` through the venv's own python
needs no shell and no shim to prove it runs.

**The report says which of the two failures it is.** `idf_py_runs` (the build's question) and
`export_sh_works` (the shell's) are separate fields, `missing` names what IDF could not find, and
`success` is `idf_py_runs` — not `missing.is_empty()`. Conflating them would send a person to repair an
environment that already builds.

**And the repair found the bug that a single check hides.** `idf_tools.py check` and `idf_tools.py
export` **disagree**, and the disagreement is what this machine's environment actually turned on:
`check` accepts a tool found only in `PATH` (`openocd-esp32` at `0.12.0`), while `export` — the step
`export.sh` itself runs — demands one installed in the tools directory, says so (`ERROR: tool
openocd-esp32 has no installed versions`) and returns non-zero, at which point `export.sh` gives up.
Reading only `check` produced the worst possible report: an environment called *complete* whose
activation script still failed, found by running the repair and watching `export_sh_works` stay
`false`. `missing` is now the union of the two parsers, so the install list is exactly what activation
needs; the `PATH` versions IDF refuses (`cmake` 4.3.1, `ninja` 1.13.2, `esp-clang` unknown) go to
`unsupported`, since they explain why the system copies are not the ones used rather than call for an
install. The check parser reads both its `ERROR:` line and its per-tool blocks, so a tool with no
version in `PATH` *or* in the tools directory is caught even if another IDF words the summary
differently — `openocd-esp32` found in `PATH` is correctly *not* named by `check`.

**Verified, and measured on the machine.** `build/idf_env.rs` unit tests hold both parsers against
fixtures of this machine's real output (`check`: two gdb tools missing, `openocd-esp32` present in
`PATH`; `export`: `openocd-esp32` refused, four unsupported `PATH` versions), the missing-tool case with
no `ERROR:` line, the unsupported-`PATH`-version case reported apart from it, the merge naming each tool
once, the mini-glob, and the resolution over a temporary install tree (newest install wins, venv listed,
a bogus `IDF_PATH` skipped, an `export.sh` hint selecting its own install) plus the synthesised
environment's keys and the toolchain's place at the front of `PATH`.
`build_tool_registration_tests.rs` and `build_manager::tests::ui_build_actions_are_registered_tools`
assert both names are advertised and resolvable, which is the failure mode that made Clean and Lint
unreachable once before. Two `#[ignore]`d live tests run against the real `~/.espressif`: the doctor, and
the repair, which asserts the user-facing outcome rather than the installer's exit code. The repair ran
here and **fixed this machine**: `xtensa-esp-elf-gdb`, `riscv32-esp-elf-gdb`, `esp-clang-libs` and
`openocd-esp32` installed, after which a fresh shell sourcing `~/.espressif/esp-idf/v5.5.5/export.sh`
reports `idf.py --version` as `ESP-IDF v5.5.5-dirty` — the environment this file twice described
installing by hand.

**And the app now supplies the environment it needs, instead of asking the person to.** The doctor
answered "is there a working ESP-IDF here?" for the *tool*; the build still ran `idf.py` **by name** and
expected `export.sh` to have been sourced, so a bundle opened with `open` — the way an app is normally
started — inherited a minimal environment and a chip build failed there for a reason that had nothing to
do with the application. That was the last thing `build/run-with-idf.sh` was load-bearing for. The rule
is now **conditional**, and the conditional is the design
(`spec_from_idf_plan_on_this_machine`, `install_for_build`, `is_exported_for`; §6f of
`docs/esp-idf-architecture.md`):

* `None` from `install_for_build` is the ordinary answer on a developer's machine, and it requires **both
  halves** of one arrangement: the install's own `IDF_PATH` **and** its venv already on `PATH`. Both,
  because `tools/idf.py`'s shebang is `#!/usr/bin/env python`, so exporting the path is how `idf.py`
  reaches the interpreter with IDF's packages — an `IDF_PATH` with no venv arranged is the half-built
  environment the doctor exists for, and adding the venv is a repair rather than a takeover. An
  environment a person exported is never replaced.
* With an install in hand the spec **states the environment** (`IDF_PATH`, `IDF_TOOLS_PATH`,
  `IDF_PYTHON_ENV_PATH`, `ESP_ROM_ELF_DIR`, the toolchain directories) with the plan's entries **last**,
  so `IDF_TARGET` stays the platform's fact rather than the machine's — the one thing that must not be
  movable by a stray export.
* And it **names the program**: `<venv>/bin/python <IDF_PATH>/tools/idf.py`, which is what runs the
  install the doctor resolved rather than whichever `idf.py` is first on `PATH`. `run-with-idf.sh` had to
  write a shim *file* to achieve the same thing; two absolute paths cannot outlive their usefulness and
  shadow a fixed install the way a shim can, and `run-with-idf.sh` is now for *choosing* an environment
  (and for `-- <cmd>` in the very environment the app will use), not for making a build possible at all.
* `BuildEnvironmentHint` moved with it: a `command not found` / `os error 2` failure now means this
  **machine** has no usable ESP-IDF rather than that the app was launched wrong, so the hint names
  `idf_env_check` / `idf_env_fix` first and `make run-idf` second (`SpireUITests` pins both).

**Verified by building it, which is the only way.** `a_chip_build_runs_on_the_injected_environment`
(`idf.rs:759`) writes a minimal ESP-IDF project into a temporary directory and builds it for `esp32s3`
through the same `BuildSpec` path a build takes, run with nothing exported at all:

```sh
env -u IDF_PATH -u IDF_TOOLS_PATH -u IDF_PYTHON_ENV_PATH -u SPIRE_IDF_EXPORT \
    cargo test -p spire-code --lib a_chip_build_runs_on_the_injected_environment -- --ignored --nocapture
```

It printed `would add: true` (so this process had no environment and the module supplied one), ran
`~/.espressif/python_env/idf5.5_py3.14_env/bin/python ~/.espressif/esp-idf/v5.5.5/tools/idf.py build`,
and finished `[1074/1074] … Generated build/spire_env_probe.bin · Project build complete` in 141.75s.
That is the claim, measured rather than argued: a chip build works from a plain `open`. The unit tests
around it pin the shape — the venv's python and `tools/idf.py` named, `IDF_TARGET` stated exactly once,
the plan's entries winning the merge, an install with no venv still stating its environment while falling
back to `idf.py` by name, and `None` leaving the spec byte-for-byte what it always was.

## A unit is a component: the actor and the stage stop living in `main/` (2026-10-01)

**The composition was one directory of headers that included one another.** `main/` held every actor — its
message, its state, its mailbox and its task — and every ramen stage, so the only place a build error could
point was `main/`, and each new unit made that worse. An actor *is* an ESP-IDF component, and so is a ramen
stage (what it pulls, what it pushes and the code between), so `unit_component_files` now writes one
`components/<unit>/` per unit and leaves `main/` what is actually the application's: the spawns, the wiring,
the pump and the board facts. `REQUIRES` moves with it — a unit's dependencies are its component's to
state, not `main/`'s — and so does the blame.

**The types cannot belong to either end of an edge**, which is what `components/messages/` is for. A sender
holds `spire::ActorRef<Receiver::Message>`, so a message type belongs to the *receiver*, and in the worked
example `Tick` is two actors' message — a type living in either actor's component would be one the other
could not name. One framework over, the value on a ramen edge (`a >> b`) is pushed by `a` and pulled by
`b`, so it belongs to the edge. The header is generated from the design: one empty `struct` per name, each
with the units that name it in the comment above it, because the *fields* are a fact about the device, the
protocol or the screen at the other end, and no scaffold knows them.

**A unit arrives as a whole component, not a manifest with a promise.** A build file names the
architecture — which framework, which library components, which fake FreeRTOS, which fake bus — so a unit's
`CMakeLists.txt` and its `test/CMakeLists.txt` are **the tool's** (`structural`), while the class, the
source and the cases are the **fill's**, as stubs: an actor is declared `spire::Actor<messages::Tick>` for
the message the design named, a stage declares `in_<value>`/`out_<value>` ports per edge, and both
**compile as they stand**. That is the shape `add_component` already gave a driver and a library, so a
person who can read `components/sps30/` can read `components/sampler/` — and an actor's host test links the
library's scheduler, its FreeRTOS fake and every wrapped component's fake bus, because a unit that holds a
driver is testable only if that device is scripted the way the component's own test scripts it. Each
component states its kind (`set(SPIRE_COMPONENT_KIND actor)`, `… stage`), which is what `component_kind`
reads back, and the edit path refuses a unit **by name** (`is_library_component` is the same distinction
`add_component` already drew): what a unit needs is the reviewed design, not a device's datasheet.

**The prompt states the file set, not only the rule.** A model told "each actor is a component of its own"
that then finds stubs on disk will either write a second copy of one or leave it as it found it, so
`framework_prompt_block` and `composition_block` now name every file — the header to fill, the source, the
test, the two `CMakeLists.txt`s it may not edit — and where the shared types go, per framework. `hints.md`
says a **unit** is a component of its own, one rule for both frameworks, and §3d of
`docs/esp-idf-architecture.md` states what is structural and what is not, and why.

**Verified, by building it.** 53 tests in `build::idf_projects`, including two new ones: for an actors
design *and* a ramen one, the emitted file set and its `structural` flags, the stubs naming the design's
message and its ports, `main/CMakeLists.txt` naming `messages` and every unit, each unit's harness reaching
the library (framework sources, FreeRTOS fake, every wrapped component's fake bus), and `component_scope`
no longer assuming a flat header path. Then the generated code itself: both worked examples scaffolded onto
disk against a library made of the framework templates, and **every emitted host test configured, built and
run** — `messages` and `sampler` for the PM2.5 meter, `preprocess`, `capture`, `detector` and `save_image`
for the insect trap (an in+out stage, a source and a sink) — 6/6 green with no warnings under `-Wall
-Wextra`, which is what proves the actor stub really starts on a `spire::Scheduler` with the fake FreeRTOS
under it. All 12 component manifests were then parsed by cmake in **script mode** with
`idf_component_register` stubbed out, the way IDF reads a component's requirements. And finally a **chip
build**: a one-actor design (no BSP, so nothing to fetch) scaffolded into a tree, `idf.py set-target
esp32s3 build` from a sourced `v5.5.5` — `Project build complete`, with `counter.cpp`,
`messages_compile_check.cpp`, `main.cpp`, `scheduler.cpp` and `task.cpp` all compiled and not one warning
from the emitted components. `cargo test -p spire-code` green (15 binaries, 587 tests), `cargo fmt --check`
clean.

**Still not built.** The library a live application is built against has to be re-scaffolded, or edited, to
pick up the new `hints.md`: `spire-idf/SPIRE.md` still states the old rule that a ramen stage lives with the
wiring. And the scratch dump (`dump_idf_projects`) still scaffolds an **undesigned** application — one with
no units — so it does not reach this layout; it is worth pointing at a design now that a designed tree is
one `idf.py build` away.


