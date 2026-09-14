// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! `modify-contract` — resolve the HAL contract cascade: contract → implementations →
//! consumers.
//!
//! The fourth use of the modify spine ([`crate::modify`]), and the one whose measure is
//! neither compiler output nor the user's words but **drift**: an interface that the
//! contract defines and a platform does not implement, or implements against an older
//! signature.
//!
//! The shape is the same spine, with that measure plugged in:
//!
//! 1. the **gaps** are the targets — interfaces that are missing or drifted on a
//!    platform, exactly what the drift analysis already reports;
//! 2. the change for each is the existing gap-fill plan, applied through the existing
//!    fill tool (the backend decides how; `apply` is not necessarily a file write);
//! 3. the round is kept only if the drift **fell** and the project still builds.
//!
//! ## Why the round's verdict is "did the project get worse", not "did this gap close"
//!
//! A gap-fill writes implementation files, and a mistake in one can break a *different*
//! interface — there is one shared contract. So the per-gap verdict is not enough on its
//! own: `reject_round` judges the whole measure, and a round that grew the drift or
//! broke the build is undone in full. That is the same reasoning as autofix's
//! project-worse guard, and it is the second consumer of [`ModifyDriver::reject_round`].
//!
//! ## The "up" direction
//!
//! Closing drift is the "down" half (contract → implementations). The "up" half —
//! implementations changing *consumers*, which shows up as ordinary compile errors — is
//! deliberately not duplicated here: those errors are exactly what
//! [`crate::build::autofix`] already fixes, verified and reversible. A caller that wants
//! both runs the cascade, then Fix & Verify.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::build::autofix::ErrorsByFile;
use crate::modify::{run_modify_loop, ChangeTarget, ModifyDriver, Observation};

/// One interface that is not fully implemented on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    pub platform: String,
    pub interface: String,
    /// Methods the contract declares and the implementation does not provide.
    pub missing: Vec<String>,
    /// Methods whose implementation no longer matches the contract's signature.
    pub drifted: Vec<String>,
    /// The implementation file exists but is a **stub** (`SPIRE-HAL-STUB`): the methods
    /// are declared and do nothing.
    ///
    /// Reported, but deliberately **not** a unit of drift on its own. Coverage already
    /// counts a stub's methods as unmet — a body that does nothing is not an
    /// implementation — so adding a unit here would double-count and make scaffolding an
    /// interface look *worse* than having nothing at all. That was the integration test's
    /// finding, and it is the difference between a measure and a mood.
    pub stub: bool,
}

impl Gap {
    /// Stable identity, and what the log and the report name it by.
    pub fn id(&self) -> String {
        format!("{}/{}", self.platform, self.interface)
    }

    /// How much is wrong with this gap: the work the contract still asks for.
    pub fn size(&self) -> usize {
        self.missing.len() + self.drifted.len()
    }
}

/// The drift measure: every interface that is not fully implemented.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gaps {
    pub open: Vec<Gap>,
}

impl Gaps {
    /// The total amount of drift — what "the project got worse" is measured against.
    pub fn size(&self) -> usize {
        self.open.iter().map(Gap::size).sum()
    }

    fn find(&self, id: &str) -> Option<&Gap> {
        self.open.iter().find(|gap| gap.id() == id)
    }
}

/// What the backend will do to close a gap, and what it will touch.
#[derive(Debug, Clone)]
pub struct ContractChange {
    /// The files this change will write, so the driver can back them up before it
    /// happens. A gap-fill is not a single file, and it is not the driver's job to know
    /// which ones the tool picked.
    pub files: Vec<PathBuf>,
    /// Opaque to the driver: handed back to [`ContractModifyBackend::apply_plan`], which
    /// is the only thing that needs to understand it.
    pub payload: String,
}

/// What the cascade needs from the outside world.
///
/// Everything HAL-specific lives behind this: reading the drift measure, turning a gap
/// into a plan, and applying that plan through the fill tool. Faked in the tests below,
/// which is what keeps the decisions provable without a project, a contract, or a model.
#[async_trait::async_trait]
pub trait ContractModifyBackend: Send + Sync {
    /// The drift measure right now.
    async fn gaps(&self) -> Gaps;

    /// A change that would close `gap`. `None` when the backend can propose nothing —
    /// a skip, not a failure.
    async fn plan(&self, gap: &Gap) -> Option<ContractChange>;

    /// Apply what [`Self::plan`] produced. The payload is the backend's own.
    async fn apply_plan(&self, payload: &str) -> Result<(), String>;

    /// Compile, returning the errors the build reports, grouped by file.
    async fn build(&self) -> ErrorsByFile;
}

/// The measured state of the project: how much drift, and what compiles.
#[derive(Debug, Clone, Default, PartialEq)]
struct ContractObs {
    gaps: Gaps,
    build_errors: ErrorsByFile,
}

impl Observation for ContractObs {
    fn is_clean(&self) -> bool {
        self.gaps.size() == 0 && total(&self.build_errors) == 0
    }
}

fn total(errors: &ErrorsByFile) -> usize {
    errors.values().map(|v| v.len()).sum()
}

/// What a `modify-contract` run did.
#[derive(Debug, Clone)]
pub struct ModifyContractReport {
    /// True when nothing is missing, nothing has drifted, and the project builds.
    pub success: bool,
    /// Rounds that applied at least one change.
    pub rounds: usize,
    /// Gaps that closed — interfaces now fully implemented and aligned.
    pub gaps_closed: Vec<String>,
    /// Gaps whose change was rolled back because the round made things worse.
    pub gaps_reverted: Vec<String>,
    /// Gaps the backend could not propose a change for.
    pub gaps_skipped: Vec<String>,
    pub drift_before: usize,
    pub drift_after: usize,
    pub build_errors_before: usize,
    pub build_errors_after: usize,
    /// What is still outstanding, so the user sees the remainder rather than a boolean.
    pub gaps_remaining: Vec<String>,
    pub log: Vec<String>,
    /// Set when the run could not start at all.
    pub error: Option<String>,
}

impl ModifyContractReport {
    fn new() -> Self {
        Self {
            success: false,
            rounds: 0,
            gaps_closed: Vec::new(),
            gaps_reverted: Vec::new(),
            gaps_skipped: Vec::new(),
            drift_before: 0,
            drift_after: 0,
            build_errors_before: 0,
            build_errors_after: 0,
            gaps_remaining: Vec::new(),
            log: Vec::new(),
            error: None,
        }
    }

    /// Human-readable result, for the UI's result pane.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        if let Some(error) = &self.error {
            out.push_str(&format!("Modify contract: {error}\n"));
        }
        if !self.gaps_closed.is_empty() {
            out.push_str(&format!("closed:\n  {}\n", self.gaps_closed.join("\n  ")));
        }
        if !self.gaps_reverted.is_empty() {
            out.push_str(&format!(
                "rolled back (the project got worse):\n  {}\n",
                self.gaps_reverted.join("\n  ")
            ));
        }
        if !self.gaps_skipped.is_empty() {
            out.push_str(&format!(
                "skipped (nothing proposed):\n  {}\n",
                self.gaps_skipped.join("\n  ")
            ));
        }
        if !self.gaps_remaining.is_empty() {
            out.push_str(&format!(
                "still outstanding:\n  {}\n",
                self.gaps_remaining.join("\n  ")
            ));
        }
        out.push_str(&format!(
            "verify: drift {} → {}, build {} → {} error(s)\n",
            self.drift_before, self.drift_after, self.build_errors_before, self.build_errors_after,
        ));
        out
    }
}

/// Adapts a [`ContractModifyBackend`] to the modify spine.
struct ContractDriver<'a> {
    backend: &'a dyn ContractModifyBackend,
    /// The round-start measurement, kept for the report's "before" figures.
    before: Mutex<Option<ContractObs>>,
    /// The live measurement, and where `targets` comes from.
    current: Mutex<ContractObs>,
    /// Which files each target's plan will write, so `apply` can back them up first.
    files: Mutex<BTreeMap<String, Vec<PathBuf>>>,
    /// The bytes each file had before this run. `None` means the file did *not exist*:
    /// a gap-fill creates implementation files, so a rollback has to remove them rather
    /// than restore them.
    backups: Mutex<BTreeMap<PathBuf, Option<String>>>,
    /// Whether a measurement has happened yet.
    measured: Mutex<bool>,
    /// Writes since the previous measurement.
    wrote_since_verify: Mutex<usize>,
    /// Set by a rollback: the disk changed, so the cached measurement is stale.
    needs_measure: Mutex<bool>,
    report: Mutex<ModifyContractReport>,
}

impl ContractDriver<'_> {
    fn into_report(self, rounds: usize) -> ModifyContractReport {
        let mut report = self.report.into_inner().unwrap();
        let before = self.before.into_inner().unwrap().unwrap_or_default();
        let after = self.current.into_inner().unwrap();

        report.rounds = rounds;
        report.drift_before = before.gaps.size();
        report.drift_after = after.gaps.size();
        report.build_errors_before = total(&before.build_errors);
        report.build_errors_after = total(&after.build_errors);
        report.gaps_remaining = after.gaps.open.iter().map(|gap| gap.id()).collect();
        report.success = after.is_clean() && report.error.is_none();
        report.log.push(format!(
            "verify: drift {} → {}, build {} → {} error(s)",
            report.drift_before,
            report.drift_after,
            report.build_errors_before,
            report.build_errors_after
        ));
        report
    }
}

impl ModifyDriver for ContractDriver<'_> {
    type Obs = ContractObs;

    fn targets(&self) -> Vec<ChangeTarget> {
        // Re-derived each round from the live measure, so a gap that closed is not
        // retried and one the fill introduced is picked up.
        self.current
            .lock()
            .unwrap()
            .gaps
            .open
            .iter()
            // A gap with nothing wrong is not a gap: a closed interface should be absent
            // from the measure, but a backend that reports it with empty lists must not
            // send the loop off to fill nothing.
            .filter(|gap| gap.size() > 0)
            .map(|gap| {
                let mut context = Vec::new();
                context.extend(gap.missing.iter().map(|m| format!("missing: {m}")));
                context.extend(gap.drifted.iter().map(|d| format!("drifted: {d}")));
                // The header is advisory: this driver applies through the fill tool, not
                // by writing the contract.
                ChangeTarget::new(
                    gap.id(),
                    PathBuf::from(format!("hal/api/{}.hpp", gap.interface)),
                )
                .with_context(context)
            })
            .collect()
    }

    async fn propose(&self, target: &ChangeTarget) -> Option<String> {
        let gap = self
            .current
            .lock()
            .unwrap()
            .gaps
            .find(&target.id)
            .cloned()?;
        let change = self.backend.plan(&gap).await?;
        self.files
            .lock()
            .unwrap()
            .insert(target.id.clone(), change.files);
        Some(change.payload)
    }

    async fn verify(&self) -> ContractObs {
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
        // Drift analysis plus a compile is not cheap, and repeating it to be told the
        // same answer costs the same. Re-measure only when the disk changed since.
        if measured && wrote == 0 && !stale {
            return self.current.lock().unwrap().clone();
        }
        let obs = ContractObs {
            gaps: self.backend.gaps().await,
            build_errors: self.backend.build().await,
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

    fn accept(&self, target: &ChangeTarget, before: &ContractObs, after: &ContractObs) -> bool {
        // Per gap: did THIS gap shrink? A round that improved a different interface while
        // leaving this one alone has not earned this change — and a gap that did not
        // exist before it was written is a regression, not a fix.
        let b = before
            .gaps
            .find(&target.id)
            .map(|gap| gap.size())
            .unwrap_or(0);
        let a = after
            .gaps
            .find(&target.id)
            .map(|gap| gap.size())
            .unwrap_or(0);
        if a >= b {
            return false;
        }
        let mut report = self.report.lock().unwrap();
        if !report.gaps_closed.iter().any(|g| g == &target.id) {
            report.gaps_closed.push(target.id.clone());
        }
        true
    }

    fn reject_round(&self, before: &ContractObs, after: &ContractObs) -> bool {
        let drift_worse = after.gaps.size() > before.gaps.size();
        let build_worse = total(&after.build_errors) > total(&before.build_errors);
        if !(drift_worse || build_worse) {
            return false;
        }
        // There is one shared contract, so a fill that helps its own interface can still
        // break another. The whole round goes.
        let mut report = self.report.lock().unwrap();
        report.log.push(format!(
            "rolled back: the round made the project worse — drift {} → {}, build {} → {} error(s)",
            before.gaps.size(),
            after.gaps.size(),
            total(&before.build_errors),
            total(&after.build_errors),
        ));
        true
    }

    async fn apply(&self, target: &ChangeTarget, content: &str) -> Result<(), String> {
        let files = self
            .files
            .lock()
            .unwrap()
            .get(&target.id)
            .cloned()
            .unwrap_or_default();
        // Back up everything the plan will write, before it writes it. A file that does
        // not exist yet is recorded as absent, so a rollback removes what the fill made
        // rather than trying to restore it.
        {
            let mut backups = self.backups.lock().unwrap();
            for path in &files {
                if !backups.contains_key(path) {
                    backups.insert(path.clone(), std::fs::read_to_string(path).ok());
                }
            }
        }
        self.backend.apply_plan(content).await?;
        *self.wrote_since_verify.lock().unwrap() += 1;
        Ok(())
    }

    async fn revert(&self, target: &ChangeTarget) -> Result<(), String> {
        let files = self
            .files
            .lock()
            .unwrap()
            .get(&target.id)
            .cloned()
            .unwrap_or_default();
        {
            let backups = self.backups.lock().unwrap();
            for path in &files {
                match backups.get(path) {
                    Some(Some(orig)) => {
                        let _ = std::fs::write(path, orig);
                    }
                    // The fill created it: putting the project back means removing it.
                    Some(None) => {
                        let _ = std::fs::remove_file(path);
                    }
                    None => {}
                }
            }
        }
        let mut report = self.report.lock().unwrap();
        if !report.gaps_reverted.iter().any(|g| g == &target.id) {
            report.gaps_reverted.push(target.id.clone());
        }
        drop(report);
        *self.needs_measure.lock().unwrap() = true;
        Ok(())
    }
}

/// Run `modify-contract`: close the HAL drift, keeping only the rounds that reduced it
/// without breaking the build.
///
/// `max_rounds` bounds the work. This loops rather than doing one pass because closing
/// one interface can reveal — or introduce — another: implementations share a contract.
pub async fn run_contract_modify(
    backend: &dyn ContractModifyBackend,
    max_rounds: usize,
) -> ModifyContractReport {
    // Measure once up front — drift AND the build — for two reasons: the spine asks for
    // targets before it verifies, and a run should act on the project's CURRENT state
    // rather than on whatever an earlier run left behind. This *is* the first
    // measurement, so `measured` starts true and the round does not repeat it.
    let seeded = ContractObs {
        gaps: backend.gaps().await,
        build_errors: backend.build().await,
    };
    let driver = ContractDriver {
        backend,
        before: Mutex::new(Some(seeded.clone())),
        current: Mutex::new(seeded),
        files: Mutex::new(BTreeMap::new()),
        backups: Mutex::new(BTreeMap::new()),
        measured: Mutex::new(true),
        wrote_since_verify: Mutex::new(0),
        needs_measure: Mutex::new(false),
        report: Mutex::new(ModifyContractReport::new()),
    };
    let spine = run_modify_loop(&driver, max_rounds).await;
    let mut report = driver.into_report(spine.rounds);
    // What the backend could not propose anything for. The spine already knows, so this
    // is a move rather than a second bookkeeping.
    report.gaps_skipped = spine.skipped;
    report
}

/// The drift measure, read from the real HAL coverage analysis.
///
/// This is the mapping between what the project actually contains and the measure the
/// spine judges rounds on, and it is deliberately part of this module rather than the
/// coordinator: the coverage analysis and the fill are plain functions over a project
/// directory, so the cascade's most important seam can be exercised against a real
/// project tree with no actors, no build, and no model.
pub fn hal_gaps(root: &std::path::Path, platform: &str) -> Gaps {
    let coverage = crate::build::generic_helpers::hal_platform_coverage_map(root);
    let mut open = Vec::new();
    for (plat, interfaces) in &coverage {
        if !platform.is_empty() && plat != platform {
            continue;
        }
        for (interface, cov) in interfaces {
            // `implemented` is false for a stub as well as for a missing file: the
            // SPIRE-HAL-STUB sentinel exists so coverage can tell "no implementation"
            // apart from "declared, but does nothing yet". Both are work.
            if cov.implemented {
                continue;
            }
            open.push(Gap {
                platform: plat.clone(),
                interface: interface.clone(),
                missing: cov.missing_sigs.iter().map(|m| m.name.clone()).collect(),
                drifted: Vec::new(),
                stub: cov.has_impl,
            });
        }
    }
    Gaps { open }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Scripts the drift measure, the build, and the fill.
    ///
    /// Each measurement call advances the script while entries remain, so a run that
    /// measures more often than it should sees the last scripted answer again.
    struct Fake {
        gaps: StdMutex<Vec<Gaps>>,
        builds: StdMutex<Vec<ErrorsByFile>>,
        /// The file each fill writes. The payload names it, which is what makes
        /// `apply_plan` real enough for a rollback to have something to undo.
        target: PathBuf,
        applied: StdMutex<usize>,
    }

    fn step<T: Clone + Default>(script: &StdMutex<Vec<T>>) -> T {
        let mut script = script.lock().unwrap();
        let value = script.first().cloned().unwrap_or_default();
        if script.len() > 1 {
            script.remove(0);
        }
        value
    }

    impl Fake {
        fn new(gaps: Vec<Gaps>, builds: Vec<ErrorsByFile>, target: PathBuf) -> Self {
            Self {
                gaps: StdMutex::new(gaps),
                builds: StdMutex::new(builds),
                target,
                applied: StdMutex::new(0),
            }
        }

        fn applied(&self) -> usize {
            *self.applied.lock().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl ContractModifyBackend for Fake {
        async fn gaps(&self) -> Gaps {
            step(&self.gaps)
        }

        async fn plan(&self, _gap: &Gap) -> Option<ContractChange> {
            Some(ContractChange {
                files: vec![self.target.clone()],
                payload: self.target.to_string_lossy().to_string(),
            })
        }

        async fn apply_plan(&self, payload: &str) -> Result<(), String> {
            std::fs::write(payload, "/* filled */\n").map_err(|e| e.to_string())?;
            *self.applied.lock().unwrap() += 1;
            Ok(())
        }

        async fn build(&self) -> ErrorsByFile {
            step(&self.builds)
        }
    }

    fn gap(platform: &str, iface: &str, missing: usize, drifted: usize) -> Gap {
        Gap {
            platform: platform.to_string(),
            interface: iface.to_string(),
            missing: (0..missing).map(|i| format!("m{i}")).collect(),
            drifted: (0..drifted).map(|i| format!("d{i}")).collect(),
            stub: false,
        }
    }

    fn gaps(list: Vec<Gap>) -> Gaps {
        Gaps { open: list }
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

    /// A project directory with the implementation file the fill writes.
    fn project(existing: bool) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let impl_file = tmp.path().join("iface_impl.cpp");
        if existing {
            std::fs::write(&impl_file, "// original\n").unwrap();
        }
        (tmp, impl_file)
    }

    /// A fill that closes the gap and keeps the build green is kept, and the file it
    /// wrote stays written.
    #[tokio::test]
    async fn closes_a_gap_and_keeps_it() {
        let (_tmp, impl_file) = project(true);
        let backend = Fake::new(
            vec![gaps(vec![gap("rpi5", "camera", 2, 1)]), gaps(vec![])],
            vec![errs(&[]), errs(&[])],
            impl_file.clone(),
        );
        let report = run_contract_modify(&backend, 3).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.gaps_closed, vec!["rpi5/camera"]);
        assert!(report.gaps_reverted.is_empty());
        assert_eq!((report.drift_before, report.drift_after), (3, 0));
        assert_eq!(report.rounds, 1);
        assert_eq!(
            std::fs::read_to_string(&impl_file).unwrap(),
            "/* filled */\n"
        );
    }

    /// A fill that leaves the gap just as big has earned nothing, and is undone.
    #[tokio::test]
    async fn rolls_back_a_change_that_does_not_close_the_gap() {
        let (_tmp, impl_file) = project(true);
        let unchanged = gaps(vec![gap("rpi5", "camera", 2, 0)]);
        let backend = Fake::new(
            vec![unchanged.clone(), unchanged],
            vec![errs(&[]), errs(&[])],
            impl_file.clone(),
        );
        let report = run_contract_modify(&backend, 3).await;

        assert!(!report.success);
        assert_eq!(report.gaps_reverted, vec!["rpi5/camera"]);
        assert!(report.gaps_closed.is_empty());
        assert_eq!(
            std::fs::read_to_string(&impl_file).unwrap(),
            "// original\n",
            "the original bytes are restored"
        );
    }

    /// A fill that closes its gap but breaks the build is undone anyway: there is one
    /// shared contract, and a green gap is not worth a red build.
    #[tokio::test]
    async fn rolls_back_a_change_that_breaks_the_build() {
        let (_tmp, impl_file) = project(true);
        let backend = Fake::new(
            vec![gaps(vec![gap("rpi5", "camera", 1, 0)]), gaps(vec![])],
            vec![errs(&[]), errs(&[("camera_impl.cpp", 2)])],
            impl_file.clone(),
        );
        let report = run_contract_modify(&backend, 3).await;

        assert!(!report.success, "{report:?}");
        assert_eq!(report.gaps_reverted, vec!["rpi5/camera"]);
        assert_eq!(report.build_errors_after, 2);
        assert_eq!(
            std::fs::read_to_string(&impl_file).unwrap(),
            "// original\n"
        );
    }

    /// A gap-fill *creates* implementation files. When the round is rolled back, the
    /// file it created has to go — restoring "nothing" means removing it, not leaving an
    /// orphan behind.
    #[tokio::test]
    async fn removes_a_file_the_fill_created() {
        let (_tmp, impl_file) = project(false);
        assert!(!impl_file.exists(), "the file starts absent");

        let unchanged = gaps(vec![gap("rpi5", "camera", 1, 0)]);
        let backend = Fake::new(
            vec![unchanged.clone(), unchanged],
            vec![errs(&[]), errs(&[])],
            impl_file.clone(),
        );
        let report = run_contract_modify(&backend, 3).await;

        assert_eq!(report.gaps_reverted, vec!["rpi5/camera"]);
        assert_eq!(backend.applied(), 1, "the fill really did run");
        assert!(
            !impl_file.exists(),
            "the file the fill created was removed again"
        );
    }

    /// Closing one interface while introducing another is a worse project, so the round
    /// goes — the guard is the whole measure, not the one gap being worked on.
    #[tokio::test]
    async fn rolls_back_a_round_that_grows_the_drift() {
        let (_tmp, impl_file) = project(true);
        let backend = Fake::new(
            vec![
                gaps(vec![gap("rpi5", "camera", 1, 0)]),
                gaps(vec![
                    gap("rpi5", "camera", 0, 0),
                    gap("rpi5", "audio", 2, 0),
                ]),
            ],
            vec![errs(&[]), errs(&[])],
            impl_file.clone(),
        );
        let report = run_contract_modify(&backend, 3).await;

        assert!(!report.success, "{report:?}");
        assert!(
            report.gaps_reverted.contains(&"rpi5/camera".to_string()),
            "the round that grew the drift was undone: {report:?}"
        );
        assert!(report.gaps_closed.is_empty());
        assert_eq!((report.drift_before, report.drift_after), (1, 2));
        assert_eq!(
            std::fs::read_to_string(&impl_file).unwrap(),
            "// original\n"
        );
    }

    /// Nothing drifted means nothing to do: no change, no build, and a clean report.
    #[tokio::test]
    async fn does_nothing_when_there_is_no_drift() {
        let (_tmp, impl_file) = project(true);
        let backend = Fake::new(vec![gaps(vec![])], vec![errs(&[])], impl_file.clone());
        let report = run_contract_modify(&backend, 3).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.rounds, 0);
        assert_eq!(report.gaps_skipped.len(), 0);
        assert_eq!(backend.applied(), 0, "nothing was filled");
        assert_eq!(report.summary().contains("drift 0 → 0"), true, "{report:?}");
    }

    /// The measure and the fill against a **real** HAL project tree: no fake, no actors,
    /// no model.
    ///
    /// This is the seam the whole cascade rests on — `hal_gaps` mapping the coverage
    /// analysis onto the measure, and the fill that `propose`/`apply` drive — and it is
    /// the one part of the flow that can be exercised end to end without a model or a
    /// board. Until now it never had been, so this is the first time the cascade has
    /// touched the real thing.
    #[tokio::test]
    async fn measures_and_fills_a_real_hal_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal").join("api")).unwrap();
        std::fs::create_dir_all(root.join("hal").join("implementations").join("rpi5")).unwrap();
        std::fs::write(
            root.join("hal").join("api").join("camera.hpp"),
            "#pragma once\n\nclass CameraHal {\npublic:\n    virtual ~CameraHal() = default;\n    \
             virtual void start() = 0;\n    virtual int frame_count() const = 0;\n};\n",
        )
        .unwrap();

        // Nothing is implemented: the interface is a gap with both methods outstanding.
        let before = hal_gaps(root, "rpi5");
        let gap = before
            .open
            .iter()
            .find(|g| g.interface == "camera")
            .unwrap_or_else(|| panic!("the contract should be a gap: {before:?}"));
        assert!(gap.missing.len() >= 2, "both methods are missing: {gap:?}");
        assert!(!gap.stub, "there is no implementation file at all");
        let drift_before = before.size();

        // Fill it for real: the plan `propose` would return, and the apply `apply` runs.
        let planned = crate::actors::hal_fill::plan(root, "rpi5", &["camera".to_string()]);
        let items = planned
            .get("plan")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            !items.is_empty(),
            "the fill should have work to do: {planned}"
        );

        let applied = crate::actors::hal_fill::apply(
            root,
            &serde_json::Value::Array(items),
            Box::pin(async { Ok(()) }),
        )
        .await;
        assert_eq!(
            applied
                .get("failures")
                .and_then(|v| v.as_array())
                .map(|f| f.len()),
            Some(0),
            "the fill reported failures: {applied}"
        );

        // The implementation exists now — but it is a STUB, and that is the finding this
        // test exists for: the deterministic fill scaffolds, it does not implement.
        let after = hal_gaps(root, "rpi5");
        let gap = after
            .open
            .iter()
            .find(|g| g.interface == "camera")
            .unwrap_or_else(|| panic!("a stub is not an implementation: {after:?}"));
        assert!(gap.stub, "the file is there and marked pending: {gap:?}");
        assert!(
            !gap.missing.is_empty(),
            "a stub body satisfies none of the contract's methods: {gap:?}"
        );
        assert_eq!(
            after.size(),
            drift_before,
            "so the measure does not fall, and the cascade would roll this round back \
             rather than accept it — closing drift needs the LLM-backed generation \
             (hal_generate_impl), not the scaffold"
        );

        // And what landed on disk really is the marked scaffold.
        let written = std::fs::read_dir(root.join("hal").join("implementations").join("rpi5"))
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            written.contains(crate::build::generic_helpers::SPIRE_HAL_STUB_SENTINEL),
            "the fill writes the pending sentinel"
        );
    }
}
