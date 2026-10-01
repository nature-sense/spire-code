// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <freertos/FreeRTOS.h>

#include <cstdint>

/// The three task calls `spire::Task` makes. `core` and `priority` are accepted and ignored: the
/// host scheduler is the OS's, and the test is about the framework's lifecycle rather than about
/// FreeRTOS's placement of threads.
BaseType_t xTaskCreatePinnedToCore(TaskFunction_t body,
                                   const char* name,
                                   std::uint32_t stack_bytes,
                                   void* arg,
                                   UBaseType_t priority,
                                   TaskHandle_t* handle,
                                   BaseType_t core);

void vTaskDelete(TaskHandle_t task);
void vTaskDelay(TickType_t ticks);
