// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <cstddef>
#include <cstdint>

namespace __NAMESPACE__ {

/// TODO: what this is for, in the terms a caller thinks in — and what it is not.
///
/// **This is a stub.** A library component is code: an algorithm, a filter, a codec, a framework of
/// plain functions. Nothing here talks to a device, and nothing here names an IDF type — which is why
/// a component like this needs no seam and no fake: its host test is an ordinary unit test, and it
/// runs anywhere.
///
/// What is fixed is the shape, and the shape is worth keeping:
///
///  * it is **pure**: the same inputs give the same outputs, and it holds no global mutable state, so
///    two callers cannot interfere and a test needs no setup;
///  * it takes the **caller's** memory and facts — a buffer to fill, a sample rate, a size — rather
///    than reading a global or assuming a board. Compiled for a host or for a chip, it behaves the
///    same;
///  * it names **no actor, no task and no IDF type**: nothing here needs a scheduler or a driver;
///  * it reports failure by **returning false** (or an error value), never by logging, so the caller
///    decides what a failure means.
///
/// TODO: the functions or types this actually offers. Delete this one and write them — a library
/// that ships a `run()` it never implements is worse than one that offers nothing yet.
class __TYPE__ {
public:
    /// TODO: what this does, what it needs, and what it returns. Keep it small: a component is one
    /// thing done well, not a framework.
    bool run();
};

}  // namespace __NAMESPACE__
