// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

// The actor framework's FreeRTOS half: the mailbox, the type-erased actor entry, and the scheduler.
//
// The split is C++'s rather than ours. What is a template on the message type cannot live here —
// `spawn`, `ActorRef<Message>` and `Actor<Message>` are in the headers — and what does not depend on
// the message type should: a queue does not know what a message means, and neither does `Task`. So
// everything that actually touches the platform is in this file, and the header next door is the
// interface an application reads.

#include <scheduler.hpp>

#include <task.hpp>

#include <string>
#include <utility>

namespace spire {

Mailbox::Mailbox(std::size_t item_size, std::size_t depth) : item_size_(item_size), depth_(depth) {}

Mailbox::~Mailbox() { close(); }

bool Mailbox::open() {
    if (queue_ != nullptr) {
        return false;
    }
    queue_ = xQueueCreate(static_cast<UBaseType_t>(depth_), static_cast<UBaseType_t>(item_size_));
    return queue_ != nullptr;
}

void Mailbox::close() {
    if (queue_ == nullptr) {
        return;
    }
    vQueueDelete(queue_);
    queue_ = nullptr;
}

bool Mailbox::is_open() const { return queue_ != nullptr; }

bool Mailbox::post(const void* item, std::size_t size, TickType_t wait) {
    if (queue_ == nullptr || size != item_size_) {
        return false;
    }
    return xQueueSend(queue_, item, wait) == pdTRUE;
}

bool Mailbox::receive(void* out, TickType_t wait) {
    if (queue_ == nullptr) {
        return false;
    }
    return xQueueReceive(queue_, out, wait) == pdTRUE;
}

std::size_t Mailbox::waiting() const {
    return queue_ == nullptr ? 0 : static_cast<std::size_t>(uxQueueMessagesWaiting(queue_));
}

/// One actor, **type-erased**: everything the scheduler needs to own and run it without knowing what
/// its messages mean.
///
/// The message type leaves exactly two traces here — how big a message is (the queue's item size and
/// the receive buffer) and a function pointer that casts that buffer back and calls `on_message`. That
/// is deliberate: it is the smallest possible surface through which a typed thing can be driven by
/// untyped machinery, and it is why the framework needs no RTTI and no virtual dispatch of its own.
struct Scheduler::Entry {
    Entry(std::string name,
          std::uint32_t stack_bytes,
          UBaseType_t priority,
          BaseType_t core,
          std::size_t message_size,
          std::size_t depth,
          void* actor,
          bool (*init)(void*),
          void (*dispatch)(void*, const void*),
          void (*shutdown)(void*),
          void (*destroy)(void*))
        : name_(std::move(name)),
          stack_bytes_(stack_bytes),
          priority_(priority),
          core_(core),
          // Aligned well enough for any plain message: `new[]` returns memory aligned for any object
          // up to the default new alignment, and a message is required to be trivially copyable —
          // which is what the `static_assert` in `Actor` is for.
          buffer_(std::make_unique<unsigned char[]>(message_size)),
          mailbox_(std::make_shared<Mailbox>(message_size, depth)),
          actor_(actor),
          init_(init),
          dispatch_(dispatch),
          shutdown_(shutdown),
          destroy_(destroy) {
        // The mailbox opens **here**, at spawn, not at start: a ref has to be a usable address from
        // the moment it is handed back, or "spawn everything, wire it, then start" — the only order
        // that keeps wiring order irrelevant — would be impossible. Messages posted before `start()`
        // wait in the queue and are handled on the loop's first iteration.
        opened_ = mailbox_->open();
    }

    ~Entry() {
        stop();
        if (actor_ != nullptr) {
            destroy_(actor_);
            actor_ = nullptr;
        }
    }

    Entry(const Entry&) = delete;
    Entry& operator=(const Entry&) = delete;

    /// `init()`, then the task. The mailbox is already open (it was opened at spawn), so there is
    /// nothing to create first — but a refusal has to **close** it, or a ref would keep accepting
    /// messages for an actor that is never going to read them.
    bool start() {
        if (task_.running() || !opened_) {
            return false;
        }
        if (!init_(actor_)) {
            mailbox_->close();
            opened_ = false;
            return false;
        }
        if (!task_.start(name_.c_str(), stack_bytes_, priority_, core_, &Entry::trampoline, this)) {
            mailbox_->close();
            opened_ = false;
            return false;
        }
        started_ = true;
        return true;
    }

    /// Stop the loop, then let the actor go. Idempotent, and each step happens once: `started_` marks
    /// an actor whose loop actually ran, which is what `shutdown()` pairs with.
    void stop() {
        task_.stop();
        if (started_) {
            started_ = false;
            shutdown_(actor_);
        }
        mailbox_->close();
    }

    bool running() const { return task_.running(); }

    /// Whether the mailbox was created — the one thing that decides whether `spawn` has a ref to hand
    /// back at all.
    bool is_open() const { return opened_; }

    std::shared_ptr<Mailbox> mailbox() const { return mailbox_; }

private:
    static void trampoline(void* self_ptr) { static_cast<Entry*>(self_ptr)->loop(); }

    void loop() {
        while (task_.should_run()) {
            // A bounded receive rather than `portMAX_DELAY`: the loop has to notice `should_run()`,
            // and a blocking receive would only notice it when the next message arrived. Half a
            // tick's worth of latency on shutdown is the price, and it is paid once.
            if (mailbox_->receive(buffer_.get(), pdMS_TO_TICKS(50))) {
                dispatch_(actor_, buffer_.get());
            }
        }
    }

    std::string name_;
    std::uint32_t stack_bytes_;
    UBaseType_t priority_;
    BaseType_t core_;
    std::unique_ptr<unsigned char[]> buffer_;
    std::shared_ptr<Mailbox> mailbox_;
    void* actor_;
    bool (*init_)(void*);
    void (*dispatch_)(void*, const void*);
    void (*shutdown_)(void*);
    void (*destroy_)(void*);
    /// Whether the mailbox exists — false if the queue could not be created, or if `init()` refused
    /// after it was.
    bool opened_ = false;
    /// Whether the loop actually ran. `shutdown()` pairs with this rather than with the mailbox,
    /// because an actor that was spawned and never started has nothing to shut down.
    bool started_ = false;
    Task task_{};
};

Scheduler::Scheduler() = default;

Scheduler::Scheduler(std::shared_ptr<Registry> registry)
    : registry_(registry != nullptr ? std::move(registry) : Registry::create()) {}

Scheduler::~Scheduler() { stop(); }

std::shared_ptr<Mailbox> Scheduler::attach(const char* name,
                                           std::uint32_t stack_bytes,
                                           UBaseType_t priority,
                                           BaseType_t core,
                                           std::size_t depth,
                                           std::size_t message_size,
                                           void* actor,
                                           bool (*init)(void*),
                                           void (*dispatch)(void*, const void*),
                                           void (*shutdown)(void*),
                                           void (*destroy)(void*)) {
    // **Ownership rule, stated once**: an `Entry` owns its actor from the moment it is constructed,
    // and this function never returns an actor to its caller. So every failure below returns nullptr
    // *after* the Entry has gone out of scope — which releases the actor — and `spawn` therefore has
    // no cleanup of its own to do. Two owners is how double frees happen; there is one.
    auto entry = std::make_unique<Entry>(name == nullptr ? std::string("actor") : std::string(name),
                                         stack_bytes, priority, core, message_size, depth, actor,
                                         init, dispatch, shutdown, destroy);
    if (!entry->is_open() || started_) {
        // No queue to post into, or a system that has already started and would have to start this
        // actor out of order. Either way: no ref, and the actor is released with the Entry.
        return nullptr;
    }
    auto mailbox = entry->mailbox();
    entries_.push_back(std::move(entry));
    return mailbox;
}

bool Scheduler::start() {
    if (started_) {
        return false;
    }
    std::size_t started = 0;
    for (; started < entries_.size(); ++started) {
        if (!entries_[started]->start()) {
            break;
        }
    }
    if (started != entries_.size()) {
        // Roll back what did start, in reverse, so a refusal leaves nothing running.
        while (started > 0) {
            --started;
            entries_[started]->stop();
        }
        return false;
    }
    started_ = true;
    return true;
}

void Scheduler::stop() {
    // Reverse order: everything spawned after an actor is stopped before it, so a system built
    // producer-first is dismantled consumer-first — nobody is left sending into a closed mailbox.
    for (auto it = entries_.rbegin(); it != entries_.rend(); ++it) {
        (*it)->stop();
    }
    entries_.clear();
    started_ = false;
}

bool Scheduler::running() const { return started_; }

std::size_t Scheduler::size() const { return entries_.size(); }

}  // namespace spire
