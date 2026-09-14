// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Autonomous compile → fix → recompile loop behind the UI's **Fix & Verify**.
//!
//! C/C++ has no reliable toolchain auto-fixer (`clang-tidy` is not part of the
//! cross toolchains), so a fix is an LLM whole-file rewrite. That makes
//! unattended application only acceptable if it is *verified* and *reversible*,
//! which is exactly what this module guarantees:
//!
//! * only **compile errors** are fixed — warnings are reported, never rewritten;
//! * every write is followed by a **rebuild**, and a file whose error count did
//!   not go down is **restored byte-for-byte** to its pre-run content;
//! * each file is tried at most once (no oscillation), the run is capped, and the
//!   result is an honest report of what remains.
//!
//! The loop is deliberately actor-free: it talks to an [`AutofixDriver`], so it
//! can be tested exhaustively with a fake driver (see the tests below) while the
//! coordinator plugs in the real LLM + build actors.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::modify::{run_modify_loop, ChangeTarget, ModifyDriver, Observation};

/// Diagnostic lines currently recorded for a file, keyed by that file.
pub type ErrorsByFile = BTreeMap<String, Vec<String>>;

/// A raw diagnostic as recorded in the project graph.
pub struct RawDiagnostic {
    pub build_type: String,
    pub severity: String,
    pub file: String,
    pub message: String,
}

/// Group the loop's input: **only compile errors from the build qualify**.
///
/// The graph also holds diagnostics from other kinds (lint, fix, analyze). Those
/// are not compile errors and must never be treated as fixable source problems:
/// a stale lint pass had left hundreds of `file not found` entries for other
/// platforms' sources, which made "Fix & Verify" claim errors on files the
/// current build never compiles. Build diagnostics are superseded on every build,
/// so this set is always the current platform's real errors.
pub fn build_errors_from(diags: &[RawDiagnostic]) -> ErrorsByFile {
    let mut out = ErrorsByFile::new();
    for d in diags {
        if d.build_type != "build" || d.severity != "error" {
            continue;
        }
        let message = d.message.trim();
        if d.file.is_empty() || message.is_empty() {
            continue;
        }
        out.entry(d.file.to_string())
            .or_default()
            .push(message.to_string());
    }
    out
}

/// Warning lines grouped by file (same shape as [`ErrorsByFile`]).
pub type WarningsByFile = ErrorsByFile;

/// Warning tags that are mechanically safe to fix automatically.
///
/// Only dead stores / dead initialisations / unused values / self-assignments
/// qualify: removing them cannot change behaviour. Everything else the tools
/// report (`core.*` analyser findings such as null dereferences, `-Wsign-compare`,
/// `-Wformat`, …) needs human judgement, so those warnings are REPORTED and never
/// rewritten.
pub const SAFE_WARNING_TAGS: &[&str] = &[
    "deadcode.DeadStores",
    "deadcode.DeadInitialization",
    "-Wunused-variable",
    "-Wunused-but-set-variable",
    "-Wunused-parameter",
    "-Wunused-value",
    "-Wunused-local-typedef",
    "-Wself-assign",
];

/// True when a warning can be fixed without changing behaviour.
pub fn warning_is_safe(message: &str) -> bool {
    SAFE_WARNING_TAGS.iter().any(|tag| message.contains(tag))
}

/// Warning diagnostics grouped by file (both compiler `-W…` warnings and analyser
/// findings are recorded with `severity = "warning"`).
pub fn warnings_from(diags: &[RawDiagnostic]) -> WarningsByFile {
    let mut out = WarningsByFile::new();
    for d in diags {
        if d.severity != "warning" {
            continue;
        }
        let message = d.message.trim();
        if d.file.is_empty() || message.is_empty() {
            continue;
        }
        out.entry(d.file.to_string())
            .or_default()
            .push(message.to_string());
    }
    out
}

/// The subset of `warnings` that may be fixed automatically.
pub fn safe_warnings(warnings: &WarningsByFile) -> WarningsByFile {
    warnings
        .iter()
        .filter_map(|(file, lines)| {
            let safe: Vec<String> = lines
                .iter()
                .filter(|line| warning_is_safe(line))
                .cloned()
                .collect();
            if safe.is_empty() {
                None
            } else {
                Some((file.clone(), safe))
            }
        })
        .collect()
}

/// The subset that must be left to a human (reported, never rewritten).
pub fn held_warnings(warnings: &WarningsByFile) -> WarningsByFile {
    warnings
        .iter()
        .filter_map(|(file, lines)| {
            let held: Vec<String> = lines
                .iter()
                .filter(|line| !warning_is_safe(line))
                .cloned()
                .collect();
            if held.is_empty() {
                None
            } else {
                Some((file.clone(), held))
            }
        })
        .collect()
}

/// Flatten a per-file map into readable `file: line` entries.
fn flatten(warnings: &WarningsByFile) -> Vec<String> {
    warnings
        .iter()
        .flat_map(|(file, lines)| lines.iter().map(move |l| format!("{file}: {l}")))
        .collect()
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutofixReport {
    /// True when the project compiled with zero errors at the end.
    pub success: bool,
    /// Rounds that actually applied at least one fix.
    pub rounds: usize,
    /// Files whose error count went down and stayed down.
    pub files_fixed: Vec<String>,
    /// Files whose fix failed to reduce the errors, and were rolled back.
    pub files_reverted: Vec<String>,
    /// Files the loop could not attempt (no proposal / unreadable / unwritable).
    pub files_skipped: Vec<String>,
    pub errors_before: usize,
    pub errors_after: usize,
    /// Analyzer warnings remaining after the final lint pass.
    pub warnings_after: usize,
    /// Rounds spent on the safe-warning phase.
    pub warning_rounds: usize,
    /// Files whose automatic warning fix was kept.
    pub warnings_fixed: Vec<String>,
    /// Files whose warning fix was rolled back.
    pub warnings_reverted: Vec<String>,
    /// Safely-fixable warnings present when the warning phase started.
    pub safe_warnings_before: usize,
    /// Safely-fixable warnings still present at the end.
    pub safe_warnings_after: usize,
    /// Warnings deliberately left for review (not safe to auto-fix).
    pub warnings_held: Vec<String>,
    /// Built executable, when the final build succeeded and it can be named.
    pub artifact: Option<String>,
    /// Platform the run was verified against; None when it could not be resolved
    /// (the build then used whichever directory discovery returned).
    pub platform: Option<String>,
    /// Human-readable trace of what the loop did, line by line.
    pub log: Vec<String>,
}

impl AutofixReport {
    /// Short, human summary for the UI's result pane.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        out.push_str("Fix & Verify: build → fix errors → lint → fix warnings → verify\n");
        out.push_str(&format!(
            "errors:   {} → {}\n",
            self.errors_before, self.errors_after
        ));
        out.push_str(&format!(
            "warnings: {} safely-fixable → {} · {} left for review\n",
            self.safe_warnings_before,
            self.safe_warnings_after,
            self.warnings_held.len()
        ));
        out.push_str(&format!(
            "rounds:   {} error · {} warning\n",
            self.rounds, self.warning_rounds
        ));
        out.push_str(&match &self.platform {
            Some(platform) => format!("platform: {platform}\n"),
            None => "platform: none selected — the build used the discovered build directory\n"
                .to_string(),
        });
        match &self.artifact {
            Some(artifact) => out.push_str(&format!("built:    {artifact}\n")),
            None => out.push_str("built:    no artifact (the build did not succeed)\n"),
        }
        if !self.files_fixed.is_empty() {
            out.push_str(&format!(
                "error fixes kept:\n  {}\n",
                self.files_fixed.join("\n  ")
            ));
        }
        if !self.files_reverted.is_empty() {
            out.push_str(&format!(
                "error fixes reverted (no net gain):\n  {}\n",
                self.files_reverted.join("\n  ")
            ));
        }
        if !self.warnings_fixed.is_empty() {
            out.push_str(&format!(
                "warning fixes kept:\n  {}\n",
                self.warnings_fixed.join("\n  ")
            ));
        }
        if !self.warnings_reverted.is_empty() {
            out.push_str(&format!(
                "warning fixes reverted:\n  {}\n",
                self.warnings_reverted.join("\n  ")
            ));
        }
        if !self.warnings_held.is_empty() {
            out.push_str("warnings left for review (not safe to auto-fix):\n");
            for entry in &self.warnings_held {
                out.push_str(&format!("  {entry}\n"));
            }
        }
        if self.rounds == 0 && self.errors_before > 0 && !self.files_skipped.is_empty() {
            out.push_str(&format!(
                "nothing was written: {} file(s) had no usable rewrite (see the trace below).\n",
                self.files_skipped.len()
            ));
        }
        if !self.log.is_empty() {
            out.push_str("--- trace ---\n");
            out.push_str(&self.log.join("\n"));
            out.push('\n');
        }
        if !self.success {
            out.push_str(&format!(
                "Result: {} error(s) still reported — see the Build tab for the files that need attention.",
                self.errors_after
            ));
        } else if self.safe_warnings_after == 0 && self.warnings_held.is_empty() {
            out.push_str("Result: clean — no errors, no warnings.");
        } else {
            out.push_str(&format!(
                "Result: compiles cleanly. {} warning(s) left for review.",
                self.warnings_held.len()
            ));
        }
        out
    }
}

/// What the loop needs from its host: the coordinator implements this with the
/// real actors, tests implement it with canned answers.
#[async_trait::async_trait]
pub trait AutofixDriver: Send + Sync {
    /// Error diagnostics currently recorded for the project, grouped by file.
    async fn errors(&self) -> ErrorsByFile;
    /// A complete replacement for the file at `path` (None when the model has
    /// nothing). `file` is the raw diagnostic key the errors are recorded under,
    /// which may be relative to the build directory while `path` is resolved.
    async fn propose(&self, file: &str, path: &Path) -> Option<String>;
    /// Recompile, then return the freshly recorded errors.
    async fn rebuild(&self) -> ErrorsByFile;
    /// Run the linter once, returning the number of warnings it reports.
    async fn lint(&self) -> usize;
    /// Warning diagnostics currently recorded for the project, grouped by file
    /// (fresh after [`Self::lint`]).
    async fn warnings(&self) -> WarningsByFile;
    /// A replacement for `file` aimed at clearing `warnings` (None when the model
    /// has nothing usable). Only called for warnings classified as safe.
    async fn propose_warning_fix(
        &self,
        file: &str,
        path: &Path,
        warnings: &[String],
    ) -> Option<String>;
}

/// Resolve a diagnostic's file path to something writable.
///
/// Compilers report paths relative to the directory the build ran in (e.g.
/// `../app/main.cpp` for Meson), so each candidate base is tried in turn.
pub fn resolve_source_path(file: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    let direct = Path::new(file);
    if direct.is_file() {
        return Some(direct.to_path_buf());
    }
    for base in bases {
        let candidate = base.join(file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// A fix is rolled back unless the file's error count strictly went down.
fn should_revert(before: usize, after: usize) -> bool {
    after >= before
}

fn has_build_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.path().is_dir() && e.file_name().to_string_lossy().starts_with("build"))
        })
        .unwrap_or(false)
}

/// Nearest ancestor of `path` that owns the build: it holds a `build*`
/// directory, or a project marker (`.git` / `.spire`). Falls back to `path`.
///
/// The UI hands Build/Lint/Verify (and Fix & Verify) the *selected subproject's*
/// absolute path, while the compile database and build directories live at the
/// project root.
pub fn find_project_root(path: &Path) -> PathBuf {
    let original = path.to_path_buf();
    let mut dir = path.to_path_buf();
    loop {
        if has_build_dir(&dir) || dir.join(".git").exists() || dir.join(".spire").exists() {
            return dir;
        }
        if !dir.pop() {
            return original;
        }
    }
}

/// Directories a relative diagnostic path may be rooted at: the project root
/// plus every `build*` directory beneath it (compilers report paths relative to
/// the directory the build ran in, e.g. `../app/main.cpp` from build-rpi5).
pub fn diagnostic_bases(project_root: &Path) -> Vec<PathBuf> {
    let mut bases = vec![project_root.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(project_root) {
        let mut builds: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .map(|n| n.to_string_lossy().starts_with("build"))
                        .unwrap_or(false)
            })
            .collect();
        builds.sort();
        bases.extend(builds);
    }
    bases
}

fn total(errors: &ErrorsByFile) -> usize {
    errors.values().map(|v| v.len()).sum()
}

/// Platform ids that have a `build-<id>` directory at the project root.
///
/// These are the platforms a run can be *verified against* — `build-a7s`,
/// `build-rpi5`, … (`build`, `builddir` and `build-native` are not platforms, so
/// only names of the form `build-<id>` count).
pub fn platform_build_dirs(project_root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(project_root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    name.strip_prefix("build-")
                        .filter(|id| !id.is_empty())
                        .map(|id| id.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Best-effort platform for a run that did not name one.
///
/// The action rail has no platform context, so a run can arrive with no platform
/// at all — and `build` then compiles whichever `build*` directory discovery
/// returns first, which may be an entirely different target. Prefer the platform
/// encoded in the build target name (`ai-trap-rpi5` → `rpi5`), then the only
/// build directory, and otherwise admit we cannot tell (None) rather than guess.
pub fn derive_platform(project_root: &Path, target: Option<&str>) -> Option<String> {
    let platforms = platform_build_dirs(project_root);
    if let Some(target) = target {
        if let Some(hit) = platforms.iter().find(|p| target.ends_with(p.as_str())) {
            return Some(hit.clone());
        }
    }
    if platforms.len() == 1 {
        return platforms.into_iter().next();
    }
    None
}

/// Decide which platform this run must verify against, or explain why we cannot.
///
/// An explicit selection always wins. Otherwise we try to derive one (target name,
/// or a single build directory). With several candidate builds and nothing to
/// choose between them we REFUSE rather than guess: `build` would otherwise
/// compile whichever `build*` directory discovery returns first — a different
/// target's sources — and the loop would try to "fix" that target instead.
pub fn resolve_platform(
    project_root: &Path,
    requested: Option<&str>,
    target: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(platform) = requested.filter(|p| !p.is_empty()) {
        return Ok(Some(platform.to_string()));
    }
    if let Some(platform) = derive_platform(project_root, target) {
        return Ok(Some(platform));
    }
    let platforms = platform_build_dirs(project_root);
    if platforms.len() > 1 {
        return Err(format!(
            "no platform selected — Fix & Verify must compile and verify one target. \
             Select a build target or platform first (available: {}). Nothing was changed.",
            platforms.join(", ")
        ));
    }
    // No configured builds (or exactly one): discovery is unambiguous enough.
    Ok(None)
}

/// Which of autofix's two passes the loop is running. The spine is the same either
/// way; only what counts as a target, what a proposal is written from, and what
/// "the project got worse" means differ.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Compile errors reported by the build.
    Errors,
    /// Safely-fixable analyzer warnings, after a clean compile.
    Warnings,
}

/// What a verification observed: the errors the project has, plus the warnings the
/// linter reported. `warnings` is only measured during the warning phase; the
/// safely-fixable subset is derived from it with `safe_warnings` where it is read.
#[derive(Clone, Default)]
struct AutofixObs {
    errors: ErrorsByFile,
    warnings: WarningsByFile,
}

impl Observation for AutofixObs {
    fn is_clean(&self) -> bool {
        // "Clean" means the project compiles. Warnings are a separate concern, and
        // `AutofixReport::success` has always meant errors only.
        total(&self.errors) == 0
    }
}

/// Expose autofix's error phase to the general modify spine.
///
/// The spine owns the control flow — rounds, the tried set, rolling back a change
/// that earned nothing, and rejecting a round before any per-file verdict — while
/// everything autofix-specific stays here: the report, the log wording, the byte
/// backups, and the two acceptance rules.
///
/// ## The rebuild cadence is part of the contract
///
/// [`AutofixDriver::errors`] *reads* the diagnostics already recorded, while
/// `rebuild` recompiles. The spine verifies once before a round's writes and once
/// after, so this dispatches on exactly that: read before, rebuild after, and also
/// rebuild whenever a rollback has left the recorded diagnostics describing a file
/// that is no longer on disk. That keeps the number of compiles identical to the
/// hand-written loop this replaced — which is what lets the driver's existing tests
/// stand as the migration's regression net.
struct AutofixAdapter<'a> {
    driver: &'a dyn AutofixDriver,
    bases: &'a [PathBuf],
    /// Which pass this is: compile errors, or safely-fixable warnings.
    phase: Phase,
    /// The safely-fixable warning count when the warning phase began. The round gate
    /// compares against this baseline rather than against the round, so a round that
    /// fails to improve on an earlier win is rolled back.
    safe_before: usize,
    /// Built as the loop runs and handed back at the end. The log lines are
    /// autofix's, so the UI and the tests keep reading what they always read.
    report: Mutex<AutofixReport>,
    /// The ORIGINAL bytes of each file, taken once, so a rollback always restores the
    /// state the project had before this run started.
    backups: Mutex<BTreeMap<String, String>>,
    /// What the loop is working from, and where `targets` comes from.
    current: Mutex<AutofixObs>,
    /// The observation at the start of the round, so a rollback can explain itself in
    /// the counts a user can see.
    round_start: Mutex<AutofixObs>,
    /// Writes since the previous verification.
    wrote_since_verify: Mutex<usize>,
    /// Files written during the round that just ended, for the rejection log.
    last_round_writes: Mutex<usize>,
    /// Set by a rollback: the recorded diagnostics no longer describe the disk.
    needs_rebuild: Mutex<bool>,
    /// Rounds that changed something — what `AutofixReport::rounds` means.
    rounds: Mutex<usize>,
}

impl<'a> AutofixAdapter<'a> {
    /// The error phase. The build's recorded errors seed the loop, so it knows what
    /// to work on before its first verification.
    fn errors(
        driver: &'a dyn AutofixDriver,
        bases: &'a [PathBuf],
        errors: ErrorsByFile,
        report: AutofixReport,
    ) -> Self {
        let obs = AutofixObs {
            errors,
            warnings: WarningsByFile::new(),
        };
        Self::build(
            driver,
            bases,
            Phase::Errors,
            0,
            obs,
            report,
            BTreeMap::new(),
        )
    }

    /// The warning phase. The first lint seeds the loop, and its safely-fixable count
    /// becomes the baseline the round gate is measured against. `backups` are carried
    /// over from the error phase, so a warning fix still rolls back to the bytes the
    /// project had before the run started.
    fn warnings(
        driver: &'a dyn AutofixDriver,
        bases: &'a [PathBuf],
        warnings: WarningsByFile,
        report: AutofixReport,
        backups: BTreeMap<String, String>,
    ) -> Self {
        let safe_before = total(&safe_warnings(&warnings));
        let obs = AutofixObs {
            errors: ErrorsByFile::new(),
            warnings,
        };
        Self::build(
            driver,
            bases,
            Phase::Warnings,
            safe_before,
            obs,
            report,
            backups,
        )
    }

    fn build(
        driver: &'a dyn AutofixDriver,
        bases: &'a [PathBuf],
        phase: Phase,
        safe_before: usize,
        obs: AutofixObs,
        report: AutofixReport,
        backups: BTreeMap<String, String>,
    ) -> Self {
        Self {
            driver,
            bases,
            phase,
            safe_before,
            report: Mutex::new(report),
            backups: Mutex::new(backups),
            current: Mutex::new(obs.clone()),
            round_start: Mutex::new(obs),
            wrote_since_verify: Mutex::new(0),
            last_round_writes: Mutex::new(0),
            needs_rebuild: Mutex::new(false),
            rounds: Mutex::new(0),
        }
    }

    fn log(&self, line: String) {
        self.report.lock().unwrap().log.push(line);
    }

    /// Record that a file could not be attempted, with autofix's wording.
    fn skip(&self, file: &str, why: String) {
        self.log(format!("skip {file}: {why}"));
        let mut report = self.report.lock().unwrap();
        if !report.files_skipped.iter().any(|f| f == file) {
            report.files_skipped.push(file.to_string());
        }
    }

    /// What `run_autofix` continues with: the report, the backups (the warning phase
    /// reuses them so a warning fix can still be rolled back), and the final
    /// observation.
    fn into_parts(self) -> (AutofixReport, BTreeMap<String, String>, AutofixObs) {
        let mut report = self.report.into_inner().unwrap();
        report.rounds = self.rounds.into_inner().unwrap();
        let backups = self.backups.into_inner().unwrap();
        let obs = self.current.into_inner().unwrap();
        (report, backups, obs)
    }
}

impl ModifyDriver for AutofixAdapter<'_> {
    type Obs = AutofixObs;

    fn targets(&self) -> Vec<ChangeTarget> {
        // The warning phase carries the lines to fix in the context, because that is
        // what the model is asked to work from.
        let files: Vec<(String, Vec<String>)> = {
            let current = self.current.lock().unwrap();
            match self.phase {
                Phase::Errors => current
                    .errors
                    .keys()
                    .cloned()
                    .map(|file| (file, Vec::new()))
                    .collect(),
                Phase::Warnings => safe_warnings(&current.warnings).into_iter().collect(),
            }
        };
        files
            .into_iter()
            .map(|(file, lines)| {
                let path =
                    resolve_source_path(&file, self.bases).unwrap_or_else(|| PathBuf::from(&file));
                ChangeTarget::new(file, path).with_context(lines)
            })
            .collect()
    }

    async fn propose(&self, target: &ChangeTarget) -> Option<String> {
        let file = &target.id;
        let Some(path) = resolve_source_path(file, self.bases) else {
            self.skip(file, "not found on disk".to_string());
            return None;
        };
        // Back up the ORIGINAL bytes once, before the model can touch them.
        {
            let mut backups = self.backups.lock().unwrap();
            if !backups.contains_key(file) {
                match std::fs::read_to_string(&path) {
                    Ok(orig) => {
                        backups.insert(file.clone(), orig);
                    }
                    Err(e) => {
                        drop(backups);
                        self.skip(file, format!("cannot read ({e})"));
                        return None;
                    }
                }
            }
        }
        let proposal = match self.phase {
            Phase::Errors => self.driver.propose(file, &path).await,
            Phase::Warnings => {
                self.driver
                    .propose_warning_fix(file, &path, &target.context)
                    .await
            }
        };
        match proposal {
            None => {
                let what = match self.phase {
                    Phase::Errors => "no fix proposed",
                    Phase::Warnings => "no warning fix proposed",
                };
                self.skip(file, what.to_string());
                None
            }
            Some(content) if content.trim().is_empty() => {
                self.skip(file, "empty proposal".to_string());
                None
            }
            Some(content) => Some(content),
        }
    }

    async fn verify(&self) -> AutofixObs {
        let wrote = {
            let mut w = self.wrote_since_verify.lock().unwrap();
            let n = *w;
            *w = 0;
            n
        };
        let stale = {
            let mut s = self.needs_rebuild.lock().unwrap();
            let v = *s;
            *s = false;
            v
        };
        if wrote > 0 {
            *self.rounds.lock().unwrap() += 1;
            *self.last_round_writes.lock().unwrap() = wrote;
        }
        if wrote > 0 || stale {
            // Just wrote, or a rollback invalidated the record: re-measure.
            let fresh = match self.phase {
                Phase::Errors => AutofixObs {
                    errors: self.driver.rebuild().await,
                    warnings: WarningsByFile::new(),
                },
                // The warning phase's re-measure is what the round gate reads: rebuild
                // (errors must stay at zero), re-lint, re-read.
                Phase::Warnings => {
                    let errors = self.driver.rebuild().await;
                    let _ = self.driver.lint().await;
                    AutofixObs {
                        errors,
                        warnings: self.driver.warnings().await,
                    }
                }
            };
            *self.current.lock().unwrap() = fresh.clone();
            if wrote == 0 {
                *self.round_start.lock().unwrap() = fresh.clone();
            }
            return fresh;
        }
        // A read of what is already recorded — nothing was written since, so the
        // diagnostics still describe the disk.
        let current = self.current.lock().unwrap().clone();
        if wrote == 0 {
            *self.round_start.lock().unwrap() = current.clone();
        }
        current
    }

    fn accept(&self, target: &ChangeTarget, before: &AutofixObs, after: &AutofixObs) -> bool {
        match self.phase {
            Phase::Errors => {
                let b = before.errors.get(&target.id).map(|v| v.len()).unwrap_or(0);
                let a = after.errors.get(&target.id).map(|v| v.len()).unwrap_or(0);
                if should_revert(b, a) {
                    return false;
                }
                let mut report = self.report.lock().unwrap();
                if !report.files_fixed.iter().any(|f| f == &target.id) {
                    report.files_fixed.push(target.id.clone());
                }
                true
            }
            Phase::Warnings => {
                // The warning gate is per ROUND, not per file (see `reject_round`), so
                // once the round survives, every file it wrote is kept.
                let mut report = self.report.lock().unwrap();
                if !report.warnings_fixed.iter().any(|f| f == &target.id) {
                    report.warnings_fixed.push(target.id.clone());
                }
                true
            }
        }
    }

    fn reject_round(&self, before: &AutofixObs, after: &AutofixObs) -> bool {
        let writes = *self.last_round_writes.lock().unwrap();
        let round = *self.rounds.lock().unwrap();
        match self.phase {
            Phase::Errors => {
                let (before_total, after_total) = (total(&before.errors), total(&after.errors));
                if after_total <= before_total {
                    return false;
                }
                // A rewrite can reduce its own file's errors while breaking something
                // else (a shared header, say), so the project is judged as a whole
                // before any per-file verdict is trusted.
                self.log(format!(
                    "round {round} made the project worse ({before_total} → {after_total} errors); rolled back {writes} file(s)"
                ));
                true
            }
            Phase::Warnings => {
                let errors_after = total(&after.errors);
                let safe_after = total(&safe_warnings(&after.warnings));
                // Mirrors the error gate: no error may appear AND the safely-fixable
                // count must actually fall below the level the phase started at.
                if errors_after > 0 || safe_after >= self.safe_before {
                    self.log(format!(
                        "warning round {round} rolled back ({} → {} safe warning(s), {} error(s))",
                        self.safe_before, safe_after, errors_after
                    ));
                    return true;
                }
                // Emitted here, not in `accept`: this is the one line per round, and
                // `accept` runs once per file.
                self.log(format!(
                    "warning round {round} kept: {} → {} safe warning(s)",
                    self.safe_before, safe_after
                ));
                false
            }
        }
    }

    async fn apply(&self, target: &ChangeTarget, content: &str) -> Result<(), String> {
        let Some(path) = resolve_source_path(&target.id, self.bases) else {
            self.skip(&target.id, "not found on disk".to_string());
            return Err("not found on disk".to_string());
        };
        if let Err(e) = std::fs::write(&path, content) {
            self.skip(&target.id, format!("write failed ({e})"));
            return Err(format!("write failed ({e})"));
        }
        match self.phase {
            Phase::Errors => {
                let before = self
                    .round_start
                    .lock()
                    .unwrap()
                    .errors
                    .get(&target.id)
                    .map(|v| v.len())
                    .unwrap_or(0);
                self.log(format!(
                    "applied fix to {} ({before} error(s) before)",
                    target.id
                ));
            }
            Phase::Warnings => {
                // The number of warnings the fix was asked to clear, carried on the
                // target because that is what the model was given.
                self.log(format!(
                    "applied warning fix to {} ({} warning(s) reported)",
                    target.id,
                    target.context.len()
                ));
            }
        }
        *self.wrote_since_verify.lock().unwrap() += 1;
        Ok(())
    }

    async fn revert(&self, target: &ChangeTarget) -> Result<(), String> {
        if let (Some(orig), Some(path)) = (
            self.backups.lock().unwrap().get(&target.id).cloned(),
            resolve_source_path(&target.id, self.bases),
        ) {
            let _ = std::fs::write(&path, orig);
        }
        match self.phase {
            Phase::Errors => {
                let (before, now) = {
                    let start = self.round_start.lock().unwrap();
                    let current = self.current.lock().unwrap();
                    (
                        start.errors.get(&target.id).map(|v| v.len()).unwrap_or(0),
                        current.errors.get(&target.id).map(|v| v.len()).unwrap_or(0),
                    )
                };
                self.log(format!("reverted {}: errors {before} → {now}", target.id));
                let mut report = self.report.lock().unwrap();
                if !report.files_reverted.iter().any(|f| f == &target.id) {
                    report.files_reverted.push(target.id.clone());
                }
            }
            Phase::Warnings => {
                // No per-file line here: the round-level message explains the rollback.
                let mut report = self.report.lock().unwrap();
                if !report.warnings_reverted.iter().any(|f| f == &target.id) {
                    report.warnings_reverted.push(target.id.clone());
                }
            }
        }
        // The file is back to its original bytes, so the recorded diagnostics no longer
        // describe it: the next verification must re-measure.
        *self.needs_rebuild.lock().unwrap() = true;
        Ok(())
    }
}

/// Run the autonomous loop: apply a fix per error file, rebuild, keep what
/// helped and roll back what did not.
///
/// `bases` are the directories a relative diagnostic path may be rooted at
/// (project root plus its `build*` directories). `max_rounds` bounds the work;
/// each file is attempted at most once, so a bad fix can never oscillate.
pub async fn run_autofix(
    driver: &dyn AutofixDriver,
    bases: &[PathBuf],
    max_rounds: usize,
) -> AutofixReport {
    let mut report = AutofixReport::default();

    // Phase 1 runs on the general modify spine: it owns the rounds, the tried set, the
    // rollback rules, and the order of the verdicts — reject the round before any
    // per-file verdict is trusted. Everything autofix-specific lives in the adapter
    // above: the report, the log wording, the byte backups, and the two rules.
    let errors = driver.errors().await;
    report.errors_before = total(&errors);
    if report.errors_before == 0 {
        report
            .log
            .push("no compile errors reported — skipping the error phase".to_string());
    }
    let adapter = AutofixAdapter::errors(driver, bases, errors, report);
    let _ = run_modify_loop(&adapter, max_rounds).await;
    let (mut report, backups, obs) = adapter.into_parts();
    report.errors_after = total(&obs.errors);
    // `report.rounds` is the adapter's count for the phase that just ran. Phase 2
    // reuses the field for its own count, so this one is kept aside first.
    let error_rounds = report.rounds;

    // ── Phase 2: safely-fixable warnings ─────────────────────────────────────
    // Runs only on a clean compile (fixing warnings on a broken build proves
    // nothing) and only for warnings classified as behaviour-preserving. The same
    // spine drives it; what differs is the gate, which here is per ROUND rather
    // than per file — keep the round only if no error appeared AND the
    // safely-fixable count fell below where the phase began.
    let mut measured: Option<WarningsByFile> = None;
    if report.errors_after == 0 {
        let _ = driver.lint().await;
        let warnings = driver.warnings().await;
        report.safe_warnings_before = total(&safe_warnings(&warnings));
        let adapter = AutofixAdapter::warnings(driver, bases, warnings, report, backups);
        let _ = run_modify_loop(&adapter, max_rounds).await;
        let (mut warnings_report, _backups, obs) = adapter.into_parts();
        // The adapter counts the rounds IT ran, which is what the warning phase's own
        // number means; the error phase's count is put back alongside it.
        warnings_report.warning_rounds = warnings_report.rounds;
        warnings_report.rounds = error_rounds;
        report = warnings_report;
        measured = Some(obs.warnings);
    }

    // Final measurement — a lint already ran whenever phase 2 did.
    let warnings_final = match measured {
        Some(warnings) => warnings,
        None => {
            let _ = driver.lint().await;
            driver.warnings().await
        }
    };
    report.warnings_after = total(&warnings_final);
    report.safe_warnings_after = total(&safe_warnings(&warnings_final));
    report.warnings_held = flatten(&held_warnings(&warnings_final));
    report.success = report.errors_after == 0;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn diag(build_type: &str, severity: &str, file: &str, message: &str) -> RawDiagnostic {
        RawDiagnostic {
            build_type: build_type.to_string(),
            severity: severity.to_string(),
            file: file.to_string(),
            message: message.to_string(),
        }
    }

    /// Warnings are split into "safe to fix automatically" vs "leave for review".
    #[test]
    fn warning_classification_separates_safe_from_judgement() {
        // Safe: removing these cannot change behaviour.
        assert!(warning_is_safe(
            "Value stored to 'dst_uv_h' is never read [deadcode.DeadStores]"
        ));
        assert!(warning_is_safe("unused variable 'x' [-Wunused-variable]"));
        assert!(warning_is_safe("unused parameter 'n' [-Wunused-parameter]"));
        assert!(warning_is_safe(
            "explicitly assigning value of variable to itself [-Wself-assign]"
        ));
        // Judgement calls: reported, never rewritten automatically.
        assert!(!warning_is_safe(
            "Dereference of null pointer [core.NullDereference]"
        ));
        assert!(!warning_is_safe(
            "comparison of integers of different signs [-Wsign-compare]"
        ));
        assert!(!warning_is_safe(
            "format specifies type 'int' but the argument has type 'long' [-Wformat]"
        ));
        assert!(!warning_is_safe(""));
    }

    #[test]
    fn warnings_are_grouped_and_split_by_safety() {
        let diags = [
            diag("build", "error", "a.cpp", "boom"),
            diag(
                "lint",
                "warning",
                "a.cpp",
                "dead store [deadcode.DeadStores]",
            ),
            diag(
                "lint",
                "warning",
                "a.cpp",
                "null deref [core.NullDereference]",
            ),
            diag(
                "lint",
                "warning",
                "b.cpp",
                "unused variable [-Wunused-variable]",
            ),
            diag("lint", "warning", "", "no file"),
        ];

        let warnings = warnings_from(&diags);
        assert_eq!(warnings.len(), 2, "errors are not warnings: {warnings:?}");
        assert_eq!(warnings["a.cpp"].len(), 2);

        let safe = safe_warnings(&warnings);
        assert_eq!(total(&safe), 2, "{safe:?}");
        assert!(safe.contains_key("a.cpp") && safe.contains_key("b.cpp"));

        let held = held_warnings(&warnings);
        assert_eq!(total(&held), 1, "{held:?}");
        assert!(held.contains_key("a.cpp") && !held.contains_key("b.cpp"));
        assert!(
            flatten(&held).iter().any(|l| l.contains("NullDereference")),
            "the held list names the finding: {held:?}"
        );
    }

    /// Canned driver: `errors()` reports the current state and `rebuild()`
    /// advances to the next scripted state, so convergence and regressions are
    /// exact.
    struct FakeDriver {
        states: Mutex<std::collections::VecDeque<ErrorsByFile>>,
        /// file -> replacement the model would return (absent = nothing to offer).
        proposals: Mutex<BTreeMap<String, Option<String>>>,
        rebuilds: Mutex<usize>,
        /// Scripted warning state: `warnings()` reports the front and a `rebuild()`
        /// (the build+lint re-measure) advances it, so a fix that clears a warning
        /// becomes visible exactly as it would in a real run.
        warning_states: Mutex<std::collections::VecDeque<WarningsByFile>>,
        warning_proposals: Mutex<BTreeMap<String, Option<String>>>,
        lints: Mutex<usize>,
    }

    impl FakeDriver {
        fn new(states: Vec<ErrorsByFile>) -> Self {
            Self {
                states: Mutex::new(states.into()),
                proposals: Mutex::new(BTreeMap::new()),
                rebuilds: Mutex::new(0),
                warning_states: Mutex::new(std::collections::VecDeque::new()),
                warning_proposals: Mutex::new(BTreeMap::new()),
                lints: Mutex::new(0),
            }
        }

        fn proposing(self, file: &str, content: &str) -> Self {
            self.proposals
                .lock()
                .unwrap()
                .insert(file.to_string(), Some(content.to_string()));
            self
        }

        /// Script the warnings the tools report, in order.
        fn with_warnings(self, states: Vec<WarningsByFile>) -> Self {
            *self.warning_states.lock().unwrap() = states.into();
            self
        }

        fn proposing_warning_fix(self, file: &str, content: &str) -> Self {
            self.warning_proposals
                .lock()
                .unwrap()
                .insert(file.to_string(), Some(content.to_string()));
            self
        }
    }

    #[async_trait::async_trait]
    impl AutofixDriver for FakeDriver {
        async fn errors(&self) -> ErrorsByFile {
            self.states
                .lock()
                .unwrap()
                .front()
                .cloned()
                .unwrap_or_default()
        }
        async fn propose(&self, file: &str, _path: &Path) -> Option<String> {
            self.proposals.lock().unwrap().get(file).cloned().flatten()
        }
        async fn rebuild(&self) -> ErrorsByFile {
            *self.rebuilds.lock().unwrap() += 1;
            // A rebuild IS the build+lint re-measure, so scripted warnings advance
            // together with the scripted errors.
            {
                let mut warnings = self.warning_states.lock().unwrap();
                if warnings.len() > 1 {
                    warnings.pop_front();
                }
            }
            let mut states = self.states.lock().unwrap();
            if states.len() > 1 {
                states.pop_front();
            }
            states.front().cloned().unwrap_or_default()
        }
        async fn lint(&self) -> usize {
            *self.lints.lock().unwrap() += 1;
            total(&self.warnings().await)
        }
        async fn warnings(&self) -> WarningsByFile {
            self.warning_states
                .lock()
                .unwrap()
                .front()
                .cloned()
                .unwrap_or_default()
        }
        async fn propose_warning_fix(
            &self,
            file: &str,
            _path: &Path,
            _warnings: &[String],
        ) -> Option<String> {
            self.warning_proposals
                .lock()
                .unwrap()
                .get(file)
                .cloned()
                .flatten()
        }
    }

    fn errs(entries: &[(&str, usize)]) -> ErrorsByFile {
        entries
            .iter()
            .map(|(f, n)| {
                (
                    f.to_string(),
                    (0..*n).map(|i| format!("{f}:{i}:1: error: boom")).collect(),
                )
            })
            .collect()
    }

    /// Warnings map with one warning per `(file, tag)` pair.
    fn warns(entries: &[(&str, &str)]) -> WarningsByFile {
        let mut out = WarningsByFile::new();
        for (file, tag) in entries {
            out.entry(file.to_string())
                .or_default()
                .push(format!("{file}:1:1: warning: something [{tag}]"));
        }
        out
    }

    /// A fix that clears the file's errors is kept, and the loop stops as soon as
    /// nothing is left to fix.
    #[tokio::test]
    async fn converges_and_keeps_the_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        std::fs::write(&file, "int broken( ;\n").unwrap();

        let driver = FakeDriver::new(vec![errs(&[("a.cpp", 1)]), errs(&[])])
            .proposing("a.cpp", "int fixed() { return 0; }\n");
        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.rounds, 1);
        assert_eq!(report.errors_before, 1);
        assert_eq!(report.errors_after, 0);
        assert_eq!(report.files_fixed, vec!["a.cpp"]);
        assert!(report.files_reverted.is_empty());
        // The verified fix is what stayed on disk.
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "int fixed() { return 0; }\n"
        );
    }

    /// A fix that improves but does not finish the file is kept and retried in
    /// the next round, so a partially-fixed file still converges to zero.
    #[tokio::test]
    async fn keeps_a_partial_improvement_and_retries_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.cpp"), "int broken( ;\n").unwrap();

        let driver = FakeDriver::new(vec![
            errs(&[("a.cpp", 3)]),
            errs(&[("a.cpp", 1)]),
            errs(&[]),
        ])
        .proposing("a.cpp", "int a() { return 0; }\n");
        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.rounds, 2, "3 → 1 → 0: {report:?}");
        assert_eq!(report.errors_after, 0);
        assert_eq!(report.files_fixed, vec!["a.cpp"], "listed once, not twice");
        assert!(report.files_reverted.is_empty());
    }

    /// A fix that does NOT reduce the errors is rolled back byte-for-byte, is not
    /// retried, and the run reports the failure honestly.
    #[tokio::test]
    async fn rolls_back_a_fix_that_does_not_help() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        let original = "int still_broken( ;\n";
        std::fs::write(&file, original).unwrap();

        // The rebuild reports the same error count → the fix achieved nothing.
        let driver = FakeDriver::new(vec![errs(&[("a.cpp", 1)]), errs(&[("a.cpp", 1)])])
            .proposing("a.cpp", "int nonsense(\n");
        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert!(!report.success);
        assert_eq!(report.files_reverted, vec!["a.cpp"]);
        assert!(report.files_fixed.is_empty());
        // Reverted exactly: the pre-run bytes are back.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        assert_eq!(report.rounds, 1, "must not retry a failed fix: {report:?}");
    }

    /// A regression (the fix added errors) is rolled back too.
    #[tokio::test]
    async fn rolls_back_a_fix_that_regresses() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        let original = "int one_error( ;\n";
        std::fs::write(&file, original).unwrap();

        let driver = FakeDriver::new(vec![errs(&[("a.cpp", 1)]), errs(&[("a.cpp", 3)])])
            .proposing("a.cpp", "int worse(\n");
        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert_eq!(report.files_reverted, vec!["a.cpp"]);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        assert!(!report.success);
    }

    /// No proposal → the file is skipped and the loop terminates without a build.
    #[tokio::test]
    async fn skips_files_the_model_cannot_fix() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.cpp"), "int broken( ;\n").unwrap();

        let driver = FakeDriver::new(vec![errs(&[("a.cpp", 1)])]);
        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert!(!report.success);
        assert_eq!(report.rounds, 0, "nothing was written: {report:?}");
        assert_eq!(report.files_skipped, vec!["a.cpp"]);
        assert_eq!(
            *driver.rebuilds.lock().unwrap(),
            0,
            "no rebuild without a write"
        );
    }

    /// A project that already compiles is left completely alone.
    #[tokio::test]
    async fn does_nothing_when_there_are_no_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let driver = FakeDriver::new(vec![errs(&[])]);
        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert!(report.success);
        assert_eq!(report.rounds, 0);
        assert_eq!(*driver.rebuilds.lock().unwrap(), 0);
        assert!(report.files_fixed.is_empty());
    }

    /// A safe warning is fixed automatically, verified by a re-measure, and kept.
    #[tokio::test]
    async fn fixes_a_safe_warning_and_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        std::fs::write(&file, "void f() { int x = 1; }\n").unwrap();

        let driver = FakeDriver::new(vec![errs(&[])])
            .with_warnings(vec![warns(&[("a.cpp", "deadcode.DeadStores")]), warns(&[])])
            .proposing_warning_fix("a.cpp", "void f() {}\n");

        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.safe_warnings_before, 1);
        assert_eq!(report.safe_warnings_after, 0);
        assert_eq!(report.warning_rounds, 1);
        assert_eq!(report.warnings_fixed, vec!["a.cpp"]);
        assert!(report.warnings_reverted.is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "void f() {}\n");
    }

    /// A warning fix that breaks the build is rolled back and reported.
    #[tokio::test]
    async fn rolls_back_a_warning_fix_that_breaks_the_build() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        let original = "void f() { int x = 1; }\n";
        std::fs::write(&file, original).unwrap();

        let warned = warns(&[("a.cpp", "deadcode.DeadStores")]);
        // The re-measure after the fix reports an ERROR → the edit must be undone.
        let driver = FakeDriver::new(vec![errs(&[]), errs(&[("a.cpp", 1)])])
            .with_warnings(vec![warned.clone(), warned.clone()])
            .proposing_warning_fix("a.cpp", "void f() { int ; }\n");

        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert!(report.warnings_fixed.is_empty(), "{report:?}");
        assert_eq!(report.warnings_reverted, vec!["a.cpp"]);
        assert_eq!(report.safe_warnings_after, 1, "the warning is still there");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            original,
            "the pre-run bytes must be back"
        );
    }

    /// Warnings that need judgement are reported, never rewritten.
    #[tokio::test]
    async fn leaves_judgement_warnings_for_review() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        let original = "void f(int *p) { *p = 1; }\n";
        std::fs::write(&file, original).unwrap();

        let driver = FakeDriver::new(vec![errs(&[])]).with_warnings(vec![warns(&[
            ("a.cpp", "core.NullDereference"),
            ("b.cpp", "-Wsign-compare"),
        ])]);

        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert_eq!(report.safe_warnings_before, 0);
        assert_eq!(report.safe_warnings_after, 0);
        assert_eq!(
            report.warning_rounds, 0,
            "nothing may be attempted: {report:?}"
        );
        assert!(report.warnings_fixed.is_empty());
        assert_eq!(report.warnings_held.len(), 2, "{:?}", report.warnings_held);
        assert!(report
            .warnings_held
            .iter()
            .any(|w| w.contains("NullDereference")));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            original,
            "the file must be untouched"
        );
        assert!(
            report.summary().contains("left for review"),
            "the summary must surface them: {}",
            report.summary()
        );
    }

    #[test]
    fn resolve_source_path_handles_build_dir_relative_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let build = root.join("build-rpi5");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/main.cpp"), "int x;\n").unwrap();

        // Compilers report paths relative to the directory the build ran in.
        let resolved = resolve_source_path("../app/main.cpp", &[root.clone(), build.clone()])
            .expect("resolves via the build dir");
        assert!(resolved.ends_with("app/main.cpp"), "{resolved:?}");
        // Absolute paths win outright.
        let abs = root.join("app/main.cpp");
        assert_eq!(
            resolve_source_path(abs.to_str().unwrap(), &[]).unwrap(),
            abs
        );
        // Unknown files are reported, never invented.
        assert!(resolve_source_path("nope.cpp", &[root]).is_none());
    }

    /// Opt-in: the real cross build reports compiler paths relative to the build
    /// directory (`../app/main.cpp` from `build-rpi5`), so the base resolution
    /// must actually reach the source — otherwise the loop would "skip" every
    /// file and quietly do nothing.
    ///
    /// Run with: SPIRE_AI_TRAPS_INTEGRATION=/abs/path/ai-traps cargo test …
    #[test]
    fn real_ai_traps_paths_resolve_through_the_build_dir() {
        let Ok(root) = std::env::var("SPIRE_AI_TRAPS_INTEGRATION") else {
            eprintln!("skipped: set SPIRE_AI_TRAPS_INTEGRATION=/abs/path/ai-traps");
            return;
        };
        let root = PathBuf::from(root);
        // The UI passes the subproject path; the build lives at the project root.
        assert_eq!(
            find_project_root(&root.join("app")),
            root,
            "a subproject path must resolve to the project root"
        );
        let bases = diagnostic_bases(&root);
        assert!(
            resolve_source_path("../app/main.cpp", &bases).is_some(),
            "the cross build's relative paths must resolve: {bases:?}"
        );
    }

    #[test]
    fn a_fix_must_strictly_reduce_the_error_count_to_be_kept() {
        assert!(!should_revert(3, 0), "3 → 0 is an improvement");
        assert!(!should_revert(3, 1), "3 → 1 is an improvement");
        assert!(should_revert(3, 3), "no progress is rolled back");
        assert!(should_revert(3, 5), "a regression is rolled back");
    }

    /// The build lives at the project root, but the UI passes a subproject path.
    #[test]
    fn find_project_root_walks_up_to_the_build_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(root.join("build-rpi5")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();

        assert_eq!(find_project_root(&root.join("app")), root);
        assert_eq!(find_project_root(&root), root);
        // Neither a build dir nor a marker above it → the path itself.
        let loose = tmp.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(find_project_root(&loose), loose);
    }

    #[test]
    fn diagnostic_bases_cover_the_root_and_every_build_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        for d in ["build-rpi5", "build-a7s", "app"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let bases = diagnostic_bases(&root);
        assert_eq!(bases.len(), 3, "root + the two build dirs: {bases:?}");
        assert_eq!(bases[0], root);
        assert!(bases.iter().any(|b| b.ends_with("build-rpi5")));
        assert!(bases.iter().any(|b| b.ends_with("build-a7s")));
        assert!(!bases.iter().any(|b| b.ends_with("app")), "{bases:?}");
    }

    #[test]
    fn derive_platform_prefers_the_target_name_then_the_only_build_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        for d in ["build-rpi5", "build-rock3c", "builddir", "build"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // Only `build-<id>` names are platforms (`builddir`/`build` are not).
        assert_eq!(
            platform_build_dirs(&root),
            vec!["rock3c".to_string(), "rpi5".to_string()]
        );

        // A target that names its platform resolves it…
        assert_eq!(
            derive_platform(&root, Some("ai-trap-rpi5")).as_deref(),
            Some("rpi5")
        );
        assert_eq!(
            derive_platform(&root, Some("ai-trap-rock3c")).as_deref(),
            Some("rock3c")
        );

        // …but with several candidates and no usable target we refuse to guess.
        assert_eq!(derive_platform(&root, Some("something-else")), None);
        assert_eq!(derive_platform(&root, None), None);

        // A project with exactly one build dir needs no target at all.
        let only = tmp.path().join("only");
        std::fs::create_dir_all(only.join("build-rpi5")).unwrap();
        assert_eq!(derive_platform(&only, None).as_deref(), Some("rpi5"));
    }

    #[test]
    fn resolve_platform_refuses_to_guess_between_several_builds() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        for d in ["build-rpi5", "build-rock3c"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // An explicit selection always wins…
        assert_eq!(
            resolve_platform(&root, Some("rpi5"), Some("ai-trap-rock3c"))
                .unwrap()
                .as_deref(),
            Some("rpi5")
        );
        // …then the build target name…
        assert_eq!(
            resolve_platform(&root, None, Some("ai-trap-rock3c"))
                .unwrap()
                .as_deref(),
            Some("rock3c")
        );
        // …and with several candidates and no usable hint we refuse, saying what
        // the user can choose.
        let err = resolve_platform(&root, None, None).expect_err("must refuse rather than guess");
        assert!(err.contains("no platform selected"), "{err}");
        assert!(err.contains("rpi5"), "must list the candidates: {err}");
        assert!(err.contains("Nothing was changed"), "{err}");

        // A single configured build needs no hint at all.
        let only = tmp.path().join("only");
        std::fs::create_dir_all(only.join("build-a7s")).unwrap();
        assert_eq!(
            resolve_platform(&only, None, None).unwrap().as_deref(),
            Some("a7s")
        );
        // No configured builds: discovery handles it.
        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(resolve_platform(&bare, None, None).unwrap(), None);
    }

    /// Opt-in: against the real project, a run with no platform must REFUSE rather
    /// than verify against whichever build dir discovery returns first, and a
    /// build-target name must resolve its platform.
    ///
    /// Run with: SPIRE_AI_TRAPS_INTEGRATION=/abs/path/ai-traps cargo test …
    #[test]
    fn real_ai_traps_platform_resolution() {
        let Ok(root) = std::env::var("SPIRE_AI_TRAPS_INTEGRATION") else {
            eprintln!("skipped: set SPIRE_AI_TRAPS_INTEGRATION=/abs/path/ai-traps");
            return;
        };
        let root = PathBuf::from(root);
        let platforms = platform_build_dirs(&root);
        if platforms.len() > 1 {
            let err = resolve_platform(&root, None, None)
                .expect_err("several configured builds must refuse, not guess");
            assert!(err.contains("no platform selected"), "{err}");
        }
        // The rpi5 executable target names its platform (ai-trap-rpi5 → rpi5).
        assert_eq!(
            resolve_platform(&root, None, Some("ai-trap-rpi5"))
                .unwrap()
                .as_deref(),
            Some("rpi5")
        );
        assert_eq!(
            resolve_platform(&root, Some("rpi5"), None)
                .unwrap()
                .as_deref(),
            Some("rpi5")
        );
    }

    /// The loop's input is COMPILE errors only.
    ///
    /// A stale lint pass had left hundreds of `file not found` entries for other
    /// platforms' sources; reading them made "Fix & Verify" claim errors on files
    /// the build never compiles (and then reported them as unfixable).
    #[test]
    fn build_errors_from_ignores_everything_but_build_errors() {
        let diags = [
            diag("build", "error", "a.cpp", "boom"),
            // Stale lint findings for another platform's sources: never fix targets.
            diag(
                "lint",
                "error",
                "../hal/implementations/rock3c/x.cpp",
                "fatal error: 'x' file not found",
            ),
            diag("lint", "warning", "a.cpp", "dead store"),
            diag("fix", "error", "b.cpp", "leftover"),
            diag("analyze", "error", "c.cpp", "leftover"),
            // Build warnings and contentless entries are not fixable errors either.
            diag("build", "warning", "w.cpp", "unused"),
            diag("build", "error", "", "no file"),
            diag("build", "error", "e.cpp", "   "),
            // A second error in the same file is grouped, not duplicated.
            diag("build", "error", "a.cpp", "boom2"),
        ];

        let errors = build_errors_from(&diags);
        assert_eq!(errors.len(), 1, "only build errors qualify: {errors:?}");
        assert_eq!(errors["a.cpp"].len(), 2, "grouped per file: {errors:?}");
        assert!(
            !errors.contains_key("../hal/implementations/rock3c/x.cpp"),
            "another platform's stale lint errors must never be fix targets: {errors:?}"
        );
    }

    /// A round that makes the project worse overall is rolled back completely,
    /// even though the file it rewrote looks "improved" on its own.
    #[tokio::test]
    async fn rolls_back_the_whole_round_when_the_project_gets_worse() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        let original = "int broken( ;\n";
        std::fs::write(&file, original).unwrap();

        // a.cpp itself improves (1 → 0), but the round introduces 3 errors elsewhere.
        let driver = FakeDriver::new(vec![
            errs(&[("a.cpp", 1)]),
            errs(&[("b.cpp", 3)]),
            errs(&[("a.cpp", 1)]),
        ])
        .proposing("a.cpp", "int a() { return 0; }\n");

        let report = run_autofix(&driver, &[tmp.path().to_path_buf()], 5).await;

        assert_eq!(report.files_reverted, vec!["a.cpp"]);
        assert!(report.files_fixed.is_empty(), "no net gain: {report:?}");
        assert_eq!(report.rounds, 1);
        assert_eq!(report.errors_after, 1);
        assert!(!report.success);
        assert!(
            report
                .log
                .iter()
                .any(|l| l.contains("made the project worse")),
            "the rollback must be explained: {report:?}"
        );
        // The pre-run bytes are back.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    }
}
