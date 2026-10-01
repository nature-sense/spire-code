// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <freertos/FreeRTOS.h>

/// The five queue calls `spire::Mailbox` makes.
QueueHandle_t xQueueCreate(UBaseType_t depth, UBaseType_t item_size);
void vQueueDelete(QueueHandle_t queue);
BaseType_t xQueueSend(QueueHandle_t queue, const void* item, TickType_t ticks_to_wait);
BaseType_t xQueueReceive(QueueHandle_t queue, void* out, TickType_t ticks_to_wait);
UBaseType_t uxQueueMessagesWaiting(QueueHandle_t queue);
