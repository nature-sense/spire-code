// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

// A **fake FreeRTOS**, for this component's host test only.
//
// The actor framework is five FreeRTOS calls deep — a queue and a task — and neither exists on this
// machine. So the test stands this in for them, exactly as a driver's host test stands a fake bus in
// for a device: the component is compiled **unmodified**, against the same `<freertos/...>` names it
// uses on a chip, and what runs underneath is a real queue and a real thread.
//
// It is deliberately not a mock in the pejorative sense: `xQueueSend` refuses when full and accepts
// when there is room, `xQueueReceive` blocks and wakes, `uxQueueMessagesWaiting` counts. A test
// against it is evidence about the framework's *behaviour*, not about whether it compiled.
//
// Only the surface the framework uses is here. That is not laziness — it is what keeps this file
// small enough to read, and it is why an added FreeRTOS call in the framework fails loudly (an
// undeclared identifier) rather than silently finding a fake that does nothing.

#include <cstddef>
#include <cstdint>

using BaseType_t = int;
using UBaseType_t = unsigned;
using TickType_t = std::uint32_t;
using TaskFunction_t = void (*)(void*);
using TaskHandle_t = void*;
using QueueHandle_t = void*;

constexpr BaseType_t pdTRUE = 1;
constexpr BaseType_t pdFALSE = 0;
constexpr BaseType_t pdPASS = 1;
constexpr BaseType_t pdFAIL = 0;
constexpr BaseType_t tskNO_AFFINITY = -1;
constexpr TickType_t portMAX_DELAY = 0xFFFFFFFFu;

/// One tick is one millisecond here, which is the only place this fake is more generous than a chip
/// (FreeRTOS on esp32s3 defaults to 100 Hz). It makes the test's timeouts readable and its timings
/// generous; nothing in the framework depends on the rate.
constexpr TickType_t pdMS_TO_TICKS(std::uint32_t milliseconds) { return milliseconds; }
