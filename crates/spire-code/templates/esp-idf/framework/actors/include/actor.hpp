// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <cstddef>
#include <type_traits>

#include <actor_ref.hpp>

namespace spire {

/// A **classical actor**: a mailbox, a task, and `on_message`.
///
/// This is the other application framework, and it is a different bet from the dataflow one rather
/// than a different spelling of it. There, a value is pushed through wired behaviours synchronously,
/// on the producer's stack. Here, a message is **posted** and the actor takes it when it is ready —
/// so a sender never runs the receiver's code, a slow actor cannot stall a fast one, and the order
/// between two actors is a queue's order rather than a call stack's.
///
/// **Which to reach for.** *Streaming* — frames, samples, readings, a pipeline — is the dataflow
/// shape, and it is simpler and faster there. *Many interacting agents* — a thing that reacts to
/// commands, timers and events from several sources, holding state of its own — is this one's. Both
/// stand on `spire::Task`; neither knows the other exists, and the components they use know neither.
///
/// The actor is a **base class** here, unlike a dataflow unit, because it owns state and a message
/// type and so has something to inherit. The lifecycle is the same convention either way — the actor
/// is *constructed* with its configuration, `init()` takes no arguments and returns false to refuse,
/// `shutdown()` releases what it holds — so an application reads the same in both frameworks.
///
/// **Writing one.** A subclass implements `on_message`, and `init()`/`shutdown()` only if it has
/// something to set up or let go. Nothing else: the scheduler gives it a mailbox and a task.
///
/// ```cpp
/// struct Add { int amount = 0; };
///
/// class Counter final : public spire::Actor<Add> {
/// public:
///     explicit Counter(std::atomic<int>& total) : total_(total) {}
///
/// protected:
///     void on_message(const Add& message) override { total_ += message.amount; }
///
/// private:
///     std::atomic<int>& total_;
/// };
/// ```
///
/// **Running one** — and this is `spire::Scheduler`'s half, so see its header for the rest:
///
/// ```cpp
/// std::atomic<int> total{0};
/// spire::Scheduler scheduler;
/// auto counter = scheduler.spawn<Add, Counter>("counter", 4096, 5, tskNO_AFFINITY, total);
/// scheduler.start();
/// counter.send(Add{3});
/// scheduler.stop();
/// ```
template <typename Message, std::size_t depth = 8>
class Actor {
public:
    /// `Message` has to be a plain, default-constructible value: it is copied into a queue, so a
    /// message that owned memory would be copied into a slot that outlives neither.
    static_assert(std::is_trivially_copyable_v<Message>,
                  "a message is copied into a queue: keep it plain");
    static_assert(std::is_default_constructible_v<Message>,
                  "a mailbox needs a default message to receive into");

    /// The actor's mailbox depth as a compile-time fact. `spawn` needs it before anything exists,
    /// and a depth is a property of the actor rather than of the call site.
    static constexpr std::size_t mailbox_depth = depth;

    Actor() = default;
    Actor(const Actor&) = delete;
    Actor& operator=(const Actor&) = delete;
    virtual ~Actor() = default;

    /// This actor's own address — for the things an actor does to itself: a retry, a follow-up, a
    /// deadline that expires into its own mailbox. Valid from `init()` onwards.
    ActorRef<Message> self() const { return self_; }

    /// The scheduler's way in. Three entry points with plain signatures, because the machinery that
    /// owns the tasks is type-erased and can hold nothing but function pointers.
    ///
    /// They are public because their *addresses* are taken, and they are not meant to be called by
    /// hand. The call goes through the **base class** deliberately: naming `on_message` through the
    /// concrete type would make the access check about that type — whose override is protected and
    /// grants nobody friendship — whereas named through the base it is this class's own protected
    /// member. The call is still virtual, so the subclass's override is what runs.
    template <typename A>
    static bool hook_init(void* self_ptr) {
        Actor* actor = static_cast<A*>(self_ptr);
        return actor->init();
    }

    template <typename A>
    static void hook_message(void* self_ptr, const void* message) {
        Actor* actor = static_cast<A*>(self_ptr);
        actor->on_message(*static_cast<const Message*>(message));
    }

    template <typename A>
    static void hook_shutdown(void* self_ptr) {
        Actor* actor = static_cast<A*>(self_ptr);
        actor->shutdown();
    }

protected:
    /// Handle one message. This runs on the **actor's own task**, so it may take as long as it likes
    /// without holding anybody else up — which is the whole difference from a dataflow behaviour,
    /// and the reason a message is worth queueing at all.
    virtual void on_message(const Message& message) = 0;

    /// Start doing it. Runs on the **caller's** task, before the mailbox or the task exists, so a
    /// refusal costs nothing and is reported as `false` rather than becoming a task that quietly
    /// does nothing. No arguments, because the actor was constructed with its configuration.
    virtual bool init() { return true; }

    /// Stop doing it. Called once, on the caller's task, after the loop has ended.
    virtual void shutdown() {}

private:
    friend class Scheduler;

    ActorRef<Message> self_{};
};

}  // namespace spire
