// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

// __NAME__'s **seam**: the whole of its interface to the bus, and the only place in this component
// that touches IDF's driver API.
//
// Everything above this file is written in terms of these three calls and nothing else. That is what
// makes the protocol testable on a host: `test/` compiles this component against a *fake*
// `__IDENT___bus.hpp` with the same three functions, which records what the protocol wrote and
// answers from a script of frames. No board, no chip, no IDF, no hardware.
//
// Three calls rather than two, because **`bus_write_read` is how a register is read**. Asking and
// listening is one exchange with the device: on I²C the two halves are separated by a STOP, which is
// a different transaction that plenty of devices do not answer; on SPI the reply is clocked out
// while the command goes in. Reaching past `bus_write_read` for a read of a register is the most
// common way a driver works on one device and not on the next.
//
// The three are deliberately narrow. A protocol that reached past them — for a delay, a GPIO, a
// second device on the bus — would be one whose behaviour a fake cannot reproduce, and so one that
// nothing can check without the hardware it happens to be wired to today.

#include <__BUS_INCLUDE__>

#include <cstddef>
#include <cstdint>
#include <vector>

namespace __NAMESPACE__ {

/// This component's handle to the bus.
///
/// In firmware it **is** the IDF handle for this bus — the same type IDF's API takes — and in the
/// host test it is a token that nothing dereferences. The component names `BusHandle` and never an
/// IDF type, which is what lets one header compile in both places and keeps the protocol's public
/// API free of the platform it happens to run on.
using BusHandle = __BUS_HANDLE__;

/// Send `out_len` bytes. `false` when the bus refused the transfer.
inline bool bus_write(BusHandle device, const uint8_t* out, std::size_t out_len) {
__BUS_WRITE__
}

/// Receive exactly `in_len` bytes. `false` when the bus returned fewer.
inline bool bus_read(BusHandle device, uint8_t* in, std::size_t in_len) {
__BUS_READ__
}

/// Send `out_len` bytes and receive `in_len` back, as **one** exchange with the device.
inline bool bus_write_read(BusHandle device, const uint8_t* out, std::size_t out_len,
                           uint8_t* in, std::size_t in_len) {
__BUS_WRITE_READ__
}

}  // namespace __NAMESPACE__
