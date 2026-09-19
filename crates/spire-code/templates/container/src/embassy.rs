// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **embassy** wiring for the actor system.
//!
//! Embassy is the runtime a `no_std` family uses — the ESP32-S3 through `esp-hal`, the RP2040
//! through `embassy-rp`. It is asynchronous where the std executor is threaded, and the actor does
//! not care: that is what [`crate::actor::Spawner`] is for. But the *wiring* differs in one way
//! that is worth understanding rather than hiding.
//!
//! **Embassy tasks cannot be generic.** `embassy_executor::Spawner::spawn` takes a `SpawnToken`, and
//! a token comes from calling a function marked `#[embassy_executor::task]` — one concrete function
//! per task. So there can be no `EmbassySpawner` implementing [`crate::actor::Spawner`]: that trait
//! spawns *whatever actor you hand it*, which would mean spawning a task whose type depends on the
//! actor at the call site, and embassy has no way to express it. The application therefore declares
//! the task, and this module supplies the two halves that are identical for every actor:
//!
//! 1. [`EmbassyMailbox`] — the producer's half: an `embassy_sync` channel sender behind the actor
//!    model's [`Mailbox`] trait. Where the std executor *allocates* a queue, this borrows a `static`
//!    one — firmware has no allocator, and a channel's capacity belongs in the type.
//! 2. [`run`] — the consumer's half: `receive → handle → repeat`, generic over the actor.
//!
//! So an application writes:
//!
//! ```ignore
//! static CHANNEL: Channel<CriticalSectionRawMutex, Blink, 4> = Channel::new();
//!
//! #[embassy_executor::task]
//! async fn blink(actor: Blinker<PinDriver<'static, Output>>) {
//!     spire_embedded::embassy::run(actor, CHANNEL.receiver()).await
//! }
//! ```
//!
//! …and at startup, where the peripherals are, `EmbassyMailbox::new(CHANNEL.sender())` is what the
//! rest of the firmware sends to. Three lines that are not ceremony: the channel is `static`
//! (firmware cannot allocate one), the task is *concrete* (embassy's constraint, not ours), and
//! [`run`] is the single place where the actor's message type meets the channel's.

use crate::actor::{Actor, Mailbox, SendError};

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::{Receiver, Sender, TrySendError};

/// A [`Mailbox`] over an `embassy_sync` channel.
///
/// The send is the channel's own, and that is the whole implementation: `try_send` is already
/// non-blocking and already hands back the message it refused, which is exactly what
/// [`Mailbox::try_send`] asks for. Nothing is wrapped, so nothing can be lost in the wrapping.
#[derive(Debug)]
pub struct EmbassyMailbox<'a, M: RawMutex, T, const N: usize> {
    tx: Sender<'a, M, T, N>,
}

impl<'a, M: RawMutex, T, const N: usize> EmbassyMailbox<'a, M, T, N> {
    /// Build one over a channel's sender.
    pub fn new(tx: Sender<'a, M, T, N>) -> Self {
        Self { tx }
    }
}

/// A sender is a handle, so copying one is how a second producer is made.
///
/// Written out rather than derived: `#[derive(Clone)]` would demand `M: Clone, T: Clone` of the
/// *mailbox*, and neither is a property a channel sender needs — the mutex type is shared and the
/// message travels by value.
impl<M: RawMutex, T, const N: usize> Clone for EmbassyMailbox<'_, M, T, N> {
    fn clone(&self) -> Self {
        Self { tx: self.tx }
    }
}

impl<M: RawMutex, T, const N: usize> Mailbox for EmbassyMailbox<'_, M, T, N> {
    type Message = T;

    fn try_send(&self, msg: T) -> Result<(), SendError<T>> {
        // `TrySendError` has exactly one variant, `Full(T)`: a channel does not disconnect, because
        // every sender is a value rather than a handle to something that can die. So "full" is the
        // only way a send fails — and the message comes back inside it, which is the property the
        // actor model insists on.
        self.tx
            .try_send(msg)
            .map_err(|TrySendError::Full(m)| SendError(m))
    }
}

/// The actor task loop: receive, handle, repeat.
///
/// This is the consumer's half of the wiring, and the whole of the actor model on embassy: no
/// queue, no thread, no allocator — the channel is the caller's `static`, and this future is the
/// task the executor drives. It never returns, so the task ends only when the executor does, and
/// `handle` must not block: it is the same single-task rule the std executor has.
pub async fn run<A, M, const N: usize>(mut actor: A, rx: Receiver<'_, M, A::Message, N>)
where
    A: Actor,
    M: RawMutex,
{
    loop {
        actor.handle(rx.receive().await);
    }
}
