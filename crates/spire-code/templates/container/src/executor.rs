// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! One thread per actor, one bounded queue per mailbox.
//!
//! Under esp-idf a std thread *is* a FreeRTOS task, so this is the FreeRTOS executor.

use crate::actor::{Actor, Mailbox, SendError, Spawner};

/// How many messages a mailbox holds before `try_send` starts handing them back.
///
/// Bounded on purpose: an unbounded queue on firmware turns "the consumer is slower than
/// the producer" — a bug you want to see — into an out-of-memory crash you cannot.
pub const DEFAULT_MAILBOX_DEPTH: usize = 16;

/// A mailbox backed by a bounded channel.
pub struct QueueMailbox<M> {
    tx: std::sync::mpsc::SyncSender<M>,
}

impl<M> Mailbox for QueueMailbox<M> {
    type Message = M;

    fn try_send(&self, msg: M) -> Result<(), SendError<M>> {
        self.tx.try_send(msg).map_err(|e| match e {
            // Both cases hand the message BACK, as `Mailbox::try_send` requires. A full mailbox is
            // the producer's problem and it must be told; a dropped message on a board is a
            // silent bug, and the endpoint-disconnected case is no different.
            std::sync::mpsc::TrySendError::Full(m) => SendError(m),
            std::sync::mpsc::TrySendError::Disconnected(m) => SendError(m),
        })
    }
}

/// The executor: one thread per actor, one bounded queue per mailbox.
///
/// `Default` is implemented so a runtime can write `StdSpawner` inline where it is used.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdSpawner;

impl StdSpawner {
    pub fn new() -> Self {
        Self
    }
}

impl Spawner for StdSpawner {
    type Mailbox<M> = QueueMailbox<M>;

    fn spawn<A>(&self, actor: A) -> QueueMailbox<A::Message>
    where
        A: Actor + Send + 'static,
        A::Message: Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel(DEFAULT_MAILBOX_DEPTH);
        std::thread::spawn(move || {
            let mut actor = actor;
            // The task loop: receive, handle, repeat — and end when every sender is gone,
            // which is what lets an actor's peripherals be handed back.
            while let Ok(msg) = rx.recv() {
                actor.handle(msg);
            }
        });
        QueueMailbox { tx }
    }
}
