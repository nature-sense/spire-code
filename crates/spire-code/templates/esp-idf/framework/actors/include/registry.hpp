// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <cstddef>
#include <memory>
#include <string>
#include <unordered_map>
#include <utility>

#include <actor_ref.hpp>

namespace spire {

/// A **process-unique tag for a message type** — how a name lookup convinces itself that the actor it
/// found takes the message the caller is about to send.
///
/// `sizeof` cannot answer that question: two different structs of the same size are indistinguishable
/// to it, and the mistake it would let through — a `Reading` posted into a mailbox that reads an
/// `Alarm` — is exactly the one a *name* shows up to hide, because the name says nothing about type.
/// RTTI would answer it and is off on this platform, so the tag is the address of one object per
/// `Message` type instead: one address across the whole image, because a function-local static in an
/// inline function (a template function is one) is a single object no matter how many translation
/// units instantiate it. Deliberately **not** `const`, so the linker has no licence to fold two
/// byte-identical tags into one address.
template <typename Message>
const void* message_tag() {
    static char tag = 0;
    return &tag;
}

/// A **name → mailbox index**: the second way to reach an actor, beside the ref you were handed.
///
/// A ref is the right address when you already know who you are talking to, and the wrong one when
/// you cannot. Two actors that must reach each other cannot both be *constructed* with the other's
/// ref, because one of them is always constructed first and its peer does not exist yet. The registry
/// breaks that cycle **by name**: an actor is handed a `const Registry&` and resolves its peers in
/// `init()`, which is the one moment every actor exists at once — the scheduler has spawned all of
/// them before any of them inits — and never in its constructor, which runs while the rest are still
/// being built.
///
/// The scheduler fills it at `spawn`, and `spawn` is refused once the system has started, so the
/// index is written on one thread before the system runs and read-only afterwards. It needs no lock
/// for the same reason a constant does.
///
/// A lookup is typed by the **message**, and checked by `message_tag`, so a name registered for one
/// message type will not resolve as another — not even one of the same size. An unknown name, or a
/// name that takes a different message, is an **invalid ref**: `valid()` is false and `send()` fails,
/// which is how a wiring mistake becomes a refusal in `init()` rather than a corruption later.
class Registry {
public:
    /// Make a registry — the **one** way a `Registry` comes into being, and it is the `Scheduler` that
    /// calls it. There is deliberately no public constructor: a registry is never a singleton, never a
    /// stack temporary, and never something application code makes for itself. An actor is *handed* the
    /// scheduler's by reference and reaches its peers through it. (`new` here rather than
    /// `std::make_shared`, because the constructor is private and `make_shared` — a free function —
    /// could not call it.)
    static std::shared_ptr<Registry> create() { return std::shared_ptr<Registry>(new Registry()); }

    Registry(const Registry&) = delete;
    Registry& operator=(const Registry&) = delete;

    /// Register one actor's mailbox under the name it was spawned with. Called by the `Scheduler`;
    /// application code reads the index and does not write to it. A name registered twice is the last
    /// one registered — a duplicate is a wiring mistake, and the *specification's* validation is what
    /// is meant to catch it, not this.
    void add(const char* name, const void* type_tag, std::shared_ptr<Mailbox> mailbox) {
        by_name_.insert_or_assign(name == nullptr ? std::string("actor") : std::string(name),
                                  Entry{type_tag, std::move(mailbox)});
    }

    /// Resolve a peer by name, checked against the message type it was registered with.
    template <typename Message>
    ActorRef<Message> get(const char* name) const {
        const auto found = by_name_.find(name == nullptr ? std::string("actor") : std::string(name));
        if (found == by_name_.end() || found->second.type_tag != message_tag<Message>()) {
            return ActorRef<Message>{};
        }
        return ActorRef<Message>(found->second.mailbox);
    }

    /// How many names are registered — the actor count, once the system has been spawned.
    std::size_t size() const { return by_name_.size(); }

private:
    /// The scheduler's and nowhere else — reached only through `create()`, so `spire::Registry()` in
    /// application code is a compile error rather than a silently empty index.
    Registry() = default;

    struct Entry {
        const void* type_tag = nullptr;
        std::shared_ptr<Mailbox> mailbox;
    };

    std::unordered_map<std::string, Entry> by_name_;
};

}  // namespace spire
