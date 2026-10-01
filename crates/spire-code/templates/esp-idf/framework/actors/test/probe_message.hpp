// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

// A message type defined **once**, in a header, and compiled into two translation units — which is
// what a message *contract* looks like from the registry's point of view: one component's actor
// registers the name, another component's actor resolves it, and the two only agree because they
// agree on this type.

struct ProbeMessage {
    int value = 0;
};
