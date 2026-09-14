// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The general **modify existing code** spine: propose → apply → verify → keep or
//! roll back.
//!
//! Modifying code that already works is an LLM job, and unattended application is
//! only acceptable if it is *verified* and *reversible*. That shape is the same
//! whatever is being changed — a toolkit actor, a pipeline stage, a HAL contract —
//! so it lives here once rather than beside each caller:
//!
//! * the **prompt** is whatever justifies the change: compiler diagnostics, a
//!   drift report, or the user's own words;
//! * every round of writes is followed by a **verification**, and a change that did
//!   not improve the measured condition is **restored byte-for-byte**;
//! * each target that fails is attempted only once (no oscillation), and a run is
//!   bounded — but a target that *improved* stays eligible, because a partial fix
//!   is worth continuing (see [`run_modify_loop`]);
//!
//! Only the *acceptance rule* is domain-specific, and the driver owns it — whether a
//! single change earned its place ([`ModifyDriver::accept`]), and whether the whole
//! round must be undone regardless ([`ModifyDriver::reject_round`]).
//! `build/autofix.rs` keeps a file only when its error count strictly went down, and
//! throws the round away when the project total rose; a HAL contract change will
//! require "no drift, contract valid, and it builds".
//!
//! `build/autofix.rs` is the reference instance this was extracted from — it is
//! still on its own implementation, and migrating it onto this loop is the
//! follow-up (kept separate, so the working Fix & Verify was not destabilised in
//! the same step that introduced the abstraction).

use std::path::PathBuf;

/// One thing the loop may rewrite, plus the context that justifies it.
///
/// `context` is free-form so a driver can carry whatever the model needs
/// (diagnostic lines, the current contract signature, the user's prompt).
#[derive(Debug, Clone)]
pub struct ChangeTarget {
    /// Stable identity — a path, an interface name, or anything else unique.
    pub id: String,
    /// The file this target rewrites.
    pub path: PathBuf,
    /// Why this target exists, for the prompt builder.
    pub context: Vec<String>,
}

impl ChangeTarget {
    pub fn new(id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            context: Vec::new(),
        }
    }

    pub fn with_context(mut self, context: Vec<String>) -> Self {
        self.context = context;
        self
    }
}

/// What a verification observed.
///
/// Deliberately opaque to the loop: only the driver can compare two of these,
/// because only the driver knows what "better" means for the code it owns.
pub trait Observation {
    /// No outstanding problems — nothing left for the loop to do.
    fn is_clean(&self) -> bool;
}

/// The domain half of the loop: what to change, how to change it, and what
/// counts as an improvement.
///
/// `async fn` in a trait is fine here because the loop is generic (`D:
/// ModifyDriver`), never `dyn` — the lint's concern is auto-trait bounds for
/// trait objects, which never arises.
#[allow(async_fn_in_trait)]
pub trait ModifyDriver {
    type Obs: Observation;

    /// Targets still needing work. Re-derived each round, so a change that
    /// introduced new targets is picked up and one that removed them is not
    /// retried.
    fn targets(&self) -> Vec<ChangeTarget>;

    /// A full replacement for `target`, or `None` when the model declines to
    /// touch it (which is a skip, not a failure).
    async fn propose(&self, target: &ChangeTarget) -> Option<String>;

    /// Observe the code as it stands now — once before a round's writes, and again
    /// after them.
    async fn verify(&self) -> Self::Obs;

    /// Whether this write earned its place. The driver's rule, not the loop's.
    fn accept(&self, target: &ChangeTarget, before: &Self::Obs, after: &Self::Obs) -> bool;

    /// Whether the **whole round** must be undone, including the writes `accept`
    /// would have kept.
    ///
    /// Off by default: the spine has no opinion about the project as a whole, and a
    /// per-target rule is the common case. A driver that measures globally opts in —
    /// `build/autofix.rs` uses it so a round that raised the total error count is
    /// rolled back in full, including the files that individually improved, because
    /// a build is judged as a whole.
    fn reject_round(&self, _before: &Self::Obs, _after: &Self::Obs) -> bool {
        false
    }

    /// Write `content` to the target's file, keeping whatever the driver needs
    /// to restore the pre-run bytes.
    async fn apply(&self, target: &ChangeTarget, content: &str) -> Result<(), String>;

    /// Restore the pre-run bytes for `target`.
    async fn revert(&self, target: &ChangeTarget) -> Result<(), String>;
}

/// What a run did, and whether it left the code clean.
#[derive(Debug, Default)]
pub struct ModifyReport {
    /// Rounds that actually applied at least one change. A round where every
    /// proposal was declined or unwritable changed nothing, so it does not count.
    pub rounds: usize,
    /// Every target the loop attempted, in order (kept + reverted).
    pub attempted: Vec<String>,
    pub kept: Vec<String>,
    pub reverted: Vec<String>,
    pub skipped: Vec<String>,
    /// True when the final verification reported nothing outstanding.
    pub success: bool,
    pub log: Vec<String>,
}

impl ModifyReport {
    /// Human-readable summary, for the result the UI shows.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        if !self.kept.is_empty() {
            out.push_str(&format!("changes kept:\n  {}\n", self.kept.join("\n  ")));
        }
        if !self.reverted.is_empty() {
            out.push_str(&format!(
                "changes rolled back (no net gain):\n  {}\n",
                self.reverted.join("\n  ")
            ));
        }
        if !self.skipped.is_empty() {
            out.push_str(&format!("skipped:\n  {}\n", self.skipped.join("\n  ")));
        }
        out.push_str(if self.success {
            "verify: clean\n"
        } else {
            "verify: problems remain\n"
        });
        out
    }
}

/// Run the loop: each round measures once, writes every pending target, measures
/// again, then keeps what the driver accepted and rolls back the rest.
///
/// The **round** is the unit of change, not the target. That is what lets a driver
/// judge a round as a whole ([`ModifyDriver::reject_round`]), and it keeps the cost
/// at one verification per round rather than one per target — which matters when a
/// verification is a compile.
///
/// `max_rounds` bounds the work. A target that is skipped or rolled back is never
/// retried; one that was **kept** stays eligible, because an improving change may
/// not be finished. Each such retry strictly improves the measure, so it
/// converges rather than oscillating.
pub async fn run_modify_loop<D: ModifyDriver>(driver: &D, max_rounds: usize) -> ModifyReport {
    let mut report = ModifyReport::default();
    let mut tried: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for round in 0..max_rounds.max(1) {
        let pending: Vec<ChangeTarget> = driver
            .targets()
            .into_iter()
            .filter(|target| !tried.contains(&target.id))
            .collect();
        if pending.is_empty() {
            break;
        }
        report
            .log
            .push(format!("round {}: {} target(s)", round + 1, pending.len()));

        // Measure the world, write every pending target, then measure it again. The
        // before/after pair belongs to the whole round, so `accept` reads the same two
        // observations `reject_round` does — narrowed to one target.
        let before = driver.verify().await;

        let mut wrote: Vec<ChangeTarget> = Vec::new();
        for target in pending {
            report.attempted.push(target.id.clone());

            let Some(content) = driver.propose(&target).await else {
                tried.insert(target.id.clone()); // declined: never retried
                report.log.push(format!("skip {}: no proposal", target.id));
                report.skipped.push(target.id.clone());
                continue;
            };
            if let Err(err) = driver.apply(&target, &content).await {
                tried.insert(target.id.clone());
                report.log.push(format!("skip {}: {err}", target.id));
                report.skipped.push(target.id.clone());
                continue;
            }
            wrote.push(target);
        }
        if wrote.is_empty() {
            break; // nothing was written, so there is nothing to verify or decide
        }
        // A round counts as one only when it actually changed something: a round in
        // which every proposal was declined or unwritable did not. This is the same
        // definition `AutofixReport::rounds` uses, so the two reports line up.
        report.rounds = round + 1;

        let after = driver.verify().await;

        // A rejected round is undone in full — including the writes `accept` would
        // have kept — and none of it is retried.
        if driver.reject_round(&before, &after) {
            for target in &wrote {
                if let Err(err) = driver.revert(target).await {
                    report
                        .log
                        .push(format!("revert {} failed: {err}", target.id));
                }
                tried.insert(target.id.clone());
                if !report.reverted.contains(&target.id) {
                    report.reverted.push(target.id.clone());
                }
            }
            report.log.push(format!(
                "round {} rejected: rolled back {} change(s)",
                round + 1,
                wrote.len()
            ));
            continue;
        }

        for target in wrote {
            if driver.accept(&target, &before, &after) {
                // Kept — and deliberately left eligible for the next round: a
                // change that improved things may not be finished, and taking it
                // further is exactly what verification said is still needed. Each
                // such retry strictly improves the measure, so it converges (and
                // `max_rounds` bounds it regardless).
                if !report.kept.contains(&target.id) {
                    report.kept.push(target.id.clone());
                }
            } else {
                // No gain, or a regression: restore it and never retry.
                if let Err(err) = driver.revert(&target).await {
                    report
                        .log
                        .push(format!("revert {} failed: {err}", target.id));
                }
                tried.insert(target.id.clone());
                report
                    .log
                    .push(format!("reverted {}: no net gain", target.id));
                if !report.reverted.contains(&target.id) {
                    report.reverted.push(target.id.clone());
                }
            }
        }
    }

    report.success = driver.verify().await.is_clean();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Observation: how many problems each target has. Clean = all zero.
    #[derive(Clone, Default)]
    struct Problems(BTreeMap<String, usize>);

    impl Observation for Problems {
        fn is_clean(&self) -> bool {
            self.0.values().all(|&n| n == 0)
        }
    }

    /// A driver over an in-memory file set, so the loop's decisions are provable
    /// without a build or an LLM. Each `verify()` pops the next observation from
    /// the script, and falls back to `last` once the script runs out.
    struct Fake {
        ids: Vec<String>,
        /// The world as it currently stands — seeded from `initial` and replaced
        /// by every `verify()`. `targets()` reads this, exactly like the real
        /// drivers read the graph: an already-clean file is not a target.
        current: Mutex<BTreeMap<String, usize>>,
        script: Mutex<Vec<BTreeMap<String, usize>>>,
        last: BTreeMap<String, usize>,
        proposals: BTreeMap<String, String>,
        applied: Mutex<BTreeMap<String, String>>,
        /// When set, the fake judges the round the way autofix does — as a whole —
        /// instead of only per target.
        project_guard: bool,
    }

    impl Fake {
        fn new(
            ids: &[&str],
            initial: BTreeMap<String, usize>,
            script: Vec<BTreeMap<String, usize>>,
            last: BTreeMap<String, usize>,
        ) -> Self {
            Self {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                current: Mutex::new(initial),
                script: Mutex::new(script),
                last,
                proposals: BTreeMap::new(),
                applied: Mutex::new(BTreeMap::new()),
                project_guard: false,
            }
        }

        fn proposing(mut self, id: &str, content: &str) -> Self {
            self.proposals.insert(id.to_string(), content.to_string());
            self
        }

        /// Opt into autofix's project-level rule: reject the round when the total rose,
        /// even if a file inside it improved.
        fn guarding_project(mut self) -> Self {
            self.project_guard = true;
            self
        }

        fn on_disk(&self, id: &str) -> String {
            self.applied
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("original-{id}"))
        }
    }

    impl ModifyDriver for Fake {
        type Obs = Problems;

        fn targets(&self) -> Vec<ChangeTarget> {
            let current = self.current.lock().unwrap();
            self.ids
                .iter()
                .filter(|id| current.get(*id).copied().unwrap_or(0) > 0)
                .map(|id| ChangeTarget::new(id.clone(), format!("/tmp/{id}")))
                .collect()
        }

        async fn propose(&self, target: &ChangeTarget) -> Option<String> {
            self.proposals.get(&target.id).cloned()
        }

        async fn verify(&self) -> Problems {
            let next = {
                let mut script = self.script.lock().unwrap();
                if script.is_empty() {
                    self.last.clone()
                } else {
                    script.remove(0)
                }
            };
            *self.current.lock().unwrap() = next.clone();
            Problems(next)
        }

        fn accept(&self, target: &ChangeTarget, before: &Problems, after: &Problems) -> bool {
            let b = before.0.get(&target.id).copied().unwrap_or(0);
            let a = after.0.get(&target.id).copied().unwrap_or(0);
            a < b
        }

        fn reject_round(&self, before: &Problems, after: &Problems) -> bool {
            if !self.project_guard {
                return false; // the default rule: per target only
            }
            let total = |p: &Problems| p.0.values().sum::<usize>();
            total(after) > total(before)
        }

        async fn apply(&self, target: &ChangeTarget, content: &str) -> Result<(), String> {
            self.applied
                .lock()
                .unwrap()
                .insert(target.id.clone(), content.to_string());
            Ok(())
        }

        async fn revert(&self, target: &ChangeTarget) -> Result<(), String> {
            self.applied.lock().unwrap().remove(&target.id);
            Ok(())
        }
    }

    fn obs(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// A change that reduces the problem count is kept, and the clean final state
    /// is reported.
    #[tokio::test]
    async fn keeps_a_change_that_helps() {
        let driver = Fake::new(
            &["a"],
            obs(&[("a", 3)]),
            vec![obs(&[("a", 3)]), obs(&[("a", 0)])],
            obs(&[("a", 0)]),
        )
        .proposing("a", "fixed");
        let report = run_modify_loop(&driver, 5).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.kept, vec!["a"]);
        assert!(report.reverted.is_empty());
        assert_eq!(report.rounds, 1);
        assert_eq!(driver.on_disk("a"), "fixed");
    }

    /// A change that does not reduce the problem count is rolled back.
    #[tokio::test]
    async fn rolls_back_a_change_that_does_not_help() {
        let driver = Fake::new(
            &["a"],
            obs(&[("a", 2)]),
            vec![obs(&[("a", 2)]), obs(&[("a", 2)])],
            obs(&[("a", 2)]),
        )
        .proposing("a", "nonsense");
        let report = run_modify_loop(&driver, 5).await;

        assert!(!report.success);
        assert_eq!(report.reverted, vec!["a"]);
        assert!(report.kept.is_empty());
        assert_eq!(driver.on_disk("a"), "original-a", "the write was undone");
    }

    /// A regression (more problems than before) is rolled back too.
    #[tokio::test]
    async fn rolls_back_a_regression() {
        let driver = Fake::new(
            &["a"],
            obs(&[("a", 1)]),
            vec![obs(&[("a", 1)]), obs(&[("a", 4)])],
            obs(&[("a", 4)]),
        )
        .proposing("a", "worse");
        let report = run_modify_loop(&driver, 5).await;

        assert_eq!(report.reverted, vec!["a"]);
        assert_eq!(driver.on_disk("a"), "original-a");
    }

    /// No proposal is a skip, not a failure, and the target is not retried.
    #[tokio::test]
    async fn skips_a_target_with_no_proposal() {
        let driver = Fake::new(
            &["a"],
            obs(&[("a", 1)]),
            vec![obs(&[("a", 1)])],
            obs(&[("a", 1)]),
        );
        let report = run_modify_loop(&driver, 5).await;

        assert_eq!(report.skipped, vec!["a"]);
        assert!(report.kept.is_empty() && report.reverted.is_empty());
        assert_eq!(report.rounds, 0, "nothing was written, so no round counted");
    }

    /// A change that gains nothing is rolled back and NOT retried, so a run
    /// cannot oscillate on a target the model keeps getting wrong.
    #[tokio::test]
    async fn does_not_retry_a_failed_change() {
        let driver = Fake::new(
            &["a"],
            obs(&[("a", 1)]),
            vec![obs(&[("a", 1)]), obs(&[("a", 1)])],
            obs(&[("a", 1)]),
        )
        .proposing("a", "still broken");
        let report = run_modify_loop(&driver, 5).await;

        assert_eq!(report.attempted, vec!["a"], "listed once: {report:?}");
        assert_eq!(report.rounds, 1);
    }

    /// An improving change that does not FINISH the job is kept *and* retried in
    /// the next round, so the run converges — the behaviour `build/autofix.rs`
    /// depends on (its own test pins 3 → 1 → 0 in two rounds).
    #[tokio::test]
    async fn keeps_retrying_an_improving_target_until_clean() {
        let driver = Fake::new(
            &["a"],
            obs(&[("a", 3)]),
            vec![
                obs(&[("a", 3)]),
                obs(&[("a", 1)]),
                obs(&[("a", 1)]),
                obs(&[("a", 0)]),
            ],
            obs(&[("a", 0)]),
        )
        .proposing("a", "better");
        let report = run_modify_loop(&driver, 5).await;

        assert!(report.success, "{report:?}");
        assert_eq!(report.rounds, 2, "3 → 1 → 0: {report:?}");
        assert_eq!(report.kept, vec!["a"], "listed once, not twice");
        assert!(report.reverted.is_empty());
        assert_eq!(driver.on_disk("a"), "better");
    }

    /// The hook: a driver that measures the project as a whole can throw a round away
    /// even when a file inside it improved. This is the spine-side form of what
    /// `autofix::tests::rolls_back_the_whole_round_when_the_project_gets_worse` pins,
    /// and the reason `reject_round` exists.
    #[tokio::test]
    async fn a_rejected_round_undoes_even_an_improving_target() {
        // a improves (3 → 2) but b regresses (1 → 5). The total rises, so the whole
        // round goes — including a's fix, which `accept` alone would have kept.
        let driver = Fake::new(
            &["a", "b"],
            obs(&[("a", 3), ("b", 1)]),
            vec![
                obs(&[("a", 3), ("b", 1)]), // before the round
                obs(&[("a", 2), ("b", 5)]), // after it
            ],
            obs(&[("a", 2), ("b", 5)]),
        )
        .proposing("a", "better")
        .proposing("b", "worse")
        .guarding_project();
        let report = run_modify_loop(&driver, 5).await;

        assert!(report.kept.is_empty(), "the round was rejected: {report:?}");
        assert_eq!(report.reverted, vec!["a", "b"], "both undone: {report:?}");
        assert_eq!(
            driver.on_disk("a"),
            "original-a",
            "a's improvement is undone too"
        );
        assert_eq!(driver.on_disk("b"), "original-b");
        assert_eq!(report.rounds, 1, "and nothing is retried: {report:?}");
        assert!(!report.success);
    }
}
