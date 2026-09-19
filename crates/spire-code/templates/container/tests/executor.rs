// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The std executor, exercised on the host.
//!
//! These tests are the reason the executor is its own crate: everything under test here is
//! the same code that runs as the FreeRTOS task loop on an ESP32, so the actor runtime is
//! verified before a board is ever flashed.

use std::sync::mpsc;
use std::time::Duration;

use spire_embedded::actor::{Actor, Mailbox, SendError, Spawner};
use spire_embedded::{StdSpawner, DEFAULT_MAILBOX_DEPTH};

/// Long enough that a failure means "it never arrived", not "the machine was busy".
const TIMEOUT: Duration = Duration::from_secs(10);

/// Records every message it is given, and where it was running when it got them.
struct Recorder {
    seen: mpsc::Sender<(u32, std::thread::ThreadId)>,
}

impl Actor for Recorder {
    type Message = u32;

    fn handle(&mut self, msg: u32) {
        let _ = self.seen.send((msg, std::thread::current().id()));
    }
}

#[test]
fn a_spawned_actor_receives_messages_in_order() {
    let spawner = StdSpawner::new();
    let (seen_tx, seen_rx) = mpsc::channel();
    let mailbox = spawner.spawn(Recorder { seen: seen_tx });

    for n in 1..=3 {
        mailbox.try_send(n).expect("a fresh mailbox has room");
    }

    assert_eq!(seen_rx.recv_timeout(TIMEOUT).unwrap().0, 1);
    assert_eq!(seen_rx.recv_timeout(TIMEOUT).unwrap().0, 2);
    assert_eq!(seen_rx.recv_timeout(TIMEOUT).unwrap().0, 3);
}

/// The executor really does move the actor to its own task: a `handle` that ran inline on
/// the caller's thread would pass every other test here and be useless on a board.
#[test]
fn the_actor_runs_on_its_own_task() {
    let spawner = StdSpawner::new();
    let (seen_tx, seen_rx) = mpsc::channel();
    let mailbox = spawner.spawn(Recorder { seen: seen_tx });

    mailbox.try_send(1).unwrap();
    let (_, actor_thread) = seen_rx.recv_timeout(TIMEOUT).unwrap();

    assert_ne!(
        actor_thread,
        std::thread::current().id(),
        "the actor must run on its own task, not the sender's"
    );
}

/// An actor that never finishes its first message, so the queue cannot drain — the only way
/// to observe the bound deterministically.
struct Stuck;

impl Actor for Stuck {
    type Message = u32;

    fn handle(&mut self, _msg: u32) {
        // Held by the caller to keep this actor busy.
        let (_tx, rx) = mpsc::channel::<()>();
        let _ = rx.recv();
    }
}

#[test]
fn a_full_mailbox_hands_the_message_back() {
    let spawner = StdSpawner::new();
    let mailbox = spawner.spawn(Stuck);

    // Fill it. Whether the actor has taken the first message yet or not, the channel's
    // capacity is the bound, so the extra send cannot succeed.
    let mut refused = None;
    for n in 0..=(DEFAULT_MAILBOX_DEPTH as u32 + 4) {
        if let Err(SendError(msg)) = mailbox.try_send(n) {
            refused = Some(msg);
            break;
        }
    }

    assert!(
        refused.is_some(),
        "a bounded mailbox must eventually refuse, and return the message with it"
    );
}
