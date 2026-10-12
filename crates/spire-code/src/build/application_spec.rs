// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **application spec** — what an application *is*, decided before any of it is written.
//!
//! An application is not assembled out of parts whose shape somebody already knows. It is a
//! **composition**: a few units of work — drivers and pure algorithms, which are framework-agnostic —
//! arranged into loops, messages and wiring, which is the application's own. Deciding that
//! arrangement is the **composition**, and this module is its artifact: a typed spec a person writes
//! and a tool can *check*, because an arrangement nobody checks is an arrangement nobody can review.
//! Its sibling here is the **design brief** ([`DesignBrief`]) — a description read into capabilities
//! and units, one step earlier and deliberately smaller.
//!
//! # Why a spec and not a paragraph
//!
//! A paragraph cannot be reviewed — "read the SPS30 on a timer and show the average" is silent about
//! who owns the timer, what the average is computed over, and which pins the sensor is on. A spec
//! names every one of those, and each answer is either right or wrong:
//!
//! * **units** — a `component` (a driver on a bus, or a pure library), an `actor` (a `Message` and the
//!   state behind it), or a ramen `stage` (a pair of ports);
//! * **wiring** — which unit talks to which, stated rather than implied;
//! * **resources** — the devices the application declares it composes against, each with the
//!   capability — or capabilities — it *is* and how it is reached (a part of the board, or a driver
//!   this project has, writes, or depends on);
//! * **board facts** — the pins, buses and addresses, which belong to the **application** and never to
//!   a component (the rule the whole component library rests on);
//! * the **board** itself, and the **framework** the above is written for.
//!
//! That is the review: a person reads that `sps30` is a driver, `air_quality` wraps `moving_average`,
//! and `view` renders — and can say "yes" or "no" in ten seconds, without reading a line of generated
//! code.
//!
//! # The framework comes first, and it is derived rather than guessed
//!
//! The framework shapes everything downstream — the `main/` template, the decomposition idiom, the
//! prompts — so it is decided before the decomposition. It is best **derived from the description and
//! confirmed**, not picked blind, because the shape of the decomposition *is* the framework:
//!
//! * **human timescale, interactive, command/event-driven → [`ApplicationFramework::Actors`]** — a
//!   touch screen, a menu, a reading that updates once a second, a device that reacts to commands and
//!   timers while holding state;
//! * **machine timescale, high-rate data flowing through stages → [`ApplicationFramework::Ramen`]** —
//!   video frames, kHz samples, an inference pipeline, where a value is *moved and transformed*
//!   rather than *waited on*.
//!
//! A *stream* is a component's job either way — a Klipper status channel and a sensor bus are both
//! drivers — so what decides the framework is whether the application **reacts** to the data or
//! **pumps** it through stages. The framework is stated in the composition a person writes, and the
//! rule is kept here because [`validate`] is what holds a composition to it.
//!
//! # What validation can and cannot promise
//!
//! [`validate`] checks eight things, each one a failure that would otherwise reach the compiler or a
//! board: no mixed frameworks, a message for every actor, every edge resolving, components that are
//! framework-agnostic, board facts that belong to a driver, nothing invented-and-unused, a
//! **declared resource** behind every device a unit reaches, and **exactly one owner** for each such
//! resource — a device is held in one actor's state, and every other actor messages that owner.
//!
//! What it deliberately does **not** promise is the type-level half — that a message is trivially
//! copyable and default-constructible. A spec names a message; whether that name is a type the
//! mailbox can hold is not a spec's business, and the gate for it is the compile: `spire::Actor`'s
//! `static_assert`s and the `actors` component's host test, which runs on this machine.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::build::idf_projects::message_type_name;

/// The `CMakeLists.txt` marker that records an application's framework.
///
/// Stated rather than inferred, for the same reason a component states its kind: a later reader — a
/// prompt, a person, another tool — has to be able to tell what an application is without deducing it
/// from the shape of `main/`.
pub const APPLICATION_FRAMEWORK_KEY: &str = "SPIRE_APPLICATION_FRAMEWORK";

/// The application framework a spec is written for.
///
/// Adding one is adding a variant *and* the three things it implies: a `main/` template, a
/// decomposition idiom, and a host test that can be run on this machine. That is why this is an enum
/// rather than a free string — a framework the tool does not know is a refusal, not a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplicationFramework {
    /// Synchronous, inline dataflow: a value is pushed through wired stages on the producer's own
    /// stack. Machine timescale.
    Ramen,
    /// A mailbox, a task and `on_message`: units react to messages while holding their own state.
    /// Human timescale.
    Actors,
}

impl ApplicationFramework {
    pub fn as_str(self) -> &'static str {
        match self {
            ApplicationFramework::Ramen => "ramen",
            ApplicationFramework::Actors => "actors",
        }
    }

    /// The kind a unit has under this framework — the one thing the model does not get to choose.
    pub fn unit_kind(self) -> UnitKind {
        match self {
            ApplicationFramework::Ramen => UnitKind::Stage,
            ApplicationFramework::Actors => UnitKind::Actor,
        }
    }

    /// The line an application states its framework with, as the scaffold writes it.
    pub fn marker_line(self) -> String {
        format!("set({APPLICATION_FRAMEWORK_KEY} {})", self.as_str())
    }
}

/// What a unit of work is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnitKind {
    /// **Framework-agnostic**: a driver on a bus, or pure code. Knows no pins, no task, no mailbox and
    /// no framework — which is what makes it reusable in an application that chose the other one.
    Component,
    /// A classical actor: a `Message` type, the state behind it, and the refs it sends to.
    Actor,
    /// A ramen dataflow stage: what it pulls and what it pushes.
    Stage,
}

/// What a component *is*, when it is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentRole {
    /// One device on one bus: a protocol, reached through a bus handle. It needs a seam so a host test
    /// can stand in for the device.
    Driver,
    /// Pure code: an algorithm, a filter, a codec. It has no bus and needs no fake.
    Library,
}

/// Where a component's code comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnitSource {
    /// Already in the library the application is built against — the composition names it.
    Existing,
    /// Not written yet: a stub to generate and fill, from a datasheet or a description.
    Stub,
    /// **Published on the ESP Component Registry** — not written by this project at all. The
    /// application *depends on* it by its registry name, and the scaffold writes that into the
    /// application's manifest (`main/idf_component.yml`) rather than stubbing it into the library.
    Published,
}

/// One unit of work.
///
/// One struct rather than three, because a spec is read by a person: the fields a kind does not use
/// are `None` or empty, and [`validate`] refuses any field a kind may not carry — so a component
/// cannot quietly grow a `message`, which would make it framework-dependent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    /// The name the spec, the wiring and the board facts refer to it by. Becomes the component
    /// directory or the class name, so it is lower_snake.
    pub id: String,
    pub kind: UnitKind,
    /// The unit in **English**: what this actor (or stage, or driver) does, in the person's own words.
    ///
    /// The per-unit half of [`ApplicationSpec::description`]: the slice of the design that belongs to
    /// this one unit, carried verbatim into the unit's generated header as its doc comment. Optional
    /// because a driver's `provides` line is usually all it needs; an actor almost always has one, since
    /// "what does this actor do on receiving X" is a question the fields — a message type and a state
    /// hint — cannot answer. Prose only: no guard, no arithmetic, no rendering, no page transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Components only: driver or library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ComponentRole>,
    /// Components only: existing, a stub to write, or published upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<UnitSource>,
    /// `published` components only: the fully-qualified registry name (`espressif/esp_lcd_st7701`),
    /// as components.espressif.com names it.
    ///
    /// It is a **managed dependency** of the application, not a component of the library — so it is
    /// resolved at build time by the application's manifest and is never stubbed. The `id` stays the
    /// local name the composition and the wiring speak.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// One line a reviewer reads: what this unit is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provides: Option<String>,
    /// Drivers only: the bus it is reached through (`i2c`, `spi`, `uart`, `mipi-csi`, `gpio`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
    /// Actors and stages only: the **components this unit uses**.
    ///
    /// The relationship a composition unit has to the framework-agnostic parts it wraps — an actor
    /// sampling `sps30`, a ramen source reading `camera`. Stated rather than implied, because it is
    /// half of what a reviewer checks ("`air_quality` wraps `moving_average`") and because it is the
    /// only thing that makes a purely-internal component *referenced*
    /// (see [`validate`]'s last rule).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    /// Actors only: the message type it receives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Actors only: the state it holds between messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Actors only: the units it posts to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends_to: Vec<String>,
    /// Stages only: the port it pulls from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulls: Option<String>,
    /// Stages only: the port it pushes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushes: Option<String>,
    /// **Power facet**: the unit may sleep between messages, which is what makes a budget reachable.
    /// A unit that sleeps is woken by its [`wake`](Unit::wake); an always-on unit that sends to it is
    /// a composition that cannot run, which is a rule [`validate`] will hold.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sleeps: bool,
    /// **Power facet**: what wakes a sleeping unit — `timer@1s`, `gpio@button`, `message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<String>,
    /// **Storage facet**: what the unit is *for* in the storage story — `buffer`, `medium`, `source`,
    /// `sink`. A `buffer` is a unit that holds data until another unit drains it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_role: Option<String>,
    /// **Networking facet**: what the unit is *for* in the network story — `uplink`, `server`,
    /// `offline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_role: Option<String>,
}

/// The board an application is built for: the silicon, its support package, and the abstraction the
/// application is written against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardChoice {
    /// `esp32s3`, `esp32p4`… — decides the compile target and the IDF version.
    pub chip: String,
    /// The BSP, as the registry names it (`espressif/m5stack_core_s3`, `waveshare/esp32_p4_nano`). It
    /// carries the pins and the dependency set, and it is written **verbatim** as a dependency key in
    /// the application's `main/idf_component.yml`.
    ///
    /// **Optional, and that is not laxity.** Most boards have no published BSP — the catalogue has
    /// `bsp:` on exactly one of ten — and an empty one means *Spire generates its own backend*, which
    /// is a real answer rather than a gap. Requiring it was how a board with none ended up with a
    /// hand-typed name, and a hand-typed name is a manifest key nobody validated.
    ///
    /// `default` so a caller that leaves the key out gets "none" rather than a parse failure; the field
    /// is still **serialized** when empty, because the review screen decodes it as a non-optional
    /// string and an omitted key would be an undecodable spec.
    #[serde(default)]
    pub bsp: String,
    /// The board **abstraction** the application is written against — `m5unified`, `bsp`, or empty for
    /// none. Free rather than an enum: these are vendor names, and a new one costs no code.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hal: String,
}

/// One board fact: how the application reaches a device it drives.
///
/// The application's, never a component's. A component takes a bus handle and knows no address; the
/// address lives here, in the one place that knows the product.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardFact {
    pub bus: String,
    /// The **unit id** of the driver component this fact reaches.
    pub device: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub address: String,
}

/// How a declared [`Resource`] is reached by the composition.
///
/// The three answers are the three places the *code that talks to the part* can be, and they are not
/// interchangeable downstream: a `board` part is reached through the BSP or the board abstraction and
/// generates no component of its own; a `library` part is driven by a component of this project
/// (already in the library, or a stub the design asks to be written); a `published` one is driven by a
/// **managed dependency** the manifest resolves from the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceVia {
    /// A part **of the board** — the BSP or the board abstraction above is what reaches it, so no
    /// component of this project drives it.
    Board,
    /// A part driven by a **component of the library** the application is built against: the one the
    /// design names, or the stub it asks to be written.
    Library,
    /// A part driven by a **published** component — a managed dependency of the application.
    Published,
}

/// One **resource**: a device the composition may reach, **declared** rather than invented.
///
/// This is the application's contribution to the layer between a board's capabilities and the units
/// that use them. An application does not invent hardware: it says which devices it composes against
/// (`name`), what each one **is** as the capabilities the vocabulary knows (`roles`), and where the
/// code that talks to it comes from ([`ResourceVia`], with the `bus`/`address` when it has them). Every
/// unit's `uses` then reaches a resource *declared here* — so a model that invents a sensor is refused
/// by the check rather than by the board, and the set a person reviews is the set the application has.
///
/// A resource is not a unit. A board's own part (`via: board`) has no component at all, and a part the
/// application adds (`via: library`/`published`) shares its name with the driver that drives it: one
/// device, one word, because that is what a `uses` speaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// The word the composition speaks, and what a unit's `uses` names. Lower_snake, and the same word
    /// as its driver component's id when it has one.
    pub name: String,
    /// What the part **is**, as the capability paths the vocabulary knows (`sensing.particulate`,
    /// `media.touch`, `timekeeping.rtc`). A device may be more than one capability at once — the
    /// SHT20 is `sensing.temperature` *and* `sensing.humidity` — which is why this is a **list**: one
    /// device, one resource, every capability it provides. A name the vocabulary does not know is
    /// refused rather than read as "this application has none" — which is the whole reason the
    /// vocabulary exists.
    ///
    /// The wire shape is `roles: [...]`. A bare `role: "sensing.temperature"` is accepted as **sugar**
    /// for a one-element list, so a spec written before multi-role parts still parses.
    #[serde(alias = "role", default, deserialize_with = "de_roles")]
    pub roles: Vec<String>,
    /// Where the code that talks to it comes from — see [`ResourceVia`]. Absent is a problem
    /// [`validate`] states rather than a default, because the choice decides whether a component is
    /// designed at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<ResourceVia>,
    /// The **wiring function** it is reached on (`grove.a`, `i2c_internal`) — a function of the board's
    /// `pins:`, not a protocol. A driver's own `bus` is the protocol it speaks; this is where the wires
    /// are, which is why the two are different fields that never disagree.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bus: String,
    /// Its I²C address, when it has one. A resource and the board fact for the same driver state one
    /// address, so when both are present they have to agree — see [`validate`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub address: String,
}

/// Read a resource's `roles` from either wire shape: the canonical `roles: [a, b]` list, or a bare
/// `role: "a"` — the older, single-capability form — as sugar for `[a]`.
///
/// The two are *one* field, not two: a device that is one capability is the common case, and reading
/// the scalar as a one-element list keeps every spec and fixture written before multi-role parts
/// valid, while the canonical output is always the list. A missing key is handled by `default`
/// (an empty list), which [`validate`] then refuses — a resource that names no capability.
fn de_roles<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(role) => vec![role],
        OneOrMany::Many(roles) => roles,
    })
}

/// The **facets**: power, storage, networking and security, as *lenses* over the composition.
///
/// The composition is one body; a facet is a way of looking at it, not a second artifact. Power is
/// arithmetic and policy — a budget, and which units may sleep; storage, networking and security are
/// policy the composition has to hold. None of them is decided as a decomposition and none of them
/// lands anywhere of its own: a facet **converges into units**, which is why what a unit *does* for a
/// facet is an annotation on that unit (`Unit::sleeps`, `Unit::storage_role`, `Unit::network_role`)
/// and only the product-wide facts live in these blocks.
///
/// Every block is optional and every field is optional: an application that says nothing about power
/// has no power facet, which is a real answer rather than a gap.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerFacet {
    /// The energy budget, in mAh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_mah: Option<u32>,
    /// How long the composition must run on that budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_days: Option<u32>,
}

/// The **storage** facet: where the composition keeps things, and for how long.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageFacet {
    /// The medium — `sd`, `flash`, `nvs`, `ram`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<String>,
    /// How long it keeps them — `until_uploaded`, `7d`, `forever`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<String>,
}

/// The **networking** facet: how the composition reaches the world, and how it is provisioned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkFacet {
    /// The transport — `wifi`, `ethernet`, `lora`, `cellular`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// How the device is set up — `softap`, `ble`, `dhcp`, `static`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision: Option<String>,
}

/// The **security** facet: the guarantees the composition must hold, named rather than assumed.
///
/// A list rather than a block of fields, because security here is a *policy* — `tls`, `secure_boot`,
/// `encrypted_nvs` — and a policy nobody can read off a list is a policy nobody checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityFacet {
    /// The guarantees the composition must hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

/// A **message-set**: the several kinds an actor receives, named once and shared.
///
/// An actor is *a message type* — one `struct`, one mailbox, one `on_message`. But an actor often reacts
/// to more than one *kind* of thing (a supervisor's `Reading`, `Command`, `Heartbeat`), and giving each
/// kind its own actor would name the same task three times. A message-set is that actor's whole inbox
/// stated as a list: each kind is a **shared type** the scaffold declares (one `struct` each, in
/// `components/messages/`), and the set itself becomes the `std::variant` of them — so the actor is still
/// `spire::Actor<messages::Supervision>`, one mailbox, and it dispatches on which kind arrived.
///
/// It is a **top-level block** rather than a field on the actor because a set is shared: two actors that
/// receive the same kinds name one set, and the composite type lives with the kinds rather than inside
/// either actor — the same reason a message type cannot live in one actor's component.
///
/// A single-kind message is **not** a set: `message: Tick` states it directly, and the scaffold writes one
/// `struct` named `Tick`. A set is for the multi-kind case, and [`validate`] refuses one with fewer than
/// two kinds — `message: <kind>` is the shorter way to say one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSet {
    /// The set's **id** — the name an actor's `message` refers to it by, and the name of the derived
    /// `std::variant` alias (`supervision` becomes `messages::Supervision`).
    pub id: String,
    /// The kinds that make up the set, in the order they are declared — the order of the variant's
    /// alternatives, which is also the order the scaffold writes the structs in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// One line a reviewer reads: what this set of messages is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The application, decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationSpec {
    /// The application in **English**: what it is for and what it does, in the person's own words.
    ///
    /// This is the half of a composition that is not a graph: a `composition.spire` a person hand-authors
    /// carries both, so the structure and the sentence that explains it cannot drift apart in two files.
    /// It is **prose, not machinery** — carried verbatim into the generated code (the header of `main/`
    /// and of each unit) so a person codes against their own sentence rather than a paraphrase, and
    /// deliberately **not read by [`validate`]**: a rule over free text would be a rule over nothing.
    /// What must *not* live here is computation — a guard, an arithmetic step, a page transition — which
    /// belongs in the unit's code in the language that has to run it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub framework: ApplicationFramework,
    /// One line, for the person confirming the choice.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub justification: String,
    pub board: BoardChoice,
    #[serde(default)]
    pub board_facts: Vec<BoardFact>,
    /// The **resources** this application declares: the devices it may reach. Every unit's `uses`
    /// reaches one of these, so a device nobody declared is a device this application does not have —
    /// which is the rule that makes "nothing invented" checkable rather than a hope. See [`Resource`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<Resource>,
    /// The **message-sets** this application declares: the actors that receive more than one kind of
    /// message, each kind a shared type. Empty for the ordinary case, where every actor's `message` names
    /// a single kind on its own. An actor's `message` **refers** to a set by its id, and the scaffold
    /// derives the composite `std::variant` from it. See [`MessageSet`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<MessageSet>,
    pub units: Vec<Unit>,
    /// `from -> to`, by unit id. Stated rather than derived from `sends_to`, because the edges *are*
    /// what a reviewer checks — and because a stage has no `sends_to` to derive them from.
    #[serde(default)]
    pub wiring: Vec<String>,
    /// The **power** facet, when the composition has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<PowerFacet>,
    /// The **storage** facet, when the composition has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageFacet>,
    /// The **networking** facet, when the composition has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkFacet>,
    /// The **security** facet, when the composition has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityFacet>,
}

impl ApplicationSpec {
    /// The unit with this id, if the spec has one.
    fn unit(&self, id: &str) -> Option<&Unit> {
        self.units.iter().find(|u| u.id == id)
    }

    /// A unit's id as a key — the identity a wiring edge and a board fact refer to.
    fn ids(&self) -> BTreeSet<&str> {
        self.units.iter().map(|u| u.id.as_str()).collect()
    }

    /// The resource with this name, if the spec declares one — what a `uses` is checked against.
    fn resource(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.name == name)
    }

    /// The message-set an actor's `message` names, if it names one.
    ///
    /// Matched on the **normalised** C++ name, because that is the name the scaffold writes and the name
    /// the membership test is really about: `supervision` and `Supervision` are one set. A `message` that
    /// matches no set is a bare kind, which is the ordinary single-kind form rather than an error.
    pub(crate) fn message_set(&self, message: &str) -> Option<&MessageSet> {
        let name = message_type_name(message);
        if name.is_empty() {
            return None;
        }
        self.messages
            .iter()
            .find(|set| message_type_name(&set.id) == name)
    }
}

/// Read the model's answer.
///
/// The model returns **JSON** — the same shape the other prompts in this codebase ask for, and for the
/// same reason: a step that can be parsed is a step a tool can check, and `serde` names the field that
/// is wrong when it is. What this deliberately does *not* do is validate: a spec that parses and does
/// not validate is the normal intermediate state, and [`validate`] is a separate answer so a reviewer
/// (or a repair round) sees all of a spec's problems at once rather than one parse error at a time.
pub fn parse_spec(text: &str) -> Result<ApplicationSpec, String> {
    // Models fence their JSON, and a fence is a formatting habit rather than a different answer — so
    // the fence is stripped here rather than being reported as a parse failure about a backtick.
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|rest| rest.trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    serde_json::from_str(body).map_err(|e| {
        format!(
            "the application spec did not parse: {e}. It is JSON of the form {{framework, board, \
             units, wiring}} — see the design request for the exact fields."
        )
    })
}

/// Read a `composition.spire` — the same [`ApplicationSpec`], authored as a **project file**.
///
/// The composition is a file a person opens and edits, so it is **YAML**: the one format here that
/// carries comments (the JSON
/// [`APPLICATION_FILE`](crate::build::idf_projects::APPLICATION_FILE) cannot), that a person can
/// hand-format, and that `serde` already reads elsewhere in this codebase. JSON is a **subset** of
/// YAML, so a fence-wrapped JSON answer parses here too — which keeps a model's output valid when it
/// returns the spec in the shape it always has.
///
/// The parser is strict and it is `serde`'s: a field of the wrong type, an unknown variant or a
/// misindented block is refused, and `serde_yaml` names the line and column. A composition that is
/// not syntactically correct is not a composition, and must not be scaffolded from — which is the
/// whole reason this is a file rather than a value in a wizard.
pub fn parse_composition(text: &str) -> Result<ApplicationSpec, String> {
    // The same fence habit the JSON reader forgives, for the same reason: a fence is formatting, not
    // a different answer.
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```yaml")
        .or_else(|| trimmed.strip_prefix("```json"))
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|rest| rest.trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    serde_yaml::from_str(body).map_err(|e| {
        format!(
            "the composition did not parse: {e}. It is YAML of the form {{framework, board, units, \
             wiring}} — see `composition.spire` in the project, and note that kinds, roles and \
             sources are lowercase (`actors`, `component`, `driver`, `stub`)."
        )
    })
}

/// The seven rules — every one of them a failure that would otherwise reach the compiler or a board.
///
/// Returns every problem rather than the first, because a spec is repaired in one pass: a model (or a
/// person) told all of them at once fixes them together, and one told a rule at a time spends six
/// rounds finding out where it went wrong.
pub fn validate(spec: &ApplicationSpec) -> Result<(), Vec<String>> {
    let mut problems: Vec<String> = Vec::new();
    let ids = spec.ids();

    if spec.units.is_empty() {
        problems.push(
            "the spec has no units — an application is a composition, and this composes nothing"
                .into(),
        );
    }

    // 1. One framework, every unit the kind that framework implies, and **one unit per id**.
    let expected = spec.framework.unit_kind();
    for unit in &spec.units {
        let ok = match unit.kind {
            // A component is the one kind both frameworks share; it is agnostic by construction.
            UnitKind::Component => true,
            kind => kind == expected,
        };
        if !ok {
            problems.push(format!(
                "'{}' is a {} unit in a {} application — one framework per application, and {}. \
                 Mixed applications are deliberately not supported yet.",
                unit.id,
                unit_kind_str(unit.kind),
                spec.framework.as_str(),
                if spec.framework == ApplicationFramework::Actors {
                    "an actors application has no dataflow stages"
                } else {
                    "a ramen application has no actors"
                }
            ));
        }
    }
    // Identity is what the wiring, the board facts, `uses` and `sends_to` are *by*, so two units with
    // one id make every one of those ambiguous — and `unit(id)` would answer with the first, so the
    // spec would read as though the second did not exist.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for unit in &spec.units {
        if !seen.insert(unit.id.as_str()) {
            problems.push(format!(
                "'{}' is declared twice — an id is what the wiring, the board facts, `uses` and \
                 `sends_to` refer to, so two units cannot share one",
                unit.id
            ));
        }
    }

    // 2. Every actor states its message; every stage states a port; and the message-sets hold
    //    together.
    for unit in &spec.units {
        match unit.kind {
            UnitKind::Actor => {
                if unit.message.as_deref().unwrap_or("").trim().is_empty() {
                    problems.push(format!(
                        "'{}' is an actor with no message — an actor *is* a message type, and the \
                         compile cannot infer one (`spire::Actor` is templated on it)",
                        unit.id
                    ));
                }
            }
            // One port or two: a *source* stage (a frame pump reading a camera) has only `pushes`, and
            // a *sink* (the stage acting on the flow's last value) has only `pulls`. A stage with
            // neither is not a stage, it is a unit nobody can place in the chain.
            UnitKind::Stage => {
                let has_pull = !unit.pulls.as_deref().unwrap_or("").trim().is_empty();
                let has_push = !unit.pushes.as_deref().unwrap_or("").trim().is_empty();
                if !has_pull && !has_push {
                    problems.push(format!(
                        "'{}' is a stage with no ports — a ramen stage is defined by what it pulls or \
                         what it pushes, and one with neither is nowhere in the chain",
                        unit.id
                    ));
                }
            }
            UnitKind::Component => {}
        }
    }

    // The message-sets a design declares, held together: one id, **more than one** kind, and nothing
    // that makes one C++ name mean two things. A set's id is the name an actor's `message` refers to it
    // by and the name of the derived `std::variant`; its kinds are the shared `struct`s the variant is
    // made of.
    let mut set_ids: BTreeSet<String> = BTreeSet::new();
    for set in &spec.messages {
        let id = message_type_name(&set.id);
        if id.is_empty() {
            problems.push(
                "a message-set has no `id` — the id is the name an actor's `message` refers to it by, \
                 and the name of the derived variant (`supervision` becomes `Supervision`)"
                    .into(),
            );
            continue;
        }
        // An id is what a `message` resolves by, so two sets sharing one would make every `message` that
        // names it ambiguous — and `message_set` answers with the first, so the second would read as
        // though it were not declared.
        if !set_ids.insert(id.clone()) {
            problems.push(format!(
                "the message-set '{}' is declared twice — an id is what an actor's `message` refers \
                 to, so two sets cannot share one",
                set.id
            ));
        }
        // A set is for the actor that receives *more than one* kind of message. One kind is not a set,
        // and writing it as one asks the scaffold for a `std::variant` of a single alternative that
        // `message: <kind>` already says without it.
        if set.kinds.len() < 2 {
            problems.push(format!(
                "the message-set '{}' names {} kind(s) — a set is the *several* kinds one actor \
                 receives (`kinds: [Reading, Command, Heartbeat]`), and a single kind is written \
                 `message: <kind>` directly",
                set.id,
                set.kinds.len()
            ));
        }
        let mut kinds: BTreeSet<String> = BTreeSet::new();
        for kind in &set.kinds {
            let name = message_type_name(kind);
            if name.is_empty() {
                problems.push(format!(
                    "the message-set '{}' names a kind '{}' that is not a name — a kind is the word \
                     for one thing the actor receives (`Reading`), and it becomes a `struct`",
                    set.id, kind
                ));
                continue;
            }
            if !kinds.insert(name) {
                problems.push(format!(
                    "the message-set '{}' names the kind '{}' twice — `kinds` is a list of distinct \
                     kinds, and two entries that mean one name are one kind written twice",
                    set.id, kind
                ));
            }
        }
    }
    // **One namespace.** A set becomes a `std::variant` under its id, and every other name here becomes a
    // `struct` — the kinds, each bare actor `message`, each stage's port value. So a set whose id
    // normalises to one of those would declare the same name twice, once a struct and once an alias, and
    // the actor that named the other would be built against a type of the wrong shape. A kind reused as a
    // bare message is *not* a collision: that is the sharing a set exists for.
    let struct_names: BTreeSet<String> = spec
        .messages
        .iter()
        .flat_map(|set| set.kinds.iter().map(|kind| message_type_name(kind)))
        .chain(spec.units.iter().flat_map(|unit| {
            let named: Vec<String> = match unit.kind {
                UnitKind::Actor => vec![message_type_name(
                    unit.message.as_deref().unwrap_or_default(),
                )],
                UnitKind::Stage => vec![
                    message_type_name(unit.pulls.as_deref().unwrap_or_default()),
                    message_type_name(unit.pushes.as_deref().unwrap_or_default()),
                ],
                UnitKind::Component => Vec::new(),
            };
            named
                .into_iter()
                .filter(|name| !name.is_empty() && !set_ids.contains(name))
                .collect::<Vec<String>>()
        }))
        .collect();
    for set in &spec.messages {
        let id = message_type_name(&set.id);
        if !id.is_empty() && struct_names.contains(&id) {
            problems.push(format!(
                "the message-set '{}' is named for a type the design already declares — a set becomes \
                 `messages::{}`, a `std::variant`, and a kind, a bare message or a port value of that \
                 name is a `struct`; one name cannot be both. Rename the set",
                set.id, id
            ));
        }
    }

    // 3. Every edge resolves; wiring connects *composition*, and `uses` reaches a component — or a
    // resource this spec declares, which is how a unit names a part of the board.
    for edge in &spec.wiring {
        match parse_edge(edge) {
            Some((from, to)) => {
                for end in [from, to] {
                    match spec.unit(end) {
                        None => problems.push(format!(
                            "the wiring names '{end}', which is not a unit — every edge has to \
                             resolve, or the composition has a hole in it"
                        )),
                        // The rule that keeps a component framework-agnostic: a driver is *used* by a
                        // unit, never wired into the dataflow or sent to. Wire it and it has ports,
                        // and ports are the framework's.
                        Some(unit) if unit.kind == UnitKind::Component => problems.push(format!(
                            "the wiring connects '{end}', which is a component — a component has no \
                             ports and no mailbox. Wire the unit that *uses* it, and let that unit \
                             list '{}' in its `uses`",
                            end
                        )),
                        Some(_) => {}
                    }
                }
            }
            None => problems.push(format!(
                "'{edge}' is not an edge: write them as `from -> to`, by unit id"
            )),
        }
    }
    for unit in &spec.units {
        for target in &unit.sends_to {
            if !ids.contains(target.as_str()) {
                problems.push(format!(
                    "'{}' sends to '{target}', which is not a unit",
                    unit.id
                ));
            }
        }
        for used in &unit.uses {
            match spec.unit(used) {
                Some(component) if component.kind == UnitKind::Component => {}
                Some(_) => problems.push(format!(
                    "'{}' uses '{used}', which is not a component — a unit *uses* the \
                     framework-agnostic parts and talks to the others through the wiring",
                    unit.id
                )),
                // A `uses` that names no unit may still name a **declared resource**: a part of the
                // board (`display`, `touch`, `rtc`) has no component of this project, and reaching it
                // is a `uses` like any other. Naming a device nobody declared is the refusal — an
                // application composes against the hardware it has, not the hardware it wishes for.
                None if spec.resource(used).is_some() => {}
                None => problems.push(format!(
                    "'{}' uses '{used}', which is neither a component in this spec nor a resource it \
                     declares — declare the device in `resources` first (its `name` is what a `uses` \
                     speaks), because a unit cannot reach hardware this application does not have",
                    unit.id
                )),
            }
        }
    }
    if spec.framework == ApplicationFramework::Ramen {
        if let Some(cycle) = find_cycle(spec) {
            problems.push(format!(
                "the dataflow has a cycle ({}). ramen pushes synchronously, so a cycle is unbounded \
                 recursion on one stack rather than a loop — which is what the actor framework is for",
                cycle.join(" -> ")
            ));
        }
    }

    // 4. A component is framework-agnostic, and its role decides what it may carry.
    for unit in &spec.units {
        if unit.kind != UnitKind::Component {
            continue;
        }
        if unit.role.is_none() {
            problems.push(format!(
                "'{}' is a component with no role: `driver` (one device on a bus) or `library` (pure \
                 code), which is what decides its skeleton and its test",
                unit.id
            ));
        }
        match unit.role {
            Some(ComponentRole::Driver) => {
                // A **published** driver is resolved from the registry, not written here: its bus is
                // the registry component's own business, so the design is not asked for one.
                if unit.source != Some(UnitSource::Published)
                    && unit.bus.as_deref().unwrap_or("").trim().is_empty()
                {
                    problems.push(format!(
                        "'{}' is a driver with no bus — a protocol written for the wrong bus never \
                         compiles, so the bus is a device fact the spec has to state",
                        unit.id
                    ));
                }
            }
            Some(ComponentRole::Library) if unit.bus.is_some() => {
                problems.push(format!(
                    "'{}' is a library with a bus — a component that names no device has none, \
                     and a bus here would be a contract that lies",
                    unit.id
                ));
            }
            _ => {}
        }
        // The **source** and the registry name agree with each other. A published component *is* a
        // managed dependency, so it has to say which one — and the two local sources must not carry a
        // registry name, because that would promise a dependency that is not one.
        match unit.source {
            Some(UnitSource::Published)
                if unit.registry.as_deref().unwrap_or("").trim().is_empty() =>
            {
                problems.push(format!(
                    "'{}' is published but names no registry component — a published unit is a \
                     dependency the application declares, so it has to carry the `namespace/name` \
                     the registry knows it by (for example `espressif/esp_lcd_st7701`)",
                    unit.id
                ));
            }
            Some(UnitSource::Existing) | Some(UnitSource::Stub) if unit.registry.is_some() => {
                problems.push(format!(
                    "'{}' carries a registry name but its source is not `published` — a registry \
                     name means the component comes from the registry, so either the source is \
                     `published` or the registry name is a mistake",
                    unit.id
                ));
            }
            _ => {}
        }
        // The whole point of the framework axis: a component that carries composition has stopped
        // being reusable in an application that chose the other framework.
        if unit.message.is_some()
            || unit.state.is_some()
            || unit.pulls.is_some()
            || unit.pushes.is_some()
            || !unit.sends_to.is_empty()
            || !unit.uses.is_empty()
        {
            problems.push(format!(
                "'{}' is a component carrying composition (message/state/ports/sends_to/uses). A \
                 component holds a device or an algorithm; *how it is driven* is the application's — \
                 the rule that keeps it usable under either framework",
                unit.id
            ));
        }
    }

    // 5. A board fact reaches a driver in this spec — not a device nobody drives.
    for fact in &spec.board_facts {
        match spec.unit(&fact.device) {
            Some(unit)
                if unit.kind == UnitKind::Component
                    && unit.role == Some(ComponentRole::Driver) => {}
            Some(_) => problems.push(format!(
                "the board fact for '{}' names a unit that is not a driver component — a board fact \
                 is how a driver is *reached*, so it belongs to one",
                fact.device
            )),
            None => problems.push(format!(
                "the board fact names '{}', which is not a unit in this spec — onboard devices (a \
                 display, a touch panel, a battery gauge) come from the board, not from board facts",
                fact.device
            )),
        }
    }

    // 6. Nothing invented and unused.
    let referenced: BTreeSet<&str> = spec
        .wiring
        .iter()
        .filter_map(|e| parse_edge(e))
        .flat_map(|(from, to)| [from, to])
        .chain(
            spec.units
                .iter()
                .flat_map(|u| u.sends_to.iter().map(String::as_str)),
        )
        .chain(
            spec.units
                .iter()
                .flat_map(|u| u.uses.iter().map(String::as_str)),
        )
        .chain(spec.board_facts.iter().map(|f| f.device.as_str()))
        .collect();
    for unit in &spec.units {
        if !referenced.contains(unit.id.as_str()) {
            problems.push(format!(
                "'{}' is referenced by no wiring edge, no `sends_to`, no `uses` and no board fact — \
                 an invented unit nobody uses is either a mistake or work that belongs to another spec",
                unit.id
            ));
        }
    }
    // A message-set is referenced the way a unit is: an actor's `message` names it. One no actor names
    // says what an inbox takes and is never received — invented, like a unit nobody wires.
    for set in &spec.messages {
        let id = message_type_name(&set.id);
        let received = spec.units.iter().any(|unit| {
            unit.kind == UnitKind::Actor
                && message_type_name(unit.message.as_deref().unwrap_or_default()) == id
        });
        if !received {
            problems.push(format!(
                "the message-set '{}' is named by no actor's `message` — a set is what an actor \
                 receives, so one nobody receives is either a mistake or belongs to another spec",
                set.id
            ));
        }
    }

    // 7. The declared resources hold together: one name for one part, a capability the vocabulary
    // knows, a source that agrees with the composition, and nothing invented.
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for resource in &spec.resources {
        if !is_identifier(resource.name.trim()) {
            problems.push(format!(
                "'{}' is not a resource name — it is the word a unit's `uses` speaks, so it is a bare \
                 lower_snake identifier (`sps30`, `rtc`)",
                resource.name
            ));
        }
        // A name is what a `uses` resolves *by*, so two resources sharing one would make every `uses`
        // of it ambiguous — and `resource(name)` would answer with the first, so the second would read
        // as though it were not declared.
        if !declared.insert(resource.name.as_str()) {
            problems.push(format!(
                "'{}' is declared twice — a resource name is what a `uses` refers to, so two resources \
                 cannot share one",
                resource.name
            ));
        }
        // The roles are the declaration's whole point: an application says what it needs *by
        // capability*, and each role names one. A resource with none names nothing; a name the
        // vocabulary does not know is a **misspelling** here exactly as it is on a board — the
        // difference between a new thing and a typo is that someone named the former.
        if resource.roles.is_empty() {
            problems.push(format!(
                "the resource '{}' names no capability — `\"roles\"` is what the device *is* \
                 (`sensing.temperature`, `timekeeping.rtc`, and both for a part that is more than one), \
                 and a device nobody can reach by requirement is one no unit can use",
                resource.name
            ));
        }
        for role in &resource.roles {
            if !crate::capability_vocabulary::is_known_path(role.trim()) {
                problems.push(format!(
                    "the resource '{}' claims the capability '{}', which is not one the vocabulary \
                     knows — a role is a capability path (`sensing.particulate`, `timekeeping.rtc`), \
                     and one nobody named is a typo rather than a new thing",
                    resource.name, role
                ));
            }
        }
        // The driver, if the application drives this part itself: the component whose `id` *is* the
        // resource name, and whose role has to be `driver` because a device is reached through a bus.
        let driver = spec.units.iter().any(|unit| {
            unit.id == resource.name
                && unit.kind == UnitKind::Component
                && unit.role == Some(ComponentRole::Driver)
        });
        // Any unit at all sharing the name — which a `board` resource may not have: the word then means
        // two things, and because a `uses` resolves a unit before a resource, the *unit* is what it
        // would reach, silently shadowing the part.
        let shadowing = spec.units.iter().any(|unit| unit.id == resource.name);
        match resource.via {
            None => problems.push(format!(
                "the resource '{}' does not say how it is reached — `\"via\"` is `board` (a part of the \
                 board), `library` (a driver this project has or writes) or `published` (a managed \
                 dependency), and the choice decides whether a component is designed at all",
                resource.name
            )),
            // The board reaches it, so the word belongs to the part: a unit of the same name would be
            // reached instead of it, and a part the board already drives needs no component of ours.
            Some(ResourceVia::Board) if shadowing => problems.push(format!(
                "the resource '{}' is `via: board` and the spec also has a unit of that name — the word \
                 means the board's part, and since a `uses` resolves a unit before a resource, '{}' \
                 would reach the unit and never the part. Name the unit after what it does — or declare \
                 this resource `library`/`published` if that unit really is its driver",
                resource.name, resource.name
            )),
            // A part this application drives needs the driver that drives it: the component is where
            // the protocol lives, and its id is the name the resource is declared by.
            Some(ResourceVia::Library | ResourceVia::Published) if !driver => problems.push(format!(
                "the resource '{}' is `via: {}` but no driver component of that name is in the spec — \
                 a `library`/`published` resource *is* its driver's id, so that component has to be \
                 designed too",
                resource.name,
                if resource.via == Some(ResourceVia::Published) { "published" } else { "library" }
            )),
            _ => {}
        }
        // The one place the older `board_facts` and these resources overlap: the address of one
        // device. Stating it twice is fine; disagreeing is not, because one of the two would be a lie.
        if let Some(fact) = spec.board_facts.iter().find(|f| f.device == resource.name) {
            if !fact.address.trim().is_empty()
                && !resource.address.trim().is_empty()
                && fact.address != resource.address
            {
                problems.push(format!(
                    "the resource '{}' is at {} and its board fact says {} — one device has one \
                     address",
                    resource.name, resource.address, fact.address
                ));
            }
        }
    }

    // Every declared resource has exactly **one owner** — the unit whose `uses` names it, which is the
    // actor that holds it in its state. A resource nobody owns was declared for nothing; a resource two
    // units own is *shared*, which the actor model forbids: a device has one driver and one place its
    // state lives, so every other unit reaches it by messaging that owner, never by using it too. A
    // `library`/`published` resource shares its name with its driver component, so this one check covers
    // a board part (used directly) and an added part (used through its driver) alike.
    for resource in &spec.resources {
        let owners: Vec<&str> = spec
            .units
            .iter()
            .filter(|unit| unit.uses.iter().any(|used| used == &resource.name))
            .map(|unit| unit.id.as_str())
            .collect();
        match owners.len() {
            0 => problems.push(format!(
                "the resource '{}' is declared but no unit uses it — a resource is owned by one actor, \
                 which holds it in its state, so declare only the devices a unit will reach",
                resource.name
            )),
            1 => {}
            _ => problems.push(format!(
                "the resource '{}' is used by {} units ({}) — a resource has one owner, the actor that \
                 holds it in its state; every other unit reaches it by messaging that owner, not by \
                 using it itself",
                resource.name,
                owners.len(),
                owners.join(", ")
            )),
        }
    }

    finish(problems)
}

/// A unit kind as it is written in a spec, for a message a person reads.
fn unit_kind_str(kind: UnitKind) -> &'static str {
    match kind {
        UnitKind::Component => "component",
        UnitKind::Actor => "actor",
        UnitKind::Stage => "stage",
    }
}

/// `skip_serializing_if` for a flag that is off: a `Unit` with no facet annotation writes no key, so
/// the JSON record stays exactly what it was before facets existed.
fn is_false(value: &bool) -> bool {
    !*value
}

fn finish(problems: Vec<String>) -> Result<(), Vec<String>> {
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// `from -> to`, both non-empty.
fn parse_edge(edge: &str) -> Option<(&str, &str)> {
    let (from, to) = edge.split_once("->")?;
    let (from, to) = (from.trim(), to.trim());
    if from.is_empty() || to.is_empty() {
        None
    } else {
        Some((from, to))
    }
}

/// A cycle in the wiring, as the path that closes it — `None` when the graph is a DAG.
///
/// Depth-first with an on-stack set, which is all a graph this size needs: the point is to catch the
/// mistake, and the *path* is what makes the refusal readable.
///
/// `pub(crate)` because the two frameworks read a cycle differently: `validate` **refuses** one for
/// `ramen`, and the fill prompt **names** one for `actors` — where a cycle is legal and is exactly what
/// the name registry resolves (`idf_projects::composition_block`).
pub(crate) fn find_cycle(spec: &ApplicationSpec) -> Option<Vec<String>> {
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &spec.wiring {
        if let Some((from, to)) = parse_edge(edge) {
            edges.entry(from).or_default().push(to);
        }
    }
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for unit in &spec.units {
        let mut path: Vec<&str> = Vec::new();
        let mut on_stack: BTreeSet<&str> = BTreeSet::new();
        if let Some(cycle) = walk(&unit.id, &edges, &mut done, &mut path, &mut on_stack) {
            return Some(cycle.into_iter().map(str::to_string).collect());
        }
    }
    None
}

fn walk<'a>(
    node: &'a str,
    edges: &BTreeMap<&'a str, Vec<&'a str>>,
    done: &mut BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
    on_stack: &mut BTreeSet<&'a str>,
) -> Option<Vec<&'a str>> {
    if on_stack.contains(node) {
        let start = path.iter().position(|n| *n == node).unwrap_or(0);
        let mut cycle = path[start..].to_vec();
        cycle.push(node);
        return Some(cycle);
    }
    if done.contains(node) {
        return None;
    }
    on_stack.insert(node);
    path.push(node);
    for next in edges.get(node).into_iter().flatten() {
        if let Some(cycle) = walk(next, edges, done, path, on_stack) {
            return Some(cycle);
        }
    }
    path.pop();
    on_stack.remove(node);
    done.insert(node);
    None
}

/// One component a library already has, as the design sees it: its name, and what its own manifest
/// says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryComponent {
    pub name: String,
    /// `None` for a component that states no kind — one written by hand, or by an older version of the
    /// tool. Stated as unknown rather than assumed, because the kind is exactly what a design has to
    /// know to decide whether it is a device driver or pure code.
    pub kind: Option<crate::build::idf_projects::ComponentKind>,
}

/// A **component library**, as far as a design needs to know it.
///
/// The design states `"source": "existing"` or `"source": "stub"` about every component it names, and
/// that is a claim about this library — so a design asked for without it is a design inventing which
/// parts already exist. Two of its facts matter:
///
/// * the components it has, which is what makes `existing` true or false;
/// * what its author wrote down about how it is meant to be used (`SPIRE.md`), which is the
///   architecture the design has to respect rather than invent around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryFacts {
    /// Where it is, for the person reading the request. Not a fact the design acts on.
    pub root: String,
    pub components: Vec<LibraryComponent>,
    pub hints: Option<String>,
}

/// The framework rule, as the **brief request** states it.
///
/// `Some(chosen)` is the pinned case (the caller already decided); `None` is the derived one — which is
/// what the brief asks for, because the person confirms the choice at the board step. A composition
/// states its framework outright, so this rule is only ever spoken to the brief.
fn framework_rule(framework: Option<ApplicationFramework>) -> String {
    match framework {
        Some(chosen) => format!(
            "The framework is **{}** — the user chose it, so it is not yours to reconsider. Design \
             the composition for it.",
            chosen.as_str()
        ),
        None => "**Choose the framework** and justify it in one line. The rule:\n\
                 \n\
                 * **human timescale, interactive, command/event-driven -> `actors`** — a touch screen, a\n\
                 menu, a reading that updates once a second, a device reacting to commands and timers\n\
                 while holding state;\n\
                 * **machine timescale, high-rate data flowing through stages -> `ramen`** — video\n\
                 frames, kHz samples, an inference pipeline, where a value is *moved and transformed*\n\
                 rather than *waited on*.\n\
                 \n\
                 A *stream* is a component's job either way (a status channel and a sensor bus are both\n\
                 drivers): what decides the framework is whether the application **reacts** to the data\n\
                 or **pumps** it through stages."
            .to_string(),
    }
}

/// How many times a leg will ask the model before giving up.
///
/// Three, like the AppSpec requirements pass: enough for a model to fix a real mistake it made (a
/// forgotten field, a wiring edge that names nothing), and few enough that a model which *cannot*
/// produce an answer is reported rather than asked forever. Shared by the brief leg and the AppSpec
/// requirements pass so the two cannot drift.
pub const MAX_DESIGN_ATTEMPTS: usize = 3;

/// The repair turn: the same request, the answer that came back, and what is wrong with it.
///
/// Deliberately a whole new message rather than a conversation: the brief keeps no chat state,
/// so a prompt that carries the answer it is asking to fix is self-contained — which is what makes it
/// reproducible, testable, and independent of whatever the LLM service remembers.
fn repair_request(request: &str, previous: &str, problems: &[String]) -> String {
    let problems = problems
        .iter()
        .map(|p| format!("- {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{request}\n\n---\n\nYour previous answer was rejected by the checks. Fix it and return the \
         corrected JSON object, and nothing else.\n\n## What you answered\n\n{previous}\n\n## What is \
         wrong with it\n\n{problems}\n"
    )
}

/// The **six points** every description is read against — the fixed shape of a brief's `points`.
///
/// One entry per point: the `id` a `points` entry carries, and the question the request asks about it.
/// Named here, rendered into [`brief_request`] and checked by [`validate_brief`], so the six questions
/// the model is asked, the six entries a brief carries and the six rows a person sees are the same six —
/// the prompt, the check and the screen cannot drift apart.
///
/// It replaces the old `BRIEF_GAP_CEILING`, and the difference is the whole change. That constant capped
/// a **count** — how many questions a brief may ask — and a cap leaves the count the model's judgement
/// about how much a description left open, which live runs measured at 0, 1, 3, 6 and 10 questions for
/// one unchanged text. Six points remove the judgement instead of bounding it: every description is read
/// against all six, so what is *open* is no longer a number the model picks — it is which of the six
/// `decided` cells are filled.
pub const BRIEF_POINTS: [BriefPointSpec; 6] = [
    BriefPointSpec {
        id: "purpose",
        question: "what it does, in a paragraph",
    },
    BriefPointSpec {
        id: "senses",
        question: "what it **senses** (sensors, buttons, network, time — and roughly how fast)",
    },
    BriefPointSpec {
        id: "acts_on",
        question: "what it **acts on** (display, audio, motor, actuator, network)",
    },
    BriefPointSpec {
        id: "reacts_to",
        question: "what it **reacts to over time** (periodic? on-command? on-threshold? on-event?)",
    },
    BriefPointSpec {
        id: "timing",
        question:
            "**timing** — what is parallel, what is a human-rate UI, what is a machine-rate stream",
    },
    BriefPointSpec {
        id: "missing",
        question:
            "what happens **when something is missing** (a sensor absent, WiFi down, battery low)",
    },
];

/// One of [`BRIEF_POINTS`]: the `id` a `points` entry carries, and the question the request asks about
/// it. A struct rather than two parallel arrays, so an id and its question are read — and edited —
/// together.
pub struct BriefPointSpec {
    pub id: &'static str,
    pub question: &'static str,
}

/// Whether `id` is one of the six points — the check that makes an invented id (`purpose2`) a
/// misspelling to correct rather than an eighth point.
pub fn is_known_point(id: &str) -> bool {
    BRIEF_POINTS.iter().any(|point| point.id == id)
}

/// The **ceiling on `forks`**: the genuine either/or decisions a brief may hand back unanswered.
///
/// The successor to `BRIEF_GAP_CEILING`, and small on purpose. A fork is the one place a question
/// survives in a brief, and it survives only where the answer would change which capability or unit the
/// application is given **and** the description cannot settle it — so a brief handing back five has not
/// read the text, it has interviewed the person. Two is enough for the forks that are real (log to an SD
/// card or stream it over WiFi) and tight enough that the model must *decide* the rest and show the
/// decision in `decided`, which is a reading a person can correct by picking rather than by rewriting.
pub const BRIEF_FORK_CEILING: usize = 2;

/// The **design brief** — what a description says the application has to do, before a board is chosen
/// or any of it is decomposed.
///
/// This is the first step of the description-first flow, and it stops deliberately short of the
/// [application spec](ApplicationSpec). It reads a description into five things and no more:
///
/// * a `summary` — the application in a paragraph, for the person to confirm;
/// * a `framework` — derived from the description, and
///   confirmed by the person when they choose a board;
/// * `requires` — the **capabilities** the application needs, as paths the vocabulary knows
///   (`sensing.temperature`, `media.display`). Board-agnostic on purpose: this is what the application
///   has to do, never what a particular board happens to carry, which is the board step's question;
/// * `suggests` — the units the description implies, as bare names for the actor step to start from,
///   naming what it senses **and** what it acts on (a display and a touch screen are units);
/// * `points` — the description read against **each of the six questions** ([`BRIEF_POINTS`]): what the
///   description **said** about the point, and what was **decided** where it said nothing;
///
/// It also carries `forks`: the rare either/or decision the description cannot settle *and* the brief
/// will not settle alone, because the answer changes which capabilities or units the application is
/// given. A fork is a pick between named alternatives ([`BriefFork`]) that carry what they add, never an
/// open question — **the model drafts and the person edits, so the brief never asks the person to
/// enumerate what is missing.**
///
/// What the brief never does is name a board, a resource, a driver or a wiring edge. Those are steps
/// two and three's to decide, and a brief that guessed them would be a decomposition nobody asked for
/// — the exact collision the brief exists to avoid.
///
/// The brief is a **guide, not a gate**. Its `requires` produces badges, a board-fit readout and soft
/// warnings; a capability nothing in the composition covers is reported, not refused, because the
/// person is mid-design and the rule the core enforces is the *spec's*, not the brief's. The one thing
/// checked here is that each `requires` path is a capability the vocabulary knows — an unknown path is
/// a misspelling rather than a new capability, exactly as a device's `roles` and a resource's `roles`
/// are checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignBrief {
    /// The application in a paragraph, in the model's words, for the person to confirm.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
    /// The framework the description points at. Derived rather than pinned — the person confirms it at
    /// the board step, which is where it stops being a suggestion. `None` when the description is too
    /// thin to tell, which is an answer rather than a gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<ApplicationFramework>,
    /// The **capabilities** the application needs, as paths the vocabulary knows. Board-agnostic:
    /// `sensing.particulate` is what the application must do, not a claim that any board carries it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// The **units** the description implies, as advisory lower_snake names (`sampler`,
    /// `air_quality`). **One per thing the application senses and one per thing it acts on** — a display
    /// (`view`) and a touch screen (`touch`) are units, not one — because a suggestion that named only
    /// the sensing side would leave half the application undesigned. Names only — no kind, no message,
    /// no wiring — because the composition is not designed yet and a suggestion is not a decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggests: Vec<String>,
    /// The description **read against each of the six points** ([`BRIEF_POINTS`]), one entry per point,
    /// each naming the point it answers. This is what replaced `gaps`.
    ///
    /// A `gaps` entry was a question the person had to answer by rewriting their description — coding in
    /// English — which is both the slowest way to answer a question and the only one the old brief
    /// offered. A reading is the opposite shape: the model answers the point, names the sentence it
    /// answered from in `said`, and where it had to supply the answer itself it says what it supplied in
    /// `decided`. Nothing here is a count, so nothing here can be counted differently twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<BriefPointRead>,
    /// The decisions the description cannot settle and the brief will not settle alone — **at most
    /// [`BRIEF_FORK_CEILING`]**, each a pick between named alternatives that carry the `requires` and
    /// `suggests` they would add.
    ///
    /// Empty is the ordinary answer. A fork exists only where a guess would change what the application
    /// *needs* (log to an SD card or stream it over WiFi), where the person's answer is one click and the
    /// model's guess would be a capability they did not ask for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forks: Vec<BriefFork>,
}

/// One point of a brief's reading: **what the description said** about it, and **what was decided** where
/// it said nothing.
///
/// This is a brief's `points` entry and the replacement for a `gaps` line. It is deliberately two cells
/// rather than one: `said` is the model's *reading* of the person's words — the part a person can check
/// against their own text — and `decided` is the model's *draft*, the answer it supplied so that the
/// design could carry on. A brief full of `said` and empty `decided` is a description that said
/// everything; a brief of empty `said` and full `decided` is a thin description the model carried by
/// guessing, and both read the same way on screen because both are prose attached to the point they
/// answer rather than a number attached to the whole.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefPointRead {
    /// Which of [`BRIEF_POINTS`] this is — one of the six ids, spelled exactly. Checked against the
    /// constant, so an invented id is a misspelling to correct rather than an eighth point.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub point: String,
    /// What the description says about the point, in the model's words. Empty when it says nothing —
    /// which is visible on screen as a blank reading and is the honest answer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub said: String,
    /// What was **decided** for the point where the description says nothing: the answer the model
    /// supplied, including the proposals a brief was already allowed to make (a screen's look, a
    /// message's shape). Empty exactly when the description answered the point.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub decided: String,
}

/// A decision the brief could not settle from the description **and** could not settle alone: the answer
/// would change which capabilities or units the application is given.
///
/// This is the only question left in a brief, and it is a *pick* rather than a prompt. Each choice
/// carries the `requires` and `suggests` it would add, so answering a fork is choosing the capabilities —
/// the person edits the structure, and no wording reaches the model that could read three gaps where
/// they meant one.
///
/// Capped by [`BRIEF_FORK_CEILING`] and checked by [`validate_brief`]: a fork needs a known point, a
/// question, at least two choices, and choices that actually differ in what they bring.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefFork {
    /// Which of [`BRIEF_POINTS`] the decision belongs to — one of the six ids.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub point: String,
    /// The decision as the **choice** it is ("log to a card, or stream it over WiFi?"), because a fork
    /// the person cannot answer by picking is not a fork but a question in disguise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub question: String,
    /// The named alternatives — at least two, and at least two of them differing in `requires` or
    /// `suggests`, because a choice whose answers bring the same thing decides nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<BriefChoice>,
}

/// One answer to a [`BriefFork`]: a short name for the alternative and **what it would add** — the
/// capability paths and unit names true of this answer and not of the others.
///
/// The paths are what make a fork answerable in one click instead of in prose, so they are checked
/// against the vocabulary exactly as a brief's own `requires` are: a path nobody knows is a misspelling
/// here too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefChoice {
    /// The alternative, in a word or three, for a button.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// The **capabilities** this answer needs, as paths the vocabulary knows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// The **units** this answer implies, as advisory lower_snake names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggests: Vec<String>,
}

/// The **brief request**: what the model is asked to read out of a description, before a board exists.
///
/// No board and no library are named, and that is the point: the brief is about what the application
/// *has to do*, and a brief written against a board would smuggle the board step's decision onto the
/// first screen. The six questions are in the prompt for the same reason they are in the design
/// request — a thin description should be reported as thin, question by question, rather than filled
/// with an invented device, address or protocol.
///
/// It also **reads the description against the six points** ([`BRIEF_POINTS`]) and tells the model what
/// to do with each: answer it, showing both the sentence it answered from and the decision it supplied
/// where there is none. That is the change from the brief that asked — a *reporting* brief handed the
/// person a variable number of questions, and live runs measured one unchanged description at 0, 1, 3, 6
/// and 10 of them, because "how much did this leave open" is a judgement and not a fact a request can pin
/// down. Six fixed points take the judgement out: the model reads all six every time, and what is open
/// falls out of which `decided` cells it filled rather than out of how many questions it thought of.
///
/// `forks` is the one place a question survives, and it is stated as a **ceiling the model can count** —
/// [`BRIEF_FORK_CEILING`], and only for a decision that would change what the application is given — so
/// the prompt, [`validate_brief`]'s check and the person's screen all read from the same constant.
pub fn brief_request(description: &str) -> String {
    format!(
        r#"You are reading the **description** of an ESP-IDF application, before a board is chosen
and before any decomposition is designed. The output is a JSON **design brief** that a person reviews
and the board and actor steps are built from.

## What the application has to do

{description}

**Read it against these six points and answer every one — a brief is a reading, not an interview:**

{points}

Each of the six gets one entry in `points`, and each entry has two cells:

* `said` — **what the description says about the point**, in your words. `""` when it says nothing about
  it;
* `decided` — **what you decided for the point** where the description left it open. Decide it yourself:
  the rest of the design is built on this reading, so a decision you can defend is worth more to the
  person than a question they have to answer. Your own proposals belong here too — the look of a screen,
  the shape of a message, the numbers a display shows — so the person reviews a concrete decision rather
  than being asked for one. `decided` is `""` exactly when the description answered the point.

**Never hand the person a question to answer in their own words.** A question here means they edit their
description, you read it again, and the answer may differ the next time — the slowest loop there is, and
the one that made this brief ask 0, then 1, then 6 things about the same text. You draft; they edit your
draft. So **decide the point**, and let the person see the decision and correct it if it is wrong.

`forks` is the one exception, and it is a **pick, not a question**: use it only where a decision would
change which capabilities or units the application is given (log to an SD card, or stream it over WiFi)
and the description does not settle it. Each fork has a `question` and at least two named `choices`, and
each choice carries the `requires` and `suggests` it would add — so answering it is choosing the
capabilities in one click. **At most {fork_ceiling}.** If you cannot name the alternatives it is not a
fork: decide it and put the decision in `decided`. Two questions about one point are one fork, and a
point the description already answers is never a fork at all.

A **fork is a requirement decision, never a hardware one.** A hardware fact is never something to ask
for: never ask the person for an I²C address, a pin number, a bus or a protocol — not in `forks`, and
not in `decided` either. A device is **named rather than addressed**: how it is wired and at what
address is resolved from the board and the device-facts store at the board and resource steps, so an
address is a **hardware fact** this step cannot act on. Where the description leaves a hardware fact
open, decide it the way you decide everything else — pick the obvious answer and say so in `decided`.

A **fork is a decision about behaviour, never about appearance.** The one thing you decide *for* the
person about a display is its **function** — how many screens, what each must **show** (which readings or
values) — and what the user can **do** and what it causes; decide those and put them in `decided` or
`summary`. How the UI *looks* is **yours to propose**, never a question for the person: an icon, a
colour, a font, a spacing, a layout, an animation — "how it looks" — belongs in your own words, so the
person reviews a concrete look instead of answering for one. Appearance is **never a fork**, and it is not
a `decided` the person has to supply either — propose it.

## The framework

{framework_rule}

## The capability vocabulary

A `requires` path must be one of these, spelled exactly as written — never a paraphrase, and never a
name that is not on the list (an SD card is `storage.sd`, a real-time clock is `timekeeping.rtc`):

{vocabulary}

## The brief

```json
{{
  "summary": "what this application does, in one paragraph",
  "framework": "actors" | "ramen",
  "requires": ["sensing.temperature", "media.display"],
  "suggests": ["sampler", "air_quality", "view", "touch"],
  "points": [
    {{ "point": "purpose",   "said": "what the description says about it", "decided": "what you decided where it says nothing, or empty" }},
    {{ "point": "senses",    "said": "…", "decided": "…" }},
    {{ "point": "acts_on",   "said": "…", "decided": "…" }},
    {{ "point": "reacts_to", "said": "…", "decided": "…" }},
    {{ "point": "timing",    "said": "…", "decided": "…" }},
    {{ "point": "missing",   "said": "…", "decided": "…" }}
  ],
  "forks": [
    {{ "point": "acts_on",
       "question": "the decision you cannot settle, phrased as the choice it is",
       "choices": [
         {{ "label": "a short name for one answer", "requires": ["capability.path"], "suggests": ["unit"] }},
         {{ "label": "the other answer", "requires": ["other.path"], "suggests": ["other_unit"] }}
       ] }}
  ]
}}
```

* `summary` — the application in a paragraph, in your words, for the person to confirm;
* `framework` — the choice from the rule above, derived from the description; the person confirms or
  overrides it when they choose a board;
* `requires` — the **capabilities** the application needs, as paths chosen **exactly** from the
  vocabulary below. Board-agnostic: what the application has to do, never what any particular board
  carries — which board satisfies it is the next step's question;
* `suggests` — the **units** the description implies, as plain lower_snake names (`sampler`,
  `air_quality`), for the actor step to start from. Suggest **one name for each thing the application
  senses and each thing it acts on**: a display is a unit (`view`), a touch screen is a unit (`touch`),
  a speaker is a unit (`audio`) — naming only what it senses leaves half the application undesigned.
  Names only: no kind, no message and no wiring, because the composition is not designed yet;
* `points` — **one entry for each of the six points above, and all six of them**, each
  `{{ "point": "<id>", "said": "…", "decided": "…" }}`. The ids are `purpose`, `senses`, `acts_on`,
  `reacts_to`, `timing` and `missing`, each **exactly once**. `said` is what the description says about
  the point — empty when it says nothing, which is the honest answer and not a failure. `decided` is
  what you decided where it says nothing, empty when the description answered it. A point whose `said`
  is empty is a point the description left open, and the decision you put in `decided` is what carries
  the design forward;
* `forks` — **at most {fork_ceiling}**, and only for a decision that would change which capabilities or
  units the application is given. Each is `{{ "point": "<id>", "question": "…", "choices": [ … ] }}` with
  **at least two choices**, each `{{ "label": "…", "requires": […], "suggests": […] }}`, and at least two
  of the choices must differ in what they bring — a choice whose answers add the same thing decides
  nothing. Every `requires` path inside a choice is checked against the vocabulary exactly as the brief's
  own are, so a path a choice names must be on the list too.

Nothing in a brief is a question the person answers in prose: a **fork is a decision about behaviour**
(points 1–6), and where behaviour is unclear you **decide** it. A **hardware fact** is never a fork,
because **a device is named rather than addressed** — an address, a pin, a bus or a protocol is resolved
from the board and the device-facts store later, not asked here. And an **appearance** fact — an icon, a
colour, a font, a layout — is never a fork either: the look is yours to propose, not the person's to
specify.

Do **not** name a board, a resource, a driver component or a wiring edge: none of those is decided here.

Respond with **only the JSON object**, and nothing else."#,
        fork_ceiling = BRIEF_FORK_CEILING,
        description = description.trim(),
        points = brief_points_for_prompt(),
        framework_rule = framework_rule(None),
        vocabulary = vocabulary_for_prompt(),
    )
}

/// The six points rendered for the brief prompt: the numbered list a description is read against, built
/// from [`BRIEF_POINTS`] so the questions the model is asked and the ids its `points` must carry are the
/// same six. Each line leads with the **id**, because the id is what an answer is keyed by — the one on
/// the screen, the one [`validate_brief`] checks, and the one a person's edit is recorded against.
fn brief_points_for_prompt() -> String {
    BRIEF_POINTS
        .iter()
        .enumerate()
        .map(|(index, point)| format!("{}. `{}` — {}", index + 1, point.id, point.question))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The capability vocabulary, rendered for the brief prompt: every known path grouped by its
/// category, so the model picks an exact name (`storage.sd`, `timekeeping.rtc`) instead of
/// paraphrasing one (`storage.sdcard`, `time.rtc`).
fn vocabulary_for_prompt() -> String {
    crate::capability_vocabulary::categorized_paths()
        .into_iter()
        .map(|(category, paths)| format!("- {category}: {}", paths.join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read a brief out of the model's answer — the same fence-forgiving JSON reader the spec uses, for the
/// same reason: a fence is a formatting habit rather than a different answer.
pub fn parse_brief(text: &str) -> Result<DesignBrief, String> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|rest| rest.trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    serde_json::from_str(body).map_err(|e| {
        format!(
            "the design brief did not parse: {e}. It is JSON of the form {{summary, framework, \
             requires, suggests, points, forks}} — see the brief request for the exact fields."
        )
    })
}

/// The rules a brief is held to: every `requires` path — the brief's own and the ones its forks offer —
/// is a capability the **vocabulary** knows; every `points` entry names one of the six points
/// ([`BRIEF_POINTS`]) and names it once; and `forks` keeps to its ceiling ([`BRIEF_FORK_CEILING`]) with
/// choices that actually differ.
///
/// Returns every problem rather than the first, exactly as [`validate`] does, so a repair round can fix
/// as much as it can in one turn. The rest of a brief is prose — a `summary`, a `said`, a `decided`, a
/// name — and **prose is not checked**: the brief guides a person, it does not gate them.
///
/// What *is* checked is what a person **cannot see from the screen**. A misspelled capability reads as a
/// capability. An entry pointing at `purpose2` reads as a reading that simply never appeared beside the
/// point it belongs to. Two entries for `timing` read as one point with two answers and no way to tell
/// which the design used. A fork offering the same capabilities twice reads as a choice where there is
/// none. Each is a mistake to correct rather than a reading to show.
///
/// What is deliberately **not** checked is a point the model left out. Under the old `gaps` a missing
/// question was invisible — the count moved and nothing on screen said which reading was short — which is
/// why the count itself had to be checked. A missing point is now a **blank row among six labelled
/// ones**: the label is on the screen, the reading is not, and the person sees exactly which question
/// went unread. A check a person can perform by looking is not the code's to perform, and failing a brief
/// over a visible gap would turn a reading into a dead step.
pub fn validate_brief(brief: &DesignBrief) -> Result<(), Vec<String>> {
    let mut problems: Vec<String> = Vec::new();

    // The brief's own capabilities and the ones its forks offer: the same check, because an unknown path
    // is a misspelling wherever it is written.
    for path in brief.requires.iter().chain(
        brief
            .forks
            .iter()
            .flat_map(|fork| &fork.choices)
            .flat_map(|choice| &choice.requires),
    ) {
        if !crate::capability_vocabulary::is_known_path(path) {
            problems.push(format!(
                "'{path}' is not a capability the vocabulary knows — a `requires` entry is a \
                 capability path like `sensing.particulate` or `media.display`, and a name nobody \
                 named is a misspelling rather than a new capability"
            ));
        }
    }

    // **The six points, once each.** An id nobody knows is a seventh point no row on screen can show,
    // and a repeated id is two readings of one point with nothing to say which the design used.
    let mut seen: Vec<&str> = Vec::new();
    for read in &brief.points {
        if !is_known_point(&read.point) {
            problems.push(format!(
                "`points` names '{}', which is not one of the six points — the ids are {}",
                read.point,
                point_ids()
            ));
        } else if seen.contains(&read.point.as_str()) {
            problems.push(format!(
                "`points` reads '{}' twice — the six points are one entry each, and two entries about \
                 one point are one point asked twice",
                read.point
            ));
        } else {
            seen.push(&read.point);
        }
        // A listed point with nothing in either cell is a point claimed and not read: the row would be
        // blank under a label that promises a reading.
        if read.said.trim().is_empty() && read.decided.trim().is_empty() {
            problems.push(format!(
                "`points` lists '{}' with nothing in `said` or `decided` — a point is read by saying \
                 what the description says about it (`said`) or by deciding it (`decided`), and an \
                 entry with neither has read nothing",
                read.point
            ));
        }
    }

    // **The forks.** The ceiling is stated in the request from this same constant, and the rest is what
    // makes a fork answerable in one click: two alternatives that bring different things.
    if brief.forks.len() > BRIEF_FORK_CEILING {
        problems.push(format!(
            "`forks` has {} entries, but a fork is kept for a decision that changes what the \
             application is given: keep at most {BRIEF_FORK_CEILING} and decide the rest yourself, \
             stating each decision in `decided`",
            brief.forks.len()
        ));
    }
    for (index, fork) in brief.forks.iter().enumerate() {
        let position = index + 1;
        if !is_known_point(&fork.point) {
            problems.push(format!(
                "fork {position} names '{}', which is not one of the six points — the ids are {}",
                fork.point,
                point_ids()
            ));
        }
        if fork.question.trim().is_empty() {
            problems.push(format!(
                "fork {position} has no `question` — the decision, phrased as the choice it is"
            ));
        }
        if fork.choices.len() < 2 {
            problems.push(format!(
                "fork {position} offers {} choice(s) — a fork is a pick between at least two named \
                 alternatives, and one answer is not a choice",
                fork.choices.len()
            ));
        }
        if fork
            .choices
            .iter()
            .any(|choice| choice.label.trim().is_empty())
        {
            problems.push(format!(
                "fork {position} has a choice with no `label` — each alternative needs the short name \
                 its button carries"
            ));
        }
        // Two choices that bring the same capabilities and units are one answer written twice: the person
        // would be picking between identical outcomes, and nothing about the application would change.
        let distinct = fork
            .choices
            .iter()
            .map(|choice| (&choice.requires, &choice.suggests))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        if !fork.choices.is_empty() && distinct < 2 {
            problems.push(format!(
                "fork {position} offers choices that bring the same capabilities and units — a fork \
                 exists because the answer changes what the application is given, and identical \
                 answers change nothing"
            ));
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// The six point ids, rendered for a refusal — `purpose`, `senses`, … — so a repair turn can name the
/// vocabulary rather than say "not one of the six" and leave the model to guess which one it invented.
fn point_ids() -> String {
    BRIEF_POINTS
        .iter()
        .map(|point| format!("`{}`", point.id))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The **brief phase's LLM stage**: ask, parse, check `requires` against the vocabulary, and repair.
///
/// Shaped like the AppSpec requirements pass — the LLM is an injected async `call`, the repair turn is
/// a whole self-contained message, and the attempts are bounded — so the brief is tested on canned
/// answers with no live actor and no network, and a model that cannot produce one is reported rather
/// than asked forever.
pub async fn brief_application<F, Fut>(description: &str, call: F) -> Result<DesignBrief, String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let request = brief_request(description);
    let mut prompt = request.clone();
    let mut last_problems: Vec<String> = Vec::new();

    for attempt in 1..=MAX_DESIGN_ATTEMPTS {
        let answer = call(prompt.clone()).await.map_err(|e| {
            format!(
                "the brief request was not answered (attempt {attempt} of {MAX_DESIGN_ATTEMPTS}): {e}"
            )
        })?;
        let problems = match parse_brief(&answer) {
            // A parse failure is a problem like any other, so a model that answered in prose gets told
            // so and asked again rather than failing the whole brief phase on a formatting habit.
            Err(parse_problem) => vec![parse_problem],
            Ok(brief) => match validate_brief(&brief) {
                Ok(()) => return Ok(brief),
                Err(problems) => problems,
            },
        };
        prompt = repair_request(&request, &answer, &problems);
        last_problems = problems;
    }

    Err(format!(
        "the brief phase produced no valid brief in {MAX_DESIGN_ATTEMPTS} attempts. The last \
         answer's problems were:\n  - {}",
        last_problems.join("\n  - ")
    ))
}

/// The board a design request names, from the `board` object a caller passed.
///
/// Refused rather than defaulted: the chip and the BSP decide the pin map, the dependency set and the
/// compile target, so a guess at either is a build that fails on hardware. `hal` is genuinely optional
/// — plenty of applications are written against the BSP alone.
pub fn board_from_json(value: &serde_json::Value) -> Result<BoardChoice, String> {
    let board: BoardChoice = serde_json::from_value(value.clone()).map_err(|e| {
        format!(
            "`board` is not a board ({e}): it needs a `chip`, and a `bsp` when the board has one"
        )
    })?;
    if board.chip.trim().is_empty() {
        return Err(
            "`board` needs a `chip` — it decides the compile target, and a guess at it is a build that \
             fails on hardware. A board with a published BSP wants one too; most have none, and for those \
             Spire generates its own backend"
                .to_string(),
        );
    }
    // **The shape of the two names, which nothing used to check.** They are entered by hand and then
    // carried verbatim: the chip routes the build, and the BSP becomes the *key* of a dependency in the
    // application's `main/idf_component.yml`. A trailing comma, a space or a stray slash there is not a
    // slight inaccuracy — it is a manifest the component manager cannot resolve, and a build that fails
    // *before the compiler sees a line of C++*, which is how a live run's invented component APIs went
    // unreported: the repair turn was handed output with no `error:` line in it and answered, correctly
    // and uselessly, that there was nothing to repair.
    //
    // What this is not: a check that the board *exists*. `m5stack/cores3` is a well-formed name and no
    // component, and telling those apart needs the registry — which is what a board picker over the
    // catalogue would be for. This refuses what cannot be a name at all.
    if !is_identifier(board.chip.trim()) {
        return Err(format!(
            "`board.chip` is not a chip name: '{}' — it is a bare identifier such as `esp32s3`, and it \
             decides the build target",
            board.chip.trim()
        ));
    }
    if !board.bsp.trim().is_empty() && !is_component_name(board.bsp.trim()) {
        return Err(format!(
            "`board.bsp` is not a component name: '{}' — a BSP is a registry component, written \
             `namespace/name` or as the name its SDK uses (`m5stack_core_s3`), and this becomes a key in \
             the application's `main/idf_component.yml`. A comma, a space or a stray slash there is a \
             manifest that cannot resolve",
            board.bsp.trim()
        ));
    }
    Ok(board)
}

/// A chip, or one half of a component name: letters, digits, `.`, `_`, `-`. Nothing else.
fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

/// Whether `name` is a registry **component name**: an optional `namespace/`, then a name.
///
/// Deliberately not "must be namespaced": the board catalogue stores the BSP as its SDK writes it
/// (`m5stack_core_s3`) and the registry form adds the namespace (`espressif/m5stack_core_s3`), so both
/// are legitimate answers and refusing either would refuse a board.
fn is_component_name(name: &str) -> bool {
    match name.split_once('/') {
        Some((namespace, rest)) => is_identifier(namespace) && is_identifier(rest),
        None => is_identifier(name),
    }
}

/// The framework an application **states** in its `CMakeLists.txt`, if it states one.
///
/// `Ok(None)` when the file says nothing, which is ordinary: an application made before the design
/// phase existed, or one whose design has not run. `Err` when it states a value this does not know —
/// that is not a missing fact but a *wrong* one, and a reader that quietly ignored it would run a
/// composition the application's own file says it did not choose.
///
/// A **comment** stating a framework is not a statement: `# set(SPIRE_APPLICATION_FRAMEWORK actors)`
/// is a note to a reader, and treating it as a choice would make a comment load-bearing.
pub fn declared_framework(contents: &str) -> Result<Option<ApplicationFramework>, String> {
    let key = format!("set({APPLICATION_FRAMEWORK_KEY}");
    let Some(value) = contents.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(&key)?;
        Some(rest.trim_end_matches(')').trim().to_string())
    }) else {
        return Ok(None);
    };
    framework_from_name(&value).map(Some)
}

/// The framework a caller pinned, by name — or `None` when it left the choice to the design.
///
/// A name this does not know is a refusal here for the same reason an unknown framework is one in
/// [`parse_spec`]: adding a framework is adding a `main/` template, a decomposition idiom and a host
/// test, so accepting the name would promise something that does not exist.
pub fn framework_from_name(name: &str) -> Result<ApplicationFramework, String> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).map_err(|_| {
        format!("'{name}' is not a framework this knows (actors, ramen), so it is not one to design for")
    })
}

/// A component a design asks a library to **gain**: a stub to write, with everything
/// `idf_add_component` needs to write it well.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentStub {
    pub name: String,
    /// What it is — which is what decides its skeleton. See `idf_projects::ComponentKind`.
    pub role: ComponentRole,
    /// A driver's bus. Absent for a library, which names no device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bus: Option<String>,
}

/// A component the application **depends on** rather than writes: its registry name, and the local
/// name the composition speaks.
///
/// A published component belongs to neither the library nor `main/` — it is a managed dependency, so
/// the scaffold writes it into the application's `main/idf_component.yml` and the library neither
/// owns it nor stubs it. The two names are kept apart because they are different things: `name` is
/// the unit id a reviewer and a `uses` refer to, and `registry` is what the component manager
/// resolves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedComponent {
    /// The local name — the unit id, which is what `uses` and the review speak.
    pub name: String,
    /// The registry's own `namespace/name`, as `idf_component.yml` needs it.
    pub registry: String,
}

/// What a design asks a component library to contain, set against what the library already has.
///
/// Four lists rather than a `bool`, because the third one is the point: a design and a library can
/// *disagree* — "the library already has this" when it does not — and which of the two is wrong is a
/// person's call. Filling the gap silently would write a component the design never asked to be
/// written; ignoring it would leave an application that cannot build.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentPlan {
    /// To write: the components the design names and the library does not have.
    pub add: Vec<ComponentStub>,
    /// Already there, so there is nothing to do — the ordinary case for `source: existing`, and the
    /// idempotent case for applying a design twice.
    pub present: Vec<String>,
    /// **Managed dependencies** resolved from the registry: the components the design marked
    /// `published`. They belong to the application's manifest, not to the library.
    pub published: Vec<PublishedComponent>,
    /// The design and the library disagree. Stated, never resolved.
    pub problems: Vec<String>,
}

/// Reconcile a design's components with the library they belong to.
///
/// `has` answers "does the library already have a component by this name" — a closure rather than a
/// path, because this is a pure function of the design and one fact about the tree, and that is what
/// makes it testable without a library on disk.
///
/// Composing units are not components and are not here: an actor is written into the application's
/// `main/`, not into the library. Neither are board facts — a component owns no address, which is the
/// rule that keeps it reusable, so they stay the application's.
pub fn component_plan(application: &ApplicationSpec, has: impl Fn(&str) -> bool) -> ComponentPlan {
    let mut plan = ComponentPlan::default();
    for unit in application
        .units
        .iter()
        .filter(|unit| unit.kind == UnitKind::Component)
    {
        // A **published** component is a managed dependency, not something the library writes: it is
        // read out of the registry at build time by the application's manifest, so it is never stubbed
        // and the library is never asked for it. Handled before `has`, deliberately — a local component
        // of the same name is a *conflict*, not a reason to skip the dependency.
        if unit.source == Some(UnitSource::Published) {
            match unit.registry.as_deref().map(str::trim) {
                Some(registry) if !registry.is_empty() => {
                    if has(&unit.id) {
                        plan.problems.push(format!(
                            "the design says '{}' is published ({registry}) and the library also has a \
                             component by that name — a managed dependency and a local component cannot \
                             share one name, so one of the two is wrong",
                            unit.id
                        ));
                    } else {
                        plan.published.push(PublishedComponent {
                            name: unit.id.clone(),
                            registry: registry.to_string(),
                        });
                    }
                }
                // `validate` refuses this, so reaching it means a plan was asked for from a spec nobody
                // checked: said rather than guessed.
                _ => plan.problems.push(format!(
                    "'{}' is published but names no registry component, so there is nothing to depend on",
                    unit.id
                )),
            }
            continue;
        }
        let Some(role) = unit.role else {
            // `validate` refuses this, so reaching it means a plan was asked for from a spec nobody
            // checked: said rather than guessed, because the role is what decides the skeleton.
            plan.problems.push(format!(
                "'{}' is a component with no role, so there is no shape to write",
                unit.id
            ));
            continue;
        };
        if has(&unit.id) {
            plan.present.push(unit.id.clone());
            continue;
        }
        match unit.source {
            Some(UnitSource::Existing) => plan.problems.push(format!(
                "the design says the library already has '{}' and it does not — add it (marking it \
                 `stub`, as one the design asks to be written) or design it out",
                unit.id
            )),
            // `stub` is the plain case. A component whose source was left unstated is treated the
            // same way, deliberately: the alternative is a component nobody ever writes.
            _ => plan.add.push(ComponentStub {
                name: unit.id.clone(),
                role,
                bus: unit.bus.clone(),
            }),
        }
    }
    plan
}

/// The **worked examples** — the decompositions that pin the schema, in the two frameworks.
///
/// They are public because they are documentation as much as fixtures: the schema is easiest to
/// understand by reading an application that passed it, and a *change* to a rule that breaks one of
/// these is a change to the design of both real applications. The tests in this module and the
/// creation flow's tests both use them, so there is exactly one copy of each.
pub mod examples {
    /// **The first application**: a PM2.5 meter on an M5Stack CoreS3, with a custom module carrying an
    /// SPS30 (particulates) and a GY213V/SHT20 (temperature and humidity), a battery block and a touch
    /// screen.
    ///
    /// It is the canonical *actors* decomposition, and the reason is the one the rule names: a touch
    /// screen, a reading a second, a battery gauge. `sps30` is a driver the library does not have (a
    /// stub to write), `moving_average` is one it does, and `air_quality` is the few lines that wrap it
    /// — which is exactly what "an actor is the application's composition" means.
    pub const PM25_METER: &str = r#"
    {
      "framework": "actors",
      "justification": "a touch UI, a reading a second and a battery gauge — interactive, human timescale",
      "board": { "chip": "esp32s3", "bsp": "m5stack_core_s3", "hal": "m5unified" },
      "board_facts": [
        { "bus": "i2c", "device": "sps30", "address": "0x69" },
        { "bus": "i2c", "device": "sht20", "address": "0x40" }
      ],
      "units": [
        { "id": "sps30", "kind": "component", "role": "driver", "source": "stub",
          "provides": "PM1/2.5/4/10 readings", "bus": "i2c" },
        { "id": "sht20", "kind": "component", "role": "driver", "source": "stub",
          "provides": "temperature and humidity", "bus": "i2c" },
        { "id": "moving_average", "kind": "component", "role": "library", "source": "existing",
          "provides": "a rolling window" },
        { "id": "sampler", "kind": "actor", "message": "Tick", "state": "reads both sensors",
          "uses": ["sps30", "sht20"], "sends_to": ["air_quality"] },
        { "id": "air_quality", "kind": "actor", "message": "Reading", "state": "the rolling average",
          "uses": ["moving_average"], "sends_to": ["view"] },
        { "id": "view", "kind": "actor", "message": "Report", "state": "the LVGL screen",
          "sends_to": [] },
        { "id": "touch", "kind": "actor", "message": "Gesture", "state": "menu and calibration",
          "sends_to": ["view", "sampler"] },
        { "id": "power", "kind": "actor", "message": "Tick", "state": "the battery gauge",
          "sends_to": ["view"] }
      ],
      "wiring": [
        "sampler -> air_quality", "air_quality -> view", "touch -> view",
        "touch -> sampler", "power -> view"
      ]
    }
    "#;

    /// **The third application**: an AI insect trap on an ESP32-P4-NANO — **a video/AI pipeline only**:
    /// a camera, a YOLOv8n detector through ESP-DL, the stages between them, and a last one that
    /// **saves an image** of each detection. There is no actuator and no GPIO: the pipeline's product is
    /// a stored image, not a pin going high.
    ///
    /// It is the canonical *ramen* decomposition: frames flow through stages at machine rate, and the
    /// chain is a DAG. Note the two ends — `capture` has only `pushes` because a frame pump reads a
    /// device rather than being handed a value, and `save_image` has only `pulls` because the verdicts
    /// are where the flow ends.
    ///
    /// **The camera is the board's**, and that is why it is not written down: the BSP carries the
    /// MIPI-CSI camera along with the pins, the display and the dependency set, so by the board
    /// paragraph's own rule it is *neither a unit nor a board fact* — `capture` reads it through the
    /// board. Which leaves `esp_dl` as the application's only unit.
    ///
    /// And it is the canonical **`published`** component: `esp_dl` is not written here at all — the
    /// runtime is Espressif's, and the application *depends on* it (`espressif/esp-dl`), which the
    /// scaffold writes into `main/idf_component.yml` rather than stubbing. So there is **no `stub` at
    /// all**: a video/AI pipeline's own code is its stages, which are the application's.
    pub const INSECT_TRAP: &str = r#"
    {
      "framework": "ramen",
      "justification": "camera frames flow through a detection pipeline at machine rate",
      "board": { "chip": "esp32p4", "bsp": "waveshare/esp32_p4_nano", "hal": "bsp" },
      "units": [
        { "id": "esp_dl", "kind": "component", "role": "library", "source": "published",
          "registry": "espressif/esp-dl", "provides": "the YOLOv8n inference runtime" },
        { "id": "capture", "kind": "stage", "pushes": "frames" },
        { "id": "preprocess", "kind": "stage", "pulls": "frames", "pushes": "tensors" },
        { "id": "detector", "kind": "stage", "uses": ["esp_dl"], "pulls": "tensors", "pushes": "detections" },
        { "id": "classifier", "kind": "stage", "pulls": "detections", "pushes": "verdicts" },
        { "id": "save_image", "kind": "stage", "pulls": "verdicts" }
      ],
      "wiring": [
        "capture -> preprocess", "preprocess -> detector",
        "detector -> classifier", "classifier -> save_image"
      ]
    }
    "#;
    /// **The PM2.5 meter's brief**: the same application as [`PM25_METER`], read out of the description
    /// *before* the board and the decomposition are chosen. It names the capabilities the application
    /// needs, the units it expects, and the description **read against each of the six points** — five
    /// answered by the text (`said`), the sixth decided by the model (`decided`), and **no forks**,
    /// because nothing in this description leaves a decision that would change what the application is
    /// given. It never names a board, a resource or a wiring edge, which is exactly the line the brief is
    /// drawn along.
    ///
    /// [`LOGGER_BRIEF`] is its counterpart: the same shape with one genuine decision left open, which is
    /// what a `forks` entry is for.
    pub const PM25_BRIEF: &str = r#"
    {
      "summary": "A PM2.5 meter with a touch screen: it samples particulate, temperature and humidity, shows the reading and a rolling average, and reports the battery.",
      "framework": "actors",
      "requires": ["sensing.particulate", "sensing.temperature", "sensing.humidity", "media.display", "media.touch", "power.battery"],
      "suggests": ["sampler", "air_quality", "view", "touch", "power"],
      "points": [
        { "point": "purpose",   "said": "reads particulate, temperature and humidity and shows them on a touch screen" },
        { "point": "senses",    "said": "particulate, temperature and humidity, about once a second" },
        { "point": "acts_on",   "said": "the display, and the touch screen it is driven by" },
        { "point": "reacts_to", "said": "a periodic tick, and a tap on the screen" },
        { "point": "timing",    "said": "a reading a second on one side, a human-rate UI on the other" },
        { "point": "missing",   "said": "", "decided": "a screen that says the reading is unavailable, keeping the last value" }
      ]
    }
    "#;

    /// **The data logger's brief**: a description that leaves one real decision open — **where the
    /// readings go** — so it is the shape a `forks` entry exists for. The two answers need different
    /// capabilities (`storage.sd` against `radio.wifi`) and different units, which is exactly the test a
    /// fork has to pass: if the answer would not change what the application is given, the brief decides
    /// it and says so in `decided` instead of offering it.
    ///
    /// Note what it does **not** do: it never asks the person anything in prose. The fork is two buttons,
    /// and picking one adds the paths that answer brings.
    pub const LOGGER_BRIEF: &str = r#"
    {
      "summary": "A temperature logger: it reads a sensor every minute and keeps the readings for later.",
      "framework": "actors",
      "requires": ["sensing.temperature"],
      "suggests": ["sampler"],
      "points": [
        { "point": "purpose",   "said": "reads a temperature every minute and keeps the readings" },
        { "point": "senses",    "said": "temperature, once a minute" },
        { "point": "acts_on",   "said": "", "decided": "a status LED, and the log itself" },
        { "point": "reacts_to", "said": "a periodic tick" },
        { "point": "timing",    "said": "one reading a minute, far below any UI rate" },
        { "point": "missing",   "said": "", "decided": "a missed read is skipped rather than queued" }
      ],
      "forks": [
        { "point": "purpose",
          "question": "Where do the readings go?",
          "choices": [
            { "label": "SD card", "requires": ["storage.sd"], "suggests": ["logger"] },
            { "label": "WiFi upload", "requires": ["radio.wifi"], "suggests": ["uploader"] }
          ] }
      ]
    }
    "#;
}

#[cfg(test)]
mod tests {
    use super::examples::{INSECT_TRAP, LOGGER_BRIEF, PM25_BRIEF, PM25_METER};
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    fn spec(json: &str) -> ApplicationSpec {
        parse_spec(json).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Every problem a fault produces, with the fault made by rewriting the working spec — so each
    /// negative case is one line, and the fixture it breaks stays visible.
    fn refusal(json: &str, from: &str, to: &str) -> Vec<String> {
        let broken = json.replace(from, to);
        assert_ne!(broken, json, "'{from}' is not in the fixture");
        validate(&spec(&broken)).expect_err("expected the spec to be refused")
    }

    fn refused(problems: &[String], needle: &str) {
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "no problem mentioned '{needle}': {problems:#?}"
        );
    }

    #[test]
    fn the_pm25_meter_decomposes_into_actors() {
        let spec = spec(PM25_METER);
        assert_eq!(spec.framework, ApplicationFramework::Actors);
        assert_eq!(spec.board.chip, "esp32s3");
        assert_eq!(spec.board.bsp, "m5stack_core_s3");
        assert_eq!(spec.board.hal, "m5unified");
        validate(&spec).expect("the canonical actors decomposition validates");

        // What a reviewer reads first: the two drivers the library does not have, the one it does, and
        // the actor that wraps it.
        let by_id = |id: &str| spec.units.iter().find(|u| u.id == id).expect(id);
        assert_eq!(by_id("sps30").role, Some(ComponentRole::Driver));
        assert_eq!(by_id("sps30").source, Some(UnitSource::Stub));
        assert_eq!(by_id("moving_average").role, Some(ComponentRole::Library));
        assert_eq!(by_id("moving_average").source, Some(UnitSource::Existing));
        assert_eq!(
            by_id("air_quality").uses,
            vec!["moving_average".to_string()]
        );
        assert_eq!(by_id("air_quality").message.as_deref(), Some("Reading"));
    }

    #[test]
    fn the_insect_trap_decomposes_into_stages() {
        let spec = spec(INSECT_TRAP);
        assert_eq!(spec.framework, ApplicationFramework::Ramen);
        validate(&spec).expect("the canonical ramen decomposition validates");

        let by_id = |id: &str| spec.units.iter().find(|u| u.id == id).expect(id);
        assert!(by_id("capture").pushes.is_some() && by_id("capture").pulls.is_none());
        assert!(by_id("save_image").pulls.is_some() && by_id("save_image").pushes.is_none());
        // The camera is the **board's** — the BSP carries it — so it is neither a unit nor a board fact,
        // and the source stage names nothing: it reads the device through the board.
        assert!(spec.board_facts.is_empty(), "{:?}", spec.board_facts);
        assert!(by_id("capture").uses.is_empty());
        // The one unit is the published runtime, which the detector wraps.
        assert_eq!(spec.units.len(), 6);
        assert_eq!(by_id("detector").uses, vec!["esp_dl".to_string()]);
    }
    /// A design whose actor receives a **message-set**: a supervisor that reacts to three kinds of
    /// message. Everything else about it is ordinary, so each fault below is the one line it breaks.
    const SUPERVISED_TRIAGE: &str = r#"{
      "framework": "actors",
      "justification": "a supervisor that reacts to a reading, a command and a heartbeat",
      "board": { "chip": "esp32s3", "bsp": "m5stack_core_s3" },
      "messages": [
        { "id": "supervision", "kinds": ["Reading", "Command", "Heartbeat"] }
      ],
      "units": [
        { "id": "sensor", "kind": "component", "role": "driver", "source": "stub", "bus": "i2c",
          "provides": "a reading" },
        { "id": "supervisor", "kind": "actor", "message": "supervision", "uses": ["sensor"],
          "state": "the last reading" },
        { "id": "view", "kind": "actor", "message": "Command" }
      ],
      "wiring": [ "supervisor -> view" ]
    }"#;

    #[test]
    fn a_message_set_is_the_several_kinds_one_actor_receives() {
        let spec = spec(SUPERVISED_TRIAGE);
        validate(&spec).expect("a set received by an actor validates");

        // The set is the id an actor's `message` speaks; its kinds are types of their own, so a kind a
        // second actor names on its own (`view` and `Command`) is one type, not two.
        assert_eq!(spec.messages.len(), 1);
        let set = spec.message_set("supervision").expect("the declared set");
        assert_eq!(
            set.kinds.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["Reading", "Command", "Heartbeat"]
        );
        assert!(spec.message_set("Command").is_none(), "a kind is not a set");
    }

    #[test]
    fn a_message_set_is_held_to_its_own_shape() {
        // One kind is not a set: `message: <kind>` says the same thing without a `std::variant`.
        refused(
            &refusal(
                SUPERVISED_TRIAGE,
                r#""kinds": ["Reading", "Command", "Heartbeat"]"#,
                r#""kinds": ["Reading"]"#,
            ),
            "names 1 kind(s)",
        );
        // Two entries that normalise to one name are one kind written twice.
        refused(
            &refusal(
                SUPERVISED_TRIAGE,
                r#""Reading", "Command""#,
                r#""Reading", "reading""#,
            ),
            "twice",
        );
        // A set no actor names states an inbox nothing receives — invented, like an unwired unit.
        refused(
            &refusal(
                SUPERVISED_TRIAGE,
                r#""message": "supervision""#,
                r#""message": "Reading""#,
            ),
            "named by no actor's `message`",
        );
    }

    #[test]
    fn a_message_set_cannot_be_named_for_a_type_the_design_declares() {
        // A set becomes `messages::Command`, a `std::variant` — and `Command` is also a `struct`, the
        // kind. One name cannot be both, so the set is the thing to rename.
        refused(
            &refusal(
                SUPERVISED_TRIAGE,
                r#""id": "supervision""#,
                r#""id": "Command""#,
            ),
            "one name cannot be both",
        );
        // Two sets sharing an id would make every `message` that names it ambiguous.
        refused(
            &refusal(
                SUPERVISED_TRIAGE,
                r#""messages": ["#,
                r#""messages": [ { "id": "supervision", "kinds": ["Halt", "Resume"] },"#,
            ),
            "declared twice",
        );
    }

    #[test]
    fn a_mixed_application_is_refused() {
        // An actor in a ramen application: the one thing "not yet" means.
        let problems = refusal(
            INSECT_TRAP,
            r#"{ "id": "esp_dl", "kind": "component""#,
            r#"{ "id": "watcher", "kind": "actor", "message": "Tick" },
        { "id": "esp_dl", "kind": "component""#,
        );
        refused(&problems, "one framework per application");
    }

    #[test]
    fn an_actor_without_a_message_is_refused() {
        let problems = refusal(PM25_METER, r#""message": "Reading", "#, "");
        refused(&problems, "is an actor with no message");
    }

    #[test]
    fn a_stage_with_no_ports_is_refused() {
        let problems = refusal(
            INSECT_TRAP,
            r#"{ "id": "preprocess", "kind": "stage", "pulls": "frames", "pushes": "tensors" }"#,
            r#"{ "id": "preprocess", "kind": "stage" }"#,
        );
        refused(&problems, "is a stage with no ports");
    }

    #[test]
    fn a_reference_to_nothing_is_refused() {
        let problems = refusal(
            PM25_METER,
            r#""air_quality -> view""#,
            r#""air_quality -> screen""#,
        );
        refused(&problems, "which is not a unit");
    }

    #[test]
    fn wiring_a_component_is_refused() {
        // The rule that keeps a component framework-agnostic: a driver is *used*, never wired.
        let problems = refusal(
            INSECT_TRAP,
            r#""classifier -> save_image""#,
            r#""classifier -> esp_dl""#,
        );
        refused(&problems, "which is a component");
    }

    #[test]
    fn a_dataflow_cycle_is_refused() {
        // A cycle in a *synchronous* dataflow is unbounded recursion on one stack, not a loop.
        let problems = refusal(
            INSECT_TRAP,
            r#""capture -> preprocess""#,
            r#""capture -> preprocess", "classifier -> capture""#,
        );
        refused(&problems, "the dataflow has a cycle");
    }

    /// **An actor cycle is a design, not a mistake.** `a_dataflow_cycle_is_refused` covers the `ramen`
    /// half; this is the other half, and it is what lets a legitimate mutual reference — a menu and the
    /// screen it drives — through `validate` rather than being refused as a DAG error. The framework
    /// resolves the pair by name through the registry, which only earns its place if the cycle is legal.
    #[test]
    fn an_actors_cycle_is_allowed() {
        let broken = PM25_METER.replace(
            r#""touch -> sampler""#,
            r#""touch -> sampler", "sampler -> touch""#,
        );
        assert_ne!(broken, PM25_METER, "the fixture has the edge to extend");
        validate(&spec(&broken)).expect("an actor cycle validates");
    }

    #[test]
    fn a_driver_without_a_bus_is_refused() {
        let problems = refusal(
            PM25_METER,
            r#""provides": "PM1/2.5/4/10 readings", "bus": "i2c""#,
            r#""provides": "PM1/2.5/4/10 readings""#,
        );
        refused(&problems, "is a driver with no bus");
    }

    #[test]
    fn a_library_with_a_bus_is_refused() {
        let problems = refusal(
            PM25_METER,
            r#""provides": "a rolling window" }"#,
            r#""provides": "a rolling window", "bus": "i2c" }"#,
        );
        refused(&problems, "is a library with a bus");
    }

    #[test]
    fn a_component_carrying_composition_is_refused() {
        let problems = refusal(
            PM25_METER,
            r#""provides": "PM1/2.5/4/10 readings", "bus": "i2c""#,
            r#""provides": "PM1/2.5/4/10 readings", "bus": "i2c", "message": "Tick""#,
        );
        refused(&problems, "carrying composition");
    }

    #[test]
    fn a_board_fact_for_something_that_is_not_a_driver_is_refused() {
        let problems = refusal(
            PM25_METER,
            r#"{ "bus": "i2c", "device": "sht20", "address": "0x40" }"#,
            r#"{ "bus": "i2c", "device": "moving_average", "address": "0x40" }"#,
        );
        refused(&problems, "is not a driver component");
    }

    #[test]
    fn an_invented_unit_nobody_uses_is_refused() {
        // Dropping the `uses` leaves `moving_average` referenced by nothing at all.
        let problems = refusal(PM25_METER, r#""uses": ["moving_average"], "#, "");
        refused(&problems, "referenced by no wiring edge");
    }

    #[test]
    fn a_published_component_is_a_managed_dependency() {
        // The insect trap's `esp_dl` is the canonical case: a component nobody here writes, that the
        // application depends on by its registry name.
        let spec = spec(INSECT_TRAP);
        validate(&spec).expect("the trap validates with a published component");
        let esp_dl = spec
            .units
            .iter()
            .find(|unit| unit.id == "esp_dl")
            .expect("esp_dl");
        assert_eq!(esp_dl.source, Some(UnitSource::Published));
        assert_eq!(esp_dl.registry.as_deref(), Some("espressif/esp-dl"));
        assert_eq!(esp_dl.role, Some(ComponentRole::Library));
        // A published library names no bus — nothing is written from it.
        assert!(esp_dl.bus.is_none());
    }

    #[test]
    fn a_published_component_with_no_registry_name_is_refused() {
        let problems = refusal(INSECT_TRAP, r#""registry": "espressif/esp-dl", "#, "");
        refused(&problems, "is published but names no registry component");
    }

    #[test]
    fn a_local_source_carrying_a_registry_name_is_refused() {
        // A registry name on `stub`/`existing` would promise a dependency that is not one.
        let problems = refusal(
            PM25_METER,
            r#""provides": "a rolling window" }"#,
            r#""provides": "a rolling window", "registry": "espressif/esp-sps30" }"#,
        );
        refused(
            &problems,
            "carries a registry name but its source is not `published`",
        );
    }

    #[test]
    fn a_published_driver_needs_no_bus() {
        // Nothing is written from a published component, so the design is not asked for its bus — the
        // rule that a *written* driver must state one is about the protocol the scaffold generates.
        let mut published_driver = spec(INSECT_TRAP);
        for unit in &mut published_driver.units {
            if unit.id == "esp_dl" {
                unit.role = Some(ComponentRole::Driver);
                unit.bus = None;
            }
        }
        validate(&published_driver).expect("a published driver with no bus validates");

        // The same component written here does need one: the contrast is the whole point of the rule.
        for unit in &mut published_driver.units {
            if unit.id == "esp_dl" {
                unit.source = Some(UnitSource::Stub);
                unit.registry = None;
            }
        }
        let problems =
            validate(&published_driver).expect_err("a stub driver with no bus is refused");
        refused(&problems, "is a driver with no bus");
    }

    #[test]
    fn a_framework_the_tool_does_not_know_is_refused_not_guessed() {
        let err = parse_spec(&PM25_METER.replace(r#""actors""#, r#""espp""#))
            .expect_err("an unknown framework is a refusal");
        assert!(err.contains("did not parse"), "{err}");
    }

    #[test]
    fn a_fenced_answer_parses() {
        // Models fence their JSON; a fence is a habit, not a different answer.
        let fenced = format!("```json\n{}\n```", PM25_METER.trim());
        validate(&spec(&fenced)).expect("a fenced spec validates like a bare one");
    }

    #[test]
    fn the_marker_line_is_what_a_cmakefile_states() {
        assert_eq!(
            ApplicationFramework::Actors.marker_line(),
            "set(SPIRE_APPLICATION_FRAMEWORK actors)"
        );
        assert_eq!(
            ApplicationFramework::Ramen.marker_line(),
            "set(SPIRE_APPLICATION_FRAMEWORK ramen)"
        );
    }

    #[test]
    fn a_board_needs_a_chip_and_a_bsp_when_it_has_one() {
        use serde_json::json;
        // The chip decides the target, so it is refused when absent — a guess at it is a build that fails
        // on hardware. The BSP is **not** required: the catalogue has one on one board in ten, and an
        // empty one means Spire generates its own backend, which is an answer rather than a gap.
        assert!(board_from_json(&json!({})).is_err(), "no chip");
        assert!(
            board_from_json(&json!({ "chip": "", "bsp": "m5stack_core_s3" })).is_err(),
            "a BSP is not a chip"
        );

        let chip_only =
            board_from_json(&json!({ "chip": "esp32s3" })).expect("a board with no BSP is a board");
        assert_eq!(
            chip_only.bsp, "",
            "and leaving the key out is that, not a parse failure"
        );

        let board = board_from_json(&json!({ "chip": "esp32s3", "bsp": "m5stack_core_s3" }))
            .expect("a chip and a BSP are enough");
        assert_eq!(board.bsp, "m5stack_core_s3");
        assert_eq!(board.hal, "", "a board abstraction is genuinely optional");

        let with_hal = board_from_json(&json!({
            "chip": "esp32s3", "bsp": "m5stack_core_s3", "hal": "m5unified"
        }))
        .expect("and naming one is fine");
        assert_eq!(with_hal.hal, "m5unified");
    }

    #[test]
    fn a_board_whose_names_are_not_names_is_refused() {
        use serde_json::json;

        // **The names are carried verbatim**: the chip routes the build, and the BSP becomes a
        // dependency *key* in the application's `main/idf_component.yml`. A trailing comma typed into the
        // board form is a manifest the component manager cannot resolve — a build that fails *before the
        // compiler*, which is how a live run's invented component APIs went unreported: the repair turn
        // was handed output with no `error:` line and answered, correctly and uselessly, that there was
        // nothing to repair.
        for (chip, bsp) in [
            ("esp32s3", "espressif/m5stack_core_s3,"),
            ("esp32s3", "m5stack core s3"),
            ("esp32s3", "m5stack/"),
            ("esp32s3", "/cores3"),
            ("esp32s3", "a/b/c"),
            ("esp32s3,", "espressif/m5stack_core_s3"),
        ] {
            let refused = board_from_json(&json!({ "chip": chip, "bsp": bsp }))
                .expect_err("a comma, a space or a stray slash is not a name");
            assert!(
                refused.contains("is not a component name")
                    || refused.contains("is not a chip name"),
                "{chip} / {bsp}: {refused}"
            );
        }

        // Both spellings of a real board are legitimate — the name its SDK writes, and the registry's —
        // so neither is refused. Refusing either would refuse a board.
        for bsp in ["m5stack_core_s3", "espressif/m5stack_core_s3"] {
            assert!(
                board_from_json(&json!({ "chip": "esp32s3", "bsp": bsp })).is_ok(),
                "{bsp} is a board"
            );
        }
        // And a well-formed name for a board that does not exist still passes: shape is not existence,
        // which is what a picker over the board catalogue would be for.
        assert!(board_from_json(&json!({ "chip": "esp32s3", "bsp": "m5stack/cores3" })).is_ok());
    }

    #[test]
    fn a_framework_name_is_pinned_or_refused_by_name() {
        assert_eq!(
            framework_from_name("actors"),
            Ok(ApplicationFramework::Actors)
        );
        assert_eq!(
            framework_from_name("ramen"),
            Ok(ApplicationFramework::Ramen)
        );

        // Not guessed at, and not silently defaulted: the name comes back in the refusal, because the
        // caller has to be able to see what it asked for.
        let err = framework_from_name("espp").expect_err("an unknown framework is a refusal");
        assert!(err.contains("espp"), "{err}");
        assert!(
            err.contains("actors, ramen"),
            "and it says what it does know: {err}"
        );
    }

    #[test]
    fn a_framework_is_read_back_from_the_application_that_states_it() {
        let stated = format!(
            "cmake_minimum_required(VERSION 3.16)\n{}\nproject(demo)\n",
            ApplicationFramework::Actors.marker_line()
        );
        assert_eq!(
            declared_framework(&stated),
            Ok(Some(ApplicationFramework::Actors))
        );

        // An application that says nothing is ordinary, not an error.
        assert_eq!(declared_framework("project(demo)\n"), Ok(None));

        // A *comment* is a note to a reader, not a choice — otherwise a comment would be load-bearing.
        assert_eq!(
            declared_framework("# set(SPIRE_APPLICATION_FRAMEWORK actors)\nproject(demo)\n"),
            Ok(None)
        );

        // A value this does not know is a wrong fact, said by the application's own file: refused by
        // name, so nobody silently runs a composition the file did not choose.
        let err = declared_framework("set(SPIRE_APPLICATION_FRAMEWORK espp)\n")
            .expect_err("an unknown framework is a refusal");
        assert!(err.contains("espp"), "{err}");
    }

    #[test]
    fn a_unit_declared_twice_is_refused() {
        // An id is what the wiring, the board facts, `uses` and `sends_to` refer *by*, so two units
        // sharing one makes every one of those ambiguous — and `unit(id)` would answer with the first,
        // reading as though the second did not exist.
        let doubled = PM25_METER.replace(
            r#"{ "id": "view", "kind": "actor""#,
            r#"{ "id": "power", "kind": "actor", "message": "Tick" },
        { "id": "view", "kind": "actor""#,
        );
        let problems = validate(&spec(&doubled)).expect_err("refused");
        refused(&problems, "is declared twice");
    }

    #[test]
    fn a_design_is_reconciled_with_the_library_it_belongs_to() {
        let app_one = spec(PM25_METER);

        // An empty library: the two stubs are asked for, and `moving_average` — which the design says
        // the library *has* — is a disagreement rather than a silent addition. Nothing composing
        // appears: an actor is written into `main/`, not into the library.
        let empty = |_name: &str| false;
        let plan = component_plan(&app_one, empty);
        assert_eq!(
            plan.add
                .iter()
                .map(|stub| (stub.name.as_str(), stub.role, stub.bus.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("sps30", ComponentRole::Driver, Some("i2c")),
                ("sht20", ComponentRole::Driver, Some("i2c")),
            ]
        );
        assert_eq!(plan.problems.len(), 1);
        assert!(
            plan.problems[0].contains("already has 'moving_average' and it does not"),
            "{:?}",
            plan.problems
        );

        // A design that asks for a **library** component to be written names no bus, and gets one
        // entry with no bus at all — the difference the kind makes.
        let mut writing_the_library = app_one.clone();
        for unit in &mut writing_the_library.units {
            if unit.id == "moving_average" {
                unit.source = Some(UnitSource::Stub);
            }
        }
        let plan = component_plan(&writing_the_library, empty);
        assert_eq!(
            plan.add
                .iter()
                .map(|stub| (stub.name.as_str(), stub.role, stub.bus.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("sps30", ComponentRole::Driver, Some("i2c")),
                ("sht20", ComponentRole::Driver, Some("i2c")),
                ("moving_average", ComponentRole::Library, None),
            ]
        );
        assert!(plan.problems.is_empty());

        // Everything already there: nothing to do and nothing to disagree about, which is what makes
        // applying a design twice safe.
        let complete = |_name: &str| true;
        let plan = component_plan(&app_one, complete);
        assert!(plan.add.is_empty() && plan.problems.is_empty());
        assert_eq!(plan.present, vec!["sps30", "sht20", "moving_average"]);
    }

    #[test]
    fn a_published_component_is_a_plan_of_its_own() {
        // `published` is neither written into the library nor reported as "the library has it": it is a
        // managed dependency, and the plan carries the registry name so the scaffold can declare it.
        let trap = spec(INSECT_TRAP);
        let empty = |_name: &str| false;
        let plan = component_plan(&trap, empty);
        assert_eq!(
            plan.published
                .iter()
                .map(|dep| (dep.name.as_str(), dep.registry.as_str()))
                .collect::<Vec<_>>(),
            vec![("esp_dl", "espressif/esp-dl")]
        );
        assert!(
            plan.add.is_empty(),
            "a video/AI pipeline writes no component of its own: {:?}",
            plan.add
        );
        // The only unit is the published one, so there is nothing to disagree about either.
        assert!(
            plan.problems.is_empty(),
            "nothing disagrees: {:?}",
            plan.problems
        );

        // And when the library *does* have a component by that name, it is a conflict rather than a
        // dependency: a managed component and a local one cannot share one name.
        let collides = |name: &str| name == "esp_dl";
        let plan = component_plan(&trap, collides);
        assert!(plan.published.is_empty(), "{plan:?}");
        assert!(plan
            .problems
            .iter()
            .any(|problem| problem.contains("also has a component by that name")));
    }

    /// A fake LLM service: canned answers in order (the last one repeats), and the prompts it was
    /// asked.
    ///
    /// Not a mock of the transport — it is the closed `call` the brief is written against,
    /// which is what the injection is for: the repair path is tested by *answering badly once*, not by
    /// pretending to be an HTTP client.
    #[derive(Clone)]
    struct FakeLlm {
        answers: Arc<Mutex<VecDeque<Result<String, String>>>>,
        prompts: Arc<Mutex<Vec<String>>>,
    }

    impl FakeLlm {
        fn new(answers: Vec<Result<String, String>>) -> Self {
            Self {
                answers: Arc::new(Mutex::new(answers.into())),
                prompts: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }

        async fn call(&self, prompt: String) -> Result<String, String> {
            self.prompts.lock().unwrap().push(prompt);
            let mut answers = self.answers.lock().unwrap();
            if answers.len() > 1 {
                answers.pop_front().unwrap()
            } else {
                answers
                    .front()
                    .cloned()
                    .unwrap_or_else(|| Err("the fake LLM has no answer left".into()))
            }
        }
    }

    // --- the design brief: a description read before a board is chosen -----------------------------

    /// One run of the brief phase against a fake LLM, over the PM2.5 meter's description.
    fn brief_with(llm: &FakeLlm) -> Result<DesignBrief, String> {
        let llm = llm.clone();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(brief_application(
                "a PM2.5 meter with a touch screen",
                move |prompt| {
                    let llm = llm.clone();
                    async move { llm.call(prompt).await }
                },
            ))
    }

    #[test]
    fn a_valid_brief_is_read_without_repair() {
        let llm = FakeLlm::new(vec![Ok(PM25_BRIEF.to_string())]);
        let brief = brief_with(&llm).expect("a valid answer is the brief");

        assert_eq!(brief.framework, Some(ApplicationFramework::Actors));
        // The capabilities are the point: board-agnostic paths the vocabulary knows.
        assert!(brief.requires.contains(&"sensing.particulate".to_string()));
        assert_eq!(llm.prompts().len(), 1, "one round trip, no repair");

        // The reading: all six points, once each, and the one the description left open decided rather
        // than asked.
        assert_eq!(brief.points.len(), BRIEF_POINTS.len());
        for point in BRIEF_POINTS {
            assert!(
                brief.points.iter().any(|read| read.point == point.id),
                "the brief reads `{}`",
                point.id
            );
        }
        let decided: Vec<&str> = brief
            .points
            .iter()
            .filter(|read| !read.decided.is_empty())
            .map(|read| read.point.as_str())
            .collect();
        assert_eq!(
            decided,
            vec!["missing"],
            "the description said everything but what happens when something is missing, and that one is \
             decided rather than asked"
        );
        assert!(
            brief.forks.is_empty(),
            "and nothing here needs the person to pick"
        );
    }

    /// The other half of the shape: a description that leaves one real decision open comes back with a
    /// **fork** whose alternatives carry the capabilities they would add — and it is a valid brief as it
    /// stands, because a fork is an answer rather than a failure to answer.
    #[test]
    fn a_brief_with_a_fork_is_read_without_repair() {
        let llm = FakeLlm::new(vec![Ok(LOGGER_BRIEF.to_string())]);
        let brief = brief_with(&llm).expect("a valid answer is the brief");
        assert_eq!(llm.prompts().len(), 1, "one round trip, no repair");

        assert_eq!(
            brief.forks.len(),
            1,
            "one decision, and it is a pick: {brief:?}"
        );
        let fork = &brief.forks[0];
        assert_eq!(fork.point, "purpose");
        assert_eq!(fork.choices.len(), 2, "two named alternatives: {fork:?}");
        assert_eq!(
            fork.choices[0].requires,
            vec!["storage.sd".to_string()],
            "and each carries the capability it would add"
        );
        assert_eq!(fork.choices[1].requires, vec!["radio.wifi".to_string()]);

        // The reading is complete as well: six points, the two open ones decided rather than asked.
        assert_eq!(brief.points.len(), BRIEF_POINTS.len());
        assert_eq!(
            brief
                .points
                .iter()
                .filter(|read| !read.decided.is_empty())
                .count(),
            2,
            "the model decided what it could and left only the real either/or standing: {brief:?}"
        );
    }

    #[test]
    fn the_brief_request_asks_for_a_brief_and_not_a_board() {
        // The whole reason the brief exists: it decides *what the application has to do* and nothing
        // about the hardware. A prompt that named a board would move the board step onto step one.
        let request = brief_request("a PM2.5 meter with a touch screen");
        for question in [
            "what it does",
            "what it **senses**",
            "what it **acts on**",
            "what it **reacts to over time**",
            "**timing**",
            "when something is missing",
        ] {
            assert!(
                request.contains(question),
                "the brief request carries the six questions: missing {question}"
            );
        }
        assert!(
            request.contains("Do **not** name a board"),
            "and says the board is not the brief's to decide"
        );
        assert!(
            request.contains("capabilit"),
            "and names what `requires` is: capability paths"
        );
        // The six points are the code's list, not the model's invention: the request names each id,
        // because the id is what a `points` entry is keyed by and what `validate_brief` checks.
        for point in BRIEF_POINTS {
            assert!(
                request.contains(&format!("`{}`", point.id)),
                "the brief request names the point `{}`",
                point.id
            );
        }
    }

    #[test]
    fn the_brief_request_never_asks_for_a_hardware_fact() {
        // The failure this rule exists for: given a description that names a part but no address, the
        // model asked the person for the I²C address. That answer comes from the board and
        // device-facts stores at a later step, so the brief is told, in as many words, not to ask.
        let request = brief_request("a PM2.5 meter with a touch screen");
        assert!(
            request.contains("never ask the person for an I²C address"),
            "the brief request refuses to ask for an address:\n{request}"
        );
        assert!(
            request.contains("a device is named rather than addressed"),
            "and says why: a device is named, not addressed:\n{request}"
        );
        assert!(
            request.contains("hardware fact"),
            "and names the class: a hardware fact is never a fork:\n{request}"
        );
    }

    #[test]
    fn the_brief_request_never_asks_for_an_appearance() {
        // The next failure after the hardware one, and the same shape: given a description that names a
        // screen, the model asked the person how the UI should look and which icons to use. A look is
        // the model's to propose, not a requirement to extract, so the brief is told to decide it.
        let request = brief_request("a PM2.5 meter with a touch screen");
        assert!(
            request.contains("never about appearance"),
            "the brief request names appearance as never a fork:\n{request}"
        );
        assert!(
            request.contains("yours to propose"),
            "and says the look is the model's to propose:\n{request}"
        );
        for requirement in ["how many screens", "never a fork"] {
            assert!(
                request.contains(requirement),
                "and keeps the display's function the model's to decide, and appearance out of \
                 `forks`: missing {requirement:?}:\n{request}"
            );
        }
    }

    #[test]
    fn the_brief_request_names_the_vocabulary_exactly() {
        // The failure this fixes: a model asked only for "capabilities" guessed `storage.sdcard` and
        // `time.rtc` when the vocabulary spells them `storage.sd` and `timekeeping.rtc`. Naming the exact
        // paths is what lets a brief pick one instead of paraphrasing it.
        let request = brief_request("a data logger on an SD card with a real-time clock");
        for path in ["storage.sd", "timekeeping.rtc", "sensing.temperature"] {
            assert!(
                request.contains(path),
                "the brief request names the vocabulary, including {path}:\n{request}"
            );
        }
        assert!(
            request.contains("## The capability vocabulary"),
            "and calls it out as the list to choose from:\n{request}"
        );
    }

    #[test]
    fn the_brief_request_asks_for_a_reading_and_not_an_interview() {
        // The failure this fixes, measured live: told to *report* gaps, the model asked 0, then 1, then 3,
        // then 6, then 10 questions about one unchanged description — a count nobody can reproduce,
        // because "how much did this leave open" is a judgement and not a fact. The request no longer asks
        // for that judgement: it asks for **a reading of all six points**, with a decision wherever the
        // description is silent, so the person edits the decision instead of answering the question.
        let request = brief_request("a PM2.5 meter with a touch screen");
        for phrase in [
            "answer every one",
            "a brief is a reading, not an interview",
            "Never hand the person a question",
            "You draft; they edit your",
            "decide the point",
        ] {
            assert!(
                request.contains(phrase),
                "the brief request asks for a reading, missing {phrase:?}:\n{request}"
            );
        }

        // And the one question that survives is bounded and countable, from the same constant
        // `validate_brief` checks: a fork is a pick between named alternatives that carry what they add.
        let ceiling = format!("At most {BRIEF_FORK_CEILING}");
        assert!(
            request.contains(&ceiling),
            "the brief request states the fork ceiling as a number ({ceiling}):\n{request}"
        );
        for phrase in ["pick, not a question", "never a fork", "carries the"] {
            assert!(
                request.contains(phrase),
                "and says what a fork is and is not, missing {phrase:?}:\n{request}"
            );
        }

        // Nothing in the request offers the old shape: no `gaps` key, and so no count for a person to
        // read as a score.
        assert!(
            !request.contains("\"gaps\""),
            "the request no longer offers `gaps` as a field:\n{request}"
        );
    }

    #[test]
    fn the_forks_a_brief_may_hand_back_are_capped() {
        // A fork is the one question left in a brief, so it keeps to the request's own number, from the
        // same constant: a brief handing back five has not read the text, it has interviewed the person.
        let fork = |n: usize| BriefFork {
            point: "purpose".to_string(),
            question: format!("decision {n}"),
            choices: vec![
                BriefChoice {
                    label: "one".to_string(),
                    suggests: vec![format!("unit_{n}_one")],
                    ..BriefChoice::default()
                },
                BriefChoice {
                    label: "the other".to_string(),
                    suggests: vec![format!("unit_{n}_other")],
                    ..BriefChoice::default()
                },
            ],
        };
        let over: Vec<BriefFork> = (1..=BRIEF_FORK_CEILING + 1).map(fork).collect();
        let problems = validate_brief(&DesignBrief {
            forks: over,
            ..DesignBrief::default()
        })
        .expect_err("more forks than the ceiling allows");
        assert_eq!(problems.len(), 1, "one problem, named once: {problems:?}");
        assert!(
            problems[0].contains(&format!("has {} entries", BRIEF_FORK_CEILING + 1)),
            "the problem says how many came back: {:?}",
            problems[0]
        );
        assert!(
            problems[0].contains(&format!("at most {BRIEF_FORK_CEILING}")),
            "and what the ceiling is: {:?}",
            problems[0]
        );

        // At the ceiling is fine.
        let at: Vec<BriefFork> = (1..=BRIEF_FORK_CEILING).map(fork).collect();
        assert!(
            validate_brief(&DesignBrief {
                forks: at,
                ..DesignBrief::default()
            })
            .is_ok(),
            "a fork per real decision is allowed"
        );
    }

    #[test]
    fn a_brief_that_asks_too_much_is_repaired_into_a_reading() {
        // What the repair is *for*: the person never sees the brief that asked three questions, and the
        // questions are not silently dropped either — the model is told what a fork is for and asked
        // again, so what reaches the screen is the model's own reading with the one decision it could not
        // settle on its own.
        let interviewing = r#"
        {
          "summary": "a temperature logger",
          "requires": ["sensing.temperature"],
          "forks": [
            { "point": "purpose", "question": "SD card or WiFi?", "choices": [
                { "label": "SD card", "requires": ["storage.sd"] },
                { "label": "WiFi", "requires": ["radio.wifi"] } ] },
            { "point": "senses", "question": "how often?", "choices": [
                { "label": "every minute", "suggests": ["fast_sampler"] },
                { "label": "every hour", "suggests": ["slow_sampler"] } ] },
            { "point": "acts_on", "question": "LED or buzzer?", "choices": [
                { "label": "LED", "suggests": ["led"] },
                { "label": "buzzer", "suggests": ["buzzer"] } ] }
          ]
        }
        "#;
        let llm = FakeLlm::new(vec![
            Ok(interviewing.to_string()),
            Ok(LOGGER_BRIEF.to_string()),
        ]);
        let brief = brief_with(&llm).expect("the repaired answer is the brief");

        let prompts = llm.prompts();
        assert_eq!(prompts.len(), 2, "the answer was repaired once");
        assert!(
            prompts[1].contains("`forks` has 3 entries"),
            "the repair turn names the count:\n{}",
            prompts[1]
        );
        assert!(
            prompts[1].contains("a fork is kept for a decision that changes"),
            "and says what a fork is for, so the model decides the rest:\n{}",
            prompts[1]
        );
        assert_eq!(
            brief.forks.len(),
            1,
            "the person is handed the repaired brief, not three choices: {:?}",
            brief.forks
        );
    }

    #[test]
    fn a_brief_that_names_a_point_nobody_knows_is_a_problem() {
        // The ids are the code's, not the model's. An invented one (`purpose2`) is a reading no row on
        // screen can show, and a repeated one is two answers to one point with nothing to say which the
        // design used — both invisible to the person looking at the card, which is why both are checked.
        let read = |point: &str| BriefPointRead {
            point: point.to_string(),
            said: format!("what the description says about {point}"),
            ..BriefPointRead::default()
        };
        let problems = validate_brief(&DesignBrief {
            points: vec![read("purpose2")],
            ..DesignBrief::default()
        })
        .expect_err("an id nobody knows");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("purpose2"), "{:?}", problems[0]);
        assert!(
            problems[0].contains("`missing`"),
            "the refusal names the six ids rather than only refusing: {:?}",
            problems[0]
        );

        let problems = validate_brief(&DesignBrief {
            points: vec![read("timing"), read("timing")],
            ..DesignBrief::default()
        })
        .expect_err("one point read twice");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("twice"), "{:?}", problems[0]);

        // A point listed with nothing read and nothing decided is a point claimed, not read.
        let problems = validate_brief(&DesignBrief {
            points: vec![BriefPointRead {
                point: "senses".to_string(),
                ..BriefPointRead::default()
            }],
            ..DesignBrief::default()
        })
        .expect_err("a blank reading");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("has read nothing"),
            "{:?}",
            problems[0]
        );

        // The six, read, are fine.
        let all: Vec<BriefPointRead> = BRIEF_POINTS.iter().map(|point| read(point.id)).collect();
        assert!(
            validate_brief(&DesignBrief {
                points: all,
                ..DesignBrief::default()
            })
            .is_ok(),
            "six points, once each"
        );

        // A point the model left out is **not** a problem: it is a blank row under a label the person can
        // read, and a check a person can perform by looking is not the code's to perform.
        assert!(
            validate_brief(&DesignBrief {
                points: vec![read("purpose")],
                ..DesignBrief::default()
            })
            .is_ok(),
            "an unread point is visible on screen, not a dead step"
        );
    }

    #[test]
    fn a_fork_that_offers_one_answer_or_two_identical_ones_is_a_problem() {
        // A fork is answerable because its alternatives bring different things. One choice is not a
        // choice, and two choices that add the same capabilities change nothing whichever is picked —
        // both are forks that would sit on screen as a decision with no decision in it.
        let one = BriefFork {
            point: "purpose".to_string(),
            question: "where do the readings go?".to_string(),
            choices: vec![BriefChoice {
                label: "SD card".to_string(),
                requires: vec!["storage.sd".to_string()],
                ..BriefChoice::default()
            }],
        };
        let problems = validate_brief(&DesignBrief {
            forks: vec![one],
            ..DesignBrief::default()
        })
        .expect_err("one choice is not a choice");
        assert_eq!(
            problems.len(),
            2,
            "no second alternative, and nothing for it to differ from: {problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("at least two")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("same capabilities")),
            "{problems:?}"
        );

        let identical = BriefFork {
            point: "purpose".to_string(),
            question: "where do the readings go?".to_string(),
            choices: vec![
                BriefChoice {
                    label: "SD card".to_string(),
                    requires: vec!["storage.sd".to_string()],
                    suggests: vec!["logger".to_string()],
                },
                BriefChoice {
                    label: "on the card".to_string(),
                    requires: vec!["storage.sd".to_string()],
                    suggests: vec!["logger".to_string()],
                },
            ],
        };
        let problems = validate_brief(&DesignBrief {
            forks: vec![identical.clone()],
            ..DesignBrief::default()
        })
        .expect_err("two answers that change nothing");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("change nothing"), "{:?}", problems[0]);

        // A capability a choice names is checked exactly as the brief's own are: a misspelling inside a
        // fork is the same mistake, and it is just as invisible on a button.
        let spelled_wrong = BriefFork {
            choices: vec![
                BriefChoice {
                    label: "SD card".to_string(),
                    requires: vec!["storage.sdcard".to_string()],
                    ..BriefChoice::default()
                },
                BriefChoice {
                    label: "WiFi".to_string(),
                    requires: vec!["radio.wifi".to_string()],
                    ..BriefChoice::default()
                },
            ],
            ..identical.clone()
        };
        let problems = validate_brief(&DesignBrief {
            forks: vec![spelled_wrong],
            ..DesignBrief::default()
        })
        .expect_err("a misspelled path inside a choice");
        assert!(
            problems.iter().any(|p| p.contains("storage.sdcard")),
            "{problems:?}"
        );

        // And the real one is fine: two answers that bring two different capabilities.
        let real = validate_brief(&DesignBrief {
            forks: vec![BriefFork {
                choices: vec![
                    BriefChoice {
                        label: "SD card".to_string(),
                        requires: vec!["storage.sd".to_string()],
                        suggests: vec!["logger".to_string()],
                    },
                    BriefChoice {
                        label: "WiFi upload".to_string(),
                        requires: vec!["radio.wifi".to_string()],
                        suggests: vec!["uploader".to_string()],
                    },
                ],
                ..identical
            }],
            ..DesignBrief::default()
        });
        assert!(real.is_ok(), "{real:?}");
    }

    #[test]
    fn the_brief_request_asks_for_a_unit_for_each_thing_it_acts_on() {
        // The failure this fixes: a PM2.5 meter with a touch screen was read as units for the sensing
        // side only (`sampler`, `air_quality`), so the actor step opened with nothing that manages the
        // display or the touch — half the application, undesigned until the person noticed by hand. The
        // brief is told to suggest one unit for each thing the application senses *and* each thing it
        // acts on, so the decomposition arrives complete enough to only correct.
        let request = brief_request("a PM2.5 meter with a touch screen");
        for phrase in [
            "each thing it acts on",
            "(`view`)",
            "(`touch`)",
            "undesigned",
        ] {
            assert!(
                request.contains(phrase),
                "the brief request asks for a unit per thing it acts on, missing {phrase:?}:\n{request}"
            );
        }
    }

    #[test]
    fn a_brief_that_names_a_capability_nobody_knows_is_repaired() {
        // A misspelled capability is the failure the brief phase can catch: an unknown path is a typo,
        // not a new capability, so it is fixed in a repair round rather than reaching the person.
        let broken = PM25_BRIEF.replace("sensing.particulate", "sensing.particle");
        let llm = FakeLlm::new(vec![Ok(broken), Ok(PM25_BRIEF.to_string())]);
        let brief = brief_with(&llm).expect("the repaired answer is the brief");

        assert_eq!(brief.framework, Some(ApplicationFramework::Actors));
        let prompts = llm.prompts();
        assert_eq!(prompts.len(), 2, "the answer was repaired once");
        assert!(
            prompts[1].contains("sensing.particle"),
            "the repair turn names the bad path:\n{}",
            prompts[1]
        );
    }

    #[test]
    fn a_brief_with_no_framework_is_still_a_brief() {
        // A description too thin to place on either side of the rule is an answer, not a blank row: the
        // person chooses the framework at the board step anyway, so an absent one is fine here — and so is
        // a brief with no readings at all, because every point it did not read is a row the person can see
        // is empty rather than a count they cannot account for.
        let thin = r#"{ "summary": "something that reads a sensor", "requires": ["sensing.temperature"] }"#;
        let llm = FakeLlm::new(vec![Ok(thin.to_string())]);
        let brief = brief_with(&llm).expect("a framework is optional in a brief");
        assert_eq!(brief.framework, None);
        assert_eq!(brief.requires, vec!["sensing.temperature".to_string()]);
        assert!(brief.points.is_empty() && brief.forks.is_empty());
    }

    #[test]
    fn the_brief_phase_gives_up_naming_what_was_wrong() {
        let broken = PM25_BRIEF.replace("sensing.particulate", "sensing.particle");
        let llm = FakeLlm::new(vec![Ok(broken)]);
        let err = brief_with(&llm).expect_err("no valid brief, so a refusal");
        assert!(
            err.contains("no valid brief in 3 attempts"),
            "the attempts are bounded: {err}"
        );
        assert!(
            err.contains("sensing.particle"),
            "and the refusal names the bad path: {err}"
        );
        assert_eq!(llm.prompts().len(), MAX_DESIGN_ATTEMPTS);
    }

    #[test]
    fn an_unreachable_llm_is_reported_for_the_brief_too() {
        let llm = FakeLlm::new(vec![Err("no API key configured".to_string())]);
        let err = brief_with(&llm).expect_err("no answer, so a refusal");
        assert!(err.contains("was not answered"), "{err}");
        assert!(err.contains("no API key configured"), "{err}");
        assert_eq!(llm.prompts().len(), 1, "an unreachable LLM is not retried");
    }

    // --- the composition file: `composition.spire`, the source a person opens ---------------------

    /// The canonical **composition file** — the YAML a person opens. Read from disk rather than held as
    /// a const, because the point of the file is that it *is* a file: the test that opens it is the test
    /// that proves spire-code can.
    fn composition_file() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/example-composition.spire");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is not readable: {e}", path.display()))
    }

    #[test]
    fn the_composition_file_opens_into_the_spec() {
        // The project file a person edits is a complete composition: it parses, it validates, and its
        // units are the ones a reviewer reads.
        let spec = parse_composition(&composition_file())
            .unwrap_or_else(|e| panic!("the canonical composition should parse: {e}"));
        assert_eq!(spec.framework, ApplicationFramework::Actors);
        assert_eq!(spec.board.chip, "esp32s3");
        assert_eq!(spec.board.bsp, "m5stack_core_s3");
        validate(&spec).expect("the canonical composition validates");

        let by_id = |id: &str| spec.units.iter().find(|u| u.id == id).expect(id);
        assert_eq!(by_id("sps30").role, Some(ComponentRole::Driver));
        assert_eq!(by_id("air_quality").message.as_deref(), Some("Reading"));
        assert_eq!(
            by_id("air_quality").uses,
            vec!["moving_average".to_string()]
        );
        // A quoted address is the string the driver wants, not the integer YAML would read.
        assert_eq!(spec.board_facts[0].address, "0x69");
        // And the **resources** layer is in the file a person opens, not only in the prompt: the example
        // is what teaches the format, so a layer missing from it is a layer nobody writes.
        let display = spec
            .resource("display")
            .expect("the board's own display is declared");
        assert_eq!(display.roles, vec!["media.display".to_string()]);
        assert_eq!(display.via, Some(ResourceVia::Board));
        let sps30 = spec
            .resource("sps30")
            .expect("the added sensor is declared");
        assert_eq!(sps30.via, Some(ResourceVia::Library));
        assert_eq!(sps30.address, "0x69", "the same literal as its board fact");
        assert_eq!(spec.resources.len(), 5, "the example shows the layer whole");
        // The facet annotations converge into the units, not into a block of their own.
        assert!(by_id("sampler").sleeps);
        assert_eq!(by_id("sampler").wake.as_deref(), Some("timer@1s"));
        // And the product-wide facts sit in the facet blocks.
        assert_eq!(spec.power.as_ref().unwrap().target_days, Some(30));
        assert_eq!(
            spec.storage.as_ref().unwrap().medium.as_deref(),
            Some("flash")
        );
        assert_eq!(
            spec.network.as_ref().unwrap().transport.as_deref(),
            Some("wifi")
        );
        assert_eq!(
            spec.security.as_ref().unwrap().requires,
            vec!["tls".to_string()]
        );

        // The **English** rides in the file a person opens, at both levels: the whole application's prose
        // and each actor's own sentence. No rule read it — the `validate` call above passed — which is the
        // point: the words are the person's, and the structure is what is checked.
        assert!(
            spec.description.contains("air-quality monitor"),
            "the whole-application prose is read: {:?}",
            spec.description
        );
        assert_eq!(
            by_id("view").description.as_deref(),
            Some("paints the reading and pages between the three screens on a gesture.")
        );
        // A unit that states none is honestly absent, not an empty string.
        assert!(by_id("sps30").description.is_none());

        // And the **messages** layer is in the file for the same reason the resources layer is: the example
        // is what teaches the format, so a layer missing from it is a layer nobody writes. The view's inbox
        // is a declared set, and the actor speaks the set's id rather than a bare struct.
        assert_eq!(
            spec.messages.len(),
            1,
            "the example declares one message-set"
        );
        assert_eq!(spec.messages[0].id, "screen_update");
        assert_eq!(
            spec.messages[0].kinds,
            vec!["Report".to_string(), "Gesture".to_string()]
        );
        assert_eq!(
            by_id("view").message.as_deref(),
            Some("screen_update"),
            "the view's inbox is the declared set, not a bare message"
        );
    }

    #[test]
    fn the_description_survives_a_render_and_reparse() {
        // The prose is part of the spec, so rendering the composition writes it and reading it back yields
        // it again — the same round trip the structural fields take. Without this, a person's words would
        // be silently dropped the moment a scaffold re-rendered the file from the parsed spec.
        use crate::build::idf_projects::render_composition;
        let spec = parse_composition(&composition_file()).expect("the composition parses");
        let rendered = render_composition(&spec).expect("the composition renders");
        let again = parse_composition(&rendered).expect("the rendered composition parses");
        assert_eq!(
            spec.description, again.description,
            "the whole-application prose survives the render"
        );
        assert_eq!(
            spec.units[0].description, again.units[0].description,
            "a unit's prose survives the render"
        );
        assert_eq!(spec, again, "a render and reparse is the identity");
    }

    #[test]
    fn the_description_is_not_read_by_validate() {
        // Prose is not a rule. A description that is nonsense — a bare colon, a URL, punctuation — is
        // accepted, because a rule over free text would be a rule over nothing. What is checked is the
        // structure; what the person *meant* is the person's to know.
        let mut spec = spec(PM25_METER);
        spec.description =
            "!!!: not: a: sentence, and not one this or any validator reads".to_string();
        spec.units[0].description = Some("...".to_string());
        validate(&spec).expect("prose is not what validate reads");
    }

    #[test]
    fn a_composition_without_a_description_still_parses() {
        // The field is additive and optional: the PM2.5 fixture predates it and must keep parsing, with
        // the absence read as an empty description rather than a failure.
        let spec = spec(PM25_METER);
        assert!(
            spec.description.is_empty(),
            "no description is an empty one"
        );
        assert!(
            spec.units.iter().all(|u| u.description.is_none()),
            "and no unit carries one either"
        );
    }

    #[test]
    fn the_composition_and_its_json_record_are_the_same_spec() {
        // The two forms are one composition: YAML is the source, JSON is the generated record the fill
        // reads back. This is the invariant that keeps them from drifting, not a coincidence.
        let from_text = parse_composition(&composition_file()).expect("the composition parses");
        let json = serde_json::to_string_pretty(&from_text).expect("the spec serializes");
        let from_json = parse_spec(&json).expect("the generated record parses");
        assert_eq!(from_text, from_json, "the two forms are one spec");
        // Serializing the record again changes nothing, which is what makes it *generated*.
        assert_eq!(json, serde_json::to_string_pretty(&from_json).unwrap());
    }

    #[test]
    fn a_spec_without_facets_serializes_exactly_as_it_did_before() {
        // The facet fields are additive: a composition that says nothing about them writes the same JSON
        // record it wrote before facets existed, so nothing downstream has to know they exist.
        let json = serde_json::to_string_pretty(&spec(PM25_METER)).unwrap();
        for absent in [
            "\"sleeps\"",
            "\"wake\"",
            "\"storage_role\"",
            "\"network_role\"",
            "\"power\": {",
            "\"storage\": {",
            "\"network\": {",
            "\"security\": {",
        ] {
            assert!(!json.contains(absent), "{absent} was written:\n{json}");
        }
    }

    #[test]
    fn a_json_answer_still_parses_as_a_composition() {
        // JSON is a subset of YAML, so the model's habit of answering in JSON keeps working — the fence
        // the JSON reader forgives is forgiven here too.
        let spec = parse_composition(&format!("```json\n{PM25_METER}\n```"))
            .expect("a fenced JSON answer is a composition");
        assert_eq!(spec.framework, ApplicationFramework::Actors);
        assert_eq!(spec.units.len(), 8);
    }

    #[test]
    fn a_scalar_that_looks_like_a_number_keeps_its_text_in_a_string_field() {
        // What YAML actually does here, pinned rather than assumed: a `String` field takes the scalar's
        // text, so `address: 0x69` and `address: "0x69"` are the same address. The canonical file quotes
        // it anyway — a quoted address means the same thing in every YAML parser, and it reads to a person
        // as text rather than as a number — but correctness does not depend on the quotes.
        for written in ["0x69", "\"0x69\""] {
            let text = format!(
                "framework: actors\nboard: {{chip: esp32s3}}\n\
                 units:\n  - {{id: sps30, kind: component, role: driver, source: stub, bus: i2c}}\n\
                 board_facts:\n  - {{bus: i2c, device: sps30, address: {written}}}\nwiring: []\n"
            );
            let spec = parse_composition(&text)
                .unwrap_or_else(|e| panic!("`address: {written}` should parse: {e}"));
            assert_eq!(
                spec.board_facts[0].address, "0x69",
                "written as `{written}`"
            );
        }
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused_with_a_location() {
        // The strictness that matters: every field is typed, so a value that is not that type is refused
        // *and names where* — rather than being coerced, or quietly dropped, in the record of what was
        // agreed.
        //
        // `on`/`off`/`yes`/`no` are the ones worth pinning: YAML 1.1 read them as booleans and a person
        // will write them meaning exactly that, but here they are the *strings* they look like — so
        // `sleeps: on` is an error rather than a silent `true`.
        let sleeping = |value: &str| {
            format!(
                "framework: actors\nboard: {{chip: esp32s3}}\n\
                 units:\n  - {{id: u, kind: actor, message: Tick, sleeps: {value}}}\nwiring: []\n"
            )
        };
        let budgeted = |value: &str| {
            format!(
                "framework: actors\nboard: {{chip: esp32s3}}\n\
                 units:\n  - {{id: u, kind: actor, message: Tick}}\npower: {{target_days: {value}}}\n\
                 wiring: []\n"
            )
        };
        for (text, field) in [
            (sleeping("on"), "sleeps"),
            (sleeping("yes"), "sleeps"),
            (sleeping("0x69"), "sleeps"),
            (budgeted("forever"), "target_days"),
            (budgeted("true"), "target_days"),
        ] {
            let err = parse_composition(&text)
                .expect_err("a value of the wrong type is refused rather than coerced");
            assert!(err.contains(field), "the refusal names the field: {err}");
            assert!(err.contains("line"), "and where it is: {err}");
        }
    }

    #[test]
    fn a_composition_that_is_not_syntactically_correct_is_refused_with_a_location() {
        // "Syntactically correct" is the requirement, so a file that does not parse is not a composition —
        // and the refusal names where, because that is what a person editing the file needs.
        let broken = "framework: actors\nboard: {chip: esp32s3\nunits: []\n";
        let err = parse_composition(broken).expect_err("a broken file is not a composition");
        assert!(
            err.contains("did not parse"),
            "the refusal names the parse: {err}"
        );
        assert!(err.contains("line"), "and where it is: {err}");
    }

    /// **The `resources` layer**: the devices an application declares, and the rule that a `uses`
    /// reaches one of them. The two halves of one idea — the set a person reviews is the set the
    /// application has — so every case here starts from the PM2.5 example with the block the design
    /// phase writes added to it: the board's own parts first, then the two drivers this application adds.
    mod resources {
        use super::*;

        /// The PM2.5 meter plus a `resources` block, as a composition writes one. Both driver
        /// resources share their component's id, which is the rule: one device, one word. Each declared
        /// resource is then owned by the one actor that holds it — the board's parts by the actors that
        /// state them, the two drivers by `sampler` — because that is what a `uses` *is*.
        fn declared() -> String {
            let with_resources = PM25_METER.replace(
                r#""board_facts": ["#,
                r#""resources": [
                     { "name": "display", "role": "media.display", "via": "board" },
                     { "name": "touch_panel", "role": "media.touch", "via": "board",
                       "bus": "i2c_internal", "address": "0x38" },
                     { "name": "battery", "role": "power.battery", "via": "board" },
                     { "name": "sps30", "role": "sensing.particulate", "via": "library",
                       "bus": "grove.a", "address": "0x69" },
                     { "name": "sht20", "role": "sensing.temperature", "via": "library",
                       "bus": "grove.a", "address": "0x40" }
                   ],
                   "board_facts": ["#,
            );
            assert_ne!(
                with_resources, PM25_METER,
                "the fixture has the block to add"
            );
            // Each declared resource has exactly one owner, so the actors that hold the board's parts
            // state it: the screen is the view's, the panel is touch's, the gauge is power's. (`sampler`
            // already owns `sps30` and `sht20`.) Ownership is what a `uses` is, not a separate field.
            let owned = with_resources
                .replace(
                    r#""state": "the LVGL screen""#,
                    r#""state": "the LVGL screen", "uses": ["display"]"#,
                )
                .replace(
                    r#""state": "menu and calibration""#,
                    r#""state": "menu and calibration", "uses": ["touch_panel"]"#,
                )
                .replace(
                    r#""state": "the battery gauge""#,
                    r#""state": "the battery gauge", "uses": ["battery"]"#,
                );
            assert_ne!(
                owned, with_resources,
                "the fixture has the actors to extend"
            );
            owned
        }

        /// The declared set with one fault written into it, at the point the fault belongs to — the same
        /// shape [`refusal`] uses, against the spec that has the block.
        fn rewritten(from: &str, to: &str) -> Vec<String> {
            refusal(&declared(), from, to)
        }

        #[test]
        fn the_declared_resources_are_the_devices_the_application_has() {
            let spec = spec(&declared());
            assert_eq!(spec.resources.len(), 5);
            validate(&spec).expect("a declared set validates");
            assert_eq!(
                spec.resource("sps30").expect("declared").roles,
                vec!["sensing.particulate".to_string()],
                "a driver's resource and its component carry one name, and the resource says what it is"
            );
        }

        /// A **multi-role** resource: one device (the SHT20), two capabilities. The spec carries both
        /// roles, the driver is one component found by name, and a `uses` of `sht20` reaches a device
        /// that answers two requirements at once — which is the whole of Phase 0.
        #[test]
        fn a_resource_may_name_several_roles() {
            let text = r#"
            {
              "framework": "actors",
              "board": { "chip": "esp32s3", "bsp": "m5stack_core_s3", "hal": "m5unified" },
              "resources": [
                { "name": "sht20", "roles": ["sensing.temperature", "sensing.humidity"],
                  "via": "library", "bus": "grove.a", "address": "0x40" }
              ],
              "units": [
                { "id": "sht20", "kind": "component", "role": "driver", "source": "stub",
                  "provides": "temperature and humidity", "bus": "i2c" },
                { "id": "sampler", "kind": "actor", "message": "Tick", "uses": ["sht20"],
                  "sends_to": ["view"] },
                { "id": "view", "kind": "actor", "message": "Report" }
              ],
              "wiring": ["sampler -> view"]
            }"#;
            let spec = parse_spec(text).expect("a multi-role resource parses");
            assert_eq!(
                spec.resource("sht20").expect("declared").roles,
                vec![
                    "sensing.temperature".to_string(),
                    "sensing.humidity".to_string()
                ],
                "one device, both capabilities"
            );
            validate(&spec).expect("a part that is two capabilities validates");

            // The scalar form is the same field, read as a one-element list — the back-compat sugar.
            let sugared = parse_spec(&text.replace(
                r#""roles": ["sensing.temperature", "sensing.humidity"]"#,
                r#""role": "sensing.temperature""#,
            ))
            .expect("the scalar `role` still parses");
            assert_eq!(
                sugared.resource("sht20").expect("declared").roles,
                vec!["sensing.temperature".to_string()],
                "`role: a` is sugar for `roles: [a]`"
            );

            // One bad name in the list is refused — per role, naming which one.
            let broken = parse_spec(&text.replace(r#""sensing.humidity""#, r#""sensing.humidty""#))
                .expect("the spec still parses; the typo is caught by validate, not the reader");
            let problems = validate(&broken).expect_err("a bad role in the list is refused");
            assert!(
                problems.iter().any(|p| p.contains("sensing.humidty")),
                "the typo is named: {problems:?}"
            );
        }

        #[test]
        fn a_unit_may_use_a_part_of_the_board_that_has_no_component() {
            // The rule the layer exists for: `display`, `touch_panel` and `battery` are the *board's* —
            // reached through the BSP, with no component of this project in between — and a unit names
            // them out of `resources`, exactly as it names a component. The fixture owns each one, so
            // this is also where ownership shows: the board part is what the actor's `uses` names.
            let spec = spec(&declared());
            validate(&spec).expect("a board part is reachable by name");
            let by_id = |id: &str| spec.units.iter().find(|u| u.id == id).expect(id);
            assert_eq!(by_id("view").uses, vec!["display".to_string()]);
            assert_eq!(by_id("power").uses, vec!["battery".to_string()]);
        }

        #[test]
        fn a_device_nobody_declared_is_refused() {
            // The refusal that makes "nothing invented" checkable: a unit cannot reach hardware this
            // application does not have, however plausible the part.
            let problems = rewritten(
                r#""uses": ["sps30", "sht20"]"#,
                r#""uses": ["sps30", "sht20", "bme680"]"#,
            );
            refused(&problems, "nor a resource it declares");
        }

        #[test]
        fn a_resource_role_the_vocabulary_does_not_know_is_refused() {
            // A capability path is the vocabulary's, so a name nobody named is a *misspelling* — the
            // same refusal a board gets, because the alternative is reading a typo as "no such device".
            let problems = rewritten(
                r#""role": "sensing.particulate", "via": "library""#,
                r#""role": "sensing.particles", "via": "library""#,
            );
            refused(&problems, "not one the vocabulary knows");
        }

        #[test]
        fn a_resource_that_does_not_say_how_it_is_reached_is_refused() {
            let problems = rewritten(
                r#"{ "name": "battery", "role": "power.battery", "via": "board" }"#,
                r#"{ "name": "battery", "role": "power.battery" }"#,
            );
            refused(&problems, "does not say how it is reached");
        }

        #[test]
        fn a_resource_declared_twice_is_refused() {
            // A name is what a `uses` resolves *by*, so two resources sharing one would make every
            // `uses` of it ambiguous.
            let problems = rewritten(
                r#"{ "name": "battery", "role": "power.battery", "via": "board" }"#,
                r#"{ "name": "display", "role": "power.battery", "via": "board" }"#,
            );
            refused(&problems, "is declared twice");
        }

        #[test]
        fn a_driver_resource_without_its_driver_component_is_refused() {
            // A part the application drives needs the component that drives it: the resource *is* its
            // driver's id, so a design that declares the device and forgets the driver has a hole in it.
            let problems = rewritten(
                r#""name": "sps30", "role": "sensing.particulate", "via": "library""#,
                r#""name": "sps30x", "role": "sensing.particulate", "via": "library""#,
            );
            refused(&problems, "no driver component of that name");
        }

        #[test]
        fn a_board_resource_shadowing_a_unit_is_refused() {
            // The word belongs to the board's part, and a `uses` resolves a unit *before* a resource — so
            // a unit of the same name would be reached instead of the part, silently.
            let problems = rewritten(
                r#"{ "name": "battery", "role": "power.battery", "via": "board" }"#,
                r#"{ "name": "moving_average", "role": "power.battery", "via": "board" }"#,
            );
            refused(&problems, "also has a unit of that name");
        }

        #[test]
        fn a_resources_address_has_to_agree_with_its_board_fact() {
            // The one place these resources and the older board facts overlap: one device has one
            // address, so stating it twice is fine and disagreeing is not.
            let problems = rewritten(
                r#""bus": "grove.a", "address": "0x69""#,
                r#""bus": "grove.a", "address": "0x6a""#,
            );
            refused(&problems, "one device has one address");
        }

        #[test]
        fn a_resource_used_by_two_units_is_refused() {
            // Exclusivity, the rule that makes ownership checkable: a device is held in one actor's
            // state, so a second `uses` of it is *sharing* — and the model's answer is that the second
            // actor messages the owner instead.
            let problems = rewritten(
                r#""uses": ["moving_average"], "sends_to": ["view"]"#,
                r#""uses": ["moving_average", "sht20"], "sends_to": ["view"]"#,
            );
            refused(&problems, "is used by");
        }

        #[test]
        fn a_declared_resource_nobody_uses_is_refused() {
            // The other half: a device was selected to be driven, so *some* unit has to own it. Dropping
            // it from the only actor that used it leaves it declared for nothing.
            let problems = rewritten(
                r#""uses": ["sps30", "sht20"], "sends_to": ["air_quality"]"#,
                r#""uses": ["sps30"], "sends_to": ["air_quality"]"#,
            );
            refused(&problems, "is declared but no unit uses it");
        }

        #[test]
        fn a_shared_pure_library_component_is_still_allowed() {
            // The distinction the rule turns on: a *device* is owned, an *algorithm* is used. Two actors
            // that both reach the same `library` component share code, not hardware, so this stays legal
            // — `moving_average` is not a resource, only a component.
            let shared = declared().replace(
                r#""uses": ["sps30", "sht20"], "sends_to": ["air_quality"]"#,
                r#""uses": ["sps30", "sht20", "moving_average"], "sends_to": ["air_quality"]"#,
            );
            assert_ne!(shared, declared(), "the fixture has the actor to extend");
            validate(&spec(&shared)).expect("a library component is an algorithm, not a device");
        }
    }

    /// The **assigned composition** the Swift actor-assignment step writes — an `actors` spec with the
    /// driver components the declared resources imply. It is a fixture because it is the *contract*:
    /// the step is only allowed to emit this shape, so a spec it would produce has to validate here.
    fn assigned_composition() -> &'static str {
        r#"
        {
          "framework": "actors",
          "board": { "chip": "esp32s3", "bsp": "m5stack_core_s3", "hal": "m5unified" },
          "resources": [
            { "name": "display", "role": "media.display", "via": "board", "bus": "display" },
            { "name": "sps30", "role": "sensing.particulate", "via": "library", "bus": "i2c", "address": "0x69" }
          ],
          "board_facts": [ { "bus": "i2c", "device": "sps30", "address": "0x69" } ],
          "units": [
            { "id": "sps30", "kind": "component", "role": "driver", "source": "stub", "provides": "sensing.particulate", "bus": "i2c" },
            { "id": "sampler", "kind": "actor", "message": "Tick", "state": "reads the sensor", "uses": ["sps30"], "sends_to": ["view"] },
            { "id": "view", "kind": "actor", "message": "Report", "uses": ["display"] }
          ],
          "wiring": ["sampler -> view"]
        }
        "#
    }

    /// The shape the Swift actor step writes — board, the resources and the drivers they imply, the
    /// actors that own them, and the edges "sends to" states — validates with no complaints. This is
    /// the seam the UI's live rules mirror: if the two ever disagree, this fixture is the one the core
    /// actually reads.
    #[test]
    fn the_actor_assignment_step_produces_a_valid_spec() {
        let spec = spec(assigned_composition());
        assert_eq!(spec.framework, ApplicationFramework::Actors);
        validate(&spec).expect("the assigned composition validates");

        let by_id = |id: &str| spec.units.iter().find(|u| u.id == id).expect(id);
        // The `library` resource *is* its driver's id — the rule the step builds the driver around.
        assert_eq!(by_id("sps30").kind, UnitKind::Component);
        assert_eq!(by_id("sps30").role, Some(ComponentRole::Driver));
        assert_eq!(by_id("sps30").source, Some(UnitSource::Stub));
        assert_eq!(by_id("sps30").bus.as_deref(), Some("i2c"));
        // The board's own part has no component of this project: the BSP reaches it.
        assert!(spec.units.iter().all(|u| u.id != "display"));
        // Ownership: each resource is `uses`-ed by exactly the one actor the step assigned it to.
        assert_eq!(by_id("sampler").uses, vec!["sps30".to_string()]);
        assert_eq!(by_id("view").uses, vec!["display".to_string()]);
        // "sends to" is the source of truth, and the wiring is its rendering.
        assert_eq!(spec.wiring, vec!["sampler -> view".to_string()]);
        assert_eq!(spec.board_facts[0].device, "sps30");
    }
}
