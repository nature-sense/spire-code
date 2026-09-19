// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The on-device actor system.
//!
//! Deliberately the *same shape* as the host's `spire_actor::Actor` — an associated
//! `Message` type and a `handle` that consumes one — with one difference that belongs to
//! the runtime rather than the design: the host's `handle` is `async` and runs on a tokio
//! task, while this one is synchronous and runs on a **FreeRTOS task**. Keeping the shape
//! identical is the point: an engineer (or a model) that can read one can read the other,
//! and the generation tooling can treat both alike.
//!
//! Nothing here spawns anything. The executor belongs to the runtime — see [`Spawner`] —
//! so the same actor code is what a runtime swaps a queue and a task underneath.

use core::fmt::Debug;

/// Something that consumes messages, one at a time.
///
/// Synchronous on purpose: the first executor is a FreeRTOS task whose loop is
/// `loop { handle(recv()) }`. An async executor (embassy, for RP2040 later) is a *runtime*
/// choice — it can drive this same `handle` from inside an async task.
pub trait Actor {
    /// What this actor understands.
    ///
    /// For a firmware module this is also its **interface**: the message set *is* what it can
    /// be asked to do, which is what makes it measurable by the drift analysis (a message with
    /// no handler is a missing implementation).
    type Message;

    /// Consume one message.
    ///
    /// Must not block indefinitely — the executor is a single task, so a long `handle`
    /// starves every other message to this actor. A runtime that needs waiting expressions
    /// puts them *between* messages, not inside this call.
    fn handle(&mut self, msg: Self::Message);
}

/// A handle to a *running* actor — the only thing a producer holds.
///
/// No `std`, no allocator, no knowledge of queues: the runtime decides the storage (a
/// FreeRTOS queue, an embassy channel, a static ring). Sending is non-blocking, so a
/// producer can never be parked by a busy consumer.
pub trait Mailbox {
    type Message;

    /// Deliver, or hand the message back.
    ///
    /// Returning it on failure is deliberate: a dropped message in firmware is a silent
    /// bug, and this forces the discard to be explicit at the call site rather than
    /// discovered later on a board.
    fn try_send(&self, msg: Self::Message) -> Result<(), SendError<Self::Message>>;
}

/// The message a [`Mailbox`] refused to take, returned to the sender.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<M>(pub M);

/// The executor seam: a runtime spawns an actor and hands back a mailbox.
///
/// This is the *only* thing a board family has to supply for the actor model to work —
/// everything else (`Actor`, `Mailbox`, the HAL traits) is board-agnostic. ESP32 implements
/// it with a std thread, which under esp-idf **is** a FreeRTOS task; a future RP2040 runtime
/// with embassy's executor plus a channel.
///
/// The two bounds live on the trait rather than on an impl because Rust will not let an impl
/// strengthen them — and they are the bounds every real executor needs, since spawning an
/// actor means moving it across a task boundary (a FreeRTOS task, an embassy task on another
/// core). A runtime that genuinely cannot require `Send` defines its own spawn instead of
/// weakening this one for everybody.
///
/// Note that a runtime owns the spawned actor's storage, which is why this trait takes the
/// actor **by value** and returns only the mailbox: for a `std` runtime (esp-idf) that
/// storage is a heap allocation, and for a `no_std` one it is a static — the difference
/// stays behind this trait.
pub trait Spawner {
    /// The mailbox type this runtime produces (a FreeRTOS queue wrapper, a channel, …).
    type Mailbox<M>: Mailbox<Message = M>;

    /// Start `actor` on the runtime's executor and return the way to talk to it.
    fn spawn<A>(&self, actor: A) -> Self::Mailbox<A::Message>
    where
        A: Actor + Send + 'static,
        A::Message: Send + 'static;
}
