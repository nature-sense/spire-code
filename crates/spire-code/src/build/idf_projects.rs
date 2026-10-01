// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! **The two ESP-IDF project types** — and the tooling that emits them.
//!
//! There are only two, and that is the point. After modelling the embedded side as a container plus
//! applications plus HAL contracts plus BSPs, the shape that survived is the one ESP-IDF already
//! has:
//!
//! 1. **A component library** ([`library_scaffold`]) — a project whose product is `components/*`:
//!    protocol drivers, board support packages, and the framework they share. No actors, no
//!    application entry point.
//! 2. **An application** ([`application_scaffold`]) — a project whose `main/` holds the wiring and the
//!    board facts, whose **units** are components of their own, built against one or more libraries.
//!
//! Everything that used to need an abstraction is one of those two. A HAL contract is not needed
//! because IDF *is* one; a BSP is not a special concept because a board's BSP is a component like
//! any other; a "container" is just a library. The urge to model more came from the Rust side, where
//! a vendor HAL, a per-board BSP crate and a driver crate were three different things — on ESP-IDF
//! they are all `components/<name>/CMakeLists.txt`.
//!
//! # Tooling, not content
//!
//! What lives here is the *skeleton* and the operation that grows it. The protocol inside a
//! component is [`add_component`]'s stub filled in by the model — not something this module knows:
//! a scaffold that shipped a finished driver would be one that only ever worked for that driver.
//!
//! # One thing deliberately not configurable
//!
//! The framework's namespace is `spire`, fixed. It is ours, it does not vary between projects, and a
//! per-project namespace would mean the two scaffolds had to agree on a rename — the coupling that
//! turns a rename into a build failure. (The retired container scaffold emitted
//! `pm25_meter::Container` against a header still saying `spire_container::`, and only a real build
//! caught it.) Only `project()` and the log tag carry the project's own name.

use crate::build::application_spec::{
    ApplicationFramework, ApplicationSpec, ComponentRole, Unit, UnitKind, UnitSource,
};
use crate::build::generic_helpers::run_cmd;
use crate::build::{ScaffoldFile, ScaffoldOutput};
use spire_core::actors::rag::RagChunkResult;
use spire_core::build_types::ProjectStructure;
use std::path::Path;

/// The template names the two scaffolds carry, replaced with the project's own.
const TEMPLATE_LIBRARY: &str = "spire-idf-library";
const TEMPLATE_APPLICATION: &str = "spire-idf-application";

/// What a **library** says about itself, in its root `CMakeLists.txt`.
pub const LIBRARY_MARKER: &str = "set(SPIRE_PROJECT_STRUCTURE idf_library)";

/// What an **application** says about itself, likewise.
pub const APPLICATION_MARKER: &str = "set(SPIRE_PROJECT_STRUCTURE idf_application)";

/// The application's own component directory: where each composition unit is a **component of its
/// own**, rather than another header in `main/`.
const COMPONENTS_DIR: &str = "components";

/// The shared **message types** component — the meeting point of every actor's mailbox, named here
/// rather than by each actor because a sender holds the *receiver's* message type.
const MESSAGES_COMPONENT: &str = "messages";

/// The ESP-IDF file both types own.
///
/// A project has no manifest to hang metadata on, and its `CMakeLists.txt` is the cmake module's — so
/// `sdkconfig.defaults`, the one file in an IDF tree that is IDF's alone, is what
/// `BuildManager::scaffold_build_config` finds a module by.
pub const CONFIG_FILE: &str = "sdkconfig.defaults";

/// The application's **managed dependencies** live in `main/`, not at the project root. The ESP-IDF
/// component manager injects a component's manifest dependencies into that component's `REQUIRES`, so
/// only a manifest *inside* `main/` makes the board's headers includable from `main.cpp` — which
/// `main/CMakeLists.txt` may not name, because it is structural.
pub const MANIFEST_FILE: &str = "main/idf_component.yml";

/// The file a **library** writes its architecture down in, at its root.
///
/// It is emitted empty of opinion — a template asking the library's author what it provides, how it
/// is meant to be used, and what it deliberately leaves to the application — and it is the only
/// place an architecture exists. Nothing in the tree carries one: not a base class, not a
/// registration table, not a `Container`.
pub const HINTS_FILE: &str = "SPIRE.md";

/// The file an **application** writes its reviewed **decomposition** down in, at its root.
///
/// `SPIRE.md` is a library's architecture in prose; this is an application's in a form a tool can
/// check and a model can follow — the components, the units, the wiring and the board facts the design
/// phase decided and a person approved. The scaffold writes it, the fill phase reads it back, and the
/// two are the same file on purpose: what reaches `main/` is the composition that was reviewed, not
/// one invented at generation time.
///
/// JSON rather than prose because this one is *consumed*: a model needs it rendered, a tool needs it
/// parsed, and a person wants to see exactly what was agreed.
pub const APPLICATION_FILE: &str = "SPIRE.application.json";

/// The composition a person authors and opens, as a **project file** rather than the tool's record.
///
/// [`APPLICATION_FILE`] is the JSON the fill phase reads back and a tool consumes. This is the
/// *source*: a YAML file that carries comments, that a person hand-formats and reviews in a pull
/// request, and that is read with
/// [`parse_composition`](crate::build::application_spec::parse_composition). Both names deserialize
/// into the same
/// [`ApplicationSpec`](crate::build::application_spec::ApplicationSpec), so there is one composition
/// in two forms — the one a person edits, and the one the fill writes.
pub const COMPOSITION_FILE: &str = "composition.spire";

/// What a library publishes about itself: its root `SPIRE.md`, verbatim.
///
/// `None` when there is no such file, **or nothing in it**. A library that says nothing is not the
/// same as one that says something blank, and the difference matters here: this string goes into a
/// prompt, and a heading with nothing under it is an instruction to the model to guess.
///
/// Read from disk rather than carried around, because the caller that has the directory is the one
/// that needs it and a library's hints change with the library, not with the build.
pub fn library_hints(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(HINTS_FILE)).ok()?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Where the application's `CMakeLists.txt` names the library it is built against.
const LIBRARY_BLOCK_TOKEN: &str = "__LIBRARY_BLOCK__";

/// Where the application's `CMakeLists.txt` states the framework and the library are written.
///
/// Its own token, and substituted by name: the first version of this prepended the framework block to
/// the *library* substitution instead, which left `__FRAMEWORK_BLOCK__` in the file as a literal line —
/// a CMake parse error ("Expected (") that every test passed, because the marker it was checking for
/// was there regardless. A real `idf.py build` is what read the file rather than the marker.
const FRAMEWORK_BLOCK_TOKEN: &str = "__FRAMEWORK_BLOCK__";

/// What every component library has — the shell, and nothing that is a component.
///
/// Five files, and the components themselves arrive separately: the **framework** comes from
/// [`FRAMEWORK_FILES`], and everything else arrives one at a time through `idf_add_component`. The
/// `SPIRE.md` is the exception that is not code — it is where this library says how it is meant to be
/// used, and it is the only place an architecture is written down.
const LIBRARY_FILES: [(&str, &str); 5] = [
    (
        include_str!("../../templates/esp-idf/library/CMakeLists.txt"),
        "CMakeLists.txt",
    ),
    (
        include_str!("../../templates/esp-idf/sdkconfig.defaults"),
        "sdkconfig.defaults",
    ),
    (
        include_str!("../../templates/esp-idf/library/main/CMakeLists.txt"),
        "main/CMakeLists.txt",
    ),
    (
        include_str!("../../templates/esp-idf/library/main/build_harness.cpp"),
        "main/build_harness.cpp",
    ),
    (include_str!("../../templates/esp-idf/hints.md"), HINTS_FILE),
];

/// The **framework** every library starts with: a task seam and the two application frameworks built
/// on it, hard-coded rather than generated or vendored.
///
/// That is a decision about what a component library *is*. A framework is the architecture an
/// application is written against, and shipping one as something the model has to write would be
/// asking it to invent the architecture — which is the one thing `SPIRE.md` exists to *state* rather
/// than guess. So these arrive with the library, they are structural, and no tool edits them.
///
/// **Nothing makes their use mandatory.** A library of plain components that adopts neither framework
/// is a perfectly good library. What the framework removes is the *gap*: "we need dataflow" and "we
/// need actors with mailboxes" are both answered by what is already in the tree, so neither becomes a
/// wrapping exercise, and neither becomes an architecture invented mid-project.
///
/// The three are deliberately arranged so the two frameworks share **one** thing and nothing else:
///
///   * `toolkit` — `spire::Task`: a loop, a stack, a priority, a core. REQUIRES nothing.
///   * `ramen`   — the dataflow framework: upstream's single header, plus a host test that pins its
///                 semantics and a compile check so a chip build compiles it rather than screening it.
///   * `actors`  — the classical framework: a scheduler that owns the actors, a name registry for
///                 reaching a peer you were not handed a ref to, typed `ActorRef`s to send to, and
///                 `on_message`. REQUIRES `toolkit` only, never `ramen` — the two are alternatives,
///                 not layers.
///
/// **Where the implementation lives is C++'s answer, not ours.** `toolkit` and `actors` keep ordinary
/// code in `src/*.cpp` — a FreeRTOS call belongs in a translation unit — while what is a template on
/// the message type (`Actor<Message>`, `ActorRef<Message>`, `spawn`) can only live in a header.
/// `ramen` is upstream's template library and has no `.cpp` to write short of forking it, so it keeps
/// its compile check as the thing that makes a chip build compile it rather than skip it.
const FRAMEWORK_FILES: [(&str, &str); 24] = [
    (
        include_str!("../../templates/esp-idf/framework/toolkit/CMakeLists.txt"),
        "components/toolkit/CMakeLists.txt",
    ),
    (
        include_str!("../../templates/esp-idf/framework/toolkit/include/task.hpp"),
        "components/toolkit/include/task.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/toolkit/src/task.cpp"),
        "components/toolkit/src/task.cpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/ramen/CMakeLists.txt"),
        "components/ramen/CMakeLists.txt",
    ),
    (
        include_str!("../../templates/esp-idf/framework/ramen/include/ramen.hpp"),
        "components/ramen/include/ramen.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/ramen/src/ramen_compile_check.cpp"),
        "components/ramen/src/ramen_compile_check.cpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/ramen/test/CMakeLists.txt"),
        "components/ramen/test/CMakeLists.txt",
    ),
    (
        include_str!("../../templates/esp-idf/framework/ramen/test/ramen_test.cpp"),
        "components/ramen/test/ramen_test.cpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/CMakeLists.txt"),
        "components/actors/CMakeLists.txt",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/include/actor.hpp"),
        "components/actors/include/actor.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/include/actor_ref.hpp"),
        "components/actors/include/actor_ref.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/include/mailbox.hpp"),
        "components/actors/include/mailbox.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/include/scheduler.hpp"),
        "components/actors/include/scheduler.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/include/registry.hpp"),
        "components/actors/include/registry.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/src/scheduler.cpp"),
        "components/actors/src/scheduler.cpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/CMakeLists.txt"),
        "components/actors/test/CMakeLists.txt",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/actors_test.cpp"),
        "components/actors/test/actors_test.cpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/message_tag_probe.cpp"),
        "components/actors/test/message_tag_probe.cpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/probe_message.hpp"),
        "components/actors/test/probe_message.hpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/freertos/FreeRTOS.h"),
        "components/actors/test/freertos/FreeRTOS.h",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/freertos/queue.h"),
        "components/actors/test/freertos/queue.h",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/freertos/task.h"),
        "components/actors/test/freertos/task.h",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/freertos_fake.cpp"),
        "components/actors/test/freertos_fake.cpp",
    ),
    (
        include_str!("../../templates/esp-idf/framework/actors/test/freertos_fake.hpp"),
        "components/actors/test/freertos_fake.hpp",
    ),
];

/// The framework components' names, so the edit path can refuse them without walking a file list.
///
/// A test keeps this in step with [`FRAMEWORK_FILES`]. The point of the list: the framework is
/// **shipped**, not generated and not vendored, so there is nothing in it to fill and nothing in it
/// to edit. That is a stronger promise than the one the vendoring machinery used to make about
/// upstream's code — and it is made in one place instead of per component.
pub const FRAMEWORK_COMPONENTS: [&str; 3] = ["toolkit", "ramen", "actors"];

/// Whether a component is one of the **shipped** ones — the framework, as opposed to something this
/// library's own work produced.
///
/// The distinction a caller needs it for is the same one the edit path refuses on: a generated
/// component is a stub somebody fills in, and a framework component is not. The UI asks so it can stop
/// offering to write one; the tools ask so they can refuse.
pub fn is_framework_component(name: &str) -> bool {
    FRAMEWORK_COMPONENTS.contains(&ident_of(name).as_str())
}

/// The **component library** skeleton: `components/*`, and a build harness so they compile.
pub fn library_scaffold(
    project_name: &str,
    platforms: &[String],
) -> Result<ScaffoldOutput, String> {
    let name = require_name(project_name, "an ESP-IDF component library")?;
    let rename = |text: &str| text.replace(TEMPLATE_LIBRARY, name);

    let mut files: Vec<ScaffoldFile> = LIBRARY_FILES
        .into_iter()
        .map(|(template, path)| ScaffoldFile {
            path: path.to_string(),
            content: rename(template),
            // Structural: the framework is not the model's to edit. The files that *are* fillable
            // arrive with `add_component`, as stubs.
            structural: true,
            ..Default::default()
        })
        .collect();
    files.push(ScaffoldFile {
        path: "README.md".to_string(),
        content: library_readme(name),
        structural: true,
        ..Default::default()
    });
    // The framework, hard-coded: the task seam and the two application frameworks. Structural, so no
    // tool rewrites them — an architecture that a model can edit is an architecture that drifts.
    files.extend(
        FRAMEWORK_FILES
            .into_iter()
            .map(|(template, path)| ScaffoldFile {
                path: path.to_string(),
                content: template.to_string(),
                structural: true,
                ..Default::default()
            }),
    );

    Ok(ScaffoldOutput {
        build_file: "CMakeLists.txt".to_string(),
        build_content: rename(include_str!(
            "../../templates/esp-idf/library/CMakeLists.txt"
        )),
        source_dir: "components".to_string(),
        source_file: "main/build_harness.cpp".to_string(),
        source_content: include_str!("../../templates/esp-idf/library/main/build_harness.cpp")
            .to_string(),
        files,
        platform_targets: platforms.to_vec(),
        // Nothing is fillable at creation: the framework is fixed, and a protocol component arrives
        // as a stub through `add_component`.
        fill_roots: Vec::new(),
        dependency_sections: Vec::new(),
        structure: ProjectStructure::IdfLibrary,
        embedded: true,
    })
}

/// The **application** skeleton: a blank entry point, and the library it is built against.
///
/// Nothing about the product is assumed — no actor owner, no wiring, no lifecycle. The shape of an
/// application belongs to the library it is built against, and that library states it in its hints;
/// a scaffold that baked one in would be choosing the architecture for every product at once.
///
/// `library` is a directory — relative to the application's root, or absolute — and is substituted
/// into the `EXTRA_COMPONENT_DIRS` block. Empty is allowed and means "no library": the application's
/// components are then its own, which is a legitimate but unusual start.
///
/// `framework` is what the design phase decided and a person reviewed, and it is **stated in the
/// file** rather than kept in the caller's head — see [`framework_block`] for why. `None` is honest:
/// the application states that it has not chosen yet.
///
/// `application` is the whole **reviewed decomposition** the framework came from. When it is given,
/// the application carries it **twice**: as [`COMPOSITION_FILE`], the file a person edits, and as
/// [`APPLICATION_FILE`], the record beside it that everything else reads. That is what makes the fill
/// phase write the composition that was reviewed instead of inventing one, and what keeps the design
/// in the tree rather than in a caller's memory.
///
/// A design reaches this from either side of the tree — the caller that ran the design phase, or the
/// composition a person left in the directory the scaffold was pointed at (see
/// [`design_for_scaffold`], which decides between them and says why). It is the same decomposition
/// either way, and the `REQUIRES` below is written from it either way.
///
/// A reviewed design also decides the layout: every unit — an **actor** and a **ramen stage** alike — is
/// emitted as a component of its own, `components/<unit>/`, carrying its manifest, its header, its source
/// and its host test, with `REQUIRES` named from the design; and the shared message types as
/// `components/messages/`, because a sender holds the *receiver's* message type and a stage pushes what
/// the next one pulls — the type belongs to the edge. `main/main.cpp` stays a blank entry point (the one
/// non-structural file) that the fill phase turns into the spawns, the wiring and the board facts. Both
/// trees are the fill's; the manifests and the per-unit test builds are structural. See
/// [`unit_component_files`].
pub fn application_scaffold(
    project_name: &str,
    platforms: &[String],
    library: &str,
    application: Option<&ApplicationSpec>,
) -> Result<ScaffoldOutput, String> {
    let name = require_name(project_name, "an ESP-IDF application")?;
    let framework = framework_block(application.map(|application| application.framework));
    let library_section = library_block(library);
    let requires = requires_block(application);
    let rename = |text: &str| {
        text.replace(TEMPLATE_APPLICATION, name)
            .replace(FRAMEWORK_BLOCK_TOKEN, &framework)
            .replace(LIBRARY_BLOCK_TOKEN, &library_section)
            .replace(REQUIRES_BLOCK_TOKEN, &requires)
    };

    let templates: [(&str, &str); 4] = [
        (
            include_str!("../../templates/esp-idf/application/CMakeLists.txt"),
            "CMakeLists.txt",
        ),
        (
            include_str!("../../templates/esp-idf/sdkconfig.defaults"),
            "sdkconfig.defaults",
        ),
        (
            include_str!("../../templates/esp-idf/application/main/CMakeLists.txt"),
            "main/CMakeLists.txt",
        ),
        (
            include_str!("../../templates/esp-idf/application/main/main.cpp"),
            "main/main.cpp",
        ),
    ];

    let mut files: Vec<ScaffoldFile> = templates
        .into_iter()
        .map(|(template, path)| ScaffoldFile {
            path: path.to_string(),
            content: rename(template),
            // **`main/main.cpp` is the product, not the scaffold.** The composition — the units, the
            // wiring and the board facts the design phase decided — is written there, so it is the one
            // file here that the fill phase must be able to write. Everything else in this list is the
            // build's (`CMakeLists.txt`, `sdkconfig.defaults`, `main/CMakeLists.txt`) or the record's
            // (`README.md`, `SPIRE.application.json`), and stays locked.
            //
            // The first live run of the loop is why this is not a matter of taste: with the product
            // file locked, the model's plan was executed and every write was refused with "a locked
            // structural file", leaving a scaffold that could never become an application.
            structural: path != "main/main.cpp",
            ..Default::default()
        })
        .collect();
    files.push(ScaffoldFile {
        path: "README.md".to_string(),
        content: application_readme(name, library, application.is_some()),
        structural: true,
        ..Default::default()
    });
    // The reviewed decomposition, in the tree **twice**: the composition a person edits, and the JSON
    // record everything else reads. Both are written from this one `ApplicationSpec`, so the two
    // cannot be written disagreeing — and [`read_application_and_sync_record`] is what keeps them
    // from drifting apart afterwards.
    //
    // A composition the tree **already** carries is not rendered over — the design resolved above *is*
    // that file, and a comment a person left in it is the one thing this rendering cannot carry, so
    // writing it again could only lose something. See [`design_for_scaffold`] for the resolution and
    // `project_creation::materialize_scaffold_files` for the write that skips it.
    //
    // Both are `structural`, and that is about the **fill phase**, not about a person: the model that
    // writes `main/main.cpp` must not be free to rewrite the architecture it was handed. Editing
    // `composition.spire` between rounds is the supported way to change a design; a second
    // `SPIRE.application.json` invented by the fill is not.
    if let Some(application) = application {
        files.push(ScaffoldFile {
            path: COMPOSITION_FILE.to_string(),
            content: render_composition(application)?,
            structural: true,
            ..Default::default()
        });
        files.push(ScaffoldFile {
            path: APPLICATION_FILE.to_string(),
            content: format!(
                "{}\n",
                serde_json::to_string_pretty(application).map_err(|e| {
                    format!("the decomposition could not be written as JSON: {e}")
                })?
            ),
            structural: true,
            ..Default::default()
        });
    }

    // **The composition's units as components of their own.** Each actor and each ramen stage gets
    // `components/<unit>/`, so its message (or the values on its edges), its state and its task are a
    // module boundary rather than a corner of `main/`; the shared types get `components/messages/`.
    // Every build file among them is `structural` — the scaffold names `REQUIRES` and the harness from
    // the reviewed design, exactly as it does for `main/` — and the classes, sources and cases inside
    // them are the fill phase's. See [`unit_component_files`].
    files.extend(unit_component_files(application, library));

    // The **managed dependencies**: the board's BSP, and any component the design marked `published`.
    // Written into `main/` rather than the project root deliberately — a component's own manifest is
    // what the ESP-IDF component manager *injects* into that component's `REQUIRES`, so `main.cpp` can
    // include the BSP's headers without `main/CMakeLists.txt` naming them (and it may not: that file is
    // structural). See `application_manifest`.
    if let Some((path, content)) = application_manifest(application) {
        files.push(ScaffoldFile {
            path,
            content,
            structural: true,
            ..Default::default()
        });
    }

    Ok(ScaffoldOutput {
        build_file: "CMakeLists.txt".to_string(),
        build_content: rename(include_str!(
            "../../templates/esp-idf/application/CMakeLists.txt"
        )),
        source_dir: "main".to_string(),
        source_file: "main/main.cpp".to_string(),
        source_content: rename(include_str!(
            "../../templates/esp-idf/application/main/main.cpp"
        )),
        files,
        platform_targets: platforms.to_vec(),
        // The composition is the model's to fill, and it now spans **two** trees: `main/` (the
        // wiring and the board facts) and `components/` (one directory per actor, and the shared
        // messages). Both are writable; the `CMakeLists.txt` inside each is structural and is not.
        // An empty list here means the opposite of what it reads as (the guard treats it as
        // "everything is writable"), and the first live run of the loop showed what the empty version
        // produced: a product file that was locked, and a fill that could write nothing.
        fill_roots: vec!["main".to_string(), COMPONENTS_DIR.to_string()],
        dependency_sections: Vec::new(),
        structure: ProjectStructure::IdfApplication,
        embedded: true,
    })
}

/// The composition, rendered as the file a person opens — what the scaffold writes as
/// [`COMPOSITION_FILE`].
///
/// Rendered from the spec rather than echoed from whatever text the design phase read, because the
/// spec is what was *validated*: a file written from unvalidated text could differ from the
/// composition that was approved. The one thing rendering cannot carry is a comment, so a comment a
/// person adds to the file is theirs from then on — this is the **first** writing of it, not a
/// rewriting, and nothing regenerates over what they wrote.
pub fn render_composition(spec: &ApplicationSpec) -> Result<String, String> {
    serde_yaml::to_string(spec)
        .map_err(|e| format!("the composition could not be written as YAML: {e}"))
}

/// The design a project carries, **read without writing**: [`COMPOSITION_FILE`] when it is there —
/// it is the source — and otherwise [`APPLICATION_FILE`], which is all an application scaffolded
/// before the composition existed carries.
///
/// A composition that is there and does **not** parse is an error, never a fall-through to the
/// record. A broken composition is not a composition, and quietly reading the older record in its
/// place is how a project builds something other than what is written in the file — which is the
/// whole reason the composition is a file rather than a value in a wizard.
///
/// `Ok(None)` is honest rather than an error: a tree that has not been designed states nothing, which
/// is the ordinary state of a project before the design phase.
///
/// Nothing here writes, which is what the **planning** phase needs: it resolves a structure in memory
/// and puts nothing on disk until the caller confirms. A caller that is about to write the tree wants
/// [`read_application_and_sync_record`] instead.
///
/// **A design that parses and does not hold together is refused too**, by the file that states it and
/// with every broken rule listed at once — the rule a *handed-in* decomposition is already held to
/// before anything is created (`CoordinatorActor::params_application`), applied to the file for the
/// same reason. This is not a model's half-finished answer on its way to a repair round: it is the
/// architecture the project states. A file that wires a unit that does not exist, or reaches a device
/// through a driver that is not there, is not a design with a gap in it — it is the wrong
/// architecture, and reading it as the truth is how a build fails somewhere that names neither the
/// file nor the rule.
pub fn read_application(root: &Path) -> Result<Option<ApplicationSpec>, String> {
    let composition = root.join(COMPOSITION_FILE);
    if !composition.is_file() {
        return match std::fs::read_to_string(root.join(APPLICATION_FILE)) {
            Ok(text) => {
                let spec = crate::build::application_spec::parse_spec(&text)
                    .map_err(|e| format!("{APPLICATION_FILE} is not a composition: {e}"))?;
                Ok(Some(design_that_holds_together(APPLICATION_FILE, spec)?))
            }
            Err(_) => Ok(None),
        };
    }
    let text = std::fs::read_to_string(&composition)
        .map_err(|e| format!("{COMPOSITION_FILE} could not be read: {e}"))?;
    let spec = crate::build::application_spec::parse_composition(&text)
        .map_err(|e| format!("{COMPOSITION_FILE} is not a composition: {e}"))?;
    Ok(Some(design_that_holds_together(COMPOSITION_FILE, spec)?))
}

/// A design read out of a project file, held to the rules — or refused by the file, with **every**
/// broken rule at once.
///
/// [`validate`](crate::build::application_spec::validate) answers with all of a spec's problems rather
/// than the first, and the refusal keeps them all for the reason `validate` does: a person repairing a
/// file fixes them in one pass, and one told a rule at a time spends six rounds finding out where it
/// went wrong. The file is named rather than the rule, too — it is the thing to open.
///
/// Shared with `createProject/ParseComposition` — the wizard's "open a `composition.spire`" door — so
/// a composition a person hands in is held to **exactly** what one read back off a tree is: the same
/// parser ([`parse_composition`](crate::build::application_spec::parse_composition)) and this same
/// check, not a second reading that could drift.
pub(crate) fn design_that_holds_together(
    file: &str,
    spec: ApplicationSpec,
) -> Result<ApplicationSpec, String> {
    match crate::build::application_spec::validate(&spec) {
        Ok(()) => Ok(spec),
        Err(problems) => Err(format!(
            "{file} states a design that does not hold together, so it is not one this project can be \
             built from:\n  - {}",
            problems.join("\n  - ")
        )),
    }
}

/// [`read_application`], and the record brought back into agreement with the composition it renders.
///
/// **`composition.spire` is the source.** When it is there, it is what is parsed and what the fill
/// phase is handed, and [`APPLICATION_FILE`] is *rewritten from it* whenever the record is missing or
/// says something else — so the record cannot go on describing a composition a person has since
/// edited, which is the one failure a second copy on disk can have.
///
/// Writing on a read is deliberate, and narrow. The only way the two can differ is that a person
/// edited the composition, which is the supported way to change a design; and the callers are ones
/// that are about to write a tree anyway, so a filesystem this cannot write to was already fatal and
/// no new way to fail has been added. It is a **separate** function from the read for exactly that
/// reason: a caller that has promised to put nothing on disk — the planning phase — must be able to
/// read a design without settling it.
///
/// Without a composition there is nothing to settle: the record *is* the source then, and rendering it
/// from itself would be a write with nothing to say.
pub fn read_application_and_sync_record(root: &Path) -> Result<Option<ApplicationSpec>, String> {
    let spec = read_application(root)?;
    if !root.join(COMPOSITION_FILE).is_file() {
        return Ok(spec);
    }
    let Some(spec) = spec else {
        return Ok(None);
    };
    // Written only when it actually differs, so an unchanged project is not rewritten on every read.
    let record = format!(
        "{}\n",
        serde_json::to_string_pretty(&spec)
            .map_err(|e| format!("the composition could not be written as JSON: {e}"))?
    );
    let path = root.join(APPLICATION_FILE);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(record.as_str()) {
        std::fs::write(&path, &record).map_err(|e| {
            format!(
                "{APPLICATION_FILE} could not be brought into agreement with \
                 {COMPOSITION_FILE}: {e}"
            )
        })?;
    }
    Ok(Some(spec))
}

/// **The design in force for a tree**: the one the tree already carries, else the one the caller
/// named.
///
/// A tree that states a design *is* that design. The application states it in three places at once —
/// the `REQUIRES` of `main/CMakeLists.txt`, [`COMPOSITION_FILE`] and the record beside it — so
/// everything that reads a project and everything that writes one has to resolve the same design, or
/// it acts on a project whose own files disagree about what it is. Two failures came from resolving
/// the caller's instead, and both were silent: with no design handed in, `REQUIRES` said *"nothing
/// yet"* while the composition named the components the product needed (a build failure at an
/// `#include`, naming neither file); and with a **stale** design handed in, the authored
/// `composition.spire` was rendered over — losing the one thing a file has and a record cannot, the
/// comments a person left in it.
///
/// So the tree wins when it has a design, and the caller's is used only for a tree that has none —
/// which is the ordinary case, a project being created for the first time. A design is changed by
/// editing [`COMPOSITION_FILE`]; re-scaffolding is not a redesign. When the two disagree, that is said
/// out loud rather than passed over: a caller whose copy was dropped should hear it here, not infer it
/// from a tree that ignores it.
///
/// **Both directions obey this** — the legs that *write* a tree (`project_creation`, through
/// [`design_for_scaffold`]) and the passes that *read* one (`CoordinatorActor::application_for_pass`).
/// A verify that ran against the caller's copy instead would report on a project checked against a
/// design the tree does not have, which is the same lie as writing one the tree did not ask for, only
/// spelled in the other tense.
///
/// [`read_application`] does the reading, so a tree whose composition does not parse — or does not hold
/// together — is refused rather than rendered over or read around. A broken composition is not a
/// composition, and it is never replaced in silence.
pub fn design_in_force(
    root: &Path,
    requested: Option<ApplicationSpec>,
) -> Result<Option<ApplicationSpec>, String> {
    resolve_design(root, requested).map(|(design, _dropped)| design)
}

/// **The resolution itself, and whether it dropped the caller's copy** — the half
/// [`design_in_force`] and [`design_for_scaffold`] share.
///
/// One rule, two audiences: a *reading* pass wants only the design, while the legs that *write* a
/// tree also have to say the drop out loud to the person in front of them — see [`ScaffoldDesign`].
/// Written once so the two cannot decide differently, which is the whole point of the rule: what a
/// pass reports on and what a scaffold writes are one design, not two.
fn resolve_design(
    root: &Path,
    requested: Option<ApplicationSpec>,
) -> Result<(Option<ApplicationSpec>, bool), String> {
    let Some(carried) = read_application(root)? else {
        // A tree that states nothing has no design to prefer, so the caller's is the design — and
        // nothing was dropped, because there was nothing to drop it for.
        return Ok((requested, false));
    };
    let dropped = requested
        .as_ref()
        .is_some_and(|requested| *requested != carried);
    if dropped {
        tracing::warn!(
            "this tree already states its design in {COMPOSITION_FILE}, so the decomposition the \
             caller handed in was not used: a design changes by editing that file"
        );
    }
    Ok((Some(carried), dropped))
}

/// **The design a scaffold works from, and what to tell a caller whose copy was dropped.**
///
/// The write-side legs need both halves of [`design_in_force`]'s answer: the design, and — when the
/// tree already states a different one — the fact that the decomposition they were handed was *not*
/// used. That is neither a parse error nor a failure: the tree keeps its design, which is the rule.
/// But it is exactly the kind of thing a person has to be *told* rather than left to infer, or the
/// wizard reviews one decomposition and is handed a project built from another. The sentence is the
/// report the sheet shows; the core owns the rule, so it owns the wording.
#[derive(Debug, Clone)]
pub struct ScaffoldDesign {
    /// The design the scaffold works from — the tree's when it carries one, the caller's otherwise.
    pub spec: Option<ApplicationSpec>,
    /// `Some` when the tree already carried a *different* design, so `spec` is the tree's and the
    /// caller's decomposition was dropped. `None` in the ordinary case: a project being created for
    /// the first time, where the caller's design *is* the design.
    pub dropped: Option<String>,
}

impl ScaffoldDesign {
    /// What a person is told when their decomposition was not the one used.
    ///
    /// The file, the fact that the reviewed design was not applied, and the two ways forward —
    /// because "your design was ignored" without them reads as a bug rather than as a rule.
    fn dropped_message() -> String {
        format!(
            "This project already states its design in `{COMPOSITION_FILE}`, so the decomposition \
             that was just reviewed was not applied — the project keeps the design that file holds. \
             Edit `{COMPOSITION_FILE}` to change it and run again, or delete the file to design \
             from scratch."
        )
    }
}

/// **The design a scaffold or a plan works from**: [`design_in_force`], for the one structure that has
/// a design at all — and, unlike the reading passes, the report the person in front of the wizard
/// needs when the design they reviewed was not the one used.
///
/// The **structure gate is here** rather than at each call site so that the legs of one creation
/// cannot resolve the design differently — the plan and the write, which are asked for separately and
/// have to agree. Which project type to emit is the caller's to state, and a `composition.spire` lying
/// in a directory someone asked to scaffold as something else is not evidence about what they asked
/// for.
pub fn design_for_scaffold(
    root: &Path,
    structure: Option<ProjectStructure>,
    requested: Option<ApplicationSpec>,
) -> Result<ScaffoldDesign, String> {
    if structure != Some(ProjectStructure::IdfApplication) {
        return Ok(ScaffoldDesign {
            spec: requested,
            dropped: None,
        });
    }
    let (spec, dropped) = resolve_design(root, requested)?;
    Ok(ScaffoldDesign {
        spec,
        dropped: dropped.then(ScaffoldDesign::dropped_message),
    })
}

/// Where the application's own component names what its sources include.
const REQUIRES_BLOCK_TOKEN: &str = "__REQUIRES__";

/// The `REQUIRES` line an application's own component carries, from the design it was built for.
///
/// A component's header resolves only if its component is named here, and the file is **structural** —
/// the model that writes the composition may not edit it. So the scaffold names them, from the design a
/// person reviewed: a composition written against a component the manifest does not name is a build
/// error at the `#include`, which is far from the line that caused it.
///
/// Without a design it says so and names nothing: an application that has not been designed includes
/// nothing yet, and inventing requires would be inventing an architecture.
fn requires_block(application: Option<&ApplicationSpec>) -> String {
    let Some(application) = application else {
        return "    # Nothing yet: this application has not been designed, so nothing in `main.cpp`\n\
                \x20   # includes a component of the library. Name them here as they come to be used.\n"
            .to_string();
    };
    let mut names: Vec<String> = framework_components(application.framework)
        .iter()
        .map(|name| name.to_string())
        .collect();
    // The composition's **own** components, before the library's: the shared types, then each unit —
    // `main.cpp` includes their headers to spawn and wire them (and, for a ramen chain, to pump it), and the
    // manifest is structural, so the scaffold names them rather than the model. Every one of them is a
    // component this application emits, so the name always resolves.
    let units: Vec<&Unit> = application
        .units
        .iter()
        .filter(|unit| unit_component_kind(unit).is_some())
        .collect();
    // The shared types arrive with the first unit, and only with it: `unit_component_files` writes no
    // `components/messages/` for an application that has no composition, and a `REQUIRES` naming a component
    // that is not there is a build that cannot resolve. A **stage** needs them for the same reason an actor
    // does — the value on each edge is a type neither end of that edge owns.
    if !units.is_empty() && !names.contains(&MESSAGES_COMPONENT.to_string()) {
        names.push(MESSAGES_COMPONENT.to_string());
    }
    for unit in &units {
        let ident = component_ident(&unit.id);
        if !names.contains(&ident) {
            names.push(ident);
        }
    }
    for unit in &application.units {
        // A **published** component is not a library component: the application's manifest names it and
        // the component manager resolves it and injects it, under the namespaced build name this file
        // has no business guessing. Naming it here would be a `REQUIRES` that does not resolve.
        if unit.kind != UnitKind::Component || unit.source == Some(UnitSource::Published) {
            continue;
        }
        let ident = component_ident(&unit.id);
        if !names.contains(&ident) {
            names.push(ident);
        }
    }
    format!(
        "    # The framework's components, then this composition's own — the shared types and one component\n    \
         # per unit — then the components of the reviewed design it wraps.\n    \
         REQUIRES {}\n",
        names.join(" ")
    )
}

/// A unit id as a **component name**: `-` becomes `_`.
///
/// A component's directory name *is* its build name, so `REQUIRES` has to spell it the way the
/// directory does. The same substitution `requires_block` has always made for a library component, and
/// it has to be the same one here: a directory named `air-quality` beside a `REQUIRES air_quality` is a
/// component the build cannot find.
fn component_ident(id: &str) -> String {
    id.replace('-', "_")
}

/// A template's `__TOKEN__`s filled in — the convention `templates/esp-idf/component/` already uses.
///
/// By name rather than by position, so the template stays readable and a token nobody filled in is
/// visible in the output, which is what the tests assert against.
fn fill_component_template(template: &str, substitutions: &[(&str, String)]) -> String {
    let mut text = template.to_string();
    for (token, value) in substitutions {
        text = text.replace(token, value);
    }
    text
}

/// The C++ **type name** a design's name for a message or an edge value becomes.
///
/// `Tick` stays a type, `frames` becomes `Frames`, `pm25_reading` becomes `Pm25Reading`. The scaffold
/// writes **both sides** of the boundary — the shared types, and the units that name them — so
/// normalising here is what makes the two agree by construction; it also turns a name a design wrote in
/// prose into an identifier a compiler accepts. And it de-duplicates through the same door: `Tick` and
/// `tick` are one type, so a design that wrote both means one.
fn message_type_name(raw: &str) -> String {
    type_name(&ident_of(raw))
}

/// The **types the design names**, in the order it first names them, each with the units that name it.
///
/// One list from two sources: an actor names the message it receives, and a stage names the value on
/// each of its edges — what it pulls and what it pushes. The same name on both sides of an edge is
/// **one** type, which is what an edge is; and a name two actors receive (`Tick`, in the worked example)
/// is one type too, which is exactly why it cannot live in either actor's component.
fn messages_of(application: &ApplicationSpec) -> Vec<(String, Vec<String>)> {
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    for unit in &application.units {
        let named: Vec<&str> = match unit.kind {
            UnitKind::Actor => vec![unit.message.as_deref().unwrap_or_default()],
            UnitKind::Stage => vec![
                unit.pulls.as_deref().unwrap_or_default(),
                unit.pushes.as_deref().unwrap_or_default(),
            ],
            // A component names no message and no edge: it is framework-agnostic, which is the rule
            // `validate` states by refusing it any of those fields.
            UnitKind::Component => Vec::new(),
        };
        for raw in named {
            let name = message_type_name(raw);
            if name.is_empty() {
                continue;
            }
            match found.iter_mut().find(|(existing, _)| *existing == name) {
                Some((_, users)) => {
                    if !users.iter().any(|user| user.as_str() == unit.id) {
                        users.push(unit.id.clone());
                    }
                }
                None => found.push((name, vec![unit.id.clone()])),
            }
        }
    }
    found
}

/// The composition's **units as components of their own**: one directory per actor and per ramen
/// stage, plus the shared type component.
///
/// # Why a unit is a component
///
/// An actor is a message type, the state it holds between messages, and a task of its own; a ramen
/// stage is what it pulls, what it pushes and the code between. Both are, exactly, what an ESP-IDF
/// component already is: a directory with a manifest, an `include/`, a `src/` and a `test/`. Leaving
/// the composition's units in `main/` makes the whole composition one directory of headers that
/// include one another, and makes `main/` the only place a build error can point. So each unit gets
/// `components/<unit>/`, and `main/` is left with what is actually the application's: the board facts,
/// the spawns and the wiring.
///
/// # Why the shared types are shared, and apart
///
/// For **actors**, a sender holds `spire::ActorRef<Receiver::Message>` — the **receiver's** type, not
/// its own. So a message type cannot live only in the sender's component, and cannot live only in the
/// receiver's either: a third actor may post the same type, and in the worked example `Tick` is the
/// message of two actors.
///
/// For **ramen**, the value that crosses an edge (`a >> b`) belongs to the *edge*: `a` pushes it and
/// `b` pulls it, so it can live in neither alone either.
///
/// `components/messages/` is that meeting point for both frameworks, and every unit component depends
/// on it. It is a *library* component in kind: a message is plain data, and nothing in it needs a
/// framework.
///
/// # The whole component, not just its manifest
///
/// A unit arrives the way a component of the **library** arrives — `add_component`'s five files, one
/// kind over: a manifest, a stub header, a stub source and a host test with the build that runs it.
/// That is not decoration. The unit's `CMakeLists.txt` and its `test/CMakeLists.txt` are **the tool's**,
/// and for the same reason `main/CMakeLists.txt` is: a build file names the architecture — which
/// framework, which library components, which fake — and a model that invented one would be writing
/// the architecture it was handed rather than the code inside it. The stubs are the other half: they
/// make the shape visible (where the class goes, how it is named, what its ports are) and they
/// **compile as they stand**, which is what a starting point has to do.
///
/// So per unit: `CMakeLists.txt` and `test/CMakeLists.txt` are **structural**; the header, the source
/// and the host test are **fillable**, and the fill writes them. In `components/messages/` the same,
/// except that the compile-check translation unit is structural too — it includes the header and
/// nothing else, so it is a check rather than code.
///
/// # Why the emitted stubs name what the design named
///
/// The stub header of an actor declares it as `spire::Actor<messages::Tick>` for the message the
/// review gave it; a stage declares an in-port per `pulls` and an out-port per `pushes`. The shared
/// types are generated from the same names, so the two agree by construction and the application
/// **builds** before it computes anything — the property a scaffold is for, and the one a live run
/// lost when it wrote a composition against headers that had no such class and a bus that had no
/// device.
///
/// # Why the host test's build is generated rather than templated
///
/// `messages/`'s host test is a fixed build over two files beside it, so it is a template. A *unit*'s
/// is not: it has to reach the **library** — the framework's headers and sources, the fake FreeRTOS
/// the `actors` framework is tested with, and every component the design says this unit wraps, whose
/// fake bus has to win the include path exactly as it does in the component's own test. All of that
/// is design facts, and `library` is the directory they hang off — resolved the way
/// `__LIBRARY_BLOCK__` resolves it, so a relative path means the same thing in both.
///
/// `main/main.cpp` still names the units it spawns and wires, so `main/CMakeLists.txt` still `REQUIRES`
/// them; what changes is that the unit's own dependencies are the unit component's to state, not
/// `main/`'s.
fn unit_component_files(application: Option<&ApplicationSpec>, library: &str) -> Vec<ScaffoldFile> {
    let Some(application) = application else {
        return Vec::new();
    };
    // The composition's **own** units, each with the kind of component it is. A published component is the
    // registry's and an existing or stubbed one is the *library's*; a `component` is neither, so it falls out
    // here rather than being handed on with a kind it does not have.
    let units: Vec<(&Unit, ComponentKind)> = application
        .units
        .iter()
        .filter_map(|unit| unit_component_kind(unit).map(|kind| (unit, kind)))
        .collect();
    // No units, no shared type boundary and no per-unit components: this application is `main/` alone
    // until a design gives it a composition.
    if units.is_empty() {
        return Vec::new();
    }

    let messages_file = |name: &str, content: String, structural: bool| ScaffoldFile {
        path: format!("{COMPONENTS_DIR}/{MESSAGES_COMPONENT}/{name}"),
        content,
        structural,
        ..Default::default()
    };
    let mut files = vec![
        ScaffoldFile {
            path: format!("{COMPONENTS_DIR}/{MESSAGES_COMPONENT}/CMakeLists.txt"),
            content: messages_component_manifest(),
            structural: true,
            ..Default::default()
        },
        messages_file(
            "include/messages.hpp",
            messages_component_header(application),
            false,
        ),
        messages_file(
            "src/messages_compile_check.cpp",
            MESSAGES_COMPILE_CHECK.to_string(),
            true,
        ),
        messages_file(
            "test/CMakeLists.txt",
            fill_component_template(
                include_str!("../../templates/esp-idf/application/messages/test/CMakeLists.txt"),
                &[],
            ),
            true,
        ),
        messages_file("test/messages_test.cpp", messages_test(application), false),
    ];

    for (unit, kind) in units {
        let ident = component_ident(&unit.id);
        let unit_file = |name: &str, content: String, structural: bool| ScaffoldFile {
            path: format!("{COMPONENTS_DIR}/{ident}/{name}"),
            content,
            structural,
            ..Default::default()
        };
        files.push(ScaffoldFile {
            path: format!("{COMPONENTS_DIR}/{ident}/CMakeLists.txt"),
            content: unit_component_manifest(application, unit, kind),
            structural: true,
            ..Default::default()
        });
        // The class, included as `#include <{ident}.hpp>` — a flat path, the same layout the library's own
        // components use, so a unit is reached exactly the way a driver or a library is.
        files.push(unit_file(
            &format!("include/{ident}.hpp"),
            unit_header(unit, kind),
            false,
        ));
        files.push(unit_file(
            &format!("src/{ident}.cpp"),
            unit_source(unit),
            false,
        ));
        // The harness: structural, because a unit's test build names the framework, the library and the fakes
        // exactly as its manifest names `REQUIRES` — see [`unit_test_manifest`].
        files.push(unit_file(
            "test/CMakeLists.txt",
            unit_test_manifest(application, unit, library),
            true,
        ));
        files.push(unit_file(
            &format!("test/{ident}_test.cpp"),
            unit_test(unit, kind),
            false,
        ));
    }
    files
}

/// The **shared types** the design names, as the file the fill starts from.
///
/// One empty struct per type, in the order the design first names them. Empty on purpose: what a message
/// *contains* is a fact about whatever is at the other end of the edge — a device's reading, a frame, a
/// screen's report — and neither the design nor the scaffold knows it. What the scaffold can state is
/// which types exist and where they live, and that is what makes every unit (generated from the same
/// names) compile before anything has been filled in.
///
/// The line above each type names the units that name it, which is a fact no reader of this file can
/// recover: `Tick` is two actors' message, so the type belongs to neither of them.
fn messages_component_header(application: &ApplicationSpec) -> String {
    let mut text = r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

// The composition's **shared types** — the one header every unit of this application includes.
//
// It is a component of its own rather than a corner of any unit, because the type on an edge belongs to
// the *edge*: a sender holds `spire::ActorRef<Receiver::Message>`, so the type is the receiver's and two
// actors may share it, and a ramen stage pushes a value that another stage pulls, so it belongs to
// neither stage alone. Nothing here is a framework's: a message is plain data, and this component
// REQUIRES no framework, exactly as it should.
//
// **These are stubs.** The names are the design's; the fields are not, and no scaffold can supply them —
// what crosses an edge is a fact about the device, the protocol or the screen at the other end. Keep
// them plain, copyable values: an actor's message is copied into a mailbox, so no heap, no ownership and
// no virtual anything.

namespace messages {

"#
    .to_string();
    for (name, users) in messages_of(application) {
        text.push_str(&format!(
            "/// TODO: what a `{name}` is — the fields that cross the edge, as plain values.\n\
             ///\n\
             /// Named by {}.\n\
             struct {name} {{}};\n\n",
            users.join(", ")
        ));
    }
    text.push_str("}  // namespace messages\n");
    text
}

/// The **compile check** a component of declarations needs: one translation unit that includes the
/// header and nothing else.
///
/// It is structural, and that is the point — a fill cannot remove it, so the types the composition names
/// are parsed by a chip build whatever else happens, and by the host test beside it. It is deliberately
/// **not** design-aware: what it proves is that the header parses on its own, with no framework, no
/// device and no board behind it, which is exactly the claim `components/messages/` makes.
const MESSAGES_COMPILE_CHECK: &str = r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// The compile check for the shared types: **this include is the check**. The header is declarations
// only, so there is nothing to run; what matters is that it parses here, on its own, with no framework
// and no device behind it — because every unit of this application, and `main/`, include it too.
//
// Keep this file. It is the one place a chip build parses `messages.hpp` whether or not a unit happens
// to name a given type, and the host test beside it builds this same translation unit.

#include <messages.hpp>

namespace {

/// Nothing to run: the include above is the whole check. A constant rather than a file the compiler is
/// handed nothing from.
[[maybe_unused]] const int messages_compile_check = 0;

}  // namespace
"#;

/// The shared types' **host test** — the fill's, like every other case in the tree.
///
/// The scaffold's own case is the one that is true of types nobody has filled in yet: each is
/// **complete**, and the name the design used resolves. That is all that can be said about a message
/// before its fields exist, and it is worth saying — a composition that names a type this header does
/// not declare fails to compile, and this is where it fails first.
fn messages_test(application: &ApplicationSpec) -> String {
    let mut text = r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// The shared types' **host test** — every type the design named, declared and complete.
//
// This is a component of declarations, so there is nothing here to compute. The cases that matter are
// about *meaning* — what a message carries, what a sender may assume a receiver sees, what a field
// defaults to — and the fill writes them as the fields arrive.
//
//   cmake -S <this dir> -B <this dir>/build && cmake --build <this dir>/build && ctest --test-dir <this dir>/build

#include <messages.hpp>

#include <cstddef>
#include <cstdio>

namespace {

int failures = 0;

void expect(bool ok, const char* what) {
    if (!ok) {
        std::printf("FAIL %s\n", what);
        ++failures;
    }
}

}  // namespace

int main() {
"#
    .to_string();
    for (name, users) in messages_of(application) {
        text.push_str(&format!(
            "    // `{name}` — named by {}. A message nothing can hold is a message nothing can send.\n\
             \x20   expect(sizeof(messages::{name}) > 0, \"{name} is a complete type\");\n",
            users.join(", ")
        ));
    }
    text.push_str(
        r#"
    // TODO: the cases that matter as the fields arrive — what a value carries by default, which of them
    // a sender may rely on, and what a receiver must be careful not to assume.

    if (failures != 0) {
        std::printf("%d case(s) failed\n", failures);
        return 1;
    }
    std::printf("ok\n");
    return 0;
}
"#,
    );
    text
}

/// The manifest of the shared **message types** component — depending on nothing, because a message is
/// plain data: the component that holds it needs the framework no more than a message does.
///
/// It compiles one translation unit, and that unit is the **compile check**: it includes the header and
/// nothing else, so a chip build parses the types the composition names rather than only the units that
/// happen to include them. `test/CMakeLists.txt` builds the same file on the host.
fn messages_component_manifest() -> String {
    r#"# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# The composition's **message types** — the one component every unit names, because the type on an edge
# belongs to the edge: a sender holds `spire::ActorRef<Receiver::Message>`, and a ramen stage pushes a
# value another stage pulls. It is a `library` kind of component on purpose — a message is plain data,
# and nothing in it needs a framework — and it is depended on by every unit, and by `main/`.
#
# The single source is the **compile check**, not code: it includes `include/messages.hpp` and
# nothing else, which is the one thing a component made of declarations can be checked for.
set(SPIRE_COMPONENT_KIND library)

idf_component_register(
    SRCS "src/messages_compile_check.cpp"
    INCLUDE_DIRS "include")
"#
    .to_string()
}

/// The manifest of one **actor** component — `actors` for the framework, `messages` for the types it
/// receives and posts, then every library component the reviewed design says this actor wraps.
///
/// A component the design marked `published` is **not** named here, for the reason `main/`'s manifest
/// does not name one: it is a managed dependency the component manager injects, under a build name this
/// file has no business guessing, and naming it would be a `REQUIRES` that does not resolve. See
/// [`push_used_components`].
fn actor_component_manifest(application: &ApplicationSpec, unit: &Unit) -> String {
    let mut requires: Vec<String> = vec!["actors".to_string(), MESSAGES_COMPONENT.to_string()];
    push_used_components(application, unit, &mut requires);
    format!(
        r#"# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# The actor `{id}` — **its own component**, so its message, its mailbox and its task are a module
# boundary rather than a corner of `main/`. The kind is stated here the way every component states it,
# and the fill writes the class (`include/{id}.hpp`, included as `#include <{id}.hpp>`) and
# the host cases beside it.
#
# `REQUIRES` is named here from the reviewed design and is **structural**: `actors` for the framework,
# `messages` for the types this actor receives and posts, then every library component it wraps — this
# actor's dependencies are its component's to state, not `main/`'s.
set(SPIRE_COMPONENT_KIND actor)

# Every source in `src/` is compiled, so a file added to this component needs no build change — an
# actor whose task and board handling outgrow its header simply gains one.
file(GLOB_RECURSE SPIRE_ACTOR_SOURCES "${{CMAKE_CURRENT_LIST_DIR}}/src/*.cpp")

idf_component_register(
    SRCS ${{SPIRE_ACTOR_SOURCES}}
    INCLUDE_DIRS "include"
    REQUIRES {requires})
"#,
        id = component_ident(&unit.id),
        requires = requires.join(" ")
    )
}

/// The manifest of one **ramen stage** component — `ramen` for the framework, `messages` for the values
/// on its edges, then every library component the reviewed design says this stage wraps.
///
/// The same three facts an actor's manifest states, and the same reason for each: the framework is
/// shipped rather than written here, the values it moves are shared because an edge belongs to neither
/// end of it, and the stage's own dependencies are its component's to state rather than `main/`'s.
fn stage_component_manifest(application: &ApplicationSpec, unit: &Unit) -> String {
    let mut requires: Vec<String> = vec!["ramen".to_string(), MESSAGES_COMPONENT.to_string()];
    push_used_components(application, unit, &mut requires);
    format!(
        r#"# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 NatureSense
#
# The ramen stage `{id}` — **its own component**, so the values it moves and the code between them are a
# module boundary rather than a corner of `main/`. The kind is stated here the way every component states
# it, and the fill writes the class (`include/{id}.hpp`, included as `#include <{id}.hpp>`) and
# the host cases beside it.
#
# `REQUIRES` is named here from the reviewed design and is **structural**: `ramen` for the framework,
# `messages` for the values that cross this stage's edges, then every library component it wraps.
set(SPIRE_COMPONENT_KIND stage)

# Every source in `src/` is compiled, so a file added to this component needs no build change.
file(GLOB_RECURSE SPIRE_STAGE_SOURCES "${{CMAKE_CURRENT_LIST_DIR}}/src/*.cpp")

idf_component_register(
    SRCS ${{SPIRE_STAGE_SOURCES}}
    INCLUDE_DIRS "include"
    REQUIRES {requires})
"#,
        id = component_ident(&unit.id),
        requires = requires.join(" ")
    )
}

/// The library components a unit **wraps**, as `REQUIRES` names them.
///
/// Shared by the actor and the stage manifest, because it is one question in both: which of the design's
/// components are this unit's to depend on. A `published` one is skipped — it is a managed dependency
/// the component manager injects, under a build name this file has no business guessing — and a name
/// already present stays where it is, so the framework's own components come first.
fn push_used_components(application: &ApplicationSpec, unit: &Unit, requires: &mut Vec<String>) {
    for used in &unit.uses {
        let published = application.units.iter().any(|candidate| {
            candidate.id == *used && candidate.source == Some(UnitSource::Published)
        });
        let ident = component_ident(used);
        if !published && !requires.contains(&ident) {
            requires.push(ident);
        }
    }
}

/// The **component kind** a unit of the composition is: an actor, or a ramen stage.
///
/// `None` for a `component`, which is the **library's** — the same distinction
/// [`ComponentKind::is_library_component`] draws for `add_component`, and the reason nothing below has to
/// invent a shape for one. Every caller filters on this before a file is generated, so an actor and a
/// stage are the only kinds a per-unit file is ever made for.
fn unit_component_kind(unit: &Unit) -> Option<ComponentKind> {
    match unit.kind {
        UnitKind::Actor => Some(ComponentKind::Actor),
        UnitKind::Stage => Some(ComponentKind::Stage),
        UnitKind::Component => None,
    }
}

/// The manifest of one **unit** component, by the kind of unit it is.
///
/// The two kinds state the same three things and differ only in the framework they name. A driver or a
/// library cannot reach here — `unit_component_kind` maps neither to a unit — and the arm says so rather
/// than answering with a manifest for the wrong kind; [`add_component`] is what writes those.
fn unit_component_manifest(
    application: &ApplicationSpec,
    unit: &Unit,
    kind: ComponentKind,
) -> String {
    match kind {
        ComponentKind::Actor => actor_component_manifest(application, unit),
        ComponentKind::Stage => stage_component_manifest(application, unit),
        ComponentKind::Driver | ComponentKind::Library => String::new(),
    }
}

/// One of a stage's **ports**: the value that crosses it, and which way.
///
/// The member name is the design's own name for the value with its side in front of it — `in_frames`,
/// `out_tensors` — so a stage that pulls and pushes the *same* value (a filter) has two ports rather than
/// one member declared twice, and the wiring in `main/` reads in the direction the value travels:
/// `capture.out_frames >> preprocess.in_frames`.
struct UnitPort {
    /// `in_<value>` or `out_<value>`, as the stage's header declares it.
    member: String,
    /// The value on that edge, as this application's shared types name it (`frames` → `Frames`).
    value: String,
    /// Whether this is the in-port. A stage is **pushed** the value it pulls — pushing is synchronous
    /// and inline, one task runs the whole chain, so the value arrives at the stage's own behaviour
    /// rather than being asked for later — and it **pushes** the value it hands on by calling the
    /// out-port.
    incoming: bool,
}

impl UnitPort {
    /// The ramen port type, as the header declares it: `ramen::Pushable<...>` in, `ramen::Pusher<...>`
    /// out — the framework's in-behavior and out-event, which is exactly the pair a chain links with
    /// `>>`.
    fn port_type(&self) -> String {
        let shape = if self.incoming { "Pushable" } else { "Pusher" };
        format!("ramen::{shape}<{}>", self.value_type())
    }

    /// The message type this port carries, as the shared-types header names it: `messages::Frames`.
    fn value_type(&self) -> String {
        format!("messages::{}", self.value)
    }

    /// The design's word for this side, for the line above the member.
    fn side(&self) -> &'static str {
        if self.incoming {
            "pulls"
        } else {
            "pushes"
        }
    }
}

/// A stage's **ports**, in the order the design names them: the in-port per `pulls`, the out-port per
/// `pushes`.
///
/// The two ends of a chain have one port each, and that is the design saying something: a source
/// (`capture`) only pushes, because a frame pump reads a device rather than being handed a value, and a
/// sink (`save_image`) only pulls, because the flow ends there.
fn unit_ports(unit: &Unit) -> Vec<UnitPort> {
    let mut ports = Vec::new();
    for (raw, incoming) in [
        (unit.pulls.as_deref().unwrap_or_default(), true),
        (unit.pushes.as_deref().unwrap_or_default(), false),
    ] {
        let value = message_type_name(raw);
        if value.is_empty() {
            continue;
        }
        ports.push(UnitPort {
            member: format!("{}_{}", if incoming { "in" } else { "out" }, ident_of(raw)),
            value,
            incoming,
        });
    }
    ports
}

/// A unit's **class**, as the file the fill starts from: the shape the reviewed design gave it, with a
/// `TODO` where its work goes.
///
/// It compiles as it stands, which is the property `add_component`'s stubs have and the reason this one
/// exists: an actor is declared `spire::Actor<messages::Tick>` for the message the design named, and a
/// stage declares one port per edge, typed by the value on it. Neither computes anything — everything a
/// unit *does* is the fill's — and what the scaffold can state is where the class goes, what it is
/// called, and which types it is written against.
fn unit_header(unit: &Unit, kind: ComponentKind) -> String {
    match kind {
        ComponentKind::Actor => actor_unit_header(unit),
        ComponentKind::Stage => stage_unit_header(unit),
        // A driver or a library is the **library's**: `unit_component_kind` maps neither to a unit, and
        // `component_files` is what writes those headers. Empty rather than a shape for a directory this
        // module does not emit.
        ComponentKind::Driver | ComponentKind::Library => String::new(),
    }
}

/// An **actor**'s header: a `spire::Actor<Message>` and an empty `on_message`.
///
/// The lines that are not a stub are the framework's own: the message type is fixed by the design, and
/// `on_message` runs on the actor's **own task** — which is the whole difference from a dataflow stage,
/// where the producer runs the consumer.
fn actor_unit_header(unit: &Unit) -> String {
    let ident = component_ident(&unit.id);
    let id = unit.id.as_str();
    let type_name = type_name(&ident);
    let message = message_type_name(unit.message.as_deref().unwrap_or_default());
    let state = unit.state.as_deref().unwrap_or("(the design states none)");
    format!(
        r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <actor.hpp>

#include <messages.hpp>

namespace {ident} {{

/// TODO: what the actor `{id}` is for, in the terms a person reading `main/` thinks in — and what it is
/// not.
///
/// **This is a stub.** An actor is the message type it receives, the state it holds between messages and
/// the refs it sends to. The message and the peers are the composition's — the reviewed design, and the
/// wiring in `main/` — and the state is the sentence the design wrote for it:
///
///   `{state}`
///
/// which describes this actor rather than claiming anything about its fields. Everything the actor *does*
/// is the `on_message` below.
///
/// What is fixed is the shape, and the shape is the framework's:
///
///  * it is a `spire::Actor<messages::{message}>` — the message the design named, which is what it
///    receives, never a type it invents. A sender elsewhere holds `ActorRef<messages::{message}>` and
///    posts **that** type, so this declaration is the boundary, written down where both sides can see it;
///  * `on_message` runs on the **actor's own task**: it may take as long as this actor needs and holds
///    nobody else up. That is why a mailbox and a task are worth their cost here, and why a slow actor is
///    not a slow system;
///  * the component it wraps stays framework-agnostic: it holds a device or an algorithm and knows no
///    task, no mailbox and no framework. The actor is the layer allowed to know those;
///  * the refs it **sends to** are `spire::ActorRef<Peer::Message>` **constructor arguments** — an
///    actor is *constructed* with the peers it reaches and never fetches one by name. `main/` spawns
///    each peer **before** this actor (a receiver before its sender) and passes its ref at `spawn`;
///  * the one peer it **cannot** be constructed with is a **cycle** — two actors that must reach each
///    other — and that peer alone is resolved **by name** in `init()`, through a `const
///    spire::Registry&` **constructor argument** kept as a member, which `main/` hands over at `spawn`
///    as `*scheduler.registry()`. The composition's cycle note gives that in full. There is **no
///    `spire::Registry::instance()` and no static `spire::Registry::get`** — a registry is never a
///    singleton and never a temporary, and `spire::Registry()` does not compile. Return `false` to
///    refuse to start.
///
/// TODO: the state (as members), the constructor's arguments (the peer refs above), and what
/// `on_message` does with a message. The design names which peers those are, and `main/` is what
/// hands them over at `spawn`.
class {type_name} final : public spire::Actor<messages::{message}> {{
public:
    // TODO: replace this with the constructor that takes the peers above — `main/` spawns each
    // receiver before its sender and passes its ref here. The no-argument form below is the stub's.
    {type_name}() = default;

protected:
    void on_message(const messages::{message}& message) override {{
        // TODO: what this actor does with a message — including the `ActorRef`s it sends to, which are
        // constructor arguments, and whatever work belongs to the component it holds.
        (void)message;
    }}
}};

}}  // namespace {ident}
"#
    )
}

/// A **stage**'s header: one port per edge, typed by the value that crosses it.
///
/// The shape is the framework's: pushing is synchronous and inline, so one task runs the whole chain, the
/// value arrives at this stage's own behaviour, and the chain is a DAG — never a cycle, which would be
/// unbounded recursion on one stack.
fn stage_unit_header(unit: &Unit) -> String {
    let ident = component_ident(&unit.id);
    let id = unit.id.as_str();
    let type_name = type_name(&ident);
    let ports = unit_ports(unit);
    let edges = if ports.is_empty() {
        "(the design states no port)".to_string()
    } else {
        ports
            .iter()
            .map(|port| {
                format!(
                    "`{}` — {side} `{value}`",
                    port.member,
                    side = port.side(),
                    value = port.value
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = format!(
        r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <ramen.hpp>

#include <messages.hpp>

namespace {ident} {{

/// TODO: what the stage `{id}` is for, in the terms a person reading `main/` thinks in — and what it is
/// not.
///
/// **This is a stub.** A stage is what it pulls, what it pushes and the code between them. The values and
/// the peers are the composition's — the reviewed design states this stage's ports:
///
///   {edges}
///
/// and `main/` is where they are wired to the rest of the chain. The code between them is the fill's.
///
/// What is fixed is the shape, and the shape is the framework's:
///
///  * **pushing is synchronous and inline**: one task runs the whole chain, so this stage's behaviour runs
///    on whoever pushed into it, and the chain is a DAG — never a cycle, which would be unbounded
///    recursion on one stack;
///  * the **in-port is a `ramen::Pushable`** (`in_<value>`): the framework invokes it with the value it
///    was pushed, which is what "pulls" means here — the value arrives at this stage rather than being
///    fetched;
///  * the **out-port is a `ramen::Pusher`** (`out_<value>`): calling it hands the value to everything
///    wired to it, before the call returns. The `>>`s are written in `main/`, where the chain is;
///  * the component it wraps stays framework-agnostic: a device or an algorithm knows no port.
///
/// TODO: the behaviour — what this stage does with what it pulls, and what it pushes on. A stage with only
/// in-ports is where a chain ends; one with only out-ports is where it starts (a pump that reads a device
/// and pushes).
class {type_name} final {{
public:
"#
    );
    for port in &ports {
        let value_ident = ident_of(&port.value);
        if port.incoming {
            text.push_str(&format!(
                "    /// The value this stage **{side}**: `{value}`.\n\
                 \x20   /// Whatever produces it is wired to this port in `main/`.\n\
                 \x20   {port_type} {member} = [](const messages::{value}& {param}) {{\n\
                 \x20       // TODO: the work — and the value it pushes on, as `out_<value>(...)` (which\n\
                 \x20       // needs `this` in the capture list above).\n\
                 \x20       (void){param};\n\
                 \x20   }};\n\n",
                side = port.side(),
                value = port.value,
                param = value_ident,
                port_type = port.port_type(),
                member = port.member,
            ));
        } else {
            text.push_str(&format!(
                "    /// The value this stage **{side}**: `{value}`.\n\
                 \x20   {port_type} {member}{{}};\n\n",
                side = port.side(),
                value = port.value,
                port_type = port.port_type(),
                member = port.member,
            ));
        }
    }
    text.push_str(&format!("}};\n\n}}  // namespace {ident}\n"));
    text
}

/// A unit's **host test**, with the one case that is true of a unit nobody has filled in yet.
///
/// For an actor: it is spawned, it starts, and its mailbox takes the message the design named. For a stage:
/// every port is there, carries the value that crosses that edge, and the links compile. Both are claims
/// about the shape the scaffold wrote — the only thing it can claim — and both fail loudly when a rename
/// on one side of a boundary is not matched on the other.
fn unit_test(unit: &Unit, kind: ComponentKind) -> String {
    match kind {
        ComponentKind::Actor => actor_unit_test(unit),
        ComponentKind::Stage => stage_unit_test(unit),
        // A driver's test runs against a fake bus and a library's is a table of cases; `driver_files` and
        // `library_files` are what write those, and neither is a unit of a composition.
        ComponentKind::Driver | ComponentKind::Library => String::new(),
    }
}

/// An **actor**'s host test: the design's message, and the actor started on a scheduler.
fn actor_unit_test(unit: &Unit) -> String {
    let ident = component_ident(&unit.id);
    let id = unit.id.as_str();
    let type_name = type_name(&ident);
    let message = message_type_name(unit.message.as_deref().unwrap_or_default());
    format!(
        r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// `{id}`'s **host test** — the actor run on this machine, with the framework's fake FreeRTOS underneath.
//
// The scaffold's own case is the one every unfilled actor passes: it is spawned, it starts, and its mailbox
// takes the message the design named. Nothing is claimed about what it *does* with one — that is the fill's,
// and the cases for it are written below the line.
//
// Two things are worth knowing about this harness, and both are why a unit has a component of its own
// rather than a file in `main/`:
//
//  * `spire::Scheduler` owns the actor's mailbox and its task, so a case is `spawn`, `send`, and what
//    arrived — on **another task**, which is why a case that observes something polls for it. The library's
//    own `components/actors/test/actors_test.cpp` is the worked example of that;
//  * the FreeRTOS underneath is the **fake** the library ships, so this runs anywhere: no board, no chip, no
//    IDF. `scheduler.stop()` releases the actors and `spire_fake_join_tasks()` is what joins the threads,
//    in that order.

#include <{ident}.hpp>

#include <actor.hpp>
#include <scheduler.hpp>
#include <freertos_fake.hpp>
#include <messages.hpp>

#include <cstdio>
#include <type_traits>

namespace {{

int failures = 0;

void expect(bool ok, const char* what) {{
    if (!ok) {{
        std::printf("FAIL %s\n", what);
        ++failures;
    }}
}}

}}  // namespace

int main() {{
    // The design's message on the design's actor. Every sender in this application is written against this
    // boundary — `ActorRef<messages::{message}>` — so it is worth a compile-time case of its own.
    static_assert(std::is_base_of_v<spire::Actor<messages::{message}>, {ident}::{type_name}>,
                  "`{id}` is an actor on `{message}` — the message the reviewed design named");

    spire::Scheduler scheduler;
    auto ref = scheduler.spawn<messages::{message}, {ident}::{type_name}>("{id}", 4096, 5, tskNO_AFFINITY);
    expect(scheduler.start(), "the actor starts");
    expect(ref.send(messages::{message}{{}}), "and its mailbox takes the message the design named");
    scheduler.stop();
    spire_fake_join_tasks();

    // TODO: the cases that matter — what a message does to this actor's state, what it sends to the peer the
    // design names, what it refuses in `init()`. Each one is a `send` and a wait for what changed.

    if (failures != 0) {{
        std::printf("%d case(s) failed\n", failures);
        return 1;
    }}
    std::printf("ok\n");
    return 0;
}}
"#
    )
}

/// A **stage**'s host test: the ports the design named, and the chain linking the way `main/` will.
fn stage_unit_test(unit: &Unit) -> String {
    let ident = component_ident(&unit.id);
    let id = unit.id.as_str();
    let type_name = type_name(&ident);
    let ports = unit_ports(unit);
    let mut text = format!(
        r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// `{id}`'s **host test** — the stage, and the two ends of the chain it sits in, on this machine.
//
// The scaffold's own cases are about the **shape**: every port the design named is declared, it carries the
// value that crosses that edge, and the `>>` links compile in the direction the value travels. What this
// stage *makes* of a value is the fill's, and the cases for it belong here too — a push, and what arrived
// downstream — because pushing is **synchronous and inline**: by the time the push returns, this stage has run.
//
// Nothing here needs a board, a task or a fake. The framework is a header and a chain is a chain of calls,
// which is what makes a stage testable on this machine the same way it is on the chip.

#include <{ident}.hpp>

#include <messages.hpp>
#include <ramen.hpp>

#include <cstdio>
#include <type_traits>

namespace {{

int failures = 0;

void expect(bool ok, const char* what) {{
    if (!ok) {{
        std::printf("FAIL %s\n", what);
        ++failures;
    }}
}}

}}  // namespace

int main() {{
    {ident}::{type_name} stage;

    // The ports, as the types they have to be — the design's word for each edge, and the value on it:
"#
    );
    for port in &ports {
        text.push_str(&format!(
            "    expect(std::is_same_v<decltype(stage.{member}), {port_type}>,\n\
             \x20          \"`{member}` is the {side}-port of the design: `{value_type}`\");\n",
            member = port.member,
            port_type = port.port_type(),
            side = port.side(),
            value_type = port.value_type(),
        ));
    }
    if let Some(incoming) = ports.iter().find(|port| port.incoming) {
        text.push_str(&format!(
            "\n    // Something that pushes the value this stage pulls, wired to its in-port. `main/` wires the\n\
             \x20   // real one; this is the shape of that line.\n\
             \x20   ramen::Pusher<{value_type}> upstream;\n\
             \x20   upstream >> stage.{member};\n",
            value_type = incoming.value_type(),
            member = incoming.member,
        ));
    }
    if let Some(outgoing) = ports.iter().find(|port| !port.incoming) {
        text.push_str(&format!(
            "\n    // And something that takes the value this stage pushes, wired to its out-port.\n\
             \x20   ramen::Pushable<{value_type}> downstream = [](const {value_type}& {value}) {{\n\
             \x20       (void){value};\n\
             \x20   }};\n\
             \x20   stage.{member} >> downstream;\n",
            value_type = outgoing.value_type(),
            value = ident_of(&outgoing.value),
            member = outgoing.member,
        ));
    }
    text.push_str(
        r#"
    // TODO: the cases that matter — what this stage makes of a value it pulls, and what it pushes on. A case
    // is `upstream(value)` followed by what arrived at `downstream`, because the push runs this stage inline.

    if (failures != 0) {
        std::printf("%d case(s) failed\n", failures);
        return 1;
    }
    std::printf("ok\n");
    return 0;
}
"#,
    );
    text
}

/// The framework's own sources a unit's host test links, as paths under the library's `components/`.
///
/// An **actor** is a mailbox and a task, so its test links the same three files the framework's own test
/// links: the scheduler, the task seam it stands on, and the fake FreeRTOS that stands in for the chip's. A
/// **stage** links none — ramen is a header, and a chain is a chain of calls.
fn framework_test_sources(kind: ComponentKind) -> &'static [&'static str] {
    match kind {
        ComponentKind::Actor => &[
            "actors/src/scheduler.cpp",
            "actors/test/freertos_fake.cpp",
            "toolkit/src/task.cpp",
        ],
        ComponentKind::Stage => &[],
        ComponentKind::Driver | ComponentKind::Library => &[],
    }
}

/// The directories that must come **first** on a unit's include path, because they stand in for something the
/// test does not have: the fake FreeRTOS under the `actors` framework.
fn framework_test_dirs(kind: ComponentKind) -> &'static [&'static str] {
    match kind {
        ComponentKind::Actor => &["actors/test"],
        ComponentKind::Stage => &[],
        ComponentKind::Driver | ComponentKind::Library => &[],
    }
}

/// The framework's headers a unit includes, as paths under the library's `components/`.
fn framework_include_dirs(kind: ComponentKind) -> &'static [&'static str] {
    match kind {
        ComponentKind::Actor => &["actors/include", "toolkit/include"],
        ComponentKind::Stage => &["ramen/include"],
        ComponentKind::Driver | ComponentKind::Library => &[],
    }
}

/// The paragraph a generated unit test build's header comment carries — the one thing that differs between an
/// actor's harness and a stage's, and the reason it is not written out in the template.
fn unit_test_why(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Actor => {
            "# An actor is a mailbox and a task, so a test that only constructed it would prove nothing: this\n\
             # build links the framework's own sources and the fake FreeRTOS underneath, which is what makes\n\
             # `scheduler.spawn<...>` runnable here. Every component this actor wraps is linked in as well."
        }
        ComponentKind::Stage => {
            "# A stage is a chain of calls — pushing is synchronous and inline, one task runs the whole chain —\n\
             # so this build needs no framework source at all: the header is the framework. Every component this\n\
             # stage wraps is linked in, with its fake bus ahead on the include path."
        }
        ComponentKind::Driver | ComponentKind::Library => "",
    }
}

/// A unit's **host test build**, from `templates/esp-idf/application/unit/test/CMakeLists.txt`.
///
/// Rendered rather than declared because most of what is in it is **design facts**: which framework the unit is
/// written in, which library the application is built against, and which of the library's components the unit
/// wraps — each of which changes what is linked and what is on the include path.
///
/// It is the build the library's own components have, one level out. A component's test compiles the component
/// and its fake; a **unit**'s compiles the unit *and the framework it is written in*. Everything the design
/// says the unit wraps comes in too, with its fake bus **ahead** on the include path, exactly as in that
/// component's own test — because a unit that holds a driver is testable only if that driver's device is
/// scripted the same way there.
fn unit_test_manifest(application: &ApplicationSpec, unit: &Unit, library: &str) -> String {
    let Some(kind) = unit_component_kind(unit) else {
        // A `component` is the library's, and its host test build is `component_files`' — the same
        // distinction `unit_component_manifest` draws, drawn once.
        return String::new();
    };
    let ident = component_ident(&unit.id);
    let library = library.trim();
    // The components the **library** owns and this unit wraps. A `published` one is a managed dependency with
    // no directory here, so there is nothing to compile and nothing to put on an include path.
    let uses: Vec<String> = unit
        .uses
        .iter()
        .filter(|used| {
            !application.units.iter().any(|candidate| {
                candidate.id.as_str() == used.as_str()
                    && candidate.source == Some(UnitSource::Published)
            })
        })
        .map(|used| component_ident(used))
        .collect();

    let component_dir = |path: &str| format!("    ${{SPIRE_LIBRARY_DIR}}/components/{path}\n");
    let mut source_globs = String::new();
    let mut fakes = String::new();
    let mut headers = String::new();
    for used in &uses {
        source_globs.push_str(&component_dir(&format!("{used}/src/*.cpp")));
        fakes.push_str(&component_dir(&format!("{used}/test")));
        headers.push_str(&component_dir(&format!("{used}/include")));
    }
    let mut framework_sources = String::new();
    let mut test_dirs = String::new();
    let mut include_dirs = String::new();
    // Nothing of the framework's can be reached when no library was named: the framework comes from it.
    if !library.is_empty() {
        for source in framework_test_sources(kind) {
            framework_sources.push_str(&component_dir(source));
        }
        for dir in framework_test_dirs(kind) {
            test_dirs.push_str(&component_dir(dir));
        }
        for dir in framework_include_dirs(kind) {
            include_dirs.push_str(&component_dir(dir));
        }
    }
    test_dirs.push_str(&fakes);
    include_dirs.push_str(&headers);

    let library_resolution = if library.is_empty() {
        "# No library was named, so there is no framework component to reach and no library component to link:\n\
         # this application's components are its own. Name the library here — and in the application's\n\
         # `CMakeLists.txt` — when there is one."
            .to_string()
    } else {
        format!(
            "set(SPIRE_LIBRARY_DIR \"{library}\")\n\
             if(NOT IS_ABSOLUTE \"${{SPIRE_LIBRARY_DIR}}\")\n\
             \x20   set(SPIRE_LIBRARY_DIR \"${{CMAKE_CURRENT_SOURCE_DIR}}/../../../${{SPIRE_LIBRARY_DIR}}\")\n\
             endif()"
        )
    };
    let used_sources = if uses.is_empty() {
        String::new()
    } else {
        format!("file(GLOB_RECURSE SPIRE_USED_SOURCES\n{source_globs})\n\n")
    };
    let used_sources_list = if uses.is_empty() {
        String::new()
    } else {
        "    ${SPIRE_USED_SOURCES}\n".to_string()
    };
    // A mailbox is a FreeRTOS queue and a task is a thread here, so an actor's harness links the threads the
    // framework's own test links. A stage's needs none.
    let threads = if kind == ComponentKind::Actor {
        format!(
            "find_package(Threads REQUIRED)\n\
             target_link_libraries({ident}_test PRIVATE Threads::Threads)\n\n"
        )
    } else {
        String::new()
    };
    let what = if kind == ComponentKind::Actor {
        "actor"
    } else {
        "stage"
    };
    fill_component_template(
        include_str!("../../templates/esp-idf/application/unit/test/CMakeLists.txt"),
        &[
            ("__IDENT__", ident),
            ("__WHAT__", what.to_string()),
            ("__WHY__", unit_test_why(kind).to_string()),
            ("__LIBRARY_RESOLUTION__", library_resolution),
            ("__USED_SOURCES__", used_sources),
            ("__FRAMEWORK_SOURCES__", framework_sources),
            ("__USED_SOURCES_LIST__", used_sources_list),
            ("__TEST_DIRS__", test_dirs),
            ("__INCLUDE_DIRS__", include_dirs),
            ("__THREADS__", threads),
        ],
    )
}

/// The application's **managed dependencies**, as `main/idf_component.yml` — or `None` when it has
/// none.
///
/// The board's BSP is the important one: it carries the pins, the display and the touch panel, and it
/// comes from the registry rather than from the library, because a BSP is a third-party board's support
/// package and not something a project writes. A component the design marked `published` is listed
/// beside it for the same reason.
///
/// **In `main/`, not at the project root**, and that is the whole design: the ESP-IDF component manager
/// *injects* a component's manifest dependencies into that component's `REQUIRES`, so a manifest in
/// `main/` is what lets `main.cpp` include the BSP's headers — without `main/CMakeLists.txt` naming
/// them, which it may not, because it is structural. (The manager adds them under `MANAGED_REQUIRES`
/// first, so the namespaced component names reach the build correctly.) The bare name the `.bsp` may
/// carry (`m5stack_core_s3`) is accepted here: the manifest grammar resolves it against `espressif`.
///
/// The **library**, by contrast, is still reached as a plain directory (see `__LIBRARY_BLOCK__`): it is
/// a project on this machine, and a build that has to reach the network to find a sibling is a build
/// that fails on a bench with no wifi. What is managed here is only the third-party board support,
/// which has no local copy to reach for.
///
/// This is the scaffold's **initial** statement, written before the composition exists: a design that
/// named a board gets its BSP, and the fill has yet to say whether it drives any of it.
/// [`finalize_application_manifest`] is the second pass — it runs once the tree is written and drops
/// the board from a composition that reaches for no board support.
fn application_manifest(application: Option<&ApplicationSpec>) -> Option<(String, String)> {
    let application = application?;
    // The design **stated** a board, so the scaffold writes its BSP while the composition is still
    // unwritten — see `finalize_application_manifest` for the pass that corrects this against what the
    // fill actually reached for.
    let deps = managed_dependencies(application, true);
    if deps.is_empty() {
        return None;
    }
    Some((MANIFEST_FILE.to_string(), manifest_content(&deps)))
}

/// The dependency list the manifest declares: the board's BSP when `board_support` says it is wanted,
/// then every component the design marked `published` — the board first, the third-party runtime beside
/// it, which is the order a reader checks them in.
///
/// The BSP is the only dependency that is ever **conditional**, because it is the only one that is a
/// *guess* at scaffold time: a design names a board, but only the composition says whether it drives
/// the board's peripherals. A `published` component is declared, not inferred, so it is always here.
fn managed_dependencies(application: &ApplicationSpec, board_support: bool) -> Vec<String> {
    let mut deps: Vec<String> = Vec::new();
    let bsp = application.board.bsp.trim();
    if board_support && !bsp.is_empty() {
        deps.push(bsp.to_string());
    }
    for unit in &application.units {
        if unit.kind != UnitKind::Component || unit.source != Some(UnitSource::Published) {
            continue;
        }
        if let Some(registry) = unit.registry.as_deref().map(str::trim) {
            if !registry.is_empty() && !deps.iter().any(|known| known == registry) {
                deps.push(registry.to_string());
            }
        }
    }
    deps
}

/// The `main/idf_component.yml` body for a dependency set — shared by the scaffold, which writes the
/// design's initial statement, and [`finalize_application_manifest`], which corrects it against the
/// composition. One renderer, so the two passes cannot disagree about the file they are writing.
fn manifest_content(deps: &[String]) -> String {
    let mut content = String::from(
        "# The application's **managed dependencies** — the components it uses but does not own.\n\
         #\n\
         # The board support package carries the pins, the display and the touch panel; a component the\n\
         # design marked `published` is here for the same reason. The ESP-IDF component manager resolves\n\
         # them at *build* time and adds them to this component's `REQUIRES`, so `main.cpp` can include\n\
         # their headers. The **library**, by contrast, is reached as a plain directory (the project's\n\
         # root `CMakeLists.txt`), because a build that reaches the network to find a sibling is a build\n\
         # that fails on a bench with no wifi — only the third-party board support is managed here.\n\
         dependencies:\n",
    );
    for dep in deps {
        content.push_str(&format!("  {dep}: \"*\"\n"));
    }
    content
}

/// The headers a composition reaches for **board support** with: the BSP's own headers, and those of
/// the peripherals it transitively provides — the display stack (LVGL), the LCD and touch drivers, the
/// audio codec, the camera/video stack, the IMU and the IO expander.
///
/// The BSP is the one component that provides **all** of them, so a composition that includes any one
/// of them is a composition whose build needs the BSP. This decides "board support, or not" — never
/// *which* driver to pin — which is why a single table answers it.
const BOARD_SUPPORT_HEADERS: &[&str] = &[
    "bsp/",
    "esp-bsp",
    "esp_bsp",
    "lvgl",
    "esp_lvgl_port",
    "esp_lcd",
    "esp_codec_dev",
    "esp_cam_sensor",
    "esp_video",
    "usb_host_uvc",
    "bmi270",
    "sensor_hub",
    "esp_io_expander",
];

/// Whether the composition written into `main/` reaches for board support at all.
///
/// Read from the **sources on disk**, so it answers the question the composition actually poses rather
/// than the one the plan meant to pose. Two shapes count as reaching for it: an `#include` of a
/// board-support header (the table above), or the BSP's own API prefix `bsp_` — a use, whatever header
/// the declaration came in through.
///
/// The `#include` check is **line-based on purpose**: a comment that says "the display is not wired up
/// yet" is not a use of the display, and a substring search over the whole text would bring eighteen
/// components back for it.
pub fn board_support_referenced(root: &Path) -> bool {
    let mut text = String::new();
    collect_source_text(&root.join("main"), &mut text);
    if text.contains("bsp_") {
        return true;
    }
    text.lines()
        .filter(|line| line.contains("#include"))
        .any(|line| {
            BOARD_SUPPORT_HEADERS
                .iter()
                .any(|header| line.contains(header))
        })
}

/// **The manifest, corrected against what the fill actually wrote.**
///
/// The scaffold states the design's dependencies before the composition exists — the board's BSP
/// unconditionally, because a design that named a board is a design that intends to use it. The fill is
/// what decides whether it *did*: in a live run the composition stubbed the display and the touch panel,
/// so the BSP — and the components it carries — was downloaded, compiled and then dead-stripped, which
/// is the first build's minutes rather than its seconds.
///
/// So the decision is made here instead, once the tree is written and before the build: read the sources
/// under `main/` ([`board_support_referenced`]), keep the components the design marked `published`
/// (they are stated, not inferred), and keep the board's BSP only when the composition reaches for
/// board support. A composition that touches no peripheral pins nothing and has no manifest at all — the
/// empty file that would promise a dependency nobody declared is removed.
///
/// Returns the dependencies the manifest now declares, so a caller can report them.
pub fn finalize_application_manifest(
    root: &Path,
    application: &ApplicationSpec,
) -> Result<Vec<String>, String> {
    let deps = managed_dependencies(application, board_support_referenced(root));
    let manifest = root.join(MANIFEST_FILE);
    if deps.is_empty() {
        if manifest.is_file() {
            std::fs::remove_file(&manifest)
                .map_err(|e| format!("could not remove {}: {e}", manifest.display()))?;
        }
        return Ok(deps);
    }
    if let Some(parent) = manifest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&manifest, manifest_content(&deps))
        .map_err(|e| format!("could not write {}: {e}", manifest.display()))?;
    Ok(deps)
}

/// Whether the **composition the design reviewed actually reached the tree** — and what is missing when
/// it did not.
///
/// The prompt asks the model for the reviewed composition, and a model can answer with a *plausible
/// application* instead: a live run ignored the `actors` framework entirely — one flat FreeRTOS poll
/// loop, `xTaskCreate`, and its own `Sps30`/`Sht20` classes in `main/sensors.h` — although the design
/// block, the framework block and the library's own headers were all in it. Nothing downstream catches
/// that, because a flat loop **compiles**. So the check is structural, and on the two things that are
/// checkable without reading the code as a person would: the framework's own markers appear in the
/// application's sources, and a component the **library** owns is not re-declared in them.
///
/// The sources are read from **both** trees the composition occupies — `main/` and `components/` — for
/// the reason the markers moved there: since each unit is a component of its own, `main/` holds the
/// wiring and the board facts, and the framework's idiom lives under `components/<unit>/`.
///
/// Deliberately not a compiler: it never claims a composition is *right*, only that it is not obviously
/// *absent*. What it cannot see — an actor that exists but whose wiring is wrong — is the build's and
/// the reader's.
pub fn composition_gaps(root: &Path, application: &ApplicationSpec) -> Vec<String> {
    let mut text = String::new();
    // The composition now spans **two** trees: `main/` (the wiring and the board facts) and
    // `components/` (one directory per actor, and the shared message types). A check that read only
    // `main/` would call a correctly-written composition absent, because the framework's markers and
    // the actors' classes are exactly what moved out of it.
    collect_source_text(&root.join("main"), &mut text);
    collect_source_text(&root.join(COMPONENTS_DIR), &mut text);
    if text.trim().is_empty() {
        return vec![
            "the composition was never written: neither `main/` nor `components/` has a source"
                .to_string(),
        ];
    }

    let mut gaps: Vec<String> = Vec::new();

    // 1. The framework's own markers. A tree that mentions none of them has written *an* application,
    //    not this framework's.
    let (markers, what): (&[&str], &str) = match application.framework {
        ApplicationFramework::Actors => (
            &["spire::", "actor.hpp", "registry.hpp"],
            "spire::Actor / spire::Scheduler / spire::Registry",
        ),
        ApplicationFramework::Ramen => (&["ramen::", "ramen.hpp"], "ramen"),
    };
    if !markers.iter().any(|marker| text.contains(marker)) {
        gaps.push(format!(
            "the composition uses no `{what}`: it was written as something other than the reviewed \
             framework — a bare loop, say — rather than with it"
        ));
    }

    // 2. A component the **library** owns, re-declared in the application. The design's `existing` and
    //    `stub` components *are* the library's, so a second copy anywhere in the application — in
    //    `main/` or in one of its own actor components — is a second copy of the same driver, free to
    //    disagree with the first.
    for unit in application.units.iter().filter(|unit| {
        unit.kind == UnitKind::Component && unit.source != Some(UnitSource::Published)
    }) {
        for name in type_names(&unit.id) {
            if declares_a_type(&text, &name) {
                gaps.push(format!(
                    "the application declares `{name}`, which is the library's `{}` — include the \
                     component (`#include <{}.hpp>`) instead of writing a second copy of it",
                    unit.id, unit.id
                ));
            }
        }
    }

    gaps
}

/// The type names a component id could be written as: the id, its `class` capitalisation, and its upper
/// case. `sps30` → `sps30`, `Sps30`, `SPS30`.
fn type_names(id: &str) -> Vec<String> {
    let ident = id.replace('-', "_");
    let mut names = vec![ident.clone()];
    let mut chars = ident.chars();
    if let Some(first) = chars.next() {
        names.push(format!("{}{}", first.to_ascii_uppercase(), chars.as_str()));
    }
    names.push(ident.to_ascii_uppercase());
    names.sort();
    names.dedup();
    names
}

/// Whether `text` declares a `class`/`struct` called `name` — the shape a re-declared component takes.
/// The name has to **end** at the match, so `class Sps30x` is a different type and not one of these.
fn declares_a_type(text: &str, name: &str) -> bool {
    ["class ", "struct "].iter().any(|keyword| {
        let needle = format!("{keyword}{name}");
        let mut from = 0;
        while let Some(at) = text[from..].find(&needle) {
            let after = from + at + needle.len();
            let ends = text[after..]
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric() && c != '_')
                .unwrap_or(true);
            if ends {
                return true;
            }
            from = after;
        }
        false
    })
}

/// Every source under `dir`, concatenated — what a fill may have written into the application. Only the
/// source extensions: a `.md`, a `.json` or a build directory is not the composition.
fn collect_source_text(dir: &Path, into: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_text(&path, into);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "h" | "hpp" | "c" | "cpp" | "cc" | "cxx"))
        {
            if let Ok(content) = std::fs::read_to_string(&path) {
                into.push_str(&content);
                into.push('\n');
            }
        }
    }
}

/// The application skeleton writes this in place of the framework block when the design phase named
/// one — the *statement* the fill phase, a person and another tool all read back.
///
/// `set(...)` rather than a comment, deliberately, and for the reason the project type is stated the
/// same way: the composition this application will contain is one framework's idiom, and a reader that
/// had to *infer* which one from the shape of `main/` would be guessing at exactly the thing the
/// design phase decided.
fn framework_block(framework: Option<ApplicationFramework>) -> String {
    match framework {
        Some(framework) => format!(
            "# The framework this application is written in, **stated rather than guessed** — chosen in\n\
             # the design phase and reviewed before any of it was written. A component stays\n\
             # framework-agnostic; the composition in `main/` is this one's.\n\
             {}\n",
            framework.marker_line()
        ),
        None => "# No framework stated: this application was made without a design phase, so the library's\n\
                 # rule for choosing one applies, and whatever is chosen belongs here — as\n\
                 # `set(SPIRE_APPLICATION_FRAMEWORK actors)`, or `ramen`.\n"
            .to_string(),
    }
}

/// The **framework** an application's fill prompt is given a block for.
///
/// This is the design phase reaching the model that writes the composition, and it is the whole reason
/// the choice is recorded: a fill prompt otherwise hands the model a library's hints — which describe
/// *both* frameworks and how to choose between them — and the model chooses again, differently, per
/// run. What the model is told here is that the choice is already made and reviewed, what it means in
/// the one or two sentences that make it unmistakable, and (when there is no choice yet) that the
/// library's rule still applies and where the answer belongs.
pub fn framework_prompt_block(framework: Option<ApplicationFramework>) -> String {
    match framework {
        Some(ApplicationFramework::Actors) => "FRAMEWORK: `actors` — already chosen and reviewed. Do \
             not choose again and do not mix.\n\
             - An **actor** is the message type it receives, the state it holds between messages, and \
             the refs it sends to: `spire::Actor<Message>` with `on_message`, spawned on a \
             `spire::Scheduler` that owns its mailbox and its task.\n\
             - An **edge carries the receiver's message, not the sender's**: for `a -> b`, `a` holds \
             `spire::ActorRef<b::Message>` and posts **that** type. A unit's own message is what is \
             posted *to it*, never what it posts onwards — so an actor whose own message is `TouchEvent` \
             and which sends to an actor taking `Reading` holds `ActorRef<Reading>` and sends a \
             `Reading`. Two units pointing at one actor post the same type; anything else does not \
             compile.\n\
             - A ref you are **handed** is passed at `spawn`, and by default every ref is handed: \
             **spawn a receiver before its sender** — for `a -> b` spawn `b` first, then pass its ref \
             to `a` at `a`'s `spawn`, which is also the order they must start in. A peer you \
             **cannot** be handed one for — two actors that must reach each other (a **cycle**) — is \
             resolved **by name** in `init()`. Take a `const spire::Registry&` **constructor \
             parameter**, keep it as a \
             member, and have `main` pass `*scheduler.registry()` at that actor's `spawn` (the scheduler \
             returns a `std::shared_ptr<Registry>`, so dereference it). In `init()` write `peer_ = \
             registry_.get<PeerMessage>(\"peer-id\"); return peer_.valid();`. A registry is **only** ever \
             the scheduler's: there is no `spire::Registry::instance()`, no static \
             `spire::Registry::get`, and `spire::Registry()` does not compile — never a global, never a \
             temporary. **Never resolve in the constructor**: the registry is complete only after every \
             spawn, and `init()` is that moment. A name is the actor's `spawn` name, and `get` is checked \
             against the message type — an unknown name or a wrong type is an invalid ref, which fails \
             `init()` and so fails `start()`.\n\
             - A **component** stays framework-agnostic — it holds a device or an algorithm and knows \
             no task, no mailbox and no framework. An actor wraps it.\n\
             - **Every actor is a component of its own**: write the class to `components/<id>/include/<id>.hpp`, included as `#include <sampler.hpp>` for an actor named `sampler` — that component's `CMakeLists.txt` is already written and is structural, so do not edit it. The **message types are shared**: every actor's `message`, and every type posted along an edge, goes in `components/messages/include/messages.hpp` (`#include <messages.hpp>`), because a sender holds `ActorRef<Receiver::Message>` — the type belongs to the receiver, and one type may be two actors' message. `main/main.cpp` **spawns and wires only**: no actor class and no message type of its own.\n\
             - A **driver's argument usually does not exist until `init()`** — a bus handle comes from \
             opening the bus. So hold it where it can be built then: a member of type \
             `std::optional<sps30::Sps30>` set with `emplace(handle)`, or a pointer; **not** a plain \
             member, and never by assigning one, because a component is non-copyable and does not \
             compile. A zero-initialised `BusHandle{}` is a placeholder only: until the `TODO` that \
             produces the real handle is filled in, the driver reaches a null device.\n\
             - Spawn everything and wire it **before** `scheduler.start()` — the peers you were handed \
             by constructor injection, and the peers a constructor cannot reach by name in `init()`.\n\
             - **Do not write a bare FreeRTOS loop.** No `while (true)` task and no `xTaskCreate` for the composition: the `spire::Scheduler` owns the tasks, and a `main/` that is one polling loop is the reviewed design ignored, not implemented.\n"
            .to_string(),
        Some(ApplicationFramework::Ramen) => "FRAMEWORK: `ramen` — already chosen and reviewed. Do \
             not choose again and do not mix.\n\
             - A **stage** is what it pulls and what it pushes, wired with `>>`.\n\
             - Pushing is **synchronous and inline**: one task runs the whole chain, so the chain is a \
             DAG and never a cycle (a cycle is unbounded recursion on one stack).\n\
             - A **component** stays framework-agnostic — an application is what wraps it in ports.\n\
             - **Every stage is a component of its own**: the class is written to \
             `components/<id>/include/<id>.hpp` — `#include <detector.hpp>` for a stage named \
             `detector` — and each one already has a `CMakeLists.txt` (structural — do not edit it) and a \
             stub header with the ports the design named. A stage's in-port is `in_<value>` \
             (`ramen::Pushable<...>`) and its out-port is `out_<value>` (`ramen::Pusher<...>`), so `main/` \
             wires `a.out_x >> b.in_x`.\n\
             - The **values are shared**: every value the design names on an edge — what a stage pulls and \
             what it pushes — goes in `components/messages/include/messages.hpp`, included as \
             `#include <messages.hpp>`, because the value on an edge belongs to the *edge*: one \
             stage pushes it and another pulls it. `main/main.cpp` **wires and pumps only** — it declares \
             no stage class and no value type of its own.\n"
            .to_string(),
        None => "FRAMEWORK: this application states none, so the library's rule for choosing one (in \
                 its hints above) decides — and the choice belongs in the application's \
                 `CMakeLists.txt`, as `set(SPIRE_APPLICATION_FRAMEWORK actors)` or `ramen`.\n"
            .to_string(),
    }
}

/// The components a stated framework **is made of**, as the library's directories name them.
///
/// A framework is not a header in the air: it is these components, and an application that uses one has
/// to declare them and include them by their real paths. The first live run of the loop wrote
/// `#include <spire/actor.hpp>` — a fair guess from the `spire::` namespace, and a file that does not
/// exist: the framework's own header is `<actor.hpp>`, and nothing had told it so.
///
/// `toolkit` is named beside `actors` even though `actors` requires it: the composition stands on
/// `spire::Task` too, and a manifest that names what the composition includes is a manifest a reader
/// can hold against the sources.
fn framework_components(framework: ApplicationFramework) -> &'static [&'static str] {
    match framework {
        ApplicationFramework::Actors => &["actors", "toolkit"],
        ApplicationFramework::Ramen => &["ramen"],
    }
}

/// Every `.hpp` a component publishes, as `(the path it is included by, its text)`, in a stable order.
///
/// That path is the *contract*: `include/actor.hpp` is included as `<actor.hpp>`, and the
/// model that writes the composition is told exactly that instead of inferring it from a namespace.
fn headers_of(root: &Path, component: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    collect_headers(
        &root.join("components").join(component).join("include"),
        Path::new(""),
        &mut found,
    );
    found.sort();
    found
}

fn collect_headers(dir: &Path, prefix: &Path, found: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = prefix.join(&name);
        if path.is_dir() {
            collect_headers(&path, &relative, found);
        } else if name.ends_with(".hpp") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                found.push((relative.to_string_lossy().replace('\\', "/"), text));
            }
        }
    }
}

/// The **headers** the composition includes, as prompt text: the framework's components and the ones
/// the design named.
///
/// A live run of the loop is why this exists. It wrote `Sps30(i2c_port, address)` and `init()` against a
/// component whose header says `Sps30(BusHandle device)` and `probe()`, and `#include <spire/actor.hpp>`
/// for a header that is `<actor.hpp>`. Telling a model that a component *exists* is not telling
/// it what to call or how to include it: the header is the contract, and this is where it is written
/// down. Every section names its include path, for the same reason it spells out the class.
///
/// Bounded per header, because a prompt is not a filing cabinet: the *shape* is what the composition
/// needs, and a header long enough to overflow is a component whose interface is the problem.
pub fn component_apis(root: &Path, application: &ApplicationSpec) -> String {
    const PER_HEADER: usize = 1800;
    let mut components: Vec<String> = framework_components(application.framework)
        .iter()
        .map(|name| name.to_string())
        .collect();
    let library = component_names(root);
    for unit in &application.units {
        let ident = unit.id.replace('-', "_");
        if unit.kind == UnitKind::Component
            && library.contains(&ident)
            && !components.contains(&ident)
        {
            components.push(ident);
        }
    }
    let mut sections = String::new();
    for component in components {
        for (relative, header) in headers_of(root, &component) {
            sections.push_str(&format!(
                "\n### `{relative}` — include it as `#include <{relative}>`\n\n"
            ));
            if header.len() > PER_HEADER {
                sections.push_str(&header[..PER_HEADER]);
                sections.push_str("\n… (truncated — read the header itself for the rest)\n");
            } else {
                sections.push_str(&header);
            }
            sections.push('\n');
        }
    }
    if sections.is_empty() {
        return String::new();
    }
    format!(
        "THE HEADERS THIS COMPOSITION INCLUDES — the library's, exactly as they are: the framework's \
         own components first, then the components the design names. Do not invent an include path, a \
         constructor, a method or a namespace: write these.\n{sections}"
    )
}

/// The **decomposition** as prompt text: the composition the model must write, unit by unit.
///
/// The framework block says *which idiom*; this says *what to build with it*, and it is the other half
/// of the same failure. Without it a fill prompt hands the model a goal and a library and the model
/// invents the components, the units, their messages and the board's addresses — so what lands in
/// `main/` is a different application from the one that was reviewed, and nothing says so.
///
/// It is rendered rather than dumped as JSON on purpose: a model following a design wants the lines a
/// person would read, and the `id`/`kind`/`role` vocabulary of the file is not what it has to write.
pub fn composition_block(application: &ApplicationSpec) -> String {
    let mut text = String::from(
        "THE DESIGN — reviewed and approved. Write this composition; do not invent another, and do \
         not leave a unit out.\n",
    );

    text.push_str("\nComponents (framework-agnostic: a driver is one device on one bus, a library is pure code):\n");
    let mut any = false;
    for unit in application
        .units
        .iter()
        .filter(|unit| unit.kind == UnitKind::Component)
    {
        any = true;
        let what = unit.provides.as_deref().unwrap_or("(nothing written down)");
        let source = match unit.source {
            Some(UnitSource::Existing) => "already in the library",
            // A published component is neither written here nor API-less: it is a managed dependency
            // whose real upstream header the composition may call.
            Some(UnitSource::Published) => "published — a managed dependency from the registry",
            _ => "to be written in the library",
        };
        let where_it_is = match (unit.role, unit.bus.as_deref()) {
            (Some(ComponentRole::Driver), Some(bus)) => format!("driver on {bus}"),
            (Some(ComponentRole::Driver), None) => "driver".to_string(),
            _ => "library".to_string(),
        };
        text.push_str(&format!(
            "- `{}` — {where_it_is}: {what} ({source})\n",
            unit.id
        ));
    }
    if !any {
        text.push_str("- none: this application's components are its own.\n");
    } else {
        // The failure this prevents, seen in the first live run of the loop: the model wrote its own
        // `main/components/sps30.hpp` instead of using the library's `sps30` — the same driver twice,
        // in two places, free to disagree. A component the library has is already on this application's
        // include path, and saying *how* is what makes "use it" actionable.
        let example = application
            .units
            .iter()
            .find(|unit| {
                unit.kind == UnitKind::Component && unit.source != Some(UnitSource::Published)
            })
            .map(|unit| unit.id.replace('-', "_"))
            .unwrap_or_default();
        if !example.is_empty() {
            text.push_str(&format!(
                "\nThe library's components are **already on this application's include path** — \
                 `#include <{example}.hpp>` — and are built into it. Use them: do **not** copy a component \
                 into `main/`, because a component that exists twice can disagree with itself.\n"
            ));
        }

        // The other half of the same lesson, and it cost the loop a build: a component that is **to be
        // written** has a header that is a shape and a `TODO` — no constructor, no methods, nothing to
        // call, because the protocol is the part no scaffold can write. A run wrote
        // `RollingAverage{window}` and `.push(...)` against such a header and the application could not
        // compile until a protocol existed; a composition that does not build is not a starting point, a
        // `TODO` where the call goes is. The rule is stated once, here, because it is the one thing in
        // the composition that has to be left open.
        let stubs: Vec<String> = application
            .units
            .iter()
            .filter(|unit| {
                unit.kind == UnitKind::Component
                    && !matches!(
                        unit.source,
                        Some(UnitSource::Existing) | Some(UnitSource::Published)
                    )
            })
            .map(|unit| format!("`{}`", unit.id))
            .collect();
        if !stubs.is_empty() {
            let verb = if stubs.len() == 1 { "has" } else { "have" };
            text.push_str(&format!(
                "\n**{} {verb} no API yet.** Each header in the library is a shape and a `TODO` — no \
                 constructor, no method, nothing to call. Do **not** invent one: write the composition so \
                 that it **compiles as it stands**, with a `// TODO: <what this unit will do with it>` \
                 where the call belongs, and let everything else in that unit be real code. A `TODO` may \
                 never stand where a **value** was needed: where a constructor wants an argument nothing \
                 can produce yet, declare a zero-initialised value of the type it takes \
                 (`sps30::BusHandle sps30_handle{{}};`) and pass that, with the `TODO` on the line that \
                 will fill it in — an empty brace list where an argument belongs does not compile either. \
                 A component that is already in the library is the opposite: its header is the contract, \
                 and it is called exactly as it is.\n",
                stubs.join(", ")
            ));
        }
    }

    text.push_str(
        "\nUnits (the composition itself — each one is a component of its own; `main/` wires them):\n",
    );
    for unit in application
        .units
        .iter()
        .filter(|unit| unit.kind != UnitKind::Component)
    {
        let uses = if unit.uses.is_empty() {
            String::new()
        } else {
            format!(", uses {}", unit.uses.join(", "))
        };
        let target = if !unit.sends_to.is_empty() {
            format!(", sends to {}", unit.sends_to.join(", "))
        } else if let Some(pushes) = &unit.pushes {
            format!(", pushes {pushes}")
        } else {
            String::new()
        };
        let shape = match unit.kind {
            UnitKind::Actor => format!(
                "actor on `{}`{}, to `components/{id}/include/{id}.hpp`",
                unit.message.as_deref().unwrap_or("?"),
                unit.state
                    .as_deref()
                    .map(|state| format!(", holding {state}"))
                    .unwrap_or_default(),
                id = component_ident(&unit.id)
            ),
            _ => {
                let pulls = unit.pulls.as_deref().unwrap_or("nothing");
                format!("stage: pulls {pulls}")
            }
        };
        text.push_str(&format!("- `{}` — {shape}{uses}{target}\n", unit.id));
    }

    // Where each unit is written — the layout that this block would otherwise leave to a model that has only
    // ever seen `main/`. A live run put every actor in one `main/`; in the reviewed design each unit is a
    // component of its own, and the shared types are a component beside them. Stated once, for the whole
    // composition, because it is the same rule for every unit — and stated as the **file set**, because the
    // units are already on disk as stubs and a model that does not know that will either write a second copy
    // of one or leave it as it found it.
    if application
        .units
        .iter()
        .any(|unit| unit_component_kind(unit).is_some())
    {
        let (unit_word, wiring) = match application.framework {
            ApplicationFramework::Actors => (
                "actor",
                "spawns every actor, wires it and starts the scheduler",
            ),
            ApplicationFramework::Ramen => ("stage", "wires the chain with `>>` and pumps it"),
        };
        text.push_str(&format!(
            "\n**Write each {unit_word} as its own component.** Every one of them is already there, one \
             directory each:\n\
             - `components/<{unit_word}>/include/<{unit_word}>.hpp` — the class, left as a stub \
             with the shape the design named: **fill it in** rather than writing a class of your own somewhere \
             else;\n\
             - `components/<{unit_word}>/src/<{unit_word}>.cpp` — whatever is not a template;\n\
             - `components/<{unit_word}>/test/<{unit_word}>_test.cpp` — the cases, of which the scaffold wrote \
             one shape check;\n\
             - `components/<{unit_word}>/CMakeLists.txt` and its `test/CMakeLists.txt` are already written and \
             are **structural**: do not edit them.\n\
             The **shared types are the composition's**: every type the design names on an edge goes in \
             `components/messages/include/messages.hpp`, included as `#include <messages.hpp>` \
             — the names are declared for you there and the fields are not. `main/main.cpp` **{wiring}** only: \
             it declares no {unit_word} class and no shared type of its own.\n"
        ));
    }

    if !application.wiring.is_empty() {
        text.push_str(&format!("\nWiring: {}\n", application.wiring.join("; ")));
    }

    // A cycle is legal for actors and illegal for ramen (the design request says so, and `validate`
    // enforces it the other way round). It is also the one thing an actor cannot wire by constructor
    // injection: two actors that reach each other cannot each be constructed with the other's ref, and
    // a peer spawned later does not exist yet to be handed one. So the fill is told, where the design
    // states the cycle, that those edges resolve **by name** in `init()` through the scheduler's
    // registry.
    if application.framework == ApplicationFramework::Actors {
        if let Some(cycle) = crate::build::application_spec::find_cycle(application) {
            text.push_str(&format!(
                "\n**This wiring has a cycle ({}).** An actor cannot hold a ref to a peer that also \
                 holds one to it, and a peer spawned later cannot be handed a ref at `spawn`. Do not \
                 construct those two with each other's ref: give each a `const spire::Registry&` \
                 **constructor parameter**, and have `main` pass `*scheduler.registry()` at `spawn` (the \
                 scheduler returns a `std::shared_ptr<Registry>`, so dereference it). Resolve the peer \
                 **in `init()`**: `peer_ = \
                 registry_.get<PeerMessage>(\"peer-id\"); return peer_.valid();`. `init()` is the \
                 moment every spawn has happened and the registry is complete — and the registry is \
                 **only** ever the scheduler's: there is no `spire::Registry::instance()`, no static \
                 `spire::Registry::get`, and `spire::Registry()` does not compile.\n",
                cycle.join(" -> ")
            ));
        }
    }

    text.push_str(
        "\nBoard facts — the application's own, never a component's. Write each literal into `main/` \
         (a `board.hpp` is the usual place), and use it from there: a board fact that reaches no line \
         in `main/` is a fact the application does not have.\n",
    );
    if application.board_facts.is_empty() {
        text.push_str("- none\n");
    }
    for fact in &application.board_facts {
        let address = if fact.address.trim().is_empty() {
            String::new()
        } else {
            format!(" at {}", fact.address)
        };
        text.push_str(&format!("- `{}` on {}{address}\n", fact.device, fact.bus));
    }

    text
}

/// A unit's **source file**, which may legitimately stay nearly empty.
///
/// A unit's class is a template on the message or the values it carries, so most of it lives in the header,
/// where it compiles. What belongs here is everything that is **not** a template — a FreeRTOS call, a
/// deadline, a device handle being opened — because that is the half that keeps the header includable from
/// a host test.
///
/// The component's manifest compiles every `src/*.cpp` it finds, so a file added here needs no build
/// change; and this one is also the header's compile check, because a class that does not parse fails
/// here, by name, rather than three files away.
fn unit_source(unit: &Unit) -> String {
    let ident = component_ident(&unit.id);
    format!(
        r#"// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#include <{ident}.hpp>

namespace {ident} {{

// TODO: the definitions that are not a template. Until there are any, this file is the header's compile
// check — the same translation unit a chip build and the host test beside it both compile.

}}  // namespace {ident}
"#
    )
}

/// The `EXTRA_COMPONENT_DIRS` block, or the comment that says why there is none.
fn library_block(library: &str) -> String {
    let library = library.trim();
    if library.is_empty() {
        return "# No library named: this application's components are its own. A component library is\n\
                # usually the better start — point SPIRE_LIBRARY_DIR at one when there is one.\n"
            .to_string();
    }
    format!(
        "# The library this application is built against.\n\
         #\n\
         # A plain directory rather than a registry dependency, deliberately: the library is a project\n\
         # on this machine, and a build that has to reach the network to find a sibling is a build that\n\
         # fails on a bench with no wifi. It is named **absolute** by a caller that knows where it is,\n\
         # and resolved against this application when it is not — the two are not the same string, and\n\
         # only cmake can tell them apart.\n\
         set(SPIRE_LIBRARY_DIR \"{library}\")\n\
         if(NOT IS_ABSOLUTE \"${{SPIRE_LIBRARY_DIR}}\")\n\
         \x20   set(SPIRE_LIBRARY_DIR \"${{CMAKE_CURRENT_LIST_DIR}}/${{SPIRE_LIBRARY_DIR}}\")\n\
         endif()\n\
         list(APPEND EXTRA_COMPONENT_DIRS \"${{SPIRE_LIBRARY_DIR}}/components\")\n"
    )
}

/// The project name, or the refusal naming what is missing and why it matters.
fn require_name<'a>(project_name: &'a str, what: &str) -> Result<&'a str, String> {
    let name = project_name.trim();
    if name.is_empty() {
        return Err(format!(
            "{what} needs a project name: it is the `project()` name `idf.py` reports, and the \
             directory the project is created in"
        ));
    }
    Ok(name)
}

/// The library's README: the title is interpolated, the body is not.
fn library_readme(project_name: &str) -> String {
    format!("# {project_name}\n{LIBRARY_README}")
}

/// A library's README body. A **plain literal**, because it is full of C++ braces and CMake
/// `${…}` and `format!` would read every one of them as an argument.
const LIBRARY_README: &str = r#"
An **ESP-IDF component library**: a project whose product is `components/*`.

It starts with a **framework** — three components that are already here — and then grows. The tool
that made this project has no opinion about what belongs *beyond* that: a driver, an algorithm, a
board's support package are all decided later, and one at a time. `SPIRE.md` is where those decisions
are written down.

## Layout

```
CMakeLists.txt        the idf.py project root, and `set(SPIRE_PROJECT_STRUCTURE idf_library)`
sdkconfig.defaults    what the project itself decides — nearly empty, deliberately
main/                 the BUILD HARNESS, not an application: it exists so `idf.py build` compiles
                      every component. It starts nothing and names no device.
components/           the product: the framework, and everything added to it
  toolkit/            spire::Task — the task seam both frameworks stand on
  ramen/              the dataflow framework (upstream Zubax/ramen, MIT) and its host test
  actors/             the classical actor framework: a scheduler, typed refs, on_message, and a name
                      registry (`*scheduler.registry()`) for a peer you were not handed a ref to
                      — and a host test that runs it, on real threads, against a fake FreeRTOS
SPIRE.md              HOW THIS LIBRARY IS MEANT TO BE USED — the architecture lives here
```

## The framework is here; using it is optional

`ramen` and `actors` are two **application frameworks**, and they are alternatives rather than
layers: streaming work (a pipeline of frames, samples, readings) suits RAMEN, and a system of many
interacting agents suits mailboxes. They share one thing — `spire::Task` — and neither requires the
other. A library that adopts neither is a perfectly good library; what the framework removes is the
*gap*, so that "we need an idiom for this" is answered by what is already in the tree instead of by
an architecture invented mid-project.

## SPIRE.md is the point

An architecture — a framework, a lifecycle convention, a wiring idiom — is not something the tool
knows about. It is something a *library* provides and describes. When someone adds a component here,
or builds an application on this library, the model reads `SPIRE.md` and follows it.

So keep it true as the components arrive: what each one is, how they are composed, what belongs in a
component versus an application, and anything the build needs that a component cannot state itself
(a compiler flag, a config symbol). Nothing else in the tree carries that.

## Adding a component

A protocol component is generated as a **stub** and then filled in. The command framing, the
checksums, the byte order and the field order are the device's own; they come off its datasheet, and
none of them can be guessed.

A component is **framework-agnostic**: it holds a device or an algorithm, and it names no pins, no
board, no `Task` and no framework. The actor — the few lines that give a component ports or a mailbox
and own its loop — belongs to the application, because it names the product.

```sh
idf.py set-target esp32s3 build      # compile-checks every component
```

## What does not go here

An application. It is a separate project that depends on this one, and the first `app_main` that
lands in `main/` is the signal that a library has quietly become an application.
"#;

/// An application's README body — a plain literal, for the same reason the library's is.
const APPLICATION_README: &str = r#"
An **ESP-IDF application**: `main/` holds the wiring and the board facts, each **unit of the
composition** is a component of its own under `components/`, and the reusable parts come from a
component library rather than being written again here.

## Layout

```
CMakeLists.txt            the idf.py project root, and `set(SPIRE_PROJECT_STRUCTURE idf_application)`
                          — plus `set(SPIRE_APPLICATION_FRAMEWORK …)` when the design phase has chosen
                          one, so the shape of the composition is stated rather than inferred
sdkconfig.defaults        what this product itself decides
main/main.cpp             the entry point: spawn, wire, start — and the board facts, which belong to
                          the application and to nothing else
components/messages/      the types that cross an edge, and shared for that reason: a sender holds the
                          *receiver's* message type, and a stage pushes what the next stage pulls, so
                          the type belongs to the edge rather than to either end of it
components/<unit>/        one component per unit of the composition — an actor and a ramen stage alike:
                          its class, its source, and the test that runs it on this machine, with
                          `REQUIRES` named from the design
```

## Building

```sh
. $IDF_PATH/export.sh       # or wherever the ESP-IDF installer put its export script
idf.py set-target esp32s3   # the chip the platform names
idf.py build
idf.py -p <port> flash monitor
```

`set-target` comes first for a reason: the component manager filters by target, so a
board-support package for an `esp32s3` board does not resolve while the project is still on the
default `esp32`.

## Where the architecture comes from

This project is scaffolded **empty on purpose.** What an application looks like — whether it has
actors, tasks, a scheduler, or a super-loop; how its parts are wired and in what order — is the
component library's business, and the library says so in its own hints.

Look for `SPIRE.md` in the library named in `CMakeLists.txt`, and read it before writing the
composition. Nothing in this project will tell you what to write; the library will.

## What does not go here

A protocol, or anything else a second product could use. That belongs in the library, where it can
be depended on rather than copied.
"#;

/// The application's README: the title is interpolated, the body is not.
fn application_readme(project_name: &str, library: &str, designed: bool) -> String {
    let against = if library.trim().is_empty() {
        "no component library (its components are its own)".to_string()
    } else {
        format!("the component library at `{}`", library.trim())
    };
    // A designed application says so, and names the composition: a reader who wants to know why
    // `main/` is shaped the way it is should be one file away from the answer — and should be told
    // **which** of the two files to edit, because editing the record instead of the source is a
    // change that gets read back over without complaint.
    let design = if designed {
        format!(
            "\nDesigned: the composition in `{COMPOSITION_FILE}` — the components, the units, the \
             wiring and the board facts, reviewed before any of it was written. **That** is the file \
             to edit to change the design; `{APPLICATION_FILE}` is the same composition as JSON, and \
             is written from the composition whenever the two disagree.\n"
        )
    } else {
        String::new()
    };
    format!("# {project_name}\n\nBuilt against {against}.\n{design}{APPLICATION_README}")
}

/// A bus a protocol component can speak: the header it includes, the handle it is given, and the
/// IDF component that provides it.
///
/// The bus is a **device fact** and an input rather than a guess, for the reason the Rust
/// `add_driver` takes one: a driver written for I²C when the part is on SPI never compiles, and the
/// mistake is worth catching when it is asked for rather than at the first build.
fn bus(bus: &str) -> Option<Bus> {
    match bus.trim().to_ascii_lowercase().as_str() {
        "i2c" => Some(Bus {
            include: "driver/i2c_master.h",
            handle: "i2c_master_dev_handle_t",
            requires: "esp_driver_i2c",
            write: "    return i2c_master_transmit(device, out, out_len, -1) == ESP_OK;",
            read: "    return i2c_master_receive(device, in, in_len, -1) == ESP_OK;",
            write_read:
                "    return i2c_master_transmit_receive(device, out, out_len, in, in_len, -1) == ESP_OK;",
        }),
        "spi" => Some(Bus {
            include: "driver/spi_master.h",
            handle: "spi_device_handle_t",
            requires: "esp_driver_spi",
            write: "    spi_transaction_t t{};\n    \
                    t.length = out_len * 8;\n    \
                    t.tx_buffer = out;\n    \
                    return spi_device_polling_transmit(device, &t) == ESP_OK;",
            read: "    spi_transaction_t t{};\n    \
                   t.length = in_len * 8;\n    \
                   t.rx_buffer = in;\n    \
                   // SPI has no read without a write: the clock comes from the frame going out.\n    \
                   std::vector<uint8_t> zeros(in_len, 0);\n    \
                   t.tx_buffer = zeros.data();\n    \
                   return spi_device_polling_transmit(device, &t) == ESP_OK;",
            // A register read on SPI is **one** full-duplex frame: the command goes out on MOSI
            // while the reply comes back on MISO on the same clock. Anything else is a different
            // device conversation, so a mismatch falls back to the two-call form rather than
            // silently padding.
            write_read: "    if (out_len == in_len) {\n        \
                        spi_transaction_t t{};\n        \
                        t.length = in_len * 8;\n        \
                        t.tx_buffer = out;\n        \
                        t.rx_buffer = in;\n        \
                        return spi_device_polling_transmit(device, &t) == ESP_OK;\n    \
                        }\n    \
                        return bus_write(device, out, out_len) && bus_read(device, in, in_len);",
        }),
        "uart" => Some(Bus {
            include: "driver/uart.h",
            handle: "uart_port_t",
            requires: "esp_driver_uart",
            write: "    return uart_write_bytes(device, out, out_len) == static_cast<int>(out_len);",
            read: "    return uart_read_bytes(device, in, in_len, portMAX_DELAY) == static_cast<int>(in_len);",
            // A serial link has no notion of a transaction at all — bytes, then more bytes. The
            // combined form is the honest name for "ask, then listen".
            write_read: "    return bus_write(device, out, out_len) && bus_read(device, in, in_len);",
        }),
        _ => None,
    }
}

/// One bus, as what a component needs from it: the IDF header to include, the handle type it owns,
/// the IDF component to name in `REQUIRES` — and the three calls everything above this layer is
/// written in terms of.
///
/// **Three, not two, and the third is the one that matters.** "Read register R" is a single exchange
/// with the device, not a write followed by a read: on I²C the two are separated by a STOP, which is
/// a *different transaction* that plenty of devices do not answer; on SPI the reply is clocked out
/// while the command goes in, on one frame. A seam without [`write_read`] would push every protocol
/// author into an exchange its device may not understand — and would hide that mistake from a test
/// that only ever sees bytes.
///
/// The bodies live here rather than in the component so the seam is one file the tool writes, and
/// every component's `SPIRE.md`-following protocol sits on the same one. That is also what makes the
/// protocol host-testable: a test supplies its own `<name>_bus.hpp` with these three functions,
/// ahead of this one on the include path, recording what the protocol wrote while answering from a
/// script.
struct Bus {
    include: &'static str,
    handle: &'static str,
    requires: &'static str,
    write: &'static str,
    read: &'static str,
    write_read: &'static str,
}

/// A device's name as a C++ identifier: `PM Sensor 2` → `pm_sensor_2`.
fn ident_of(name: &str) -> String {
    let mut ident = String::with_capacity(name.len());
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            ident.push(ch.to_ascii_lowercase());
        } else if !ident.is_empty() && !ident.ends_with('_') {
            ident.push('_');
        }
    }
    ident.trim_end_matches('_').to_string()
}

/// The class name a stub declares: `pm_sensor` → `PmSensor`.
fn type_name(ident: &str) -> String {
    ident
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// True when a root `CMakeLists.txt` declares itself a **component library**.
pub fn declares_library(root_cmake: &str) -> bool {
    declares(root_cmake, LIBRARY_MARKER)
}

/// True when a root `CMakeLists.txt` declares itself an **application**.
pub fn declares_application(root_cmake: &str) -> bool {
    declares(root_cmake, APPLICATION_MARKER)
}

/// The ESP-IDF structure a `CMakeLists.txt` declares itself to be, if it declares one.
///
/// The marker is written by the scaffolds and read back here, and it is the only thing in an
/// ESP-IDF tree that tells a library from an application: the two differ in what they *contain*, not
/// in how they build, so there is nothing else to look at. `None` for any other CMake project, which
/// is what keeps this from claiming every `CMakeLists.txt` in Spire.
///
/// Read from the file's own text rather than inferred from a layout — the same rule the analyzer's
/// other structures follow.
pub fn structure_from(root_cmake: &str) -> Option<ProjectStructure> {
    if declares_library(root_cmake) {
        Some(ProjectStructure::IdfLibrary)
    } else if declares_application(root_cmake) {
        Some(ProjectStructure::IdfApplication)
    } else {
        None
    }
}

/// A marker read, forgiving whitespace: an editor may reformat a file and a reader should not care.
fn declares(root_cmake: &str, marker: &str) -> bool {
    root_cmake.lines().any(|line| {
        line.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains(marker)
    })
}

/// What a **design** needs to know about a component library: the components it has, and what its
/// author wrote down about how it is meant to be used.
///
/// Read from the tree rather than remembered, for the reason everything else here is: the library is a
/// fact about files, and a design built on a remembered list is a design built on a list that drifts.
pub fn library_facts(root: &Path) -> crate::build::application_spec::LibraryFacts {
    crate::build::application_spec::LibraryFacts {
        root: root.to_string_lossy().to_string(),
        components: component_names(root)
            .into_iter()
            .map(|name| crate::build::application_spec::LibraryComponent {
                kind: component_kind(root, &name),
                name,
            })
            .collect(),
        hints: library_hints(root),
    }
}

/// The names of the components a library has: its `components/` directories.
///
/// Sorted, because a report a person reads should not depend on a filesystem's order. A directory with
/// no `CMakeLists.txt` is not a component — IDF would not build it, so it is not one to reconcile
/// against either.
pub fn component_names(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("components")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| entry.path().join("CMakeLists.txt").is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// A component's **kind**, as a design's role says it.
///
/// The two vocabularies are the same two words on purpose, and this is where they must agree: the
/// design's role decides the skeleton `add_component` writes, and the kind is then a fact of the
/// component's own `CMakeLists.txt`. A variant added to one and not the other fails to compile here,
/// which is the point of writing the conversion rather than casting between strings.
impl From<crate::build::application_spec::ComponentRole> for ComponentKind {
    fn from(role: crate::build::application_spec::ComponentRole) -> Self {
        match role {
            crate::build::application_spec::ComponentRole::Driver => ComponentKind::Driver,
            crate::build::application_spec::ComponentRole::Library => ComponentKind::Library,
        }
    }
}

/// Apply a **design's** component plan to the library at `root`: write the stubs it asks for, and say
/// what happened to every component.
///
/// Nothing is resolved silently. A stub that could not be written is reported with the reason rather
/// than stopping the others (a library that gained three of its four components is further along than
/// one that gained none), and the plan's own problems — the design and the library disagreeing about
/// what the library has — are carried through to the caller, because which of the two is wrong is a
/// person's call.
pub fn apply_component_plan(
    root: &Path,
    plan: &crate::build::application_spec::ComponentPlan,
) -> serde_json::Value {
    let mut added: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<serde_json::Value> = Vec::new();
    for stub in &plan.add {
        let kind: ComponentKind = stub.role.into();
        match add_component(
            root,
            &stub.name,
            kind,
            stub.bus.as_deref().unwrap_or_default(),
        ) {
            Ok(report) => added.push(report),
            Err(error) => errors.push(serde_json::json!({ "name": stub.name, "error": error })),
        }
    }
    serde_json::json!({
        "added": added,
        "present": plan.present,
        "published": plan.published,
        "problems": plan.problems,
        "errors": errors,
        "next": "fill each stub (`idf_component_edit`, from its datasheet); the `published` components \
                 need nothing here — the application's `main/idf_component.yml` resolves them at build \
                 time — then build the application: the design's units, wiring and board facts go into \
                 its `main/`",
    })
}

/// What a component **is**.
///
/// Stated rather than guessed — the same rule the project follows with `SPIRE_PROJECT_STRUCTURE` — and
/// the one question that decides a component's skeleton: whether it has a seam at all.
///
/// Both kinds are **board-agnostic**. A driver owns a bus *handle* and takes no pins, ports or
/// addresses; a library names no device at all. A board support package is therefore not a kind of
/// component here: nothing in a component library carries a board fact, and where a project needs a
/// board it is an existing ESP-IDF or third-party component included at the *application* level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// A **protocol driver**: one device, one bus. It gets a seam and a fake bus, because a protocol
    /// is the one layer that talks to something that is not there during a test.
    Driver,
    /// **Pure code**: an algorithm, a filter, a codec, a framework of plain functions. No device, no
    /// bus, no board — so no seam and no fake, and its host test is an ordinary unit test, because
    /// there is nothing to stand in for.
    Library,
    /// An application **actor**: a message, the state behind it and a task of its own. It is a
    /// component because that is what an actor already is — a directory with a manifest, an
    /// `include/`, and a `.cpp` — and it names the `actors` framework rather than a device.
    ///
    /// Emitted by [`application_scaffold`], never by [`add_component`]: an actor belongs to an
    /// application's composition, not to a library's product.
    Actor,
    /// An application **ramen stage**: what it pulls, what it pushes, and the code between them.
    /// Like an actor, it is a component of the *application* and names the `ramen` framework.
    Stage,
}

impl ComponentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ComponentKind::Driver => "driver",
            ComponentKind::Library => "library",
            ComponentKind::Actor => "actor",
            ComponentKind::Stage => "stage",
        }
    }

    /// The kind from its key. Anything else is `None`: an unknown kind is not a default.
    pub fn from_str(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_lowercase().as_str() {
            "driver" => Some(ComponentKind::Driver),
            "library" => Some(ComponentKind::Library),
            "actor" => Some(ComponentKind::Actor),
            "stage" => Some(ComponentKind::Stage),
            _ => None,
        }
    }

    /// Whether this kind is one the **library** grows — a directory a user adds with
    /// [`add_component`]. An actor and a stage are the opposite: the *application's* own units,
    /// written by [`application_scaffold`] from a reviewed design, and a library that grew one would
    /// be a library no application could reuse.
    pub fn is_library_component(self) -> bool {
        matches!(self, ComponentKind::Driver | ComponentKind::Library)
    }
}

/// What a component says it is, in its own `CMakeLists.txt`.
pub const COMPONENT_KIND_KEY: &str = "SPIRE_COMPONENT_KIND";

/// The line that states a component's kind, as the scaffolds write it.
pub fn component_kind_line(kind: ComponentKind) -> String {
    format!("set({COMPONENT_KIND_KEY} {})", kind.as_str())
}

/// The kind a component's own `CMakeLists.txt` says it is.
///
/// Read rather than inferred, so a component is a fact about itself and not about whoever is looking
/// at it. `None` for a component that states no kind — which is what a component written by hand, or
/// by an older version of this tool, looks like.
pub fn component_kind(root: &Path, name: &str) -> Option<ComponentKind> {
    let ident = ident_of(name);
    let cmake =
        std::fs::read_to_string(root.join("components").join(&ident).join("CMakeLists.txt"))
            .ok()?;
    [
        ComponentKind::Driver,
        ComponentKind::Library,
        ComponentKind::Actor,
        ComponentKind::Stage,
    ]
    .into_iter()
    .find(|kind| declares(&cmake, &component_kind_line(*kind)))
}

/// The refusal an application's **unit** gets from the paths that write a *library's* components.
///
/// A unit is an actor or a ramen stage: a unit of one application's composition, written by the application's
/// fill from the design a person reviewed. That is a different job from filling a driver's protocol or a
/// library's code — what a unit needs is the design (its message or the values on its edges, its state, the
/// peers it names), and the design is in the tree — so every one of those paths refuses **by name** rather
/// than handing the model invariants for a kind this is not. The distinction is
/// [`ComponentKind::is_library_component`]'s, made once.
fn unit_component_refusal(kind: ComponentKind) -> String {
    format!(
        "an **{}** is not a component of a library: it is a unit of one application's composition, written by \
         the application's fill from a reviewed design — its message (or the values on its edges), its state \
         and its peers come from that design, and the types it shares with the rest of the composition live in \
         `components/messages/`. Add a driver or a library component here, and let the application that needs \
         an actor or a stage name the library.",
        kind.as_str()
    )
}

/// A component's files, as a **stub for the model to fill**.
///
/// Pure — nothing here touches the filesystem — so the shape can be asserted in a test and
/// [`add_component`] is nothing but "check the library, then write these".
///
/// `kind` decides the skeleton, and the two skeletons do not resemble each other: a driver gets a
/// seam and a fake bus, a library gets neither. `bus_name` is the driver's bus and is ignored by a
/// library — which has none, and for which a bus would be a contract that lies. The host test is
/// built at the **library's** standard (C++20), which is a fact of the framework the library ships
/// rather than a preference per component.
pub fn component_files(
    name: &str,
    kind: ComponentKind,
    bus_name: &str,
) -> Result<Vec<ScaffoldFile>, String> {
    let ident = ident_of(name);
    if ident.is_empty() || !ident.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err(format!(
            "'{}' does not make a usable component name: it becomes a directory, a C++ namespace \
             and a class, so it has to start with a letter",
            name.trim()
        ));
    }
    match kind {
        ComponentKind::Driver => driver_files(&ident, bus_name),
        ComponentKind::Library => Ok(library_files(&ident)),
        // An actor and a stage are the *application's* units. They are scaffolded from a reviewed
        // design — a message, a state, ports and a wiring are what one is, and none of them is known
        // here — so there is no stub a user could add by hand: an actor with no composition around it
        // is a class that names a message type nothing declares.
        ComponentKind::Actor | ComponentKind::Stage => Err(unit_component_refusal(kind)),
    }
}

/// A **driver**'s seven files: the skeleton, the **seam** it talks through, and the harness that
/// stands a fake bus in for the device that is not there during a test.
fn driver_files(ident: &str, bus_name: &str) -> Result<Vec<ScaffoldFile>, String> {
    let bus = bus(bus_name).ok_or_else(|| {
        format!(
            "'{}' is not a bus this knows: pass `i2c`, `spi` or `uart`. A device's bus is a device \
             fact, and a protocol written for the wrong one never compiles.",
            bus_name.trim()
        )
    })?;

    let type_name = type_name(ident);
    let substitutions = [
        ("__NAME__", ident.to_string()),
        ("__IDENT__", ident.to_string()),
        ("__NAMESPACE__", ident.to_string()),
        ("__TYPE__", type_name),
        ("__BUS_INCLUDE__", bus.include.to_string()),
        ("__BUS_HANDLE__", bus.handle.to_string()),
        ("__REQUIRES__", bus.requires.to_string()),
        ("__BUS_WRITE__", bus.write.to_string()),
        ("__BUS_READ__", bus.read.to_string()),
        ("__BUS_WRITE_READ__", bus.write_read.to_string()),
    ];
    let fill = |template: &str| {
        let mut text = template.to_string();
        for (token, value) in &substitutions {
            text = text.replace(token, value);
        }
        text
    };

    Ok(vec![
        ScaffoldFile {
            path: format!("components/{ident}/CMakeLists.txt"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/driver/CMakeLists.txt"
            )),
            // Structural: the tool wrote it, `REQUIRES` included, and there is nothing in it for a
            // model to decide. The two sources below are the fill.
            structural: true,
            ..Default::default()
        },
        ScaffoldFile {
            path: format!("components/{ident}/include/{ident}.hpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/driver/include/__IDENT__.hpp"
            )),
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::Shared),
        },
        ScaffoldFile {
            path: format!("components/{ident}/src/{ident}.cpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/driver/src/__IDENT__.cpp"
            )),
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::Shared),
        },
        ScaffoldFile {
            path: format!("components/{ident}/include/{ident}_bus.hpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/driver/include/__IDENT___bus.hpp"
            )),
            // Structural: this is the component's **seam**, and the tool owns it. A protocol that
            // edited its own bus layer would be a protocol no host test could stand in for — and
            // the fake below is possible only because this file is one known shape.
            structural: true,
            ..Default::default()
        },
        ScaffoldFile {
            path: format!("components/{ident}/test/CMakeLists.txt"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/driver/test/CMakeLists.txt"
            )),
            structural: true,
            ..Default::default()
        },
        ScaffoldFile {
            path: format!("components/{ident}/test/{ident}_bus.hpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/driver/test/__IDENT___bus.hpp"
            )),
            structural: true,
            ..Default::default()
        },
        ScaffoldFile {
            path: format!("components/{ident}/test/{ident}_test.cpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/driver/test/__IDENT___test.cpp"
            )),
            // Fillable: the *cases* are the model's — which frames this device really answers with,
            // and which malformed ones it must refuse. The fake bus and the build around them are
            // the tool's, so the driver harness is the same for every driver.
            structural: false,
            fill_role: Some(spire_core::build_types::SourceRole::Shared),
        },
    ])
}

/// A **library**'s five files: plain code and an ordinary unit test.
///
/// There is no seam and no fake, and no include-path order to get right: a library takes the caller's
/// inputs and returns a value, so there is nothing to stand in for. What it *is* — the component's
/// own code and the cases that pin it — is the model's; the layout around it is the tool's, so every
/// library's test is built and run the same way.
fn library_files(ident: &str) -> Vec<ScaffoldFile> {
    let substitutions = [
        ("__NAME__", ident.to_string()),
        ("__IDENT__", ident.to_string()),
        ("__NAMESPACE__", ident.to_string()),
        ("__TYPE__", type_name(ident)),
    ];
    let fill = |template: &str| {
        let mut text = template.to_string();
        for (token, value) in &substitutions {
            text = text.replace(token, value);
        }
        text
    };
    let shared = || Some(spire_core::build_types::SourceRole::Shared);

    vec![
        ScaffoldFile {
            path: format!("components/{ident}/CMakeLists.txt"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/library/CMakeLists.txt"
            )),
            // Structural: the tool wrote it, kind and source list included. There is nothing in it
            // for a model to decide — and in particular no `REQUIRES`, because a library names no
            // IDF type.
            structural: true,
            ..Default::default()
        },
        ScaffoldFile {
            path: format!("components/{ident}/include/{ident}.hpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/library/include/__IDENT__.hpp"
            )),
            structural: false,
            fill_role: shared(),
        },
        ScaffoldFile {
            path: format!("components/{ident}/src/{ident}.cpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/library/src/__IDENT__.cpp"
            )),
            structural: false,
            fill_role: shared(),
        },
        ScaffoldFile {
            path: format!("components/{ident}/test/CMakeLists.txt"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/library/test/CMakeLists.txt"
            )),
            structural: true,
            ..Default::default()
        },
        ScaffoldFile {
            path: format!("components/{ident}/test/{ident}_test.cpp"),
            content: fill(include_str!(
                "../../templates/esp-idf/component/library/test/__IDENT___test.cpp"
            )),
            // Fillable: the cases are the model's, exactly as for a driver — inputs in, expected
            // values out. The build around them is the tool's, so the harness is the same for every
            // library, and the gate means the same thing for both kinds.
            structural: false,
            fill_role: shared(),
        },
    ]
}

/// Add a **component** of the given kind to the library at `root`: the skeleton, written.
///
/// Refuses anything that is not a library, which is the check that keeps the two project types apart
/// in practice rather than only in the docs: a component dropped into an application is a component
/// the next product copies.
///
/// The kind is **stated by the caller** and is then a fact of the component: it decides the skeleton
/// here, and it is written into the component's own `CMakeLists.txt`, where [`component_kind`] reads
/// it back. Nothing infers it.
pub fn add_component(
    root: &Path,
    name: &str,
    kind: ComponentKind,
    bus_name: &str,
) -> Result<serde_json::Value, String> {
    let root_cmake = read(root, "CMakeLists.txt")?;
    if !declares_library(&root_cmake) {
        return Err(format!(
            "{} is not an ESP-IDF component library — its CMakeLists.txt does not carry \
             `{LIBRARY_MARKER}`. A component belongs to a library; an application's parts belong to \
             the application.",
            root.display()
        ));
    }

    let ident = ident_of(name);
    // The framework is already here, and it is not a stub: scaffolding it again would replace
    // upstream's code, or the task seam, with an empty component of the same name.
    if FRAMEWORK_COMPONENTS.contains(&ident.as_str()) {
        return Err(format!(
            "`{ident}` is one of this library's **framework** components — it is already here, \
             shipped with the library, and its code is not generated into the project. Adding it \
             again would replace it with an empty stub."
        ));
    }

    let files = component_files(name, kind, bus_name)?;
    let mut written: Vec<String> = Vec::with_capacity(files.len());
    for file in &files {
        write_file(root, &file.path, &file.content)?;
        written.push(file.path.clone());
    }

    let (bus, what_is_left, test, next) = match kind {
        ComponentKind::Driver => (
            serde_json::json!(bus_name.trim().to_ascii_lowercase()),
            // What is left, stated where a caller will see it: the stub is not the driver.
            "the protocol — command framing, checksums, word and field order — read off the \
             device's datasheet, then proved against a fake bus",
            format!("components/{ident}/test — builds and runs on the host against a fake bus, no board and no chip"),
            "fill the protocol, then prove it in test/ — and `idf.py build` to compile-check it",
        ),
        ComponentKind::Library => (
            serde_json::Value::Null,
            "the code — what it computes, its inputs and its outputs, and the cases where it \
             refuses — written as pure functions, then pinned by an ordinary unit test",
            format!("components/{ident}/test — a plain unit test: inputs in, expected values out, run on the host"),
            "fill the code, then pin it in test/ — and `idf.py build` to compile-check it",
        ),
        // A unit of an application is not a component this installs: `component_files` refuses it above, by the
        // same test — `is_library_component` — and this arm answers with that refusal rather than inventing a
        // summary for a kind this function cannot write.
        ComponentKind::Actor | ComponentKind::Stage => return Err(unit_component_refusal(kind)),
    };

    Ok(serde_json::json!({
        "component": ident,
        "kind": kind.as_str(),
        "bus": bus,
        "files": written,
        "fill": what_is_left,
        "test": test,
        "next": next,
    }))
}

/// The component's **fillable** files: the ones a model may rewrite.
///
/// The manifest, the seam and the fake bus are the tool's, and are deliberately not in scope — a model
/// that rewrote its own bus layer would be a model whose work nothing could check, and one that
/// edited its test harness could make its own failures disappear.
///
/// A **framework** component is narrower still: nothing. Its code is the library's, shipped in the
/// template and upgraded by replacing it, so there is no file here that an edit could improve — see
/// [`FRAMEWORK_COMPONENTS`].
pub fn component_scope(root: &Path, name: &str) -> Vec<std::path::PathBuf> {
    let ident = ident_of(name);
    if FRAMEWORK_COMPONENTS.contains(&ident.as_str()) {
        return Vec::new();
    }
    let dir = root.join("components").join(&ident);
    let test = dir.join(format!("test/{ident}_test.cpp"));
    [
        dir.join(component_header_relpath(
            component_kind_for_prompt(root, &ident),
            &ident,
        )),
        dir.join(format!("src/{ident}.cpp")),
        test,
    ]
    .into_iter()
    .filter(|p| p.is_file())
    .collect()
}

/// Where a component's **public header** is, relative to `components/<ident>/`.
///
/// **One layout, for every kind**: a component publishes a flat `include/<ident>.hpp`, included as
/// `<ident>.hpp` — the same shape the library's own components use, so a unit is reached exactly the way a
/// driver or a library is. The framework's own components are the same shape — flat headers — and are
/// simply not written through here.
fn component_header_relpath(_kind: ComponentKind, ident: &str) -> String {
    format!("include/{ident}.hpp")
}

/// The **request** the component-edit path sends the model: everything it needs to write a component,
/// and nothing it can read for itself.
///
/// It is the `request` of `modify/code`, which puts each file's current contents in front of the model
/// when that file's turn comes and takes the rewrites one at a time. So this carries what the *other*
/// files do not say:
///
///  * what the library says about how its components are meant to be used (`SPIRE.md`) — the
///    architecture, and the only place one exists;
///  * the component's **public header as it stands** — the contract its sources and its test have to
///    match while one of them is being rewritten;
///  * the invariants of the shape, which are **the kind's**: a driver owns a handle and returns false;
///    a library is pure and holds no state;
///  * what the user knows, which is the part no code can supply;
///  * **reference material** retrieved for the device (`reference`) — facts about the device that
///    came from the knowledge store rather than from a person. It stands in for the user's words
///    when there are none, and is a source of facts exactly as they are. It arrives as **labelled
///    sections** and the labels are load-bearing: one section is this part's own protocol (where the
///    bytes come from), another is somebody else's driver for a comparable device (where the shape
///    comes from, and the commands must not). The framing below says so rather than leaving the
///    distinction to headings a model would have to infer;
///  * how the answer will be checked.
///
/// What differs between the two kinds is almost all of it: a driver is told about the **seam** and the
/// register read, a library about purity and edge cases. What does not differ is the frame around
/// them and the gate at the end — which is the point of the two kinds sharing one path.
///
/// It deliberately does **not** carry the source or the test: `modify_code_prompt` supplies those
/// when their turn comes, and the same file twice in one prompt is a prompt the model has to
/// reconcile with itself. It does **name** the source, though, and says what belongs in it — the
/// header is the contract and the source is the code — because a request that showed the model a
/// header and nothing else is how the implementation ends up inlined in the header, leaving the
/// source a file with nothing to be.
pub fn component_edit_request(
    root: &Path,
    name: &str,
    instruction: &str,
    reference: &str,
) -> Result<String, String> {
    let ident = ident_of(name);
    let dir = root.join("components").join(&ident);
    if !dir.is_dir() {
        return Err(format!(
            "{} has no component '{ident}': there is no components/{ident} directory.",
            root.display()
        ));
    }
    let kind = component_kind_for_prompt(root, &ident);
    // An application's **unit** — an actor, a ramen stage — is not a component this path edits, and the
    // distinction is [`ComponentKind::is_library_component`]'s. A unit is written by the *application's* fill,
    // from the design a person reviewed; what a model would need here is that design (its message, its ports,
    // its peers) rather than a device's datasheet. Refused **by name, before a header is read**, because a
    // unit's header is not where this path looks for one and the answer should be the reason rather than a
    // path that does not resolve.
    if !kind.is_library_component() {
        return Err(unit_component_refusal(kind));
    }
    // The framework is not a component to fill in: it is what the library *is*, and a model that
    // rewrote it would change one project and no other.
    if FRAMEWORK_COMPONENTS.contains(&ident.as_str()) {
        return Err(format!(
            "components/{ident} is this library's **framework** — shipped with the library rather \
             than generated into it. There is nothing in it to fill and nothing in it to edit: \
             upgrading it means replacing it in the template, so an edit here would live in this one \
             project and nowhere else."
        ));
    }

    let what_the_library_says = match library_hints(root) {
        Some(hints) => format!(
            "The library writes down how its components are meant to be used in `SPIRE.md`. This is \
             that file, and it is the architecture — follow it:\n\n{hints}"
        ),
        None => "The library has written nothing down about itself yet (`SPIRE.md` is absent or \
                 empty), so nothing here contradicts anything you choose — but say what you chose, \
                 in the file that exists for it, if what you chose is an idiom."
            .to_string(),
    };

    let header_path = component_header_relpath(kind, &ident);
    let header = std::fs::read_to_string(dir.join(&header_path))
        .map_err(|e| format!("cannot read components/{ident}/{header_path}: {e}"))?;
    // What the model is told about the device. Three cases, and the order matters: the user's own
    // words win; then reference material — which is a *source of facts*, like the user's words, so
    // its presence is what makes the protocol writable; and only when there is neither is the model
    // told to invent nothing.
    let reference = reference.trim();
    let what_the_user_knows = if !instruction.trim().is_empty() {
        instruction.trim().to_string()
    } else if !reference.is_empty() {
        "The user typed nothing beyond the device's name and bus. Everything known about this \
         device is in the reference material above: treat it as the only source of facts, and invent \
         no command word, no register address and no checksum it does not state."
            .to_string()
    } else {
        match kind {
            ComponentKind::Driver => {
                "The user has said nothing about this device beyond its name and bus. That means the \
                 protocol cannot be written: say so by leaving the `TODO`s in place, and add no \
                 command word, no register address and no checksum you were not given."
            }
            ComponentKind::Library => {
                "The user has said nothing beyond the component's name. That means the code cannot be \
                 written: leave the `TODO`s in place, choose no algorithm, no unit and no constant you \
                 were not given — and do not invent a use case to fill the gap."
            }
            // A unit is refused above, before a header is read: what it needs is the reviewed design rather
            // than anything the user typed here.
            ComponentKind::Actor | ComponentKind::Stage => return Err(unit_component_refusal(kind)),
        }
        .to_string()
    };

    // The chunks are framed here, once, so both prompts carry the same caveat: reference material is
    // not instruction, and a driver's header is not the driver.
    //
    // The two **kinds** of section are told apart here too, because that difference is what the
    // command words depend on: one section is this device's own protocol, the other is somebody
    // else's driver for a comparable device — and a comparable part's command word is the plausible
    // way to get this wrong. Headings alone would not be enough; an unlabelled blob of retrieved code
    // invites exactly the copy that must not happen.
    let reference_block = if reference.is_empty() {
        String::new()
    } else {
        format!(
            "## Reference material retrieved for this device\n\n\
             The knowledge store already holds the following about this device, in labelled \
             sections. It is *reference*, not instruction: if it disagrees with the user, the user is \
             right.\n\n\
             The two kinds of section are not interchangeable:\n\n\
             - **This part's own protocol** is where the command words, the framing and the checksum \
             come from — use its bytes.\n\
             - **Somebody else's driver for a comparable device** gives the shape: how such a driver \
             names things, how it talks to the bus, how it reports. That is *not* the bytes — a \
             comparable part's command word looks right and is wrong, so take its idiom and not its \
             commands.\n\n\
             If no section states this part's own protocol, you have the shape and **not** the bytes: \
             write no command word, no register address and no checksum you were not given, and say \
             which fact you were missing. If a section describes a different part than the one you \
             are writing, say so rather than writing the wrong protocol.\n\n{reference}\n\n"
        )
    };

    Ok(match kind {
        ComponentKind::Driver => driver_edit_request(
            root,
            &ident,
            &header,
            &what_the_library_says,
            &what_the_user_knows,
            &reference_block,
        ),
        ComponentKind::Library => library_edit_request(
            root,
            &ident,
            &header,
            &what_the_library_says,
            &what_the_user_knows,
            &reference_block,
        ),
        // Refused at the top of this function: a unit of an application is not a component this path edits.
        // The arm is here rather than an `_`, so the two kinds it *does* edit stay the only names below.
        ComponentKind::Actor | ComponentKind::Stage => return Err(unit_component_refusal(kind)),
    })
}

/// The domain the bundled **device facts** corpus ingests into: one document per part number,
/// stating that part's own protocol. See `resources/device-facts/` and `rag_bundle`.
pub const DEVICE_FACTS_DOMAIN: &str = "device-facts";

/// The domain a driver's **shape** is looked up in: somebody else's driver for a comparable device,
/// which is what gives the API surface and the bus idiom rather than the bytes.
pub const DRIVER_PRECEDENTS_DOMAIN: &str = "esp-idf-lib";

/// What an edit looks its component's device up as: two corpora, asked two different questions.
///
/// See [`component_device_lookups`] for why there are two.
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceLookups {
    /// The query for [`DEVICE_FACTS_DOMAIN`] — the **part number alone**, because that corpus is
    /// keyed by it: one document per part, named for it.
    pub facts: String,
    /// The query for [`DRIVER_PRECEDENTS_DOMAIN`] — the device described in prose.
    pub precedents: String,
}

/// The device lookups an edit makes, or `None` when this component has no device behind it.
///
/// **Two, because neither corpus answers both questions.** `device-facts` is asked for the part
/// number — it is the only source that can state the command words, the framing and the checksum for
/// *this* device. `esp-idf-lib` is asked in prose — "has somebody written this device, and how is it
/// driven" — which is where the API shape and the bus idiom come from. Asking one corpus for both is
/// what produced the failure this pair exists to avoid: a library that carries `sht3x` and not
/// `sht20` answers a `sht20` query with a *different* device's commands, and the bytes that would
/// settle it are in the `components/*/*.c` files its manifest excludes on purpose.
///
/// A **library** gets neither — it has no device behind it, so either query would return whatever
/// else in the corpus happens to share a word, and prompt noise is worse than a missing section.
///
/// The kind is read the same way `component_edit_request` reads it — from `component_kind`, and from
/// the presence of the seam — so the two can never disagree about whether this component has a device.
pub fn component_device_lookups(root: &Path, name: &str) -> Option<DeviceLookups> {
    let ident = ident_of(name);
    match component_kind_for_prompt(root, &ident) {
        ComponentKind::Driver => Some(DeviceLookups {
            facts: ident.clone(),
            precedents: format!("{ident} device driver protocol commands register"),
        }),
        ComponentKind::Library => None,
        // A **unit** of an application has no device behind it either: an actor's facts are its message and the
        // peers it names, a stage's are the values on its edges, and both are in the composition the
        // application carries rather than in a corpus. The edit path refuses a unit outright; this is the same
        // answer one layer down.
        ComponentKind::Actor | ComponentKind::Stage => None,
    }
}

/// Retrieved chunks as prompt-ready markdown, or `""` when there is nothing to show.
///
/// Pure, and deliberately not inside the RAG actor: what the *model is shown* is then testable
/// without a knowledge store, an embedder or a retrieval that happens to return something. Each
/// chunk is labelled with its source path — the model has to tell a driver's header from a catalogue
/// entry — and empty chunks are dropped, because a heading with nothing under it reads as "the
/// corpus says nothing", which is a different claim from "retrieval found nothing".
pub fn format_retrieved_chunks(chunks: &[RagChunkResult]) -> String {
    format_chunks_at(chunks, "###")
}

/// What a retrieved section is evidence *of*, for the heading it is introduced under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReferenceRole {
    /// The device's own protocol: command words, framing, checksum. Authoritative for the bytes.
    DeviceFacts,
    /// A driver for a comparable device: the API shape and the bus idiom — and **not** its commands.
    DriverPrecedent,
}

impl ReferenceRole {
    /// The heading text, which is the whole point of the role being a type: a model handed two
    /// unlabelled blocks of code and told to treat them as its facts cannot tell which one states
    /// *this* device's bytes, and taking the wrong one's command word is the failure this prevents.
    fn as_str(self) -> &'static str {
        match self {
            ReferenceRole::DeviceFacts => "The device's own protocol, from the device-facts corpus",
            ReferenceRole::DriverPrecedent => {
                "Somebody else's driver for a comparable device — the shape, not the commands"
            }
        }
    }
}

/// The heading a retrieval is introduced under: what the material is **for**, then the corpus it came
/// from. Both halves matter — the role because the two are not interchangeable, the name because a
/// reviewer reading a transcript can check the claim against that corpus.
pub fn reference_heading(role: ReferenceRole, domain: &str) -> String {
    format!("{} (`{domain}`)", role.as_str())
}

/// Several labelled retrievals as one reference section, or `""` when none of them returned anything.
///
/// Nested one level under the caller's heading, and each section dropped when it is empty: a corpus
/// that returned nothing (not installed, not ingested, no near match) must leave **no trace** in the
/// prompt. A heading with nothing under it does not read as "unavailable", it reads as "the facts
/// are: nothing" — which would have the model write a driver from the label alone.
pub fn format_retrieved_reference(sections: &[(String, &[RagChunkResult])]) -> String {
    let mut out = String::new();
    for (title, chunks) in sections {
        let body = format_chunks_at(chunks, "####");
        if body.is_empty() {
            continue;
        }
        out.push_str(&format!("### {title}\n\n{body}"));
    }
    out
}

/// Chunks as markdown blocks under a source-path heading at `heading`, empty chunks dropped.
///
/// The one place the shape of a retrieved chunk is decided, so the single-corpus formatter and the
/// labelled reference cannot drift apart in how they present a source.
fn format_chunks_at(chunks: &[RagChunkResult], heading: &str) -> String {
    let mut out = String::new();
    for chunk in chunks.iter().filter(|c| !c.text.trim().is_empty()) {
        out.push_str(&format!(
            "{heading} {}\n\n{}\n\n",
            chunk.source_path,
            chunk.text.trim()
        ));
    }
    out
}

/// The kind of an existing component, for a prompt that has to describe it.
///
/// Stated if the component states it. Otherwise read off the component itself: a component that
/// carries `include/<ident>_bus.hpp` is a driver, because that file *is* the seam — which is a fact
/// about the files, not a guess about intent. A component written by hand, or one that predates the
/// marker, is described as what it is.
fn component_kind_for_prompt(root: &Path, ident: &str) -> ComponentKind {
    component_kind(root, ident).unwrap_or_else(|| {
        let seam = root
            .join("components")
            .join(ident)
            .join(format!("include/{ident}_bus.hpp"));
        if seam.is_file() {
            ComponentKind::Driver
        } else {
            ComponentKind::Library
        }
    })
}

/// A **driver**'s request: the seam, the register read, and the shape a protocol keeps.
fn driver_edit_request(
    root: &Path,
    ident: &str,
    header: &str,
    what_the_library_says: &str,
    what_the_user_knows: &str,
    reference: &str,
) -> String {
    format!(
        r#"Write the protocol for `{ident}`, one component of the ESP-IDF component library at {root}.

## What this component is

`{ident}` is a **protocol component**. It owns a bus handle and it speaks one device's protocol. Its
product is the protocol — not a board, not an application, not a task, not a loop.

{what_the_library_says}

## The seam: how this component reaches the device

The component's entire interface to the bus is three functions, declared in
`components/{ident}/include/{ident}_bus.hpp` and already written — **do not edit that file**:

```cpp
bool bus_write(BusHandle device, const uint8_t* out, std::size_t out_len);
bool bus_read(BusHandle device, uint8_t* in, std::size_t in_len);
bool bus_write_read(BusHandle device, const uint8_t* out, std::size_t out_len,
                    uint8_t* in, std::size_t in_len);
```

**A register read is `bus_write_read` — one exchange.** Asking in two calls puts a gap between the
halves (on I²C a STOP), which is a different conversation that plenty of devices do not answer. This
is the most common way a driver works on the one device it was written against and on no other.

Reach past these three and the component stops being checkable: a delay, a GPIO, a second device or a
direct IDF call is something the host test cannot stand in for, so it becomes code nobody can verify
without your bench.

## The component as it stands

`{ident}` is a **header/source pair**: the contract is in the header below, and its definitions belong
in `components/{ident}/src/{ident}.cpp` — a separate file you are given on its own turn.

### `components/{ident}/include/{ident}.hpp`

```cpp
{header}
```

{reference}## What the user knows about this device

{what_the_user_knows}

## The shape this component has to keep

* it owns its **bus handle** and takes no board facts — pins, ports and addresses belong to the
  application, because they are the board's, and a protocol that hardcodes them works on exactly one
  board. It takes the **handle**, not the address: the application opens the bus and adds the device
  at its address, and what comes back from that is what this constructor is given;
* it names **no actor, no task and no framework type** — a protocol is a library, and what drives it
  is the product's business;
* it reports failure by **returning false**, never by logging: only the caller knows whether a
  failure is a startup refusal or a reading worth retrying;
* `probe()` answers whether the device is really there — read its id or its status register — rather
  than trusting that a handle was passed;
* **the header is the contract, the source is the code**: `components/{ident}/include/{ident}.hpp`
  carries **declarations only** — the class and its method signatures, with their doc comments — and
  every **definition** (the body of each method) goes in `components/{ident}/src/{ident}.cpp`, which
  `#include`s the header. Do **not** inline an implementation in the header: a header that holds the
  bodies leaves the source with nothing to be, and the two files stop agreeing;
* delete a `TODO` you have answered; **leave one you have not**, and make it say what is missing.

## How this will be checked

`components/{ident}/test/{ident}_test.cpp` is built and run on this machine by CMake and ctest —
against a fake bus that records what the protocol wrote and answers from frames the test scripts. The
change is kept only if the component still compiles and that test passes; otherwise every file is
restored byte-for-byte. So the test cases are part of the work, not an extra: script this device's
real replies from what the user told you, and script the malformed ones too — a short reply, a
corrupt one, and no reply at all."#,
        root = root.display(),
    )
}

/// A **library**'s request: pure code, its edges, and the same gate at the end.
///
/// The frame is the driver's — the library's `SPIRE.md`, the header as it stands, what the user knows,
/// how it will be checked. What is gone is everything that only makes sense for a device: the seam,
/// the register read, the bus handle. Being told about a seam this component does not have is how a
/// model ends up writing a driver when it was asked for a filter.
fn library_edit_request(
    root: &Path,
    ident: &str,
    header: &str,
    what_the_library_says: &str,
    what_the_user_knows: &str,
    reference: &str,
) -> String {
    format!(
        r#"Write the code of `{ident}`, one component of the ESP-IDF component library at {root}.

## What this component is

`{ident}` is a **library component**. It is **pure code**: an algorithm, a filter, a codec, a
framework of plain functions. It has no device and no bus, it holds no bus handle, and it names no IDF
type — its `CMakeLists.txt` has no `REQUIRES` for the same reason: it is code, and code that reaches
for a driver is code that stopped being usable off the chip. Its product is the computation — not a
driver, not a board, not a task, not a loop.

{what_the_library_says}

## The component as it stands

`{ident}` is a **header/source pair**: the contract is in the header below, and its definitions belong
in `components/{ident}/src/{ident}.cpp` — a separate file you are given on its own turn.

### `components/{ident}/include/{ident}.hpp`

```cpp
{header}
```

{reference}## What the user says this should do

{what_the_user_knows}

## The shape this component has to keep

* it is **pure**: the same inputs give the same outputs. No global mutable state, no hidden
  configuration, no one-shot initialisation — two callers must not be able to interfere, and a test
  must not have to set anything up;
* it takes the **caller's** facts — a buffer to fill, a size, a sample rate, a count — and never reads
  a global, a pin or a board constant. That is what makes it the same code on the host and on the
  chip, and what makes it checkable here at all;
* it names **no actor, no task, no IDF type and no peripheral** — nothing in it needs a scheduler, a
  driver or a board. If the work needs a peripheral, the work belongs above this component;
* it reports failure by **returning false** (or an error value), never by logging or asserting: only
  the caller knows whether a failure is a refusal or a value worth retrying;
* it is **small**: one thing done well, through the functions the header promises. A component that
  grows a framework is a component nobody can check;
* **the header is the contract, the source is the code**: `components/{ident}/include/{ident}.hpp`
  carries **declarations only** — the class and its method signatures, with their doc comments — and
  every **definition** (the body of each method) goes in `components/{ident}/src/{ident}.cpp`, which
  `#include`s the header. Do **not** inline an implementation in the header: a header that holds the
  bodies leaves the source with nothing to be, and the two files stop agreeing;
* delete a `TODO` you have answered; **leave one you have not**, and make it say what is missing.

## How this will be checked

`components/{ident}/test/{ident}_test.cpp` is built and run on this machine by CMake and ctest — an
ordinary unit test, with no fake and nothing stood in for, because this component takes the caller's
inputs and returns a value. The change is kept only if the component still compiles and that test
passes; otherwise every file is restored byte-for-byte.

So the cases are part of the work, not an extra. Write them as a table of inputs and expected
outputs:

* the ordinary case, with the values a real caller would pass;
* the edges — a zero size, an empty buffer, a value at the top of its range;
* the refusals — the arguments it must reject, and the fact that it says so rather than guessing at an
  answer."#,
        root = root.display(),
    )
}

/// What a component's **host test** did when it was compiled.
pub struct HostTestBuild {
    /// Compile errors, grouped by file — the shape the modify spine compares.
    pub build_errors: crate::build::autofix::ErrorsByFile,
    /// Whether it compiled. `false` means nothing was re-linked.
    pub success: bool,
    /// What the commands printed, for the report.
    pub output: String,
}

/// Compile a component's host test: `cmake -S test -B test/build`, then `cmake --build`.
///
/// This is the first half of the **acceptance gate** for changing a component's protocol, and it needs
/// no board, no chip and no IDF — which is the whole reason a protocol can be written and checked in
/// seconds rather than minutes.
///
/// `cmake` is the one tool it needs, and a machine without it gets an `Err` naming that — not an empty
/// error list, which would read as a clean build.
pub async fn build_host_test(root: &Path, name: &str) -> Result<HostTestBuild, String> {
    let test_dir = host_test_dir(root, name)?;

    let configure = run_cmd(&test_dir, "cmake", &["-S", ".", "-B", "build"])
        .await
        .map_err(|e| {
            format!(
                "cannot run `cmake` to build the component's host test ({e}). It is the one tool \
                 this check needs, and it is not on this machine's PATH."
            )
        })?;
    let mut output = configure.output.clone();
    if !configure.success {
        // Configuration failed: nothing was compiled, and the errors are configuration ones.
        return Ok(HostTestBuild {
            build_errors: compiler_errors(&output),
            success: false,
            output,
        });
    }

    let build = run_cmd(&test_dir, "cmake", &["--build", "build"])
        .await
        .map_err(|e| e.to_string())?;
    output.push_str(&build.output);
    Ok(HostTestBuild {
        build_errors: compiler_errors(&output),
        success: build.success,
        output,
    })
}

/// Run the component's host test binary — `ctest` in `test/build`.
///
/// The second half of the gate, and deliberately separate from the compile: a test run against a
/// binary that the last build did not replace is a test of code that is no longer there.
pub async fn run_host_test_binary(root: &Path, name: &str) -> Result<bool, String> {
    let test_dir = host_test_dir(root, name)?;
    let ctest = run_cmd(&test_dir, "ctest", &["--test-dir", "build"])
        .await
        .map_err(|e| e.to_string())?;
    Ok(ctest.success)
}

/// A component's `test/` directory, or the refusal naming what is missing.
fn host_test_dir(root: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    let ident = ident_of(name);
    let test_dir = root.join("components").join(&ident).join("test");
    if !test_dir.join("CMakeLists.txt").is_file() {
        return Err(format!(
            "components/{ident}/test has no CMakeLists.txt, so this component has no host test to run"
        ));
    }
    Ok(test_dir)
}

/// The compile errors in a compiler's *own output*, grouped by file.
///
/// `path:line:col: error: message` is what clang and gcc both print, and the only question the
/// modify spine asks of it is *how many*, and whether that went up — so this stays a line scan rather
/// than becoming a parser. `warning:` and `note:` are not errors and are not counted.
fn compiler_errors(output: &str) -> crate::build::autofix::ErrorsByFile {
    let mut errors: crate::build::autofix::ErrorsByFile = std::collections::BTreeMap::new();
    for line in output.lines() {
        let Some((location, message)) = line.split_once(": error: ") else {
            continue;
        };
        // `<file>:<line>:<col>` — the file is what remains after the last two colons.
        let mut parts = location.rsplitn(3, ':');
        let _column = parts.next();
        let _line_number = parts.next();
        let Some(file) = parts.next() else { continue };
        if file.trim().is_empty() {
            continue;
        }
        errors
            .entry(file.trim().to_string())
            .or_default()
            .push(message.trim().to_string());
    }
    errors
}

/// Remove a component from the library at `root`: its directory, and nothing else.
///
/// **Directory only**, on purpose. ESP-IDF discovers a component by its directory, so removing
/// `components/<name>` removes the component — there is no registry line to unpick and no root
/// manifest to edit.
///
/// What it will not do is edit another component on the caller's behalf. If something else
/// `REQUIRES` this one, the removal **stops and names what depends on it**: a component other
/// components were built on is a deletion whose consequences are not in the file being deleted, and a
/// tool that quietly repaired the dependents would be making a design decision inside a delete
/// command.
pub fn remove_component(root: &Path, name: &str) -> Result<serde_json::Value, String> {
    let root_cmake = read(root, "CMakeLists.txt")?;
    if !declares_library(&root_cmake) {
        return Err(format!(
            "{} is not an ESP-IDF component library — its CMakeLists.txt does not carry \
             `{LIBRARY_MARKER}`. Only a library has components to remove.",
            root.display()
        ));
    }
    let ident = ident_of(name);
    let dir = root.join("components").join(&ident);
    if !dir.is_dir() {
        return Err(format!(
            "{} has no component '{ident}': there is no components/{ident} directory.",
            root.display()
        ));
    }
    // The framework is not removable, for the same reason it is not editable: it is what the library
    // *is*, shipped in every library's template, and removing it from one project would leave that
    // project's `SPIRE.md` describing a framework it no longer has.
    if is_framework_component(&ident) {
        return Err(format!(
            "components/{ident} is this library's **framework** — shipped with the library rather \
             than added to it, so it is not a component that can be removed. Nothing was removed."
        ));
    }

    // Who is built on it. A component states its dependencies in its own `idf_component_register`
    // (`REQUIRES <name>`), so the answer is read from the components that are *staying*.
    let dependents = required_by(root, &ident)?;
    if !dependents.is_empty() {
        let them: Vec<String> = dependents
            .iter()
            .map(|d| format!("components/{d}"))
            .collect();
        return Err(format!(
            "components/{ident} is required by {}, so it is not a leaf — removing it would leave \
             {} unable to build. Nothing was removed.",
            them.join(", "),
            if them.len() == 1 {
                "that component".to_string()
            } else {
                "those components".to_string()
            }
        ));
    }

    std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;

    Ok(serde_json::json!({
        "removed": format!("components/{ident}"),
        "next": "idf.py build, to confirm nothing still referenced it",
    }))
}

/// The components that name `ident` in a `REQUIRES`, by directory name.
fn required_by(root: &Path, ident: &str) -> Result<Vec<String>, String> {
    let mut dependents = Vec::new();
    let entries = match std::fs::read_dir(root.join("components")) {
        Ok(entries) => entries,
        // No `components/` at all: nothing can depend on anything.
        Err(_) => return Ok(dependents),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !entry.path().is_dir() || name == ident {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path().join("CMakeLists.txt")) else {
            continue;
        };
        if requires_names(&text, ident) {
            dependents.push(name);
        }
    }
    dependents.sort();
    Ok(dependents)
}

/// True when a component's `CMakeLists.txt` names `ident` in a `REQUIRES`.
///
/// Token-based and whitespace-forgiving, because `REQUIRES` is a CMake list: it may be spelled
/// `REQUIRES a b`, `REQUIRES "a"`, or wrapped across lines. The scan stops at the end of the
/// argument list — the closing paren — so a token that merely *looks* like the name further down
/// the file is not mistaken for a dependency.
fn requires_names(cmake: &str, ident: &str) -> bool {
    let unwrap = |token: &str| {
        token
            .trim_matches(|c: char| c == '"' || c == '\'')
            .to_string()
    };
    let tokens: Vec<&str> = cmake.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        if unwrap(tokens[i]).eq_ignore_ascii_case("REQUIRES") {
            let mut j = i + 1;
            while j < tokens.len() {
                let closed = tokens[j].contains(')');
                let token = unwrap(tokens[j].trim_end_matches(')'));
                if token == ident {
                    return true;
                }
                if closed {
                    break;
                }
                j += 1;
            }
            i = j;
        }
        i += 1;
    }
    false
}

/// Read a file inside the project, naming it in the error so a missing one is not a mystery.
fn read(root: &Path, relative: &str) -> Result<String, String> {
    let path = root.join(relative);
    std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Write a file inside the project, creating its directories.
fn write_file(root: &Path, relative: &str, content: &str) -> Result<(), String> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library's hints are read whole, and a blank one is not a hint.
    ///
    /// The distinction is the point of the function: this string goes into a prompt, and a file with
    /// nothing in it is a heading with nothing under it — an instruction to the model to guess at the
    /// architecture it was supposed to be handed.
    #[test]
    fn hints_are_read_whole_and_a_blank_one_is_not_a_hint() {
        let dir = tempfile::tempdir().unwrap();

        // Nothing there at all: an application's own root, or a library that never filled it in.
        assert_eq!(library_hints(dir.path()), None);

        // Present, and empty.
        std::fs::write(dir.path().join(HINTS_FILE), "\n\n   \n").unwrap();
        assert_eq!(
            library_hints(dir.path()),
            None,
            "a blank hint is not a hint"
        );

        // Said something.
        let written = "# sensors\n\n## How to use it\n\nCall `sensors::begin()` once.\n";
        std::fs::write(dir.path().join(HINTS_FILE), written).unwrap();
        assert_eq!(library_hints(dir.path()).as_deref(), Some(written));
    }

    /// The file the scaffold emits and the file the prompt reads are the same one.
    ///
    /// Pinned because the two spellings would drift silently: a library would be given a hints file
    /// under one name and the model would read another, and both would report success.
    #[test]
    fn the_hints_the_scaffold_writes_are_the_hints_that_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let out = library_scaffold("sensors", &[]).expect("scaffolds");
        for file in &out.files {
            let path = dir.path().join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &file.content).unwrap();
        }
        assert!(
            library_hints(dir.path()).is_some(),
            "the scaffold writes {HINTS_FILE}, and that is what `library_hints` reads"
        );
    }

    /// The marker the scaffold writes is the structure the analyzer reads back.
    ///
    /// The two halves live in different modules — one emits the file, the other reads it — and
    /// nothing else in an ESP-IDF tree tells a library from an application: the two differ in what
    /// they *contain*, not in how they build. A drift between the spelling here and the read there
    /// would show up only as a project opening as a generic CMake one, with no error anywhere.
    #[test]
    fn the_structure_comes_from_the_marker_the_scaffold_wrote() {
        let library = library_scaffold("sensors", &[]).expect("scaffolds");
        assert_eq!(
            structure_from(&library.build_content),
            Some(ProjectStructure::IdfLibrary)
        );

        let application =
            application_scaffold("pm25-meter", &[], "../sensors", None).expect("scaffolds");
        assert_eq!(
            structure_from(&application.build_content),
            Some(ProjectStructure::IdfApplication)
        );

        // The harness component carries no marker — it is a component, not the project. That is
        // what keeps this reader from claiming every `CMakeLists.txt` under a root.
        let harness = library
            .files
            .iter()
            .find(|f| f.path == "main/CMakeLists.txt")
            .expect("the harness is emitted");
        assert_eq!(structure_from(&harness.content), None);

        // And an ordinary CMake project is not claimed at all.
        assert_eq!(
            structure_from(
                "cmake_minimum_required(VERSION 3.16)\nproject(x)\nadd_executable(x main.cpp)\n"
            ),
            None
        );
    }

    /// A component arrives with a **host-test harness**: the seam it talks through, a fake with the
    /// same three calls, and a build that puts the fake first.
    ///
    /// Pinned twice over — because this is what makes a protocol checkable without hardware, and
    /// because all seven files are token substitutions. A misspelt token ships a component that
    /// still looks right in a file listing and does not compile.
    #[test]
    fn a_component_arrives_with_a_host_test_harness() {
        let files = component_files("SPS30", ComponentKind::Driver, "i2c").expect("the stub");
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        for path in [
            "components/sps30/CMakeLists.txt",
            "components/sps30/include/sps30.hpp",
            "components/sps30/src/sps30.cpp",
            "components/sps30/include/sps30_bus.hpp",
            "components/sps30/test/CMakeLists.txt",
            "components/sps30/test/sps30_bus.hpp",
            "components/sps30/test/sps30_test.cpp",
        ] {
            assert!(paths.contains(&path), "{path} missing from {paths:?}");
        }

        for file in &files {
            assert!(
                !file.content.contains("__"),
                "{} still carries a template token:\n{}",
                file.path,
                file.content
            );
        }

        // The harness is the tool's; the protocol and its cases are the model's.
        let structural = |p: &str| files.iter().find(|f| f.path == p).unwrap().structural;
        assert!(
            structural("components/sps30/include/sps30_bus.hpp"),
            "the seam is the tool's"
        );
        assert!(
            structural("components/sps30/test/sps30_bus.hpp"),
            "so is the fake"
        );
        assert!(structural("components/sps30/test/CMakeLists.txt"));
        assert!(
            !structural("components/sps30/src/sps30.cpp"),
            "the protocol is the model's"
        );
        assert!(
            !structural("components/sps30/test/sps30_test.cpp"),
            "so are its cases"
        );
    }

    /// The seam speaks each bus's own dialect — and the combined exchange is the one that differs.
    ///
    /// "Read a register" is not the same conversation on the three buses, and a wrapper that got it
    /// wrong would still compile, still pass a test that only counts bytes, and still work on the
    /// one device it was written against. That is the failure this pins.
    #[test]
    fn the_seam_speaks_each_bus_s_own_dialect() {
        let seam = |bus: &str| {
            component_files("dev", ComponentKind::Driver, bus)
                .expect("the stub")
                .iter()
                .find(|f| f.path == "components/dev/include/dev_bus.hpp")
                .expect("the seam is emitted")
                .content
                .clone()
        };

        // I²C: one transaction with a repeated start — not a transmit and a receive.
        let i2c = seam("i2c");
        assert!(i2c.contains("i2c_master_transmit_receive"), "{i2c}");
        assert!(i2c.contains("i2c_master_dev_handle_t"), "{i2c}");

        // SPI: one full-duplex frame, and a `zeros` buffer when a read stands alone — SPI has no
        // read without a write, because the clock comes from the frame going out.
        let spi = seam("spi");
        assert!(spi.contains("spi_device_polling_transmit"), "{spi}");
        assert!(spi.contains("std::vector<uint8_t> zeros"), "{spi}");

        // UART: a serial link has no transaction at all, so the combined form is ask-then-listen.
        let uart = seam("uart");
        assert!(uart.contains("uart_write_bytes"), "{uart}");
        assert!(uart.contains("uart_read_bytes"), "{uart}");
    }

    /// A component names **no IDF type outside its seam** — which is what lets one header compile
    /// both into firmware and into the host test.
    ///
    /// Pinned because the IDF type in the *public* header is precisely the mistake that breaks the
    /// host build, and it breaks it as `'driver/i2c_master.h' file not found` — a missing file,
    /// rather than a design error. The seam owns the bus; the component speaks `BusHandle`.
    #[test]
    fn the_component_names_no_idf_type_outside_its_seam() {
        let files = component_files("sps30", ComponentKind::Driver, "i2c").expect("the stub");
        let content = |p: &str| {
            files
                .iter()
                .find(|f| f.path == p)
                .unwrap_or_else(|| panic!("{p} is emitted"))
                .content
                .clone()
        };

        let header = content("components/sps30/include/sps30.hpp");
        assert!(
            !header.contains("driver/"),
            "the public header names an IDF header:\n{header}"
        );
        // Angle brackets, not quotes: a quoted seam resolves beside the including file and always
        // finds the real header — which is the failure this whole arrangement exists to avoid.
        assert!(header.contains("#include <sps30_bus.hpp>"), "{header}");
        assert!(header.contains("Sps30(BusHandle device)"), "{header}");

        let source = content("components/sps30/src/sps30.cpp");
        assert!(
            !source.contains("driver/"),
            "the source names an IDF header:\n{source}"
        );
        assert!(source.contains("#include <sps30_bus.hpp>"), "{source}");
        assert!(source.contains("Sps30(BusHandle device)"), "{source}");

        // The IDF include lives in the seam, which names the handle; the fake names a token instead.
        let seam = content("components/sps30/include/sps30_bus.hpp");
        assert!(seam.contains("driver/i2c_master.h"), "{seam}");
        assert!(
            seam.contains("using BusHandle = i2c_master_dev_handle_t;"),
            "{seam}"
        );
        assert!(content("components/sps30/test/sps30_bus.hpp").contains("using BusHandle = void*;"));

        // And the generated test constructs the component with the fake's handle.
        assert!(content("components/sps30/test/sps30_test.cpp").contains("device(nullptr)"));
    }

    /// Removing a component takes its directory — and **stops** when something is built on it.
    ///
    /// Directory-only is the point: there is no registry line to unpick, and the tool does not edit
    /// another component's `REQUIRES` to make a deletion succeed. A component others depend on is a
    /// design decision, and a delete command is not where design decisions get made.
    #[test]
    fn removing_a_component_removes_the_directory_and_respects_its_users() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("CMakeLists.txt"),
            format!("{LIBRARY_MARKER}\nproject(sensors)\n"),
        )
        .unwrap();
        let component = |name: &str, requires: &str| {
            let dir = root.join("components").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("CMakeLists.txt"),
                format!(
                    "idf_component_register(\n    SRCS \"src/{name}.cpp\"\n    INCLUDE_DIRS \
                     \"include\"\n    REQUIRES {requires}\n)\n"
                ),
            )
            .unwrap();
        };

        // A component two others are built on is not a leaf, and the refusal names them.
        component("logic", "");
        component("sps30", "esp_driver_i2c logic");
        component("bme280", "esp_driver_i2c logic");
        let shared = remove_component(root, "logic").expect_err("two components require it");
        assert!(shared.contains("components/sps30"), "{shared}");
        assert!(shared.contains("components/bme280"), "{shared}");
        assert!(
            root.join("components/logic").exists(),
            "nothing was removed"
        );

        // Take the dependents away and the same call is a removal.
        std::fs::remove_dir_all(root.join("components/sps30")).unwrap();
        std::fs::remove_dir_all(root.join("components/bme280")).unwrap();
        let report = remove_component(root, "logic").expect("a leaf is removable");
        assert_eq!(report["removed"], "components/logic");
        assert!(!root.join("components/logic").exists());

        // Removing what is not there says so rather than succeeding quietly.
        let missing = remove_component(root, "logic").expect_err("it is gone");
        assert!(missing.contains("no component 'logic'"), "{missing}");
    }

    /// The component-edit request carries all six parts of the context — the whole of what the model
    /// is given.
    ///
    /// Pinned because this prompt *is* the feature: a protocol can only be written from the library's
    /// architecture, the seam, the header it has to match, the shape's invariants, what the user
    /// knows, and how the answer will be checked. A section that quietly stopped being included would
    /// show up as a plausible driver that ignores the library, or invents an exchange its device does
    /// not answer.
    #[test]
    fn the_component_edit_request_carries_the_whole_context() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("CMakeLists.txt"),
            format!("{LIBRARY_MARKER}\nproject(sensors)\n"),
        )
        .unwrap();
        std::fs::write(
            root.join("SPIRE.md"),
            "# sensors\n\n## How to use it\n\nEvery driver begins with `probe()`.\n",
        )
        .unwrap();
        for file in component_files("sps30", ComponentKind::Driver, "i2c").unwrap() {
            let path = root.join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.content).unwrap();
        }

        let request = component_edit_request(root, "sps30", "Id register 0xD0, returns 0x03.", "")
            .expect("built");

        // 1. the library's own words, verbatim.
        assert!(
            request.contains("Every driver begins with `probe()`."),
            "{request}"
        );
        // 2. the seam, and why the combined call exists.
        assert!(
            request.contains("A register read is `bus_write_read`"),
            "{request}"
        );
        assert!(request.contains("do not edit that file"), "{request}");
        // 3. the header as it stands — the contract the sources must match.
        assert!(request.contains("class Sps30 {"), "{request}");
        // 4. the invariants of the shape.
        assert!(request.contains("takes no board facts"), "{request}");
        assert!(request.contains("returning false"), "{request}");
        // 5. what the user knows.
        assert!(
            request.contains("Id register 0xD0, returns 0x03."),
            "{request}"
        );
        // 6. how it will be checked — and that a failure is rolled back.
        assert!(request.contains("restored byte-for-byte"), "{request}");
        assert!(request.contains("CMake and ctest"), "{request}");

        // The source is deliberately *not* inlined: the per-file step supplies it on its own turn,
        // and the same file twice is a prompt the model has to reconcile with itself.
        assert!(
            !request.contains("Sps30::probe"),
            "the source belongs to its own turn"
        );
    }
    /// The request says **where the code goes**, for both kinds of component.
    ///
    /// A scaffold puts a declaration in the header and the definition in the source, and the request
    /// hands the model the header on its own. Without a word about the pair, the model "completes"
    /// the header it can see — inlining the implementation — and the source it is given next has
    /// nothing left to be but a comment. Pinned because that failure is invisible in the header and
    /// only shows up as a source file that stopped agreeing with its own declarations.
    #[test]
    fn the_request_says_where_the_code_goes() {
        for (ident, kind, bus) in [
            ("sps30", ComponentKind::Driver, "i2c"),
            ("rolling_average", ComponentKind::Library, ""),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            for file in component_files(ident, kind, bus).unwrap() {
                let path = root.join(&file.path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, file.content).unwrap();
            }

            let header = format!("components/{ident}/include/{ident}.hpp");
            let source = format!("components/{ident}/src/{ident}.cpp");
            let request = component_edit_request(root, ident, "", "").expect("built");

            // Both files are named, and the split is stated — the scaffold's own convention, which
            // the request used to leave the model to guess from a header alone.
            assert!(request.contains(&header), "{ident}: {request}");
            assert!(request.contains(&source), "{ident}: {request}");
            assert!(
                request.contains("the header is the contract, the source is the code"),
                "{ident}: {request}"
            );
            assert!(
                request.contains("carries **declarations only**"),
                "{ident}: {request}"
            );
            assert!(
                request.contains("Do **not** inline an implementation in the header"),
                "{ident}: {request}"
            );

            // Naming the source is not the same as carrying it: the per-file step still supplies it
            // on its own turn, so its body is not in the request.
            assert!(
                !request.contains("::run() {"),
                "{ident}: the source belongs to its own turn"
            );
        }
    }

    /// An edit with nothing said about the device is told to **leave the TODOs in place** rather than
    /// to invent a protocol, and a library that has said nothing about itself is not invented for it
    /// either. A register map is not something a model can look up.
    #[test]
    fn a_component_edit_with_no_description_is_told_to_invent_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        for file in component_files("gps", ComponentKind::Driver, "uart").unwrap() {
            let path = tmp.path().join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.content).unwrap();
        }

        let request = component_edit_request(tmp.path(), "gps", "   ", "").expect("built");
        assert!(
            request.contains("no command word, no register address"),
            "{request}"
        );
        assert!(
            request.contains("written nothing down about itself"),
            "{request}"
        );
    }

    /// What was retrieved for a device is **carried into the prompt** — and it counts as facts.
    ///
    /// The model that writes a protocol gets the edit request and nothing else: `run_code_modify` is
    /// one prompt with no tools, so a device the corpus knows and the user has not described is
    /// writable only if the retrieval is *in* the prompt. This pins both halves: the material is
    /// there, labelled with its source, and its presence is what stops the "cannot be written" line
    /// from firing — because at that point something *was* given.
    #[test]
    fn retrieved_reference_material_is_carried_and_counts_as_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for file in component_files("sht20", ComponentKind::Driver, "i2c").unwrap() {
            let path = root.join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.content).unwrap();
        }

        let facts = vec![RagChunkResult {
            domain: DEVICE_FACTS_DOMAIN.to_string(),
            source_path: "docs/sht20.md".to_string(),
            chunk_index: 0,
            text: "Measure temperature, no-hold: command 0xF3.".to_string(),
            score: 0.95,
        }];
        let precedents = vec![RagChunkResult {
            domain: DRIVER_PRECEDENTS_DOMAIN.to_string(),
            source_path: "components/sht3x/sht3x.h".to_string(),
            chunk_index: 0,
            text: "sht3x_measure(): the no-hold measurement command is 0x2400.".to_string(),
            score: 0.91,
        }];
        let reference = format_retrieved_reference(&[
            (
                reference_heading(ReferenceRole::DeviceFacts, DEVICE_FACTS_DOMAIN),
                facts.as_slice(),
            ),
            (
                reference_heading(ReferenceRole::DriverPrecedent, DRIVER_PRECEDENTS_DOMAIN),
                precedents.as_slice(),
            ),
        ]);

        let request = component_edit_request(root, "sht20", "", &reference).expect("built");

        // The material reached the prompt, under a heading that names it, with its source shown.
        assert!(
            request.contains("## Reference material retrieved for this device"),
            "{request}"
        );
        assert!(request.contains("docs/sht20.md"), "{request}");
        assert!(request.contains("0xF3"), "{request}");
        assert!(request.contains("components/sht3x/sht3x.h"), "{request}");
        assert!(request.contains("0x2400"), "{request}");
        // The two kinds of section are told apart *in the prompt*, not only in the headings. The
        // point of retrieving a comparable driver is to take its shape, and its command words are the
        // one thing in it that must not be copied — so the prompt has to say which is which, and what
        // to do when only the second kind came back.
        assert!(request.contains("take its idiom"), "{request}");
        assert!(request.contains("**not** the bytes"), "{request}");
        // …and it is a *source of facts*: the model is no longer told the protocol cannot be written.
        assert!(!request.contains("protocol cannot be written"), "{request}");
        assert!(
            request.contains("treat it as the only source of facts"),
            "{request}"
        );
    }

    /// The two sections are labelled by what they are evidence **of**, and a corpus that returned
    /// nothing leaves **no trace**.
    ///
    /// This is why the reference is two labelled sections rather than one blob of retrieved text: a
    /// `sht20` edit shown only `esp-idf-lib` reads `sht3x`'s `0x2400`, which is not a missing answer
    /// but a plausible wrong one. Naming one section as the device's own protocol is what lets the
    /// model take the command words from there and only the shape from the other — and an *absent*
    /// section must not appear at all, because an empty heading reads as "the device's protocol is:
    /// nothing", which is a claim, not a gap.
    #[test]
    fn reference_sections_are_labelled_by_role_and_dropped_when_empty() {
        let facts = vec![RagChunkResult {
            domain: DEVICE_FACTS_DOMAIN.to_string(),
            source_path: "docs/sht20.md".to_string(),
            chunk_index: 0,
            text: "0xF3 / 0xF5.".to_string(),
            score: 0.9,
        }];
        let rendered = format_retrieved_reference(&[
            (
                reference_heading(ReferenceRole::DeviceFacts, DEVICE_FACTS_DOMAIN),
                facts.as_slice(),
            ),
            (
                reference_heading(ReferenceRole::DriverPrecedent, DRIVER_PRECEDENTS_DOMAIN),
                &[],
            ),
        ]);

        assert!(rendered.contains(DEVICE_FACTS_DOMAIN), "{rendered}");
        assert!(rendered.contains("#### docs/sht20.md"), "{rendered}");
        // The corpus that answered nothing is *absent* — neither its heading nor its role appears.
        assert!(!rendered.contains(DRIVER_PRECEDENTS_DOMAIN), "{rendered}");
        assert!(
            !rendered.contains(ReferenceRole::DriverPrecedent.as_str()),
            "{rendered}"
        );
        assert_eq!(
            format_retrieved_reference(&[("t".to_string(), &[])]),
            "",
            "a section with no chunks leaves no trace"
        );
    }

    /// With nothing typed and nothing retrieved, an edit is still told to invent nothing — retrieval
    /// is an *extra* source, not a substitute for having one.
    #[test]
    fn an_empty_retrieval_leaves_the_invent_nothing_line_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        for file in component_files("gps", ComponentKind::Driver, "uart").unwrap() {
            let path = tmp.path().join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.content).unwrap();
        }
        let request = component_edit_request(tmp.path(), "gps", "", "").expect("built");
        assert!(request.contains("protocol cannot be written"), "{request}");
        assert!(
            !request.contains("## Reference material retrieved"),
            "{request}"
        );
    }

    /// A retrieval is asked for a **driver** — it has a device — and not for a library, which has
    /// none: a query on a library's name would pull in whatever else in the corpus shares a word.
    ///
    /// And a driver is asked **twice, in two forms**: the part number, which is the key the
    /// `device-facts` corpus is written for ("one document per part, named for it"), and the device in
    /// prose, which is what a corpus of *code* is matched by. The two forms are not interchangeable —
    /// asking one of them of both corpora is what handed a `sht20` edit `sht3x`'s command words.
    #[test]
    fn a_driver_gets_two_device_lookups_and_a_library_gets_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("CMakeLists.txt"),
            format!("{LIBRARY_MARKER}\nproject(sensors)\n"),
        )
        .unwrap();
        add_component(root, "sht20", ComponentKind::Driver, "i2c").expect("installs");
        add_component(root, "moving_average", ComponentKind::Library, "").expect("installs");

        let lookups = component_device_lookups(root, "sht20").expect("a driver is looked up");
        // The key: the part number alone — not the generated identifier, not a sentence. A corpus
        // keyed by part number is asked with one, and a document named for the part is what that
        // reaches.
        assert_eq!(lookups.facts, "sht20");
        assert!(
            lookups.precedents.contains("sht20"),
            "{}",
            lookups.precedents
        );
        assert!(
            lookups.precedents.contains("driver"),
            "{}",
            lookups.precedents
        );
        assert_eq!(component_device_lookups(root, "moving_average"), None);
    }

    /// `format_retrieved_chunks` labels each chunk with its source and drops the empty ones: a
    /// heading with nothing under it would read as "the corpus says nothing", which is a different
    /// claim from "retrieval found nothing".
    #[test]
    fn format_retrieved_chunks_labels_sources_and_drops_empty_chunks() {
        let chunks = vec![
            RagChunkResult {
                domain: "d".to_string(),
                source_path: "a.h".to_string(),
                chunk_index: 0,
                text: "  ".to_string(),
                score: 0.5,
            },
            RagChunkResult {
                domain: "d".to_string(),
                source_path: "b.h".to_string(),
                chunk_index: 1,
                text: "  bus_write_read(dev, &cmd, 1, buf, 3);  ".to_string(),
                score: 0.4,
            },
        ];
        let rendered = format_retrieved_chunks(&chunks);
        assert!(!rendered.contains("a.h"), "{rendered}");
        assert!(rendered.contains("### b.h"), "{rendered}");
        assert!(rendered.contains("bus_write_read"), "{rendered}");
        assert!(!rendered.contains("  \n"), "no trailing spaces: {rendered}");
        assert_eq!(format_retrieved_chunks(&[]), "");
    }

    /// An unknown kind is **not** a default: the two keys are the two keys, and anything else is
    /// refused.
    ///
    /// Pinned because this is what `idf_add_component` refuses on. A tool that quietly fell back to
    /// `driver` for a typo would emit the bus skeleton for what the user said was a filter — and the
    /// skeleton is the one place the mistake cannot be seen afterwards: nothing about a driver's files
    /// says they should have been a library's.
    #[test]
    fn an_unknown_kind_is_refused_rather_than_defaulted() {
        assert_eq!(
            ComponentKind::from_str("driver"),
            Some(ComponentKind::Driver)
        );
        assert_eq!(
            ComponentKind::from_str("Library"),
            Some(ComponentKind::Library)
        );
        assert_eq!(
            ComponentKind::from_str("  LIBRARY "),
            Some(ComponentKind::Library)
        );
        assert_eq!(ComponentKind::from_str("component"), None);
        assert_eq!(ComponentKind::from_str(""), None);
        // And the key written into a component is the key read back.
        assert_eq!(
            component_kind_line(ComponentKind::Driver),
            "set(SPIRE_COMPONENT_KIND driver)"
        );
        assert_eq!(
            component_kind_line(ComponentKind::Library),
            "set(SPIRE_COMPONENT_KIND library)"
        );
    }

    /// A **library** component is the other shape: five files, no seam, no fake, no `REQUIRES`.
    ///
    /// The two kinds do not resemble each other, and this is the assertion that says so. A library
    /// that arrived with a bus seam would be a filter that had been handed a device to talk to — and
    /// a `REQUIRES` in its manifest would be the same mistake one layer down, where it stops the
    /// component from compiling off the chip.
    #[test]
    fn a_library_component_has_no_seam_and_no_bus() {
        let files =
            component_files("moving_average", ComponentKind::Library, "").expect("it generates");
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "components/moving_average/CMakeLists.txt",
                "components/moving_average/include/moving_average.hpp",
                "components/moving_average/src/moving_average.cpp",
                "components/moving_average/test/CMakeLists.txt",
                "components/moving_average/test/moving_average_test.cpp",
            ],
            "{paths:?}"
        );

        let content = |p: &str| {
            files
                .iter()
                .find(|f| f.path == p)
                .unwrap_or_else(|| panic!("{p} is emitted"))
                .content
                .clone()
        };

        // No manifest token survives, and no IDF component is required: that is what makes this
        // compilable on the host and on any chip.
        for file in &files {
            assert!(
                !file.content.contains("__"),
                "{} still carries a template token:\n{}",
                file.path,
                file.content
            );
        }
        let cmake = content("components/moving_average/CMakeLists.txt");
        // The word appears in the file's own comment explaining why there is none, so the assertion
        // is on the **registration call**: everything from `idf_component_register` on.
        let registration = &cmake[cmake
            .find("idf_component_register")
            .expect("the component registers itself")..];
        assert!(!registration.contains("REQUIRES"), "{registration}");
        assert!(
            cmake.contains("set(SPIRE_COMPONENT_KIND library)"),
            "{cmake}"
        );

        // The code itself names no bus and no IDF type — not even a handle.
        for path in [
            "components/moving_average/include/moving_average.hpp",
            "components/moving_average/src/moving_average.cpp",
        ] {
            let text = content(path);
            for token in ["BusHandle", "bus_", "driver/", "esp_"] {
                assert!(!text.contains(token), "{path} names {token}:\n{text}");
            }
        }

        // The test is an ordinary unit test: the source is linked in directly and nothing is made to
        // win the include path ahead of it. A driver's test has to put a fake bus first; this one has
        // nothing to put anywhere.
        let test_cmake = content("components/moving_average/test/CMakeLists.txt");
        assert!(
            test_cmake.contains("../src/moving_average.cpp"),
            "{test_cmake}"
        );
        assert!(test_cmake.contains("add_test("), "{test_cmake}");
        assert!(!test_cmake.contains("bus"), "{test_cmake}");
    }

    /// The kind a component **states** is the kind that is read back.
    ///
    /// Pinned because the kind is what the edit path branches on: a component whose marker and whose
    /// files disagreed would be described to the model as something it is not, and the model would
    /// then write the invariants it was handed.
    #[test]
    fn the_kind_a_component_states_is_the_kind_that_is_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("CMakeLists.txt"),
            format!("{LIBRARY_MARKER}\nproject(sensors)\n"),
        )
        .unwrap();

        for (name, kind) in [
            ("sps30", ComponentKind::Driver),
            ("moving_average", ComponentKind::Library),
        ] {
            let bus = if kind == ComponentKind::Driver {
                "i2c"
            } else {
                ""
            };
            add_component(root, name, kind, bus).expect("it installs");
            assert_eq!(component_kind(root, name), Some(kind), "{name}");
        }

        // A component that states no kind is not guessed at *here*: this reader says `None`, and the
        // prompt falls back to what the component's files themselves show.
        std::fs::create_dir_all(root.join("components/handwritten")).unwrap();
        std::fs::write(
            root.join("components/handwritten/CMakeLists.txt"),
            "idf_component_register(SRCS \"src/x.cpp\" INCLUDE_DIRS \"include\")\n",
        )
        .unwrap();
        assert_eq!(component_kind(root, "handwritten"), None);
    }

    /// The generated tree is **flat**: every component an application scaffold writes publishes
    /// `include/<name>.hpp`, included as `<name>.hpp` — the same shape a driver or a library uses, so a
    /// unit is reached exactly the way a library component is. The framework's own components are the one
    /// exception (they publish a namespace of headers), and they are not application-generated.
    #[test]
    fn an_applications_components_publish_a_flat_header() {
        use crate::build::application_spec::{examples, parse_spec};

        let spec = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let out =
            application_scaffold("pm25-meter", &[], "../sensors", Some(&spec)).expect("scaffolds");

        let headers: Vec<String> = out
            .files
            .iter()
            .map(|file| file.path.clone())
            .filter(|path| path.contains("/include/"))
            .collect();

        for ident in ["sampler", "air_quality", "view", "touch", "power", "messages"] {
            let flat = format!("components/{ident}/include/{ident}.hpp");
            assert!(headers.contains(&flat), "{flat} is emitted:\n{headers:#?}");
            assert!(
                !headers.contains(&format!("components/{ident}/include/{ident}/{ident}.hpp")),
                "and {ident}'s header is not nested"
            );
        }

        // And the reader the edit path uses resolves that same flat path — so the file the model is
        // handed is the file the scaffold wrote, rather than a path that no longer exists.
        let tmp = tempfile::tempdir().unwrap();
        write_scaffold(tmp.path(), &out);
        let scope: Vec<String> = component_scope(tmp.path(), "sampler")
            .into_iter()
            .map(|path| {
                path.strip_prefix(tmp.path())
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            scope.contains(&"components/sampler/include/sampler.hpp".to_string()),
            "the edit path resolves the flat header:\n{scope:#?}"
        );
    }

    /// The framework is **shipped, not generated**: not a stub to fill, not fillable, not editable,
    /// and not scaffoldable over. That is what [`FRAMEWORK_COMPONENTS`] exists to make true, and what
    /// keeps every library's framework the same library's framework.
    #[test]
    fn the_framework_is_not_a_component_to_fill_in() {
        // The emitted set and the refused set are the same set. The names are listed by hand, so
        // nothing but this test keeps them in step with the files.
        let mut emitted: Vec<&str> = FRAMEWORK_FILES
            .iter()
            .filter_map(|(_, path)| {
                let rest = path.strip_prefix("components/")?;
                rest.split('/').next()
            })
            .collect();
        emitted.sort_unstable();
        emitted.dedup();
        let mut stated = FRAMEWORK_COMPONENTS.to_vec();
        stated.sort_unstable();
        assert_eq!(
            emitted, stated,
            "the framework list and the framework files"
        );

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_scaffold(root, &library_scaffold("sensors", &[]).expect("scaffolds"));

        for name in FRAMEWORK_COMPONENTS {
            // Nothing in it is fillable…
            assert!(
                component_scope(root, name).is_empty(),
                "{name} is the library's, so no file in it is the model's"
            );
            // …and nothing in it is editable, with a refusal that says why.
            let err = component_edit_request(root, name, "", "").expect_err("refused");
            assert!(err.contains("framework"), "{err}");
            // …and it cannot be scaffolded over, which would replace upstream's code with a stub.
            let err = add_component(root, name, ComponentKind::Library, "").expect_err("refused");
            assert!(err.contains("already here"), "{err}");
            // …and it cannot be removed, which would leave a library whose SPIRE.md describes a
            // framework it no longer has.
            let err = remove_component(root, name).expect_err("refused");
            assert!(err.contains("framework"), "{err}");
            assert!(
                root.join("components").join(name).is_dir(),
                "{name} survived the refusal"
            );
        }

        // A component beside them is unaffected: the guard is about the three, not about components.
        add_component(root, "moving_average", ComponentKind::Library, "").expect("installs");
        assert!(!component_scope(root, "moving_average").is_empty());
    }

    /// The facts a **design** is given about a library: what it has, what each component says it is, and
    /// what its author wrote down. Read from the tree, so a design is never told about a library that
    /// has since changed.
    #[test]
    fn a_librarys_facts_are_what_a_design_is_told() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_scaffold(root, &library_scaffold("sensors", &[]).expect("scaffolds"));
        add_component(root, "sps30", ComponentKind::Driver, "i2c").expect("installs");

        let facts = library_facts(root);
        assert_eq!(facts.root, root.to_string_lossy());
        let named: Vec<(&str, Option<ComponentKind>)> = facts
            .components
            .iter()
            .map(|component| (component.name.as_str(), component.kind))
            .collect();
        assert_eq!(
            named,
            vec![
                // The framework it ships: libraries, both of them.
                ("actors", Some(ComponentKind::Library)),
                ("ramen", Some(ComponentKind::Library)),
                ("sps30", Some(ComponentKind::Driver)),
                ("toolkit", Some(ComponentKind::Library)),
            ]
        );
        assert!(
            facts.hints.is_some(),
            "the hints file the scaffold emits is the author's starting point, so it is said"
        );

        // A library that has written nothing down says *nothing* — not an empty string, which a prompt
        // would render as a heading over no content.
        let bare = tempfile::tempdir().unwrap();
        write_scaffold(
            bare.path(),
            &library_scaffold("bare", &[]).expect("scaffolds"),
        );
        std::fs::write(bare.path().join(HINTS_FILE), "\n\n   \n").unwrap();
        let facts = library_facts(bare.path());
        assert_eq!(facts.hints, None);
        assert!(
            !facts.components.is_empty(),
            "but its components are still its own"
        );
    }

    /// The **interfaces** a composition has to call: the components' own headers, read from the library.
    ///
    /// A live run of the loop is why this exists — the model wrote `Sps30(i2c_port, address)` and
    /// `init()` against a component whose header says `Sps30(BusHandle device)` and `probe()`. Telling a
    /// model that a component *exists* is not telling it what to call.
    #[test]
    fn the_apis_a_composition_calls_are_the_components_own_headers() {
        use crate::build::application_spec::{examples, parse_spec};

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_scaffold(root, &library_scaffold("sensors", &[]).expect("scaffolds"));
        add_component(root, "sps30", ComponentKind::Driver, "i2c").expect("installs");
        add_component(root, "moving_average", ComponentKind::Library, "").expect("installs");

        // The design names two components the library has and one it does not: the two it has are
        // rendered, the one that is only a stub on paper is not (there is no header to read).
        let mut spec = parse_spec(examples::PM25_METER).expect("the worked example parses");
        for unit in &mut spec.units {
            if unit.id == "moving_average" {
                unit.source = Some(crate::build::application_spec::UnitSource::Stub);
            }
        }
        let apis = component_apis(root, &spec);
        assert!(
            apis.contains("THE HEADERS THIS COMPOSITION INCLUDES"),
            "{apis}"
        );
        // The framework's own components first, each header named by the path it is included by: the
        // model wrote `<spire/actor.hpp>` for this header, and a namespace is not a path.
        assert!(
            apis.contains("#include <actor.hpp>") && apis.contains("namespace spire"),
            "the framework's header, and the path it is included by:\n{apis}"
        );
        assert!(
            apis.contains("#include <task.hpp>"),
            "a composition stands on the toolkit's Task too:\n{apis}"
        );
        assert!(
            apis.contains("#include <sps30.hpp>")
                && apis.contains("explicit Sps30(BusHandle device)"),
            "the driver's own constructor is what the composition must call:\n{apis}"
        );
        assert!(
            apis.contains("#include <moving_average.hpp>")
                && apis.contains("namespace moving_average"),
            "and the library component's:\n{apis}"
        );
        assert!(
            !apis.contains("sht20"),
            "a component the library does not have has no header to carry:\n{apis}"
        );
    }

    /// A library's components are its `components/` directories **that have a manifest**: IDF would not
    /// build a directory without one, so it is not a component to reconcile a design against either.
    #[test]
    fn a_librarys_components_are_the_directories_that_have_a_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_scaffold(root, &library_scaffold("sensors", &[]).expect("scaffolds"));
        std::fs::create_dir_all(root.join("components/not_a_component")).unwrap();
        std::fs::write(root.join("components/stray_file"), "x").unwrap();

        let names = component_names(root);
        assert_eq!(names, vec!["actors", "ramen", "toolkit"], "{names:?}");
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "sorted, so a report does not depend on a filesystem's order: {names:?}"
        );

        // Adding one makes it appear — which is what makes a second apply of the same design a no-op.
        add_component(root, "sps30", ComponentKind::Driver, "i2c").expect("installs");
        assert_eq!(
            component_names(root),
            vec!["actors", "ramen", "sps30", "toolkit"]
        );
    }

    /// Asserting the *absence* is the half that matters.
    #[test]
    fn each_kind_is_told_about_its_own_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("CMakeLists.txt"),
            format!("{LIBRARY_MARKER}\nproject(sensors)\n"),
        )
        .unwrap();

        add_component(root, "sps30", ComponentKind::Driver, "i2c").expect("installs");
        add_component(root, "moving_average", ComponentKind::Library, "").expect("installs");

        let driver = component_edit_request(root, "sps30", "Id register 0xD0.", "").expect("built");
        let library = component_edit_request(root, "moving_average", "A 4-sample window.", "")
            .expect("built");

        // What both are told: the same frame, and the same gate.
        for request in [&driver, &library] {
            assert!(request.contains("CMake and ctest"), "{request}");
            assert!(request.contains("restored byte-for-byte"), "{request}");
            assert!(
                request.contains("written nothing down about itself"),
                "{request}"
            );
        }

        // The driver: its seam, and the invariants of a protocol.
        assert!(
            driver.contains("A register read is `bus_write_read`"),
            "{driver}"
        );
        assert!(driver.contains("do not edit that file"), "{driver}");
        assert!(driver.contains("it owns its **bus handle**"), "{driver}");
        assert!(driver.contains("takes no board facts"), "{driver}");
        assert!(driver.contains("against a fake bus"), "{driver}");

        // The library: none of that, and purity and edges instead.
        assert!(!library.contains("bus_write"), "{library}");
        assert!(!library.contains("_bus.hpp"), "{library}");
        assert!(!library.contains("BusHandle"), "{library}");
        assert!(!library.contains("it owns its **bus handle**"), "{library}");
        assert!(!library.contains("takes no board facts"), "{library}");
        assert!(!library.contains("against a fake bus"), "{library}");
        assert!(library.contains("it is **pure**"), "{library}");
        assert!(library.contains("returning false"), "{library}");
        assert!(library.contains("ordinary unit test"), "{library}");
        assert!(library.contains("A 4-sample window."), "{library}");
        assert!(
            library.contains("What the user says this should do"),
            "{library}"
        );
    }

    /// An edit with nothing said is told to **invent nothing** — and each kind is told what that means
    /// for it: a driver must not invent a register map, a library must not invent an algorithm.
    #[test]
    fn a_library_edit_with_no_description_is_told_to_invent_no_algorithm() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("CMakeLists.txt"),
            format!("{LIBRARY_MARKER}\nproject(sensors)\n"),
        )
        .unwrap();
        add_component(root, "moving_average", ComponentKind::Library, "").expect("installs");

        let request = component_edit_request(root, "moving_average", "  ", "").expect("built");
        assert!(request.contains("choose no algorithm"), "{request}");
        assert!(request.contains("do not invent a use case"), "{request}");
        assert!(!request.contains("no register address"), "{request}");
    }

    /// Compile errors are read out of the compiler's **own output** — clang's and gcc's shared shape
    /// — and only errors are counted.
    ///
    /// Pinned because this is the number the modify spine compares to decide whether to keep or roll
    /// back a protocol: counting a warning would roll back honest work, and missing an error would
    /// keep code that does not compile.
    #[test]
    fn compiler_output_is_read_for_errors_and_nothing_else() {
        let output = "\
[ 50%] Building CXX object CMakeFiles/sps30_test.dir/sps30_test.cpp.o
/tmp/x/components/sps30/src/sps30.cpp:41:12: error: use of undeclared identifier 'probe'
/tmp/x/components/sps30/src/sps30.cpp:44:5: error: expected ';'
/tmp/x/components/sps30/test/sps30_test.cpp:9:3: warning: unused variable 'x'
gmake[2]: *** [CMakeFiles/sps30_test.dir/all] Error 2
";
        let errors = compiler_errors(output);
        // One key per *file*: both of the first file's errors land on the same entry, which is what
        // makes the spine's comparison a per-file one.
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(
            errors
                .get("/tmp/x/components/sps30/src/sps30.cpp")
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            errors.values().map(Vec::len).sum::<usize>(),
            2,
            "a warning is not an error"
        );
    }

    /// Only a library has components; an application's directory is refused by name, by both tools.
    #[test]
    fn components_belong_to_a_library_and_both_tools_say_so() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("CMakeLists.txt"),
            format!("{APPLICATION_MARKER}\nproject(product)\n"),
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("components/thing")).unwrap();

        let err = remove_component(tmp.path(), "thing").expect_err("not a library");
        assert!(err.contains(LIBRARY_MARKER), "{err}");
        let err = add_component(tmp.path(), "thing", ComponentKind::Driver, "i2c")
            .expect_err("not a library");
        assert!(err.contains(LIBRARY_MARKER), "{err}");
    }

    /// A library starts with its **framework**, and says how it is meant to be used.
    ///
    /// Hard-coding the framework is a deliberate reversal of the earlier rule ("a library starts empty
    /// and the tool has no opinion") — see [`FRAMEWORK_FILES`] for why. What survives from that rule is
    /// the part that matters: the library *provides* the frameworks without *choosing* one for the
    /// product, so neither the shell nor the build harness adopts either.
    #[test]
    fn a_library_starts_with_its_framework_and_says_how_it_is_meant_to_be_used() {
        let out = library_scaffold("sensors", &[]).expect("scaffolds");
        assert_eq!(out.structure, ProjectStructure::IdfLibrary);
        assert!(declares_library(&out.build_content));
        assert!(out.build_content.contains("project(sensors)"));

        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        for path in [
            "CMakeLists.txt",
            "sdkconfig.defaults",
            "main/CMakeLists.txt",
            "main/build_harness.cpp",
            "SPIRE.md",
            "README.md",
            // The framework: the task seam, the two application frameworks on it, and the host tests
            // that come with them.
            "components/toolkit/CMakeLists.txt",
            "components/toolkit/include/task.hpp",
            "components/toolkit/src/task.cpp",
            "components/ramen/CMakeLists.txt",
            "components/ramen/include/ramen.hpp",
            "components/ramen/src/ramen_compile_check.cpp",
            "components/ramen/test/ramen_test.cpp",
            "components/actors/CMakeLists.txt",
            "components/actors/include/actor.hpp",
            "components/actors/include/actor_ref.hpp",
            "components/actors/include/mailbox.hpp",
            "components/actors/include/scheduler.hpp",
            "components/actors/include/registry.hpp",
            "components/actors/src/scheduler.cpp",
            "components/actors/test/CMakeLists.txt",
            "components/actors/test/actors_test.cpp",
            "components/actors/test/message_tag_probe.cpp",
            "components/actors/test/probe_message.hpp",
            "components/actors/test/freertos/FreeRTOS.h",
            "components/actors/test/freertos_fake.cpp",
        ] {
            assert!(paths.contains(&path), "{path} missing from {paths:?}");
        }
        // Still not an application: no entry point that belongs to a product.
        for file in &out.files {
            assert!(
                !file.path.ends_with("main.cpp"),
                "{} is an application's file",
                file.path
            );
        }

        // **Provided, not chosen.** The harness is the shape an application would otherwise inherit,
        // and the library's own CMakeLists is what builds it: neither reaches for a framework.
        let harness = out
            .files
            .iter()
            .find(|f| f.path == "main/build_harness.cpp")
            .expect("the harness is emitted");
        assert!(
            declares_no_framework(&harness.content),
            "the build harness must not adopt a framework:\n{}",
            harness.content
        );
        assert!(
            declares_no_framework(&out.build_content),
            "the library's project file must not adopt a framework:\n{}",
            out.build_content
        );

        // And the hints say which frameworks are here, and when to reach for which — the one place an
        // architecture is written down, and the thing a model reads before building on this library.
        let hints = out
            .files
            .iter()
            .find(|f| f.path == HINTS_FILE)
            .expect("the hints are emitted");
        assert!(hints.content.contains("`ramen`"), "the dataflow framework");
        assert!(
            hints.content.contains("`actors`"),
            "the classical framework"
        );
        assert!(
            hints.content.contains("Choosing a framework"),
            "and when to reach for which:\n{}",
            hints.content
        );

        assert!(out.files.iter().all(|f| f.structural));
        assert!(out.fill_roots.is_empty());
    }

    /// An application is a **blank** entry point and a dependency on a library — not a composition.
    /// The shape of a product belongs to the library it is built against.
    #[test]
    fn an_application_is_blank_and_names_its_library() {
        let out = application_scaffold("pm25-meter", &[], "../sensors", None).expect("scaffolds");
        assert_eq!(out.structure, ProjectStructure::IdfApplication);
        assert!(declares_application(&out.build_content));
        assert!(!declares_library(&out.build_content));

        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        for path in [
            "CMakeLists.txt",
            "sdkconfig.defaults",
            "main/CMakeLists.txt",
            "main/main.cpp",
            "README.md",
        ] {
            assert!(paths.contains(&path), "{path} missing from {paths:?}");
        }
        // With no design there are no units, so the application has no components of its own *yet* —
        // and naming one would be inventing an architecture. The roots to write in exist either way,
        // because the fill phase is where a composition (and so its components) comes from when no
        // design phase ran.
        for file in &out.files {
            assert!(
                !file.path.starts_with("components/"),
                "{} is a component of a design that has not happened",
                file.path
            );
        }
        // The **product file** is the fill phase's, and everything else is locked. An application whose
        // `main.cpp` could not be written is an application that can never become a product — the state
        // the first live run of the loop ended in, with every write refused.
        let fillable: Vec<&str> = out
            .files
            .iter()
            .filter(|file| !file.structural)
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            fillable,
            vec!["main/main.cpp"],
            "exactly one file is the model's: the composition"
        );
        assert_eq!(
            out.fill_roots,
            vec!["main", "components"],
            "and it may write in both trees the composition occupies"
        );
        // Without a design there are no requires to name, and the manifest says so rather than guessing
        // at an architecture: a component named there that `main.cpp` does not include is a dependency
        // nobody asked for.
        let own_component = out
            .files
            .iter()
            .find(|file| file.path == "main/CMakeLists.txt")
            .expect("the application's own component");
        assert!(
            own_component.content.contains("Nothing yet"),
            "{}",
            own_component.content
        );
        // **No template token may survive into a file.** A leftover `__TOKEN__` is not cosmetic: this
        // one was `__FRAMEWORK_BLOCK__` on its own line in `CMakeLists.txt`, which made cmake refuse the
        // whole project ("Parse error. Expected (") while every assertion above passed — the marker it
        // looked for was in the file either way. Only a real `idf.py build` reads the file.
        for file in &out.files {
            assert!(
                !file.content.contains("__"),
                "{} carries a template token: {}",
                file.path,
                file.content
                    .lines()
                    .find(|line| line.contains("__"))
                    .unwrap_or_default()
            );
        }
        assert!(
            carries_no_architecture(&out),
            "an application shell must not choose an architecture either"
        );

        let main_cpp = out
            .files
            .iter()
            .find(|f| f.path == "main/main.cpp")
            .expect("main.cpp");
        // There is still an entry point — ESP-IDF needs one — but it does nothing and assumes
        // nothing.
        assert!(main_cpp.content.contains("app_main"));
        assert!(
            main_cpp.content.contains("kTag = \"pm25-meter\""),
            "the name reaches the log tag"
        );
        // The wiring seam is gone: how a product is composed is the library's to describe.
        assert!(!main_cpp.content.contains("// spire:wire"));
        assert!(!main_cpp.content.contains("// spire:include"));
    }

    /// **Each actor is a component of its own**, and the message types are a component beside them.
    ///
    /// This is the layout the reviewed design implies: an actor is a message type, the state behind it,
    /// a mailbox and a task — a component's worth — and a sender holds the *receiver's* message type, so
    /// the types cannot belong to either actor's component alone. Each unit arrives as a **whole**
    /// component, the way a library's does: the manifest and the host test's build are the scaffold's
    /// (they name the framework, the library and the fakes), and the class, the source and the cases are
    /// the fill's, as stubs that compile as they stand. `main/` keeps the wiring and the board facts, and
    /// names the actors it spawns.
    #[test]
    fn a_designed_application_gives_each_actor_its_own_component() {
        use crate::build::application_spec::{examples, parse_spec};

        let spec = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let out =
            application_scaffold("pm25-meter", &[], "../sensors", Some(&spec)).expect("scaffolds");
        let manifest = |path: &str| {
            out.files
                .iter()
                .find(|file| file.path == path)
                .unwrap_or_else(|| panic!("{path} is emitted"))
        };

        // One component per actor, plus the shared messages — all **structural**, because the scaffold
        // names `REQUIRES` from the design and the model that writes the composition may not edit the
        // architecture it was handed.
        for actor in ["sampler", "air_quality", "view", "touch", "power"] {
            let path = format!("components/{actor}/CMakeLists.txt");
            assert!(manifest(&path).structural, "{path} is the scaffold's");
        }
        assert!(manifest("components/messages/CMakeLists.txt").structural);

        // The framework and the shared messages first, then the library components the actor wraps —
        // the actor's own dependencies are its component's to state, not `main/`'s.
        assert!(
            manifest("components/sampler/CMakeLists.txt")
                .content
                .contains("REQUIRES actors messages sps30 sht20)"),
            "{}",
            manifest("components/sampler/CMakeLists.txt").content
        );
        assert!(
            manifest("components/air_quality/CMakeLists.txt")
                .content
                .contains("REQUIRES actors messages moving_average)"),
            "{}",
            manifest("components/air_quality/CMakeLists.txt").content
        );
        assert!(
            manifest("components/view/CMakeLists.txt")
                .content
                .contains("REQUIRES actors messages)"),
            "an actor that wraps no library component names none:\n{}",
            manifest("components/view/CMakeLists.txt").content
        );

        // `main/` keeps the wiring and names the components it spawns — the scaffold does that, because
        // `main/CMakeLists.txt` is structural.
        let own = &manifest("main/CMakeLists.txt").content;
        for name in [
            "messages",
            "sampler",
            "air_quality",
            "view",
            "touch",
            "power",
        ] {
            assert!(own.contains(name), "main/ must name `{name}`:\n{own}");
        }

        // Both trees the composition occupies are the model's to fill, and nothing else changed about
        // the guard.
        assert_eq!(out.fill_roots, vec!["main", "components"]);

        // A unit arrives as a **whole component**, the way a library's does. The two build files are the
        // scaffold's — they name the framework, the library and the fakes, which is the architecture, and a
        // model that invented one would be writing the architecture it was handed — and the class, the
        // source and the cases are the fill's.
        for path in [
            "components/messages/CMakeLists.txt",
            "components/messages/src/messages_compile_check.cpp",
            "components/messages/test/CMakeLists.txt",
            "components/sampler/CMakeLists.txt",
            "components/sampler/test/CMakeLists.txt",
        ] {
            assert!(manifest(path).structural, "{path} is the scaffold's");
        }
        for path in [
            "components/messages/include/messages.hpp",
            "components/messages/test/messages_test.cpp",
            "components/sampler/include/sampler.hpp",
            "components/sampler/src/sampler.cpp",
            "components/sampler/test/sampler_test.cpp",
        ] {
            assert!(!manifest(path).structural, "{path} is the fill's");
        }

        // Each component states its **kind**, the way every component in the tree does — `add_component`
        // writes the same line for a driver and a library. That is what makes a unit's component a fact about
        // itself, and what the edit path reads before it refuses one.
        assert!(manifest("components/sampler/CMakeLists.txt")
            .content
            .contains("set(SPIRE_COMPONENT_KIND actor)"));
        assert!(manifest("components/messages/CMakeLists.txt")
            .content
            .contains("set(SPIRE_COMPONENT_KIND library)"));

        // The stubs **name what the design named**, which is what makes the application build before it
        // computes anything: the actor is an actor on its message type, and the shared header declares it.
        // `Tick` is two actors' message, and the header says so — the type belongs to neither of them.
        let header = &manifest("components/sampler/include/sampler.hpp").content;
        assert!(
            header.contains("class Sampler final : public spire::Actor<messages::Tick>"),
            "{header}"
        );
        assert!(
            header.contains("#include <messages.hpp>"),
            "{header}"
        );
        let messages = &manifest("components/messages/include/messages.hpp").content;
        for message in [
            "struct Tick {}",
            "struct Reading {}",
            "struct Report {}",
            "struct Gesture {}",
        ] {
            assert!(
                messages.contains(message),
                "{message} is missing:\n{messages}"
            );
        }
        assert!(
            messages.contains("/// Named by sampler, power."),
            "{messages}"
        );

        // The unit's host test build **reaches the library**: the framework's sources and the fake FreeRTOS
        // under them, the library resolved the way the application resolves it, and every component the unit
        // wraps linked with its fake bus ahead on the include path. An actor is a mailbox and a task, so a
        // test that only constructed one would prove nothing.
        let harness = &manifest("components/sampler/test/CMakeLists.txt").content;
        assert!(
            harness.contains("set(SPIRE_LIBRARY_DIR \"../sensors\")"),
            "{harness}"
        );
        assert!(
            harness.contains("actors/test/freertos_fake.cpp"),
            "{harness}"
        );
        assert!(harness.contains("Threads::Threads"), "{harness}");
        assert!(
            harness.contains("sps30/src/*.cpp"),
            "a component the actor wraps is linked: {harness}"
        );
        assert!(
            harness.contains("sps30/test"),
            "and its fake bus wins the include path: {harness}"
        );

        // **The README a person reads**, and the only place the layout is stated in prose. It describes
        // *one* layout, because there is one: `main/` is the wiring and the board facts, and every unit is
        // a component of its own, an actor and a ramen stage alike.
        let readme = &manifest("README.md").content;
        assert!(
            readme.contains("components/<unit>/") && readme.contains("components/messages/"),
            "the README says where a unit is written:\n{readme}"
        );
        assert!(
            !readme.contains("(actors)") && !readme.contains("stages are inline"),
            "the README describes one layout rather than two:\n{readme}"
        );

        // No template token survives into anything emitted.
        for file in &out.files {
            assert!(
                !file.content.contains("__"),
                "{} carries a token",
                file.path
            );
        }
    }

    /// A **ramen** application lays its stages out the same way: one component per stage, and the values that
    /// cross the edges in the shared component beside them.
    ///
    /// The rule is an actor's, for the same reasons. A stage is what it pulls, what it pushes and the code
    /// between — a component's worth — and the value on an edge belongs to the *edge*: one stage pushes it and
    /// another pulls it, so it can live in neither of their components alone. The chain is wired in `main/`,
    /// and nothing else about it is.
    #[test]
    fn a_ramen_application_gives_each_stage_its_own_component() {
        use crate::build::application_spec::{examples, parse_spec};

        let spec = parse_spec(examples::INSECT_TRAP).expect("the ramen example parses");
        let out =
            application_scaffold("insect-trap", &[], "../sensors", Some(&spec)).expect("scaffolds");
        let file = |path: &str| {
            out.files
                .iter()
                .find(|file| file.path == path)
                .unwrap_or_else(|| panic!("{path} is emitted"))
        };

        // One component per stage, each stating its kind and depending on the framework and the shared values.
        for stage in [
            "capture",
            "preprocess",
            "detector",
            "classifier",
            "save_image",
        ] {
            let manifest = format!("components/{stage}/CMakeLists.txt");
            assert!(file(&manifest).structural, "{manifest} is the scaffold's");
            assert!(
                file(&manifest)
                    .content
                    .contains("set(SPIRE_COMPONENT_KIND stage)"),
                "{}",
                file(&manifest).content
            );
        }
        // `ramen` and the shared values, then the one component this stage wraps — which is `published`, so it
        // is named nowhere: the component manager injects it, under a build name this file cannot guess.
        let detector = &file("components/detector/CMakeLists.txt").content;
        assert!(detector.contains("REQUIRES ramen messages)"), "{detector}");

        // The edges are the shared types — one per value the design named, and deduplicated: `frames` is
        // `capture`'s push *and* `preprocess`'s pull, and that is **one** type, which is what an edge is.
        let messages = &file("components/messages/include/messages.hpp").content;
        for value in ["Frames", "Tensors", "Detections", "Verdicts"] {
            assert!(
                messages.contains(&format!("struct {value} {{}}")),
                "{value} is missing:\n{messages}"
            );
        }
        assert!(
            messages.contains("/// Named by capture, preprocess."),
            "{messages}"
        );

        // A stage's ports are its edges, named for the value on them: `preprocess` pulls `frames` and pushes
        // `tensors`, and `capture` — a source, because a frame pump reads a device rather than being handed a
        // value — only pushes.
        let preprocess = &file("components/preprocess/include/preprocess.hpp").content;
        assert!(
            preprocess.contains("ramen::Pushable<messages::Frames> in_frames"),
            "{preprocess}"
        );
        assert!(
            preprocess.contains("ramen::Pusher<messages::Tensors> out_tensors"),
            "{preprocess}"
        );
        let capture = &file("components/capture/include/capture.hpp").content;
        assert!(
            capture.contains("ramen::Pusher<messages::Frames> out_frames"),
            "{capture}"
        );
        assert!(
            !capture.contains("in_frames"),
            "a source has no in-port: {capture}"
        );

        // A stage's host test needs no framework source — ramen is a header, and a chain is a chain of calls —
        // but it does need the framework's headers and the application's own shared types.
        let harness = &file("components/preprocess/test/CMakeLists.txt").content;
        assert!(harness.contains("ramen/include"), "{harness}");
        assert!(
            !harness.contains("freertos_fake") && !harness.contains("Threads"),
            "a stage links no fake and no thread: {harness}"
        );
        assert!(harness.contains("../../messages/include"), "{harness}");

        // And `main/` names the chain it wires.
        let own = &file("main/CMakeLists.txt").content;
        for name in [
            "messages",
            "capture",
            "preprocess",
            "detector",
            "classifier",
            "save_image",
        ] {
            assert!(own.contains(name), "main/ must name `{name}`:\n{own}");
        }

        // The README says the same thing here as it does for an actors design — the layout is one rule —
        // and this is the case it used to get wrong, when a stage was inline in `main/`.
        let readme = &file("README.md").content;
        assert!(
            !readme.contains("stages are inline") && !readme.contains("(actors)"),
            "a ramen application's README states the same layout:\n{readme}"
        );
    }

    /// The **composition block** renders the reviewed design as the lines a model follows: components
    /// with their bus and where they come from, the units with their messages and what they use, the
    /// wiring, and — the application's own — the board facts.
    ///
    /// The two branches no worked example reaches are here: an application with no components of its
    /// own, and a ramen **source** stage, which has one port rather than two.
    #[test]
    fn the_composition_block_renders_the_design_a_model_follows() {
        use crate::build::application_spec::{examples, parse_spec, UnitKind};

        // The worked example, rendered whole.
        let app_one = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let text = composition_block(&app_one);
        assert!(
            text.contains("THE DESIGN — reviewed and approved"),
            "{text}"
        );
        assert!(
            text.contains(
                "- `sps30` — driver on i2c: PM1/2.5/4/10 readings (to be written in the library)"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "- `moving_average` — library: a rolling window (already in the library)"
            ),
            "{text}"
        );
        assert!(
            text.contains("- `air_quality` — actor on `Reading`, holding the rolling average, to `components/air_quality/include/air_quality.hpp`, uses moving_average, sends to view"),
            "{text}"
        );
        assert!(text.contains("Wiring: sampler -> air_quality"), "{text}");
        assert!(text.contains("- `sht20` on i2c at 0x40"), "{text}");
        // **Where each unit is written**: each one is a component of its own, and the shared types are a
        // component beside them. The units above name their headers; this says it once, and as the whole file
        // set, because the layout `main/` alone does not show it and the units are already there as stubs.
        assert!(
            text.contains("Write each actor as its own component")
                && text.contains("`components/<actor>/include/<actor>.hpp`")
                && text.contains("components/messages/include/messages.hpp")
                && text.contains("**spawns every actor, wires it and starts the scheduler** only"),
            "the composition states where its units live:\n{text}"
        );
        // How to *reach* them, which is what the first live run got wrong: the model wrote its own
        // `main/components/sps30.hpp` rather than using the library's.
        assert!(
            text.contains("already on this application's include path")
                && text.contains("`#include <sps30.hpp>`")
                && text.contains("do **not** copy a component"),
            "{text}"
        );
        // And **which of them have no API yet** — the boundary the first *build* found. A stub's header
        // is a shape and a `TODO`, so a composition that calls it cannot compile; the rule is stated once
        // rather than left for the model to discover at a build it never sees.
        assert!(
            text.contains("**`sps30`, `sht20` have no API yet.**")
                && text.contains("Do **not** invent one")
                && text.contains("compiles as it stands"),
            "{text}"
        );
        // The shape that actually compiles, learned from the run that followed the rule too literally:
        // `Sps30{/* TODO: bus handle */}` is an empty brace list where an argument belongs, and it does
        // not build either.
        assert!(
            text.contains("never stand where a **value** was needed")
                && text.contains("sps30::BusHandle sps30_handle{};"),
            "{text}"
        );
        assert!(
            !text.contains("`moving_average` has no API yet")
                && !text.contains("moving_average`,"),
            "a component the library already has is not a stub, and its header is callable:\n{text}"
        );

        // An application whose components are its own: the block says so rather than rendering nothing.
        let mut bare = app_one.clone();
        bare.units.retain(|unit| unit.kind != UnitKind::Component);
        for unit in &mut bare.units {
            unit.uses.clear();
        }
        bare.board_facts.clear();
        let text = composition_block(&bare);
        assert!(
            text.contains("- none: this application's components are its own."),
            "{text}"
        );
        assert!(text.contains("- none\n"), "its board facts too: {text}");
        assert!(!text.contains("driver on i2c"), "{text}");
        assert!(
            !text.contains("include path"),
            "with no components of its own there is nothing to reach:\n{text}"
        );

        // A ramen source has only a push, and the last stage only a pull, and both read as stages.
        let trap = parse_spec(examples::INSECT_TRAP).expect("the insect trap parses");
        let text = composition_block(&trap);
        assert!(
            text.contains("- `capture` — stage: pulls nothing, pushes frames"),
            "the board's camera is not a unit, so the source names nothing:\n{text}"
        );
        assert!(
            text.contains("- `save_image` — stage: pulls verdicts"),
            "the stage that stores the detection is a sink:\n{text}"
        );
        assert!(
            text.contains("Write each stage as its own component")
                && text.contains("`components/<stage>/include/<stage>.hpp`")
                && text.contains("**wires the chain with `>>` and pumps it** only"),
            "a ramen composition states the same layout for its stages:\n{text}"
        );
    }

    /// **A cycle says *how* to wire it, not only *that* there is one.** The DAG case is the whole rest of
    /// this module; this is the one branch only a cyclic `actors` design reaches, and it is the one thing
    /// an actor cannot wire by construction — so the block has to name the cycle and hand the model the
    /// registry that resolves it in `init()`.
    #[test]
    fn the_composition_block_names_an_actor_cycle_as_the_registry_case() {
        use crate::build::application_spec::{examples, parse_spec};

        // The worked example is a DAG: no cycle, so the block stays about the design and mentions no
        // registry.
        let dag = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let text = composition_block(&dag);
        assert!(
            !text.contains("This wiring has a cycle"),
            "a DAG design gets no cycle note:\n{text}"
        );

        // The same design with a mutual reference: legal for actors, and the one edge a constructor
        // cannot wire — so the block names the cycle where it states the wiring, and points at the
        // registry and the moment (`init()`) it is complete.
        let cyclic = parse_spec(&examples::PM25_METER.replace(
            r#""touch -> sampler""#,
            r#""touch -> sampler", "sampler -> touch""#,
        ))
        .expect("the cyclic design parses");
        let text = composition_block(&cyclic);
        assert!(
            text.contains("This wiring has a cycle (sampler -> touch -> sampler)"),
            "the block names the cycle:\n{text}"
        );
        assert!(
            text.contains("registry_.get<PeerMessage>") && text.contains("*scheduler.registry()"),
            "and hands over the registry that resolves it:\n{text}"
        );
        assert!(
            text.contains("constructor parameter")
                && text.contains("spire::Registry::instance()")
                && text.contains("spire::Registry::get"),
            "and forbids inventing a registry of one's own:\n{text}"
        );
    }

    /// True when a scaffold carries no **architecture** — no framework, no actor idiom, no
    /// lifecycle, no component of any kind.
    ///
    /// This is the assertion the whole generic-tool idea rests on. A shell that named a framework, or
    /// a `Container`, or an actor's `init()`, would be choosing an architecture on behalf of every
    /// project made from it — which is precisely what the *library* is for, and what its hints are
    /// for.
    ///
    /// A framework is checked by **declaration** rather than by the word: a comment that says which
    /// two values a reader may write into `SPIRE_APPLICATION_FRAMEWORK` is documentation, while
    /// `set(SPIRE_APPLICATION_FRAMEWORK …)` is a choice. A *designed* application states one and is
    /// checked by the scaffold's own test; this guard is about the blank shell.
    fn carries_no_architecture(out: &ScaffoldOutput) -> bool {
        const ARCHITECTURE: [&str; 7] = [
            "spire::",
            "namespace spire",
            "Container",
            "class Task",
            "Pushable",
            "Pusher",
            "shutdown()",
        ];
        out.files.iter().all(|file| {
            !ARCHITECTURE
                .iter()
                .any(|token| file.content.contains(token))
                && crate::build::application_spec::declared_framework(&file.content) == Ok(None)
        })
    }

    /// Whether a file *adopts* an application framework, as opposed to merely living beside one.
    ///
    /// The distinction is the whole of "provided, not chosen": `components/ramen/include/ramen.hpp` says
    /// `namespace ramen` and is not adopting anything, whereas a `main/` that says `ramen::Pusher`
    /// has picked a framework for the product.
    fn declares_no_framework(content: &str) -> bool {
        const ADOPTED: [&str; 6] = [
            "ramen::",
            "spire::",
            "namespace spire",
            "Pushable",
            "Pusher",
            "on_message",
        ];
        !ADOPTED.iter().any(|token| content.contains(token))
    }

    /// A component stub is typed for the bus it is on: the header it includes, the handle it is
    /// given, and the IDF component that provides them.
    #[test]
    fn a_component_stub_is_typed_for_its_bus() {
        for (bus_name, include, handle, requires) in [
            (
                "i2c",
                "driver/i2c_master.h",
                "i2c_master_dev_handle_t",
                "esp_driver_i2c",
            ),
            (
                "I2C",
                "driver/i2c_master.h",
                "i2c_master_dev_handle_t",
                "esp_driver_i2c",
            ),
            (
                "spi",
                "driver/spi_master.h",
                "spi_device_handle_t",
                "esp_driver_spi",
            ),
            ("uart", "driver/uart.h", "uart_port_t", "esp_driver_uart"),
        ] {
            let files =
                component_files("sps30", ComponentKind::Driver, bus_name).expect("it generates");
            // The IDF header and the handle type live in the **seam**, not in the component's own
            // header: that is what keeps the protocol free of the platform, and what lets the host
            // test swap the whole thing for a fake.
            let seam = files
                .iter()
                .find(|f| f.path == "components/sps30/include/sps30_bus.hpp")
                .expect("a seam");
            let cmake = files
                .iter()
                .find(|f| f.path == "components/sps30/CMakeLists.txt")
                .expect("a manifest");
            assert!(seam.content.contains(include), "{bus_name}: {include}");
            assert!(seam.content.contains(handle), "{bus_name}: {handle}");
            assert!(cmake.content.contains(requires), "{bus_name}: {requires}");
        }
    }

    /// A component is named where a C++ name has to be: the directory, the namespace and the class
    /// all come from the device's name, and none of them is left as a token.
    #[test]
    fn a_component_is_named_and_namespaced() {
        let files =
            component_files("PM Sensor 2", ComponentKind::Driver, "i2c").expect("it generates");
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.contains(&"components/pm_sensor_2/include/pm_sensor_2.hpp"),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"components/pm_sensor_2/src/pm_sensor_2.cpp"),
            "{paths:?}"
        );

        for file in &files {
            for token in [
                "__NAME__",
                "__IDENT__",
                "__NAMESPACE__",
                "__TYPE__",
                "__BUS_INCLUDE__",
                "__BUS_HANDLE__",
                "__REQUIRES__",
            ] {
                assert!(
                    !file.content.contains(token),
                    "{} still carries {token}",
                    file.path
                );
            }
        }
        let header = files
            .iter()
            .find(|f| f.path == "components/pm_sensor_2/include/pm_sensor_2.hpp")
            .unwrap();
        assert!(
            header.content.contains("namespace pm_sensor_2 {"),
            "{}",
            header.content
        );
        assert!(
            header.content.contains("class PmSensor2 {"),
            "{}",
            header.content
        );
        assert!(
            header.content.contains("PmSensor2(BusHandle device);"),
            "{}",
            header.content
        );
        // The manifest and the harness are the tool's; the protocol and its cases are the fill's.
        let cmake = files
            .iter()
            .find(|f| f.path.ends_with("CMakeLists.txt"))
            .unwrap();
        assert!(cmake.structural);
        let fillable: Vec<&str> = files
            .iter()
            .filter(|f| !f.structural)
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(
            fillable,
            vec![
                "components/pm_sensor_2/include/pm_sensor_2.hpp",
                "components/pm_sensor_2/src/pm_sensor_2.cpp",
                "components/pm_sensor_2/test/pm_sensor_2_test.cpp",
            ]
        );
    }

    /// A bus this does not know, and a name that cannot be a C++ identifier, are both refused —
    /// rather than producing a component that does not compile.
    #[test]
    fn a_bad_bus_or_name_is_refused() {
        let err = component_files("sps30", ComponentKind::Driver, "onewire").expect_err("refused");
        assert!(err.contains("'onewire'") && err.contains("i2c"), "{err}");

        let err = component_files("2x-sensor", ComponentKind::Driver, "i2c").expect_err("refused");
        assert!(err.contains("start with a letter"), "{err}");
        assert!(component_files("   ", ComponentKind::Driver, "i2c").is_err());
    }

    /// A project needs a name: it becomes the `project()` name `idf.py` reports and the directory
    /// the project lives in, so there is nothing sensible to fall back to.
    #[test]
    fn a_project_needs_a_name() {
        assert!(library_scaffold("  ", &[]).is_err());
        assert!(application_scaffold("", &[], "../sensors", None).is_err());
    }

    /// Adding a component writes it into a **library**, and refuses an **application** — the check
    /// that keeps the two project types apart in practice rather than only in the documentation.
    #[test]
    fn a_component_goes_into_a_library_and_not_an_application() {
        let library = tempfile::tempdir().expect("a temp dir");
        write_scaffold(
            library.path(),
            &library_scaffold("sensors", &[]).expect("scaffolds"),
        );

        let report = add_component(library.path(), "sps30", ComponentKind::Driver, "i2c")
            .expect("it installs");
        assert_eq!(report["component"], "sps30");
        assert!(library
            .path()
            .join("components/sps30/src/sps30.cpp")
            .is_file());
        assert!(library
            .path()
            .join("components/sps30/include/sps30.hpp")
            .is_file());
        // A second device arrives the same way, without disturbing the first.
        add_component(library.path(), "bme280", ComponentKind::Driver, "i2c").expect("it installs");
        assert!(library
            .path()
            .join("components/bme280/src/bme280.cpp")
            .is_file());
        assert!(library
            .path()
            .join("components/sps30/src/sps30.cpp")
            .is_file());

        // The same call against an application is refused, by name.
        let application = tempfile::tempdir().expect("a temp dir");
        write_scaffold(
            application.path(),
            &application_scaffold("pm25-meter", &[], "../sensors", None).expect("scaffolds"),
        );
        let err = add_component(application.path(), "sps30", ComponentKind::Driver, "i2c")
            .expect_err("refused");
        assert!(err.contains(LIBRARY_MARKER), "{err}");
        assert!(err.contains("application"), "{err}");
    }

    /// An application with a design carries its **managed dependencies** — the board's BSP, and any
    /// component the design marked `published` — in `main/idf_component.yml`.
    ///
    /// In `main/`, not at the project root, because that is what the ESP-IDF component manager
    /// *injects* into `main`'s `REQUIRES`: a manifest at the root would download the board and never
    /// make it includable, and `main/CMakeLists.txt` is structural so nothing else can.
    #[test]
    fn a_designed_application_declares_its_managed_dependencies() {
        use crate::build::application_spec::{examples, parse_spec};
        let trap = parse_spec(examples::INSECT_TRAP).expect("the insect trap parses");
        let out = application_scaffold("insect-trap", &[], "../spire-idf", Some(&trap))
            .expect("scaffolds");

        let manifest = out
            .files
            .iter()
            .find(|file| file.path == "main/idf_component.yml")
            .expect("the application's managed dependencies");
        assert!(manifest.structural, "the tool writes it, not the model");
        assert!(
            manifest.content.contains("waveshare/esp32_p4_nano: \"*\""),
            "the board's BSP is the dependency that carries the display and the pins:\n{}",
            manifest.content
        );
        assert!(
            manifest.content.contains("espressif/esp-dl: \"*\""),
            "and the design's published component is one too:\n{}",
            manifest.content
        );
        // The library is a plain directory, not a managed dependency: a build that reaches the network
        // to find a sibling fails on a bench with no wifi.
        assert!(
            !manifest.content.contains("spire-idf:"),
            "the library is not managed:\n{}",
            manifest.content
        );

        // The published component is **not** named in `REQUIRES`: its real build name is namespaced
        // (`espressif__esp-dl`), which this file has no business guessing — the injected manifest does
        // it. The framework and the components written here are still named.
        let own = out
            .files
            .iter()
            .find(|file| file.path == "main/CMakeLists.txt")
            .expect("the application's own component");
        assert!(
            own.content.contains("REQUIRES ramen")
                && !own.content.contains("esp_dl")
                && !own.content.contains("camera"),
            "only the framework is a library `REQUIRES`: the published runtime is injected by the \
             manifest, and the board's camera is not a component at all:\n{}",
            own.content
        );
    }

    /// An application with **no** design — or whose board names no BSP and which has no published
    /// component — carries no manifest at all, rather than an empty `dependencies:` block that
    /// promises a dependency nobody declared.
    #[test]
    fn an_application_with_no_dependencies_carries_no_manifest() {
        use crate::build::application_spec::{examples, parse_spec};
        let out = application_scaffold("pm25-meter", &[], "../sensors", None).expect("scaffolds");
        assert!(
            !out.files
                .iter()
                .any(|file| file.path == "main/idf_component.yml"),
            "no design means no managed dependencies"
        );

        // A design with no BSP and no published component: the dependency set is empty, so the
        // manifest is not written either.
        let mut bsp_less = parse_spec(examples::PM25_METER).expect("parses");
        bsp_less.board.bsp = String::new();
        let out = application_scaffold("pm25-meter", &[], "../sensors", Some(&bsp_less))
            .expect("scaffolds");
        assert!(
            !out.files
                .iter()
                .any(|file| file.path == "main/idf_component.yml"),
            "an empty dependency set writes no manifest"
        );
    }

    /// **The manifest, corrected against the composition.** The scaffold pins the BSP for a design that
    /// named a board; a composition that reaches for no board support pins none — which is the whole
    /// point, because a BSP is downloaded, compiled and then dead-stripped whole.
    ///
    /// The read is of the **sources on disk**, so a comment that says the display is not wired up yet is
    /// not a use of the display — otherwise the correction would never fire for exactly the composition
    /// it exists for.
    #[test]
    fn finalizing_a_manifest_drops_the_bsp_a_composition_never_uses() {
        use crate::build::application_spec::{examples, parse_spec};
        let spec = parse_spec(examples::PM25_METER).expect("the pm25 meter parses");
        let bsp = spec.board.bsp.clone();
        assert!(!bsp.is_empty(), "the example names a board");

        // The scaffold states the board before the composition exists — the initial, honest guess.
        let scaffold =
            application_scaffold("pm25-meter", &[], "../sensors", Some(&spec)).expect("scaffolds");
        let initial = scaffold
            .files
            .iter()
            .find(|file| file.path == MANIFEST_FILE)
            .expect("the scaffold pins the board");
        assert!(
            initial.content.contains(&format!("{bsp}: \"*\"")),
            "{}",
            initial.content
        );

        // Write it out, then a `main/` that reaches for no board support: a sensor-only composition.
        let tmp = tempfile::tempdir().expect("a temp dir");
        write_scaffold(tmp.path(), &scaffold);
        std::fs::create_dir_all(tmp.path().join("main")).expect("main/");
        std::fs::write(
            tmp.path().join("main/main.cpp"),
            "// the display and the touch panel are not part of this composition yet\n\
             #include \"sps30.hpp\"\n#include \"scheduler.hpp\"\n\
             extern \"C\" void app_main() {}\n",
        )
        .expect("the composition");
        assert!(
            !board_support_referenced(tmp.path()),
            "a comment is not a use of the display"
        );

        let deps = finalize_application_manifest(tmp.path(), &spec).expect("finalizes");
        assert!(deps.is_empty(), "nothing is needed: {deps:?}");
        assert!(
            !tmp.path().join(MANIFEST_FILE).is_file(),
            "an empty dependency set leaves no manifest behind"
        );

        // And the moment the composition reaches for the board, the board is back — by its header.
        std::fs::write(
            tmp.path().join("main/display.cpp"),
            "#include \"bsp/esp-bsp.h\"\nvoid start_display();\n",
        )
        .expect("board support");
        assert!(
            board_support_referenced(tmp.path()),
            "a BSP header is a use"
        );
        let deps = finalize_application_manifest(tmp.path(), &spec).expect("finalizes");
        assert_eq!(deps, vec![bsp.clone()], "the board is pinned again");
        let manifest =
            std::fs::read_to_string(tmp.path().join(MANIFEST_FILE)).expect("the manifest is back");
        assert!(manifest.contains(&format!("{bsp}: \"*\"")), "{manifest}");

        // The BSP's API prefix counts too, whatever header the declaration came in through.
        std::fs::remove_file(tmp.path().join("main/display.cpp")).expect("clears it");
        std::fs::write(
            tmp.path().join("main/main.cpp"),
            "extern \"C\" void app_main() { bsp_display_start(); }\n",
        )
        .expect("a BSP call");
        assert!(board_support_referenced(tmp.path()), "`bsp_` is a use");
    }

    /// A `published` component is **declared, not inferred**: the manifest keeps it even when the
    /// composition reaches for no board support, because the design stated it and the board did not.
    /// Only the BSP is a guess the composition is allowed to overrule.
    #[test]
    fn finalizing_a_manifest_keeps_published_components_without_board_support() {
        use crate::build::application_spec::{examples, parse_spec};
        let spec = parse_spec(examples::INSECT_TRAP).expect("the insect trap parses");
        let bsp = spec.board.bsp.clone();
        assert!(
            !bsp.is_empty() && bsp != "espressif/esp-dl",
            "the example names a board and a separate published component"
        );

        let scaffold = application_scaffold("insect-trap", &[], "../spire-idf", Some(&spec))
            .expect("scaffolds");
        let tmp = tempfile::tempdir().expect("a temp dir");
        write_scaffold(tmp.path(), &scaffold);
        std::fs::create_dir_all(tmp.path().join("main")).expect("main/");
        std::fs::write(
            tmp.path().join("main/main.cpp"),
            "#include \"ramen.hpp\"\nextern \"C\" void app_main() {}\n",
        )
        .expect("the composition");

        let deps = finalize_application_manifest(tmp.path(), &spec).expect("finalizes");
        assert_eq!(
            deps,
            vec!["espressif/esp-dl".to_string()],
            "the published component stays; the board does not"
        );
        let manifest =
            std::fs::read_to_string(tmp.path().join(MANIFEST_FILE)).expect("the manifest remains");
        assert!(manifest.contains("espressif/esp-dl: \"*\""), "{manifest}");
        assert!(!manifest.contains(&format!("{bsp}: \"*\"")), "{manifest}");
    }

    /// Applying a design reports the **published** components apart from the stubs: they are managed
    /// dependencies, so the library is never asked to write them.
    #[test]
    fn applying_a_design_reports_published_components_apart() {
        use crate::build::application_spec::{examples, parse_spec, ComponentRole, UnitSource};
        let library = tempfile::tempdir().expect("a temp dir");
        // `add_component` refuses anything that is not a library, so the library is scaffolded first —
        // which is also what `idf_apply_design`'s caller does.
        write_scaffold(
            library.path(),
            &library_scaffold("sensors", &[]).expect("the library scaffolds"),
        );

        // The PM2.5 meter's drivers are on i2c, which this machine's scaffold writes; a published
        // component is added to it so the report carries one of each source.
        let mut spec = parse_spec(examples::PM25_METER).expect("parses");
        let mut esp_dl = spec.units[0].clone();
        esp_dl.id = "esp_dl".into();
        esp_dl.role = Some(ComponentRole::Library);
        esp_dl.source = Some(UnitSource::Published);
        esp_dl.registry = Some("espressif/esp-dl".into());
        esp_dl.bus = None;
        esp_dl.provides = Some("the detector runtime".into());
        spec.units.push(esp_dl);
        for unit in &mut spec.units {
            if unit.id == "view" {
                unit.uses.push("esp_dl".into());
            }
        }

        let plan = crate::build::application_spec::component_plan(&spec, |_name| false);
        let report = apply_component_plan(library.path(), &plan);

        assert_eq!(
            report["published"],
            serde_json::json!([{ "name": "esp_dl", "registry": "espressif/esp-dl" }])
        );
        let added: Vec<&str> = report["added"]
            .as_array()
            .expect("added")
            .iter()
            .filter_map(|item| item["component"].as_str())
            .collect();
        assert_eq!(added, vec!["sps30", "sht20"], "only the stubs are written");
        assert!(
            !library.path().join("components/esp_dl").exists(),
            "a published component is not written into the library"
        );
        assert!(
            report["errors"].as_array().expect("errors").is_empty(),
            "and nothing was refused: {}",
            report["errors"]
        );
    }

    /// The framework block is what an application is told even when **no design phase ran** — the wizard
    /// chose the framework and the line is in its `CMakeLists.txt` — so the rule that a unit is an actor
    /// and *not* a poll loop has to live here, and not only in the composition block a design produces.
    #[test]
    fn the_actors_framework_block_forbids_a_bare_poll_loop() {
        use crate::build::application_spec::ApplicationFramework;

        let actors = framework_prompt_block(Some(ApplicationFramework::Actors));
        assert!(
            actors.contains("Do not write a bare FreeRTOS loop")
                && actors.contains("`spire::Actor<Message>` with `on_message`")
                && actors.contains("`spire::Scheduler`"),
            "the idiom, and the loop it forbids:\n{actors}"
        );
        assert!(actors.contains("not choose again"), "{actors}");
        // And the layout: each actor is a component of its own, and the message types are shared — the
        // rule that `main/` is wiring and board facts, not the whole product.
        assert!(
            actors.contains("Every actor is a component of its own")
                && actors.contains("components/<id>/include/<id>.hpp")
                && actors.contains("components/messages/include/messages.hpp")
                && actors.contains("spawns and wires only"),
            "each actor is a component of its own, and the types are shared:\n{actors}"
        );
        assert!(
            actors.contains("spire::Registry")
                && actors.contains("registry_.get<PeerMessage>")
                && actors.contains("*scheduler.registry()")
                && actors.contains("in `init()`"),
            "and how a peer a constructor cannot reach is resolved — by name, in `init()`:\n{actors}"
        );
        assert!(
            actors.contains("constructor parameter")
                && actors.contains("spire::Registry::instance()")
                && actors.contains("spire::Registry::get"),
            "and no registry of one's own — the injected reference is the only way to it:\n{actors}"
        );
        assert!(
            !actors.contains("FRAMEWORK: `ramen`"),
            "one framework's block, never both:\n{actors}"
        );
        assert!(
            framework_prompt_block(None).contains("states none"),
            "with nothing stated the library's rule decides:\n{}",
            framework_prompt_block(None)
        );
    }

    /// The composition gate: the design against the sources the fill actually wrote. A flat application
    /// still **compiles**, so this is the only thing that notices the design was ignored — and what it
    /// reports has to be the two things a person would look for first.
    #[test]
    fn the_composition_gate_sees_a_flat_application_and_passes_a_written_one() {
        use crate::build::application_spec::{examples, parse_spec};
        let tmp = tempfile::tempdir().expect("a temp dir");
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let app = parse_spec(examples::PM25_METER).expect("parses");

        // What a live run's fill produced instead of the reviewed design: one poll loop, and its own
        // driver classes rather than the library's `sps30` and `sht20`.
        std::fs::write(
            main.join("main.cpp"),
            "static void sample_task(void *arg) {\n  while (true) { vTaskDelay(pdMS_TO_TICKS(1000)); }\n}\n",
        )
        .unwrap();
        std::fs::write(
            main.join("sensors.h"),
            "class Sps30 {\n public:\n  void begin();\n};\n",
        )
        .unwrap();

        let gaps = composition_gaps(tmp.path(), &app);
        assert!(
            gaps.iter().any(|gap| gap.contains("uses no `spire::Actor")),
            "the framework's own markers are absent: {gaps:?}"
        );
        assert!(
            gaps.iter().any(|gap| gap.contains("declares `Sps30`")),
            "the library's own component was re-declared: {gaps:?}"
        );

        // And a composition that **did** land reports nothing — a check that the design is not
        // obviously absent, never a claim that it is right.
        std::fs::remove_file(main.join("sensors.h")).unwrap();
        std::fs::write(
            main.join("main.cpp"),
            "#include <actor.hpp>\n\
             class Sampler : public spire::Actor<Tick> {\n\
              public:\n\
               void on_message(const Tick &tick) override {}\n\
             };\n",
        )
        .unwrap();
        let gaps = composition_gaps(tmp.path(), &app);
        assert!(gaps.is_empty(), "{gaps:?}");

        // And a tree with nothing written in it is the gap it looks like.
        std::fs::write(main.join("main.cpp"), "void app_main() {}\n").unwrap();
        let gaps = composition_gaps(tmp.path(), &app);
        assert!(
            gaps.iter().any(|gap| gap.contains("uses no `spire::Actor")),
            "an unwritten `main/` is a gap: {gaps:?}"
        );
        std::fs::remove_file(main.join("main.cpp")).unwrap();
        let gaps = composition_gaps(tmp.path(), &app);
        assert!(
            gaps.iter().any(|gap| gap.contains("never written")),
            "an empty `main/` is a gap: {gaps:?}"
        );
    }

    /// Write a scaffold's files to disk, so a test can exercise the filesystem path.
    fn write_scaffold(root: &Path, out: &ScaffoldOutput) {
        for file in &out.files {
            let path = root.join(&file.path);
            std::fs::create_dir_all(path.parent().expect("a path has a parent")).unwrap();
            std::fs::write(&path, &file.content).unwrap();
        }
    }

    /// Write **both project types** out, plus one component, so a real `idf.py build` can be run
    /// against exactly what spire-code produces rather than against a hand-made copy.
    ///
    /// The application names its library `../sensors` — the sibling layout this writes — so the
    /// build exercises the `EXTRA_COMPONENT_DIRS` wiring too, not just the files.
    ///
    /// Ignored by default because it writes outside the target directory:
    ///
    /// ```sh
    /// SPIRE_SCAFFOLD_OUT=/tmp/idf-demo \
    ///     cargo test -p spire-code --lib dump_idf_projects -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes two projects to $SPIRE_SCAFFOLD_OUT, for a real `idf.py build`"]
    fn dump_idf_projects() {
        let root = std::path::PathBuf::from(
            std::env::var("SPIRE_SCAFFOLD_OUT").unwrap_or_else(|_| "/tmp/spire-idf".to_string()),
        );
        let _ = std::fs::remove_dir_all(&root);

        let library_dir = root.join("sensors");
        let library = library_scaffold("sensors", &[]).expect("the library scaffolds");
        write_scaffold(&library_dir, &library);
        println!(
            "library: {} ({} files)",
            library_dir.display(),
            library.files.len()
        );

        let application_dir = root.join("pm25-meter");
        let application = application_scaffold("pm25-meter", &[], "../sensors", None)
            .expect("the application scaffolds");
        write_scaffold(&application_dir, &application);
        println!(
            "app:     {} ({} files)",
            application_dir.display(),
            application.files.len()
        );

        // One component per bus: a single `idf.py build` then proves all three seams and all three
        // stubs compile, which is the only way to check a wrapper for a bus this machine cannot
        // exercise.
        for (name, bus) in [("sps30", "i2c"), ("bme280", "spi"), ("gps", "uart")] {
            let report = add_component(&library_dir, name, ComponentKind::Driver, bus)
                .expect("the component installs");
            println!("component: {}", serde_json::to_string(&report).unwrap());
        }

        // And one **library** component, so the same `idf.py build` proves the other kind compiles
        // too: no seam, no fake, no `REQUIRES`, and a host test that is an ordinary unit test.
        let report = add_component(&library_dir, "moving_average", ComponentKind::Library, "")
            .expect("the library component installs");
        println!("component: {}", serde_json::to_string(&report).unwrap());

        println!(
            "\nlibrary:  cd {} && idf.py set-target esp32s3 build",
            library_dir.display()
        );
        println!(
            "app:      cd {} && idf.py set-target esp32s3 build",
            application_dir.display()
        );
        for name in ["sps30", "bme280", "gps", "moving_average", "ramen"] {
            println!(
                "host test: cd {0}/components/{1}/test && cmake -S . -B build && \
                 cmake --build build && ctest --test-dir build",
                library_dir.display(),
                name
            );
        }
    }

    /// The scaffold writes **two** files from one spec, and both are the composition that was
    /// approved.
    ///
    /// This is the invariant the whole arrangement rests on: if the two are ever written from
    /// different things, a project has two designs and no way to say which one it was built from.
    #[test]
    fn the_composition_and_the_record_are_written_from_one_spec() {
        use crate::build::application_spec::{examples, parse_composition, parse_spec};

        let spec = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let out = application_scaffold("pm25-meter", &[], "../sensors", Some(&spec))
            .expect("the application scaffolds");

        let composition = out
            .files
            .iter()
            .find(|file| file.path == COMPOSITION_FILE)
            .expect("the application carries the composition a person edits");
        assert!(
            composition.structural,
            "the fill phase may not rewrite the architecture it was handed"
        );
        assert_eq!(
            parse_composition(&composition.content)
                .expect("the file it wrote is a composition, readable by the reader it is for"),
            spec,
            "and it is the composition it was given"
        );

        let record = out
            .files
            .iter()
            .find(|file| file.path == APPLICATION_FILE)
            .expect("and the record beside it");
        assert_eq!(
            parse_spec(&record.content).expect("the record parses"),
            spec
        );
    }

    /// On disk, as the scaffold wrote it: what is read back is the composition that was approved.
    #[test]
    fn a_project_reads_back_the_composition_it_carries() {
        use crate::build::application_spec::{examples, parse_spec};

        let spec = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(COMPOSITION_FILE),
            render_composition(&spec).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.path().join(APPLICATION_FILE),
            format!("{}\n", serde_json::to_string_pretty(&spec).unwrap()),
        )
        .unwrap();

        assert_eq!(
            read_application(root.path()).expect("it reads"),
            Some(spec),
            "the composition is what the fill phase is handed"
        );
    }

    /// **The composition decides**, and the record is brought back into agreement with it.
    ///
    /// The failure this guards against is a project whose two files say different things: a person
    /// edits the composition, the record still describes the design that was replaced, and whatever
    /// reads the record builds the older application without complaint.
    #[test]
    fn an_edited_composition_decides_and_the_record_follows_it() {
        use crate::build::application_spec::{examples, parse_spec};

        let approved = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let root = tempfile::tempdir().unwrap();
        // The tree as the scaffold left it: both files, agreeing.
        std::fs::write(
            root.path().join(COMPOSITION_FILE),
            render_composition(&approved).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.path().join(APPLICATION_FILE),
            format!("{}\n", serde_json::to_string_pretty(&approved).unwrap()),
        )
        .unwrap();

        // A person edits the composition — the file's whole reason for existing — and nothing else.
        let mut edited = approved.clone();
        edited.justification = "edited by hand after the review".to_string();
        std::fs::write(
            root.path().join(COMPOSITION_FILE),
            render_composition(&edited).unwrap(),
        )
        .unwrap();

        let read = read_application_and_sync_record(root.path())
            .expect("the edited composition reads")
            .expect("and it is a composition");
        assert_eq!(
            read.justification, "edited by hand after the review",
            "what a person wrote is what is read"
        );
        assert_eq!(
            parse_spec(&std::fs::read_to_string(root.path().join(APPLICATION_FILE)).unwrap())
                .expect("the record parses"),
            read,
            "and the record no longer describes the design that was replaced"
        );
    }

    /// A composition that is there and does not parse is **refused**, not read around.
    ///
    /// The record beside it is valid and plausible, so falling back to it would succeed quietly — and
    /// a project would then be planned from a design its own file no longer states.
    #[test]
    fn a_composition_that_does_not_parse_is_refused_rather_than_read_around() {
        use crate::build::application_spec::{examples, parse_spec};

        let approved = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(APPLICATION_FILE),
            format!("{}\n", serde_json::to_string_pretty(&approved).unwrap()),
        )
        .unwrap();
        std::fs::write(
            root.path().join(COMPOSITION_FILE),
            "framework: actors\nboard: {chip: esp32s3\nunits: []\n",
        )
        .unwrap();

        let err = read_application(root.path()).expect_err("a broken composition is refused");
        assert!(
            err.contains(COMPOSITION_FILE),
            "the refusal names the file a person has to fix, not the one it went without: {err}"
        );
        assert!(
            err.contains("line"),
            "and where in it the problem is: {err}"
        );
    }

    /// **Reading a design writes nothing**, and settling it writes the record — the split the planning
    /// phase depends on.
    ///
    /// `PlanScaffold` resolves a structure in memory and puts nothing on disk until the caller confirms,
    /// so it has to be able to read a project's design without making the one write this arrangement
    /// makes possible. A read that settled the record on its way through would leave
    /// `SPIRE.application.json` behind in a project a person then cancelled.
    #[test]
    fn reading_a_design_writes_nothing_and_settling_it_writes_the_record() {
        use crate::build::application_spec::{examples, parse_spec};

        let spec = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let root = tempfile::tempdir().unwrap();
        // A project designed in its own file: the composition, and no record beside it.
        std::fs::write(
            root.path().join(COMPOSITION_FILE),
            render_composition(&spec).unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_application(root.path()).expect("it reads"),
            Some(spec.clone()),
            "the planning phase is handed the design the file states"
        );
        assert!(
            !root.path().join(APPLICATION_FILE).exists(),
            "and nothing is put on disk on its behalf"
        );

        assert_eq!(
            read_application_and_sync_record(root.path()).expect("it reads and settles"),
            Some(spec.clone()),
            "the leg that is about to write the tree gets the same design"
        );
        assert_eq!(
            parse_spec(&std::fs::read_to_string(root.path().join(APPLICATION_FILE)).unwrap())
                .expect("the record parses"),
            spec,
            "and the record is now there, stating it"
        );

        // A tree whose record is the **only** source is left exactly as it is: there is no composition
        // for the record to follow, and regenerating a file from itself is a write with nothing to say.
        let legacy = tempfile::tempdir().unwrap();
        let compact = serde_json::to_string(&spec).unwrap();
        std::fs::write(legacy.path().join(APPLICATION_FILE), &compact).unwrap();
        assert_eq!(
            read_application_and_sync_record(legacy.path()).expect("the record reads"),
            Some(spec),
            "an application scaffolded before the composition existed still reads"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.path().join(APPLICATION_FILE)).unwrap(),
            compact,
            "and the record it is read from is not rewritten"
        );
    }

    /// A tree that states nothing has not been designed — `None`, not an error — and one carrying only
    /// the record is still an application, which is what a scaffold from before the composition
    /// existed looks like.
    #[test]
    fn a_tree_states_its_design_or_states_nothing() {
        use crate::build::application_spec::{examples, parse_spec};

        let unborn = tempfile::tempdir().unwrap();
        assert_eq!(
            read_application(unborn.path()).expect("nothing stated is not a failure"),
            None
        );

        let spec = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let only_record = tempfile::tempdir().unwrap();
        std::fs::write(
            only_record.path().join(APPLICATION_FILE),
            format!("{}\n", serde_json::to_string_pretty(&spec).unwrap()),
        )
        .unwrap();
        assert_eq!(
            read_application(only_record.path()).expect("the record reads on its own"),
            Some(spec),
            "an application scaffolded before `composition.spire` existed still reads"
        );
    }

    /// **The scaffold resolves the design the tree carries**, and falls back to the caller's only for a
    /// tree that states none — and when the caller's copy is the one dropped, it says so.
    ///
    /// This is the read side of the rule that keeps a scaffold from writing a project whose own files
    /// disagree about what it is — the `REQUIRES` of `main/CMakeLists.txt`, the composition and the
    /// record are all stated from what this returns, so it has to be one design. Two live failures came
    /// from resolving the caller's: with nothing handed in, `REQUIRES` said *"nothing yet"* over a
    /// composition that named components; with a stale one handed in, the authored `composition.spire`
    /// was rendered over.
    ///
    /// The **report** is the other half, and the reason the write legs ask for this form rather than
    /// `design_in_force`: the wizard's review step showed the caller's decomposition, so a person whose
    /// copy was dropped is told here — the alternative is a screen that reviewed one design and a tree
    /// built from another, with nothing saying which.
    #[test]
    fn a_scaffold_resolves_the_design_the_tree_carries() {
        use crate::build::application_spec::{examples, parse_spec};

        let authored = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let stale = parse_spec(examples::INSECT_TRAP).expect("the other worked example parses");

        // A tree that has been designed: the file decides, whatever the caller is still holding.
        let designed = tempfile::tempdir().unwrap();
        std::fs::write(
            designed.path().join(COMPOSITION_FILE),
            render_composition(&authored).unwrap(),
        )
        .unwrap();
        let dropped = design_for_scaffold(
            designed.path(),
            Some(ProjectStructure::IdfApplication),
            Some(stale.clone()),
        )
        .expect("the tree's design resolves");
        assert_eq!(
            dropped.spec,
            Some(authored.clone()),
            "the composition a person edits decides, not the copy a caller still holds"
        );
        let told = dropped
            .dropped
            .expect("a dropped copy is reported, not swallowed");
        assert!(
            told.contains(COMPOSITION_FILE),
            "and the report names the file that decides, so it is actionable: {told}"
        );

        let nothing_handed_in = design_for_scaffold(
            designed.path(),
            Some(ProjectStructure::IdfApplication),
            None,
        )
        .expect("the tree's design resolves");
        assert_eq!(
            nothing_handed_in.spec,
            Some(authored.clone()),
            "and with nothing handed in it is still the tree's, never `None`: this is what `REQUIRES` \
             is stated from, and the failure it prevents is a component the composition names not being \
             required by the application that includes it"
        );
        assert_eq!(
            nothing_handed_in.dropped, None,
            "nothing was dropped, because there was no caller's copy to drop"
        );

        // A tree that has not been designed: the caller's is all there is, and it is used — the ordinary
        // case, a project being created for the first time.
        let unborn = tempfile::tempdir().unwrap();
        let used = design_for_scaffold(
            unborn.path(),
            Some(ProjectStructure::IdfApplication),
            Some(stale.clone()),
        )
        .expect("the caller's design resolves");
        assert_eq!(used.spec, Some(stale.clone()), "there is no file to prefer");
        assert_eq!(
            used.dropped, None,
            "the caller's own design *is* the design here, so there is nothing to report"
        );

        // And the structure gate: a composition lying in a directory someone asked to scaffold as
        // something else is not evidence about what they asked for.
        let gated = design_for_scaffold(
            designed.path(),
            Some(ProjectStructure::IdfLibrary),
            Some(stale.clone()),
        )
        .expect("a library resolves to what it was handed");
        assert_eq!(
            gated.spec,
            Some(stale),
            "the gate is the caller's own word about what to emit"
        );
        assert_eq!(
            gated.dropped, None,
            "no application design is resolved for a library, so there is nothing to report"
        );
    }

    /// **The same rule, without the structure gate** — the form the *reading* passes ask.
    ///
    /// `CoordinatorActor::application_for_pass` resolves `VerifyApplication` and `FinalizeManifest`
    /// through this, so a pass reports on the design the tree states and never on a copy a caller is
    /// still holding: the answer it would otherwise give describes a different application, which is the
    /// write-side failure spelled in the other tense. There is no project type to gate on here — the
    /// caller has already pointed at a body of work, and the question is only what design it states.
    #[test]
    fn a_design_in_a_tree_decides_over_a_copy_a_caller_still_holds() {
        use crate::build::application_spec::{examples, parse_spec};

        let authored = parse_spec(examples::PM25_METER).expect("the worked example parses");
        let stale = parse_spec(examples::INSECT_TRAP).expect("the other worked example parses");

        let designed = tempfile::tempdir().unwrap();
        std::fs::write(
            designed.path().join(COMPOSITION_FILE),
            render_composition(&authored).unwrap(),
        )
        .unwrap();
        assert_eq!(
            design_in_force(designed.path(), Some(stale.clone()))
                .expect("the tree's design resolves"),
            Some(authored),
            "the file a person edits decides, and nothing here asks what was being emitted"
        );

        let unborn = tempfile::tempdir().unwrap();
        assert_eq!(
            design_in_force(unborn.path(), Some(stale.clone()))
                .expect("the caller's design resolves"),
            Some(stale),
            "a tree that states nothing has no design to prefer"
        );
        assert_eq!(
            design_in_force(unborn.path(), None).expect("nothing stated is not a failure"),
            None
        );
    }

    /// A composition that **parses** and does not hold together is refused too — by the file, and with
    /// every broken rule at once.
    ///
    /// The rule is the one a handed-in decomposition is already held to before anything is created
    /// (`CoordinatorActor::params_application`), applied to the file for the same reason: what is in
    /// `composition.spire` is the architecture the project states, not a model's answer on its way to a
    /// repair round. A file whose wiring reaches a unit nobody declared describes an application that
    /// cannot be built, and reading it as the truth puts the failure somewhere that names neither the
    /// file nor the rule.
    #[test]
    fn a_composition_that_does_not_hold_together_is_refused_with_its_rules() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(COMPOSITION_FILE),
            // It parses: every shape and every name is well-formed. What is wrong is the *composition*
            // — an actor that states no message, and an edge to a unit that is not in the spec.
            "framework: actors\n\
             board:\n  chip: esp32s3\n\
             units:\n  - id: sampler\n    kind: actor\n\
             wiring:\n  - \"sampler -> nobody\"\n",
        )
        .unwrap();

        let err = design_for_scaffold(root.path(), Some(ProjectStructure::IdfApplication), None)
            .expect_err("a design that does not hold together is refused");
        assert!(
            err.contains(COMPOSITION_FILE),
            "the refusal names the file a person has to fix: {err}"
        );
        assert!(
            err.contains("an actor with no message"),
            "and the rule the unit breaks: {err}"
        );
        assert!(
            err.contains("'nobody'"),
            "and **every** broken rule rather than the first — a file is repaired in one pass: {err}"
        );
    }
}
