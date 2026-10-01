// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

#pragma once

#include <freertos/FreeRTOS.h>
#include <freertos/task.h>

#include <atomic>
#include <cstdint>

namespace spire {

/// One loop, on a FreeRTOS task of its own — the seam both application frameworks stand on.
///
/// A loop needs three things from the platform: a **stack** big enough for what it does, a
/// **priority**, and on a two-core part a **core** to share. That is the whole of this class, and it
/// is deliberately the only thing the two frameworks have in common: a dataflow actor's loop pushes
/// inline (so its stack has to hold the whole chain of consumers), and a mailbox actor's loop
/// receives and dispatches; neither is this class's business.
///
/// `std::thread` would also work — ESP-IDF's is pthreads over FreeRTOS — but it cannot say how much
/// stack the loop needs or which core it must share, and on a chip whose default is a few kilobytes
/// that is the difference between working and a stack overflow at run time.
///
/// The loop is responsible for noticing `should_run()`; `stop()` waits for it to, and says so by
/// hanging rather than detaching. A loop that ignores the flag is a bug in that loop, and one that
/// a hang reports.
///
/// The bodies live in `src/task.cpp`: everything here touches FreeRTOS, so this header is the
/// **interface** — the contract is below, and the calls that keep it are next door.
class Task {
public:
    Task() = default;
    Task(const Task&) = delete;
    Task& operator=(const Task&) = delete;
    ~Task();

    /// Start `body(arg)` on its own task.
    ///
    /// `stack_bytes` is what the loop actually needs — one that parses a frame wants far more than
    /// one that reads a register. `core` is `0`/`1` to pin, `tskNO_AFFINITY` to let the scheduler
    /// choose. Returns false when FreeRTOS refused (almost always: not enough heap for the stack),
    /// which callers surface rather than ignore.
    ///
    /// `name` must outlive the task: FreeRTOS keeps the pointer, not a copy.
    bool start(const char* name,
               std::uint32_t stack_bytes,
               UBaseType_t priority,
               BaseType_t core,
               void (*body)(void*),
               void* arg);

    /// The flag the loop polls: `while (task.should_run()) { ... }`.
    bool should_run() const;

    /// Ask the loop to finish and wait for it. Idempotent, and safe on a task that never started.
    void stop();

    bool running() const;

private:
    /// The task body's entry point, which FreeRTOS calls with `this` — the one place that needs to
    /// know the handle exists, so `stop()` can wait for the loop rather than detach from it.
    static void trampoline(void* self_ptr);

    void (*body_)(void*) = nullptr;
    void* arg_ = nullptr;
    std::atomic<bool> run_{false};
    std::atomic<TaskHandle_t> handle_{nullptr};
};

}  // namespace spire
