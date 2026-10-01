// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **application spec** — what an application *is*, decided before any of it is written.
//!
//! An application is not assembled out of parts whose shape somebody already knows. It is a
//! **composition**: a few units of work — drivers and pure algorithms, which are framework-agnostic —
//! arranged into loops, messages and wiring, which is the application's own. Deciding that
//! arrangement is the **design phase**, and this module is its artifact: a typed spec a model can
//! write and a tool can *check*, because an arrangement nobody checks is an arrangement nobody can
//! review.
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
//! **pumps** it through stages. [`design_request`] says exactly this to the model, and the model
//! returns its choice with a one-line justification for a person to confirm or override.
//!
//! # What validation can and cannot promise
//!
//! [`validate`] checks six things, each one a failure that would otherwise reach the compiler or a
//! board: no mixed frameworks, a message for every actor, every edge resolving, components that are
//! framework-agnostic, board facts that belong to a driver, and nothing invented-and-unused.
//!
//! What it deliberately does **not** promise is the type-level half — that a message is trivially
//! copyable and default-constructible. A spec names a message; whether that name is a type the
//! mailbox can hold is not a spec's business, and the gate for it is the compile: `spire::Actor`'s
//! `static_assert`s and the `actors` component's host test, which runs on this machine.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

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
    /// Already in the library the application is built against — the design phase names it.
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

/// The application, decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationSpec {
    pub framework: ApplicationFramework,
    /// One line, for the person confirming the choice.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub justification: String,
    pub board: BoardChoice,
    #[serde(default)]
    pub board_facts: Vec<BoardFact>,
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

/// The six rules — every one of them a failure that would otherwise reach the compiler or a board.
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

    // 2. Every actor states its message; every stage states a port.
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

    // 3. Every edge resolves; wiring connects *composition*, and `uses` reaches a component.
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
                None => problems.push(format!(
                    "'{}' uses '{used}', which is not a unit in this spec",
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

/// The library a design request names, rendered into the prompt.
///
/// The **no-library** case is stated rather than left out, and it is not the same as an empty one: with
/// no library named there is nothing for a component to already be in, so every component the design
/// names has to be written. Silence here would leave that to be inferred, and a model that infers
/// "the library probably has this" writes an application that cannot build.
fn library_block(library: Option<&LibraryFacts>) -> String {
    let Some(library) = library else {
        return "## The library this application is built against\n\n\
                None named. Nothing exists for a component to already be in, so **every component you \
                design has to be written**: mark them all `\"source\": \"stub\"`.\n"
            .to_string();
    };

    let mut text = format!(
        "## The library this application is built against\n\n`{}`, and the components it already \
         has:\n",
        library.root
    );
    if library.components.is_empty() {
        text.push_str("- none of its own\n");
    }
    for component in &library.components {
        let what = match component.kind {
            Some(kind) => kind.as_str(),
            None => "no kind stated",
        };
        text.push_str(&format!("- `{}` — {what}\n", component.name));
    }
    text.push_str(
        "\nThose are `\"source\": \"existing\"` — they are here. Anything else you design has to be \
         written, so mark it `\"source\": \"stub\"`, and do not design a component this library \
         already has.\n",
    );
    if let Some(hints) = &library.hints {
        text.push_str(&format!(
            "\nLIBRARY HINTS — how the author of this library says it is meant to be used. It is the \
             architecture, it is not in the code, and the design has to respect it:\n\n{hints}\n"
        ));
    }
    text
}

/// The **design request**: what the model is asked, with the board and the description in it.
///
/// `description` is the user's answers to the design form — the six questions below, in their own
/// words. `framework` is `Some` when the user pinned the choice up front; otherwise the model chooses
/// it and has to justify it, which is the *derived-and-confirmed* shape: the choice is made where it
/// can be informed (after reading the description) and confirmed where it can be overridden (by the
/// person reading the spec).
///
/// The six questions are in the prompt on purpose. A model handed a thin description should be able to
/// see *which* question went unanswered and say so, rather than filling the gap — the same rule the
/// component prompts work under: nothing is invented, and a gap left open is reported.
pub fn design_request(
    board: &BoardChoice,
    description: &str,
    framework: Option<ApplicationFramework>,
    // The **component library** the application is built against, when the caller named one. It is what
    // makes `"source": "existing"` a fact rather than a guess — see [`LibraryFacts`].
    library: Option<&LibraryFacts>,
) -> String {
    let framework_rule = match framework {
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
    };

    format!(
        r#"You are designing the **composition** of an ESP-IDF application, before any of it is
written. The output is a JSON **application spec** that a person will review and a tool will check.

## The board

{board}

(Onboard devices — a display, a touch panel, a camera, a battery gauge, an IMU — are the *board's*, reached
through the BSP or the board abstraction above. They are not units and they are not board facts.)

The board above is **given**: echo it in `board` exactly. Substituting a different BSP or abstraction is
not a design choice — it is an application built for a board nobody has.

{library}

## What the application has to do

{description}

If this leaves one of these unanswered, **say so and leave the field empty** — do not invent a
device, an address, a pin or a protocol:

1. what it does, in a paragraph;
2. what it **senses** (sensors, buttons, network, time — and roughly how fast);
3. what it **acts on** (display, audio, motor, actuator, network);
4. what it **reacts to over time** (periodic? on-command? on-threshold? on-event?);
5. **timing** — what is parallel, what is a human-rate UI, what is a machine-rate stream;
6. what happens **when something is missing** (a sensor absent, WiFi down, battery low).

## The framework

{framework_rule}

## The spec

```json
{{
  "framework": "actors" | "ramen",
  "justification": "one line for the person confirming this",
  "board": {{ "chip": "...", "bsp": "...", "hal": "..." }},
  "board_facts": [ {{ "bus": "i2c", "device": "<a driver unit id>", "address": "0x69" }} ],
  "units": [
    {{ "id": "sps30", "kind": "component", "role": "driver", "source": "stub",
       "provides": "PM1/2.5/4/10 readings", "bus": "i2c" }},
    {{ "id": "moving_average", "kind": "component", "role": "library", "source": "existing",
       "provides": "rolling window" }},
    {{ "id": "esp_dl", "kind": "component", "role": "library", "source": "published",
       "registry": "espressif/esp-dl", "provides": "a pre-trained detector the registry already has" }},
    {{ "id": "sampler", "kind": "actor", "message": "Tick", "state": "reads both sensors",
       "uses": ["sps30"], "sends_to": ["air_quality"] }},
    {{ "id": "preprocess", "kind": "stage", "pulls": "frames", "pushes": "tensors" }}
  ],
  "wiring": ["sampler -> air_quality", "air_quality -> view"]
}}
```

A unit is exactly one of three kinds:

* `component` — a **driver** (one device on one bus) or a **library** (pure code). It knows no pins, no
  address, no task, no mailbox and no framework: that is what makes it reusable in an application that
  chose the other framework, and it is why a component carries **no** `message`, `state`, `pulls`,
  `pushes`, `sends_to` or `uses`. Components are **never in the wiring** — a unit *uses* one.
* `actor` — a classical actor: a `Message` type it receives, the `state` it holds between messages, the
  components it `uses`, and the units it `sends_to`. The framework's scheduler owns its mailbox and its
  task; you do not write a loop.
* `stage` — a ramen dataflow stage: what it `pulls` and what it `pushes` (a **source** stage has only
  `pushes` — a frame pump reading a camera; a **sink** has only `pulls` — the stage that acts on the
  flow's last value: storing an image, driving a pin). One task runs a whole chain, pushing
  synchronously — so the chain is a **DAG**, never a cycle.

`uses` is how the composition reaches the framework-agnostic parts: `sampler` sampling `sps30`, a
`detector` stage using `esp_dl`. It is also what tells a reviewer which component a unit wraps.

**An edge carries the receiver's message, not the sender's.** In an `actors` application every edge
`a -> b` means `a` posts a message of **`b`'s** type — `a` holds `ActorRef<b::Message>`, and that is the
only thing `b` can receive. So an actor sending *into* a unit is posting that unit's own message type
however differently the two units are named; two units pointing at one actor post the same type, and a
unit whose own message differs (a `TouchEvent` posted at an actor that takes `Reading`) does not compile.

**A cycle is legal for `actors`, and it is why the framework has a registry.** Two actors that must
reach each other — a menu and the screen it drives, a sampler and the view it reports to — state **both**
edges (`a -> b` and `b -> a`), because each is the other's peer. The framework resolves such a pair by
**name** when the application starts rather than by handing one the other's ref at construction, and the
actor's `id` is that name. A `ramen` chain, pushed synchronously, has no cycles at all — a cycle there
is unbounded recursion on one stack, not a loop.

## The rules the tool checks

1. one framework per application, and **one unit per id**: an `actors` application has `component` and
   `actor` units, a `ramen` application has `component` and `stage` units. No mixing, and no two units
   sharing an id.
2. every `actor` states a `message`; every `stage` states at least one of `pulls`/`pushes`.
3. every reference resolves — both ends of a `wiring` edge, every `sends_to` and every `uses` name a
   unit that exists — the wiring connects only **actors and stages** (never a component), `uses`
   names only **components**, and a ramen dataflow has no cycles.
4. every component has a `role`; a `driver` states a `bus` (a `published` one need not — it is not
   written here) and a `library` states none; a `published` component states its `registry` name and
   no other source does; no component carries composition.
5. every board fact's `device` is a **driver component in this spec**: a fact is how a driver is
   reached. An onboard device is not a fact — it comes from the board.
6. nothing invented and unused: every unit is referenced by the wiring, by a `sends_to`, by a `uses`,
   or by a board fact.

A component's `"source"` says where its code comes from:

* `"existing"` — the library already has it (the list above);
* `"stub"` — it has to be written, from a datasheet or a description; the design generates the stub,
  so name it rather than avoiding it;
* `"published"` — it already exists on the ESP Component Registry (`components.espressif.com`). **Look
  it up** with the `registry/*` tools rather than designing a second copy of a driver somebody has
  already published, and put the registry's own `namespace/name` in the unit's `"registry"` field. A
  published component is a **managed dependency** of this application, not a component of the
  library: the scaffold writes it into `main/idf_component.yml`, so it needs no `bus` and is never
  stubbed.

Respond with **only the JSON object**, and nothing else."#,
        board = board_block(board),
        library = library_block(library),
        description = description.trim(),
        framework_rule = framework_rule,
    )
}

/// The board, as a prompt block.
fn board_block(board: &BoardChoice) -> String {
    let mut lines = vec![
        format!("* chip: `{}`", board.chip),
        format!(
            "* BSP: `{}` (carries the pins and the dependency set)",
            board.bsp
        ),
    ];
    if !board.hal.trim().is_empty() {
        lines.push(format!(
            "* board abstraction: `{}` (the API the application is written against)",
            board.hal
        ));
    }
    lines.join("\n")
}

/// Total LLM rounds — the first answer plus its repairs — before the design phase gives up.
///
/// Three, like the AppSpec requirements pass: enough for a model to fix a real mistake it made (a
/// forgotten field, a wiring edge that names nothing, a component it gave a mailbox), and few enough
/// that a model which *cannot* produce a decomposition is reported rather than asked forever.
pub const MAX_DESIGN_ATTEMPTS: usize = 3;

/// The spec with the **caller's board**, not the model's.
///
/// The board is a *given*: the wizard collected it — a chip and a BSP a build can be made for — and the
/// design request states it. A model that answers with a different one has the application depend on a
/// board nobody chose, and one did: `espressif/m5stack_core_s3` came back as `m5stack/cores3`, which the
/// scaffold then wrote into `main/idf_component.yml` and the build could not resolve. So the answer's
/// board is **overwritten rather than trusted** — the six rules are about the *composition*, which is
/// the part the model is actually being asked to decide, and the board is the part it is not.
fn with_the_callers_board(mut spec: ApplicationSpec, board: &BoardChoice) -> ApplicationSpec {
    spec.board = board.clone();
    spec
}

/// The **design phase's LLM stage**: ask, parse, check against the six rules, and repair.
///
/// The LLM is injected as a plain async `call`, exactly as the AppSpec requirements pass injects it,
/// so the tests run on canned answers — no live actor, no network, and the repair path is exercised
/// rather than assumed.
///
/// A refusal is the whole answer rather than a draft plus problems, and that is a decision about what
/// a person reviews: a decomposition that does not hold together is not something to approve, so the
/// reviewer is shown a spec that *passed*. When every attempt fails, the error names what was wrong
/// with the last answer, so the caller can say which rule the model could not satisfy rather than
/// "invalid spec".
pub async fn design_application<F, Fut>(
    board: &BoardChoice,
    description: &str,
    framework: Option<ApplicationFramework>,
    library: Option<&LibraryFacts>,
    call: F,
) -> Result<ApplicationSpec, String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let request = design_request(board, description, framework, library);
    let mut prompt = request.clone();
    let mut last_problems: Vec<String> = Vec::new();

    for attempt in 1..=MAX_DESIGN_ATTEMPTS {
        let answer = call(prompt.clone()).await.map_err(|e| {
            format!("the design request was not answered (attempt {attempt} of {MAX_DESIGN_ATTEMPTS}): {e}")
        })?;
        let problems = match parse_spec(&answer) {
            // A parse failure is a problem like any other, and stating it that way is what lets the
            // repair turn exist for it: a model that emitted YAML or prose gets told so and answers in
            // JSON, instead of the whole design phase failing on a formatting habit.
            Err(parse_problem) => vec![parse_problem],
            Ok(spec) => match validate(&spec) {
                // The **caller's** board, not the model's: see [`with_the_callers_board`].
                Ok(()) => return Ok(with_the_callers_board(spec, board)),
                Err(problems) => problems,
            },
        };
        prompt = repair_request(&request, &answer, &problems);
        last_problems = problems;
    }

    Err(format!(
        "the design phase produced no valid spec in {MAX_DESIGN_ATTEMPTS} attempts. The last \
         answer's problems were:\n  - {}",
        last_problems.join("\n  - ")
    ))
}

/// The repair turn: the same request, the answer that came back, and what is wrong with it.
///
/// Deliberately a whole new message rather than a conversation: the design phase keeps no chat state,
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
}

#[cfg(test)]
mod tests {
    use super::examples::{INSECT_TRAP, PM25_METER};
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
    fn the_design_request_carries_the_form_the_rule_and_the_schema() {
        let board = BoardChoice {
            chip: "esp32s3".into(),
            bsp: "m5stack_core_s3".into(),
            hal: "m5unified".into(),
        };
        let request = design_request(&board, "A PM2.5 meter with a touch screen.", None, None);
        for expected in [
            "esp32s3", // the board is in it
            "m5stack_core_s3",
            "m5unified",
            "A PM2.5 meter with a touch screen.", // the user's words are in it
            "human timescale",                    // the framework rule
            "machine timescale",
            "Choose the framework", // not pinned, so the model must choose and justify
            "what it **senses**",   // the form, so a thin answer is visible as thin
            "board_facts",          // the schema
            "no cycles",            // the rules it is checked against
            // No library named, which is stated rather than left to be inferred: nothing exists for a
            // component to already be in, so nothing can be `existing`.
            "None named",
            "every component you design has to be written",
        ] {
            assert!(
                request.contains(expected),
                "the request is missing {expected:?}"
            );
        }

        // Pinned: the model is told the choice is not its to reconsider.
        let pinned = design_request(&board, "...", Some(ApplicationFramework::Ramen), None);
        assert!(pinned.contains("The framework is **ramen**"), "{pinned}");
        assert!(!pinned.contains("Choose the framework"));

        // A library names what it has, which is what makes `"source": "existing"` a fact rather than a
        // guess — and carries its hints, the architecture the design has to respect.
        let facts = LibraryFacts {
            root: "/work/sensors".to_string(),
            components: vec![
                LibraryComponent {
                    name: "sps30".to_string(),
                    kind: Some(crate::build::idf_projects::ComponentKind::Driver),
                },
                LibraryComponent {
                    name: "moving_average".to_string(),
                    kind: Some(crate::build::idf_projects::ComponentKind::Library),
                },
                LibraryComponent {
                    name: "written_by_hand".to_string(),
                    kind: None,
                },
            ],
            hints: Some("Call `sensors::begin()` once.".to_string()),
        };
        let with_library = design_request(
            &board,
            "A PM2.5 meter with a touch screen.",
            None,
            Some(&facts),
        );
        for expected in [
            "/work/sensors",
            "- `sps30` — driver",
            "- `moving_average` — library",
            "- `written_by_hand` — no kind stated", // said as unknown, not assumed
            "Those are `\"source\": \"existing\"`",
            "Call `sensors::begin()` once.", // the library's architecture
        ] {
            assert!(
                with_library.contains(expected),
                "the request is missing {expected:?}:\n{with_library}"
            );
        }
        assert!(!with_library.contains("None named"));
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
    /// Not a mock of the transport — it is the closed `call` the design phase is written against,
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

    /// The board every design test uses, and one run of the design phase against a fake LLM.
    fn design_with(llm: &FakeLlm) -> Result<ApplicationSpec, String> {
        let board = BoardChoice {
            chip: "esp32s3".into(),
            bsp: "m5stack_core_s3".into(),
            hal: "m5unified".into(),
        };
        let llm = llm.clone();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(design_application(
                &board,
                "a PM2.5 meter with a touch screen",
                None,
                None,
                move |prompt| {
                    let llm = llm.clone();
                    async move { llm.call(prompt).await }
                },
            ))
    }

    #[test]
    fn a_valid_answer_is_a_spec_and_needs_no_repair() {
        let llm = FakeLlm::new(vec![Ok(PM25_METER.to_string())]);
        let spec = design_with(&llm).expect("a valid answer is the spec");
        assert_eq!(spec.framework, ApplicationFramework::Actors);
        assert_eq!(llm.prompts().len(), 1, "one round trip, no repair");
        assert!(
            llm.prompts()[0].contains("esp32s3"),
            "the first prompt is the design request, board and all"
        );
    }

    #[test]
    fn the_design_request_names_the_three_sources_and_the_registry_field() {
        // The prompt is where the third source is *learned*: a model that is never told about
        // `published` designs a second copy of a driver somebody already published.
        let request = design_request(
            &BoardChoice {
                chip: "esp32p4".into(),
                bsp: "waveshare/esp32_p4_nano".into(),
                hal: "bsp".into(),
            },
            "an AI insect trap that detects and traps insects",
            Some(ApplicationFramework::Ramen),
            None,
        );
        for expected in [
            r#""source": "published""#,
            r#""registry""#,
            "components.espressif.com",
            "managed dependency",
            "`registry/*`",
            "main/idf_component.yml",
        ] {
            assert!(
                request.contains(expected),
                "the design request does not mention {expected:?}:\n{request}"
            );
        }
    }

    #[test]
    fn the_design_keeps_the_callers_board_and_not_the_models() {
        // A live run replaced `espressif/m5stack_core_s3` with `m5stack/cores3` — which the scaffold then
        // wrote into `main/idf_component.yml`, where it did not resolve. The board is a *given*, so the
        // answer's is overwritten rather than trusted.
        let swapped = PM25_METER.replace(
            r#""bsp": "m5stack_core_s3", "hal": "m5unified""#,
            r#""bsp": "m5stack/cores3", "hal": "esp-idf""#,
        );
        assert_ne!(swapped, PM25_METER, "the fixture's board was rewritten");
        let llm = FakeLlm::new(vec![Ok(swapped)]);
        let spec = design_with(&llm).expect("the answer is the spec");

        assert_eq!(spec.board.chip, "esp32s3");
        assert_eq!(
            spec.board.bsp, "m5stack_core_s3",
            "the caller's BSP, never the model's"
        );
        assert_eq!(spec.board.hal, "m5unified");
        // The composition the model *did* decide is untouched: only the board is pinned.
        assert_eq!(spec.units.len(), 8, "{:?}", spec.units.len());
    }

    #[test]
    fn a_spec_that_breaks_a_rule_is_repaired_with_the_problem_named() {
        // One bad answer, then the good one: the second prompt has to say what was wrong, or the
        // model has nothing to fix.
        let broken = PM25_METER.replace(r#""message": "Reading", "#, "");
        let llm = FakeLlm::new(vec![Ok(broken), Ok(PM25_METER.to_string())]);
        let spec = design_with(&llm).expect("the repaired answer is the spec");

        assert_eq!(spec.framework, ApplicationFramework::Actors);
        let prompts = llm.prompts();
        assert_eq!(prompts.len(), 2, "the answer was repaired once");
        assert!(
            prompts[1].contains("is an actor with no message"),
            "the repair turn names the problem:\n{}",
            prompts[1]
        );
        assert!(
            prompts[1].contains("What you answered"),
            "and carries the answer it is asking to fix"
        );
        assert!(
            prompts[1].contains("esp32s3"),
            "the request is repeated in full, so a repair is a self-contained question"
        );
    }

    #[test]
    fn an_answer_that_is_not_json_is_repaired_too() {
        // A formatting habit is not a design failure: a model that answers in prose is told so and
        // asked again, rather than the whole design phase failing on a backtick.
        let llm = FakeLlm::new(vec![
            Ok("Sure! Here is the decomposition you asked for...".to_string()),
            Ok(PM25_METER.to_string()),
        ]);
        design_with(&llm).expect("the repaired answer is the spec");
        let prompts = llm.prompts();
        assert_eq!(prompts.len(), 2);
        assert!(
            prompts[1].contains("did not parse"),
            "the repair turn names the parse failure:\n{}",
            prompts[1]
        );
    }

    #[test]
    fn the_design_phase_gives_up_naming_what_was_wrong() {
        // The same bad answer every time: bounded attempts, and an error that says which rule the
        // model could not satisfy rather than "invalid spec".
        let broken = PM25_METER.replace(r#""message": "Reading", "#, "");
        let llm = FakeLlm::new(vec![Ok(broken)]);
        let err = design_with(&llm).expect_err("no valid spec, so a refusal");
        assert!(
            err.contains("no valid spec in 3 attempts"),
            "the attempts are bounded: {err}"
        );
        assert!(
            err.contains("is an actor with no message"),
            "and the refusal names the problem: {err}"
        );
        assert_eq!(llm.prompts().len(), MAX_DESIGN_ATTEMPTS);
    }

    #[test]
    fn an_llm_that_cannot_be_reached_is_reported_as_that() {
        // Not a bad spec — no answer at all. The distinction matters to the caller: one is worth
        // rephrasing, the other is worth fixing the wiring.
        let llm = FakeLlm::new(vec![Err("no API key configured".to_string())]);
        let err = design_with(&llm).expect_err("no answer, so a refusal");
        assert!(err.contains("was not answered"), "{err}");
        assert!(err.contains("no API key configured"), "{err}");
        assert_eq!(
            llm.prompts().len(),
            1,
            "an unreachable LLM is not retried as a repair"
        );
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
}
