// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <__IDENT___bus.hpp>

#include <cstdint>

namespace __NAMESPACE__ {

/// TODO: what this device is, in the terms a caller thinks in — and what it is not.
///
/// **This is a stub, and the protocol is the part no scaffold can write.** The command framing, the
/// checksums, the byte order and the field order are the device's own; they come off its datasheet
/// rather than out of a guess.
///
/// What is fixed here is the shape, and the shape is worth keeping:
///
///  * it owns its **bus handle** and takes no board facts — pins, ports and addresses are the
///    application's to supply, because they are the board's, and a protocol that hardcodes them is
///    a protocol that works on exactly one board. It takes the **handle**, not the address: opening
///    the bus and adding the device at its address is the application's job, and this constructor is
///    handed what comes back from that. A call site passing a bare `0x69` where a handle is wanted
///    does not compile, which is the compiler saying this sentence again;
///  * it names **no actor, no task and no framework type** — a protocol is a library, and what drives
///    it is the product's business;
///  * it reaches the bus through `bus_write` / `bus_read` / `bus_write_read` and nothing else, so it
///    is testable on a host (`test/`) as well as on the chip;
///  * it reports failure by **returning false**, never by logging, so the caller can tell a startup
///    refusal from a reading worth retrying.
class __TYPE__ {
public:
    explicit __TYPE__(BusHandle device);

    __TYPE__(const __TYPE__&) = delete;
    __TYPE__& operator=(const __TYPE__&) = delete;

    /// TODO: one method per thing the device actually does, and delete the rest. A class offering a
    /// `read()` it never implements is worse than one that does not offer it at all.
    bool probe();

private:
    BusHandle device_;
};

}  // namespace __NAMESPACE__
