// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// __NAME__'s **host test** — the protocol exercised on this machine, with no board and no chip.
//
// `__IDENT___bus.hpp` resolves to the fake in this directory, because the seam is included with
// angle brackets and this directory is ahead of the component's `include/` on the include path. So
// this device's real frames go in as *bytes* and what the protocol wrote comes back out. See the
// fake's own comments for the two things it keeps.
//
// The case below is the one every stub passes: a protocol that cannot tell whether the device is
// there says so, rather than claiming it is. **Replace it** with this device's own cases as they are
// learned:
//
//   * the reply that proves presence — an id or status register, with the value the datasheet says;
//   * the register reads the protocol performs, with their expected values;
//   * and the malformed ones: a short reply, a corrupt one, no reply at all. Those three matter
//     most, because they are the ones that only ever happen on a bench.

#include <__IDENT___bus.hpp>
#include "__IDENT__.hpp"

#include <cstdio>
#include <vector>

namespace {

int failures = 0;

void expect(bool ok, const char* what) {
    if (!ok) {
        std::printf("FAIL %s\n", what);
        ++failures;
    }
}

}  // namespace

int main() {
    __NAMESPACE__::fake_reset();

    __NAMESPACE__::__TYPE__ device(nullptr);

    // When `probe()` starts reading a real register, this becomes the case that scripts its answer:
    //
    //   __NAMESPACE__::fake_answer({0x69});
    //   expect(device.probe(), "probe() accepts the device's own id");
    //   expect(__NAMESPACE__::fake_written() == std::vector<uint8_t>{0xD0}, "it asked for the id");
    expect(!device.probe(), "an unfilled probe() must not claim the device is there");

    if (failures != 0) {
        std::printf("%d case(s) failed\n", failures);
        return 1;
    }
    std::printf("ok\n");
    return 0;
}
