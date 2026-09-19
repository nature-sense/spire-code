// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The embassy wiring, against a real channel on the host — no executor, no board.
//!
//! Half of this is embassy's own behaviour (`try_send` on a full channel), and asserting it is the
//! point: the actor model's promise to a producer is that a refused message comes *back*, and the
//! only way to know the channel keeps that promise is to fill one and look.
//!
//! The other half is [`run`], driven here by hand — one poll, one `try_send`, one poll — so the
//! assertions are about the loop's *shape*: it parks when the channel is empty (it does not spin),
//! and it handles exactly one message per receive, in order.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Waker};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TryReceiveError};

use spire_embedded::actor::{Actor, Mailbox, SendError};
use spire_embedded::embassy::{run, EmbassyMailbox};

/// An actor that records what it was asked to do, through a handle the test keeps.
struct Recorder {
    seen: Rc<RefCell<Vec<u32>>>,
}

impl Actor for Recorder {
    type Message = u32;

    fn handle(&mut self, msg: u32) {
        self.seen.borrow_mut().push(msg);
    }
}

/// The channel's whole contract towards a producer: a message that does not fit comes back.
#[test]
fn a_full_mailbox_hands_the_message_back() {
    static CHANNEL: Channel<CriticalSectionRawMutex, u32, 2> = Channel::new();
    let mailbox = EmbassyMailbox::new(CHANNEL.sender());

    assert!(mailbox.try_send(1).is_ok());
    assert!(mailbox.try_send(2).is_ok());
    assert_eq!(
        mailbox.try_send(3),
        Err(SendError(3)),
        "a full channel refuses — and returns what it refused"
    );

    // Draining makes room again, which is the property a producer actually relies on.
    assert_eq!(CHANNEL.receiver().try_receive(), Ok(1));
    assert!(mailbox.try_send(4).is_ok());
}

/// Several producers are a normal thing to want; a channel sender is cheap to copy.
#[test]
fn a_mailbox_can_be_cloned_for_a_second_producer() {
    static CHANNEL: Channel<CriticalSectionRawMutex, u32, 2> = Channel::new();
    let first = EmbassyMailbox::new(CHANNEL.sender());
    let second = first.clone();

    assert!(first.try_send(1).is_ok());
    assert!(second.try_send(2).is_ok());

    let rx = CHANNEL.receiver();
    assert_eq!(rx.try_receive().map_err(|_| TryReceiveError::Empty), Ok(1));
    assert_eq!(rx.try_receive().map_err(|_| TryReceiveError::Empty), Ok(2));
}

/// The loop: parked when idle, one handle per message, in the order they arrived.
#[test]
fn the_task_loop_handles_one_message_per_receive() {
    static CHANNEL: Channel<CriticalSectionRawMutex, u32, 4> = Channel::new();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mailbox = EmbassyMailbox::new(CHANNEL.sender());

    let mut task = core::pin::pin!(run(
        Recorder {
            seen: Rc::clone(&seen)
        },
        CHANNEL.receiver()
    ));
    let mut cx = Context::from_waker(Waker::noop());

    // Nothing to receive: the task is waiting on the channel, not spinning on it.
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert!(seen.borrow().is_empty());

    mailbox.try_send(7).expect("the channel has room");
    assert!(
        task.as_mut().poll(&mut cx).is_pending(),
        "one message handled, then parked again — the loop does not run on its own"
    );
    assert_eq!(*seen.borrow(), vec![7]);

    // A backlog is drained one message per poll, in order: that ordering is the actor model.
    mailbox.try_send(8).expect("the channel has room");
    mailbox.try_send(9).expect("the channel has room");
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert_eq!(*seen.borrow(), vec![7, 8, 9]);
}
