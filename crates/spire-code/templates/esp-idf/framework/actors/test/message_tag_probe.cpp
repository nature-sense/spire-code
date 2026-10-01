// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// A **second translation unit** in the same test binary, and nothing more. Its only job is to answer
// one question that cannot be answered from a single unit: does `message_tag<ProbeMessage>()` give the
// *same* address here as it does in `actors_test.cpp`?
//
// It is not a convenience — it is the invariant the whole naming scheme rests on. A name resolves
// because the two sides agree on a tag, and `message_tag` is the address of a function-local static
// in an inline function. If that address were per-unit, one component's `spawn` would register a name
// that another component's `get` could never resolve — a silent failure that only ever shows up as a
// peer that is mysteriously "not there". So the test says it out loud rather than trusting the rule.

#include <registry.hpp>

#include <probe_message.hpp>

const void* probe_message_tag_elsewhere() { return spire::message_tag<ProbeMessage>(); }
