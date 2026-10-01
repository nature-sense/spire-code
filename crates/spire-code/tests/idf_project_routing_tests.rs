// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The two ESP-IDF project types, routed the way a caller routes them.
//!
//! `BuildManager` routes `Build`, `BuildStreaming` and `Flash` by **platform** (`opts.platform`),
//! but a scaffold by **config file**: `scaffold_build_config` looks the module up by the file it is
//! asked for and has no platform to route by. So a module is reachable for a scaffold only if the
//! config file it claims reaches the config *router* — a link worth a test, because without it a
//! module registers, describes itself, answers builds and flashes, and still cannot create a project
//! of its kind.

use spire_code::build::idf_projects::{declares_application, declares_library, CONFIG_FILE};
use spire_code::build::{BuildModuleMessage, IdfBuildModule, ModuleCapability};
use spire_code::subsystems::build::{BuildManagerActor, BuildManagerMessage};
use spire_core::build_types::ProjectStructure;
use spire_core::subsystems::graph::memory_graph::MemoryGraphMessage;

use spire_actor::ActorSystem;
use tokio::sync::mpsc;

/// Spawn a build module and return its capability + message sender.
async fn describe_module<A: spire_actor::Actor<Message = BuildModuleMessage>>(
    system: &ActorSystem,
    module: A,
) -> (ModuleCapability, mpsc::Sender<BuildModuleMessage>) {
    let (tx, _handle) = system.spawn(module);
    let (r_tx, r_rx) = tokio::sync::oneshot::channel();
    tx.send(BuildModuleMessage::DescribeCapabilities { reply_to: r_tx })
        .await
        .unwrap();
    (r_rx.await.unwrap(), tx)
}

/// A channel whose messages are drained (mock memory-graph persistence).
fn drain_sender<T: Send + 'static>() -> mpsc::Sender<T> {
    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}

/// A `BuildManager` with the ESP-IDF module registered, and the sender requests go through.
async fn manager_with_idf(system: &ActorSystem) -> mpsc::Sender<BuildManagerMessage> {
    let (idf_cap, idf_tx) = describe_module(system, IdfBuildModule::new()).await;
    let (bm_tx, _bm_handle) =
        system.spawn(BuildManagerActor::new(drain_sender::<MemoryGraphMessage>()));
    bm_tx
        .send(BuildManagerMessage::AddPlatformModule {
            os: "esp-idf".to_string(),
            capability: idf_cap,
            module_tx: idf_tx,
        })
        .await
        .unwrap();
    bm_tx
}

/// Ask for a scaffold, through the manager, exactly as a caller does.
async fn scaffold(
    bm_tx: &mpsc::Sender<BuildManagerMessage>,
    name: &str,
    structure: ProjectStructure,
    library: Option<&str>,
    application: Option<&spire_code::build::application_spec::ApplicationSpec>,
) -> Result<spire_code::build::ScaffoldOutput, String> {
    let (r_tx, r_rx) = tokio::sync::oneshot::channel();
    bm_tx
        .send(BuildManagerMessage::ScaffoldBuildConfig {
            project_name: name.to_string(),
            goal: "a product".to_string(),
            build_file: CONFIG_FILE.to_string(),
            platforms: Vec::new(),
            structure: Some(structure),
            embedded: true,
            library: library.map(str::to_string),
            application: application.cloned(),
            reply_to: r_tx,
        })
        .await
        .unwrap();
    r_rx.await.unwrap()
}

/// A **component library** reaches the module that claims the config file, and comes back as a
/// library: components, a build harness, and no actors.
#[tokio::test]
async fn a_library_request_scaffolds_a_library() {
    let system = ActorSystem::new();
    let bm_tx = manager_with_idf(&system).await;

    let out = scaffold(&bm_tx, "sensors", ProjectStructure::IdfLibrary, None, None)
        .await
        .expect("the library scaffolds");

    assert_eq!(out.structure, ProjectStructure::IdfLibrary);
    assert!(out.embedded);
    assert!(
        declares_library(&out.build_content),
        "it declares itself a library"
    );
    assert!(!declares_application(&out.build_content));
    assert!(out.build_content.contains("project(sensors)"));

    let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
    for path in [
        "main/build_harness.cpp",
        "sdkconfig.defaults",
        "SPIRE.md",
        // …and its **framework**, which is what a library starts with now: the task seam and the two
        // application frameworks built on it, shipped rather than left to be generated.
        "components/toolkit/include/task.hpp",
        "components/ramen/include/ramen.hpp",
        "components/actors/include/actor.hpp",
    ] {
        assert!(paths.contains(&path), "{path} is missing from a library");
    }
    // Still not an application: no `app_main` of the product.
    assert!(out.files.iter().all(|f| !f.path.ends_with("main.cpp")));
    // …and the hints, which is where an architecture *is* written down: the two frameworks, and when
    // to reach for which.
    let hints = out
        .files
        .iter()
        .find(|f| f.path == "SPIRE.md")
        .expect("the hints are emitted");
    assert!(hints.content.contains("How to use it"));
    assert!(
        hints.content.contains("Choosing a framework"),
        "the hints say which frameworks are here and when to reach for which"
    );
}

/// An **application** comes back as an application: `main/` with the wiring seam, and the library it
/// is built against named in its `CMakeLists.txt`.
#[tokio::test]
async fn an_application_request_scaffolds_an_application() {
    let system = ActorSystem::new();
    let bm_tx = manager_with_idf(&system).await;

    let out = scaffold(
        &bm_tx,
        "pm25-meter",
        ProjectStructure::IdfApplication,
        Some("../sensors"),
        None,
    )
    .await
    .expect("the application scaffolds");

    assert_eq!(out.structure, ProjectStructure::IdfApplication);
    assert!(declares_application(&out.build_content));
    assert!(!declares_library(&out.build_content));
    assert!(out.build_content.contains("project(pm25-meter)"));
    // The dependency is a directory, and it is in the file rather than in a convention.
    assert!(
        out.build_content
            .contains("set(SPIRE_LIBRARY_DIR \"../sensors\")"),
        "{}",
        out.build_content
    );
    assert!(out.build_content.contains("EXTRA_COMPONENT_DIRS"));

    // `main.cpp` is blank — an entry point and nothing else. The wiring seam is gone, because how a
    // product is composed is the library's to describe, not the scaffold's.
    let main_cpp = out
        .files
        .iter()
        .find(|f| f.path == "main/main.cpp")
        .expect("main.cpp is emitted");
    assert!(main_cpp.content.contains("app_main"));
    assert!(
        main_cpp.content.contains("kTag = \"pm25-meter\""),
        "the log tag is the product's own name"
    );
    assert!(!main_cpp.content.contains("spire::"));
    assert!(!main_cpp.content.contains("// spire:wire"));
    // An application with **no design** has no components of its own yet: the units a design names are
    // what become components, and inventing them here would be inventing an architecture.
    assert!(out.files.iter().all(|f| !f.path.starts_with("components/")));
}

/// An application that names no library is still scaffolded, and says in its build file that
/// nothing was named — rather than reaching for a sibling that may not exist.
#[tokio::test]
async fn an_application_without_a_library_says_so() {
    let system = ActorSystem::new();
    let bm_tx = manager_with_idf(&system).await;

    let out = scaffold(
        &bm_tx,
        "lonely",
        ProjectStructure::IdfApplication,
        None,
        None,
    )
    .await
    .expect("the application scaffolds");

    assert!(
        out.build_content.contains("No library named"),
        "{}",
        out.build_content
    );
    assert!(!out.build_content.contains("EXTRA_COMPONENT_DIRS"));
}

/// The **decomposition** reaches the application's tree, through the manager — the framework stated
/// in its own file *and* the spec itself written beside it. This is what stops the fill phase writing
/// a different application from the one that was reviewed: the model reads the composition back from
/// here, and so can a person.
#[tokio::test]
async fn a_designed_application_states_its_framework_and_carries_its_decomposition() {
    use spire_code::build::application_spec::{
        declared_framework, examples, parse_spec, ApplicationFramework,
    };
    use spire_code::build::idf_projects::APPLICATION_FILE;

    let designed = parse_spec(examples::PM25_METER).expect("the worked example parses");
    let system = ActorSystem::new();
    let bm_tx = manager_with_idf(&system).await;

    let out = scaffold(
        &bm_tx,
        "pm25-meter",
        ProjectStructure::IdfApplication,
        Some("../sensors"),
        Some(&designed),
    )
    .await
    .expect("the application scaffolds");

    assert!(
        out.build_content
            .contains(&ApplicationFramework::Actors.marker_line()),
        "the choice is stated, not inferred:\n{}",
        out.build_content
    );
    assert_eq!(
        declared_framework(&out.build_content),
        Ok(Some(ApplicationFramework::Actors)),
        "and reads back as what it says"
    );

    let carried = out
        .files
        .iter()
        .find(|file| file.path == APPLICATION_FILE)
        .expect("the application carries its decomposition");
    assert!(
        carried.structural,
        "the reviewed design is not the model's to edit"
    );
    assert_eq!(
        parse_spec(&carried.content).expect("and it is the spec that was approved"),
        designed
    );
    // A reader who wonders why `main/` is shaped the way it is should be one file away from the answer.
    let readme = out
        .files
        .iter()
        .find(|file| file.path == "README.md")
        .expect("the README");
    assert!(
        readme.content.contains(APPLICATION_FILE),
        "{}",
        readme.content
    );
    // And the components the composition calls are named in the application's **own** component: the
    // manifest is structural, so the scaffold names them from the reviewed design — an include of a
    // component nobody declares fails at the `#include`, far from the line that caused it. The
    // **framework's** components come first, then this composition's own — the shared `messages` and one
    // component per actor, which `main.cpp` includes to spawn and wire — and then the components of the
    // library the design wraps.
    let own_component = out
        .files
        .iter()
        .find(|file| file.path == "main/CMakeLists.txt")
        .expect("the application's own component");
    assert!(
        own_component
            .content
            .contains("REQUIRES actors toolkit messages sampler air_quality view touch power sps30 sht20 moving_average"),
        "{}",
        own_component.content
    );

    // **Each actor is a component of its own**, and the message types are a component beside them —
    // emitted by the scaffold, structural, with `REQUIRES` named from the design, so the fill writes the
    // headers and nothing about the architecture.
    for path in [
        "components/messages/CMakeLists.txt",
        "components/sampler/CMakeLists.txt",
        "components/air_quality/CMakeLists.txt",
        "components/view/CMakeLists.txt",
        "components/touch/CMakeLists.txt",
        "components/power/CMakeLists.txt",
    ] {
        let file = out
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} is emitted"));
        assert!(file.structural, "{path} is the scaffold's, not the model's");
    }
    let sampler = out
        .files
        .iter()
        .find(|file| file.path == "components/sampler/CMakeLists.txt")
        .expect("sampler's component");
    assert!(
        sampler
            .content
            .contains("REQUIRES actors messages sps30 sht20)"),
        "{}",
        sampler.content
    );
    assert_eq!(
        out.fill_roots,
        vec!["main", "components"],
        "and both trees the composition occupies are the fill phase's"
    );

    // The **library** states none and carries none: it is built against nothing, belongs to neither
    // framework, and a library that claimed an application's decomposition would be claiming its
    // product's shape.
    let library = scaffold(&bm_tx, "sensors", ProjectStructure::IdfLibrary, None, None)
        .await
        .expect("the library scaffolds");
    assert!(
        !library
            .files
            .iter()
            .any(|file| file.path == APPLICATION_FILE),
        "a library carries no application's decomposition"
    );
}

/// Write a scaffolded application **onto disk**, so that a real `idf.py build` can read it.
///
/// Ignored, and not an assertion: it is the other half of the live loop. `system_flow_tests` drives a
/// model into a tree and pays for the model; this writes the tree alone, so that a *manifest* can be
/// handed to cmake without one.
///
/// It exists because a generated `CMakeLists.txt` is read by **cmake**, and three real defects of this
/// scaffold were invisible to every assertion in this file — a leftover `__FRAMEWORK_BLOCK__` line, an
/// absolute library path concatenated onto the application's own, and a `SRCS` glob that IDF takes as a
/// literal filename. Each one was found by running this and reading cmake's complaint.
///
/// ```text
/// SPIRE_SCRATCH_APP_OUT=/tmp/app SPIRE_SCRATCH_LIBRARY=/tmp/lib \
///   cargo test -p spire-code --test idf_project_routing_tests -- --ignored --nocapture \
///   scratch_writes_an_application_tree
/// ```
///
/// `SPIRE_SCRATCH_SPEC` is the point of it for a tree that already exists: name the
/// `SPIRE.application.json` a live run produced, and this re-scaffolds that application's tree as the
/// current code would write it — the same design, the same library, the same composition, but a fresh
/// manifest. Rebuilding the scaffold of a live run without another model call is how the manifest bugs
/// above were chased down.
#[tokio::test]
#[ignore = "writes a tree outside the test's tempdir, for a person to run `idf.py build` in"]
async fn scratch_writes_an_application_tree_for_a_person_to_build() {
    use spire_code::build::application_spec::{examples, parse_spec};

    let out_dir = std::env::var("SPIRE_SCRATCH_APP_OUT")
        .expect("SPIRE_SCRATCH_APP_OUT is the directory to write the application into");
    let library = std::env::var("SPIRE_SCRATCH_LIBRARY")
        .unwrap_or_else(|_| "/tmp/spire-app-one/library".to_string());
    let designed = match std::env::var("SPIRE_SCRATCH_SPEC") {
        Ok(path) => {
            let text = std::fs::read_to_string(&path).expect("the spec is readable");
            parse_spec(&text).expect("the spec parses")
        }
        Err(_) => parse_spec(examples::PM25_METER).expect("the worked example parses"),
    };

    let system = ActorSystem::new();
    let bm_tx = manager_with_idf(&system).await;
    let out = scaffold(
        &bm_tx,
        "pm25-meter",
        ProjectStructure::IdfApplication,
        Some(&library),
        Some(&designed),
    )
    .await
    .expect("the application scaffolds");

    let root = std::path::Path::new(&out_dir);
    for file in &out.files {
        let path = root.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the tree is creatable");
        }
        std::fs::write(&path, &file.content).expect("a scaffolded file is writable");
        println!(
            "wrote {}{}",
            path.display(),
            if file.structural { " (structural)" } else { "" }
        );
    }
    println!(
        "scaffolded {} files into {}, against the library {library}",
        out.files.len(),
        root.display()
    );
}
