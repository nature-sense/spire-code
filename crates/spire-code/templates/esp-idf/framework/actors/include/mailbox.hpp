// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <freertos/FreeRTOS.h>
#include <freertos/queue.h>

#include <cstddef>

namespace spire {

/// One actor's **mailbox**: a FreeRTOS queue, and nothing else.
///
/// Non-template on purpose. A queue does not care what a message *means*, only how big it is, so the
/// typed part of the framework (`ActorRef<Message>`) is a thin wrapper over this, and the queue
/// itself — create, send, receive, delete — lives in `src/scheduler.cpp` with the rest of the
/// FreeRTOS calls rather than in a header.
///
/// It is **reference-counted** rather than owned by the actor, and that is a lifetime decision
/// rather than a style one. An `ActorRef` outliving the thing that spawned it is not an edge case —
/// an application keeps one past `stop()`, a coordinator hands one to a peer — and a queue the
/// scheduler deleted underneath a live handle is a use-after-free discovered during shutdown, which
/// is exactly the wrong moment. Held by `std::shared_ptr`, so a ref keeps the *object* alive;
/// `close()` is what the scheduler does to it, and a closed mailbox refuses every send.
class Mailbox {
public:
    Mailbox(std::size_t item_size, std::size_t depth);
    ~Mailbox();
    Mailbox(const Mailbox&) = delete;
    Mailbox& operator=(const Mailbox&) = delete;

    /// Create the queue. Called once, by the scheduler, at the moment the actor is **spawned** — so a
    /// ref is a usable address before the system starts, and a message posted early waits rather than
    /// being lost.
    bool open();

    /// Drop the queue. Every send and receive after this refuses, and the object stays alive for
    /// whoever still holds it.
    void close();

    bool is_open() const;

    /// Post `size` bytes, waiting up to `wait` ticks for room.
    ///
    /// False when the mailbox is closed or full, and also when `size` is not this mailbox's item
    /// size — which is a wrong-typed `ActorRef`, a programming error, and one a caller should hear
    /// about rather than watch messages vanish for.
    bool post(const void* item, std::size_t size, TickType_t wait);

    /// Take one item, waiting up to `wait` ticks. False on timeout or when the mailbox is closed.
    bool receive(void* out, TickType_t wait);

    std::size_t waiting() const;
    std::size_t item_size() const { return item_size_; }
    std::size_t depth() const { return depth_; }

private:
    QueueHandle_t queue_ = nullptr;
    std::size_t item_size_;
    std::size_t depth_;
};

}  // namespace spire
