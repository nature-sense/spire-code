# spire-idf-library

<!--
  What this library provides, and **how it is intended to be used**.

  This file is the contract between whoever wrote this library and the model that will build against
  it — a component added here, or an application built on it. It is where an *architecture* lives, and
  the tool knows nothing about actors, tasks, mailboxes or protocols. Keep it true as components
  arrive; an invented rule is worse than a missing one.
-->

## Provides

**The framework** — three components, already here, and not mandatory:

- **`ramen`** — the **dataflow** framework (upstream `Zubax/ramen`, MIT, C++20): typed ports
  (`ramen::Pusher` / `Pushable` / `Puller` / `Pullable`), `>>` to wire them, and topics. Pushing is
  **synchronous and inline**: a producer's `Task` runs every wired behaviour before its `Pusher`
  returns. There is no scheduler, no mailbox and no runtime — one task runs a whole chain.
- **`actors`** — the **classical actor** framework: a **scheduler** that owns the actors and stops them
  in reverse, a **registry** for reaching a peer you cannot be handed a ref to, typed **refs** to send
  to, and `on_message`. An actor is a mailbox (a FreeRTOS queue), a task of its own, and a message
  type; a sender holds an `ActorRef<Message>`, never the actor, so a slow actor cannot stall a fast one
  and the order between two actors is a queue's order:

  ```cpp
  struct Add { int amount = 0; };

  class Counter final : public spire::Actor<Add> {
  public:
      explicit Counter(std::atomic<int>& total) : total_(total) {}
  protected:
      void on_message(const Add& message) override { total_ += message.amount; }
  private:
      std::atomic<int>& total_;
  };

  spire::Scheduler scheduler;                                    // owns them, stops them in reverse
  auto counter = scheduler.spawn<Add, Counter>("counter", 4096, 5, tskNO_AFFINITY, total);
  scheduler.start();                                             // nothing runs before this
  counter.send(Add{3});
  scheduler.stop();
  ```

  Spawn everything and wire it **before** `start()` — a message posted early waits in its mailbox,
  which is what makes wiring order irrelevant. An actor that refuses in `init()` starts nothing, and
  a full mailbox refuses the sender rather than blocking it.

  **Spawn a receiver before its sender, and hand the ref over.** For `a -> b` the receiver `b` is
  spawned first, then its ref is passed to `a` at `a`'s `spawn` — so a peer is a **constructor
  argument** (`spire::ActorRef<Peer::Message>`), which is also the order they must start in. An actor
  is constructed with the peers it reaches, and never fetches one by name:

  ```cpp
  auto b = scheduler.spawn<BMsg, B>("b", 4096, 5, tskNO_AFFINITY);
  auto a = scheduler.spawn<AMsg, A>("a", 4096, 5, tskNO_AFFINITY, b);   // b's ref, handed over
  ```

  Only a peer that **cannot** be handed a ref — because it must reach you in turn, a **cycle** — is
  reached **by name**: the actor takes a `const spire::Registry&` **in its constructor**, keeps it as
  a member, and resolves the peer in `init()`, where every actor exists. `main/` hands it over at
  `spawn` by dereferencing the scheduler's registry:

  ```cpp
  class Forwarder final : public spire::Actor<Add> {
  public:
      explicit Forwarder(const spire::Registry& registry) : registry_(registry) {}   // injected
      bool init() override {
          counter_ = registry_.get<Add>("counter");   // `.`, not `->` — registry_ is a reference
          return counter_.valid();                     // false refuses to start
      }
  protected:
      void on_message(const Add& message) override { counter_.send(message); }
  private:
      const spire::Registry& registry_;
      spire::ActorRef<Add> counter_;
  };

  // main/ hands over the scheduler's registry — a shared_ptr, so dereference it at spawn:
  auto forwarder = scheduler.spawn<Add, Forwarder>("forwarder", 4096, 5, tskNO_AFFINITY,
                                                   *scheduler.registry());
  ```

  A `Registry` is the scheduler's and only the scheduler's. There is no `spire::Registry::instance()`
  and no static `spire::Registry::get`, and `spire::Registry()` does not even compile — so an actor
  never makes one and never keeps one as a global; it is *handed* the scheduler's by reference. Resolve
  by name **only** in `init()`, never in the constructor: the registry is complete only after every
  `spawn`.
- **`toolkit`** — `spire::Task`: one loop, on a task of its own, with a stated stack, priority and
  core. It is the **only** thing the two frameworks share: neither requires the other, and a component
  requires neither.

**Components** — the payload. Add lines here as they arrive:

<!-- One line per component: what it is, and what it is for. -->

## Choosing a framework

- **Human timescale, interactive, command/event-driven → `actors`.** A touch screen, a menu, a reading
  that updates once a second, a device reacting to commands and timers while holding state of its own.
  A unit that has to *wait* for its inputs is a mailbox, a task and `on_message`: post a message and get
  on with something else, and a slow unit is slow without stalling a fast one.
- **Machine timescale, high-rate data flowing through stages → `ramen`.** Frames, kHz samples, an
  inference pipeline: a value is *moved and transformed* rather than *waited on*. Inline dispatch is
  simpler and faster than a queue, and one task runs the whole chain — so give the producer's task a
  stack that holds its consumers.

The distinction is not "streaming versus agents" — a stream is a **component's** job either way (a
sensor bus and a status channel are both drivers). It is whether the application **reacts** to the data
or **pumps** it through stages.

Neither is required. A library of plain components that uses neither is a valid library — and a
component must never assume which framework it is used under.

The choice is made **per application**, before the decomposition: the decomposition *is* the framework,
so the two are decided together rather than one being fitted to the other. An application records the
choice in its `CMakeLists.txt` — `set(SPIRE_APPLICATION_FRAMEWORK actors)`, or `ramen` — so no later
reader has to deduce it from the shape of `main/`.

## How to use it

**A component holds a device or an algorithm, and nothing else.** It takes a bus handle, or its
inputs, and it knows no pins, no addresses, no `Task` and no framework — that is what makes it
reusable and what makes it testable on a host.

**An application holds the composition**: the board facts (pins, bus, addresses), the wiring, and the
**units**. **A unit is a component of its own** — `components/<unit>/`: for an actor, a message type, the
state behind it, a mailbox and a task; for a ramen stage, what it pulls, what it pushes and the code
between. The **shared types** are a component beside them, because the type on an edge belongs to the
*edge*: a sender holds `ActorRef<Receiver::Message>`, so the type is the receiver's and two actors can
share one message, and one stage pushes the value another pulls. `main/` is then what is left: the
spawns, the wiring, the pump and the board facts. A component carries no mailbox and no task either
way: a unit wraps it.

**The lifecycle is the same in both frameworks**, so an application reads the same either way:

```
constructed with its configuration   // the board facts go here, and only here
bool init();                         // takes no arguments; false refuses
void shutdown();                     // releases what it holds
```

Start in the order actors were added (a consumer exists before its producer pushes), stop in reverse.

**Wire before starting.** RAMEN links at the point of `>>`, so a `Pusher` connected after a producer's
task begins silently drops everything pushed before the link was made — which reads as a sensor that
never reports.

### Build requirements a component cannot state itself

- **C++20**, because `ramen` does not compile without it. Nothing needs adding for a chip build —
  ESP-IDF compiles C++ at `-std=gnu++2b` for chip targets — but anything built **outside** IDF (a host
  test, a host tool) has to set it itself.
- Anything else this library needs that no component can state: a config symbol, a compiler flag, a
  partition-table entry. Say it here, because nothing else in the tree carries it.

## Not provided

- **A scheduler, a message bus, an event loop.** A `Task` and a `Pusher`, or a `Task` and a mailbox,
  are the whole of the concurrency story, deliberately.
- **A container or a composition helper.** The application owns the composition: the actors, the order
  they start in, and what is wired to what — and every *name* those actors are spawned under. The
  `actors` framework's registry is only the index the scheduler keeps of what it spawned, so a peer
  that cannot be handed a ref can still be found; it is not a place to keep or look up components.
- **Devices and boards.** A driver is its own component and a board support package belongs to the
  *application* — this library stays off the shelf.
