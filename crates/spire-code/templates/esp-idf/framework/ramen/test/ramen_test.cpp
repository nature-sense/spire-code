// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// RAMEN's **host test** — the framework's own semantics, asserted here rather than assumed.
//
// Four claims, and each is one an application silently depends on:
//
//   1. pushing is **synchronous and inline** — the producer's `Pusher` runs the consumer's behaviour
//      before it returns, on the producer's own stack. Everything about RAMEN follows from this: no
//      scheduler, no mailbox, one task per chain;
//   2. a topic fires its behaviours **in the order they were linked**, which is the only ordering
//      guarantee there is;
//   3. a combinator (`PushUnary`) transforms on the way through, rather than adding a hop;
//   4. a `Latch` bridges the push and pull models — the one place a value crosses between them.
//
// It is written with plain asserts and prints one line per failure, like every other host test in
// this library: no framework, nothing to install, and the same result on this machine and on a chip.

#include <ramen.hpp>

#include <cstdio>
#include <vector>

namespace {

int failures = 0;

void check(bool ok, const char* what) {
    if (!ok) {
        std::printf("FAIL %s\n", what);
        ++failures;
    }
}

void check_eq(int got, int want, const char* what) {
    if (got != want) {
        std::printf("FAIL %s: got %d, expected %d\n", what, got, want);
        ++failures;
    }
}

/// 1. Pushing runs the wired behaviour inline: the flag is set by the time `push` returns, and it is
///    set on the same task that pushed. There is nothing to pump and nothing to wait for.
void pushing_is_inline() {
    ramen::Pusher<int> out;
    int seen = 0;
    ramen::Pushable<int> in = [&seen](const int& value) { seen = value; };

    out >> in;
    out(42);

    check_eq(seen, 42, "the consumer ran before the pusher returned");
}

/// 2. Link order is the firing order — the only ordering RAMEN promises, and the one a `Logger`
///    chained after a `Filter` depends on.
void topics_fire_in_link_order() {
    ramen::Pusher<int> out;
    std::vector<int> order;
    ramen::Pushable<int> first = [&order](const int&) { order.push_back(1); };
    ramen::Pushable<int> second = [&order](const int&) { order.push_back(2); };
    ramen::Pushable<int> third = [&order](const int&) { order.push_back(3); };

    out >> first;
    out >> second;
    out >> third;
    out(0);

    check(order == std::vector<int>({1, 2, 3}), "behaviours fire in the order they were linked");
}

/// 3. A combinator sits in the middle of a chain and transforms what passes through it.
void a_combinator_transforms() {
    ramen::PushUnary<int, int> doubler = [](int value) { return value * 2; };
    ramen::Pusher<int> source;
    int seen = 0;
    ramen::Pushable<int> sink = [&seen](const int& value) { seen = value; };

    source >> doubler.in;
    doubler.out >> sink;
    source(21);

    check_eq(seen, 42, "the value was transformed on the way through");
}

/// 4. A `Latch` is the bridge between the two models: pushed once, read whenever the pull side asks.
void a_latch_bridges_push_to_pull() {
    ramen::Latch<int> latch;
    ramen::Pusher<int> source;
    ramen::Puller<int> reader;
    int pulled = 0;

    source >> latch.in;
    reader >> latch.out;

    source(7);
    reader(pulled);
    check_eq(pulled, 7, "the pulled value is the one that was pushed");

    // The latch holds the last value, so a second pull reads it again without another push — which is
    // what makes it useful as a "last known reading" rather than a queue.
    pulled = 0;
    reader(pulled);
    check_eq(pulled, 7, "a latch holds what it was given");
}

}  // namespace

int main() {
    pushing_is_inline();
    topics_fire_in_link_order();
    a_combinator_transforms();
    a_latch_bridges_push_to_pull();

    if (failures != 0) {
        std::printf("%d case(s) failed\n", failures);
        return 1;
    }
    std::printf("ok\n");
    return 0;
}
