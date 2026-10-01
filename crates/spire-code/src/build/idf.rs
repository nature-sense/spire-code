// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **ESP-IDF** build module: a **C++** ESP-IDF project, built and flashed by `idf.py`.
//!
//! This replaces what used to be three modules — `esp-idf` (Rust over ESP-IDF), `esp-hal` (Rust
//! bare-metal) and `rp2040` — with the one the family is actually built by now. What makes it a
//! module of its own rather than `cmake`'s job is the *invocation*, in three parts:
//!
//! 1. **The chip is a platform fact, not a project one.** `IDF_TARGET` comes from the registry
//!    entry's `architecture.cpu` (`esp32s3`), so one source tree builds for the chip the platform
//!    names rather than the one a developer happened to export.
//! 2. **`idf.py` owns the build.** ESP-IDF is CMake underneath, but `idf.py` is what selects the
//!    toolchain file, sets the target and drives the component manager — so the plan names
//!    `idf.py`, never `cmake` directly.
//! 3. **The flash is the same tool over USB.** `idf.py -p <port> flash`. There is no MCP and no
//!    network leg: a board that has never been flashed has nothing running to talk to, so the only
//!    way onto it is a host-side serial flash.
//!
//! **The environment is conditional, and that is the fourth thing.** A shell that exported ESP-IDF
//! is the person's environment and is left exactly alone: `idf.py` by name, `IDF_TARGET` and nothing
//! else. A process with none — a bundle opened with `open`, which is how an app is normally
//! started — is given the install [`crate::build::idf_env`] resolves, named explicitly enough
//! (`<venv>/bin/python <IDF_PATH>/tools/idf.py`) that the build runs the install the doctor reports
//! rather than whatever `idf.py` means on this `PATH`. `build/run-with-idf.sh` remains the way to
//! launch with an environment a person *chose*; it is no longer the only way a chip build can work.
//!
//! Registered with `AddPlatformModule { os: "esp-idf" }` and **never** with a config file: an
//! ESP-IDF project's `CMakeLists.txt` is what the cmake module claims, and claiming it here would
//! send every CMake project in Spire down the ESP-IDF path.

use crate::build::{
    BuildEvent, BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability, ScaffoldOutput,
};
use crate::platform::Platform;
use async_trait::async_trait;
use spire_actor::Actor;
use spire_core::build_types::BuildSpec;
use std::path::Path;

/// The platform `os` this module answers for.
pub const IDF_OS: &str = "esp-idf";

/// The one tool every part of an ESP-IDF build and flash goes through.
pub const IDF_TOOL: &str = "idf.py";

/// One ESP-IDF invocation: the chip, the arguments, and the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdfPlan {
    /// The chip — `IDF_TARGET`, and the thing a user recognises (`esp32s3`).
    pub chip: String,
    /// Arguments after the program name.
    pub args: Vec<String>,
    /// Environment additions, on top of the inherited environment.
    pub env: Vec<(String, String)>,
}

/// The chip a platform targets — `IDF_TARGET` — or `None` when this is not an ESP-IDF platform.
///
/// `architecture.cpu` is the source because a **resolved** platform is what a build receives: a
/// board states no architecture itself, it declares `chip:` and `build_facts()` brings the
/// silicon's `cpu` across, so a board and its chip name the same target here.
pub fn idf_chip(platform: &Platform) -> Option<String> {
    if platform.os != IDF_OS {
        return None;
    }
    let chip = platform.architecture.cpu.trim();
    (!chip.is_empty()).then(|| chip.to_string())
}

/// The `idf.py build` plan for `platform`, or `None` when this is not an ESP-IDF platform.
///
/// Returning `None` is what stops this module claiming a plain CMake project: a project with a
/// `CMakeLists.txt` and no ESP-IDF platform must go to `CmakeBuildModule`, exactly as before.
pub fn idf_plan(platform: &Platform, opts: &BuildOptions) -> Option<IdfPlan> {
    let chip = idf_chip(platform)?;
    let mut args = vec!["build".to_string()];
    // `mode` maps to CMake's build type — ESP-IDF is CMake underneath and this is the flag that
    // decides optimisation. A debug build is left at IDF's own default rather than asserted.
    if opts.mode.eq_ignore_ascii_case("release") {
        args.push("-DCMAKE_BUILD_TYPE=Release".to_string());
    }
    Some(IdfPlan {
        chip: chip.clone(),
        args,
        env: vec![("IDF_TARGET".to_string(), chip)],
    })
}

/// The `idf.py -p <port> flash` plan, or `None` when this is not an ESP-IDF platform.
///
/// `port` is optional because IDF can detect a single attached board; when it is given it is passed
/// on, which is what makes the operation deterministic on a machine with several devices.
pub fn idf_flash_plan(platform: &Platform, port: Option<&Path>) -> Option<IdfPlan> {
    let chip = idf_chip(platform)?;
    let mut args = Vec::new();
    if let Some(port) = port {
        args.push("-p".to_string());
        args.push(port.to_string_lossy().to_string());
    }
    args.push("flash".to_string());
    Some(IdfPlan {
        chip: chip.clone(),
        args,
        env: vec![("IDF_TARGET".to_string(), chip)],
    })
}

/// The [`BuildSpec`] an [`IdfPlan`] becomes.
///
/// **The plan, and only the plan.** `IDF_TARGET` and nothing else — the SDK and the toolchain belong
/// to the machine, and a plan that changed with whichever machine read it would not be a plan. This
/// is the form a caller prints, compares or routes on; [`spec_from_idf_plan_on_this_machine`] is the
/// form a build runs.
pub fn spec_from_idf_plan(plan: IdfPlan) -> BuildSpec {
    BuildSpec {
        command: IDF_TOOL.to_string(),
        arguments: plan.args,
        working_dir: String::new(),
        env: plan.env,
    }
}

/// The same spec, made runnable **here**: the plan's environment plus what an ESP-IDF install needs,
/// when the process has none of its own.
///
/// `install` is [`crate::build::idf_env::install_for_build`]'s answer, and `None` is the ordinary
/// case on a developer's machine — a shell exported the environment, so the spec stays byte-for-byte
/// what [`spec_from_idf_plan`] returns and nothing about launching changes. With an install in hand
/// the spec stops depending on how the process was started, in two ways:
///
/// 1. **the environment is stated** — `IDF_PATH`, `IDF_TOOLS_PATH`, the venv, `ESP_ROM_ELF_DIR` and
///    the toolchain directories on `PATH` — with the plan's own entries last, so `IDF_TARGET` stays
///    the platform's fact rather than the machine's;
/// 2. **`idf.py` is named**, as `<venv>/bin/python <IDF_PATH>/tools/idf.py`, instead of left to
///    `PATH`. The script's shebang is `#!/usr/bin/env python`, so running it *by name* is really
///    running whatever `python` means, and that is precisely the ambiguity a bare launch cannot
///    resolve. Naming both paths is the difference between the install that was resolved and the one
///    that happens to be first.
///
/// `IDF_TOOL` is still what the plan says the tool is, so nothing downstream has to learn a second
/// name for it; what changes is only which program the spec hands to the process runner.
pub fn spec_from_idf_plan_on_this_machine(
    plan: IdfPlan,
    install: Option<&crate::build::idf_env::IdfInstall>,
) -> BuildSpec {
    let mut spec = spec_from_idf_plan(plan);
    if let Some(install) = install {
        spec.env = install.merge_env(&spec.env);
        if let Some((python, idf_py)) = install.idf_py_invocation() {
            let mut arguments = vec![idf_py.to_string_lossy().to_string()];
            arguments.append(&mut spec.arguments);
            spec.command = python.to_string_lossy().to_string();
            spec.arguments = arguments;
        }
    }
    spec
}

/// The resolved platform a build *or* a flash both need, or the refusal naming what is missing.
///
/// One function so the two paths cannot drift in what they accept — while the message names the
/// operation, because "an ESP-IDF build needs a platform" is a confusing thing to read when you
/// asked for a flash.
fn idf_platform(op: &str, opts: &BuildOptions) -> Result<Platform, String> {
    let platform_id = opts
        .platform
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| format!("an ESP-IDF {op} needs a platform, e.g. \"m5stack-core-s3\""))?;
    let platform = Platform::from_registry(platform_id)
        .ok_or_else(|| format!("unknown platform '{platform_id}'"))?;
    if idf_chip(&platform).is_none() {
        return Err(format!(
            "platform '{platform_id}' is not an ESP-IDF platform (os '{}'): this module builds \
             ESP-IDF projects",
            platform.os
        ));
    }
    Ok(platform)
}

/// Run an `idf.py build` for `opts.platform`, through the **shared** process runner.
///
/// Reusing `run_build_spec` rather than spawning a `Command` here means environment handling,
/// duration measurement and exit-code reporting behave exactly as they do for every other module.
///
/// The environment comes from [`spec_from_idf_plan_on_this_machine`], so a build started without an
/// exported ESP-IDF (a bundle opened with `open`) resolves one instead of failing for a reason that
/// has nothing to do with the application. Resolving it here rather than in the actor keeps every
/// entry point — the tool, the module message, a test — on the same path.
pub async fn run_idf_build(path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
    let platform = idf_platform("build", opts)?;
    let plan = idf_plan(&platform, opts)
        .ok_or_else(|| format!("platform '{}' has no ESP-IDF build", platform.id))?;
    let install = crate::build::idf_env::install_for_build();
    crate::build::generic_helpers::run_build_spec(
        path,
        &spec_from_idf_plan_on_this_machine(plan, install.as_ref()),
    )
    .await
}

/// Run `idf.py -p <port> flash` for `opts.platform`.
///
/// A host-side serial flash, deliberately: the board may be running nothing at all, so this is the
/// one operation that cannot depend on the board already speaking to us. The environment is resolved
/// exactly as for a build — a flash needs the toolchain and `esptool` from the same venv, so the two
/// paths must not disagree about which install they run.
pub async fn run_idf_flash(
    path: &Path,
    opts: &BuildOptions,
    port: Option<&Path>,
) -> Result<BuildOutput, String> {
    let platform = idf_platform("flash", opts)?;
    let plan = idf_flash_plan(&platform, port)
        .ok_or_else(|| format!("platform '{}' has no ESP-IDF flash", platform.id))?;
    let install = crate::build::idf_env::install_for_build();
    crate::build::generic_helpers::run_build_spec(
        path,
        &spec_from_idf_plan_on_this_machine(plan, install.as_ref()),
    )
    .await
}

/// The ESP-IDF build module.
///
/// Owns the *invocation* and nothing else. Analysis is deliberately not its job: an ESP-IDF
/// project's `CMakeLists.txt` is a CMake project, so the cmake module keeps that — the same split
/// the old esp module had with cargo, where only the invocation differs.
#[derive(Debug, Clone, Copy, Default)]
pub struct IdfBuildModule;

impl IdfBuildModule {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Actor for IdfBuildModule {
    type Message = BuildModuleMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: IDF_OS.to_string(),
                    // **One** file, and deliberately not the `CMakeLists.txt`: an ESP-IDF project
                    // *is* a CMake project, so claiming that would capture every CMake project in
                    // Spire. `sdkconfig.defaults` is the one file in an ESP-IDF tree that is
                    // ESP-IDF's alone — and claiming it is what makes a *scaffold* request routable
                    // here, because `BuildManager::scaffold_build_config` routes by config file and
                    // has no platform to route by.
                    config_files: vec![crate::build::idf_projects::CONFIG_FILE.to_string()],
                    // The label a caller spells, so it matches the convention the other modules
                    // set (`"Meson"`, `"Cargo"`) and `module_for_build_system("ESP-IDF")` finds it.
                    // The old `"Cargo (esp-idf)"` form existed to tell two flavours of one module
                    // apart, and there is one flavour now.
                    build_system: "ESP-IDF".to_string(),
                    language: "C++".to_string(),
                    // Empty on purpose, like the config files. The project *is* a CMake project, so
                    // parsing and analysis belong to the cmake module; what this one owns is the
                    // invocation, and claiming the extensions would take them away from there.
                    source_extensions: Vec::new(),
                    mcp_servers: Vec::new(),
                    // The one operation this module is for: `idf.py -p <port> flash`, over USB.
                    supports_flash: true,
                    // Declared false so the manager refuses these *before* routing to us: a clean or
                    // a lint here would run against chip build output rather than the project.
                    supports_clean: false,
                    supports_lint: false,
                    supports_format: false,
                    supports_fix: false,
                });
            }

            BuildModuleMessage::Build {
                path,
                opts,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(run_idf_build(&path, &opts).await);
            }

            BuildModuleMessage::BuildStreaming {
                path,
                opts,
                event_tx,
                reply_to,
                ..
            } => {
                let result = run_idf_build(&path, &opts).await;
                // One synthetic finished line, mirroring how the other modules fall back when they
                // cannot stream per-line. Worth knowing: the first build for a chip configures the
                // whole IDF project, so it is minutes rather than seconds.
                let _ = event_tx.send(BuildEvent {
                    line: format!(
                        "Finished {} in {:?}s",
                        path.display(),
                        result.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)
                    ),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::Flash {
                path,
                opts,
                port,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(run_idf_flash(&path, &opts, port.as_deref()).await);
            }

            // The two operations here that are not invocations: emitting the skeleton of either
            // ESP-IDF project type. Both are this module's because both are `os: esp-idf` — a
            // library and an application differ in what they contain, not in what compiles them.
            BuildModuleMessage::ScaffoldBuildConfig {
                project_name,
                platforms,
                structure,
                library,
                application,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(scaffold_for(
                    structure,
                    &project_name,
                    &platforms,
                    library.as_deref(),
                    application.as_ref(),
                ));
            }

            _ => {
                // Everything else (test/lint/clean/fix/parse/analyze) is either refused by the
                // capability above — so the manager never routes it here — or belongs to the cmake
                // module, which owns an ESP-IDF project's `CMakeLists.txt`. Warn rather than reply,
                // so a future routing change surfaces here instead of being silently swallowed.
                tracing::warn!(
                    "IdfBuildModule: a message it does not implement was routed here; the build \
                     manager should have refused or routed it elsewhere"
                );
            }
        }
    }
}

/// The scaffold this module emits for a requested structure, or the refusal naming what it does not
/// build.
///
/// Both ESP-IDF project types are answered here, and everything else is refused **by name**: a
/// caller that asked for `Native` and was handed a firmware project would have no way to tell.
fn scaffold_for(
    structure: spire_core::build_types::ProjectStructure,
    project_name: &str,
    platforms: &[String],
    library: Option<&str>,
    application: Option<&crate::build::application_spec::ApplicationSpec>,
) -> Result<ScaffoldOutput, String> {
    use spire_core::build_types::ProjectStructure;

    match structure {
        ProjectStructure::IdfLibrary => {
            crate::build::idf_projects::library_scaffold(project_name, platforms)
        }
        // A library is not required to scaffold an application — an application with local
        // components is legitimate, if unusual — so an unnamed one is passed through and the
        // scaffold says in its `CMakeLists.txt` that nothing was named.
        ProjectStructure::IdfApplication => crate::build::idf_projects::application_scaffold(
            project_name,
            platforms,
            library.unwrap_or_default(),
            application,
        ),
        other => Err(format!(
            "the esp-idf module scaffolds the two ESP-IDF project types ('{}' and '{}'); '{}' \
             belongs to the module that owns its build system",
            ProjectStructure::IdfLibrary.as_str(),
            ProjectStructure::IdfApplication.as_str(),
            other.as_str(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{PlatformArchitecture, PlatformSysroot, PlatformToolchain};
    use tempfile::TempDir;

    /// A **resolved** ESP-IDF board, as a build receives it: the chip on `architecture.cpu`, the
    /// BSP named, and no Rust toolchain anywhere.
    fn board(os: &str, cpu: &str) -> Platform {
        Platform {
            id: "m5stack-core-s3".into(),
            name: "M5Stack Core S3".into(),
            os: os.into(),
            architecture: PlatformArchitecture {
                cpu_family: "xtensa".into(),
                cpu: cpu.into(),
                endian: "little".into(),
                target_triple: "xtensa-esp32s3-elf".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            device: None,
            family: Some("esp32".into()),
            chip: None,
            hal: None,
            bsp: Some("m5stack_core_s3".into()),
            rust: None,
            library_hints: None,
        }
    }

    /// The chip is `IDF_TARGET`, read from the resolved platform's architecture — not from a
    /// `rust.idf_target` that a C++ platform no longer carries.
    #[test]
    fn the_chip_is_the_resolved_architecture() {
        assert_eq!(
            idf_chip(&board(IDF_OS, "esp32s3")).as_deref(),
            Some("esp32s3")
        );
    }

    /// A platform with no chip is refused rather than planned with an empty `IDF_TARGET` — an
    /// unresolved board would otherwise build for whatever target happened to be exported.
    #[test]
    fn a_platform_with_no_chip_is_not_planned() {
        assert!(idf_plan(&board(IDF_OS, ""), &BuildOptions::default()).is_none());
    }

    /// A Linux platform is not this module's, which is what keeps a plain CMake project on the
    /// cmake module's path.
    #[test]
    fn a_non_esp_platform_is_not_planned() {
        let linux = board("linux", "aarch64");
        assert!(idf_plan(&linux, &BuildOptions::default()).is_none());
        assert!(idf_flash_plan(&linux, None).is_none());
    }

    /// The plan names `idf.py`, passes the chip as `IDF_TARGET`, and contributes **no** other
    /// environment: the SDK and the toolchain are things this machine already has.
    #[test]
    fn the_plan_is_idf_py_with_the_target() {
        let plan = idf_plan(&board(IDF_OS, "esp32s3"), &BuildOptions::default()).expect("plans");
        assert_eq!(plan.chip, "esp32s3");
        assert_eq!(plan.args, vec!["build"]);
        assert_eq!(
            plan.env,
            vec![("IDF_TARGET".to_string(), "esp32s3".to_string())]
        );
    }

    /// Release maps to CMake's build type — ESP-IDF is CMake underneath, and this is the flag that
    /// decides optimisation.
    #[test]
    fn a_release_build_names_the_cmake_build_type() {
        let opts = BuildOptions {
            mode: "release".into(),
            ..BuildOptions::default()
        };
        let plan = idf_plan(&board(IDF_OS, "esp32s3"), &opts).expect("plans");
        assert_eq!(plan.args, vec!["build", "-DCMAKE_BUILD_TYPE=Release"]);
    }

    /// The flash names the port when there is one and omits it when there is not — IDF detects a
    /// single attached board itself, and inventing a port would be a guess.
    #[test]
    fn the_flash_passes_the_port_only_when_given() {
        let with = idf_flash_plan(&board(IDF_OS, "esp32s3"), Some(Path::new("/dev/ttyUSB0")))
            .expect("plans");
        assert_eq!(with.args, vec!["-p", "/dev/ttyUSB0", "flash"]);

        let without = idf_flash_plan(&board(IDF_OS, "esp32s3"), None).expect("plans");
        assert_eq!(without.args, vec!["flash"]);
    }

    /// Both paths become the same shape of spec, so the shared runner handles environment, duration
    /// and exit codes identically for a build and for a flash.
    #[test]
    fn a_plan_becomes_an_idf_py_spec() {
        let plan = idf_plan(&board(IDF_OS, "esp32s3"), &BuildOptions::default()).expect("plans");
        let spec = spec_from_idf_plan(plan);
        assert_eq!(spec.command, "idf.py");
        assert_eq!(spec.arguments, vec!["build"]);
        assert!(spec.working_dir.is_empty());
        assert_eq!(
            spec.env,
            vec![("IDF_TARGET".to_string(), "esp32s3".to_string())]
        );
    }

    /// An install with real paths — a venv interpreter and `tools/idf.py` that exist — because which
    /// of them is *found* is what decides whether the spec names the install or falls back to
    /// `idf.py` by name.
    fn fake_install() -> (TempDir, crate::build::idf_env::IdfInstall) {
        let temp = TempDir::new().unwrap();
        let venv = temp.path().join("python_env/idf5.5_py3.14_env");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::write(venv.join("bin/python"), "").unwrap();
        let idf_path = temp.path().join("esp-idf/v5.5.5");
        std::fs::create_dir_all(idf_path.join("tools")).unwrap();
        std::fs::write(idf_path.join("tools/idf.py"), "# idf.py\n").unwrap();
        let install = crate::build::idf_env::IdfInstall {
            idf_path,
            tools_path: temp.path().to_path_buf(),
            python_env: Some(venv),
        };
        (temp, install)
    }

    /// Given an install, the spec names the program **and** the script — the venv's interpreter on
    /// the install's `tools/idf.py` — and states the environment, while `IDF_TARGET` stays the
    /// platform's fact and is stated exactly once.
    #[test]
    fn an_install_makes_the_spec_self_contained() {
        let (_temp, install) = fake_install();
        let plan = idf_plan(&board(IDF_OS, "esp32s3"), &BuildOptions::default()).expect("plans");
        let spec = spec_from_idf_plan_on_this_machine(plan, Some(&install));

        assert_eq!(
            spec.command,
            install
                .python()
                .expect("the venv has an interpreter")
                .to_string_lossy(),
            "the venv's own python, not whatever `python` means on PATH"
        );
        assert_eq!(
            spec.arguments,
            vec![
                install.idf_py().to_string_lossy().to_string(),
                "build".to_string()
            ]
        );
        assert!(
            spec.env.contains(&(
                "IDF_PATH".to_string(),
                install.idf_path.to_string_lossy().to_string()
            )),
            "{:?}",
            spec.env
        );
        assert_eq!(
            spec.env
                .iter()
                .filter(|(key, _)| key == "IDF_TARGET")
                .count(),
            1,
            "the plan's target is stated once, by the plan"
        );
        assert!(spec
            .env
            .contains(&("IDF_TARGET".to_string(), "esp32s3".to_string())));
    }

    /// With no install — the exported-environment case — the spec is the plan and nothing else, so a
    /// shell that arranged its own environment sees the command it has always seen.
    #[test]
    fn without_an_install_the_spec_is_the_plan() {
        let plan = idf_plan(&board(IDF_OS, "esp32s3"), &BuildOptions::default()).expect("plans");
        let spec = spec_from_idf_plan_on_this_machine(plan.clone(), None);
        let pure = spec_from_idf_plan(plan);

        assert_eq!(spec.command, "idf.py");
        assert_eq!(spec.arguments, pure.arguments);
        assert_eq!(spec.env, pure.env);
    }

    /// An install with no venv interpreter still states its environment but cannot name a program:
    /// `idf.py` by name is the honest fallback there, and IDF's own message is what a person sees.
    #[test]
    fn an_install_with_no_venv_still_states_the_environment() {
        let temp = TempDir::new().unwrap();
        let idf_path = temp.path().join("esp-idf/v5.5.5");
        std::fs::create_dir_all(idf_path.join("tools")).unwrap();
        std::fs::write(idf_path.join("tools/idf.py"), "# idf.py\n").unwrap();
        let install = crate::build::idf_env::IdfInstall {
            idf_path: idf_path.clone(),
            tools_path: temp.path().to_path_buf(),
            python_env: None,
        };
        let plan = idf_plan(&board(IDF_OS, "esp32s3"), &BuildOptions::default()).expect("plans");
        let spec = spec_from_idf_plan_on_this_machine(plan, Some(&install));

        assert_eq!(spec.command, "idf.py");
        assert_eq!(spec.arguments, vec!["build"]);
        assert!(
            spec.env.contains(&(
                "IDF_PATH".to_string(),
                idf_path.to_string_lossy().to_string()
            )),
            "{:?}",
            spec.env
        );
    }

    /// The capability is what the manager gates on, and it has to name the one config file that
    /// makes a scaffold routable here — without it `scaffold_build_config` finds no module for the
    /// container and the project can never be created at all.
    #[tokio::test]
    async fn the_capability_claims_the_scaffold_config_file() {
        let mut module = IdfBuildModule::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        module
            .handle(BuildModuleMessage::DescribeCapabilities { reply_to: tx })
            .await;
        let capability = rx.await.expect("the capability is sent");

        assert_eq!(capability.name, "esp-idf");
        // The label a scaffold caller passes to `module_for_build_system`, and the language as the
        // second way to name this module — either spelling must reach it.
        assert_eq!(capability.build_system, "ESP-IDF");
        assert_eq!(capability.language, "C++");
        assert!(capability.supports_flash);
        assert_eq!(
            capability.config_files,
            vec![crate::build::idf_projects::CONFIG_FILE.to_string()]
        );
    }

    /// Both ESP-IDF project types are this module's; any other structure is refused **by name**
    /// rather than answered with a firmware project the caller did not ask for.
    #[test]
    fn the_two_idf_project_types_are_scaffolded_and_nothing_else() {
        use spire_core::build_types::ProjectStructure;

        let library = scaffold_for(ProjectStructure::IdfLibrary, "sensors", &[], None, None)
            .expect("the library scaffolds");
        assert_eq!(library.structure, ProjectStructure::IdfLibrary);
        assert!(
            !library
                .build_content
                .contains(crate::build::application_spec::APPLICATION_FRAMEWORK_KEY),
            "a library states no framework — it is built against nothing and belongs to neither"
        );

        // The decomposition the design phase produced and a person reviewed, as the scaffold is given
        // it: the framework is stated in the file, and the spec itself is written beside it.
        let designed = crate::build::application_spec::parse_spec(
            crate::build::application_spec::examples::PM25_METER,
        )
        .expect("the worked example parses");
        let application = scaffold_for(
            ProjectStructure::IdfApplication,
            "pm25-meter",
            &[],
            Some("../sensors"),
            Some(&designed),
        )
        .expect("the application scaffolds");
        assert_eq!(application.structure, ProjectStructure::IdfApplication);
        assert!(
            application.build_content.contains("../sensors"),
            "the library it is built against is named in its CMakeLists.txt"
        );
        // The path is used **as given** and resolved against the application only when it is relative.
        // A flow that names an absolute library — the wizard does, and a live run did — otherwise gets
        // `<app>/<the whole absolute path>/components`, which is not a directory and reads like one:
        // cmake's error was `Add the installation prefix of "…/components" to CMAKE_PREFIX_PATH`.
        assert!(
            application.build_content.contains("IS_ABSOLUTE"),
            "{}",
            application.build_content
        );
        assert!(
            application
                .build_content
                .contains(r#"list(APPEND EXTRA_COMPONENT_DIRS "${SPIRE_LIBRARY_DIR}/components")"#),
            "the library directory is not concatenated onto the application's own:\n{}",
            application.build_content
        );
        let absolute = scaffold_for(
            ProjectStructure::IdfApplication,
            "pm25-meter",
            &[],
            Some("/opt/sensors"),
            None,
        )
        .expect("an absolute library path is accepted");
        assert!(
            absolute
                .build_content
                .contains(r#"set(SPIRE_LIBRARY_DIR "/opt/sensors")"#),
            "an absolute path is written as it was given — the `if(NOT IS_ABSOLUTE …)` is what\n             keeps it from being resolved against this application:\n{}",
            absolute.build_content
        );
        // The framework the design phase decided is *stated* in the file, so the fill phase, a
        // person and another tool all read the same choice back instead of inferring one.
        assert_eq!(
            crate::build::application_spec::declared_framework(&application.build_content),
            Ok(Some(
                crate::build::application_spec::ApplicationFramework::Actors
            )),
            "the application states its framework in its CMakeLists.txt:\n{}",
            application.build_content
        );
        // And the decomposition itself is in the tree, so the fill phase writes the composition that
        // was reviewed rather than inventing one — structural, because changing it means designing
        // again rather than editing the record of what was designed.
        let spec_file = application
            .files
            .iter()
            .find(|file| file.path == crate::build::idf_projects::APPLICATION_FILE)
            .expect("the application carries its decomposition");
        assert!(
            spec_file.structural,
            "the reviewed design is not a file to edit"
        );
        assert_eq!(
            crate::build::application_spec::parse_spec(&spec_file.content)
                .expect("and it is the spec that was approved"),
            designed
        );

        let refused = scaffold_for(ProjectStructure::Native, "notes-app", &[], None, None)
            .expect_err("refused");
        assert!(
            // The enum's own keys, so this asserts the wording a caller sees rather than a prettier
            // spelling the code never produces.
            refused.contains("'idf_library'")
                && refused.contains("'idf_application'")
                && refused.contains("'native'"),
            "{refused}"
        );
    }

    /// A **real chip build** on the environment this module injects — the whole claim, and
    /// unprovable without running the toolchain.
    ///
    /// Writes a minimal ESP-IDF project into a temporary directory and builds it through the same
    /// [`BuildSpec`] path a build takes, then asserts the firmware was linked. Run it from a shell
    /// with **nothing** exported, which is the case it is about:
    ///
    /// ```sh
    /// env -u IDF_PATH -u IDF_TOOLS_PATH -u IDF_PYTHON_ENV_PATH -u SPIRE_IDF_EXPORT \
    ///     cargo test -p spire-code --lib a_chip_build_runs_on_the_injected_environment \
    ///     -- --ignored --nocapture
    /// ```
    ///
    /// Ignored by default because it configures and compiles ESP-IDF: minutes on a warm `ccache`, and
    /// a few hundred megabytes under the temporary directory.
    #[tokio::test]
    #[ignore = "builds a real ESP-IDF project for esp32s3: minutes, and it needs the toolchain"]
    async fn a_chip_build_runs_on_the_injected_environment() {
        use crate::build::idf_env::{install_for_build, resolve_idf_install};

        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.16)\n\
             include($ENV{IDF_PATH}/tools/cmake/project.cmake)\n\
             project(spire_env_probe)\n",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join("main")).unwrap();
        std::fs::write(
            temp.path().join("main/CMakeLists.txt"),
            "idf_component_register(SRCS \"main.c\" INCLUDE_DIRS \".\")\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("main/main.c"),
            "#include <stdio.h>\nvoid app_main(void) { printf(\"env probe\\n\"); }\n",
        )
        .unwrap();

        // `resolve_idf_install` and not `install_for_build`: whether *this* process happens to have an
        // exported environment must not change what is being tested. What it has is printed instead,
        // so both cases are visible in the log.
        let install = resolve_idf_install().expect("an ESP-IDF install to build with");
        println!("install:   {:?}", install);
        println!("would add: {}", install_for_build().is_some());

        let plan = idf_plan(&board(IDF_OS, "esp32s3"), &BuildOptions::default()).expect("plans");
        let spec = spec_from_idf_plan_on_this_machine(plan, Some(&install));
        println!("runs:      {} {}", spec.command, spec.arguments.join(" "));

        let output = crate::build::generic_helpers::run_build_spec(temp.path(), &spec)
            .await
            .expect("the runner spawns it");
        println!("{}", output.output);

        assert!(
            output.success,
            "a chip build must work on the environment this module injects:\n{}",
            output.output
        );
        assert!(
            temp.path().join("build/spire_env_probe.bin").is_file(),
            "the firmware was linked:\n{}",
            output.output
        );
    }

    /// Read the **real** registry entry through the real code — `~/.spire/<app>/boards/
    /// m5stack-core-s3.yaml`, resolved to its chip the way a build resolves it — so the module and
    /// the data cannot disagree about what an ESP-IDF build is: the target, the tool, and that the
    /// board carries a BSP rather than a Rust toolchain.
    ///
    /// Ignored by default because it reads outside the target directory, which is the caller's
    /// decision rather than a test's. `SPIRE_APP_NAME` is needed because a test binary has no
    /// application name of its own (`spire-core` falls back to a neutral default) and would
    /// otherwise read `~/.spire/spire/`:
    ///
    /// ```sh
    /// SPIRE_APP_NAME=spire-code cargo test -p spire-code --lib dump_idf_registry_entry \
    ///     -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "reads the platform stores from ~/.spire/<app>/{boards,platforms,chips}"]
    fn dump_idf_registry_entry() {
        let platform = Platform::from_registry("m5stack-core-s3").expect(
            "the CoreS3 resolves out of the registry — set SPIRE_APP_NAME to the app whose \
             stores hold it (`SPIRE_APP_NAME=spire-code`)",
        );
        let plan = idf_plan(&platform, &BuildOptions::default()).expect("it plans a build");
        let flash = idf_flash_plan(&platform, Some(Path::new("/dev/cu.usbmodem1101")))
            .expect("it plans a flash");

        println!("os:          {}", platform.os);
        println!("target:      {}", plan.chip);
        println!("bsp:         {:?}", platform.bsp);
        println!("rust block:  {:?}", platform.rust.is_some());
        println!("build spec:  {:?}", spec_from_idf_plan(plan));
        println!("flash spec:  {:?}", spec_from_idf_plan(flash));
    }
}
