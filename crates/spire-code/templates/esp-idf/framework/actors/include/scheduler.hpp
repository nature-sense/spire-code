// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <freertos/FreeRTOS.h>

#include <cstddef>
#include <cstdint>
#include <memory>
#include <type_traits>
#include <utility>
#include <vector>

#include <actor.hpp>
#include <registry.hpp>

namespace spire {

/// The **scheduler**: it owns the actors, and therefore owns the thing that has to be stopped.
///
/// This is the part that makes an actor *system* rather than a base class with a queue. It holds
/// every actor it spawned — the object, the mailbox and the task — starts them in the order they
/// were spawned, and stops them in reverse. Nothing else in the framework needs to know how many
/// there are or in what order they should go away, which is the point: the only thing that reliably
/// knows the lifecycle of a system is the thing that created it.
///
/// **Start after spawning, never before.** Until `start()` nothing runs — no task exists — so a
/// message sent to a ref before the system starts waits in its mailbox and is handled on the first
/// iteration. That is deliberate: it makes wiring order irrelevant, and it means a half-built system
/// cannot run half-built.
///
/// **Lifecycle.** Construct → `spawn` (as many as the product has) → `start` → run → `stop`. A
/// scheduler is not restartable: `stop()` releases the actors and closes the mailboxes, and refs
/// held past it go invalid rather than dangling, which is what `ActorRef::valid()` is for.
///
/// **Spawn before starting, and let names do the wiring that refs cannot.** `spawn` also fills the
/// `Registry` — the name → mailbox index — so an actor can reach a peer it could not have been
/// *constructed* with: two actors that reference each other are both spawned before either's `init()`
/// runs, and `init()` is where each finds the other. See `Registry` for why that is the only order
/// that works, and `registry()` for how an application hands the index to its actors.
///
/// The type-erased half — the `Entry` that actually holds an actor, its mailbox and its task — lives
/// in `src/scheduler.cpp`, because everything interesting in it is a FreeRTOS call or a `Task` call
/// and neither belongs in a header. What has to be here is the part that is a template.
class Scheduler {
public:
    Scheduler();
    /// A scheduler sharing an index the caller already holds — the same one it hands to its actors —
    /// rather than the one `Scheduler()` makes for itself. A null pointer means "make one", so the
    /// registry is never null and `registry()` always has something to hand back.
    explicit Scheduler(std::shared_ptr<Registry> registry);
    ~Scheduler();
    Scheduler(const Scheduler&) = delete;
    Scheduler& operator=(const Scheduler&) = delete;

    /// Create an actor, give it a mailbox, and return the address to send to.
    ///
    /// `Message` is the actor's message type, `A` the actor's type, and the rest is its constructor's
    /// arguments — so an actor that needs a peer's ref or a board constant is constructed with it
    /// rather than reaching for a global. `spawn` is a no-op returning an invalid ref once the system
    /// has started: starting one actor in the middle of a running system is a lifecycle nobody asked
    /// for, and refusing is how a caller finds out.
    template <typename Message, typename A, typename... Args>
    ActorRef<Message> spawn(const char* name,
                            std::uint32_t stack_bytes,
                            UBaseType_t priority,
                            BaseType_t core,
                            Args&&... args) {
        using Base = Actor<Message, A::mailbox_depth>;
        static_assert(std::is_base_of_v<Base, A>,
                      "an actor is a subclass of spire::Actor<Message>, with the same message type "
                      "`spawn` was told about");
        auto* actor = new A(std::forward<Args>(args)...);
        if (actor == nullptr) {
            return ActorRef<Message>{};
        }
        auto mailbox = attach(name, stack_bytes, priority, core, Base::mailbox_depth,
                              sizeof(Message), actor, &Base::template hook_init<A>,
                              &Base::template hook_message<A>, &Base::template hook_shutdown<A>,
                              &Scheduler::destroy_of<A>);
        if (!mailbox) {
            // Nothing to delete here: `attach` took ownership of `actor` and released it on the way
            // out. An invalid ref is a refusal a caller can see — `valid()` is false and `send()`
            // fails — rather than a crash waiting to happen.
            return ActorRef<Message>{};
        }
        // Register the name **here**, where the message type is still known: `attach` sizes the mailbox
        // but cannot name the type, and telling two same-sized types apart is the registry's job. A
        // refused spawn registers nothing, so a name is an address only if its actor exists.
        registry_->add(name, message_tag<Message>(), mailbox);
        ActorRef<Message> ref(mailbox);
        // Through the base pointer: the naming class for this private access is the base, which is
        // the class that grants the friendship.
        static_cast<Base*>(actor)->self_ = ref;
        return ref;
    }

    /// Start every actor, in the order it was spawned.
    ///
    /// All or nothing: if one refuses (`init()` says no, or FreeRTOS could not give it a task), the
    /// ones already started are stopped in reverse before `false` is returned — a refused start must
    /// not leave half a system running, because that is the state nobody can diagnose.
    bool start();

    /// Stop every actor, in **reverse** the order it was spawned, then release them. Idempotent.
    void stop();

    /// Whether the system has been started and not stopped.
    bool running() const;

    /// How many actors were spawned.
    std::size_t size() const;

    /// The shared name → mailbox index, filled as actors are spawned. An application reads it to hand
    /// to its actors, which resolve their peers through it in `init()` — the only moment a peer that
    /// has not been spawned yet (or that references this actor in turn) is there to be found.
    const std::shared_ptr<Registry>& registry() const { return registry_; }

private:
    struct Entry;

    /// The non-template half of `spawn`: everything that does not depend on the message type, and
    /// therefore everything that can live in a translation unit.
    std::shared_ptr<Mailbox> attach(const char* name,
                                    std::uint32_t stack_bytes,
                                    UBaseType_t priority,
                                    BaseType_t core,
                                    std::size_t depth,
                                    std::size_t message_size,
                                    void* actor,
                                    bool (*init)(void*),
                                    void (*dispatch)(void*, const void*),
                                    void (*shutdown)(void*),
                                    void (*destroy)(void*));

    template <typename A>
    static void destroy_of(void* actor) {
        delete static_cast<A*>(actor);
    }

    /// The name → mailbox index, filled at `spawn`. Declared **before** `entries_` so it is destroyed
    /// *after* them: an actor holds a `const Registry&`, and an index that died first would leave
    /// every one of them holding a dangling reference on the way out. `Scheduler::stop()` (called
    /// from the destructor) already releases the actors before any member dies; this is the ordering
    /// that still holds if that ever changes.
    std::shared_ptr<Registry> registry_ = Registry::create();
    std::vector<std::unique_ptr<Entry>> entries_;
    bool started_ = false;
};

}  // namespace spire
