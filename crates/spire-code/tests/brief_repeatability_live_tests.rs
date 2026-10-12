// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The brief's **repeatability**, against the real model — the one property a prompt cannot state.
//!
//! `application_spec`'s own tests pin the ceiling in the *request* and the check in the *code*; a
//! scripted endpoint cannot answer
//! the question a person asks after reading two briefs in a row: **is one description read the same
//! way twice?** The brief is the one step whose output *cardinality* is a judgement — how much of the
//! description was left open — and a judgement is the one thing a request cannot pin down: it can say
//! "at most six", and only repeated live runs say what the model does with that. Nothing in how the
//! request is *sampled* helps either, which is measured below rather than assumed.
//!
//! So this reads the same description [`runs`] times and prints what came back: the six points with
//! their `said`/`decided` cells, the forks, and the round trips each took (`> 1` means the model's own
//! answer broke a rule and the repair turn had to correct it). Then it asserts the property this exists
//! for: **the same text gets the same reading every time** — the same six points, and the same cells
//! decided. The description below is deliberately *complete* — it answers all six points, with rates,
//! thresholds and failure modes — so a brief read correctly has **nothing for the person to pick**; what
//! live reads actually returned is in the section below, which is why this file exists rather than
//! another paragraph in the request.
//!
//! Gated like the other live tests: `#[ignore]` so a plain `cargo test` never spends money or network,
//! and a runtime key check so `--ignored` skips cleanly on a machine with no key configured. One
//! paragraph-in, brief-out call per run, so a run of this costs single-digit cents, not dollars.
//!
//! ```text
//! SPIRE_APP_NAME=spire-code cargo test -p spire-code --test brief_repeatability_live_tests \
//!     -- --ignored --nocapture
//! ```
//!
//! `SPIRE_BRIEF_RUNS` changes the sample size and `SPIRE_BRIEF_DESCRIPTION_FILE` points the harness at
//! a description of your own — the whole reason this is a harness and not a screenshot.
//!
//! ## What it has already said
//!
//! **2026-10-11 — the brief that asked.** One unchanged description (~11 KB) and one unchanged request
//! hash came back with `gaps` = **0, 1, 1, 0, 3, 1** entries, and a greedy (`temperature: 0.0`),
//! penalty-free sampling of the very same request gave **0, 6, 2, 2, 1, 1, 0, 1** — one run declaring
//! all six points open where another found the text complete. (That variant was reverted: it did not
//! help, so it had no reason to ship.) The count was neither the sampler's nor the code's to fix: it
//! was the model's judgement about how much a description left open, and a judgement written as a count
//! is not reproducible — while the capabilities, the units and the summary **were**, because nine
//! `requires` and nine `suggests` came back as the same set every time. Anything showing a person a
//! number derived from the unstable part — "3 gaps", "0 gaps", "8 questions" — was showing them a coin
//! flip.
//!
//! **2026-11-10 — the reading that replaced it.** The count is gone, not bounded: a brief now reports
//! the description read against the **six points** ([`BRIEF_POINTS`]), each with what the text said and
//! what the model decided where it said nothing, plus at most [`BRIEF_FORK_CEILING`] **forks** for the
//! decisions that would change what the application is given. With no count left for a model to choose,
//! the property below is that the *shape* is identical every run: the same six points, the same cells
//! decided, and nothing to pick for a description that settles everything.

use spire_actor::ActorSystem;
use spire_code::actors::{LlmActor, LlmMessage};
use spire_code::build::application_spec::{self, BRIEF_FORK_CEILING, BRIEF_POINTS};
use spire_core::subsystems::llm::llm::LlmModelRole;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// The description under test — and the reason it is this long.
///
/// Every one of the six points is answered in it, on purpose: what it does, what it senses and how
/// fast, what a person acts on, what runs in parallel, what happens when something is missing, and
/// when it reacts. The correct brief for it is **six readings drawn from the text and no forks at
/// all** — nothing here leaves a decision that would change what the application is given, so nothing
/// here is the person's to answer. That makes it a *correctness* fixture as much as a consistency one:
/// a fork on this text is a decision nobody left open, and a thin description is exactly where such a
/// thing hides.
const BUILT_IN: &str = "A room air-quality monitor for a workshop, on a battery, with a screen a \
    person reads at a glance.\n\n\
    What it does: every five minutes it reads a particulate sensor (PM1.0, PM2.5, PM10) and a \
    CO2/temperature/humidity sensor, keeps a rolling fifteen-minute average, and shows the current \
    values and a three-step verdict (good / fair / poor) on a 240x135 colour display. A person can \
    tap the screen to page between \"air\", \"trend\" and \"about\" — those are the three screens, and \
    each page is a single frame of the same readings, so the screen is never scrolled. A short press \
    on the side button switches the display off; a long press (2 s) starts a fresh baseline sample, \
    announced on screen as \"re-baselining\" for the ten seconds it takes.\n\n\
    What it senses: the particulate sensor and the CO2 sensor, both on the shared I2C bus, read once \
    every five seconds — a machine rate, because the averaging has to see the peaks rather than the \
    mean of the means; the touch panel, the side button and the battery gauge, which are read at \
    human rate, polled every 50 ms with the display frame; and the network, which is a once-a-minute \
    HTTP POST of the last fifteen minutes of averages to a server on the home LAN, with a two-second \
    timeout and no retry until the next minute.\n\n\
    What it acts on: the display (a full redraw at 30 fps only while a page transition or the \
    re-baseline animation runs, and otherwise redrawn only when a value changes by more than 1%); the \
    backlight (dimmed to 20% after 30 s with no touch, off with the button); one RGB status LED that \
    is green/fair/amber/poor and pulses once a second while measuring; a piezo buzzer that is \
    deliberately not used for readings, only for the re-baseline confirmation; and the network.\n\n\
    What it reacts to over time: sampling is periodic (five seconds); the screen is on-command, from \
    the touch panel or the button, with an on-threshold redraw (a value changing by more than 1%); the \
    LED is on-threshold (the verdict band changing) plus the one-second measuring pulse; the buzzer is \
    on-event (a long press); and the POST is periodic (once a minute) and independent of everything \
    else — it must not block sampling or the display, so it runs in its own task and its result is \
    only ever reported on the \"about\" page as \"last sent 3 min ago\".\n\n\
    What happens when something is missing: if the particulate sensor stops answering on the bus, the \
    screen keeps the last good value with a \"--\" beside it and the LED goes amber, and sampling \
    continues at the same rate so it recovers by itself the moment the sensor answers again, with no \
    restart; if the CO2 sensor is absent at boot, the CO2 field is not drawn at all rather than \
    showing a fake zero, and the verdict is computed from particulates alone; if the network is down \
    or the POST fails, nothing is shown and nothing is retried until the next minute, except that the \
    \"about\" page reports \"last sent\" with a staleness age, or \"never\"; if charge drops below 10% \
    the backlight dims to 20% regardless of the touch timer and the POST interval doubles to two \
    minutes; and if it reaches 3% the display shows a full-screen red message with no exit but the \
    reset button, and sampling stops. On boot the device does not need the network at all — a missing \
    network is not an error, and the monitor runs stand-alone for the nine hours the battery lasts at \
    these rates.";

/// How many times the same text is read. Env-tunable so a suspicious result can be chased without
/// editing the file (`SPIRE_BRIEF_RUNS=20`), and floored at one so a zero cannot pass for a clean
/// sweep.
fn runs() -> usize {
    std::env::var("SPIRE_BRIEF_RUNS")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(6)
}

/// The description under test: the built-in one, or whatever file `SPIRE_BRIEF_DESCRIPTION_FILE`
/// names. Read from a path rather than pasted in, so a live run can be repeated against a description
/// of the reader's own without editing this file.
fn description() -> String {
    match std::env::var("SPIRE_BRIEF_DESCRIPTION_FILE") {
        Ok(path) => std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("SPIRE_BRIEF_DESCRIPTION_FILE={path}: {e}")),
        Err(_) => BUILT_IN.to_string(),
    }
}

/// One round trip to the **planning** model, made the way the coordinator makes it: through the LLM
/// actor's mailbox, so the request body — model, `response_format`, temperature and penalties — is
/// the one the app sends, not one this file invented. Which is the point: the sampling settings are
/// exactly what is under suspicion here.
async fn plan(llm_tx: &mpsc::Sender<LlmMessage>, prompt: String) -> Result<String, String> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    if llm_tx
        .send(LlmMessage::Complete {
            prompt,
            // The role the coordinator's brief leg passes — mirrored here rather than chosen, because
            // what this harness measures is what the app sends.
            role: LlmModelRole::Planning,
            reply_to: reply_tx,
        })
        .await
        .is_err()
    {
        return Err("LLM actor unavailable".to_string());
    }
    match reply_rx.await {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(format!("LLM error: {e}")),
        Err(e) => Err(format!("LLM reply lost: {e}")),
    }
}

/// **One description, read over and over.** A brief is a judgement about a text — how much of it was
/// said, and what to decide where it was not — so this is the only way to see whether that judgement is
/// stable: the model, the request body, the vocabulary check, the point ids, the fork ceiling and the
/// repair turn are all the real ones, and only the number of repetitions is the test's.
#[ignore = "live model: spends real DeepSeek calls — run explicitly with `--ignored`"]
#[tokio::test]
async fn the_same_description_is_read_the_same_way_every_time() {
    // ── The gate: same as the other live tests, so `--ignored` on a machine with no key skips ──
    let config = spire_core::config::load_global_llm_config();
    if config.api_key.is_empty() {
        eprintln!(
            "skipped: no deepseek.api_key in {}\n             A test binary is not named `spire-code`, so `SPIRE_APP_NAME=spire-code` is what\n             points this at the app's own config dir.",
            spire_core::config::llm_config_path().display()
        );
        return;
    }

    let description = description();
    let runs = runs();
    eprintln!(
        "live model: {} at {}\ndescription: {} chars, read {runs} times, {} points, at most \
         {BRIEF_FORK_CEILING} fork(s)",
        config.planning_model,
        config.api_url,
        description.chars().count(),
        BRIEF_POINTS.len()
    );

    let system = ActorSystem::new();
    let (llm_tx, _) = system.spawn(LlmActor::new(config));

    let mut points_read: Vec<Vec<String>> = Vec::new();
    let mut decided: Vec<Vec<String>> = Vec::new();
    let mut forks: Vec<usize> = Vec::new();
    let mut round_trips: Vec<usize> = Vec::new();
    let mut prompts: Vec<u64> = Vec::new();
    let mut summaries: Vec<String> = Vec::new();

    for run in 1..=runs {
        // Counted per run: more than one round trip means the model's own first answer broke a rule
        // and the repair turn had to correct it — the ceiling holding in the *code* rather than in the
        // answer, which is a different thing from it holding in the answer.
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        // The **premise** of this measurement, recorded rather than assumed: `brief_request` is a pure
        // function of the description, so every run must send a byte-identical prompt. The request is
        // thousands of characters, so it is hashed rather than kept — and if a later change ever made
        // the prompt depend on anything that moves, this is what would say so instead of the variance
        // below being blamed on the model.
        let hashes: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let sent = Arc::clone(&hashes);
        let llm = llm_tx.clone();
        let brief = application_spec::brief_application(&description, move |prompt| {
            let llm = llm.clone();
            let counter = Arc::clone(&counter);
            let sent = Arc::clone(&sent);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                prompt.hash(&mut hasher);
                sent.lock().unwrap().push(hasher.finish());
                plan(&llm, prompt).await
            }
        })
        .await
        .unwrap_or_else(|problems| panic!("run {run}: no valid brief came back — {problems}"));

        let read: Vec<String> = brief
            .points
            .iter()
            .map(|point| point.point.clone())
            .collect();
        let decided_here: Vec<String> = brief
            .points
            .iter()
            .filter(|point| !point.decided.is_empty())
            .map(|point| point.point.clone())
            .collect();
        let trips = calls.load(Ordering::SeqCst);
        eprintln!(
            "\n── run {run}/{runs}: {} point(s) read, {} decided, {} fork(s), {trips} round trip(s) — \
             framework={:?}, requires={:?}, suggests={:?}",
            read.len(),
            decided_here.len(),
            brief.forks.len(),
            brief.framework,
            brief.requires,
            brief.suggests
        );
        for point in &brief.points {
            eprintln!(
                "   {}: said={:?} decided={:?}",
                point.point, point.said, point.decided
            );
        }
        for fork in &brief.forks {
            eprintln!(
                "   fork on {}: {} — {:?}",
                fork.point, fork.question, fork.choices
            );
        }
        eprintln!("   summary: {}", brief.summary);

        points_read.push(read);
        decided.push(decided_here);
        forks.push(brief.forks.len());
        round_trips.push(trips);
        prompts.extend(hashes.lock().unwrap().iter().copied());
        summaries.push(brief.summary.clone());
    }

    eprintln!(
        "\n── {runs} reads of one description: points={points_read:?}, decided={decided:?}, \
         forks={forks:?}, round trips={round_trips:?}"
    );
    eprintln!("── requests sent: {prompts:?}");

    // **The premise, before the claim.** One request, and the same one every time: anything else and
    // the distribution below would be measuring the prompt rather than the model.
    assert!(
        prompts.windows(2).all(|pair| pair[0] == pair[1]),
        "the request changed between runs — this would be measuring the prompt, not the model: \
         {prompts:?}"
    );

    // **The property this exists for.** Not that the text is the same (it is), and not that the model is
    // deterministic (nothing sampled is): *which* of the six points a description leaves open is a fact
    // about the description, so a brief that reads all six once and four the next has not read the text —
    // it has sampled it, and the person is the one who finds out. A failure here is a measurement first:
    // the distributions printed above are the finding, not a broken assertion.
    let sorted = |ids: &[String]| {
        let mut ids = ids.to_vec();
        ids.sort();
        ids
    };
    let six: Vec<String> = BRIEF_POINTS
        .iter()
        .map(|point| point.id.to_string())
        .collect();
    assert_eq!(
        sorted(&points_read[0]),
        six,
        "each run must read the six points, once each: points={points_read:?}"
    );
    assert!(
        points_read
            .iter()
            .all(|read| sorted(read) == sorted(&points_read[0])),
        "the same description was read against a different set of points: points={points_read:?}, \
         round trips={round_trips:?}, summaries={summaries:?}"
    );
    assert!(
        decided
            .iter()
            .all(|set| set.as_slice() == decided[0].as_slice()),
        "the same description left a different set of points decided: decided={decided:?}, \
         points={points_read:?}, summaries={summaries:?}"
    );
    // **And nothing to pick.** This description answers all six points — rates, thresholds, failure
    // modes — so a fork here is the model handing back a decision nobody left open: the interview the old
    // `gaps` count used to measure, now visible as one button too many.
    assert!(
        forks.iter().all(|count| *count == 0),
        "a description that settles everything came back with forks: forks={forks:?}, \
         round trips={round_trips:?}"
    );
    assert!(
        forks.iter().all(|count| *count <= BRIEF_FORK_CEILING),
        "the fork ceiling ({BRIEF_FORK_CEILING}) did not hold: forks={forks:?}"
    );
}
