// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// The application's entry point — and the one file in this project that is not a component.
//
// ESP-IDF calls `app_main` once, on the main task, after the scheduler is running. What belongs here is
// the composition's **wiring**: the board facts (pins, buses, addresses), the units spawned with what
// they need, and the loop or timer that drives them. What a unit *is* — its class, its state and what it
// does with a message — lives in that unit's own component under `components/`, which this file spawns
// rather than contains.
//
// None of it is assumed by this scaffold, and that is deliberate: **the architecture belongs to the
// component library this project is built against**, and it says how it is meant to be used in that
// library's hints. Read those before writing anything here.

#include <esp_log.h>

namespace {
constexpr const char* kTag = "spire-idf-application";
}  // namespace

extern "C" void app_main() {
    ESP_LOGI(kTag, "nothing to do yet: no unit is spawned and the wiring is empty");
}
