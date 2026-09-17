//! **The verify spine**: gate -> build -> hand the compiler's errors back to whatever generated the
//! code -> rebuild, bounded.
//!
//! Two places already did this and were written apart: the embedded-HAL fill leg (it builds each
//! backend it wrote and repairs with the compiler's words) and, less completely, the scaffold's
//! deterministic plan gate (parse + host build, no feedback). The shape is the same and it is the
//! shape every `prompt -> generate -> verify` feature needs, so it lives here once:
//!
//! - **gate** — structural checks before anything is built. Cheap, and it catches the answer that was
//!   never a file to begin with (an empty answer, a placeholder left in).
//! - **build** — the only authority on whether the code compiles. A failure carries the compiler's
//!   own output, verbatim: paraphrase would lose the line numbers and type names that are the whole
//!   point of feeding it back.
//! - **repair** — one more generation, with those errors in the prompt. `Ok` means a new artifact is
//!   on disk and a rebuild is worth attempting; `Err` means the repair itself was refused (the gate,
//!   a missing model), and then the loop **stops** rather than burning rounds on a generator that
//!   cannot produce an acceptable answer.
//! - **bounded** — `max_rounds` is a cost ceiling, not an aspiration. Round 1 is the original
//!   attempt; each additional round is one model call. A leftover failure is reported with the
//!   compiler's words attached, which is strictly better than a silent one.
//!
//! The distinction that makes it safe to automate: a **compiler** failure is the generator's to fix
//! (a wrong vendor API, a missing import), while a **setup** failure — no build module for this
//! platform, a toolchain that is not installed, a channel that closed — is nobody's to fix by
//! writing code. Feeding the second kind back would spend a model call to be told nothing, so the
//! spine stops and reports it as "not built" rather than as "does not compile". `built` is an
//! `Option` for exactly that reason: `None` means no compiler ran.
//!
//! A trait rather than closures: a closure returning a future that borrows its captured environment
//! does not survive an `Fn() -> impl Future` bound in this edition (tried — it is why
//! `build_backend_crate` is a method). A trait is also what makes the loop testable with no model, no
//! toolchain and no filesystem: the tests below are fakes that fail twice and then succeed.

use async_trait::async_trait;
use serde_json::json;

/// Why a build did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuildFailure {
    /// The compiler's own output. The generator can act on this.
    Compiler(String),
    /// The build could not be attempted or completed. **Never handed to the generator**: no answer
    /// can fix an environment, and feeding it back would claim a repair that could not help.
    Setup(String),
}

/// One attempt's verdict, as every caller wants to report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// `None` when no compiler ran — a gate refusal or a setup failure. **Not** the same as
    /// `Some(false)`, which means a compiler ran and rejected the artifact.
    pub built: Option<bool>,
    /// How many repair rounds were spent — 0 when the first build passed.
    pub rounds: u32,
    /// The **last** compiler output. Empty when nothing failed, and when nothing was built.
    pub errors: String,
    /// Why a repair was not attempted or the loop did not continue (a refused answer, a gate
    /// failure). `None` means the loop ran to its own conclusion.
    pub refused: Option<String>,
    /// Why no build happened, when that is the case.
    pub not_built: Option<String>,
}

impl VerifyOutcome {
    /// The outcome as JSON, in the shape the callers already report.
    ///
    /// `repaired` stays a bool (the fill leg's UI reads it) *and* `rounds` is added: "it built after
    /// one repair" and "it built after three" are different facts about the same success. `built`
    /// keeps its three-valued meaning, which is why the fill result could already say `null`.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "built": self.built,
            "repaired": self.rounds > 0,
            "rounds": self.rounds,
            "errors": self.errors,
            "refused": self.refused,
            "not_built": self.not_built,
        })
    }
}

/// What the spine needs from whatever generated the artifact.
///
/// `Send + Sync` so a caller can hold it across awaits inside an actor; `async_trait` because an
/// `impl Future` method in a trait is not object-safe here and the alternative — generic closures —
/// cannot borrow (see the module note).
#[async_trait]
pub(crate) trait GeneratedArtifact {
    /// A short name for this artifact in messages, e.g. the file's path.
    fn describe(&self) -> String;

    /// Structural gate, before any build: is this the artifact it claims to be?
    fn gate(&self) -> Result<(), String>;

    /// Build it. [`BuildFailure::Compiler`] carries the compiler's output verbatim.
    async fn build(&self) -> Result<(), BuildFailure>;

    /// Ask the generator for one more answer, with the compiler's errors. `Ok` means something new
    /// is on disk; `Err` is why no repair happened.
    async fn repair(&self, errors: &str) -> Result<(), String>;
}

/// Run the spine: gate, build, and on failure repair and rebuild — at most `max_rounds` repairs.
///
/// `max_rounds == 0` is legal and means "build once, report" — the honest configuration for a caller
/// with no model to repair with, rather than a special case at every call site.
pub(crate) async fn verify_generated(
    artifact: &impl GeneratedArtifact,
    max_rounds: u32,
) -> VerifyOutcome {
    let not_built = |reason: String| VerifyOutcome {
        built: None,
        rounds: 0,
        errors: String::new(),
        refused: Some(reason),
        not_built: None,
    };

    if let Err(reason) = artifact.gate() {
        return not_built(format!("{}: {reason}", artifact.describe()));
    }

    let mut errors = String::new();
    for attempt in 0..=max_rounds {
        match artifact.build().await {
            Ok(()) => {
                return VerifyOutcome {
                    built: Some(true),
                    rounds: attempt,
                    errors,
                    refused: None,
                    not_built: None,
                }
            }
            // Not the generator's to fix: stop before spending a round on it.
            Err(BuildFailure::Setup(reason)) => {
                return VerifyOutcome {
                    built: None,
                    rounds: attempt,
                    errors: String::new(),
                    refused: None,
                    not_built: Some(reason),
                }
            }
            Err(BuildFailure::Compiler(output)) => errors = output,
        }

        // The last attempt's failure is the one reported: there is no round left to fix it, and the
        // caller needs the compiler's words rather than a claim about them.
        if attempt == max_rounds {
            break;
        }
        if let Err(refused) = artifact.repair(&errors).await {
            return VerifyOutcome {
                built: Some(false),
                rounds: attempt,
                errors,
                refused: Some(refused),
                not_built: None,
            };
        }
    }

    VerifyOutcome {
        built: Some(false),
        rounds: max_rounds,
        errors,
        refused: None,
        not_built: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    /// A generator that gets it wrong a fixed number of times and then succeeds — the shape a real
    /// run takes when the vendor API is guessed wrong, written so the test needs no model, no
    /// compiler and no files.
    struct Fake {
        /// How many builds fail before one passes (`u32::MAX` = never passes).
        fail_builds: u32,
        builds: AtomicU32,
        repairs: AtomicU32,
        /// The errors each repair was handed, in order.
        repaired: Mutex<Vec<String>>,
        gate: Result<(), String>,
        /// Refuse the repair from this round on (1 = the first repair).
        refuse_repair_from: AtomicU32,
        /// Fail the build as a *setup* problem rather than a compiler error.
        setup_failure: Option<String>,
    }

    impl Fake {
        fn failing(fail_builds: u32) -> Self {
            Self {
                fail_builds,
                builds: AtomicU32::new(0),
                repairs: AtomicU32::new(0),
                repaired: Mutex::new(Vec::new()),
                gate: Ok(()),
                refuse_repair_from: AtomicU32::new(u32::MAX),
                setup_failure: None,
            }
        }

        fn passed() -> Self {
            Self::failing(0)
        }
    }

    #[async_trait]
    impl GeneratedArtifact for Fake {
        fn describe(&self) -> String {
            "crates/demo/src/lib.rs".to_string()
        }

        fn gate(&self) -> Result<(), String> {
            self.gate.clone()
        }

        async fn build(&self) -> Result<(), BuildFailure> {
            let n = self.builds.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_builds {
                if let Some(reason) = &self.setup_failure {
                    return Err(BuildFailure::Setup(reason.clone()));
                }
                Err(BuildFailure::Compiler(format!(
                    "error[E0107]: build {n} does not compile"
                )))
            } else {
                Ok(())
            }
        }

        async fn repair(&self, errors: &str) -> Result<(), String> {
            let n = self.repairs.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= self.refuse_repair_from.load(Ordering::SeqCst) {
                return Err("refused: the answer is still a placeholder".to_string());
            }
            self.repaired.lock().unwrap().push(errors.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_first_time_build_spends_no_repair_and_reports_what_it_saw() {
        let fake = Fake::passed();
        let outcome = verify_generated(&fake, 3).await;
        assert_eq!(outcome.built, Some(true));
        assert_eq!(outcome.rounds, 0, "nothing to repair");
        assert_eq!(fake.repairs.load(Ordering::SeqCst), 0);
        assert_eq!(outcome.refused, None);
        assert_eq!(outcome.to_json()["repaired"], json!(false));
    }

    /// The case the spine exists for: the second answer is only reachable because the first
    /// attempt's errors were handed back.
    #[tokio::test]
    async fn each_round_sees_the_errors_of_the_attempt_before_it() {
        let fake = Fake::failing(2);
        let outcome = verify_generated(&fake, 3).await;
        assert_eq!(outcome.built, Some(true), "{outcome:?}");
        assert_eq!(outcome.rounds, 2, "two repairs were spent");
        assert_eq!(fake.builds.load(Ordering::SeqCst), 3);
        let handed = fake.repaired.lock().unwrap().clone();
        assert_eq!(
            handed,
            vec![
                "error[E0107]: build 0 does not compile".to_string(),
                "error[E0107]: build 1 does not compile".to_string(),
            ],
            "round 2 must see round 1's *new* errors, not the first round's again"
        );
        let json = outcome.to_json();
        assert_eq!(json["built"], json!(true));
        assert_eq!(json["repaired"], json!(true));
        assert_eq!(json["rounds"], json!(2), "how many, not just whether");
    }

    #[tokio::test]
    async fn the_budget_is_a_ceiling_and_the_last_failure_is_reported_verbatim() {
        let fake = Fake::failing(u32::MAX);
        let outcome = verify_generated(&fake, 2).await;
        assert_eq!(outcome.built, Some(false));
        assert_eq!(outcome.rounds, 2, "two repairs, three builds");
        assert_eq!(fake.builds.load(Ordering::SeqCst), 3, "1 + max_rounds");
        assert_eq!(outcome.errors, "error[E0107]: build 2 does not compile");
        assert_eq!(
            outcome.refused, None,
            "it ran out of rounds, it was not refused"
        );

        // Zero rounds is a legitimate configuration: no model to repair with, so build and report.
        let once = Fake::failing(u32::MAX);
        let outcome = verify_generated(&once, 0).await;
        assert_eq!(outcome.built, Some(false));
        assert_eq!(once.builds.load(Ordering::SeqCst), 1);
        assert_eq!(once.repairs.load(Ordering::SeqCst), 0);
    }

    /// A refused repair stops the loop: continuing would spend rounds on a generator that has just
    /// said it cannot produce an acceptable answer.
    #[tokio::test]
    async fn a_refused_repair_stops_the_loop_with_the_reason() {
        let fake = Fake::failing(u32::MAX);
        fake.refuse_repair_from.store(1, Ordering::SeqCst);
        let outcome = verify_generated(&fake, 3).await;
        assert_eq!(outcome.built, Some(false));
        assert_eq!(outcome.rounds, 0);
        assert_eq!(fake.builds.load(Ordering::SeqCst), 1, "no second build");
        assert_eq!(
            outcome.refused.as_deref(),
            Some("refused: the answer is still a placeholder")
        );
        assert!(
            outcome.errors.contains("build 0"),
            "the compiler's words are kept even when the repair is refused: {outcome:?}"
        );
    }

    /// A **setup** failure is not the generator's to fix, so no round is spent on it and `built`
    /// stays `null` — "no compiler ran" is a different claim from "it does not compile".
    #[tokio::test]
    async fn a_setup_failure_is_reported_as_not_built_rather_than_uncompilable() {
        let mut fake = Fake::failing(u32::MAX);
        fake.setup_failure =
            Some("platform 'esp32c6' does not route to a platform module".to_string());
        let outcome = verify_generated(&fake, 3).await;
        assert_eq!(outcome.built, None, "{outcome:?}");
        assert_eq!(fake.builds.load(Ordering::SeqCst), 1);
        assert_eq!(
            fake.repairs.load(Ordering::SeqCst),
            0,
            "a model call would have been spent to be told nothing"
        );
        assert!(outcome.errors.is_empty(), "no compiler words to report");
        assert_eq!(
            outcome.not_built.as_deref(),
            Some("platform 'esp32c6' does not route to a platform module")
        );
        assert_eq!(outcome.to_json()["built"], json!(null));
    }

    /// The gate runs before any build, and names the artifact it refused.
    #[tokio::test]
    async fn a_gate_failure_is_reported_without_building() {
        let mut fake = Fake::passed();
        fake.gate = Err("the file still contains `unimplemented!()`".to_string());
        let outcome = verify_generated(&fake, 3).await;
        assert_eq!(outcome.built, None);
        assert_eq!(fake.builds.load(Ordering::SeqCst), 0, "nothing was built");
        let refused = outcome.refused.unwrap_or_default();
        assert!(refused.contains("crates/demo/src/lib.rs"), "{refused}");
        assert!(refused.contains("unimplemented!()"), "{refused}");
    }
}
