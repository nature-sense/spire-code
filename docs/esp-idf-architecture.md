# ESP32 architecture: from a form's answers to a flashed board

<!--
  How spire-code takes an ESP32 product from a person's answers to a board that runs it — the
  knowledge it retrieves, the prompts it builds, the code it fills in, the changes it amends, and the
  build that decides whether any of it was real.

  Every claim carries a `file:line` that resolves under `crates/spire-code/`, so this document can be
  checked against the tree and corrected in the same commit as the code it describes.

  The decision the whole design is organised around: **a model is asked for a decomposition, a person
  approves it, and everything after that is checked by a compiler or a test.** The model never gets
  the last word — the build does.
-->

## The thesis

There are two ways to build firmware with an assistant, and only one of them is reviewable. The usual
way asks a model for code, writes it, then asks a second model whether it looks right. spire-code asks
for a **decomposition** first — the board, the units, the wiring, in a small JSON document — and puts
it in front of a person *before* a line of C++ exists. Everything downstream is then either
deterministic (the scaffold), or generated and gated (the fill, the component stubs), or generated and
verified with rollback (the amend loop).

Three properties fall out of that, and recur in every section below:

- **The design is data, not prose.** An `ApplicationSpec` is JSON that a tool validates against six
  rules and a person approves (`src/build/application_spec.rs:244`, `:300`). The application carries it
  in the tree **twice**: `composition.spire`, the YAML a person opens and edits, and
  `SPIRE.application.json` beside it, the record everything else reads — written *from* the composition,
  so what the fill phase writes is the composition that was reviewed rather than one it invented, and the
  two can never state different designs (`src/build/idf_projects.rs`).
- **Every generated file has a gate.** A component's protocol is gated by its *own host test* — plain
  `cmake`/`ctest`, no board, no chip, no IDF — so it is checked in seconds
  (`src/build/idf_projects.rs:1847`). An application adds `idf.py build`; a free-text change adds host
  and (when a board is connected) target tests, with a byte-for-byte rollback on any regression
  (`src/build/modify_code.rs:428`).
- **Every prompt is assembled from facts on disk.** The board, the library's own `SPIRE.md`, the
  component registry and retrieved reference material are the ingredients; the model is told which
  ingredient is the bytes and which is only the shape (`src/build/idf_projects.rs:2001`).

## The two things a project can be

Only two project structures are recognised, and the difference decides which of the phases below even
run:

| | **component library** (`idf_library`) | **application** (`idf_application`) |
|---|---|---|
| Recognised by | `set(SPIRE_PROJECT_STRUCTURE idf_library)` in the root `CMakeLists.txt`, checked by `declares_library` (`src/build/idf_projects.rs:1406`) | `SPIRE.application.json` in the root — `APPLICATION_FILE` (`src/build/idf_projects.rs:86`) |
| Scaffolded by | `library_scaffold` (`src/build/idf_projects.rs:307`) | `application_scaffold` (`src/build/idf_projects.rs:395`) |
| Holds | `components/*`, each one device or algorithm | `main/` — the wiring, the spawns and the board facts — plus the composition's own components: **one per actor**, and the shared message types |
| Its architecture lives in | `SPIRE.md` (`HINTS_FILE`, `src/build/idf_projects.rs:82`), shipped from `templates/esp-idf/hints.md` (`:139`) | `composition.spire` — the reviewed decomposition a person edits — with `SPIRE.application.json` beside it as the record generated from it |
| Fillable at creation | nothing — `fill_roots: Vec::new()` (`:357`); components arrive later as stubs via `add_component` (`:1807`) | `main/` and `components/` — `fill_roots: vec!["main", "components"]` (`:529`) |
| Everything else | structural: "an architecture that a model can edit is an architecture that drifts" (`:301`) | structural: `CMakeLists.txt`, `sdkconfig.defaults`, `main/CMakeLists.txt`, `README.md`, and each emitted `components/<name>/CMakeLists.txt` (the `REQUIRES` the scaffold names from the design) |

The three **framework** components — `toolkit`, `ramen`, `actors` — are *shipped*, not generated and
not vendored, so they are deliberately absent from every scope a model may write: `add_component`
refuses them by name (`src/build/idf_projects.rs:1826`; `FRAMEWORK_COMPONENTS` at `:262`),
`component_scope` returns nothing for them (`:1879`), and `component_edit_request` refuses too
(`:1943`). Upgrading the framework means replacing it in the template, so an edit would live in one
project and nowhere else — and a library scaffolded *before* that change is brought level with the
template by re-materializing it **from the scaffold itself** rather than by copying bytes by hand
(`cargo run -p spire-code --example rematerialize_library -- --dry-run` prints the delta first).

## The pipeline in one picture

```
  form answers ──▶ ┌───────────────────┐   spec (JSON)   ┌────────────┐
  + board          │ createProject/    │ ──────────────▶ │  a person  │
                   │ DesignApplication │                 │  reviews   │
                   └───────────────────┘                 └─────┬──────┘
                          ▲ Planning role                     │ approved
                          │                                   ▼
                     ┌────┴─────┐                    ┌──────────────────┐
                     │ LlmActor │                    │ createProject/   │  deterministic:
                     └────┬─────┘                    │ Scaffold         │  tree + the spec
                          │ Coding role              └────────┬─────────┘
                          │                                   ▼
        ┌─────────────────┴──────────────┐         ┌─────────────────────┐
        │  fill / amend prompts          │◀────────│ main/ + components/ │
        │  (board, SPIRE.md, RAG, header)│         │ are fillable        │
        └─────────────────┬──────────────┘         └─────────────────────┘
                          ▼
        ┌──────────────────────────────────────────┐
        │ gate: host test (cmake/ctest)            │
        │ then: idf.py build   then: idf.py flash  │
        │ a regression rolls back byte-for-byte    │
        └──────────────────────────────────────────┘
```

The tools a caller (the wizard, or the model itself) drives, in order:
`createProject/DesignApplication` (`idf_design_application`), `createProject/Scaffold`,
`createProject/GeneratePlan`, `createProject/ExecutePlan` (or `createProject/Fill`),
`createProject/VerifyApplication`, `createProject/FinalizeManifest`, `createProject/RepairFromBuild`;
then the three editing entries — `idf_apply_design` (write the design's components as stubs),
`idf_component_edit` (fill one component) and `modify_code` (its RPC method is `modify/code`) — and
finally the build/flash path. RAG is reached through `rag/*` and the component registry through
`registry/search` and `registry/component`.

## 1. One LLM actor, two roles, two call shapes

Every model call in the application goes through **one** `LlmActor` with **one** config and client —
the coordinator never owns an HTTP client. A call is a
`LlmMessage::Complete { prompt, role, reply_to }` (`src/actors/coordinator.rs:320`), and the `role` is
the only thing that differs between a design and a rewrite:

- `LlmModelRole::Planning` — the decomposition, and only that (`src/actors/coordinator.rs:6171`).
- `LlmModelRole::Coding` — every whole-file rewrite and every free-text answer (`:322`, `:366`).

Two helpers wrap the send, and the distinction between them is a bug that was actually paid for:

- **`llm_rewrite`** (`src/actors/coordinator.rs:314`) asks for a *whole file* and validates the reply
  as C++ through `cpp_syntax_check` (`src/build/generic_helpers.rs:2896`). A truncated or garbled
  answer is caught before it can be written, and **one retry feeds the syntax errors back** so the
  model can correct itself. It returns `(content, passed_structural_check)`; a caller that writes
  unattended must refuse a `false`.
- **`llm_text`** (`src/actors/coordinator.rs:360`) asks for text with no structural assumption, and
  only strips code fences (`src/build/generic_helpers.rs:2940`). It exists because a **list of paths
  is not C++**: checking it with `llm_rewrite` meant the structural check *always* failed, silently
  burning a retry and handing the caller the **retry's** answer instead of the model's first one. A
  fake-endpoint test caught exactly that, and the comment at `:353` records it. Anything that is not a
  file rewrite uses `llm_text`.

Both helpers are used by `plan_rewrites` below, and the choice of which is per **step**, not per tool.

## 2. RAG: where retrieved context comes from

### The corpora

The bundled manifests are one list in one place — `BUNDLE` in `src/actors/rag_bundle.rs:43` — because
three things read it and none may drift: `rag/install-bundle-manifests`, the manifest-parse test, and
the live ingest test. Two families:

- **Spire corpora** describe how this application works: `spire-core`, `spire-actor`.
- **Embedded corpora** are what generated firmware is written against: `esp-rs-book`, `esp-idf-hal`,
  `esp-idf`, `esp-idf-lib` (ninety-odd device drivers), `esp-bsp`, `m5unified`, `esp-dl` (the
  on-device inference runtime), `waveshare`, `rust`.

and a third entry that is neither, listed last in the same `BUNDLE` array (`:89`):

- **`device-facts`** (`DEVICE_FACTS_CORPUS`, `src/actors/rag_bundle.rs:98`) — one document per **part
  number**, stating that part's *own* protocol: command words, framing, checksum, malformed replies.
  It is the only corpus whose documents ship beside the manifest rather than being fetched by the
  ingest config (`FACTS_DOCS`, `:122`) — `resources/device-facts/sht20.md` and `sht30.md` are compiled
  in with `include_str!`, and a file sitting in the directory without an entry here is **not shipped at
  all** (`:33`). `install_into` writes each manifest to `<store>/<corpus>/ingest.yaml` and the facts
  documents to `<store>/device-facts/docs/<name>` (`:145`).

### Why two corpora are asked together

This is the load-bearing retrieval decision, and `src/actors/rag_bundle.rs:23` states it as the reason
the corpus exists:

> a device the library does not carry is not in it (there is no `sht20` in `esp-idf-lib`, only `sht3x`
> — a different device with different commands), and the bytes live in the `components/*/*.c` files
> those manifests exclude.

So a library corpus can answer *"has somebody written this device, and how is it driven"* but **not**
*"what are the bytes for this part"*. The seam asks two questions to two corpora, and labels which is
which. `component_device_lookups` (`src/build/idf_projects.rs:2078`) derives them from the component's
own code:

- the **part number alone** → `DEVICE_FACTS_DOMAIN` (`"device-facts"`, `:2045`) — keyed by part number,
  one document per part, so the query is the name;
- the **device in prose** → `DRIVER_PRECEDENTS_DOMAIN` (`"esp-idf-lib"`, `:2049`) — a corpus of code,
  matched by description.

A **library** component gets neither: it has no device behind it, so either query would return whatever
shares a word, and prompt noise is worse than a missing section (`:2073`).

### The seam: pre-fetched, not a tool

`retrieve_component_reference` (`src/actors/coordinator.rs:1032`) makes both lookups **before** the
prompt is built, because the edit runs through `run_code_modify`, whose loop is one prompt with no tool
calls — a model that wanted to look the device up could not (`:1012`). Concretely it:

1. asks `component_device_lookups` for the two queries — `None` for a library, which returns
   `ComponentReference::none()` immediately (`:1044`);
2. resolves the RAG actor through the registry and the `RagMessage` channel, ignoring every failure;
3. queries each corpus with `rag_query_corpus` (`:1229`), which sends
   `RagMessage::Query { domain, query, top_k }` and **returns an empty vector on any error** (`:1246`);
4. labels each result set with `reference_heading(role, corpus)` (`src/build/idf_projects.rs:2126`) —
   e.g. *"The device's own protocol, from the device-facts corpus (`device-facts`)"* (`ReferenceRole`,
   `:2102`) — versus the comparable-driver heading;
5. formats them with `format_retrieved_reference` (`:2136`), which **drops any section with no
   non-empty chunk**: a heading with nothing under it does not read as "unavailable", it reads as "the
   facts are: nothing" — which would have the model write a driver from the label alone (`:2132`);
6. reports `answered`, **per corpus**, because the situation this seam was built for was not an error
   but a *wrong answer*, so "the facts answered and the precedents did not" must be distinguishable
   from "neither answered" (`src/actors/coordinator.rs:1096`).

`domain` is an **override that replaces the pair with the one corpus it names** — what keeps a corpus
selectable from the UI — and the query follows the corpus: naming `device-facts` asks by part number,
anything else in prose (`:1023`, `:1057`).

**Best-effort by design** (`:1028`): no actor (the standalone binary), no embedder, an undeliverable
send, a retrieval that errors — every one is *empty rather than a refusal*. A protocol must still be
writable with no corpus. The caller reports which it was; the edit never fails because knowledge was
missing.

### How the retrieved material reaches the prompt

`component_edit_request` (`src/build/idf_projects.rs:1926`) frames the retrieved markdown **once**, so
both the driver and the library prompts carry the same caveat (`:2001`):

- it is *reference, not instruction*: "if it disagrees with the user, the user is right";
- the two kinds of section are **not interchangeable**: *this part's own protocol* is where the command
  words, framing and checksum come from — use its bytes; *somebody else's driver for a comparable
  device* gives only the shape (naming, bus idiom, reporting) — "a comparable part's command word looks
  right and is wrong, so take its idiom and not its commands";
- if no section states this part's own protocol, the model has the shape and **not** the bytes: write
  no command word, register address or checksum it was not given, and say which fact was missing.

Headings alone are not enough — an unlabelled blob of retrieved code invites exactly the copy that must
not happen (`:1996`).

> **Evidence in the tree:** `resources/device-facts/` carries a near-miss pair deliberately
> (`sht20.md` beside `sht30.md` — one family, same CRC polynomial, different addresses, command words
> and framing; `src/actors/rag_bundle.rs:114`). `tests/rag_fill_tests.rs:119` pins that a query for one
> part answers with that part's document rather than its neighbour's *by a small margin* — which is
> exactly why the seam labels every chunk with its source rather than trusting the ranking — and
> `tests/idf_component_edit_tests.rs:284` pins that the neighbour still shows up in a `top_k = 3`
> result and is *labelled and caveated* rather than silently preferred.

### The rest of the RAG surface

`rag/*` is the general interface: `rag/install-bundle-manifests`, `rag/list-domains`,
`rag/list-manifests`, `rag/search`, `rag/find-interfaces`, `rag/set-domain`, `rag/list-sources`,
`rag/ingest-graph-config`, `rag/reingest-graph-config`. Only the component-edit seam uses RAG
*implicitly*; everywhere else it is an explicit tool call the model chooses to make. Note that the
ingest path needs a **real embedder** — `NoopEmbedder` **fails every call on purpose**
(`tests/rag_fill_tests.rs:68`), so there is no keyword fallback to degrade to
(`tests/idf_component_edit_tests.rs:74`).

## 3. The form's answers become a decomposition, then a skeleton

### 3a. `createProject/DesignApplication` — the design phase

The handler is `handle_create_project_design_application` (`src/actors/coordinator.rs:5765`), also
exposed to the model as the tool `idf_design_application` (`:6190`). Three things are refused **before**
the model is ever asked, and each refusal is deliberate:

- **No board → refuse.** `board` is required and never defaulted: "a guessed chip or BSP is a build
  that fails on hardware, and whoever asks this already knows the board" (`:5771`). A chip-less board
  is refused by `board_from_json` (`src/build/application_spec.rs:987`).
- **No answers → refuse.** `description` — the design form's answers — is what the decomposition is
  derived from, so an empty one is an error (`src/actors/coordinator.rs:5787`).
- **A `libraryRoot` that is not a component library → refuse *by name*.** `declares_library`
  (`src/build/idf_projects.rs:1406`) must pass. "A design built on facts read from the wrong directory
  is worse than no design at all" (`src/actors/coordinator.rs:5813`), and the fake-endpoint test
  `a_library_root_that_is_not_a_library_is_refused` asserts the model was **not asked at all**.

Nothing is written to disk and nothing needs to exist yet: the spec is designed *before* the tree it
describes, which is what makes it reviewable — a person says yes to a decomposition rather than to a
pile of generated code (`src/actors/coordinator.rs:5757`).

### 3b. What goes into the design prompt

`design_request` (`src/build/application_spec.rs:728`) is the whole prompt, assembled from sources:

| Block | Source | Where |
|---|---|---|
| **The board** | the caller's `BoardChoice` — chip, BSP, HAL — echoed exactly, with a note that onboard devices (display, camera, IMU) belong to the board, not the units | `:761`, `:765` |
| **The library** | `library_facts` (`src/build/idf_projects.rs:1449`): the components that already exist, each with its kind, plus the library's `SPIRE.md` — which is why `"source": "existing"` is a fact rather than a guess | `:771` |
| **What it has to do** | the form's `description`, then the **six questions** it must answer — (1) what it does, (2) what it **senses** and how fast, (3) what it **acts on**, (4) what it **reacts to over time**, (5) **timing** / what is parallel, (6) what happens **when something is missing**. "If this leaves one of these unanswered, **say so and leave the field empty** — do not invent a device, an address, a pin or a protocol." | `:775`, `:777` |
| **The framework rule** | either the user *chose* it ("not yours to reconsider"), or the model must choose and justify it: human-timescale/interactive → `actors`; machine-timescale/high-rate stages → `ramen`; and "a *stream* is a component's job either way" | `:736` |
| **The spec schema** | the JSON shape: `framework`, `justification`, `board`, `board_facts`, `units`, `wiring` | `:793` |
| **The three unit kinds** | `component` (driver or library — knows no pins, address, task, mailbox or framework), `actor` (a `Message`, `state`, `uses`, `sends_to`), `stage` (`pulls`/`pushes`, a DAG) | `:814` |
| **The six rules the tool checks** | one framework & one unit per id; every actor states a `message` and every stage a `pulls`/`pushes`; every reference resolves and a dataflow has no cycle; every component has a `role` and `driver`/`library`/`published` discipline; every board fact names a driver in this spec; nothing invented and unused | `:837` |
| **Where code comes from** | `"existing"` (the library has it), `"stub"` (it will be written — "name it rather than avoiding it"), `"published"` (it is on the ESP Component Registry — "**look it up** with the `registry/*` tools rather than designing a second copy") | `:854` |

The last line of the prompt is `Respond with **only the JSON object**, and nothing else.` (`:866`).

### 3c. The loop: design, validate, repair

`design_application` (`src/build/application_spec.rs:923`) is the whole loop, with the LLM leg injected
as a closure so it is testable without a model. It runs at most `MAX_DESIGN_ATTEMPTS = 3` (`:897`):

1. send `design_request` through the injected caller — in production the coordinator's
   `design_application` (`src/actors/coordinator.rs:6149`), which routes it through the LLM actor with
   `LlmModelRole::Planning` (`:6171`);
2. `parse_spec` (`:278`) the reply;
3. `validate` (`:300`) it against the six rules;
4. on failure, make a **repair turn**: re-send the request, the previous answer and the *exact*
   problems — no chat history, no state, because the model needs the facts and not the conversation.

After the loop, the **board in the answer is overwritten with the caller's** — the model may not
substitute a BSP. `with_the_callers_board` (`src/build/application_spec.rs:899`) exists because one did:
`espressif/m5stack_core_s3` came back as `m5stack/cores3`, which the scaffold wrote into
`main/idf_component.yml` and the build could not resolve. The reasoning is stated there — the six rules
are about the *composition*, which is what the model is actually being asked to decide; the board is
the part it is not (`:905`). The result is a validated `ApplicationSpec` (`:244`) for a person to
approve.

Because the rules live in `validate` rather than in the prompt, they are provable with a scripted
closure and no model at all — which is why `application_spec`'s own tests can prove the six rules and
the repair turn, while `tests/design_application_llm_tests.rs` proves the *plumbing*: the HTTP request,
the response parse, the coordinator's route, the planning role, the validation and the repair as a
second round trip, with only the model's text scripted. As that file says, what neither can tell you is
whether a real model gives *good* decompositions — that is a quality question for a live endpoint.

### 3d. `createProject/Scaffold` — the skeleton, deterministic

Approval hands the spec to `application_scaffold` (`src/build/idf_projects.rs:395`), and the model is
out of the picture. It renders the templates and adds the records:

- `CMakeLists.txt` — states `set(SPIRE_APPLICATION_FRAMEWORK …)`, read back by `declared_framework`
  (`src/build/application_spec.rs:1060`) so no later reader has to deduce the choice from the shape of
  `main/`;
- `sdkconfig.defaults`, `main/CMakeLists.txt`, `main/main.cpp` — the blank entry point;
- `README.md`, the spec rendered as `composition.spire` (YAML — the file a person opens, and the **first**
  writing of it: a scaffold never renders over a composition that is already there), and re-serialized as
  `SPIRE.application.json` (`structural: true` — the record follows the composition, so changing the
  design means editing the composition, not the record of what was designed);
- one `components/<unit>/` **per unit** — an actor, or a ramen stage — each arriving as a *whole*
  component: a `CMakeLists.txt` and a `test/CMakeLists.txt` that are **structural** (the scaffold names
  `REQUIRES`, the framework, the library and the fakes from the design), and a stub
  `include/<unit>.hpp`, `src/<unit>.cpp` and `test/<unit>_test.cpp` that are the **fill's**
  (`unit_component_files`, `:981`) — the header sits **flat** under `include/`, the one layout every
  component in the tree uses, so an application unit is reached exactly the way a driver or a library is
  (`<unit>.hpp`);
- `components/messages/` — the shared types, a `library`-kind component beside them and split the same
  way: its manifest, its host-test build and its one translation unit (the **compile check**, which
  includes the header and nothing else) are structural, and the header and the cases are the fill's;
- `main/idf_component.yml`, generated by `application_manifest` for the board's BSP and any component
  the design marked `published` (`:1976`); it lives in `main/` deliberately, because a component's own
  manifest is what the IDF component manager injects into that component's `REQUIRES`, so `main.cpp`
  may include the BSP's headers without `main/CMakeLists.txt` naming them.

#### Why each unit is a component of its own

An actor is a message type, the state behind it, a mailbox and a task; a ramen stage is what it pulls, what
it pushes and the code between — and both are what an ESP-IDF component already is. So the scaffold gives
each one `components/<unit>/`, leaves `main/` the wiring, the pump and the board facts, and names the units
in `main/CMakeLists.txt`, which is structural. A unit's own dependencies (the library components it `uses`)
are then that component's `REQUIRES`, not `main/`'s.

It emits the **whole** component rather than only the manifest, for the reason the library's own components
arrive whole: a build file names the architecture — which framework, which library components, which fake
FreeRTOS, which fake bus — so the build files are the tool's and the fill may not invent one, while the
class, the source and the cases are exactly what a `TODO` stands in for. Each stub **compiles as it
stands**, which is what makes a scaffolded tree a starting point rather than a description of one.

The **shared types** are in `components/messages/` — the one component every unit names — and that is not a
convenience: a sender holds `spire::ActorRef<Receiver::Message>`, so the type on an edge belongs to the
**receiver**, and in the worked example `Tick` is the message of two actors. A type that lived in the
sender's or the receiver's component alone would be a type the other could not name. The same holds one
framework over: a ramen stage pushes the value on an edge and the next stage pulls it, so the value belongs
to neither stage's component. The names come from the design in both cases; the fields never do.

The **framework** a unit is written in decides the harness, and nothing else about the layout: an actor's
host test links the framework's sources and the fake FreeRTOS underneath them, while a stage's links no
framework source at all — ramen is a header, and a chain is a chain of calls. `unit_component_files`
returns nothing when the design has no units, and `fill_roots` still names both trees — a composition with
no design phase is written by the fill, and its units land in `components/` the same way.

Rendered above the file lists, both prompt blocks state the layout so the model does not have to infer
it: each framework block carries its own per-unit-component rule — an actor's message, a stage's
`in_<value>`/`out_<value>` ports — and where the shared types go (`framework_prompt_block`, `:2283`), and
the composition block names each destination header beside its message or its edges and its `uses`
(`composition_block`, `:2453`).

Which design those files are rendered from is **not** the caller's to decide: it is the tree's when the
tree already states one, because a design changes by editing `composition.spire`
(`idf_projects::design_for_scaffold`) — the spec the caller is still holding is used only for a tree
that states none. When that file decides and the caller's copy differs, the drop is **reported, not
inferred**: the sentence comes back on the reply (`ScaffoldSpec.design_warning`) and the wizard shows
it, so a decomposition a person reviewed is never silently replaced by another.

Exactly one file is **not** structural:

```rust
structural: path != "main/main.cpp",   // src/build/idf_projects.rs:445
```

"**`main/main.cpp` is the product, not the scaffold.**" The first live run is why this is not a matter
of taste: with the product file locked, the model's plan was executed and every write was refused with
"a locked structural file", leaving a scaffold that could never become an application (`:397`). Hence
`fill_roots: vec!["main", "components"]` (`:529`) — the two trees the composition occupies — with the
warning that an *empty* list means the opposite of what it reads as — the guard treats it as
"everything is writable" (`:453`). The component manifests are structural, so the model writes the
headers and sources and nothing about the architecture.

The **library** scaffold is the mirror image: `library_scaffold` (`:307`) writes the framework
components and `SPIRE.md` structurally with `fill_roots: Vec::new()` (`:357`), because nothing is
fillable at creation — a protocol component arrives later as a stub via `add_component` (`:1807`).

`add_component` writes the stub files from `component_files` (`:1607`) and returns what is *left to
do*, not just what it wrote: for a driver, "the protocol — command framing, checksums, word and field
order — read off the device's datasheet, then proved against a fake bus", tested at
`components/<id>/test` on the host (`:1845`); for a library, "the code — what it computes, its inputs
and its outputs, and the cases where it refuses", pinned by an ordinary unit test (`:1852`). The
manifest, the bus seam and the fake bus are the **tool's**: "a model that rewrote its own bus layer
would be a model whose work nothing could check, and one that edited its test harness could make its own
failures disappear" (`component_scope`, `:1870`).

## 4. Code filling: the skeleton becomes a product

"Filling" is two different jobs with two different prompts, and conflating them is how a project ends
up as neither.

### 4a. Filling an application — `generate_fill_plan`

`generate_fill_plan` (`src/subsystems/project/project_creation.rs:677`) asks the **planning** model for
a JSON list of steps (`write_source_file`, `create_directory`, `declare_dependencies`, `build`, `test`,
`parse_and_validate`, `tool_call`) that it will then execute through `createProject/ExecutePlan`.

The context is the point, and it is all facts read from disk *at plan time* — the tree does not exist
yet, so nothing is read from the scaffold (`:765`):

- **`library_hints`** — the library's `SPIRE.md`, introduced as *"how the library this project builds
  on says it is meant to be used. Its author wrote this down, it is the architecture, and it is not in
  the code. Follow it."* (`:771`). Read from the **library** root for an application, and from the
  project's **own** root for a library (`:766`); a brand-new library has written nothing down, and
  "that is not a gap to paper over — it is the truth about it" (`:761`).
- **`framework_prompt_block`** (`src/build/idf_projects.rs:824`) — the framework's idiom, from the
  decomposition when there is one, and otherwise from the line the application itself states
  (`:806`). A tree that states a framework this build does not know is **refused** rather than planned
  around (`:786`).
- **`composition_block`** (`:973`) — the reviewed decomposition, rendered back into the prompt, plus
  **`component_apis`** (`:923`), the actual headers of the components the composition calls. "A design
  says which components exist; only the headers say what to call on them, and a live run wrote a
  plausible API that did not exist" (`:838`). When an `actors` wiring has a **cycle**, the block says so
  and names the path, and hands over the `spire::Registry` (`*scheduler.registry()`, passed as the
  actor's `const spire::Registry&` constructor argument) — a cycle is the one edge an actor *cannot* wire
  by constructor injection, so it is resolved by name in `init()`. The library makes that structural:
  `Registry` has no public constructor, so a singleton, a static accessor or a stack temporary is a
  **compile error** rather than a silently empty index.
- **`composition_rules`** (`:851`) — three prohibitions, each one a live failure promoted to a rule:
  write every unit in the **framework's own idiom** (for `actors`, `spire::Actor<Message>` +
  `on_message` on a `spire::Scheduler`, with `spire::Registry` resolving — in `init()` — a peer you were
  not handed a ref to — *not* a bare FreeRTOS `while (true)` loop and *not* an
  `xTaskCreate` per unit); the design's components belong to the **library** — include them, and do
  **not** declare your own class in `main/` for a component the design names ("a second copy of the
  same driver is free to disagree with the first"); and **use only the methods the headers declare** —
  a component whose header offers only what a stub offers has no read API yet, so write the `TODO` and
  do not invent `read(...)`.

The framing is emphatic and load-bearing: *"THE COMPOSITION IS THE ARCHITECTURE — implement it, do not
approximate it."* A live run was handed the reviewed `actors` decomposition, the framework's idiom and
the library's own headers, and answered with one flat FreeRTOS poll loop plus its own `Sps30`/`Sht20`
classes (`:849`). The rules and the concision/anti-loop instructions ("a truncated or looping response
is a hard failure") exist because that happened.

Two structural guards sit above all of it:

- **The tree says what it is.** Every application-specific block is gated on
  `structure == IdfApplication`, and because that field is `#[serde(default)]` a hand-built spec can
  silently deserialize as `native`. So `SPIRE.application.json` is also consulted: "the file is the
  fact, the field the claim" (`:697`). This is not hypothetical — it cost two live runs.
- **Filling is atomic.** No LLM → a fatal error, and nothing is scaffolded. "No deterministic template
  fallback is permitted: a new project is only written once a real plan exists" (`:672`).

### 4b. Filling a component — `component_edit_request`

A component's protocol is filled and amended by **one** path, `component_edit_request`
(`src/build/idf_projects.rs:1926`), which builds either `driver_edit_request` (`:2185`) or
`library_edit_request` (`:2267`) from the same four sources:

| Source | Where it comes from | Where |
|---|---|---|
| **The header as it stands** | `components/<id>/include/<id>.hpp`, read verbatim — the API the fill must not change | `:1963` |
| **What the library says** | the library's `SPIRE.md`, introduced as the architecture to follow — or an honest note that nothing is written down yet | `:1952` |
| **What the user knows** | the caller's `instruction` when non-empty; else the retrieved reference material is declared "the only source of facts"; else a kind-specific instruction to **leave the `TODO`s in place** and invent no command word, register address, checksum, algorithm, unit or constant | `:1970` |
| **Reference material** | the labelled, pre-fetched RAG sections — see §2 | `:2001` |

The precedence is deliberate and stated in the code: "the user's own words win; then reference material
— which is a *source of facts*, like the user's words, so its presence is what makes the protocol
writable; and only when there is neither is the model told to invent nothing" (`:1965`).

The **gate** is the component's own host test — `components/<id>/test`, built and run on the host with
no board, no chip and no IDF, so a protocol can be written and checked in seconds rather than in the
minutes an `idf.py build` takes (`src/actors/coordinator.rs:895`). The chip build is the *second,
optional* gate: it catches what a host test cannot (a wrong `REQUIRES`, a link error against the real
IDF driver) and runs only when the caller names a chip. When it does not run, the report says so rather
than implying the code was checked against one (`:999`).

## 5. Code amending: `modify/code` and every path that shares it

`modify/code` (`src/build/modify_code.rs:4`) is the third use of the modify spine — where autofix is
driven by compiler diagnostics and the HAL cascade by a drift report, this one's prompt is the
**user's own words**: "make the sampler emit 200 Hz", "rename this flag", "add a timeout". The shape is
three steps, and the third is what makes the first two safe:

1. **Plan** — the model reads the request and the files in `scope` and returns a complete rewrite per
   file;
2. **Apply and verify** — the project must still build, and the tests that can run must still pass;
3. **Restore** — anything that made the project worse is reverted **byte-for-byte**.

### 5a. Planning is two LLM steps, with two different validators

`plan_rewrites` (`src/actors/coordinator.rs:845`) was written once and shared, because "every
interesting part of it is a *detail*" and a second copy is a second place for those details to be
forgotten:

1. **Which files?** `modify_scope_prompt` (`src/build/generic_helpers.rs:3380`) lists the candidate
   files and asks for one path per line, or the word `NONE`. It is answered with **`llm_text`**, not
   `llm_rewrite` — "this answer is a list of paths, and checking it as C++ made every reply fail the
   structural check" (`src/actors/coordinator.rs:858`). The reply is then filtered through `select_files`
   (`src/build/modify_code.rs:129`): the model chooses from what it was given and *does not get to name
   a path of its own* (`src/actors/coordinator.rs:842`).
2. **Rewrite each file.** `modify_code_prompt` (`src/build/generic_helpers.rs:3356`) asks for the
   **whole file** in one ` ```cpp ` block, with the narrowness that makes a verified change possible:
   "make the smallest change that carries out the request, and keep every other line, signature,
   include and behaviour **byte-identical**: do NOT reformat, do NOT reorder includes, do NOT rename
   anything the request did not name... If the request needs no change in this file, return the file
   exactly as it is." Each reply goes through **`llm_rewrite`**, so the tree-sitter parse is the check
   — and a rewrite that does not parse, is empty, or equals the current file is **skipped, never
   written** (`src/actors/coordinator.rs:882`), because this runs unattended.

A free-text change is a single coherent intent, so the plan is handed to the spine **once** and never
re-derived: "a second round would just re-apply the same rewrites" (`src/build/modify_code.rs:27`).

### 5b. The verify spine and the rollback

`run_code_modify` (`src/build/modify_code.rs:428`) hands the plan to `run_modify_loop` with a
`CodeModifyBackend` (`:63`) — the trait the coordinator implements over the real LLM and build/test
actors, and the tests implement with a script:

| Backend leg | What it is |
|---|---|
| `plan(prompt, scope)` | the rewrites, or `None`/empty — "a failure to show the user, not a silent no-op" (`:64`) |
| `build()` | compile, errors grouped by file |
| `host_tests()` | `None` when there is nothing to run |
| `target_tests()` | `None` when no board is connected — which is exactly what makes a run host-only |

Every file the model proposes is backed up before it is written; `revert` (`:407`) writes the original
bytes back and marks the cached measurement stale. Verification reports **how far it reached**
(`Verified`, `:50`):

- `Verified::WithTarget` — "verified: build, host tests, and target tests" (`:213`);
- `Verified::HostOnly` — build and host tests, plus a caveat, because a run that never reached the
  hardware must not be presented as fully verified (`:215`).

The round is the unit of judgement, not the file: "a half-applied plan is not a meaningful outcome, so
`accept` keeps every file and `reject_round` decides" (`:29`).

### 5c. `idf_component_edit` — the same spine, a different gate

`handle_idf_component_edit` (`src/actors/coordinator.rs:901`) is `modify/code`'s spine with a
component's gate. It reads `root`/`name`/`instruction`, pre-fetches the reference material (§2), builds
`component_edit_request` (§4b) and runs it through `run_code_modify`. What differs is only the gate:
acceptance is the component's own host test rather than the whole application's build. The report names
the reference corpora that answered and whether the chip build ran — because "a run where the facts
answered and the precedents did not is a different situation from one where neither did, and only the
names say which happened" (`:996`).

## 6. Building, flashing, and the loops that repair

### 6a. `idf.py` owns the chip build

`idf_plan` (`src/build/idf.rs:67`) returns `None` for anything that is not an ESP-IDF platform — "what
stops this module claiming a plain CMake project", which must go to `CmakeBuildModule` instead. For an
IDF platform it produces `idf.py build`, with `-DCMAKE_BUILD_TYPE=Release` when the mode is release
(`:72`), in an environment carrying **`IDF_TARGET`** and nothing else (`:78`). No `PATH` surgery and no
SDK variables: "the ESP-IDF environment is what `export.sh` sets... a build that fails because the
environment was never exported fails with IDF's own message, which names it" (`:103`).
`idf_flash_plan` (`:86`) is the same tool over USB — `idf.py -p <port> flash`, with the port optional
because IDF can detect a single attached board and passed through when given, "which is what makes the
operation deterministic on a machine with several devices" (`:84`).

### 6b. The host test is the cheap gate

`cmake` + `ctest`, no board, no chip, no IDF. This is what makes the component loop fast enough to be
worth running on every protocol edit, and it is the same gate `add_component` promises at
`components/<id>/test`.

### 6c. The repair loops

When a gate fails, the failure is fed back rather than thrown away. `verify_generated`
(`src/build/verify_spine.rs:107`) drives a bounded loop — each round sees the errors of the attempt
before it, the budget is a ceiling, and the last failure is reported verbatim. That triple is pinned by
a test named for it, `the_budget_is_a_ceiling_and_the_last_failure_is_reported_verbatim` (`:284`). The
prompts are the narrow ones: `compile_fix_prompt` (`src/build/generic_helpers.rs:3151`) for compile
errors, and `warning_fix_prompt` (`:3406`) for the *safe* warnings only (`deadcode.*`, unused values,
self-assignment), with an explicit instruction to preserve side effects — "a 'dead' store to a volatile
or otherwise observable location must not lose its effect" (`:3404`). The proposal path
`propose_warning_fix_for` (`src/actors/coordinator.rs:385`) refuses a rewrite that failed the
structural check, "because the loop applies it unattended" (`:384`). `createProject/RepairFromBuild`
is the same loop reached from a finished build, and it distinguishes what it could not repair: a
diagnostic blamed on a file outside the fill roots is reported by its compiler line, "so the caller can
show a person exactly what has to be theirs" (`:6120`).

### 6d. Where the model is used, end to end

| Step | Role | Validator / gate | Rewrite shape |
|---|---|---|---|
| Design decomposition | Planning | `validate` (6 rules) + a person | JSON spec |
| Application fill | Coding | `idf.py build`, then `VerifyApplication` | JSON step list |
| Component fill | Coding | the component's own host test | whole-file rewrite |
| `modify/code` | Coding | build + host tests (+ target) | whole-file rewrite |
| Compile/warning fix | Coding | rebuild | whole-file rewrite |

The pattern is uniform: **Planning produces data a tool can check against rules; Coding produces files
a compiler can check.** Nothing in between trusts the model's own judgement of its output.

### 6e. The environment is tested, not assumed

`idf_env_check` / `idf_env_fix` (`src/build/idf_env.rs`) are the app's own diagnosis of the machine it
is running on, and they exist because "the environment was never exported" is not always the whole
story. An install can be **present yet unusable**: `export.sh` aborts when `idf_tools.py check` finds a
required tool missing — on this machine the three *debug* tools, `xtensa-esp-elf-gdb`,
`riscv32-esp-elf-gdb` and `openocd-esp32`, none of which a build or a USB flash invokes. That is a
broken environment with a working toolchain, and the distinction is a fact about the machine, not
something a person should have to derive from `idf.py`'s refusal.

`resolve_idf_install` (`:308`) finds the install the way `build/run-with-idf.sh` does — `SPIRE_IDF_EXPORT`,
then `IDF_PATH`, then the newest `<tools>/esp-idf/<version>` — and finds the venv **by listing**, never by
deriving its name: `idf5.5_py3.14_env` is what the installer wrote, and a name that is *found* cannot
drift from a name that is *guessed*. `doctor_idf_environment` (`:601`) then runs `idf.py --version`
through the venv's own python (so no shell and no shim are needed), asks `export.sh` to activate in its
own subshell, and reads **both** tool commands (`:480`, `:536`): `idf_tools.py check`, whose `ERROR:` line
names the required tools it cannot find and whose per-tool blocks catch a tool with no version anywhere
even when the summary is worded differently; and `idf_tools.py export`, which is what `export.sh` itself
runs.

**The two disagree, and the disagreement is the whole reason both are read.** `check` accepts a tool
found only in `PATH` — on this machine `openocd-esp32` at `0.12.0` — while `export` demands one
*installed in the tools directory*, says so (`ERROR: tool openocd-esp32 has no installed versions`) and
returns non-zero, at which point `export.sh` gives up. Reading only `check` therefore produced the worst
possible report: an environment called complete whose activation script still failed. This was found by
running the repair against the real install and watching `export_sh_works` stay `false` — which is what
that field is for. `missing` is now the union of the two, so the install list is exactly what activation
needs and nothing more; the tools IDF refuses to take from `PATH` (`cmake` 4.3.1, `ninja` 1.13.2,
`esp-clang` unknown) are reported separately in `unsupported`, since they are why the system copies are
not the ones used rather than a reason to install anything.

`repair_idf_environment` (`:822`) installs **exactly** the tools the doctor named — never a blanket
`install all` — with the same synthesised environment, and then doctors the machine again so the report
carries the before and the after. `IdfEnvReport.success` is deliberately `idf_py_runs` rather than
`missing.is_empty()`: a machine can be perfectly **buildable** and still need the repair to become
*exportable*, and a report that conflated the two would send a person to fix the wrong thing. On this
machine the fix is measured: three debug tools plus `openocd-esp32`, after which a fresh shell that
sources `export.sh` gets a working `idf.py`.

Neither tool needs a model, so neither is intercepted by the coordinator for an LLM leg: they travel
`tools/call` → `ToolRouter` → `BuildManagerActor::call_tool` (`build_manager.rs:3724`) like `build_*`.
The dashed method names `idf-env/check` and `idf-env/fix` (`coordinator.rs:1822`) forward to the same
two handlers, so the RPC surface beside the other `idf-*` verbs is complete.

### 6f. The environment is supplied, when the process has none

§6e answers "is there a working ESP-IDF here?". This one answers the question immediately after it: *the
build is about to run, and the process it runs in has no ESP-IDF in its environment at all* — which is
what a bundle started with `open` has, and the gap `build/run-with-idf.sh` filled by hand, with a comment
that said in as many words that the module "does no PATH surgery on purpose". The module still does not.
The rule became **conditional**, and the conditional is the design.

`spec_from_idf_plan` is the plan and nothing else: `idf.py`, its arguments, `IDF_TARGET`. A plan that
changed with whichever machine read it would have stopped being a plan, so the machine enters at
`spec_from_idf_plan_on_this_machine` — the one function `run_idf_build` and `run_idf_flash` both use,
which keeps a tool call, a module message and a test on a single path. What it is handed comes from
`install_for_build`, and **`None` is the ordinary answer on a developer's machine**: an environment a
person exported is theirs, and replacing it would be this module inventing an install rather than
referencing one.

`None` requires *both* halves of one arrangement, which is what `is_exported_for` tests: the install's own
`IDF_PATH`, **and** its venv already on `PATH`. Both, because they are one thing — `tools/idf.py` is a
script whose shebang is `#!/usr/bin/env python`, so putting the venv first is how `idf.py` reaches the
interpreter that has IDF's packages — and on this machine that is exactly what `export.sh` produces:
`which -a idf.py` is `~/.espressif/esp-idf/v5.5.5/tools/idf.py`, and it runs. An `IDF_PATH` with no venv
arranged is the hand-built environment §6e describes; adding the venv is what it was missing, so that case
is a repair rather than a takeover.

With an install in hand the spec stops depending on how the process was started, in two ways. The
environment is stated — `IDF_PATH`, `IDF_TOOLS_PATH`, the venv, `ESP_ROM_ELF_DIR` and the toolchain
directories on `PATH` — with the plan's own entries **last**, so `IDF_TARGET` stays the platform's fact
rather than the machine's. And `idf.py` is *named*: `<venv>/bin/python <IDF_PATH>/tools/idf.py`. Running it
by name means "whatever `python` means", which is precisely what a bare launch cannot answer, and naming
the pair is also what makes the build run the install the doctor resolved rather than whichever one happens
to be first on this `PATH`. That is `export.sh`'s arrangement made explicit — where `run-with-idf.sh` has to
write a shim *file* to get the same effect, with the difference that a shim can outlive its usefulness and
shadow a fixed install, while an absolute pair of paths cannot.

**Verified by building.** "A chip build works on the environment this module injects" cannot be checked
without the toolchain, so `a_chip_build_runs_on_the_injected_environment` writes a minimal ESP-IDF project
into a temporary directory and builds it for `esp32s3` through the same `BuildSpec` path a build takes, then
asserts the firmware was linked. It is `#[ignore]`d — minutes, and a few hundred megabytes — and it is meant
to be run with **nothing** exported, which is the case it is about:

```sh
env -u IDF_PATH -u IDF_TOOLS_PATH -u IDF_PYTHON_ENV_PATH -u SPIRE_IDF_EXPORT \
    cargo test -p spire-code --lib a_chip_build_runs_on_the_injected_environment \
    -- --ignored --nocapture
```

`build/run-with-idf.sh` remains the way to launch with an environment a person *chose*, and the way to build
or flash by hand in the environment the app will use. It is no longer the only way a chip build can work.

## 7. Where every prompt's context comes from

If you want to know *why a model said something*, this is the table to read. Every row is a source that
is read from disk (or the caller) at prompt-assembly time, never from conversation history.

| Source | Read from | Reaches which prompt | Where it is assembled |
|---|---|---|---|
| The board (chip, BSP, HAL) | the caller's `BoardChoice` | design | `src/build/application_spec.rs:761` |
| The board's onboard devices | the board prompt's own note | design | `src/build/application_spec.rs:765` |
| Existing components + library `SPIRE.md` | `<library>/components/*`, `<library>/SPIRE.md` | design | `src/build/idf_projects.rs:1449` |
| The form's answers | the caller's `description` | design | `src/build/application_spec.rs:775` |
| The six questions | the prompt itself, as constraints | design | `src/build/application_spec.rs:777` |
| The six spec rules | `validate` — *not* only prose in the prompt | design + repair turn | `src/build/application_spec.rs:300`, `:837` |
| The spec JSON schema | the prompt, hand-written beside the parser | design | `src/build/application_spec.rs:793` |
| Framework choice/justification | the caller's `framework`, else the model decides and justifies | design, then fill | `:736`, `src/build/idf_projects.rs:824` |
| The reviewed decomposition | `ApplicationSpec` | application fill | `src/build/idf_projects.rs:973` |
| The components' real headers | `<library>/components/<id>/include/<id>.hpp` | application fill | `src/build/idf_projects.rs:923` |
| The library's `SPIRE.md` | the library root (application) or own root (library) | fill (both), component edit | `src/subsystems/project/project_creation.rs:766`, `src/build/idf_projects.rs:1952` |
| The component's own header | `components/<id>/include/<id>.hpp` | component edit | `src/build/idf_projects.rs:1963` |
| The user's instruction | the caller's `instruction` | component edit | `src/build/idf_projects.rs:1970` |
| The device's own protocol | `device-facts` corpus, queried by **part number** | component edit | `src/actors/coordinator.rs:1032` → `:1229` |
| A comparable driver's shape | `esp-idf-lib` corpus, queried **in prose** | component edit | same |
| The candidate file list | `scope`, filtered by `select_files` | `modify/code` scope turn | `src/build/generic_helpers.rs:3380`, `src/build/modify_code.rs:129` |
| The current file contents | read at plan time, byte-for-byte | `modify/code` rewrite turn, compile/warning fixes | `src/build/generic_helpers.rs:3356`, `:3151`, `:3406` |

Note what is **absent**: there is no chat history, no tool-call transcript and no memory in any of these
prompts. A design or a rewrite is recomputed from facts each time, which is what makes the repair turn
("here is your answer and the exact problems") sufficient on its own.

## 8. One trace, end to end: a PM2.5 meter

*Illustrative but mechanically accurate — each step is the code path named, in the order it runs.*

1. **A person fills the form.** Goal: "log PM2.5 and temperature every minute and show the reading."
   Board: `m5stack-core-s3` (chip `esp32s3`, BSP `m5stack_core_s3`), against a library root that
   `declares_library` recognises. No framework is pinned.
2. **Design.** `idf_design_application` refuses nothing (board ✓, description ✓, library ✓) and asks the
   **Planning** model. The prompt is `design_request`: board echoed, the library's existing components,
   the six questions, the schema, the six rules. The model answers with a spec naming a `pm_sensor`
   driver (`source: "stub"`, since the library does not have it), an `actors` framework with a sensor
   actor and a display actor, the wiring between them, and board facts for the display.
3. **Validate + review.** `validate` checks the six rules exhaustively. Anything wrong becomes a
   **repair turn** carrying the exact problems — at most three times. Then the board is overwritten
   with the caller's and a person approves.
4. **Scaffold.** `application_scaffold` writes the four templates plus `composition.spire` — the
   decomposition as the YAML file a person opens — `SPIRE.application.json` beside it, stated from that
   same file, and `main/idf_component.yml` for the design's managed dependencies; all structural except
   `main/main.cpp`. Nothing was generated by a model here that could drift. A composition the tree
   **already** carries is left exactly as it is: the design is read from the file, and the `REQUIRES` of
   `main/CMakeLists.txt` and the record are stated from what it says
   (`idf_projects::design_for_scaffold`). When that file is what decides, the caller's dropped copy is
   **reported** on the scaffold's reply (`ScaffoldSpec.design_warning`) and the wizard shows the
   sentence verbatim: the rule is not silent, because a person who reviews one decomposition and is
   handed a project built from another has been told the wrong thing about their own project.
5. **Stubs.** `idf_apply_design` writes `components/pm_sensor/` as a **driver** stub: a header, the bus
   seam, a fake bus, and `test/` — and reports that the design and the library agreed, or where they
   did not, leaving that to a person.
6. **Fill the component.** `idf_component_edit` is called with the datasheet material as `instruction`.
   The reference seam queries `device-facts` with the part number and `esp-idf-lib` in prose, labels
   each heading by corpus, and drops empty sections. Since the datasheet material was given, the
   protocol is writable; the gate is `components/pm_sensor/test` on the host.
7. **Fill the application.** `createProject/GeneratePlan` builds the prompt from the composition, the
   components' real headers and the framework's idiom — and the composition is stated as *the
   architecture, not advice*. `createProject/ExecutePlan` writes only under `main/`.
8. **Verify the composition landed.** `createProject/VerifyApplication` compares the design's framework
   and components against the sources the fill actually wrote — structural, because a fill that ignored
   the design still compiles. The design it compares against is **the tree's**, and a copy the caller is
   still holding is used only for a tree that states none (`idf_projects::design_in_force`, the same rule
   the scaffold resolves by, so a pass never reports on a project checked against a design it does not
   have). `createProject/FinalizeManifest` then corrects `main/idf_component.yml` against what the
   sources reach for.
9. **Build.** `idf_plan` produces `idf.py build` with `IDF_TARGET=esp32s3`. If it fails,
   `createProject/RepairFromBuild` runs the bounded fix loop; a diagnostic in a file outside the fill
   roots is reported *as a person's job*.
10. **Amend.** "Log every thirty seconds instead." `modify/code` → the scope turn (a list of paths,
    answered by `llm_text`) → per-file rewrites (`llm_rewrite`) → build + host tests → else
    byte-for-byte rollback, with the result saying whether target tests ran.
11. **Flash.** `idf.py -p <port> flash` with `IDF_TARGET` — the same platform resolution the build used,
    so the two cannot drift.

The near-miss case is the one worth watching: if the part were an SHT20 and only `sht30.md` existed in
`device-facts`, the retrieved *facts* would be empty and only *precedents* would be present. The prompt
then says so explicitly — the model has the shape and not the bytes, and must invent no command word,
register address or checksum. `resources/device-facts/` ships the near-miss pair on purpose so this
behaviour is testable rather than hoped for.

## Where to look

| Question | File |
|---|---|
| Routing, design handler, `plan_rewrites`, RAG seam | `crates/spire-code/src/actors/coordinator.rs` |
| Spec type, `validate`, `design_request`, `design_application` | `crates/spire-code/src/build/application_spec.rs` |
| Scaffolds, component scope, `component_edit_request`, prompts | `crates/spire-code/src/build/idf_projects.rs` |
| `idf.py` build/flash plans | `crates/spire-code/src/build/idf.rs` |
| The modify plan, `Verified`, rollback | `crates/spire-code/src/build/modify_code.rs` |
| Fix loops | `crates/spire-code/src/build/verify_spine.rs` |
| Prompt text helpers | `crates/spire-code/src/build/generic_helpers.rs` |
| Fill-plan prompt assembly | `crates/spire-code/src/subsystems/project/project_creation.rs` |
| Corpus manifests and facts documents | `crates/spire-code/src/actors/rag_bundle.rs`, `resources/device-facts/` |
| Design plumbing tests (scripted model text) | `crates/spire-code/tests/design_application_llm_tests.rs` |
| Component-edit tests, incl. the near-miss | `crates/spire-code/tests/idf_component_edit_tests.rs` |

