// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <cstddef>
#include <memory>
#include <utility>
#include <vector>

#include <esp_log.h>

namespace spire {

/// The actor set: what owns the actors, wires them, and starts and stops them in order.
///
/// A container is *the wiring*. An actor is a plain struct with Ramen ports — `ramen::Pusher` for
/// what it emits, `ramen::Pushable` for what it accepts — plus `init()` and `shutdown()`; this is
/// what owns those structs for the life of the program. There is no scheduler here: a producing
/// actor owns its own `Task` (see `task.hpp`), and Ramen invokes the wired behaviours inline, so
/// the container's job is **lifetime and ordering**, not scheduling.
///
/// Ordering is the part that matters, and it is not symmetric. Actors start in the order they were
/// added, so a consumer exists before its producer can push into it; they stop in the **reverse**
/// order, so a producer is quiet before the thing it pushes into goes away.
///
/// # The actor convention
///
/// An actor is constructed with its configuration and `init()` takes **no arguments** — an actor
/// that wanted the board's I2C address is given it by its constructor, so `init()` means "start
/// doing it" and cannot be called with the wrong thing twice. `init()` returns false to refuse,
/// which is how a missing sensor becomes a boot-time log line rather than a task that silently
/// pushes nothing.
class Container {
public:
    Container() = default;
    Container(const Container&) = delete;
    Container& operator=(const Container&) = delete;
    ~Container() { stop(); }

    /// Add an actor of type `A` and get it back, so wiring reads as one expression at the call
    /// site:
    ///
    ///     auto& sensor  = container.add<SensorActor>(bus);
    ///     auto& display = container.add<DisplayActor>(bsp);
    ///     sensor.out_reading >> display.in_reading;
    ///
    /// Constructs in place, so constructor arguments belong here rather than in `init()`.
    template <typename A, typename... Args>
    A& add(Args&&... args) {
        auto holder = std::make_unique<Holder<A>>(std::forward<Args>(args)...);
        A& actor = holder->actor;
        entries_.push_back(std::move(holder));
        return actor;
    }

    /// How many actors have been added.
    std::size_t size() const { return entries_.size(); }

    /// `init()` every actor, in the order they were added.
    ///
    /// A refusal unwinds the actors already started instead of leaving them running, so a container
    /// is never half-up. Header-only, like everything in an application's `main/`: two application
    /// files that have to be kept in step are one argument for a library, and this is not a library.
    bool start() {
        if (started_) {
            return true;
        }
        for (std::size_t i = 0; i < entries_.size(); ++i) {
            if (!entries_[i]->init()) {
                // Unwind rather than leave a partial set running: an actor whose peer never came up
                // would push into nothing, which reads as a wiring bug and is really a startup one.
                ESP_LOGE(kTag, "actor %u refused to start; unwinding %u already started",
                         static_cast<unsigned>(i), static_cast<unsigned>(i));
                for (std::size_t j = i; j > 0; --j) {
                    entries_[j - 1]->shutdown();
                }
                return false;
            }
        }
        started_ = true;
        ESP_LOGI(kTag, "%u actor(s) running", static_cast<unsigned>(entries_.size()));
        return true;
    }

    /// `shutdown()` every actor, in reverse order. Idempotent.
    void stop() {
        if (!started_) {
            return;
        }
        // Reverse order: a producer stops before the consumer it pushes into.
        for (std::size_t i = entries_.size(); i > 0; --i) {
            entries_[i - 1]->shutdown();
        }
        started_ = false;
        ESP_LOGI(kTag, "%u actor(s) stopped", static_cast<unsigned>(entries_.size()));
    }

    bool started() const { return started_; }

private:
    /// The log tag, for the two lines this class writes: a container that came up and a container
    /// that came down. Anything more belongs to the actor that has something to say.
    static constexpr const char* kTag = "container";

    /// Everything the container needs to know about an actor, and the whole of it.
    struct Entry {
        virtual ~Entry() = default;
        virtual bool init() = 0;
        virtual void shutdown() = 0;
    };

    template <typename A>
    struct Holder final : Entry {
        template <typename... Args>
        explicit Holder(Args&&... args) : actor(std::forward<Args>(args)...) {}
        bool init() override { return actor.init(); }
        void shutdown() override { actor.shutdown(); }
        A actor;
    };

    std::vector<std::unique_ptr<Entry>> entries_;
    bool started_ = false;
};

}  // namespace spire
