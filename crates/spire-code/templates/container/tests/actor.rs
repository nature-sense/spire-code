// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The abstraction, exercised from *outside* — the way a project and a runtime will use it.
//!
//! These fakes are the point. An actor written against this crate holds **`embedded-hal` traits and
//! nothing else** — no board type, no vendor HAL — so a host fake stands in for the ESP32 and the
//! *same actor code* runs on both. If this file needed `esp-idf-hal`, the seam would already have
//! failed; if it needed a trait of *ours*, the driver ecosystem would be shut out.

use core::cell::RefCell;
use spire_embedded::actor::{Actor, Mailbox, SendError};
use spire_embedded::embedded_hal::delay::DelayNs;
use spire_embedded::embedded_hal::digital::{ErrorType, OutputPin};

/// One millisecond in the nanoseconds `DelayNs` speaks — so the assertion below can stay in the unit
/// the firmware means.
const MS: u32 = 1_000_000;

// ── A board-agnostic actor ────────────────────────────────────────────────────────────
// Note what is *not* here: no ESP32, no FreeRTOS, no `std`, and no trait of this crate's own.
// The actor borrows `embedded-hal` traits — the ones every vendor HAL and every driver speaks —
// which is how the same code compiles for every family.

struct Blink<'a, L, D> {
    led: &'a mut L,
    delay: &'a mut D,
    on: bool,
}

impl<'a, L, D> Blink<'a, L, D> {
    fn new(led: &'a mut L, delay: &'a mut D) -> Self {
        Self {
            led,
            delay,
            on: false,
        }
    }
}

/// `Debug` because a `Mailbox` failure hands the message back inside a `SendError`, and
/// `SendError<M>: Debug` needs `M: Debug` — so a producer can `.unwrap()` a send.
#[derive(Debug)]
enum BlinkMsg {
    Toggle,
    Pattern { toggles: u32, gap_ms: u32 },
}

impl<L: OutputPin, D: DelayNs> Actor for Blink<'_, L, D> {
    type Message = BlinkMsg;

    fn handle(&mut self, msg: Self::Message) {
        match msg {
            BlinkMsg::Toggle => {
                self.on = !self.on;
                // `embedded-hal`'s `OutputPin` is fallible by signature and infallible in practice on
                // every MCU GPIO: the error type exists for expanders and other buses. Discarding it
                // here is deliberate — a blink that cannot report a failure is worse than a blink that
                // assumes the pin holds a logic level.
                let _ = if self.on {
                    self.led.set_high()
                } else {
                    self.led.set_low()
                };
            }
            BlinkMsg::Pattern { toggles, gap_ms } => {
                for _ in 0..toggles {
                    self.handle(BlinkMsg::Toggle);
                    self.delay.delay_ns(gap_ms * MS);
                }
            }
        }
    }
}

// ── Host fakes: what a runtime supplies, minus the hardware ───────────────────────────

#[derive(Default)]
struct FakeLed {
    states: RefCell<Vec<bool>>,
}

/// `ErrorType` is a trait of its own in `embedded-hal` 1.0 (`OutputPin: ErrorType`), so the error is
/// declared once and the pin impls below borrow it.
///
/// `Infallible` for a fake: a fake GPIO cannot fail, which is the honest signature for one — and the
/// reason an actor can discard the result without pretending an error is possible.
impl ErrorType for FakeLed {
    type Error = core::convert::Infallible;
}

impl OutputPin for FakeLed {
    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.states.borrow_mut().push(true);
        Ok(())
    }

    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.states.borrow_mut().push(false);
        Ok(())
    }
}

#[derive(Default)]
struct FakeDelay {
    waited: RefCell<Vec<u32>>,
}

impl DelayNs for FakeDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.waited.borrow_mut().push(ns);
    }
}

/// A fixed-capacity, allocator-free ring.
///
/// `core`-only on purpose: it is evidence that a `no_std` runtime can satisfy [`Mailbox`]
/// without an allocator, which is the constraint the RP2040 runtime will be under.
struct Ring<M, const N: usize> {
    slots: RefCell<[Option<M>; N]>,
    head: RefCell<usize>,
    len: RefCell<usize>,
}

impl<M, const N: usize> Ring<M, N> {
    fn new() -> Self {
        Self {
            slots: RefCell::new(core::array::from_fn(|_| None)),
            head: RefCell::new(0),
            len: RefCell::new(0),
        }
    }

    /// The consumer half — what a runtime's task loop does with a real queue.
    fn pop(&self) -> Option<M> {
        if *self.len.borrow() == 0 {
            return None;
        }
        let i = *self.head.borrow();
        let msg = self.slots.borrow_mut()[i].take();
        *self.head.borrow_mut() = (i + 1) % N;
        *self.len.borrow_mut() -= 1;
        msg
    }
}

impl<M, const N: usize> Mailbox for Ring<M, N> {
    type Message = M;

    fn try_send(&self, msg: M) -> Result<(), SendError<M>> {
        if *self.len.borrow() >= N {
            return Err(SendError(msg));
        }
        let i = (*self.head.borrow() + *self.len.borrow()) % N;
        self.slots.borrow_mut()[i] = Some(msg);
        *self.len.borrow_mut() += 1;
        Ok(())
    }
}

/// An actor plus its mailbox — the *shape* a runtime supplies (FreeRTOS swaps the ring for a
/// queue and `pump` for a task).
struct HostTask<A: Actor, const N: usize> {
    actor: RefCell<A>,
    ring: Ring<A::Message, N>,
}

impl<A: Actor, const N: usize> HostTask<A, N> {
    fn new(actor: A) -> Self {
        Self {
            actor: RefCell::new(actor),
            ring: Ring::new(),
        }
    }

    /// The task loop: drain the queue into `handle`, in order.
    fn pump(&self) -> usize {
        let mut handled = 0;
        while let Some(msg) = self.ring.pop() {
            self.actor.borrow_mut().handle(msg);
            handled += 1;
        }
        handled
    }
}

impl<A: Actor, const N: usize> Mailbox for HostTask<A, N> {
    type Message = A::Message;

    fn try_send(&self, msg: A::Message) -> Result<(), SendError<A::Message>> {
        self.ring.try_send(msg)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_blink_actor_is_board_agnostic() {
    let mut led = FakeLed::default();
    let mut delay = FakeDelay::default();
    {
        let mut blink = Blink::new(&mut led, &mut delay);
        blink.handle(BlinkMsg::Pattern {
            toggles: 3,
            gap_ms: 100,
        });
    }

    assert_eq!(
        led.states.borrow().as_slice(),
        &[true, false, true],
        "each toggle must drive the output"
    );
    assert_eq!(
        delay.waited.borrow().as_slice(),
        &[100 * MS, 100 * MS, 100 * MS],
        "and wait between them — recorded rather than slept, so this test is instantaneous"
    );
}

#[test]
fn a_running_actor_is_reached_only_through_its_mailbox() {
    let mut led = FakeLed::default();
    let mut delay = FakeDelay::default();
    let handled = {
        let task = HostTask::<_, 4>::new(Blink::new(&mut led, &mut delay));
        // A producer holds the MAILBOX, never the actor — the actor belongs to its task.
        task.try_send(BlinkMsg::Toggle).unwrap();
        task.try_send(BlinkMsg::Toggle).unwrap();
        task.try_send(BlinkMsg::Toggle).unwrap();
        task.pump()
    };

    assert_eq!(handled, 3, "every message should have been handled");
    assert_eq!(
        led.states.borrow().as_slice(),
        &[true, false, true],
        "and in the order they were sent"
    );
}

#[test]
fn a_full_mailbox_hands_the_message_back() {
    let mut led = FakeLed::default();
    let mut delay = FakeDelay::default();
    let refused = {
        let task = HostTask::<_, 2>::new(Blink::new(&mut led, &mut delay));
        task.try_send(BlinkMsg::Toggle).unwrap();
        task.try_send(BlinkMsg::Toggle).unwrap();
        // The third cannot fit. It must come BACK, not vanish: a dropped message on
        // firmware is a silent bug, which is why `try_send` returns it.
        matches!(
            task.try_send(BlinkMsg::Toggle),
            Err(SendError(BlinkMsg::Toggle))
        )
    };

    assert!(
        refused,
        "a full mailbox must return the message, not drop it"
    );
}

/// The claim the whole crate makes, in one test: **the actor does not change when the board
/// does.**
///
/// A second, independently written pair of `embedded-hal` implementations — one that counts instead
/// of recording, one that does not wait — runs the identical `Blink`. A second board family is a
/// runtime like this, not an edit to the actor.
#[test]
fn the_same_actor_runs_against_a_different_runtime() {
    struct CountingLed {
        flips: u32,
    }
    impl ErrorType for CountingLed {
        type Error = core::convert::Infallible;
    }
    impl OutputPin for CountingLed {
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.flips += 1;
            Ok(())
        }

        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.flips += 1;
            Ok(())
        }
    }

    struct NoWait;
    impl DelayNs for NoWait {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    let mut led = CountingLed { flips: 0 };
    let mut wait = NoWait;
    {
        let mut blink = Blink::new(&mut led, &mut wait);
        blink.handle(BlinkMsg::Pattern {
            toggles: 5,
            gap_ms: 1,
        });
    }

    assert_eq!(
        led.flips, 5,
        "same actor, different HAL, neither one changed"
    );
}
