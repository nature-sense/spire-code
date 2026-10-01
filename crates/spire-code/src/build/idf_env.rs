// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **ESP-IDF environment** — found, tested, and repaired.
//!
//! The IDF build module does no `PATH` surgery of its own: it runs `idf.py` and expects the
//! environment `export.sh` sets ([`super::idf`] says why, and `build/run-with-idf.sh` arranges it
//! for a launch). That is right for the *product* — Spire references an install this machine has
//! rather than inventing one — but it leaves the two things a person actually hits on a machine:
//!
//! * an install that is **present yet unusable** — `export.sh` aborts, or `idf.py` does not run — so
//!   the build fails for a reason that has nothing to do with the application;
//! * a process with **no environment at all**, which is what a bundle opened with `open` has — so a
//!   chip build fails there and works in the terminal, the worst version of this, because it makes
//!   the app look broken rather than the environment.
//!
//! Both are the app's to answer, so this module is the app's own doctor — and, when the process has
//! no environment, its supplier ([`install_for_build`]):
//!
//! * [`resolve_idf_install`] **finds** the install the way `run-with-idf.sh` does — `SPIRE_IDF_EXPORT`,
//!   then `IDF_PATH`, then `<tools>/esp-idf/<version>` — and the venv **by listing**, never by
//!   deriving its name. The derived name is what broke here (`idf5.5_py3.14_env`, not `py3.12`), and
//!   a name that is *found* cannot drift from a name that is *guessed*.
//! * [`doctor_idf_environment`] **tests** it: does `idf.py --version` run, does `export.sh` activate
//!   at all, and what do `idf_tools.py check` **and** `idf_tools.py export` say is missing — both,
//!   because they disagree about a tool that is only in `PATH`, and it is `export` that decides
//!   whether `export.sh` activates.
//! * [`repair_idf_environment`] **fixes** it: `idf_tools.py install` for exactly the tools the doctor
//!   named, then the doctor again.
//! * [`install_for_build`] is what the **build** path calls: the install to run in when this process
//!   carries no environment of its own, and `None` when a shell already arranged one — because an
//!   environment a person exported is theirs, and replacing it would be inventing an install rather
//!   than referencing one.
//!
//! Nothing here sources a shell to *build the environment*: the environment is **synthesised** in
//! Rust (the venv's python, the toolchain directories on `PATH`, `ESP_ROM_ELF_DIR`), the same
//! fallback `run-with-idf.sh` uses. `export.sh` is only ever *asked* to activate, in its own
//! subshell, because whether it activates is a fact worth reporting.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::generic_helpers::{run_cmd, run_cmd_with_env};

/// The directory an ESP-IDF install and its tools live under when nothing overrides it.
pub const DEFAULT_TOOLS_PATH_DIR: &str = ".espressif";

/// An ESP-IDF install found on this machine, with the pieces a build needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdfInstall {
    /// The `IDF_PATH` — the install directory, holding `tools/idf.py` and `export.sh`.
    pub idf_path: PathBuf,
    /// The `IDF_TOOLS_PATH` — the directory holding `tools/`, `dist/` and `python_env/`.
    pub tools_path: PathBuf,
    /// The virtualenv IDF's python packages live in, when one was found.
    pub python_env: Option<PathBuf>,
}

impl IdfInstall {
    /// The real `idf.py` script, which the venv's python runs directly.
    pub fn idf_py(&self) -> PathBuf {
        self.idf_path.join("tools").join("idf.py")
    }

    /// `idf_tools.py` — the installer/checker the repair drives.
    pub fn idf_tools_py(&self) -> PathBuf {
        self.idf_path.join("tools").join("idf_tools.py")
    }

    /// The activation script whose failure started all of this.
    pub fn export_sh(&self) -> PathBuf {
        self.idf_path.join("export.sh")
    }

    /// The venv's python, or `None` when there is no venv (or it has no interpreter).
    ///
    /// `bin/python` on unix, `Scripts/python.exe` on Windows — checked rather than assumed, because
    /// the whole point of this module is not to assume.
    pub fn python(&self) -> Option<PathBuf> {
        let env = self.python_env.as_ref()?;
        for candidate in ["bin/python", "Scripts/python.exe"] {
            let path = env.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
        None
    }

    /// The `esp-rom-elfs` directory `ESP_ROM_ELF_DIR` names — without it every `idf.py` configure
    /// prints a gdbinit warning (`esp_rom/gen_gdbinit.py`).
    pub fn esp_rom_elfs(&self) -> Option<PathBuf> {
        expand(&self.tools_path.join("tools").join("esp-rom-elfs"), &["*"])
            .into_iter()
            .next()
    }

    /// The toolchain directories to prepend to `PATH`, in the order `export.sh` would have set them.
    ///
    /// Only the dirs that *exist*: an install with no `esp-clang` must not put a dead path in front
    /// of a working compiler.
    pub fn tool_bin_dirs(&self) -> Vec<PathBuf> {
        let tools = self.tools_path.join("tools");
        let mut dirs = Vec::new();
        dirs.extend(expand(
            &tools.join("xtensa-esp-elf"),
            &["*", "xtensa-esp-elf", "bin"],
        ));
        dirs.extend(expand(
            &tools.join("riscv32-esp-elf"),
            &["*", "riscv32-esp-elf", "bin"],
        ));
        dirs.extend(expand(&tools.join("esp-clang"), &["*", "esp-clang", "bin"]));
        dirs.extend(expand(&tools.join("ninja"), &["*"]));
        dirs.extend(expand(
            &tools.join("cmake"),
            &["*", "CMake.app", "Contents", "bin"],
        ));
        dirs.extend(expand(&tools.join("cmake"), &["*", "bin"]));
        // The venv's own `bin` — the parent of the interpreter `python()` found, so the macOS/unix
        // `bin` and the Windows `Scripts` case are one line rather than two that can disagree.
        if let Some(bin) = self
            .python()
            .and_then(|python| python.parent().map(Path::to_path_buf))
        {
            dirs.push(bin);
        }
        dirs.retain(|dir| dir.is_dir());
        dirs
    }

    /// The `PATH` a build needs: the toolchain dirs, then whatever this process already had.
    ///
    /// The inherited `PATH` is kept (appended, not replaced) so the system tools IDF falls back to —
    /// `ccache`, `git` — still resolve.
    pub fn composed_path(&self) -> Option<String> {
        let dirs = self.tool_bin_dirs();
        if dirs.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = dirs
            .iter()
            .map(|dir| dir.to_string_lossy().to_string())
            .collect();
        if let Some(inherited) = std::env::var_os("PATH") {
            parts.push(inherited.to_string_lossy().to_string());
        }
        Some(parts.join(if cfg!(windows) { ";" } else { ":" }))
    }

    /// The environment additions that make `idf.py` runnable without `export.sh`.
    ///
    /// Exactly what `run-with-idf.sh` synthesises, in Rust: `IDF_PATH`, `IDF_TOOLS_PATH`, the venv,
    /// `ESP_ROM_ELF_DIR`, and the toolchain on `PATH`.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            (
                "IDF_PATH".to_string(),
                self.idf_path.to_string_lossy().to_string(),
            ),
            (
                "IDF_TOOLS_PATH".to_string(),
                self.tools_path.to_string_lossy().to_string(),
            ),
        ];
        if let Some(python_env) = self.python_env.as_ref() {
            env.push((
                "IDF_PYTHON_ENV_PATH".to_string(),
                python_env.to_string_lossy().to_string(),
            ));
        }
        if let Some(rom) = self.esp_rom_elfs() {
            env.push((
                "ESP_ROM_ELF_DIR".to_string(),
                rom.to_string_lossy().to_string(),
            ));
        }
        if let Some(path) = self.composed_path() {
            env.push(("PATH".to_string(), path));
        }
        env
    }

    /// The environment additions for a process whose plan already states `plan_env` — the plan's own
    /// entries **last**, so the plan wins.
    ///
    /// `IDF_TARGET` is the whole reason for the rule: it is a fact about the platform being built
    /// ([`super::idf`] point 1), so a machine that happens to have `IDF_TARGET` exported must not be
    /// able to move it. Reading the order off the vector rather than de-duplicating by hand also
    /// means the precedence is whatever `Command::env` does last-wins, which is one rule, not two.
    pub fn merge_env(&self, plan_env: &[(String, String)]) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = self
            .env()
            .into_iter()
            .filter(|(key, _)| !plan_env.iter().any(|(planned, _)| planned == key))
            .collect();
        env.extend(plan_env.iter().cloned());
        env
    }

    /// The program and its first argument that run *this* install's `idf.py` with no shell and no
    /// shim: the venv's own python, on `tools/idf.py`. `None` when there is no venv interpreter.
    ///
    /// This is what `export.sh` arranges by putting the venv first on `PATH` — the script's shebang
    /// is `#!/usr/bin/env python`, so `idf.py` *by name* is really "whatever `python` means" — and
    /// what `run-with-idf.sh` has to write a shim for when it cannot arrange it. Naming both paths is
    /// the difference between running the install that was resolved and the one that happens to be
    /// first on this `PATH`.
    pub fn idf_py_invocation(&self) -> Option<(PathBuf, PathBuf)> {
        Some((self.python()?, self.idf_py()))
    }
}

/// Directory children of `root` whose name satisfies `pred`.
fn matching_dirs(root: &Path, pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if pred(name) {
                out.push(path);
            }
        }
    }
    out
}

/// Expand `root` against a path pattern where `"*"` matches any one directory level.
///
/// A tiny glob, on purpose: the patterns here are the fixed handful `export.sh` uses for its
/// toolchain directories, and a real glob crate would be a dependency bought for those strings.
fn expand(root: &Path, pattern: &[&str]) -> Vec<PathBuf> {
    match pattern.split_first() {
        None => {
            if root.is_dir() {
                vec![root.to_path_buf()]
            } else {
                Vec::new()
            }
        }
        Some((&"*", rest)) => {
            let mut out = Vec::new();
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        out.extend(expand(&path, rest));
                    }
                }
            }
            out
        }
        Some((segment, rest)) => {
            let path = root.join(segment);
            if path.is_dir() {
                expand(&path, rest)
            } else {
                Vec::new()
            }
        }
    }
}

/// Push `name` unless it is already there — the check output names a tool once per section and the
/// error line names it again.
fn push_unique(list: &mut Vec<String>, name: &str) {
    let name = name.trim();
    if !name.is_empty() && !list.iter().any(|existing| existing == name) {
        list.push(name.to_string());
    }
}

/// Single-quote a path for `bash -c`, so a path with a space still works.
fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

/// Every place an ESP-IDF install or venv is looked for, for a report that has to say where it
/// looked. The rule this project keeps learning: a gate that cannot run must name what it tried.
pub fn looked_at(tools_path: &Path) -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("$HOME"));
    [
        "$SPIRE_IDF_EXPORT".to_string(),
        "$IDF_PATH".to_string(),
        tools_path
            .join("esp-idf")
            .join("<version>")
            .to_string_lossy()
            .to_string(),
        home.join(".espressif")
            .join("esp-idf")
            .join("<version>")
            .to_string_lossy()
            .to_string(),
    ]
    .join(", ")
}

/// **Find** the ESP-IDF install on this machine — or `None` when there is none.
///
/// Reads the process environment and the home directory; [`resolve_idf_install_with`] is the pure
/// half, so the search is testable against a temporary tree.
pub fn resolve_idf_install() -> Option<IdfInstall> {
    resolve_idf_install_with(
        std::env::var("IDF_PATH").ok(),
        std::env::var("IDF_TOOLS_PATH").ok(),
        std::env::var("IDF_PYTHON_ENV_PATH").ok(),
        std::env::var("SPIRE_IDF_EXPORT").ok(),
        dirs::home_dir(),
    )
}

/// The testable half of [`resolve_idf_install`]: same search, explicit inputs, no process state.
///
/// The order matters and is the order `run-with-idf.sh` tries: an explicit `SPIRE_IDF_EXPORT` (a
/// path to an `export.sh`, whose directory *is* the install), then `IDF_PATH`, then the newest
/// `<tools>/esp-idf/<version>` the installer wrote. Newest first because a machine with two installs
/// is a machine where the newer one is the one a person means.
pub fn resolve_idf_install_with(
    idf_path: Option<String>,
    tools_path: Option<String>,
    python_env: Option<String>,
    export_hint: Option<String>,
    home: Option<PathBuf>,
) -> Option<IdfInstall> {
    fn non_empty(value: String) -> Option<String> {
        let trimmed = value.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    let tools_path = tools_path
        .and_then(non_empty)
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(DEFAULT_TOOLS_PATH_DIR)))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TOOLS_PATH_DIR));

    let mut candidates: Vec<PathBuf> = Vec::new();
    // `SPIRE_IDF_EXPORT` names an `export.sh`; its directory is the install.
    if let Some(hint) = export_hint.and_then(non_empty) {
        let path = PathBuf::from(hint);
        candidates.push(if path.is_dir() {
            path
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        });
    }
    if let Some(path) = idf_path.and_then(non_empty) {
        candidates.push(PathBuf::from(path));
    }
    // Newest first: version names sort, so `v5.5.5` beats `v5.4`.
    let mut installed = expand(&tools_path.join("esp-idf"), &["*"]);
    installed.sort();
    installed.reverse();
    candidates.extend(installed);

    let idf_path = candidates
        .into_iter()
        .find(|path| path.join("tools").join("idf.py").is_file())?;

    // The venv is **listed**, never derived: `idf<major.minor>_py<python>_env` is a name the
    // installer writes and this module must not reproduce by guessing.
    let python_env = python_env
        .and_then(non_empty)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            let mut envs = matching_dirs(&tools_path.join("python_env"), |name| {
                name.starts_with("idf") && name.ends_with("_env")
            });
            envs.sort();
            envs.reverse();
            envs.into_iter().next()
        });

    Some(IdfInstall {
        idf_path,
        tools_path,
        python_env,
    })
}

/// The install a **build or flash** should run in — or `None` when this process was launched with an
/// environment already exported for the same install.
///
/// `None` does not mean "no ESP-IDF here": it means *there is nothing to add*. A process started by
/// `build/run-with-idf.sh`, or from a shell where `export.sh` succeeded, has `IDF_PATH` and the venv
/// already in front of `PATH`; that environment is the person's, and replacing it would be this
/// module inventing an install rather than referencing one. A process started by `open` has neither,
/// and that is the case this exists for: the install is resolved here and handed to
/// [`super::idf::spec_from_idf_plan_on_this_machine`], so a chip build works from a bundle nobody
/// wrapped.
///
/// Note what *is* overridden: a valid `IDF_PATH` whose install we resolved is still injected **when
/// the venv was never arranged** (the hand-built environment this machine needed before `idf.py` ran
/// at all). Adding the venv is what that environment was missing, so this is a repair rather than a
/// takeover.
pub fn install_for_build() -> Option<IdfInstall> {
    let install = resolve_idf_install()?;
    let exported = is_exported_for(
        &install,
        std::env::var("IDF_PATH").ok().as_deref(),
        std::env::var("PATH").ok().as_deref(),
    );
    (!exported).then_some(install)
}

/// Whether the process environment already carries `install`: its `IDF_PATH`, **and** its venv's
/// directory already on `PATH`.
///
/// Both halves, because they are one arrangement rather than two settings. `export.sh` sets
/// `IDF_PATH` *and* puts the venv first so that `idf.py` — a script whose shebang is
/// `#!/usr/bin/env python` — resolves to the interpreter that has IDF's packages; the venv half is
/// the one that makes the command work, so an `IDF_PATH` without it is not an environment we may
/// stand on.
///
/// The comparison is `canonicalize`-tolerant because `/var/folders/…` and `/private/var/folders/…`
/// are the same directory on macOS and only one of them is what a person typed.
fn is_exported_for(install: &IdfInstall, idf_path: Option<&str>, path: Option<&str>) -> bool {
    let Some(stated) = idf_path
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    else {
        return false;
    };
    if !(stated == install.idf_path || same_dir(&stated, &install.idf_path)) {
        return false;
    }
    let Some(venv_bin) = install
        .python()
        .and_then(|python| python.parent().map(Path::to_path_buf))
    else {
        return false;
    };
    path_has_dir(path, &venv_bin)
}

/// Whether `PATH` lists `dir` as a whole entry — a substring test would let `/venv/bin` match
/// `/venv/bin-old`, which is exactly the kind of near-miss this module exists to catch.
fn path_has_dir(path: Option<&str>, dir: &Path) -> bool {
    let Some(path) = path else {
        return false;
    };
    path.split(if cfg!(windows) { ';' } else { ':' })
        .any(|entry| {
            let entry = Path::new(entry.trim());
            !entry.as_os_str().is_empty() && (entry == dir || same_dir(entry, dir))
        })
}

/// Whether two paths name the same directory once symlinks are resolved — `false` rather than a
/// guess when either cannot be resolved.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// What `idf_tools.py check` says, in the two ways that matter here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolCheck {
    /// Required tools the check could not find anywhere — the ones that make `export.sh` abort.
    pub missing: Vec<String>,
    /// Tools it found only in `PATH`, at a version IDF does not accept.
    pub unsupported: Vec<String>,
}

/// Read `idf_tools.py check`'s output.
///
/// The check prints a per-tool block (`Checking tool <name>` then indented status lines) and, when
/// required tools are absent, an `ERROR:` line naming them. Both are read: the error line is
/// authoritative for *required* tools, and the blocks catch a tool with no version anywhere even if
/// the error line is formatted differently in another IDF version. A tool with a `PATH` version is
/// **not** missing — `openocd-esp32` is found in `PATH` here and IDF accepts it.
pub fn parse_idf_tools_check(output: &str) -> ToolCheck {
    let mut check = ToolCheck::default();
    let mut current: Option<String> = None;
    let mut have_version = false;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ERROR:") {
            if let Some((_, after)) = rest.split_once("required tools were not found:") {
                for name in after.split_whitespace() {
                    push_unique(&mut check.missing, name);
                }
            }
        }
        if let Some(name) = trimmed.strip_prefix("Checking tool ") {
            // A new block begins: the previous one, if it never named a version, is missing.
            if let Some(previous) = current.replace(name.trim().to_string()) {
                if !have_version {
                    push_unique(&mut check.missing, &previous);
                }
            }
            have_version = false;
            continue;
        }
        if let Some(name) = current.as_ref() {
            if trimmed.starts_with("version found in PATH:")
                || trimmed.starts_with("version installed in tools directory:")
            {
                have_version = true;
            }
            // `version found in PATH: 4.3.1: not supported` / `not compatible`.
            if trimmed.contains("in PATH")
                && (trimmed.contains("not supported") || trimmed.contains("not compatible"))
            {
                push_unique(&mut check.unsupported, name);
            }
        }
    }
    if let Some(previous) = current {
        if !have_version {
            push_unique(&mut check.missing, &previous);
        }
    }
    check
}

/// Read `idf_tools.py export`'s output — the step `export.sh` runs to establish the environment.
///
/// It is read **as well as** `check` because the two *disagree*, and the disagreement is what made an
/// environment that `check` called complete still refuse to activate here: `check` accepts a `PATH`
/// copy of a required tool (`openocd-esp32` at `0.12.0`), while `export` demands one installed in the
/// tools directory, says so (`ERROR: tool openocd-esp32 has no installed versions`), and returns
/// non-zero — at which point `export.sh` gives up. So a tool named here is missing in the only sense
/// that matters for activation, which is the whole reason this module exists.
///
/// The `PATH` versions IDF refuses are collected too: they are why `cmake`, `ninja` and `esp-clang`
/// are *not* used from the system even though they are present.
pub fn parse_idf_tools_export(output: &str) -> ToolCheck {
    let mut check = ToolCheck::default();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ERROR: tool ") {
            if let Some((name, _)) = rest.split_once(" has no installed versions") {
                push_unique(&mut check.missing, name);
            }
        }
        // `Not using an unsupported version of tool cmake found in PATH: 4.3.1.` and
        // `An unsupported version of tool openocd-esp32 was found in PATH: 0.12.0.` — one wording is
        // a warning, the other a statement, and the tool's name sits after the same phrase in both.
        if let Some((_, after)) = trimmed.split_once("unsupported version of tool ") {
            let name = after.split_whitespace().next().unwrap_or("");
            push_unique(&mut check.unsupported, name.trim_end_matches(['.', ':']));
        }
    }
    check
}

/// Fold one check's findings into another's, naming each tool once.
fn merge_checks(mut into: ToolCheck, from: ToolCheck) -> ToolCheck {
    for name in from.missing {
        push_unique(&mut into.missing, &name);
    }
    for name in from.unsupported {
        push_unique(&mut into.unsupported, &name);
    }
    into
}

/// The environment, as tested.
///
/// `success` is the question a build asks — *can `idf.py` run?* — and it is deliberately not
/// `missing.is_empty()`: the tools that block `export.sh` are the **debug** tools (gdb, openocd),
/// which a build and a flash never invoke. So a machine can be perfectly **buildable** and still need
/// [`repair_idf_environment`] to become *exportable*, and a report that conflated the two would send
/// a person to repair an environment that already builds.
#[derive(Debug, Clone, Serialize)]
pub struct IdfEnvReport {
    pub success: bool,
    pub found: bool,
    pub idf_path: Option<String>,
    pub tools_path: Option<String>,
    pub python_env: Option<String>,
    pub version: Option<String>,
    /// Tools the environment has no *installed* copy of — the union of what `idf_tools.py check` and
    /// `idf_tools.py export` refuse, because the two disagree about a `PATH` copy.
    pub missing: Vec<String>,
    /// Tools found in `PATH` at a version IDF will not use — reported even when IDF's own copy is
    /// installed, because it is *why* the system `cmake`/`ninja`/`openocd` is not the one used.
    pub unsupported: Vec<String>,
    /// Whether `export.sh` activates in its own subshell — the user-facing "is my IDF set up?".
    pub export_sh_works: bool,
    /// Whether `idf.py --version` runs through the venv python, which is what a build needs.
    pub idf_py_runs: bool,
    /// The exact command that installs what is missing, when something is.
    pub repair_command: Option<String>,
    /// A plain-language summary — the line that distinguishes "the build failed" from "your machine
    /// has no ESP-IDF in its environment".
    pub detail: String,
}

/// **Test** the ESP-IDF environment: find the install, prove `idf.py` runs, and read what
/// `idf_tools.py check` and `idf_tools.py export` say is missing. Never writes anything.
pub async fn doctor_idf_environment() -> IdfEnvReport {
    match resolve_idf_install() {
        Some(install) => doctor_install(&install).await,
        None => not_found_report(),
    }
}

/// The tools path this process would use, for a report that has to say where it looked.
fn tools_path_for_report() -> PathBuf {
    std::env::var_os("IDF_TOOLS_PATH")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|home| home.join(DEFAULT_TOOLS_PATH_DIR)))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TOOLS_PATH_DIR))
}

/// The report when there is no install at all — with the places it looked, so the next step is clear.
fn not_found_report() -> IdfEnvReport {
    let tools_path = tools_path_for_report();
    IdfEnvReport {
        success: false,
        found: false,
        idf_path: None,
        tools_path: Some(tools_path.to_string_lossy().to_string()),
        python_env: None,
        version: None,
        missing: Vec::new(),
        unsupported: Vec::new(),
        export_sh_works: false,
        idf_py_runs: false,
        repair_command: None,
        detail: format!(
            "no ESP-IDF install found (looked at {}); set SPIRE_IDF_EXPORT to your export.sh, or \
             install ESP-IDF, to build a chip application",
            looked_at(&tools_path)
        ),
    }
}

/// Doctor one resolved install: version, tool check, and whether `export.sh` activates.
async fn doctor_install(install: &IdfInstall) -> IdfEnvReport {
    let env = install.env();
    let python = install.python();

    // The version probe runs `idf.py` through **its own venv python**, so neither `export.sh` nor a
    // shim is needed — if this fails, `idf.py` genuinely cannot run here.
    let version = match python.as_ref() {
        Some(py) => run_idf_version(install, py, &env).await,
        None => None,
    };
    let idf_py_runs = version.is_some();

    // **Both** halves of the tool question: `check` says what IDF cannot find, and `export` says what
    // it refuses to activate without — and they disagree about a tool that is only in `PATH`.
    let check = match python.as_ref() {
        Some(py) => merge_checks(
            run_tool_check(install, py, &env).await,
            run_tool_export(install, py, &env).await,
        ),
        None => ToolCheck::default(),
    };

    let export_sh_works = export_sh_activates(install).await;

    let repair_command = match (python.as_ref(), check.missing.is_empty()) {
        (Some(py), false) => Some(format!(
            "{} {} install {}",
            py.display(),
            install.idf_tools_py().display(),
            check.missing.join(" ")
        )),
        _ => None,
    };

    // The version string carries its own `ESP-IDF ` prefix (`ESP-IDF v5.5.5`), so the sentences
    // below name it rather than prefixing it a second time.
    let idf = version.as_deref().unwrap_or("this ESP-IDF install");

    let detail = if !idf_py_runs {
        format!(
            "ESP-IDF at {} does not run: `idf.py --version` failed{}. Check the venv ({}).",
            install.idf_path.display(),
            if python.is_none() {
                " and no venv python was found"
            } else {
                ""
            },
            install
                .python_env
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(none found)".to_string()),
        )
    } else if !check.missing.is_empty() {
        format!(
            "{idf} runs and the chip toolchain is present, but {} tool(s) `export.sh` needs are not \
             installed: {}. A build and a flash invoke none of them — `idf_env_fix` installs them so \
             the shell environment is whole again.",
            check.missing.len(),
            check.missing.join(", "),
        )
    } else if !export_sh_works {
        format!(
            "{idf} runs (through its own venv), but `export.sh` does not activate — a shell that \
             sources it will not get a working environment."
        )
    } else {
        format!("{idf} is complete: `idf.py` runs and `export.sh` activates.")
    };

    IdfEnvReport {
        success: idf_py_runs,
        found: true,
        idf_path: Some(install.idf_path.to_string_lossy().to_string()),
        tools_path: Some(install.tools_path.to_string_lossy().to_string()),
        python_env: install
            .python_env
            .as_ref()
            .map(|path| path.to_string_lossy().to_string()),
        version,
        missing: check.missing,
        unsupported: check.unsupported,
        export_sh_works,
        idf_py_runs,
        repair_command,
        detail,
    }
}

/// `idf.py --version`, run through the venv python — the last non-empty line is the version.
async fn run_idf_version(
    install: &IdfInstall,
    python: &Path,
    env: &[(String, String)],
) -> Option<String> {
    let python = python.to_string_lossy().to_string();
    let script = install.idf_py().to_string_lossy().to_string();
    let output = run_cmd_with_env(&install.idf_path, &python, &[&script, "--version"], env)
        .await
        .ok()?;
    if !output.success {
        return None;
    }
    output
        .output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

/// `idf_tools.py check`, parsed — or an empty check when it cannot run at all.
async fn run_tool_check(
    install: &IdfInstall,
    python: &Path,
    env: &[(String, String)],
) -> ToolCheck {
    let python = python.to_string_lossy().to_string();
    let script = install.idf_tools_py().to_string_lossy().to_string();
    match run_cmd_with_env(&install.idf_path, &python, &[&script, "check"], env).await {
        Ok(output) => parse_idf_tools_check(&output.output),
        Err(_) => ToolCheck::default(),
    }
}
/// `idf_tools.py export` — the step `export.sh` itself runs — parsed for what blocks activation.
///
/// A non-zero exit is expected and is not an error here: the output names the reason, which is the
/// point. This is the half that catches a tool `check` accepts from `PATH` but `export` will not use.
async fn run_tool_export(
    install: &IdfInstall,
    python: &Path,
    env: &[(String, String)],
) -> ToolCheck {
    let python = python.to_string_lossy().to_string();
    let script = install.idf_tools_py().to_string_lossy().to_string();
    match run_cmd_with_env(&install.idf_path, &python, &[&script, "export"], env).await {
        Ok(output) => parse_idf_tools_export(&output.output),
        Err(_) => ToolCheck::default(),
    }
}

/// Whether `export.sh` activates in a subshell.
///
/// Run in its own shell, on purpose: a source that fails half-way must not leave a broken
/// environment behind it — the same reason `run-with-idf.sh` sources it in a subshell first.
async fn export_sh_activates(install: &IdfInstall) -> bool {
    let script = install.export_sh();
    if !script.is_file() {
        return false;
    }
    let command = format!(
        "set +u; . {} >/dev/null 2>&1 && idf.py --version >/dev/null 2>&1",
        shell_quote(&script)
    );
    matches!(
        run_cmd(&install.idf_path, "bash", &["-c", &command]).await,
        Ok(output) if output.success
    )
}

/// What a repair did — the tools it installed, the installer's own output, and the doctor before and
/// after, so a caller can show the difference rather than assert it.
#[derive(Debug, Clone, Serialize)]
pub struct IdfEnvRepairReport {
    pub success: bool,
    /// The tools the doctor named and the install was asked for.
    pub installed: Vec<String>,
    pub message: String,
    /// `idf_tools.py install`'s combined stdout/stderr.
    pub output: String,
    pub before: IdfEnvReport,
    /// The doctor again after installing — `None` only when there was nothing to run it against.
    pub after: Option<IdfEnvReport>,
}

/// **Fix** the environment: install exactly the tools the doctor found missing, then test again.
///
/// Only the tools `idf_tools.py check` named, never a blanket `install all`: on this machine that is
/// the three debug tools (gdb ×2, openocd) whose absence aborts `export.sh` while the build and flash
/// need none of them. Installing what is missing is the smallest change that makes the environment
/// whole. Needs network access — the tools come from `dl.espressif.com`.
pub async fn repair_idf_environment() -> IdfEnvRepairReport {
    let Some(install) = resolve_idf_install() else {
        let before = not_found_report();
        let message = before.detail.clone();
        return IdfEnvRepairReport {
            success: false,
            installed: Vec::new(),
            message,
            output: String::new(),
            before,
            after: None,
        };
    };

    let before = doctor_install(&install).await;
    if before.missing.is_empty() {
        return IdfEnvRepairReport {
            success: true,
            installed: Vec::new(),
            message: "nothing to install — the environment is complete".to_string(),
            output: String::new(),
            before,
            after: None,
        };
    }

    let Some(python) = install.python() else {
        return IdfEnvRepairReport {
            success: false,
            installed: Vec::new(),
            message: "no venv python was found, so idf_tools.py cannot be run".to_string(),
            output: String::new(),
            before,
            after: None,
        };
    };

    // `<venv>/bin/python <IDF_PATH>/tools/idf_tools.py install <tool>…` — non-interactive by nature
    // (idf_tools.py installs without prompting), with the synthesised environment so it finds the
    // tools directory and the venv it is installing into.
    let python = python.to_string_lossy().to_string();
    let mut argv: Vec<String> = vec![
        install.idf_tools_py().to_string_lossy().to_string(),
        "install".to_string(),
    ];
    argv.extend(before.missing.iter().cloned());
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let output =
        match run_cmd_with_env(&install.idf_path, &python, &argv_refs, &install.env()).await {
            Ok(out) => out.output,
            Err(error) => error,
        };

    let after = doctor_install(&install).await;
    let success = after.missing.is_empty() && after.idf_py_runs;
    let message = if after.missing.is_empty() {
        format!("installed {}", before.missing.join(", "))
    } else {
        format!("still missing after install: {}", after.missing.join(", "))
    };

    IdfEnvRepairReport {
        success,
        installed: before.missing.clone(),
        message,
        output,
        before,
        after: Some(after),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A faithful slice of `idf_tools.py check` on the machine this module was written for: the two
    /// debug tools the installer never fetched, `openocd-esp32` found only in `PATH`, and the
    /// toolchain that *is* installed. It is the shape — one `Checking tool <name>` block per tool,
    /// then an `ERROR:` summary — that the parser has to read both halves of.
    const CHECK_OUTPUT: &str = "\
Checking tool cmake
\tcmake
\t    version found in PATH: 3.30.2
Checking tool esp-clang
\tesp-clang
\t    no version found in PATH
\t    version found in PATH: unknown
\t    version installed in tools directory: 18.1.2
Checking tool esp-rom-elfs
\tesp-rom-elfs
\t    version installed in tools directory: 20230113
Checking tool ninja
\tninja
\t    version installed in tools directory: 1.12.1
Checking tool openocd-esp32
\topenocd
\t    version found in PATH: 0.12.0
Checking tool riscv32-esp-elf
\triscv32-esp-elf
\t    version installed in tools directory: 14.2.0_20241119
Checking tool riscv32-esp-elf-gdb
\triscv32-esp-elf-gdb
\t    no version found in PATH
\t    no version installed in tools directory
Checking tool xtensa-esp-elf
\txtensa-esp-elf
\t    version installed in tools directory: 14.2.0_20241119
Checking tool xtensa-esp-elf-gdb
\txtensa-esp-elf-gdb
\t    no version found in PATH
\t    no version installed in tools directory
ERROR: The following required tools were not found: xtensa-esp-elf-gdb riscv32-esp-elf-gdb
";

    /// The check names the missing tools on its `ERROR:` line, and a tool IDF finds in `PATH` is not
    /// missing — `openocd-esp32` is here, at a version IDF accepts.
    #[test]
    fn the_check_parser_reads_the_missing_tools_and_ignores_the_ones_in_path() {
        let check = parse_idf_tools_check(CHECK_OUTPUT);
        assert_eq!(
            check.missing,
            vec![
                "riscv32-esp-elf-gdb".to_string(),
                "xtensa-esp-elf-gdb".to_string()
            ],
            "exactly the two debug tools the check could not find"
        );
        assert!(
            !check.missing.iter().any(|name| name == "openocd-esp32"),
            "openocd-esp32 has a PATH version and is not missing"
        );
        assert!(check.unsupported.is_empty(), "nothing here is unsupported");
    }

    /// A tool with no version in `PATH` *or* in the tools directory is missing even when the
    /// `ERROR:` line is absent — an IDF that words its summary differently must not go unnoticed.
    #[test]
    fn the_check_parser_finds_a_tool_with_no_version_anywhere() {
        let output = "\
Checking tool xtensa-esp-elf
\txtensa-esp-elf
\t    version installed in tools directory: 14.2.0_20241119
Checking tool openocd-esp32
\topenocd
\t    no version found in PATH
";
        let check = parse_idf_tools_check(output);
        assert_eq!(check.missing, vec!["openocd-esp32".to_string()]);
    }

    /// A `PATH` version IDF refuses is reported apart from a missing tool: it exists, it is wrong.
    #[test]
    fn the_check_parser_reports_an_unsupported_path_version_separately() {
        let output = "\
Checking tool ninja
\tninja
\t    version found in PATH: 1.10.0: not supported
\t    version installed in tools directory: 1.12.1
ERROR: The following required tools were not found: 
";
        let check = parse_idf_tools_check(output);
        assert_eq!(check.unsupported, vec!["ninja".to_string()]);
        assert!(
            check.missing.is_empty(),
            "a tool found at the wrong version is not a missing tool"
        );
    }

    /// An empty output — a check that could not run at all — means nothing is known to be missing,
    /// so a repair does not install on the strength of a crash.
    #[test]
    fn the_check_parser_says_nothing_about_empty_output() {
        assert_eq!(parse_idf_tools_check(""), ToolCheck::default());
    }

    /// `idf_tools.py export` on this machine, verbatim: it refuses to activate without an *installed*
    /// `openocd-esp32` even though the check accepted the `PATH` copy, and it says which `PATH`
    /// versions it will not use. This is the half that a "complete" environment hid behind.
    const EXPORT_OUTPUT: &str = "\
Not using an unsupported version of tool esp-clang found in PATH: unknown. To use it, run '/Users/steve/.espressif/python_env/idf5.5_py3.14_env/bin/python /Users/steve/.espressif/esp-idf/v5.5.5/tools/idf_tools.py export --prefer-system'
Not using an unsupported version of tool cmake found in PATH: 4.3.1. To use it, run '/Users/steve/.espressif/python_env/idf5.5_py3.14_env/bin/python /Users/steve/.espressif/esp-idf/v5.5.5/tools/idf_tools.py export --prefer-system'
ERROR: tool openocd-esp32 has no installed versions. Please run '/Users/steve/.espressif/python_env/idf5.5_py3.14_env/bin/python /Users/steve/.espressif/esp-idf/v5.5.5/tools/idf_tools.py install' to install it.
An unsupported version of tool openocd-esp32 was found in PATH: 0.12.0.  To use it, run '/Users/steve/.espressif/python_env/idf5.5_py3.14_env/bin/python /Users/steve/.espressif/esp-idf/v5.5.5/tools/idf_tools.py export --prefer-system'
Not using an unsupported version of tool ninja found in PATH: 1.13.2. To use it, run '/Users/steve/.espressif/python_env/idf5.5_py3.14_env/bin/python /Users/steve/.espressif/esp-idf/v5.5.5/tools/idf_tools.py export --prefer-system'
";

    /// The tool `export` refuses is missing, whatever `check` thought of the copy in `PATH`.
    #[test]
    fn the_export_parser_finds_the_tool_that_blocks_activation() {
        let check = parse_idf_tools_export(EXPORT_OUTPUT);
        assert_eq!(
            check.missing,
            vec!["openocd-esp32".to_string()],
            "`export` demands an installed copy, and it is the one that aborts export.sh"
        );
    }

    /// Both wordings of the `PATH` refusal name a tool, and the tool that is *also* missing is
    /// reported in both lists — present on the machine, unusable to IDF, and not installed.
    #[test]
    fn the_export_parser_reports_the_path_versions_idf_will_not_use() {
        let check = parse_idf_tools_export(EXPORT_OUTPUT);
        assert_eq!(
            check.unsupported,
            vec![
                "esp-clang".to_string(),
                "cmake".to_string(),
                "openocd-esp32".to_string(),
                "ninja".to_string()
            ]
        );
    }

    /// The two checks are folded together without duplicating a tool, so the repair's argument list
    /// names each tool exactly once.
    #[test]
    fn the_two_checks_merge_without_repeating_a_tool() {
        let merged = merge_checks(
            parse_idf_tools_check(CHECK_OUTPUT),
            parse_idf_tools_export(EXPORT_OUTPUT),
        );
        assert_eq!(
            merged.missing,
            vec![
                "riscv32-esp-elf-gdb".to_string(),
                "xtensa-esp-elf-gdb".to_string(),
                "openocd-esp32".to_string()
            ],
            "check's findings first, then export's, each named once"
        );
        assert_eq!(
            merged.unsupported,
            vec![
                "esp-clang".to_string(),
                "cmake".to_string(),
                "openocd-esp32".to_string(),
                "ninja".to_string()
            ]
        );
    }

    /// The mini-glob walks the version directory and the tail `export.sh` names, and stops at
    /// anything absent rather than inventing a path.
    #[test]
    fn expand_walks_the_version_directory_and_the_tail() {
        let temp = TempDir::new().unwrap();
        let bin = temp
            .path()
            .join("tools/xtensa-esp-elf/14.2.0/xtensa-esp-elf/bin");
        std::fs::create_dir_all(&bin).unwrap();

        let found = expand(
            &temp.path().join("tools/xtensa-esp-elf"),
            &["*", "xtensa-esp-elf", "bin"],
        );
        assert_eq!(found, vec![bin]);

        assert!(
            expand(&temp.path().join("tools/riscv32-esp-elf"), &["*"]).is_empty(),
            "an absent tool must expand to nothing, not to its pattern"
        );
    }

    /// Build the tree an ESP-IDF install leaves behind, with `versions` of `esp-idf` and `venvs`
    /// of `python_env`, so the search runs against something real rather than a mocked `read_dir`.
    fn fake_install(versions: &[&str], venvs: &[&str]) -> TempDir {
        let temp = TempDir::new().unwrap();
        for version in versions {
            let tools = temp.path().join("esp-idf").join(version).join("tools");
            std::fs::create_dir_all(&tools).unwrap();
            std::fs::write(tools.join("idf.py"), "# idf.py\n").unwrap();
            std::fs::write(tools.join("idf_tools.py"), "# idf_tools.py\n").unwrap();
            std::fs::write(
                temp.path().join("esp-idf").join(version).join("export.sh"),
                "export IDF_PATH=...\n",
            )
            .unwrap();
        }
        for venv in venvs {
            std::fs::create_dir_all(temp.path().join("python_env").join(venv).join("bin")).unwrap();
            std::fs::write(
                temp.path()
                    .join("python_env")
                    .join(venv)
                    .join("bin")
                    .join("python"),
                "",
            )
            .unwrap();
        }
        temp
    }

    /// The venv is **listed**, not derived, and the newest install wins — the whole point of not
    /// deriving `idf5.5_py3.14_env` from `idf5.5`.
    #[test]
    fn resolution_lists_the_venv_and_prefers_the_newest_install() {
        let temp = fake_install(
            &["v5.4", "v5.5.5"],
            &["idf5.4_py3.11_env", "idf5.5_py3.14_env"],
        );
        let install = resolve_idf_install_with(
            None,
            Some(temp.path().to_string_lossy().to_string()),
            None,
            None,
            None,
        )
        .expect("a temp tree with a tools/idf.py install must resolve");

        assert_eq!(install.idf_path, temp.path().join("esp-idf").join("v5.5.5"));
        assert_eq!(
            install.python_env,
            Some(temp.path().join("python_env").join("idf5.5_py3.14_env")),
            "the venv must be found by listing, so its name cannot drift from a guess"
        );
        assert!(
            install.python().is_some(),
            "the listed venv has an interpreter"
        );
    }

    /// An explicit `IDF_PATH` that is not an install is skipped, not trusted: the search falls
    /// through to a real one rather than reporting a broken environment with a plausible name.
    #[test]
    fn resolution_skips_an_idf_path_that_is_not_an_install() {
        let temp = fake_install(&["v5.5.5"], &[]);
        let not_an_install = temp.path().join("somewhere-else");
        std::fs::create_dir_all(&not_an_install).unwrap();

        let install = resolve_idf_install_with(
            Some(not_an_install.to_string_lossy().to_string()),
            Some(temp.path().to_string_lossy().to_string()),
            None,
            None,
            None,
        )
        .expect("the real install under the tools path must still be found");

        assert_eq!(install.idf_path, temp.path().join("esp-idf").join("v5.5.5"));
    }

    /// `SPIRE_IDF_EXPORT` names an `export.sh`, and its directory is the install — so a person who
    /// points at one gets *that* install even when a newer one sits beside it.
    #[test]
    fn an_export_hint_selects_its_own_install() {
        let temp = fake_install(&["v5.4", "v5.5.5"], &[]);
        let hint = temp.path().join("esp-idf").join("v5.4").join("export.sh");

        let install = resolve_idf_install_with(
            None,
            Some(temp.path().to_string_lossy().to_string()),
            None,
            Some(hint.to_string_lossy().to_string()),
            None,
        )
        .expect("the hinted install must resolve");

        assert_eq!(install.idf_path, temp.path().join("esp-idf").join("v5.4"));
    }

    /// No install anywhere is `None` — the caller then says where it looked instead of pretending.
    #[test]
    fn resolution_without_an_install_is_none() {
        let temp = TempDir::new().unwrap();
        assert!(resolve_idf_install_with(
            None,
            Some(temp.path().to_string_lossy().to_string()),
            None,
            None,
            None,
        )
        .is_none());
    }

    /// The environment a build is handed names every path IDF needs, and the toolchain comes
    /// **first** on `PATH` — the same environment `run-with-idf.sh` synthesises by hand.
    #[test]
    fn the_synthesised_environment_names_the_paths_a_build_needs() {
        let temp = fake_install(&["v5.5.5"], &["idf5.5_py3.14_env"]);
        let bin = temp
            .path()
            .join("tools/xtensa-esp-elf/14.2.0/xtensa-esp-elf/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let rom = temp.path().join("tools/esp-rom-elfs/20230113");
        std::fs::create_dir_all(&rom).unwrap();

        let install = resolve_idf_install_with(
            None,
            Some(temp.path().to_string_lossy().to_string()),
            None,
            None,
            None,
        )
        .unwrap();
        let idf_path = install.idf_path.to_string_lossy().to_string();
        let tools_path = temp.path().to_string_lossy().to_string();
        let env: std::collections::HashMap<String, String> = install.env().into_iter().collect();

        assert_eq!(
            env.get("IDF_PATH").map(String::as_str),
            Some(idf_path.as_str())
        );
        assert_eq!(
            env.get("IDF_TOOLS_PATH").map(String::as_str),
            Some(tools_path.as_str())
        );
        assert!(
            env.contains_key("IDF_PYTHON_ENV_PATH"),
            "the venv is exported"
        );
        assert_eq!(
            env.get("ESP_ROM_ELF_DIR").map(String::as_str),
            Some(rom.to_string_lossy().as_ref())
        );
        let path = env.get("PATH").expect("the toolchain goes on PATH");
        assert!(
            path.starts_with(&bin.to_string_lossy().to_string()),
            "the cross-compiler must win over any system tool: {path}"
        );
    }

    /// The plan's entries win because they come **last**: `IDF_TARGET` is the platform's fact, and a
    /// machine that happens to have one exported must not be able to move it. One value per key, and
    /// the install's additions are still all there.
    #[test]
    fn merging_the_environment_leaves_the_plan_in_charge() {
        let temp = fake_install(&["v5.5.5"], &["idf5.5_py3.14_env"]);
        let install = resolve_idf_install_with(
            None,
            Some(temp.path().to_string_lossy().to_string()),
            None,
            None,
            None,
        )
        .unwrap();
        let plan = vec![
            ("IDF_TARGET".to_string(), "esp32s3".to_string()),
            // Something the install also states, so precedence is what is being tested rather than
            // two disjoint key sets that could never disagree.
            ("IDF_PATH".to_string(), "/the/plan/says".to_string()),
        ];
        let merged = install.merge_env(&plan);
        let by_key: std::collections::HashMap<String, String> = merged.iter().cloned().collect();

        assert_eq!(
            by_key.get("IDF_TARGET").map(String::as_str),
            Some("esp32s3")
        );
        assert_eq!(
            by_key.get("IDF_PATH").map(String::as_str),
            Some("/the/plan/says"),
            "the plan's value, not the install's: {merged:?}"
        );
        for key in ["IDF_TARGET", "IDF_PATH"] {
            assert_eq!(
                merged.iter().filter(|(k, _)| k == key).count(),
                1,
                "{key} is stated exactly once: {merged:?}"
            );
        }
        assert!(
            by_key.contains_key("IDF_TOOLS_PATH") && by_key.contains_key("PATH"),
            "the install's own additions survive the merge: {merged:?}"
        );
    }

    /// An environment is **exported** only when both halves are there: this install's `IDF_PATH`
    /// *and* its venv already on `PATH`. Half an environment is not something a build may stand on —
    /// that hand-built case is what this module exists to fill in.
    #[test]
    fn an_exported_environment_needs_both_halves() {
        let temp = fake_install(&["v5.5.5"], &["idf5.5_py3.14_env"]);
        let install = resolve_idf_install_with(
            None,
            Some(temp.path().to_string_lossy().to_string()),
            None,
            None,
            None,
        )
        .unwrap();
        let idf_path = install.idf_path.to_string_lossy().to_string();
        let venv_bin = install
            .python()
            .expect("the fake venv has an interpreter")
            .parent()
            .expect("the interpreter is in a directory")
            .to_string_lossy()
            .to_string();
        let path = format!("/usr/bin:{venv_bin}");

        assert!(
            is_exported_for(&install, Some(&idf_path), Some(&path)),
            "`export.sh` arranges exactly this: IDF_PATH and the venv on PATH"
        );
        assert!(
            !is_exported_for(&install, None, Some(&path)),
            "no IDF_PATH means nothing was exported"
        );
        assert!(
            !is_exported_for(&install, Some(&idf_path), Some("/usr/bin")),
            "IDF_PATH without the venv is the half-built environment a build cannot use"
        );
        assert!(
            !is_exported_for(&install, Some("/somewhere/else"), Some(&path)),
            "another install's IDF_PATH is not this one"
        );
        assert!(
            !is_exported_for(&install, Some(&idf_path), Some(&format!("{venv_bin}-old"))),
            "a near-miss directory is not the venv"
        );
    }

    /// The live check: this machine's own ESP-IDF, asked the same questions a user would.
    ///
    /// Ignored by default because it needs an install and a venv to be present — it is the test to
    /// run by hand when the environment is what is in question.
    #[tokio::test]
    #[ignore = "requires a real ESP-IDF install (~/.espressif)"]
    async fn the_live_environment_is_found_and_tested() {
        let report = doctor_idf_environment().await;
        eprintln!("{report:#?}");
        assert!(report.found, "no ESP-IDF found: {}", report.detail);
        assert!(report.idf_py_runs, "idf.py does not run: {}", report.detail);
    }

    /// The live **repair** — the whole point of the module, run against the real `~/.espressif`.
    ///
    /// Ignored by default because it downloads from `dl.espressif.com` and writes into the tools
    /// directory. The assertion is the user-facing outcome, not the installer's exit code: after the
    /// repair `export.sh` must **activate**, which is exactly what its missing debug tools stopped it
    /// from doing.
    #[tokio::test]
    #[ignore = "downloads ESP-IDF tools into ~/.espressif"]
    async fn the_live_repair_makes_the_environment_exportable() {
        let repair = repair_idf_environment().await;
        eprintln!("{repair:#?}");
        assert!(repair.success, "repair failed: {}", repair.message);
        let after = repair.after.expect("a repair reports the doctor after it");
        assert!(
            after.export_sh_works,
            "`export.sh` still does not activate after installing {}: {}",
            repair.installed.join(", "),
            after.detail
        );
    }
}
