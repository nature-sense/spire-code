// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// The fake FreeRTOS behind the actors' host test. See `freertos/FreeRTOS.h` for why it exists and
// what it is allowed to be.
//
// Two decisions are worth noting, because both are about not lying to the test:
//
//   * a **queue is a real queue** — a deque under a mutex and a condition variable, with a depth
//     that can be reached and a blocking receive that wakes. A mailbox that "never fills up" would
//     make the refusal case untestable, and the refusal is half the design;
//   * a **task is a real thread** — so `on_message` runs on another stack, and the test cannot pass
//     by accident on a framework that only ever dispatches synchronously.
//
// Queues and threads are kept alive until the process ends rather than freed where FreeRTOS would
// free them: a fake that frees eagerly turns a lifecycle bug in the *test* into a use-after-free in
// the *fake*, which is a much worse failure to debug than a leak nobody minds in a 20 ms run.

#include <freertos/FreeRTOS.h>
#include <freertos/queue.h>
#include <freertos/task.h>

#include <freertos_fake.hpp>

#include <chrono>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace {

struct FakeQueue {
    std::mutex mutex;
    std::condition_variable changed;
    std::deque<std::vector<unsigned char>> items;
    std::size_t depth = 0;
    std::size_t item_size = 0;
    bool closed = false;
};

struct FakeTask {
    std::thread thread;
    std::string name;
};

/// Everything the fake allocated, held until the process ends. The destructor joins the threads, so
/// a test that forgets to is still not a `std::terminate` at exit — but the test does say so
/// explicitly, because "the loops have finished" is an assertion, not a hope.
struct Registry {
    std::mutex mutex;
    std::vector<std::unique_ptr<FakeQueue>> queues;
    std::vector<std::unique_ptr<FakeTask>> tasks;

    ~Registry() { join_tasks(); }

    void join_tasks() {
        std::lock_guard<std::mutex> lock(mutex);
        for (auto& task : tasks) {
            if (task->thread.joinable()) {
                task->thread.join();
            }
        }
    }
};

Registry& registry() {
    static Registry shared;
    return shared;
}

std::chrono::steady_clock::time_point deadline_after(TickType_t ticks) {
    return std::chrono::steady_clock::now() + std::chrono::milliseconds(ticks);
}

}  // namespace

void spire_fake_join_tasks() { registry().join_tasks(); }

QueueHandle_t xQueueCreate(UBaseType_t depth, UBaseType_t item_size) {
    auto queue = std::make_unique<FakeQueue>();
    queue->depth = depth;
    queue->item_size = item_size;
    FakeQueue* raw = queue.get();
    std::lock_guard<std::mutex> lock(registry().mutex);
    registry().queues.push_back(std::move(queue));
    return raw;
}

void vQueueDelete(QueueHandle_t handle) {
    auto* queue = static_cast<FakeQueue*>(handle);
    if (queue == nullptr) {
        return;
    }
    {
        std::lock_guard<std::mutex> lock(queue->mutex);
        queue->closed = true;
    }
    // Wake anybody waiting, so a closed mailbox returns rather than hangs.
    queue->changed.notify_all();
}

BaseType_t xQueueSend(QueueHandle_t handle, const void* item, TickType_t ticks_to_wait) {
    auto* queue = static_cast<FakeQueue*>(handle);
    if (queue == nullptr) {
        return pdFALSE;
    }
    std::unique_lock<std::mutex> lock(queue->mutex);
    if (queue->closed) {
        return pdFALSE;
    }
    const auto full = [&] { return queue->items.size() >= queue->depth; };
    if (full()) {
        if (ticks_to_wait == 0) {
            return pdFALSE;
        }
        const auto deadline = deadline_after(ticks_to_wait);
        while (full() && !queue->closed) {
            if (queue->changed.wait_until(lock, deadline) == std::cv_status::timeout) {
                break;
            }
        }
        if (full() || queue->closed) {
            return pdFALSE;
        }
    }
    const auto* bytes = static_cast<const unsigned char*>(item);
    queue->items.emplace_back(bytes, bytes + queue->item_size);
    queue->changed.notify_all();
    return pdTRUE;
}

BaseType_t xQueueReceive(QueueHandle_t handle, void* out, TickType_t ticks_to_wait) {
    auto* queue = static_cast<FakeQueue*>(handle);
    if (queue == nullptr) {
        return pdFALSE;
    }
    std::unique_lock<std::mutex> lock(queue->mutex);
    const auto empty = [&] { return queue->items.empty(); };
    if (empty()) {
        if (ticks_to_wait == 0) {
            return pdFALSE;
        }
        const auto deadline = deadline_after(ticks_to_wait);
        while (empty() && !queue->closed) {
            if (queue->changed.wait_until(lock, deadline) == std::cv_status::timeout) {
                break;
            }
        }
        if (empty()) {
            return pdFALSE;
        }
    }
    std::memcpy(out, queue->items.front().data(), queue->item_size);
    queue->items.pop_front();
    queue->changed.notify_all();
    return pdTRUE;
}

UBaseType_t uxQueueMessagesWaiting(QueueHandle_t handle) {
    auto* queue = static_cast<FakeQueue*>(handle);
    if (queue == nullptr) {
        return 0;
    }
    std::lock_guard<std::mutex> lock(queue->mutex);
    return static_cast<UBaseType_t>(queue->items.size());
}

BaseType_t xTaskCreatePinnedToCore(TaskFunction_t body,
                                   const char* name,
                                   std::uint32_t stack_bytes,
                                   void* arg,
                                   UBaseType_t priority,
                                   TaskHandle_t* handle,
                                   BaseType_t core) {
    (void)stack_bytes;
    (void)priority;
    (void)core;
    auto task = std::make_unique<FakeTask>();
    task->name = name != nullptr ? name : "";
    FakeTask* raw = task.get();
    {
        std::lock_guard<std::mutex> lock(registry().mutex);
        registry().tasks.push_back(std::move(task));
    }
    raw->thread = std::thread([body, arg] { body(arg); });
    if (handle != nullptr) {
        *handle = raw;
    }
    return pdPASS;
}

void vTaskDelete(TaskHandle_t task) { (void)task; }

void vTaskDelay(TickType_t ticks) { std::this_thread::sleep_for(std::chrono::milliseconds(ticks)); }
