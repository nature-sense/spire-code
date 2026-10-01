// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

// The **fake bus** __NAME__'s host test runs against.
//
// Same three functions as the real `__IDENT___bus.hpp` — that is the whole contract, and the reason
// the protocol above them is the same code the firmware runs. Behind them:
//
//   * a **script**: the frames "the device" answers with, in order. A capture off a real device, or
//     one written from its datasheet; either way it is *bytes*, not a behaviour, which is why the
//     test proves the protocol and not the fixture;
//   * a **recording**: every byte the protocol wrote, so a test can assert the exact command rather
//     than only the value that came back. Half of a protocol's bugs are in what it sent.
//
// Globals, because the component's bus interface is free functions. That is the price of a seam this
// thin — and it is why this file lives under `test/`: it is never linked into firmware.

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <initializer_list>
#include <vector>

namespace __NAMESPACE__ {

/// The same name as the real header's, and deliberately a different thing: a token. Nothing here
/// dereferences it. A component that reached into the handle would be one this fake could not stand
/// in for — and so one nothing could check without the device it happens to be wired to.
using BusHandle = void*;

/// One scripted answer, consumed in order by `bus_read`/`bus_write_read`.
using FakeFrame = std::vector<uint8_t>;

inline std::deque<FakeFrame>& fake_script() {
    static std::deque<FakeFrame> script;
    return script;
}

/// Everything the protocol wrote, in order.
inline std::vector<uint8_t>& fake_written() {
    static std::vector<uint8_t> written;
    return written;
}

/// Forget the last test's traffic and point the fake at a device that answers nothing.
inline void fake_reset() {
    fake_script().clear();
    fake_written().clear();
}

/// Queue one answer. Call once per read the protocol is expected to make.
inline void fake_answer(std::initializer_list<uint8_t> bytes) {
    fake_script().push_back(FakeFrame(bytes));
}

/// The handle is a token here: nothing below dereferences it.
inline bool bus_write(BusHandle, const uint8_t* out, std::size_t out_len) {
    fake_written().insert(fake_written().end(), out, out + out_len);
    return true;
}

inline bool bus_read(BusHandle, uint8_t* in, std::size_t in_len) {
    if (fake_script().empty()) {
        // A device that does not answer. Not an error, and not a crash: the case a protocol has to
        // handle by returning false rather than by reading whatever was in the buffer.
        return false;
    }
    FakeFrame frame = fake_script().front();
    fake_script().pop_front();
    if (frame.size() < in_len) {
        return false;  // a short reply — neither success nor a buffer overrun
    }
    std::copy(frame.begin(), frame.begin() + static_cast<std::ptrdiff_t>(in_len), in);
    return true;
}

inline bool bus_write_read(BusHandle device, const uint8_t* out, std::size_t out_len,
                           uint8_t* in, std::size_t in_len) {
    // One exchange: the write is recorded, then the answer is taken from the same script the
    // two-call form would use. A test that needs "the write happened but the device stayed silent"
    // scripts no frame.
    return bus_write(device, out, out_len) && bus_read(device, in, in_len);
}

}  // namespace __NAMESPACE__
