// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#include <esp_log.h>

// The build harness.
//
// ESP-IDF builds a `main` component, so a project that is a *library* still needs one — and having
// it means every component here is compiled on every build, which is the only way a component that
// stopped compiling is noticed before an application trips over it.
//
// It does not start anything, wire anything, or know any device. An application does that, in its
// own project, against this one.
extern "C" void app_main() {
    ESP_LOGI("library", "build harness: components compiled; this binary does nothing");
}
