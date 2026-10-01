// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// The actor framework's **host test** — the framework's own semantics, asserted on this machine.
//
// What it proves, and why each one is worth a test:
//
//   1. a message posted to a ref is **handled**, on the actor's own task, and both messages of a
//      pair arrive — the whole point of a mailbox, and the thing a compile check cannot show;
//   2. an actor that refuses in `init()` **starts nothing**: no mailbox, no task, and a system that
//      says so rather than running half-built;
//   3. an actor can **send to another actor** through a ref it was constructed with, with two
//      different message types — which is the classical pattern and the reason refs are typed by
//      message rather than by actor;
//   4. a full mailbox **refuses instead of blocking** the sender — the design decision that makes a
//      mailbox actor worth its own task;
//   5. an actor can **post to itself**, which is what `self()` is for;
//   6. a peer can be resolved **by name** — the registry — so two actors that reference each other
//      can both be constructed before either is resolved, which constructor injection cannot arrange;
//   7. a name lookup is **checked**: a name registered for one message type will not resolve as
//      another of the same size, an unregistered name resolves to an invalid ref, not a wrong actor,
//      and the tag the check rests on is one address in every translation unit — which is what a
//      shared message-contract header has to be;
//   8. a lookup that finds nothing **refuses the start**, so a wiring mistake is a system that says
//      so at `start()` rather than one that runs with a peer that is not there.
//
// Underneath it is a `std::thread` per actor and a real queue per mailbox (see `freertos_fake.cpp`),
// so "it dispatches" is observed rather than asserted.

#include <actor.hpp>
#include <registry.hpp>
#include <scheduler.hpp>

#include <freertos_fake.hpp>
#include <probe_message.hpp>

#include <atomic>
#include <chrono>
#include <cstdio>
#include <thread>

/// Defined in `message_tag_probe.cpp` — a second translation unit — so the check below is about the
/// invariant and not about this unit's copy of one function.
const void* probe_message_tag_elsewhere();

namespace {

int failures = 0;

void check(bool ok, const char* what) {
    if (!ok) {
        std::printf("FAIL %s\n", what);
        ++failures;
    }
}

/// Poll a condition for up to two seconds, then ask it once more — so a slow machine is not a
/// failure and a genuine hang is.
template <typename Predicate>
bool wait_for(Predicate done) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
    while (std::chrono::steady_clock::now() < deadline) {
        if (done()) {
            return true;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    return done();
}

// --- 1. a message reaches the actor -------------------------------------------------------------

struct Add {
    int amount = 0;
};

class Counter final : public spire::Actor<Add> {
public:
    explicit Counter(std::atomic<int>& total) : total_(total) {}

protected:
    void on_message(const Add& message) override { total_ += message.amount; }

private:
    std::atomic<int>& total_;
};

void a_message_reaches_the_actor() {
    std::atomic<int> total{0};
    spire::Scheduler scheduler;

    auto counter = scheduler.spawn<Add, Counter>("counter", 4096, 5, tskNO_AFFINITY, total);
    check(counter.valid(), "a spawned actor has a live ref");
    check(scheduler.size() == 1, "and the scheduler owns it");

    check(scheduler.start(), "the system starts");
    check(scheduler.running(), "and says so");
    check(counter.send(Add{3}), "the first message is accepted");
    check(counter.send(Add{4}), "and the second");
    check(wait_for([&] { return total.load() == 7; }), "both were handled, on the actor's own task");

    scheduler.stop();
    check(!scheduler.running(), "the system stops");
    spire_fake_join_tasks();

    // The mailbox is shared with the ref, so a ref outlives the actor safely: invalid, not dangling.
    check(!counter.valid(), "a ref goes invalid after stop");
    check(!counter.send(Add{1}), "and a send after stop refuses rather than crashing");
}

// --- 2. an actor that refuses starts nothing ----------------------------------------------------

class Refuser final : public spire::Actor<Add> {
public:
    explicit Refuser(std::atomic<bool>& asked) : asked_(asked) {}

protected:
    bool init() override {
        asked_ = true;
        return false;
    }
    void on_message(const Add&) override {}

private:
    std::atomic<bool>& asked_;
};

void a_refused_actor_starts_nothing() {
    std::atomic<bool> asked{false};
    spire::Scheduler scheduler;

    auto refuser = scheduler.spawn<Add, Refuser>("refuser", 4096, 5, tskNO_AFFINITY, asked);
    check(!asked.load(), "nothing runs until the system is started");

    check(!scheduler.start(), "a refused actor refuses the start");
    check(asked.load(), "and `init()` was the reason");
    check(!refuser.valid(), "a refused actor has no mailbox to post into");
    check(!refuser.send(Add{1}), "so nothing can be sent to it");
    check(scheduler.size() == 1, "the scheduler still owns it, to be cleaned up");

    scheduler.stop();
    spire_fake_join_tasks();
}

// --- 3. one actor sends to another --------------------------------------------------------------

struct Reading {
    int value = 0;
};

struct Alarm {
    int value = 0;
};

/// The far end: remembers the last alarm.
class Sink final : public spire::Actor<Alarm> {
public:
    explicit Sink(std::atomic<int>& last) : last_(last) {}

protected:
    void on_message(const Alarm& alarm) override { last_ = alarm.value; }

private:
    std::atomic<int>& last_;
};

/// The near end: consumes readings and forwards each as an alarm — an actor that talks to an actor,
/// through a ref it was *constructed* with rather than one it looks up.
class Sampler final : public spire::Actor<Reading> {
public:
    Sampler(spire::ActorRef<Alarm> next, std::atomic<int>& seen) : next_(next), seen_(seen) {}

protected:
    void on_message(const Reading& reading) override {
        seen_ += reading.value;
        next_.send(Alarm{reading.value});
    }

private:
    spire::ActorRef<Alarm> next_;
    std::atomic<int>& seen_;
};

void an_actor_can_send_to_another() {
    std::atomic<int> last{0};
    std::atomic<int> seen{0};
    spire::Scheduler scheduler;

    auto sink = scheduler.spawn<Alarm, Sink>("sink", 4096, 5, tskNO_AFFINITY, last);
    auto sampler = scheduler.spawn<Reading, Sampler>("sampler", 4096, 5, tskNO_AFFINITY, sink, seen);
    check(scheduler.size() == 2, "two actors, two mailboxes");

    check(scheduler.start(), "the system starts");
    check(sampler.send(Reading{42}), "a reading goes in at one end");
    check(wait_for([&] { return last.load() == 42; }), "and comes out at the other");
    check(seen.load() == 42, "having been handled by the actor in between");

    scheduler.stop();
    spire_fake_join_tasks();
}

// --- 4. a full mailbox refuses rather than blocks -----------------------------------------------

class Blocking final : public spire::Actor<Add> {
public:
    explicit Blocking(std::atomic<bool>& gate) : gate_(gate) {}

protected:
    void on_message(const Add&) override {
        while (!gate_.load()) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
    }

private:
    std::atomic<bool>& gate_;
};

void a_full_mailbox_refuses_rather_than_blocks() {
    std::atomic<bool> gate{false};
    spire::Scheduler scheduler;

    auto blocking = scheduler.spawn<Add, Blocking>("blocking", 4096, 5, tskNO_AFFINITY, gate);
    check(scheduler.start(), "the system starts");

    // One message is taken and the handler sits on it; the rest fill the mailbox, and then there is
    // no room. Which of the 32 lands where is a race, so the assertions are about both outcomes
    // existing rather than about which index failed.
    int accepted = 0;
    int refused = 0;
    for (int i = 0; i < 32; ++i) {
        if (blocking.send(Add{1})) {
            ++accepted;
        } else {
            ++refused;
        }
    }
    check(accepted > 0, "the mailbox took what it had room for");
    check(refused > 0, "and refused the rest: a full mailbox is a refusal, not a wait");

    gate.store(true);
    check(wait_for([&] { return blocking.waiting() == 0; }), "the actor drains once it is let go");

    scheduler.stop();
    spire_fake_join_tasks();
}

// --- 5. an actor can post to itself -------------------------------------------------------------

struct Kick {
    int remaining = 0;
};

class Tickler final : public spire::Actor<Kick> {
public:
    explicit Tickler(std::atomic<int>& deliveries) : deliveries_(deliveries) {}

protected:
    void on_message(const Kick& kick) override {
        ++deliveries_;
        if (kick.remaining > 0) {
            self().send(Kick{kick.remaining - 1});
        }
    }

private:
    std::atomic<int>& deliveries_;
};

void an_actor_can_post_to_itself() {
    std::atomic<int> deliveries{0};
    spire::Scheduler scheduler;

    auto tickler = scheduler.spawn<Kick, Tickler>("tickler", 4096, 5, tskNO_AFFINITY, deliveries);
    check(scheduler.start(), "the system starts");
    check(tickler.send(Kick{3}), "one kick goes in");
    check(wait_for([&] { return deliveries.load() == 4; }),
          "and the actor re-posts to itself until the kick is spent");
    check(tickler.waiting() == 0, "with nothing left in its mailbox");

    scheduler.stop();
    spire_fake_join_tasks();
}

// --- 6. an actor resolves a peer by name ---------------------------------------------------------

/// The near end of a chain, wired **by name** instead of by ref: what `Sampler` above does with a ref
/// it was *constructed* with, this does in `init()` — the only place it can be done when the peer
/// does not exist yet at construction time.
class LookingUpSampler final : public spire::Actor<Reading> {
public:
    LookingUpSampler(const spire::Registry& registry, std::atomic<int>& seen)
        : registry_(registry), seen_(seen) {}

protected:
    bool init() override {
        next_ = registry_.get<Alarm>("sink");
        return next_.valid();
    }

    void on_message(const Reading& reading) override {
        seen_ += reading.value;
        next_.send(Alarm{reading.value});
    }

private:
    const spire::Registry& registry_;
    spire::ActorRef<Alarm> next_;
    std::atomic<int>& seen_;
};

void an_actor_resolves_a_peer_by_name() {
    std::atomic<int> last{0};
    std::atomic<int> seen{0};
    spire::Scheduler scheduler;

    scheduler.spawn<Alarm, Sink>("sink", 4096, 5, tskNO_AFFINITY, last);
    auto sampler = scheduler.spawn<Reading, LookingUpSampler>("sampler", 4096, 5, tskNO_AFFINITY,
                                                              *scheduler.registry(), seen);
    check(scheduler.registry()->size() == 2, "both spawned actors are in the registry");

    check(scheduler.start(), "the system starts");
    check(sampler.send(Reading{7}), "a reading goes in");
    check(wait_for([&] { return last.load() == 7; }),
          "and reaches the peer the actor looked up by name in `init()`");
    check(seen.load() == 7, "having been handled in between");

    scheduler.stop();
    spire_fake_join_tasks();
}

// --- 7. a name lookup is checked, not merely proved to exist -------------------------------------

void a_lookup_checks_the_message_type() {
    std::atomic<int> total{0};
    spire::Scheduler scheduler;
    scheduler.spawn<Add, Counter>("counter", 4096, 5, tskNO_AFFINITY, total);

    const spire::Registry& registry = *scheduler.registry();
    check(registry.get<Add>("counter").valid(), "the name resolves as the type it was spawned with");
    // `Add` and `Reading` are both `{ int }`, so this is the case a size check would let through: the
    // lookup has to be about the *type*, and the tag is what makes it so.
    check(!registry.get<Reading>("counter").valid(),
          "and not as another message type of the *same size*");
    check(!registry.get<Add>("unwired").valid(), "an unregistered name resolves to an invalid ref");

    // The tag is what makes those checks meaningful, and it has to be the *same* address in every
    // translation unit that names the type — which for a real application is a message-contract header
    // compiled into two components. Otherwise one component's `spawn` would register a name that
    // another component's `get` could never resolve.
    check(spire::message_tag<ProbeMessage>() == probe_message_tag_elsewhere(),
          "a message type has one tag across translation units");

    scheduler.stop();
    spire_fake_join_tasks();
}

// --- 8. two actors that reference each other -----------------------------------------------------

struct Ping {
    int hops = 0;
};

struct Pong {
    int hops = 0;
};

/// One half of a pair. Each resolves the other in `init()`, which is the only way a pair like this can
/// be wired: one of them is always constructed first, and its peer does not exist yet.
class PingActor final : public spire::Actor<Ping> {
public:
    PingActor(const spire::Registry& registry, std::atomic<int>& bounces)
        : registry_(registry), bounces_(bounces) {}

protected:
    bool init() override {
        peer_ = registry_.get<Pong>("pong");
        return peer_.valid();
    }

    void on_message(const Ping& ping) override {
        ++bounces_;
        if (ping.hops > 0) {
            peer_.send(Pong{ping.hops - 1});
        }
    }

private:
    const spire::Registry& registry_;
    spire::ActorRef<Pong> peer_;
    std::atomic<int>& bounces_;
};

class PongActor final : public spire::Actor<Pong> {
public:
    PongActor(const spire::Registry& registry, std::atomic<int>& bounces)
        : registry_(registry), bounces_(bounces) {}

protected:
    bool init() override {
        peer_ = registry_.get<Ping>("ping");
        return peer_.valid();
    }

    void on_message(const Pong& pong) override {
        ++bounces_;
        if (pong.hops > 0) {
            peer_.send(Ping{pong.hops - 1});
        }
    }

private:
    const spire::Registry& registry_;
    spire::ActorRef<Ping> peer_;
    std::atomic<int>& bounces_;
};

void mutual_references_resolve_in_init() {
    std::atomic<int> pings{0};
    std::atomic<int> pongs{0};
    spire::Scheduler scheduler;

    auto ping = scheduler.spawn<Ping, PingActor>("ping", 4096, 5, tskNO_AFFINITY,
                                                 *scheduler.registry(), pings);
    auto pong = scheduler.spawn<Pong, PongActor>("pong", 4096, 5, tskNO_AFFINITY,
                                                 *scheduler.registry(), pongs);
    check(ping.valid() && pong.valid(), "both have an address before either has been resolved");

    // The assertion the registry is *for*: neither `init()` could have run before the other actor was
    // spawned, so if the lookup happened at construction time this start would have to fail.
    check(scheduler.start(), "both actors find the other, so the start is accepted");

    check(ping.send(Ping{3}), "a ping goes in at one end");
    // ping→pong→ping→pong, each counting its own hops.
    check(wait_for([&] { return pongs.load() == 2; }), "and bounces back and forth between them");
    check(pings.load() == 2, "counting every hop on the way");

    scheduler.stop();
    spire_fake_join_tasks();
}

// --- 9. a lookup that finds nothing refuses the start --------------------------------------------

/// An actor wired to a peer that was never spawned: `init()` cannot resolve it, so it refuses — and a
/// refused `init()` refuses the whole start, which is where a wiring mistake is meant to surface.
class NeedyActor final : public spire::Actor<Add> {
public:
    NeedyActor(const spire::Registry& registry, std::atomic<bool>& asked)
        : registry_(registry), asked_(asked) {}

protected:
    bool init() override {
        asked_ = true;
        return registry_.get<Add>("not-spawned").valid();
    }

    void on_message(const Add&) override {}

private:
    const spire::Registry& registry_;
    std::atomic<bool>& asked_;
};

void a_lookup_that_finds_nothing_refuses_the_start() {
    std::atomic<bool> asked{false};
    spire::Scheduler scheduler;
    scheduler.spawn<Add, NeedyActor>("needy", 4096, 5, tskNO_AFFINITY, *scheduler.registry(), asked);

    check(!scheduler.start(), "a peer that was never spawned refuses the start");
    check(asked.load(), "and `init()` — the lookup — was the reason");

    scheduler.stop();
    spire_fake_join_tasks();
}

}  // namespace

int main() {
    a_message_reaches_the_actor();
    a_refused_actor_starts_nothing();
    an_actor_can_send_to_another();
    a_full_mailbox_refuses_rather_than_blocks();
    an_actor_can_post_to_itself();
    an_actor_resolves_a_peer_by_name();
    a_lookup_checks_the_message_type();
    mutual_references_resolve_in_init();
    a_lookup_that_finds_nothing_refuses_the_start();

    spire_fake_join_tasks();
    if (failures != 0) {
        std::printf("%d case(s) failed\n", failures);
        return 1;
    }
    std::printf("ok\n");
    return 0;
}
