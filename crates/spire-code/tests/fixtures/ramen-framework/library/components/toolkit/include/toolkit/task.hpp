// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <freertos/FreeRTOS.h>
#include <freertos/task.h>

#include <atomic>
#include <cstdint>

namespace spire {

/// One actor's producing loop, on a FreeRTOS task of its own.
///
/// Ramen's push model is **synchronous**: a producer calls its `Pusher` and every wired behaviour
/// runs inline, on the producer's own stack. There is therefore no central event loop to pump —
/// the "pump" is each producing actor's loop, and what that loop needs from the platform is a
/// stack, a priority and a core. This is that, and nothing else.
///
/// `std::thread` would also work here — ESP-IDF's is pthreads over FreeRTOS — but it cannot say how
/// much stack the loop needs or which core it must share, and on a chip whose default is a few
/// kilobytes that is the difference between working and a stack overflow at run time.
///
/// The loop is responsible for noticing `should_run()`; `stop()` waits for it to, and says so by
/// hanging rather than detaching. A loop that ignores the flag is a bug in that loop, and one that
/// a hang reports.
class Task {
public:
    Task() = default;
    Task(const Task&) = delete;
    Task& operator=(const Task&) = delete;
    ~Task() { stop(); }

    /// Start `body(arg)` on its own task.
    ///
    /// `stack_bytes` is what the loop actually needs — one that parses a frame wants far more than
    /// one that reads a register. `core` is `0`/`1` to pin, `tskNO_AFFINITY` to let the scheduler
    /// choose. Returns false when FreeRTOS refused (almost always: not enough heap for the stack),
    /// which callers surface rather than ignore.
    bool start(const char* name,
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

    /// The flag the loop polls: `while (task.should_run()) { ... }`.
    bool should_run() const { return run_.load(std::memory_order_acquire); }

    /// Ask the loop to finish and wait for it. Idempotent, and safe on a task that never started.
    void stop() {
        if (handle_.load() == nullptr) {
            return;
        }
        run_.store(false, std::memory_order_release);
        // The body is mid-iteration, finishing a read or a sleep; one tick is not enough to assume
        // it has seen the flag, so wait for the trampoline to clear the handle.
        while (handle_.load() != nullptr) {
            vTaskDelay(pdMS_TO_TICKS(10));
        }
    }

    bool running() const { return handle_.load() != nullptr; }

private:
    /// The task body's entry point, which FreeRTOS calls with `this` — the one place that needs to
    /// know the handle exists, so `stop()` can wait for the loop rather than detach from it.
    static void trampoline(void* self_ptr) {
        auto* self = static_cast<Task*>(self_ptr);
        self->body_(self->arg_);
        self->handle_.store(nullptr, std::memory_order_release);
        vTaskDelete(nullptr);
    }

    void (*body_)(void*) = nullptr;
    void* arg_ = nullptr;
    std::atomic<bool> run_{false};
    std::atomic<TaskHandle_t> handle_{nullptr};
};

}  // namespace spire
