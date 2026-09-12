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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Diagnostic lines currently recorded for a file, keyed by that file.
pub type ErrorsByFile = BTreeMap<String, Vec<String>>;

/// Outcome of one autofix run.
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
    /// Human-readable trace of what the loop did, line by line.
    pub log: Vec<String>,
}

impl AutofixReport {
    /// Short, human summary for the UI's result pane.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Fix & Verify: {} error(s) before → {} after · {} warning(s) remaining\n",
            self.errors_before, self.errors_after, self.warnings_after
        ));
        out.push_str(&format!(
            "rounds used: {} · fixed: {} · reverted: {} · skipped: {}\n",
            self.rounds,
            self.files_fixed.len(),
            self.files_reverted.len(),
            self.files_skipped.len()
        ));
        if !self.files_fixed.is_empty() {
            out.push_str(&format!("fixed:\n  {}\n", self.files_fixed.join("\n  ")));
        }
        if !self.files_reverted.is_empty() {
            out.push_str(&format!(
                "reverted (the fix did not reduce the errors):\n  {}\n",
                self.files_reverted.join("\n  ")
            ));
        }
        if !self.log.is_empty() {
            out.push_str("--- trace ---\n");
            out.push_str(&self.log.join("\n"));
            out.push('\n');
        }
        if self.success {
            out.push_str("Result: the project compiles with no errors.");
        } else {
            out.push_str(&format!(
                "Result: {} error(s) still reported — see the Build tab for the files that need attention.",
                self.errors_after
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
            rd.flatten().any(|e| {
                e.path().is_dir() && e.file_name().to_string_lossy().starts_with("build")
            })
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
    let mut errors = driver.errors().await;
    report.errors_before = total(&errors);
    if report.errors_before == 0 {
        report.warnings_after = driver.lint().await;
        report.success = true;
        report
            .log
            .push("nothing to fix: no compile errors reported".to_string());
        return report;
    }

    let mut tried: BTreeSet<String> = BTreeSet::new();
    let mut backups: BTreeMap<String, String> = BTreeMap::new();

    for round in 0..max_rounds.max(1) {
        let candidates: Vec<String> = errors
            .keys()
            .filter(|f| !tried.contains(*f))
            .cloned()
            .collect();
        if candidates.is_empty() {
            break;
        }
        report.log.push(format!(
            "round {}: {} file(s) with errors",
            round + 1,
            candidates.len()
        ));

        let mut wrote: Vec<String> = Vec::new();
        for file in &candidates {
            let Some(path) = resolve_source_path(file, bases) else {
                report.log.push(format!("skip {file}: not found on disk"));
                tried.insert(file.clone());
                report.files_skipped.push(file.clone());
                continue;
            };
            // Back up the ORIGINAL bytes once, so a rollback always restores the
            // state the project had before this run started.
            if !backups.contains_key(file) {
                match std::fs::read_to_string(&path) {
                    Ok(orig) => {
                        backups.insert(file.clone(), orig);
                    }
                    Err(e) => {
                        report.log.push(format!("skip {file}: cannot read ({e})"));
                        tried.insert(file.clone());
                        report.files_skipped.push(file.clone());
                        continue;
                    }
                }
            }
            let before = errors.get(file).map(|v| v.len()).unwrap_or(0);
            let Some(content) = driver.propose(file, &path).await else {
                report.log.push(format!("skip {file}: no fix proposed"));
                tried.insert(file.clone());
                report.files_skipped.push(file.clone());
                continue;
            };
            if content.trim().is_empty() {
                report.log.push(format!("skip {file}: empty proposal"));
                tried.insert(file.clone());
                report.files_skipped.push(file.clone());
                continue;
            }
            if let Err(e) = std::fs::write(&path, &content) {
                report.log.push(format!("skip {file}: write failed ({e})"));
                tried.insert(file.clone());
                report.files_skipped.push(file.clone());
                continue;
            }
            report
                .log
                .push(format!("applied fix to {file} ({before} error(s) before)"));
            wrote.push(file.clone());
        }

        if wrote.is_empty() {
            break; // nothing written — no point looping
        }
        report.rounds += 1;

        // One rebuild per round, then a per-file verdict: diagnostics are
        // per-file, so a bad fix stays attributable even in a batch.
        let after = driver.rebuild().await;
        let mut rolled_back = false;
        for file in &wrote {
            let before = errors.get(file).map(|v| v.len()).unwrap_or(0);
            let now = after.get(file).map(|v| v.len()).unwrap_or(0);
            if !should_revert(before, now) {
                if !report.files_fixed.contains(file) {
                    report.files_fixed.push(file.clone());
                }
                continue;
            }
            if let (Some(orig), Some(path)) = (backups.get(file), resolve_source_path(file, bases)) {
                let _ = std::fs::write(&path, orig);
            }
            report
                .log
                .push(format!("reverted {file}: errors {before} → {now}"));
            if !report.files_reverted.contains(file) {
                report.files_reverted.push(file.clone());
            }
            // A rolled-back file is never attempted again: a fix that keeps
            // failing would otherwise oscillate until the round cap.
            tried.insert(file.clone());
            rolled_back = true;
        }
        // The measurement is stale once a file has been rolled back.
        errors = if rolled_back {
            driver.rebuild().await
        } else {
            after
        };
    }

    report.errors_after = total(&errors);
    report.warnings_after = driver.lint().await;
    report.success = report.errors_after == 0;
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Canned driver: `errors()` reports the current state and `rebuild()`
    /// advances to the next scripted state, so convergence and regressions are
    /// exact.
    struct FakeDriver {
        states: Mutex<std::collections::VecDeque<ErrorsByFile>>,
        /// file -> replacement the model would return (absent = nothing to offer).
        proposals: Mutex<BTreeMap<String, Option<String>>>,
        rebuilds: Mutex<usize>,
        warnings: usize,
    }

    impl FakeDriver {
        fn new(states: Vec<ErrorsByFile>) -> Self {
            Self {
                states: Mutex::new(states.into()),
                proposals: Mutex::new(BTreeMap::new()),
                rebuilds: Mutex::new(0),
                warnings: 0,
            }
        }

        fn proposing(self, file: &str, content: &str) -> Self {
            self.proposals
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
            let mut states = self.states.lock().unwrap();
            if states.len() > 1 {
                states.pop_front();
            }
            states.front().cloned().unwrap_or_default()
        }
        async fn lint(&self) -> usize {
            self.warnings
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

        let driver = FakeDriver::new(vec![errs(&[("a.cpp", 3)]), errs(&[("a.cpp", 1)]), errs(&[])])
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
}
