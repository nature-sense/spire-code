# `composition.spire` — the format of a composition

<!--
  The reference for the file that states what an ESP-IDF application *is*. It is read with
  `application_spec::parse_composition` and held to the rules in `application_spec::validate`
  (`crates/spire-code/src/build/application_spec.rs`), and it is written into a project by
  `idf_projects::render_composition` (`crates/spire-code/src/build/idf_projects.rs`).

  Two companions: `docs/example-composition.spire` is a fully annotated composition that exercises
  nearly every field below, and `docs/esp-idf-architecture.md` §3b–3c is where this file sits in the
  pipeline from a description to a flashed board.

  Every claim here resolves under `crates/spire-code/`, so it can be checked against the tree and
  corrected in the same commit as the code it describes.
-->

A `composition.spire` states what an application **is** — the board, the framework, the devices it
composes against, the units of work, and the wiring between them — as a typed document a person writes,
reviews in a pull request, and a tool checks before a line of C++ exists. The whole point is that the
design is **data a reviewer can read and a check can hold**, rather than prose a model has to
reinterpret.

It is the **source of truth** for the design. `SPIRE.application.json`, written beside it, is a
*generated record* of the same spec: the composition is parsed and the record is rewritten from it
whenever the two disagree (`idf_projects::read_application_and_sync_record`), so the record cannot go
on describing a design the file has since changed. Both names deserialize into the same
`ApplicationSpec`, so there is one composition in two forms — the one a person edits, and the one the
fill phase reads back.

## Why YAML, and how strictly it is read

It is **YAML**, for the one thing JSON cannot carry: comments. A composition is read by a person, and
the reasoning ("this part is `via: board` because the BSP already reaches it") belongs next to the line
it explains — `docs/example-composition.spire` is a composition whose comments are half its value. JSON
is a **subset** of YAML, so a JSON document parses here too, and an answer in a fenced code block
(labelled `yaml`, `json`, or unlabelled) is unwrapped rather than refused: a fence is formatting, not a
different answer.

Reading is **strict**. `parse_composition` hands the text to `serde_yaml`, so a field of the wrong
type, an enum value that is not one of the known variants (kinds, roles and sources are lowercase:
`actors`, `component`, `driver`, `stub`), or a block that is misindented is refused with the line and
column that caused it. A composition that does not parse is **not** a composition, and nothing is
scaffolded from it — which is the whole reason the design is a file rather than a value in a wizard.

Two things a strict parser does *not* do, and both are worth knowing:

- **Unknown keys are ignored**, not refused (the structs carry no `deny_unknown_fields`). A misspelled
  key is silently dropped, so the fields that matter are the ones this document names — nothing else is
  read.
- **Fields the reader does not require may be omitted.** A `description`, a `justification`, a
  `resources` list or a facet block that is absent is simply absent (see *Omission* below), which is why
  a composition written to this document still parses in a reader that predates a newer optional field.

## Shape at a glance

```yaml
description: |            # the application in English (optional)
  A room air-quality monitor that samples particulate matter and shows it on screen.
framework: actors         # actors | ramen            (required)
justification: a touch UI at human timescale           # one line (optional)

board:                    # required
  chip: esp32s3           # the compile target
  bsp: m5stack_core_s3    # the BSP, as the registry names it (optional)
  hal: m5unified          # the board abstraction (optional)

resources:                # the devices the application declares (optional)
  - name: display
    roles: [media.display]
    via: board

board_facts:              # bus + address per driver component (optional)
  - bus: i2c
    device: sps30
    address: "0x69"

messages:                 # the inboxes that take several kinds (optional)
  - id: screen_update
    kinds: [Reading, Gesture]

units:                    # required — the units of work
  - id: sps30
    kind: component
    role: driver
    source: stub
    provides: PM1/2.5/4/10 readings
    bus: i2c
  - id: sampler
    kind: actor
    message: Tick
    description: wakes on the timer, reads the sensor and posts the reading on.
    state: reads the sensor
    uses: [sps30]
    sends_to: [view]
  - id: view
    kind: actor
    message: screen_update
    description: shows the latest reading on the board's screen and reacts to a tap.
    uses: [display]

wiring:                   # `from -> to`, by unit id (optional)
  - sampler -> view

power:                    # the facets, when the composition has them (optional)
  budget_mah: 2000
  target_days: 30
```

That skeleton is a **complete composition**: it parses, and it validates — `display` has exactly one
owner (`view`), `sps30` is reached by a board fact and used by `sampler`, `view`'s inbox is the declared
`screen_update` set that an actor names, and nothing is left unreferenced.

## Top level

Every key of `ApplicationSpec`, in the order `render_composition` writes them:

| key | required | written as | what it states |
| --- | --- | --- | --- |
| `description` | no | multi-line string | the application **in English** — see [Prose](#prose-the-description-fields) |
| `framework` | **yes** | `actors` \| `ramen` | the composition idiom the whole file is written for |
| `justification` | no | one line | why this framework, for the person confirming it |
| `board` | **yes** | mapping | chip, BSP, HAL — see [`board`](#board) |
| `board_facts` | no | list | how each driver is reached — see [`board_facts`](#board_facts) |
| `resources` | no | list | the devices declared — see [`resources`](#resources) |
| `messages` | no | list | the inboxes that take **several** kinds — see [`messages`](#messages) |
| `units` | **yes** | list | the units of work — see [`units`](#units) |
| `wiring` | no | list of `a -> b` | the edges, by unit id — see [`wiring`](#wiring) |
| `power` | no | mapping | `budget_mah`, `target_days` — see [Facets](#the-facets) |
| `storage` | no | mapping | `medium`, `retention` |
| `network` | no | mapping | `transport`, `provision` |
| `security` | no | mapping | `requires: [...]` |

An application with no units is refused outright — *"the spec has no units — an application is a
composition, and this composes nothing"* — so `framework`, `board` and `units` are the three keys a
composition cannot be without.

## `framework`

The framework decides everything downstream — the `main/` template, the decomposition idiom, the
generated per-unit skeletons — so it is stated **in the file** rather than kept in a caller's head, and
it is stated once:

- **`actors`** — *human timescale*: a unit is a mailbox, a task and an `on_message`, and it holds its
  own state between messages. A touch UI, a reading a second, a device that reacts to gestures and
  timers.
- **`ramen`** — *machine timescale*: a value is pushed **synchronously** through wired stages on the
  producer's own stack. Video frames, kHz samples, an inference pipeline — data that is *moved and
  transformed* rather than *waited on*.

The framework fixes the one unit kind it allows. `actors` implies `actor`, `ramen` implies `stage`, and
`component` is the one kind **both** share, because a driver on a bus and a pure algorithm are
framework-agnostic by construction. A spec that mixes them is refused, by name, with the remedy:
*"`x` is a stage unit in an actors application — one framework per application, and an actors
application has no dataflow stages. Mixed applications are deliberately not supported yet."*

`justification` is prose for a reviewer and carries no rule: one line, in the person's own words. The
only mechanical echo of the framework is the marker line it writes into the application's
`CMakeLists.txt` (`set(SPIRE_APPLICATION_FRAMEWORK actors)`), which `ApplicationFramework::marker_line`
produces.

## `board`

The board an application is built for: the silicon, its support package, and the abstraction the
application is written against.

| key | required | notes |
| --- | --- | --- |
| `chip` | **yes** | `esp32s3`, `esp32p4`, `esp32c3`… — decides the compile target and the IDF version |
| `bsp` | no | the BSP as the registry names it: `espressif/m5stack_core_s3` or `m5stack_core_s3` |
| `hal` | no | the board **abstraction** the code is written against — `m5unified`, `bsp`, or empty for none |

**`bsp` is optional, and that is not laxity.** Most boards have no published BSP, and an empty one means
*Spire generates its own backend*, which is a real answer rather than a gap. A BSP, when stated, becomes
the key of a dependency in the application's `main/idf_component.yml`, so it is carried **verbatim** and
a stray comma, space or slash there is a manifest that cannot resolve — which is why the field is
checked to be a well-formed component name wherever a board is supplied programmatically
(`board_from_json`).

`hal` is free text rather than an enum: these are vendor names, and a new one should cost no code. Note
that `bsp` is always **serialized** when the file is written back, even when empty, because the review
UI decodes it as a non-optional string.

## `resources`

The devices this application **declares** that it composes against. An application does not invent
hardware: it says which devices it has and what each one **is**, and every unit's `uses` then has to
reach one of these — so a device nobody declared is a device the application does not have. That is the
rule that turns "nothing invented" from a hope into a check.

```yaml
resources:
  - name: sht20
    roles: [sensing.temperature, sensing.humidity]
    via: library
    bus: i2c
    address: "0x40"
```

| key | required | notes |
| --- | --- | --- |
| `name` | **yes** | the word a unit's `uses` speaks — a bare identifier (`sps30`, `rtc`) |
| `roles` | **yes** | a **list** of capability paths the vocabulary knows |
| `via` | **yes** | `board` \| `library` \| `published` |
| `bus` | no | the **wiring function** it is reached on (`grove.a`, `i2c_internal`) |
| `address` | no | its I²C address, when it has one (`"0x40"`) |

**`roles` is a list because a part can be more than one thing at once.** The SHT20 is
`sensing.temperature` *and* `sensing.humidity`; one device, one resource, every capability it provides.
Each path is checked against the **capability vocabulary** (`capability_vocabulary::is_known_path`):
`media.display`, `sensing.particulate`, `timekeeping.rtc`, and nested paths like `media.video.encode`
are names a device may take. A path the vocabulary does not know is a **misspelling** rather than a new
thing — the difference being that someone named the new one — so it is refused rather than read as "this
application has none". A bare `role: "sensing.temperature"` is accepted as **sugar** for a one-element
list, so a composition written before multi-role parts existed still parses.

**`via` is the one place the code that talks to a part can be**, and the three answers are not
interchangeable downstream:

- **`board`** — a part *of the board*: the BSP or the board abstraction above reaches it, so no
  component of this project drives it (a display, a touch panel, a battery gauge). The name belongs to
  the part — a unit of the same name would shadow it, because a `uses` resolves a unit before a
  resource, so that is refused;
- **`library`** — a part driven by a **component of this project**, whose `id` *is* the resource name
  (already in the library, or a `stub` the design asks to be written). No driver component of that name
  in the spec is a refusal;
- **`published`** — a part driven by a **managed dependency** the manifest resolves from the ESP
  Component Registry.

`bus` is the wiring **function** the part is reached on, not a protocol: a driver's own `bus` is the
protocol it speaks (`i2c`), while a resource's is where the wires are (`grove.a`) — which is why the two
are different fields that never disagree. `address` may also appear in a `board_fact` for the same
driver; stating it twice is fine, disagreeing is not (*"one device has one address"*).

## `board_facts`

How the application **reaches** a device it drives — the application's fact, never a component's. A
component takes a bus handle and knows no address; the address lives here, in the one place that knows
the product.

```yaml
board_facts:
  - bus: i2c
    device: sht20
    address: "0x40"
```

| key | required | notes |
| --- | --- | --- |
| `bus` | **yes** | the wiring function, e.g. `i2c` |
| `device` | **yes** | the **unit id** of the driver component this fact reaches |
| `address` | no | e.g. `"0x69"` — quoted as a house rule |

Each fact must name a **driver component** in this spec: naming an actor, a library or a unit that does
not exist is refused, because a board fact is how a *driver* is reached. Onboard devices (a display, a
touch panel, a battery gauge) come from the board — declare them in `resources` with `via: board`; they
are not board facts. Addresses are quoted by convention: the reader keeps a scalar's text in a string
field either way, but a quoted address means the same thing in every YAML parser and reads as text
rather than a number.

## `messages`

The **inboxes that take several kinds**. Most actors receive one kind of thing and say so on the unit
itself (`message: Tick`). An actor that reacts to a reading, a command *and* a heartbeat needs one type
that carries all three — and that type is a decision about a **conversation**: a sender posts one of the
kinds, the receiver dispatches on which arrived. So it is stated here, once, by name:

```yaml
messages:
  - id: screen_update
    kinds: [Reading, Gesture]
```

| key | required | notes |
| --- | --- | --- |
| `id` | **yes** | the set's name — what an actor's `message` refers to it by |
| `kinds` | **yes** | two or more message kinds, the alternatives of the variant |
| `description` | no | the inbox **in English** — see [Prose](#prose-the-description-fields) |

An actor that holds a set is written like any other actor: `message: screen_update` names the **mailbox**,
and the mailbox still holds one type. What changes is the type — the scaffold writes
`using ScreenUpdate = std::variant<Reading, Gesture>;` into the shared `components/messages/`, so
`on_message` still takes one argument, and a sender posts **one of the kinds** (`std::get<Gesture>`, or by
index), never the set itself. The set's `description` leads that type's doc comment in the header, exactly
as a unit's description leads its class — read back off the tree, a set is the set the person stated.

Three rules hold a set to its own shape, and each is stated in the language of the thing it refuses:

- **At least two kinds, all distinct.** One kind is not a set — the unit already says that with
  `message: <kind>` — so it is refused rather than emitted as a variant of one alternative. A kind named
  twice is refused too: `kinds` is a list of *distinct* kinds.
- **Named by some actor's `message`.** A set nobody receives is a type nothing can be sent, refused like
  any other declaration nothing references.
- **A name of its own.** The kinds become `struct`s, the set becomes a `std::variant` alias, and all of
  them live in the one `messages::` namespace — so a set id that normalises onto another declared name
  (`screen_update` beside a kind, bare message or port value named `ScreenUpdate`) would be one name for
  two shapes, and is refused.

What is **not** a collision is a **kind reused elsewhere**: the kinds are shared types in their own right
(one `struct` each), so another actor may name one kind alone (`message: Gesture`) and receive the same
type the set carries, and one kind may belong to two sets. A set exists only where a design states one:
with no `messages` key the section is empty, and every generated header is byte-for-byte what it was
before — the ordinary one-kind case costs nothing and reads the same.

## `units`

The units of work. One list holds all three kinds — `component`, `actor`, `stage` — because a
composition is read by a person: the fields a kind does not use are simply absent, and the *check*
refuses any field a kind may not carry (so a `component` cannot quietly grow a `message`, which would
make it framework-dependent).

### Common to every unit

| key | required | notes |
| --- | --- | --- |
| `id` | **yes** | the name everything else refers to it by — `wiring`, `uses`, `sends_to`, board facts, resource names. Lower_snake, and **unique**: two units sharing one id make every one of those ambiguous |
| `kind` | **yes** | `component` \| `actor` \| `stage` |
| `description` | no | the unit in **English** — see [Prose](#prose-the-description-fields) |
| `provides` | no | **one line** a reviewer reads: what this unit is for |

### A `component` — a driver on a bus, or pure code

Framework-agnostic by construction: it knows no pins, no address, no task, no mailbox and no framework,
which is what makes it reusable in an application that chose the other one.

| key | required | notes |
| --- | --- | --- |
| `role` | **yes** | `driver` (one device on one bus, reached through a bus handle, needing a seam so a host test can stand in for it) or `library` (pure code: an algorithm, a filter, a codec — no bus, no fake) |
| `source` | no | `existing` (already in the library) \| `stub` (not written yet: a stub to generate and fill) \| `published` (from the ESP Component Registry) |
| `registry` | published only | the fully-qualified registry name, `namespace/name` (`espressif/esp_lcd_st7701`) |
| `bus` | drivers only | the **protocol** it speaks — `i2c`, `spi`, `uart`, `mipi-csi`, `gpio`… |

The rules a component is held to:

- **a `driver` states its `bus`** — a protocol written for the wrong bus never compiles, so an empty one
  is refused. A **`published`** driver is the exception: it is resolved from the registry, and its bus is
  the registry component's own business;
- **a `library` names no bus** — a bus on a component that names no device would be a contract that
  lies, so it is refused;
- **`registry` and `source` agree** — a `published` component *must* carry the registry name it is
  resolved by, and an `existing`/`stub` one must not (a registry name promises a dependency that is not
  one);
- **no composition** — a component carrying `message`, `state`, `uses`, `sends_to`, `pulls` or `pushes`
  is refused. A component holds a device or an algorithm; *how it is driven* is the application's.

A `published` component is a **managed dependency** of the application, not a component of the library:
the scaffold writes it into the application's `main/idf_component.yml`, and the `id` stays the local
name the composition and the wiring speak. A `stub` is the opposite: a component this project writes,
generated from a datasheet or a description and gated by its own host test.

### An `actor` — the `actors` framework

A classical actor: a `Message` type, the state behind it, and the refs it sends to.

| key | required | notes |
| --- | --- | --- |
| `message` | **yes** | the **mailbox type** it receives — *an actor **is** a message type*, and the compile cannot infer one (`spire::Actor` is templated on it). One name, so one type however many kinds of message it carries — a kind (`Tick`), or the id of a declared [message-set](#messages) — see below |
| `state` | no | the state it holds between messages |
| `uses` | no | the **components** (and declared resources) this actor wraps |
| `sends_to` | no | the unit ids it posts to — a **list**, so one sender may post to several, each receiver typed by its own `message` (see [Messages across units](#messages-across-units-one-type-many-actors)) |

**One `message` is a mailbox type, not a count of message kinds.** The field names the type the actor's
mailbox holds, and that mailbox holds exactly **one** type: `spire::Actor<Message>` is templated on a
single `Message`, `on_message` takes that one type, and the FreeRTOS queue behind it stores one
fixed-size slot, so a ref can be sent that type and nothing else. An actor that reacts to several things
— a reading, a command, a timer — therefore does **not** name them separately. It names the one type that
carries all of them, and the *shape* of that type is where the several kinds live: a
`std::variant<Tick, Calibrate, Shutdown>`, or a struct with a tag and a union, dispatched inside the one
`on_message`. So the composition names the **mailbox**, and the fill decides its shape — the
`struct Tick {};` the scaffold writes into the shared `components/messages/include/messages.hpp` is
written out as what the actor actually receives. The worked example fills one in, ten kinds deep — see
[One mailbox type, many kinds](#one-mailbox-type-many-kinds--filled-in).

The one shape the composition **does** state is the sum type itself: when the several kinds are known
*when the design is written* — the caller and the callee of one conversation — they are declared once in
[`messages`](#messages) under a name, and every actor that receives them says `message: <set id>`. Then
the mailbox is a `std::variant` over the kinds from the start, and the fill is about what the actor does
with each one rather than about the type at all.

What keeps that from being unbounded is the **compile**, not `validate`: `spire::Actor`'s
`static_assert`s require the type to be trivially copyable and default-constructible (a message is
*copied* into a queue), so a sum type is fine as long as every alternative is plain, and its first
alternative — or a leading `std::monostate` — is the default the mailbox receives into. `validate` checks
only that the field is **stated**; whether the name is a type the mailbox can hold is the type-level
half it deliberately leaves to the compiler (see [What `validate` refuses](#what-validate-refuses)).

### A `stage` — the `ramen` framework

A dataflow stage: what it pulls and what it pushes.

| key | required | notes |
| --- | --- | --- |
| `pulls` | one of the two | the port it pulls from |
| `pushes` | one of the two | the port it pushes to |
| `uses` | no | the components it wraps |

A stage needs **at least one** port: a *source* stage (a frame pump reading a camera) has only `pushes`,
a *sink* stage has only `pulls`, and a stage with neither is not a stage but a unit nobody can place in
the chain. A `ramen` application also refuses a **cycle** in the wiring — ramen pushes synchronously, so
a cycle is unbounded recursion on one stack rather than a loop, which is what the actor framework is
for.

`uses` (on an actor or a stage) is **the relationship a composition unit has to the framework-agnostic
parts it wraps** — an actor sampling `sps30`, a stage reading `camera`. It is stated rather than implied
because it is half of what a reviewer checks (*"`air_quality` wraps `moving_average`"*) and because it
is what makes a purely-internal component *referenced*. Each name must be a `component` in this spec or
a **declared resource**; naming a device nobody declared is the refusal — an application composes
against the hardware it has, not the hardware it wishes for.

### Facet annotations on a unit

A facet is a **lens** over the composition, not a second artifact — so what a unit *does* for a facet is
an annotation on that unit, and only the product-wide facts live in the facet blocks.

| key | facet | values |
| --- | --- | --- |
| `sleeps` | power | `true` — this unit may sleep between messages, which is what makes a budget reachable |
| `wake` | power | what wakes it: `timer@1s`, `gpio@button`, `message` |
| `storage_role` | storage | `buffer` (holds data until another unit drains it), `medium`, `source`, `sink` |
| `network_role` | networking | `uplink`, `server`, `offline` |

A unit that sleeps is woken by its `wake`; an always-on unit that sends to it is a composition that
cannot run, which is the power facet's arithmetic to hold.

## `wiring`

`from -> to`, by unit id, as a bare string:

```yaml
wiring:
  - sampler -> air_quality
  - touch -> view
```

The edges are **stated rather than derived** — from `sends_to`, and from a stage's ports — because the
edges *are* what a reviewer checks, and because a `ramen` stage has no `sends_to` to derive them from. How
the two planes relate for `actors` is
[Messages across units](#messages-across-units-one-type-many-actors).
Both endpoints must be units of the composition, and neither may be a `component`: a component has no
ports and no mailbox, so wiring one is refused with the remedy — *"wire the unit that uses it, and let
that unit list it in its `uses`"*. An edge that is not `from -> to` is refused with its own remedy —
*"write them as `from -> to`, by unit id"* — and any name that resolves to no unit is refused. For
`ramen`, the graph must be acyclic; for `actors`, a cycle is legal and is exactly what the name registry
resolves at fill time.

## Messages across units: one type, many actors

Messaging has two facts that are easy to run together, and they are different mechanisms. A message
**type** may be **shared** — two actors can take the same message — and a message may be **fanned out** —
one unit may post to several. Both are ordinary here; neither is a broadcast. A third fact belongs to the
*receiver* rather than to the edges: one mailbox may carry **several kinds**, and when the design knows
which, they are declared once as a [`messages`](#messages) **message-set** and every actor that receives
them names that one id.

### One type, several actors

A name two actors use is one declaration, because the scaffold de-duplicates by type and notes who uses
it. `docs/example-composition.spire` does this — `sampler` and `power` are both `message: Tick` — and the
generated stub says so:

```cpp
/// TODO: what a `Tick` is — the fields that cross the edge, as plain values.
///
/// Named by sampler, power.
struct Tick {};
```

That `Named by …` line is the point: the type belongs to the **edge**, so whoever fills it writes for two
readers at once. If the two need different fields, that is the moment to give them **two** types rather
than one that half-serves each.

Sharing a type shares no mailbox. Each actor has its own queue and therefore its own ref, and two actors
that take `Tick` are two `spire::ActorRef<messages::Tick>` values told apart by the mailbox they address.
The composition's *name* is what picks the actor: `Registry::get<messages::Tick>("sampler")` and the same
for `"power"` both resolve — both were registered under that one *type tag* — while `get<Reading>("power")`
refuses, because the name matches and the tag does not.

This is where a message type is deliberately **unlike a resource**: a declared resource has **exactly one
owner** ([rule 7](#what-validate-refuses)), because a device has one driver and one place its state lives.
A message type may be any number of actors' message, because it is a *shape*, not a thing.

### One sender, several receivers

`sends_to` is a **list**, so fan-out is stated in the design rather than improvised in code:

```yaml
- id: touch
  kind: actor
  message: Gesture
  sends_to: [view, sampler]
```

Each target is resolved on its own terms, and the receivers need not take the same type: `touch` posts one
of the `screen_update` kinds into `view`'s mailbox and a `Tick` to `sampler`, so it holds
`ActorRef<ScreenUpdate>` **and** `ActorRef<Tick>` — one ref per edge, typed by that edge's *receiver*,
never by the sender. Those refs are the sender's
**constructor arguments**; `main/` spawns every receiver **before** the actor that sends to it, and the one
peer a sender cannot be constructed with — the far side of a cycle — is resolved by name in `init()`
through the `Registry` it was handed.

There is **no broadcast**: fan-out is one `send()` per target, so it is best-effort *per target* and not
atomic. A full mailbox refuses rather than blocks, while the other sends still go, and each receiver sees
messages in its own queue's order — never a global order.

`wiring` and `sends_to` are **two planes that say the same thing** to two different readers: the edges are
what a reviewer checks, and `sends_to` is what a sender's refs are derived from. Both are checked for
*resolving* — an edge names two units, a `sends_to` names a unit — and both count as a reference for
[rule 6](#what-validate-refuses). What `validate` does **not** do today is hold the two planes to each
other: it does not require a `sends_to` target to have a matching edge, and it does not refuse a receiver
named twice. Keeping them in step is a review discipline, not a rule.

## The facets

Each facet is optional, is decided and checked differently, and lands nowhere of its own — it is a lens
over the units above. Every field within a facet is optional too: an application that says nothing about
power has no power facet, which is a real answer rather than a gap.

| block | key | values |
| --- | --- | --- |
| `power` | `budget_mah` | the energy budget, in mAh |
| | `target_days` | how long the composition must run on that budget |
| `storage` | `medium` | `sd` \| `flash` \| `nvs` \| `ram` |
| | `retention` | `until_uploaded` \| `7d` \| `forever` |
| `network` | `transport` | `wifi` \| `ethernet` \| `lora` \| `cellular` |
| | `provision` | `softap` \| `ble` \| `dhcp` \| `static` |
| `security` | `requires` | a list of the guarantees the composition must hold: `tls`, `secure_boot`, `encrypted_nvs` |

`security.requires` is a **list** rather than a block of fields, because security here is a *policy* —
and a policy nobody can read off a list is a policy nobody checks. Note that a facet name and a unit id
may collide legally: a unit `power` and the `power:` facet block are different planes — a unit and a
lens — and a composition may have both, as `docs/example-composition.spire` does.

## Prose: the `description` fields

A composition states the design **twice**: as structure (everything above) and as **English**. The whole
application has a `description`, each unit may have its own, and so may each declared
[`messages`](#messages) entry — the inbox in English. They exist so the structure and the sentence that
explains it cannot drift apart in two files.

```yaml
description: |
  A room air-quality monitor. It samples particulate matter and temperature/humidity once a second,
  keeps a short rolling average and shows it on the board's screen.

units:
  - id: air_quality
    kind: actor
    message: Reading
    description: folds each reading into the rolling average and posts it to the view.
    state: the rolling average
```

Three properties matter:

- **It is prose, not machinery.** No rule reads it — `validate` is deliberately blind to it, because a
  rule over free text would be a rule over nothing. What must *not* live here is computation: a guard, an
  arithmetic step, a page transition belongs in the unit's code, in the language that has to run it.
- **It is carried verbatim into the generated tree.** The whole-application `description` becomes the
  comment block at the top of `main/main.cpp` (the one file a person opens first, substituted by name
  through `DESCRIPTION_BLOCK_TOKEN`), each unit's `description` becomes that unit's generated class
  doc-comment, and a message-set's leads the doc-comment of the variant the scaffold writes for it. So a
  person codes against the same words the design was reviewed against, rather than a paraphrase.
- **It is optional.** A driver's `provides` line is usually enough; an actor almost always has one,
  because *"what does this actor do on receiving X"* is a question its message type and state hint
  cannot answer.

## Omission

Optional keys are **left out** when they are empty rather than written as blanks, so a composition (and
the JSON record rendered from it) stays exactly what it was before a later optional field existed. The
exceptions worth knowing:

- `board.bsp` is **always written**, even when empty, because the review UI decodes it as a non-optional
  string — an omitted key would be an undecodable spec;
- `resources` entries always write `roles` as a **list**, even when the input used the `role:` sugar;
- a `sleeps: false` writes no key at all.

Because omitted optionals parse (`#[serde(default)]`), a composition that says nothing about
`board_facts`, `resources` or a facet is complete, not truncated.

## What `validate` refuses

`validate` (`application_spec::validate`) is the whole contract, and it lives in code rather than in a
prompt — so every rule below is provable with no model at all. It returns **every** problem rather than
the first, because a composition is repaired in one pass: someone told all of them at once fixes them
together, and someone told one at a time spends rounds finding out where it went wrong. A project reads
a file through `idf_projects::design_that_holds_together`, which refuses a file that parses but does not
hold together, naming the file and listing the broken rules:

> `composition.spire` states a design that does not hold together, so it is not one this project can be
> built from:
>   - …

The precondition and the seven rules:

0. **It composes something.** No units at all is refused: *"the spec has no units — an application is a
   composition, and this composes nothing."*
1. **One framework, one kind per unit, one unit per id.** Every non-component unit must be the kind the
   framework implies (`actors`→`actor`, `ramen`→`stage`); a mixed application is refused by name. Two
   units sharing an id are refused, because an id is what the wiring, the board facts, `uses` and
   `sends_to` refer to, and `unit(id)` would answer with the first — the second would read as though it
   did not exist.
2. **Every actor states a `message`; every stage states a port.** An actor *is* a message type, and the
   compile cannot infer one; a stage with neither `pulls` nor `pushes` is nowhere in the chain. The rule
   is that the field is **stated** — one name. How many kinds of message that one type carries is its
   *shape*: a fill-phase decision this rule never sees — unless the shape is declared, in which case the
   [`messages`](#messages) section is held to its own rules (a non-empty, unique id; at least two distinct
   kinds; and an id that does not collide with another declared name). See
   [An `actor`](#an-actor--the-actors-framework).
3. **Every reference resolves, and a `ramen` dataflow has no cycle.** Each `wiring` edge must be
   `from -> to` and must name two units that exist (neither a component); each `sends_to` must name a
   unit; each `uses` must name a `component` in this spec or a **declared resource** — anything else is
   *"a unit cannot reach hardware this application does not have"*. A cycle is refused for `ramen` with
   the path that closes it.
4. **A component is framework-agnostic, and its role decides what it may carry.** A component with no
   `role` is refused; a `driver` states a `bus` (unless `published`); a `library` names none; a
   `published` component carries its `registry` name and an `existing`/`stub` one does not; and any
   component carrying `message`, `state`, `ports`, `sends_to` or `uses` is refused.
5. **Every board fact reaches a driver.** A board fact naming a non-driver unit, or a unit that does not
   exist, is refused — a board fact is how a *driver* is reached.
6. **Nothing invented and unused.** A unit referenced by no wiring edge, no `sends_to`, no `uses` and no
   board fact is refused: *"an invented unit nobody uses is either a mistake or work that belongs to
   another spec."*
   A message-set is referenced the same way — an actor's `message` names it — so a set no actor receives
   is refused for the same reason: a set nobody receives is a type nothing can be sent.
7. **The declared resources hold together.** A resource `name` must be a bare identifier and unique; it
   must name at least one capability and every `role` must be a path the vocabulary knows; it must state
   a `via`; a `via: board` resource may not share its name with a unit; a `library`/`published` resource
   must have a driver component of that name; an address stated both here and in a board fact must
   agree; and each resource must have **exactly one owner** — the unit whose `uses` names it. A resource
   nobody owns was declared for nothing; a resource two units own is *shared*, which the actor model
   forbids (a device has one driver and one place its state lives; every other unit reaches it by
   messaging its owner).

What `validate` deliberately does **not** promise is the type-level half — that a message is trivially
copyable, that a port carries the type its neighbour expects. That is the compiler's, and the host test
per component is the cheap gate that catches it before a chip build. It also does not hold `wiring` and
`sends_to` to each other — a `sends_to` target with no matching edge, and a receiver named twice, are both
accepted; see [Messages across units](#messages-across-units-one-type-many-actors).

## Reading and writing

| where | what |
| --- | --- |
| `application_spec::parse_composition` | parse a `composition.spire` (or any YAML/JSON spec text) into an `ApplicationSpec`, strictly |
| `application_spec::parse_spec` | the same for the JSON record, tolerating a ```` ```json ```` fence |
| `idf_projects::render_composition` | render an `ApplicationSpec` back to YAML — what the scaffold writes |
| `idf_projects::read_application` | read a project's design **without writing**: `composition.spire` when it is there, otherwise `SPIRE.application.json` |
| `idf_projects::read_application_and_sync_record` | the same, and rewrite the record from the composition when they disagree |
| `idf_projects::design_that_holds_together` | parse-then-`validate`, refusing by file name with every broken rule listed |

**Nothing regenerates over what a person wrote.** The composition is written **once**, by the scaffold,
from the *validated* spec — never echoed from the text it was read from, because a file written from
unvalidated text could differ from the composition that was approved. `render_composition` cannot carry a
comment, so a comment a person adds to the file is theirs from then on. Editing the composition and
re-running is the way a design changes; re-scaffolding is not a redesign.

**A file that is there and does not parse is an error, never a fall-through to the JSON record.** A
broken composition is not a composition, and quietly reading the older record in its place is how a
project builds something other than what is written in the file.

## A worked example

`docs/example-composition.spire` is a complete, commented composition — a room air-quality monitor — and
reading it against this document is the fastest way to see the format hold together. Its shape:

```yaml
framework: actors                     # a touch UI at human timescale
justification: a touch UI, a reading a second and a battery gauge — interactive, human timescale

board:
  chip: esp32s3
  bsp: m5stack_core_s3
  hal: m5unified

resources:
  - { name: display,     roles: [media.display],                                   via: board }
  - { name: touch_panel, roles: [media.touch],           bus: i2c_internal, address: "0x38", via: board }
  - { name: battery,     roles: [power.battery],                                   via: board }
  - { name: sps30,       roles: [sensing.particulate],   bus: i2c, address: "0x69", via: library }
  - { name: sht20,       roles: [sensing.temperature, sensing.humidity], bus: i2c, address: "0x40", via: library }
```

and how each rule is met:

- **The framework is stated once and everything follows it.** `actors`, and every non-component unit is
  an `actor`; the three `component`s are the framework-agnostic parts they wrap.
- **The board's own parts are resources, not units.** `display`, `touch_panel` and `battery` are
  `via: board`: the BSP reaches them, so no component of the project drives them. Each still has one
  owner below.
- **An added part shares its driver's name — one device, one word.** `sps30` and `sht20` are both a
  declared resource and the `component` that drives them, so the word a `uses` speaks is the same word.
- **The address is stated once per device, in two planes that agree.** `sht20` is `0x40` in `resources`
  *and* in `board_facts` — the same literal, which is what the check requires.
- **Every declared resource has exactly one owner.** `display`→`view`, `touch_panel`→`touch`,
  `battery`→`power`, `sps30`&`sht20`→`sampler` — each held in one actor's state; `power` reports the
  battery *to* `view` rather than reading the screen itself.
- **Nothing is invented or unused.** `moving_average` is referenced by `air_quality`'s `uses`; every unit
  is an endpoint of a wiring edge, a `sends_to` target, a `uses` name or a board fact.
- **The facets annotate units, and state only product-wide facts.** The unit `power` carries
  `sleeps`/`wake` for the power facet; the `power:` block states the budget and the target — the same
  word `power` in two planes, which is legal.
- **The English travels with the structure.** The whole-application `description` and every actor's
  `description` become the generated code's doc comments, so the sentence a person wrote is the sentence
  in the file.

### One mailbox type, many kinds — filled in

[An `actor`](#an-actor--the-actors-framework) states **one** `message`, however many kinds of message it
reacts to; this is what that means on the fill side. A supervisor that handles ten kinds of sub-message
still names one mailbox type:

```yaml
- id: supervisor
  kind: actor
  message: SupervisorMessage        # one name — the mailbox type, however many kinds it carries
  description: routes each sub-message to the unit that handles it
```

The fill is where the kinds are named, *unless the design already named them*: when the several kinds are
known at design time they are a declared [`messages`](#messages) set, and the scaffold writes the
`std::variant` over them into the shared header itself — leaving the fill nothing to decide about the type
and everything to decide about the routing. The rest of this section is the case where they are **not**
declared, and the mailbox's shape is the fill's.

The scaffold writes that one name as an empty struct in the **shared** header, with the units that named
it:

```cpp
// components/messages/include/messages.hpp
namespace messages {
/// TODO: what a `SupervisorMessage` is — the fields that cross the edge, as plain values.
///
/// Named by supervisor.
struct SupervisorMessage {};
}
```

The fill replaces the stub and declares the sub-messages beside it, because the kinds live in the type's
**shape** and not in ten composition fields:

```cpp
// components/messages/include/messages.hpp
#include <variant>

namespace messages {
struct Reading   { float pm25 = 0.0f; };   // the first alternative is the mailbox's default
struct Command   { int code = 0; };
struct Heartbeat { uint32_t seq = 0; };
// …the rest of the ten…

using SupervisorMessage = std::variant<Reading, Command, Heartbeat /* … */>;
}
```

and `on_message` keeps the **one** signature the framework gave it, routing by alternative:

```cpp
// the standard C++17 helper, not a framework type: one visitor out of several lambdas
template <class... Ts> struct overloaded : Ts... { using Ts::operator()...; };
template <class... Ts> overloaded(Ts...) -> overloaded<Ts...>;

void on_message(const messages::SupervisorMessage& message) override {
    std::visit(overloaded{
        [](const messages::Reading&   r) { /* route it */ },
        [](const messages::Command&   c) { /* route it */ },
        [](const messages::Heartbeat& h) { /* route it */ },
    }, message);
}
```

Two things bite at ten kinds. Every alternative must be **plain** — trivially copyable, so no owning
pointer and no `std::string`: a sub-message that needs variable-length data carries a fixed-size buffer, or
keeps that part in the component the actor wraps. And the mailbox holds `depth × sizeof(SupervisorMessage)`
— the *largest* alternative plus a small discriminant — at a depth that defaults to **8**, so the largest
sub-message is where the actor's RAM goes.

## Checklist before committing a composition

- [ ] `framework` is one of `actors` / `ramen`, and every non-component unit is the kind it implies.
- [ ] Every `id` is unique and lower_snake; every enum value is lowercase (`component`, `driver`,
      `stub`…).
- [ ] `board.chip` is stated; `bsp`/`hal` are stated only if real.
- [ ] Every device a unit reaches is declared in `resources`, with a `via` and at least one known
      capability path in `roles`.
- [ ] Each resource has **exactly one** owning unit, and (for `library`/`published`) a driver component
      of the same name.
- [ ] Every `wiring` edge is `from -> to` between units that exist, and a `ramen` graph is acyclic.
- [ ] Every `sends_to` target also appears as a `wiring` edge, with no receiver named twice — a discipline
      `validate` does not enforce today.
- [ ] Every actor's `message` names one **mailbox type** — even when the actor reacts to several kinds of
      message (`std::variant<…>`) — and every `stage` states at least one port.
- [ ] Every declared message-set has two or more distinct kinds, and some actor's `message` names it — and
      its id is not also the name of a kind, a bare message or a port value.
- [ ] Every `component` is framework-agnostic — no `message`, `state`, `uses`, `sends_to` or ports — and
      its `role`, `bus` and `source` agree.
- [ ] No unit is left referenced by nothing.
- [ ] `description` reads as prose a person would code against, and carries no guard or arithmetic.

## Where to look

- `docs/example-composition.spire` — a fully annotated composition.
- `docs/esp-idf-architecture.md` §3b (`createProject/ParseComposition`) and §3c (*What a composition
  states*) — where this file sits in the pipeline.
- `crates/spire-code/src/build/application_spec.rs` — the types, `parse_composition`, `parse_spec`,
  `validate`.
- `crates/spire-code/src/build/idf_projects.rs` — `COMPOSITION_FILE`, `render_composition`,
  `read_application`, `design_that_holds_together`, and the `DESCRIPTION_BLOCK_TOKEN` substitution.
- `crates/spire-code/src/capability_vocabulary.rs` — the capability paths a `roles:` entry may name.
