# The stages of a composition

<!--
  A design note, and deliberately a **proposal**: of what is below, the contracts block itself has since
  been implemented — a `messages:` entry may declare a message **type** and its `fields`, and the parser,
  `validate` and the scaffold are real — while the promotion of the brief's `decided` shape into that
  block is still to come. Where this note and `docs/composition-format.md` disagree, that file is the
  reference.

  It lays out the stages a composition already passes through — the brief
  (`application_spec::brief_request`), the decomposition (`docs/composition-format.md`) and the fill —
  the contract-shaped gap between the first and the last, and one way to close it with a stage in
  between. The three decisions taken are collected at the end.

  Every claim about the stages as they are today resolves under `crates/spire-code/`, so the note can be
  checked against the tree and corrected in the same commit as the code it describes.
-->

## The stages today

A composition already moves through three stages, each one narrowing the last:

| stage | artifact | what it holds | held to |
| --- | --- | --- | --- |
| 0 · the **brief** | `DesignBrief` (JSON) | the `description` read against six fixed points — `purpose`, `senses`, `acts_on`, `reacts_to`, `timing`, `missing` — each with a `said` (what the description says) and a `decided` (the reading where it is open) | structure only; the prose is not checked |
| 1 · the **decomposition** | `composition.spire` (YAML) | names: units, mailboxes, edges, the kinds of a set, the fields of a declared type, resources, facets — plus prose `description`s carried verbatim | fully, by `validate` |
| 2 · the **fill** | generated `struct`s and `on_message` | the actual shape and behaviour, written by hand over stubs | by the C++ compile |

The rule that has kept the two structural stages clean is worth stating once, because the proposal below
only continues it: **prose is carried, never validated; structure is validated, never paraphrased.**
`description` travels verbatim from the file into a generated doc comment, and `validate` refuses to read
it; every structural field is parsed strictly and refused on the wrong type, with line and column.

## The hole between stages one and two

The brief already *decides the contract*. Its prompt tells the model to decide, in prose, the things a
description leaves open — and names the message shape among them:

> Your own proposals belong here too — the look of a screen, **the shape of a message**, the numbers a
> display shows — so the person reviews a concrete decision rather than being asked for one.

So the shape of a message is decided at stage 0, as prose. The decomposition (stage 1) keeps only the
*name* — `message: Reading` — and the shape evaporates. The fill (stage 2) then re-decides the shape by
hand, in C++, inside the stub the scaffold wrote:

```cpp
/// TODO: what a `Reading` is — the fields that cross the edge, as plain values.
/// Named by air_quality.
struct Reading {};
```

The same decision is made twice — once in prose that disappears, once in code — and the hand-off between
them keeps nothing but the noun. That is the gap this note proposes to close.

## The proposal: four stages

Insert one stage between the decomposition and the fill, and nothing else changes:

- **0 · the brief** — unchanged. Six points, `decided` prose. Its job is to *surface* decisions in English
  for a person to approve, not to hold structure.
- **1 · the decomposition** — unchanged. The clean, deterministic YAML: *what exists and what talks to
  what*. Names, edges, kinds-as-names, resources, facets.
- **2 · the contracts** — new. The **shape** of every message, declared structurally in the same file,
  growing out of the `messages:` block that is there now. This is where the brief's `decided` shape lands.
- **3 · the fill** — unchanged, and narrower for it. `on_message` bodies, stage transforms, host-test
  cases. The *what* stays prose in the composition; the *how* stays code.

The invariant across all four: a later stage **refines** a name the earlier stage introduced, and never
re-decides it. That is checkable rather than aspirational — `validate` already enforces the one-namespace
rule over every declared name, and it would simply extend to the new field names.

## The contracts stage

`messages:` used to be a set-only block. It now holds *every message, bare or set* — the same move `units`
makes by holding `component`, `actor` and `stage` in one list:

```yaml
messages:
  - id: screen_update              # a set — several kinds, one mailbox
    kinds: [Report, Gesture]
    description: what the view answers — a reading to paint, or a gesture to turn the page.

  - id: Reading                    # a bare message, its contract now declared
    description: one particulate sample, as a plain value.
    fields:
      - { name: pm_2_5, type: uint16, description: PM2.5 as a raw count, µg/m³ }
      - { name: pm_10,  type: uint16, description: PM10 as a raw count, µg/m³ }

  - id: Gesture
    description: a tap to page, a long press to re-baseline.
    fields:
      - { name: kind, type: enum(tap, long_press), description: which gesture arrived }
```

A declaration is one of two things, and the block holds both: a **type** (`id`, `fields`, `description`)
and a **set** (`id`, `kinds`, `description`). The set *names* its kinds, so `Gesture` above is a kind of
`screen_update` and a type in its own right — declared once, read in two places, which is exactly the
sharing a set exists for. A kind named but not declared (`Report` here) stays a stub, just as an
undeclared bare message does.

The scaffold then writes `struct Reading { … }` with **real fields and their `///` comments**, and the
`TODO` in the header collapses to the two things the scaffold genuinely cannot know: the types the design
did not name, and the bodies the person must write. The brief's prose decision finally has a structural
home, so the fill is not re-deciding it.

## Why this stays deterministic

The discipline that keeps `composition.spire` clean carries straight through, because the new stage is a
strict schema, not pseudo-code:

- **Structure is a schema at every stage.** `fields` is a list of `{ name, type?, description? }`; `name`
  is validated as an identifier, unique within its type; the type's own name stays unique across the
  design. There is no free pseudo-code anywhere.
- **Prose is the only free region, and it is carried, not interpreted.** The brief's `decided` → the
  message's `description` → the field's `///`, verbatim, exactly as `description` travels today.
- **The record ties the stages.** `SPIRE.application.json` is already the generated record the fill reads
  back; the new fields serialize into it, so brief → composition → code share one name and one lineage,
  and the existing rule — a `composition.spire` that is there and does not parse is an error, never a
  fall-through — catches drift.

## The three decisions

Three decisions, now all taken:

1. **One file or two — decided: one.** The contract grows out of the `messages:` block in
   `composition.spire`, so a single diff shows the whole design and structure and prose cannot drift apart
   in two places.
2. **How much of a type system — decided: a hint, not a checker.** `type` is carried verbatim into the
   stub, unvalidated, because it is precisely the "fact about the device, the protocol or the screen" the
   fill owns. Only `name` (an identifier, unique within its type) and the message's own `id` are checked.
3. **How a set's kinds carry fields — decided: a kind is a declared type.** The `messages:` block holds
   **types** (`{ id, fields, description }`) and **sets** (`{ id, kinds: [<type names>], description }`),
   the way `units` holds components, actors and stages in one list. A set *names* its kinds, so a kind that
   is also some actor's bare message is declared once and referenced twice — the sharing a set exists for.
   A kind named but not declared stays a stub, exactly as an undeclared bare message does.

Two halves of this note stand differently now. The contracts block **has landed**: a `messages:` entry may
declare a type and its `fields`, `validate` holds the `id` and each field's `name`, and the scaffold
writes the members and their `///` comments into the stub — so the shape a design decided survives into
the code instead of being re-decided in the fill. What is still a proposal is the other half: `DesignBrief`
remains prose (`said`/`decided`), and nothing yet promotes a `decided` shape into the block, so a brief's
contract is still retyped by hand into `messages:`. The gap this note opened with is closing from the code
side rather than from the brief's.
