// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#include "__IDENT__.hpp"
#include <__IDENT___bus.hpp>

namespace __NAMESPACE__ {

__TYPE__::__TYPE__(BusHandle device) : device_(device) {}

// TODO: the protocol. Every branch below is a datasheet question and none of them can be guessed:
//
//   - what the command words are, and which of them carry an argument;
//   - whether each is checksummed, and with which polynomial and seed;
//   - a reply's length, word order and field order, and how the transfer is framed;
//   - what a short, corrupt or absent reply looks like, and how it is told from a valid one.
//
// Talk to the device through `bus_write`, `bus_read` and `bus_write_read` and nothing else — see
// `__IDENT___bus.hpp`, which is this component's entire bus interface and the only file in it that
// names IDF. **A register read is `bus_write_read`**, one exchange: asking in two calls puts a stop
// between the halves, which is a different conversation that plenty of devices do not answer.
//
// Return false rather than logging: the caller knows whether a failure is a startup refusal, a
// reading worth retrying, or a device that has gone away — and a driver that decides that for it is
// one whose failures cannot be told apart.
//
// Then **prove it**, which is the part that makes this component worth having: a protocol is the one
// layer of a firmware stack that can be tested on a host, against a fake bus answering with recorded
// bytes — no board, no IDF, no hardware. `test/` is already built that way and already runs; the
// cases in `test/__IDENT___test.cpp` are yours, and a protocol whose cases are written is one that
// can be changed later.

bool __TYPE__::probe() {
    // TODO: something that answers only if the device is really there — read its id, or its status
    // register, through `bus_write_read` — rather than trusting that a bus handle exists. A probe
    // that returns true because a handle was passed is a probe that reports success on a board
    // where the device is not fitted.
    //
    // Until then: `false`. The component's contract is to report what it knows, and a stub knows
    // nothing yet.
    return false;
}

}  // namespace __NAMESPACE__
