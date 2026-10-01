// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// __NAME__'s **host test** — inputs in, expected values out.
//
// Nothing is stood in for here: this component is code, so its test is a table of cases. That is why
// the same test runs on the host and on the chip — and why it is the gate a change to this component
// has to pass.
//
// The case below is the one every stub passes: a computation that is not written yet says so rather
// than reporting a number it made up. **Replace it** with the real cases as the code arrives:
//
//   * the ordinary case, with the values a caller would really pass;
//   * the edges: a zero size, an empty buffer, a value at the top of its range;
//   * the refusals: a bad argument, which the component reports by returning false.

#include "__IDENT__.hpp"

#include <cstdio>

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
    __NAMESPACE__::__TYPE__ component;

    // When `run()` starts computing, this becomes the case that checks what it computed:
    //
    //   expect(component.run(), "run() succeeds on a real input");
    //   expect(component.value() == 42, "and gives the expected answer");
    expect(!component.run(), "an unfilled compute must not report an answer it does not have");

    if (failures != 0) {
        std::printf("%d case(s) failed\n", failures);
        return 1;
    }
    std::printf("ok\n");
    return 0;
}
