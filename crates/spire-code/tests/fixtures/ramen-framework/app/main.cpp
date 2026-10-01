// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// The application's entry point, and the one file that says what is wired to what.
//
// Everything here is the **composition**: which components this product uses, how their actors are
// wired, and the board facts — pins, bus, address — that turn a shelf of parts into a device. The
// parts themselves live in a component library and are not edited from here.
//
// ESP-IDF calls `app_main` once, on the main task, after the scheduler is running. Returning from it
// is not a shutdown of the program, but it *does* destroy everything scoped to this function — so
// this function does not return.

#include <freertos/FreeRTOS.h>
#include <freertos/task.h>

#include <esp_log.h>

#include "container.hpp"
// Where a component's headers are included, and where its actor wires itself in. spire-code adds a
// component by rewriting the region below this marker, so mark it: it is not decoration. Wiring
// *after* `container.start()` would silently drop every reading pushed before the link was made.
// spire:include

namespace {
constexpr const char* kTag = "spire-idf-application";
}  // namespace

extern "C" void app_main() {
    spire::Container container;

    // ── Wiring ──────────────────────────────────────────────────────────────────────────────
    // Actors are added and wired here, before anything starts. Ramen links at the point of `>>`, so
    // every `Pusher` must be connected to the `Pushable`s that read it *before* the producing
    // actor's task begins.
    //
    //     auto& sensor  = container.add<SensorActor>(bus, board.power_pin);
    //     auto& display = container.add<DisplayActor>(board);
    //     sensor.out_reading >> display.in_reading;
    //
    // A component is added to an application by rewriting this region, so the line below is a marker
    // and not decoration: an operation that cannot find it refuses rather than guessing.
    // spire:wire

    if (!container.start()) {
        ESP_LOGE(kTag, "startup failed; the application is not running");
        return;
    }
    ESP_LOGI(kTag, "running (%u actor(s))", static_cast<unsigned>(container.size()));

    // Nothing is left to do on this task — every loop is on a task of its own (see the toolkit's
    // `task.hpp`). Blocking rather than returning is what keeps the composition alive: the scope's
    // destructor would otherwise stop every actor on the way out.
    while (true) {
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}

