// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <freertos/FreeRTOS.h>

#include <cstddef>
#include <memory>
#include <utility>

#include <mailbox.hpp>

namespace spire {

/// A **typed, shared handle** to one actor's mailbox: the address you send to.
///
/// This is what makes the framework *classical* rather than a base class with a queue bolted on: a
/// sender holds a ref, not the actor, and never learns what the actor's type is or whether it is
/// still the object that was spawned. A ref is copyable, cheap, and safe to keep — it shares the
/// mailbox, so it stays valid until the scheduler closes it, and `valid()` says so.
///
/// The type is the **message**, not the actor: `ActorRef<Reading>` can be sent a `Reading` and
/// nothing else, which is the whole of what a sender needs to know.
template <typename Message>
class ActorRef {
public:
    ActorRef() = default;

    /// Whether the actor can still be sent to: it was spawned, its `init()` accepted, and it has not
    /// been stopped. A default-constructed ref is invalid — that is its whole state.
    bool valid() const { return mailbox_ != nullptr && mailbox_->is_open(); }

    /// Post a message, waiting up to `wait` ticks for room.
    ///
    /// A full mailbox is a **refusal, not a block**, by default: an actor that waits on a busy peer
    /// has given up the reason it is on a task of its own. A caller that would rather wait passes a
    /// timeout and means it.
    bool send(const Message& message, TickType_t wait = 0) const {
        return mailbox_ != nullptr && mailbox_->post(&message, sizeof(Message), wait);
    }

    /// How many messages are waiting to be handled.
    std::size_t waiting() const { return mailbox_ == nullptr ? 0 : mailbox_->waiting(); }

    /// Whether two refs address the same mailbox — the same actor, into the same queue.
    friend bool operator==(const ActorRef& a, const ActorRef& b) {
        return a.mailbox_ == b.mailbox_;
    }

private:
    friend class Scheduler;
    // `Registry::get` resolves a *name* to the mailbox it indexed and hands back a ref the same way
    // `Scheduler::spawn` does, so it needs the same door.
    friend class Registry;
    explicit ActorRef(std::shared_ptr<Mailbox> mailbox) : mailbox_(std::move(mailbox)) {}

    std::shared_ptr<Mailbox> mailbox_{};
};

}  // namespace spire
