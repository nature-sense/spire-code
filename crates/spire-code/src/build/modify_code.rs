// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! `modify/code` — change existing code from the user's own words.
//!
//! The third use of the modify spine ([`crate::modify`]). Where autofix is driven by
//! compiler diagnostics and the HAL cascade by a drift report, this one's prompt is
//! the user's: "make the sampler emit 200 Hz", "rename this flag", "add a timeout".
//!
//! The shape is:
//!
//! 1. one **plan** call — the model reads the prompt and the files in `scope` and
//!    returns a complete rewrite per file;
//! 2. the spine applies them and **verifies** — the project must still build, and the
//!    tests that can run must still pass;
//! 3. anything that made the project worse is **restored byte-for-byte**.
//!
//! ## Verification is layered, and the report says which layer it reached
//!
//! Build and host tests are the gate. Target tests need a board and a live MCP
//! server, so they run only when one is connected — and when they do not, the report
//! says so ([`Verified::HostOnly`], plus a caveat). A run that never reached the
//! hardware is not presented as fully verified.
//!
//! ## One plan, one round
//!
//! A free-text change is a single coherent intent, so the plan is handed to the spine
//! once and never re-derived: a second round would just re-apply the same rewrites.
//! That is also why the round is the unit of judgement — a half-applied plan is not a
//! meaningful outcome, so `accept` keeps every file and `reject_round` decides.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::build::autofix::ErrorsByFile;
use crate::modify::{run_modify_loop, ChangeTarget, ModifyDriver, Observation};

/// One file the model wants rewritten, and what it should say afterwards.
#[derive(Debug, Clone)]
pub struct PlannedChange {
    /// Path the backend resolved; used verbatim as the file to write.
    pub file: String,
    /// The complete new contents.
    pub content: String,
}

/// How far verification actually reached on a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verified {
    /// Build and host tests only: no board was connected, so target tests did not run.
    HostOnly,
    /// Build, host tests, and target tests on a real board.
    WithTarget,
}

/// What `modify/code` needs from the outside world.
///
/// Implemented for real by the coordinator (LLM actor + build/test actors); faked in
/// the tests below, which is what keeps the flow's decisions provable without a
/// compiler, a board, or a model.
#[async_trait::async_trait]
pub trait CodeModifyBackend: Send + Sync {
    /// Ask the model for the rewrites. `None`, or an empty list, means it proposed
    /// nothing usable — which is a failure to show the user, not a silent no-op.
    async fn plan(&self, prompt: &str, scope: &[PathBuf]) -> Option<Vec<PlannedChange>>;

    /// Compile, and return the errors the build reports, grouped by file.
    async fn build(&self) -> ErrorsByFile;

    /// Run the host tests. `None` when there is nothing to run.
    async fn host_tests(&self) -> Option<usize>;

    /// Run the target tests on the connected board. `None` when no board is connected
    /// for the selected platform, which is what makes verification host-only.
    async fn target_tests(&self) -> Option<usize>;
}

/// The measured state of the project: what compiles, and what passes.
#[derive(Debug, Clone, Default, PartialEq)]
struct CodeObs {
    build_errors: ErrorsByFile,
    /// Host test failures; `None` when there was nothing to run.
    host_failures: Option<usize>,
    /// Target test failures; `None` when no board was connected to run them on.
    target_failures: Option<usize>,
}

impl Observation for CodeObs {
    fn is_clean(&self) -> bool {
        total(&self.build_errors) == 0
            && self.host_failures.map_or(true, |f| f == 0)
            && self.target_failures.map_or(true, |f| f == 0)
    }
}

fn total(errors: &ErrorsByFile) -> usize {
    errors.values().map(|v| v.len()).sum()
}

/// A test leg got worse only when it ran *both* times and the count rose: a leg that
/// could not run cannot have regressed, and saying otherwise would roll back honest
/// work for want of hardware.
fn worse(before: Option<usize>, after: Option<usize>) -> bool {
    matches!((before, after), (Some(b), Some(a)) if a > b)
}

/// Render a test count for the log, distinguishing "passed" from "never ran".
fn show(count: Option<usize>) -> String {
    count.map_or_else(|| "not run".to_string(), |n| n.to_string())
}

/// Adapts a [`CodeModifyBackend`] and one plan to the modify spine.
struct CodeDriver<'a> {
    backend: &'a dyn CodeModifyBackend,
    /// The plan, handed to the spine once. `targets` takes it, so a second round finds
    /// nothing to do and the loop ends after one.
    plan: Mutex<Option<Vec<ChangeTarget>>>,
    /// The rewrites the plan produced, keyed by target id.
    contents: Mutex<BTreeMap<String, String>>,
    /// The bytes each file had before this run, so a rollback restores them exactly.
    backups: Mutex<BTreeMap<String, String>>,
    /// The round-start measurement, kept for the report's "before" figures.
    before: Mutex<Option<CodeObs>>,
    /// The latest measurement.
    current: Mutex<CodeObs>,
    /// Whether a measurement has happened yet: the first `verify` must take one.
    measured: Mutex<bool>,
    /// Writes since the previous measurement.
    wrote_since_verify: Mutex<usize>,
    /// Set by a rollback: the disk changed, so the cached measurement is stale and the
    /// next verification has to re-measure rather than hand back the old answer.
    needs_measure: Mutex<bool>,
    report: Mutex<ModifyCodeReport>,
}

impl CodeDriver<'_> {
    fn into_report(self, rounds: usize) -> ModifyCodeReport {
        let mut report = self.report.into_inner().unwrap();
        let before = self.before.into_inner().unwrap().unwrap_or_default();
        let after = self.current.into_inner().unwrap();

        report.rounds = rounds;
        report.build_errors_before = total(&before.build_errors);
        report.build_errors_after = total(&after.build_errors);
        report.host_failures_before = before.host_failures;
        report.host_failures_after = after.host_failures;
        report.target_failures_before = before.target_failures;
        report.target_failures_after = after.target_failures;

        // Layered verification, stated plainly rather than left to be inferred.
        report.verified = if after.target_failures.is_some() {
            Verified::WithTarget
        } else {
            Verified::HostOnly
        };
        if after.target_failures.is_none() {
            report.caveats.push(
                "no board connected: verified against the host only, target tests not run"
                    .to_string(),
            );
        }
        if after.host_failures.is_none() {
            report
                .caveats
                .push("no host tests to run for this project".to_string());
        }

        report.success = after.is_clean() && report.error.is_none();
        report.log.push(match report.verified {
            Verified::WithTarget => "verified: build, host tests, and target tests".to_string(),
            Verified::HostOnly => {
                "verified: build and host tests (target tests not run)".to_string()
            }
        });
        report
    }
}

/// What a `modify/code` run did.
#[derive(Debug, Clone)]
pub struct ModifyCodeReport {
    /// True when the code built and every test that ran passed.
    pub success: bool,
    /// How far verification reached. Never overstate this.
    pub verified: Verified,
    /// Rounds that wrote something. Normally 1: a plan is applied as a whole, or not.
    pub rounds: usize,
    pub files_changed: Vec<String>,
    pub files_reverted: Vec<String>,
    pub files_skipped: Vec<String>,
    pub build_errors_before: usize,
    pub build_errors_after: usize,
    pub host_failures_before: Option<usize>,
    pub host_failures_after: Option<usize>,
    pub target_failures_before: Option<usize>,
    pub target_failures_after: Option<usize>,
    /// What could not be checked, in the user's words.
    pub caveats: Vec<String>,
    pub log: Vec<String>,
    /// Set when the run could not start, or the model proposed nothing.
    pub error: Option<String>,
}

impl ModifyCodeReport {
    fn new() -> Self {
        Self {
            success: false,
            verified: Verified::HostOnly,
            rounds: 0,
            files_changed: Vec::new(),
            files_reverted: Vec::new(),
            files_skipped: Vec::new(),
            build_errors_before: 0,
            build_errors_after: 0,
            host_failures_before: None,
            host_failures_after: None,
            target_failures_before: None,
            target_failures_after: None,
            caveats: Vec::new(),
            log: Vec::new(),
            error: None,
        }
    }

    /// Human-readable result, for the UI's result pane.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        if let Some(error) = &self.error {
            out.push_str(&format!("Modify: {error}\n"));
        }
        if !self.files_changed.is_empty() {
            out.push_str(&format!(
                "changed:\n  {}\n",
                self.files_changed.join("\n  ")
            ));
        }
        if !self.files_reverted.is_empty() {
            out.push_str(&format!(
                "rolled back (the project got worse):\n  {}\n",
                self.files_reverted.join("\n  ")
            ));
        }
        if !self.files_skipped.is_empty() {
            out.push_str(&format!(
                "skipped:\n  {}\n",
                self.files_skipped.join("\n  ")
            ));
        }
        out.push_str(&format!(
            "verify: build {} → {} error(s), host tests {} → {}, target tests {} → {}\n",
            self.build_errors_before,
            self.build_errors_after,
            show(self.host_failures_before),
            show(self.host_failures_after),
            show(self.target_failures_before),
            show(self.target_failures_after),
        ));
        for caveat in &self.caveats {
            out.push_str(&format!("note: {caveat}\n"));
        }
        out
    }
}

impl ModifyDriver for CodeDriver<'_> {
    type Obs = CodeObs;

    fn targets(&self) -> Vec<ChangeTarget> {
        // Taken, not re-derived: the plan is one intent, applied once. The next round
        // therefore finds nothing to do and the loop ends.
        self.plan.lock().unwrap().take().unwrap_or_default()
    }

    async fn propose(&self, target: &ChangeTarget) -> Option<String> {
        self.contents.lock().unwrap().get(&target.id).cloned()
    }

    async fn verify(&self) -> CodeObs {
        let wrote = {
            let mut w = self.wrote_since_verify.lock().unwrap();
            let n = *w;
            *w = 0;
            n
        };
        let stale = {
            let mut s = self.needs_measure.lock().unwrap();
            let v = *s;
            *s = false;
            v
        };
        let measured = {
            let mut m = self.measured.lock().unwrap();
            let v = *m;
            *m = true;
            v
        };
        // Re-running the build and the tests costs minutes; doing it to be told the
        // same answer as last time costs them for nothing. Re-measure only when the
        // disk has changed since the last measurement.
        if measured && wrote == 0 && !stale {
            return self.current.lock().unwrap().clone();
        }
        let obs = CodeObs {
            build_errors: self.backend.build().await,
            host_failures: self.backend.host_tests().await,
            target_failures: self.backend.target_tests().await,
        };
        {
            let mut before = self.before.lock().unwrap();
            if before.is_none() {
                *before = Some(obs.clone());
            }
        }
        *self.current.lock().unwrap() = obs.clone();
        obs
    }

    fn accept(&self, target: &ChangeTarget, _before: &CodeObs, _after: &CodeObs) -> bool {
        // A plan is one intent, so the verdict belongs to the round: a file the spine
        // keeps is simply recorded here. `reject_round` makes the real decision.
        let mut report = self.report.lock().unwrap();
        if !report.files_changed.iter().any(|f| f == &target.id) {
            report.files_changed.push(target.id.clone());
        }
        true
    }

    fn reject_round(&self, before: &CodeObs, after: &CodeObs) -> bool {
        let build_worse = total(&after.build_errors) > total(&before.build_errors);
        let host_worse = worse(before.host_failures, after.host_failures);
        let target_worse = worse(before.target_failures, after.target_failures);
        if !(build_worse || host_worse || target_worse) {
            return false;
        }
        let mut report = self.report.lock().unwrap();
        report.log.push(format!(
            "rolled back: build errors {} → {}, host failures {} → {}, target failures {} → {}",
            total(&before.build_errors),
            total(&after.build_errors),
            show(before.host_failures),
            show(after.host_failures),
            show(before.target_failures),
            show(after.target_failures),
        ));
        true
    }

    async fn apply(&self, target: &ChangeTarget, content: &str) -> Result<(), String> {
        // Read the original before the model's version replaces it.
        {
            let mut backups = self.backups.lock().unwrap();
            if !backups.contains_key(&target.id) {
                let orig = std::fs::read_to_string(&target.path)
                    .map_err(|e| format!("cannot read ({e})"))?;
                backups.insert(target.id.clone(), orig);
            }
        }
        std::fs::write(&target.path, content).map_err(|e| format!("write failed ({e})"))?;
        *self.wrote_since_verify.lock().unwrap() += 1;
        Ok(())
    }

    async fn revert(&self, target: &ChangeTarget) -> Result<(), String> {
        if let Some(orig) = self.backups.lock().unwrap().get(&target.id).cloned() {
            let _ = std::fs::write(&target.path, orig);
        }
        let mut report = self.report.lock().unwrap();
        if !report.files_reverted.iter().any(|f| f == &target.id) {
            report.files_reverted.push(target.id.clone());
        }
        drop(report);
        // The file is back to its original bytes, so the cached measurement no longer
        // describes the disk.
        *self.needs_measure.lock().unwrap() = true;
        Ok(())
    }
}

/// Run `modify/code`: plan once, apply, verify, keep or roll back.
///
/// `scope` is what the model may consider — the project, or the user's selection.
/// `max_rounds` bounds the work, though a plan is a single intent and normally lands
/// in one round.
pub async fn run_code_modify(
    backend: &dyn CodeModifyBackend,
    prompt: &str,
    scope: &[PathBuf],
    max_rounds: usize,
) -> ModifyCodeReport {
    let mut report = ModifyCodeReport::new();

    let Some(plan) = backend.plan(prompt, scope).await else {
        report.error = Some("the model proposed no changes; nothing was written".to_string());
        return report;
    };

    let mut targets = Vec::new();
    let mut contents = BTreeMap::new();
    for change in plan {
        if change.content.trim().is_empty() {
            report.files_skipped.push(change.file.clone());
            report
                .log
                .push(format!("skip {}: the plan was empty", change.file));
            continue;
        }
        contents.insert(change.file.clone(), change.content);
        targets.push(ChangeTarget::new(
            change.file.clone(),
            PathBuf::from(&change.file),
        ));
    }
    if targets.is_empty() {
        report.error =
            Some("the model proposed no usable changes; nothing was written".to_string());
        return report;
    }
    report.log.push(format!("plan: {} file(s)", targets.len()));

    let driver = CodeDriver {
        backend,
        plan: Mutex::new(Some(targets)),
        contents: Mutex::new(contents),
        backups: Mutex::new(BTreeMap::new()),
        before: Mutex::new(None),
        current: Mutex::new(CodeObs::default()),
        measured: Mutex::new(false),
        wrote_since_verify: Mutex::new(0),
        needs_measure: Mutex::new(false),
        report: Mutex::new(report),
    };
    let spine = run_modify_loop(&driver, max_rounds).await;
    driver.into_report(spine.rounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripts a run's measurements and supplies a plan.
    ///
    /// Each call returns the current head; the head advances only while something is
    /// left behind it, so a run that measures *more* often than it should sees the last
    /// scripted answer again rather than a panic. The build counter is what makes an
    /// over-eager verification visible.
    struct Fake {
        plan: Option<Vec<PlannedChange>>,
        builds: Mutex<Vec<ErrorsByFile>>,
        host: Mutex<Vec<Option<usize>>>,
        target: Mutex<Vec<Option<usize>>>,
        build_calls: Mutex<usize>,
    }

    fn step<T: Clone + Default>(script: &Mutex<Vec<T>>) -> T {
        let mut script = script.lock().unwrap();
        let value = script.first().cloned().unwrap_or_default();
        if script.len() > 1 {
            script.remove(0);
        }
        value
    }

    impl Fake {
        fn new(builds: Vec<ErrorsByFile>) -> Self {
            Self {
                plan: None,
                builds: Mutex::new(builds),
                host: Mutex::new(Vec::new()),
                target: Mutex::new(Vec::new()),
                build_calls: Mutex::new(0),
            }
        }

        fn planning(mut self, changes: Vec<(String, String)>) -> Self {
            self.plan = Some(
                changes
                    .into_iter()
                    .map(|(file, content)| PlannedChange { file, content })
                    .collect(),
            );
            self
        }

        fn hosting(self, host: Vec<Option<usize>>) -> Self {
            *self.host.lock().unwrap() = host;
            self
        }

        fn targeting(self, target: Vec<Option<usize>>) -> Self {
            *self.target.lock().unwrap() = target;
            self
        }

        fn build_calls(&self) -> usize {
            *self.build_calls.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl CodeModifyBackend for Fake {
        async fn plan(&self, _prompt: &str, _scope: &[PathBuf]) -> Option<Vec<PlannedChange>> {
            self.plan.clone()
        }

        async fn build(&self) -> ErrorsByFile {
            *self.build_calls.lock().unwrap() += 1;
            step(&self.builds)
        }

        async fn host_tests(&self) -> Option<usize> {
            step(&self.host)
        }

        async fn target_tests(&self) -> Option<usize> {
            step(&self.target)
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

    /// A one-file project on disk, plus the file's name as the plan would give it.
    fn project(contents: &str) -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.cpp");
        std::fs::write(&file, contents).unwrap();
        let name = file.to_string_lossy().to_string();
        (tmp, file, name)
    }

    /// A change that builds clean and passes the host tests is kept, and the report is
    /// explicit that it was only verified against the host.
    #[tokio::test]
    async fn keeps_a_change_that_builds_and_passes() {
        let (tmp, file, name) = project("int broken;\n");
        let fixed = "int fixed() { return 0; }\n";

        let backend = Fake::new(vec![errs(&[]), errs(&[])])
            .planning(vec![(name.clone(), fixed.to_string())])
            .hosting(vec![Some(0), Some(0)])
            .targeting(vec![None, None]);
        let report = run_code_modify(&backend, "fix it", &[tmp.path().to_path_buf()], 3).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.verified, Verified::HostOnly);
        assert_eq!(report.files_changed, vec![name]);
        assert!(report.files_reverted.is_empty());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), fixed);
        assert!(
            report
                .caveats
                .iter()
                .any(|c| c.contains("no board connected")),
            "the report must not imply hardware verification: {:?}",
            report.caveats
        );
    }

    /// The whole plan is rolled back when the build gets worse — a free-text change is
    /// one intent, so half of it is not an outcome.
    #[tokio::test]
    async fn rolls_back_a_change_that_breaks_the_build() {
        let (tmp, file, name) = project("int original;\n");

        let backend = Fake::new(vec![errs(&[]), errs(&[("a.cpp", 2)])])
            .planning(vec![(name.clone(), "int broken( ;\n".to_string())])
            .hosting(vec![Some(0), Some(0)])
            .targeting(vec![None, None]);
        let report = run_code_modify(&backend, "break it", &[tmp.path().to_path_buf()], 3).await;

        assert!(!report.success, "{report:?}");
        assert_eq!(report.files_reverted, vec![name]);
        assert!(report.files_changed.is_empty());
        assert_eq!(report.build_errors_before, 0);
        assert_eq!(report.build_errors_after, 2);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "int original;\n",
            "the original bytes are restored"
        );
    }

    /// Host tests are a gate: a change that breaks them is rolled back even though it
    /// still compiles.
    #[tokio::test]
    async fn rolls_back_a_change_that_breaks_the_host_tests() {
        let (tmp, file, name) = project("int original;\n");

        let backend = Fake::new(vec![errs(&[]), errs(&[])])
            .planning(vec![(name.clone(), "int changed;\n".to_string())])
            .hosting(vec![Some(0), Some(3)])
            .targeting(vec![None, None]);
        let report =
            run_code_modify(&backend, "break the tests", &[tmp.path().to_path_buf()], 3).await;

        assert!(!report.success);
        assert_eq!(report.files_reverted, vec![name]);
        assert_eq!(report.host_failures_before, Some(0));
        assert_eq!(report.host_failures_after, Some(3));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "int original;\n");
    }

    /// With a board connected the target tests run, and the report says so — no caveat.
    #[tokio::test]
    async fn verifies_against_the_board_when_one_is_connected() {
        let (tmp, file, name) = project("int original;\n");

        let backend = Fake::new(vec![errs(&[]), errs(&[])])
            .planning(vec![(name.clone(), "int changed;\n".to_string())])
            .hosting(vec![Some(0), Some(0)])
            .targeting(vec![Some(0), Some(0)]);
        let report = run_code_modify(&backend, "change it", &[tmp.path().to_path_buf()], 3).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.verified, Verified::WithTarget);
        assert_eq!(report.target_failures_after, Some(0));
        assert!(
            !report
                .caveats
                .iter()
                .any(|c| c.contains("no board connected")),
            "a board ran the tests, so there is nothing to caveat: {:?}",
            report.caveats
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "int changed;\n");
    }

    /// Target tests are a gate too when they ran: passing on the host is not enough if
    /// the board now fails.
    #[tokio::test]
    async fn rolls_back_when_the_target_tests_regress() {
        let (tmp, file, name) = project("int original;\n");

        let backend = Fake::new(vec![errs(&[]), errs(&[])])
            .planning(vec![(name.clone(), "int changed;\n".to_string())])
            .hosting(vec![Some(0), Some(0)])
            .targeting(vec![Some(0), Some(2)]);
        let report = run_code_modify(
            &backend,
            "break it on the board",
            &[tmp.path().to_path_buf()],
            3,
        )
        .await;

        assert!(!report.success);
        assert_eq!(report.files_reverted, vec![name]);
        assert_eq!(report.target_failures_after, Some(2));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "int original;\n");
    }

    /// Nothing proposed is a failure the user sees, and nothing is written — not a
    /// silent success.
    #[tokio::test]
    async fn writes_nothing_when_the_model_proposes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = Fake::new(vec![errs(&[])]);
        let report =
            run_code_modify(&backend, "do something", &[tmp.path().to_path_buf()], 3).await;

        assert!(!report.success);
        assert!(report.error.is_some(), "{report:?}");
        assert_eq!(report.rounds, 0);
        assert_eq!(backend.build_calls(), 0, "no plan means no build");
    }

    /// A run builds twice — once to learn the baseline, once to judge the change — and
    /// not a third time, because nothing changed in between. That cadence is what keeps
    /// a free-text change affordable.
    #[tokio::test]
    async fn measures_once_for_the_baseline_and_once_for_the_verdict() {
        let (tmp, _file, name) = project("int original;\n");

        let backend = Fake::new(vec![errs(&[]), errs(&[])])
            .planning(vec![(name, "int changed;\n".to_string())])
            .hosting(vec![Some(0), Some(0)])
            .targeting(vec![None, None]);
        let report = run_code_modify(&backend, "change it", &[tmp.path().to_path_buf()], 3).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.rounds, 1);
        assert_eq!(
            backend.build_calls(),
            2,
            "the verdict measurement is reused for the final state"
        );
    }
}
