# ramen-framework

<!--
  A **fixture**: the hints a component library would carry if it provided the RAMEN dataflow actor
  framework. It exists to prove the tool's side of the contract — that a library can describe an
  architecture, and that the description is what a model reads before adding to the library or
  building an application against it.

  It is deliberately *not* shipped as a seed. The tool must stay generic; this is one architecture
  among many, and it is here as a test artefact.
-->

## Provides

- **`ramen`** — the dataflow library itself, a single header: typed ports, and `>>` to wire one
  unit's output to another's input. No allocator, no runtime, no threads.
- **`toolkit`** — `spire::Task`, a FreeRTOS task wrapper with a stack size, a priority and a core.
  This is the seam between the dataflow and the platform; nothing else in the framework touches
  FreeRTOS.

## How to use it

**A unit of work is a plain struct** with Ramen ports as members and two lifecycle calls. There is
no base class to inherit and no registration to perform:

```cpp
struct SensorActor {
    ramen::Pusher<Reading> out_reading{};              // what it emits
    explicit SensorActor(hal::Sensor& sensor);         // constructed with its configuration
    bool init();                                       // takes no arguments; false refuses
    void shutdown();
};
```

**Ports carry values, and the value type is the contract between two units.** A producer declares a
`ramen::Pusher<T>`; every consumer declares a `ramen::Pushable<T>`. Wire them output-to-input, before
anything starts:

```cpp
auto& sensor = container.add<SensorActor>(sensor_hal);
auto& sink   = container.add<LoggerActor>();
sensor.out_reading >> sink.in_reading;
```

**Pushing is synchronous.** A producer calls its `Pusher` and every wired behaviour runs *inline, on
the producer's stack* — so a chain of consumers costs one task, not one per unit, and the stack a
producer's task is given has to hold the whole chain.

**A producing unit owns a `spire::Task`; a consuming unit owns nothing.** There is no central event
loop to run and nothing to poll: the loop *is* the producing unit's own.

**Wire before starting.** Ramen links at the point of `>>`, so a `Pusher` connected after the
producer's task begins silently drops everything already pushed — which reads as a sensor that never
reports.

### What belongs where

- A **component** holds a device and its bus: it takes a bus handle and knows no pins, no board and
  no task. It is reusable, so it does not know who is calling it.
- An **application** holds the composition: the board facts (pins, addresses), the concrete
  `spire::Task`s, and the wiring. This is the layer that knows what the product is.

### Build requirements a component cannot state itself

RAMEN's name-based dispatch uses `typeid`, so **`CONFIG_COMPILER_CXX_RTTI=y`** is required —
ESP-IDF defaults it off and `ramen.hpp` does not compile without it. This is exactly the kind of
thing that belongs here and nowhere else: the library needs it, but only the project's
`sdkconfig.defaults` can say it.

## Not provided

- Any device, protocol or board support. Those are their own components, and they depend on this one
  rather than the other way round.
- A scheduler, a queue, or a message bus. A `Task` and a `Pusher` are the whole of the concurrency
  story, deliberately.
