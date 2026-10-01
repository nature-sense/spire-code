// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// `spire::Task` — the FreeRTOS half, and therefore the half that lives in a translation unit.
//
// Nothing here is a template and nothing here is a policy: each function is a thin, deliberate
// wrapper around one FreeRTOS call, and the decisions that are *ours* (what `stop()` waits for, who
// deletes the task, why the flag is atomic) are argued in the header beside the declarations a
// caller reads.

#include <task.hpp>

namespace spire {

Task::~Task() { stop(); }

bool Task::start(const char* name,
                 std::uint32_t stack_bytes,
                 UBaseType_t priority,
                 BaseType_t core,
                 void (*body)(void*),
                 void* arg) {
    if (handle_.load() != nullptr) {
        return false;
    }
    body_ = body;
    arg_ = arg;
    run_.store(true, std::memory_order_release);
    TaskHandle_t handle = nullptr;
    if (xTaskCreatePinnedToCore(&Task::trampoline, name, stack_bytes, this, priority, &handle,
                                core) != pdPASS) {
        run_.store(false, std::memory_order_release);
        return false;
    }
    handle_.store(handle, std::memory_order_release);
    return true;
}

bool Task::should_run() const { return run_.load(std::memory_order_acquire); }

void Task::stop() {
    if (handle_.load() == nullptr) {
        return;
    }
    run_.store(false, std::memory_order_release);
    // The body is mid-iteration, finishing a read or a sleep; one tick is not enough to assume it
    // has seen the flag, so wait for the trampoline to clear the handle. This is the difference
    // between "asked to stop" and "has stopped", and callers rely on the second.
    while (handle_.load() != nullptr) {
        vTaskDelay(pdMS_TO_TICKS(10));
    }
}

bool Task::running() const { return handle_.load() != nullptr; }

void Task::trampoline(void* self_ptr) {
    auto* self = static_cast<Task*>(self_ptr);
    self->body_(self->arg_);
    // Cleared *before* the task deletes itself: `stop()` waits on this, and a task that vanished
    // first would leave the waiter hanging for a handle nobody will ever clear.
    self->handle_.store(nullptr, std::memory_order_release);
    vTaskDelete(nullptr);
}

}  // namespace spire
