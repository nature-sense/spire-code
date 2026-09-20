//! The board-capability **vocabulary**: the names and shapes a board's facts must use.
//!
//! Not a board's facts — the grammar they are written in. It exists so that
//! `media.video.encode.h264` means the same thing on a bare-metal board and on a Linux SBC, and so a
//! **misspelling** can be told from a **new thing** rather than being read as "this board has none".
//!
//! Two rules, and they are the whole discipline:
//!   * **Strict on shape** — a capability of the wrong shape is refused.
//!   * **Open on vocabulary** — a new name under a category is legal, *because* it is one line in
//!     `schema/capabilities.yaml`. It is still refused from a board entry, so a typo cannot become a
//!     silent absence: the difference between *new* and *misspelled* is that someone added the former
//!     to this file.
//!
//! The vocabulary is **embedded**, not read from a path — it ships with the code it validates, so the
//! file and the loader can never be different versions.

use serde_json::Value;

/// The vocabulary, compiled in. The path is relative to this file: `src/` → the crate → `crates/` →
/// the repo root, where `schema/` lives.
const VOCABULARY: &str = include_str!("../../../schema/capabilities.yaml");

/// The vocabulary, parsed — `categories: { radio: { wifi: {…}, … }, … }`.
fn vocabulary() -> Value {
    serde_yaml::from_str(VOCABULARY).expect("the embedded vocabulary parses")
}

/// Every category, and the names directly beneath it, both sorted.
///
/// One level deep, because that is where the discipline bites: a category is a *kind* of capability
/// and the names under it are the things a board may declare. A name whose value is itself a block
/// (`radio.ieee802154`) is a name here; its children are checked when they are used.
pub fn categories() -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = vocabulary()["categories"]
        .as_object()
        .map(|cats| {
            cats.iter()
                .map(|(category, names)| {
                    let mut names: Vec<String> = names
                        .as_object()
                        .map(|n| n.keys().cloned().collect())
                        .unwrap_or_default();
                    names.sort();
                    (category.clone(), names)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Whether `name` is a known capability under `category`.
pub fn is_known(category: &str, name: &str) -> bool {
    categories()
        .iter()
        .any(|(c, names)| c == category && names.iter().any(|n| n == name))
}

/// The capability paths in `caps` the vocabulary does not know, as `category.name` — the
/// misspellings, and any category that is not one of ours.
///
/// Empty means every declared name is known. An empty *tree* is also empty: a board declaring no
/// capabilities is not an error, it is a board with no declared capabilities.
pub fn unknown_names(caps: &Value) -> Vec<String> {
    let known = categories();
    let Some(obj) = caps.as_object() else {
        return vec!["(not a mapping)".to_string()];
    };
    let mut out = Vec::new();
    for (category, names) in obj {
        let Some((_, known_names)) = known.iter().find(|(c, _)| c == category) else {
            out.push(format!("{category} (unknown category)"));
            continue;
        };
        if let Some(names) = names.as_object() {
            for name in names.keys() {
                if !known_names.iter().any(|n| n == name) {
                    out.push(format!("{category}.{name}"));
                }
            }
        }
    }
    out.sort();
    out
}

/// Every **capability path** in a declared tree, sorted — the names a seeder makes nodes for.
///
/// The rule is structural, and it needs no vocabulary lookup: **a key whose value is a mapping is a
/// capability; a scalar or a list is a property *of* the enclosing capability.** So
///
/// ```text
/// media: { video: { encode: { codec: h264 } }, camera: { interface: mipi-csi } }
/// ```
///
/// yields `media.camera` and `media.video.encode` — *not* `media.video.encode.h264`, because `codec`
/// is a property and `h264` is its value. That distinction was wrong in an earlier hand-written
/// example, and this is the function that settles it rather than a convention.
///
/// A capability that carries nothing else is emitted as itself (`zigbee: {}` → `radio.ieee802154.
/// zigbee`), and an intermediate that only holds capabilities (`radio`, `radio.ieee802154`) is a
/// *path segment*, not a thing a board declares.
pub fn capability_paths(tree: &Value) -> Vec<String> {
    fn walk(value: &Value, prefix: &str, out: &mut Vec<String>) {
        let Some(obj) = value.as_object() else {
            return;
        };
        for (key, child) in obj {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            // A scalar or list is a property, not a capability: it names no node.
            let Some(child) = child.as_object() else {
                continue;
            };
            // The **top level is the category level** — the vocabulary's own shape — so a category is
            // always a path segment and never a thing a board declares. That is the whole difference
            // between an empty `radio: {}` (which names nothing) and an empty `zigbee: {}` (which
            // names a capability that carries nothing else).
            if prefix.is_empty() {
                walk(&Value::Object(child.clone()), &path, out);
                continue;
            }
            if child.values().any(Value::is_object) {
                walk(&Value::Object(child.clone()), &path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, "", &mut out);
    out.sort();
    out
}

#[cfg(test)]
mod capability_path_tests {
    use super::*;

    /// A capability is a mapping; a property is what its leaves hold. The example that was wrong in
    /// the notes is the first thing this pins.
    #[test]
    fn a_property_is_not_a_capability_path() {
        let media = serde_json::json!({
            "media": {
                "video": { "encode": { "codec": "h264", "max": "1080p30" } },
                "camera": { "interface": "mipi-csi" }
            }
        });
        assert_eq!(
            capability_paths(&media),
            vec!["media.camera", "media.video.encode"],
            "`codec: h264` is a property of `media.video.encode`, not a capability called `h264`"
        );
    }

    /// The nesting the vocabulary was designed around: one radio, three protocol families, and an
    /// empty block that is still a capability.
    #[test]
    fn nesting_and_empty_blocks_are_both_capabilities() {
        let radio = serde_json::json!({
            "radio": {
                "ieee802154": { "zigbee": {}, "thread": {} },
                "wifi": { "standard": "802.11ax", "bands": ["2.4g"] }
            }
        });
        assert_eq!(
            capability_paths(&radio),
            vec![
                "radio.ieee802154.thread",
                "radio.ieee802154.zigbee",
                "radio.wifi",
            ],
            "`radio` and `radio.ieee802154` are path segments; the leaves are what a board declares"
        );

        assert!(capability_paths(&serde_json::json!({})).is_empty());
        assert!(
            capability_paths(&serde_json::json!({ "radio": {} })).is_empty(),
            "a declared-but-empty category names nothing"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vocabulary loads from the embedded file — so the file and the loader cannot drift, and a
    /// capability added there is a capability the loader knows.
    #[test]
    fn the_embedded_vocabulary_loads() {
        let names: Vec<String> = categories().into_iter().map(|(c, _)| c).collect();
        assert_eq!(
            names,
            vec!["compute", "io", "media", "power", "radio", "sensing", "storage"]
        );
        assert!(is_known("radio", "wifi"));
        assert!(
            is_known("radio", "ieee802154"),
            "a name whose value is a block"
        );
        assert!(is_known("compute", "ml"));
        assert!(
            !is_known("radio", "wiffi"),
            "a misspelling is not a capability"
        );
        assert!(!is_known("wireless", "wifi"), "nor is an unknown category");
    }

    /// The point of the validator: a misspelled capability is **refused**, not read as "this board
    /// has none". That distinction is the whole reason the vocabulary is data.
    #[test]
    fn a_misspelled_capability_is_refused() {
        let good: Value = serde_json::json!({
            "radio": { "ieee802154": { "thread": {} } },
            "media": { "camera": { "interface": "mipi-csi" } }
        });
        assert!(
            unknown_names(&good).is_empty(),
            "known names pass: {:?}",
            unknown_names(&good)
        );

        assert_eq!(
            unknown_names(&serde_json::json!({ "radio": { "wiffi": {} } })),
            vec!["radio.wiffi"]
        );
        assert_eq!(
            unknown_names(&serde_json::json!({ "wireless": { "wifi": {} } })),
            vec!["wireless (unknown category)"]
        );

        // Declaring nothing, or nothing at all, is a board with no capabilities — not an error.
        assert!(unknown_names(&serde_json::json!({ "radio": {} })).is_empty());
        assert!(unknown_names(&serde_json::json!({})).is_empty());
    }
}

/// The capabilities a block declares, each with the mapping written under it — what a seeder turns
/// into a node (the path) and an edge (the `via`/`firmware` inside it).
///
/// Built on [`capability_paths`] so the naming rule keeps **one** implementation: the paths come
/// from there, and each one's properties are found by following it back down the tree (`a.b` →
/// `tree["a"]["b"]`), which needs no second walk and so cannot disagree with the first.
///
/// The input is a block, and a block is **category-rooted** — a chip's `capabilities:` and a board's
/// `realized:` are the same shape, which is the point: a board says it realizes `media.camera`, and a
/// chip says its silicon *is* `media.camera`.
pub fn realizations(tree: &Value) -> Vec<(String, Value)> {
    capability_paths(tree)
        .into_iter()
        .map(|path| {
            let mut node = tree;
            for part in path.split('.') {
                node = &node[part];
            }
            (path, node.clone())
        })
        .collect()
}

#[cfg(test)]
mod realization_tests {
    use super::*;

    /// The leaf's own mapping comes back with its path — the `via:` a seeder makes an edge from — and
    /// the path is the *same* path `capability_paths` would give, because it is literally the same
    /// rule with the properties kept.
    #[test]
    fn a_realization_carries_the_properties_written_under_it() {
        let board = serde_json::json!({
            "media": { "camera": { "via": "esp32p4", "connector": "csi0" } },
            "io": { "ethernet": { "via": "esp32p4" } }
        });
        let got = realizations(&board);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, "io.ethernet", "sorted by path");
        assert_eq!(got[0].1["via"], "esp32p4");
        assert_eq!(got[1].0, "media.camera");
        assert_eq!(got[1].1["connector"], "csi0");

        // The names are the same ones the path rule produces, always.
        let names: Vec<String> = got.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(names, capability_paths(&board));
    }
}

/// The silicon a board **carries** beside its host — a board's `companions:` as seeder input: each
/// companion's chip id, with the rest of its mapping (`role`, `link`, `firmware`) beside it.
///
/// A **list**, not a tree, and that is the point: a board carries a *set* of companions, not a
/// taxonomy of them. `companions:` holds the silicon that *can* be attached — the ESP32-C6 on the
/// Stamp-P4 — while which ones *are* attached belongs to a project, which is why this produces an
/// edge per companion rather than a property of the board.
///
/// No block, an empty list, or an entry with no `chip:` all mean the same thing: nothing to carry.
/// And an entry without a chip names no silicon, so it is dropped rather than written as an edge to
/// nothing — the same refusal as everywhere else, applied to a list instead of a name.
pub fn carries(companions: Option<&Value>) -> Vec<(String, Value)> {
    companions
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    let chip = entry.get("chip")?.as_str()?.to_string();
                    Some((chip, entry.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod carries_tests {
    use super::*;

    /// The chip id is the edge's target; everything else written beside it (`role`, `link`,
    /// `firmware`) travels as the edge's properties.
    #[test]
    fn a_board_carries_companion_chips() {
        let companions = serde_json::json!([{
            "chip": "esp32c6",
            "role": "radio",
            "link": { "bus": "uart" },
            "firmware": "esp-hosted"
        }]);
        let got = carries(Some(&companions));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "esp32c6");
        assert_eq!(got[0].1["role"], "radio");
        assert_eq!(got[0].1["link"]["bus"], "uart");
        assert_eq!(got[0].1["firmware"], "esp-hosted");

        // Nothing declared, nothing carried.
        assert!(carries(None).is_empty());
        assert!(carries(Some(&serde_json::json!([]))).is_empty());
        assert!(
            carries(Some(&serde_json::json!([{ "role": "radio" }]))).is_empty(),
            "an entry with no chip names no silicon to carry"
        );
    }
}

/// A declared block as **seeder input**: what a graph writer needs, with no tree left in it.
///
/// Flattened here, on this side of the message, because it cannot be flattened on the other:
/// `spire-core` is what this crate depends on, so a seeder there can only be handed names and
/// properties. What it is handed:
///
/// * `capabilities` — every path the entry declares, as node names. A chip's `capabilities:` and a
///   board's `realized:` are deliberately the same shape, so both contribute here.
/// * `realizes` — a board's realization edges: the path, and the `via`/`firmware` written under it.
/// * `carries` — a board's companion edges: the chip, and what is written beside it.
/// * `pins` — the wiring, passed through untouched. It is board facts for a BSP, not graph edges.
///
/// Keys with nothing in them are omitted, so an entry that declares nothing produces nothing — and
/// a caller can tell that from `null` without inspecting four empty containers.
pub fn seeder_input(blocks: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let mut names: Vec<String> = Vec::new();

    if let Some(caps) = blocks.get("capabilities") {
        names.extend(capability_paths(caps));
    }
    if let Some(realized) = blocks.get("realized") {
        let edges = realizations(realized);
        names.extend(edges.iter().map(|(path, _)| path.clone()));
        if !edges.is_empty() {
            out.insert(
                "realizes".into(),
                Value::Array(
                    edges
                        .into_iter()
                        .map(|(capability, properties)| {
                            serde_json::json!({ "capability": capability, "properties": properties })
                        })
                        .collect(),
                ),
            );
        }
    }
    if let Some(companions) = blocks.get("companions") {
        let carried = carries(Some(companions));
        if !carried.is_empty() {
            out.insert(
                "carries".into(),
                Value::Array(
                    carried
                        .into_iter()
                        .map(|(chip, properties)| {
                            serde_json::json!({ "chip": chip, "properties": properties })
                        })
                        .collect(),
                ),
            );
        }
    }
    if let Some(pins) = blocks.get("pins") {
        out.insert("pins".into(), pins.clone());
    }

    names.sort();
    names.dedup();
    // A name the vocabulary does not know is **refused here**, where the names are: a typo (`wiffi`
    // for `wifi`) must not become a silent absence that reads exactly like a board with no radio.
    // Refused from the *names* rather than rewritten out of the tree — one place, and the declared
    // facts stay intact for the diagnostic that has to name them.
    let mut refused: Vec<String> = Vec::new();
    for key in ["capabilities", "realized"] {
        if let Some(block) = blocks.get(key) {
            refused.extend(unknown_names(block));
        }
    }
    if !refused.is_empty() {
        names.retain(|name| !refused.contains(name));
        tracing::warn!(
            refused = ?refused,
            "capabilities not in the vocabulary - refused; add them to schema/capabilities.yaml if they are real"
        );
    }
    if !names.is_empty() {
        out.insert("capabilities".into(), serde_json::json!(names));
    }
    if out.is_empty() {
        Value::Null
    } else {
        Value::Object(out)
    }
}

#[cfg(test)]
mod seeder_input_tests {
    use super::*;

    /// A chip contributes names and nothing else; a board contributes names, its realization edges,
    /// its companion edges — and passes its wiring through, because a BSP needs it and the graph
    /// does not.
    #[test]
    fn a_declared_block_becomes_seeder_input() {
        let chip = serde_json::json!({
            "capabilities": { "media": { "video": { "encode": { "codec": "h264" } } } }
        });
        assert_eq!(
            seeder_input(&chip)["capabilities"],
            serde_json::json!(["media.video.encode"]),
            "a chip names nodes; it has no edges"
        );
        assert!(seeder_input(&chip).get("realizes").is_none());

        let board = serde_json::json!({
            "realized": { "media": { "camera": { "via": "esp32p4" } } },
            "companions": [{ "chip": "esp32c6", "role": "radio" }],
            "pins": { "led": { "pin": "GPIO48" } }
        });
        let got = seeder_input(&board);
        assert_eq!(got["capabilities"], serde_json::json!(["media.camera"]));
        assert_eq!(got["realizes"][0]["capability"], "media.camera");
        assert_eq!(got["realizes"][0]["properties"]["via"], "esp32p4");
        assert_eq!(got["carries"][0]["chip"], "esp32c6");
        assert_eq!(got["carries"][0]["properties"]["role"], "radio");
        assert_eq!(got["pins"]["led"]["pin"], "GPIO48", "wiring passes through");

        assert!(
            seeder_input(&serde_json::json!({})).is_null(),
            "nothing declared, nothing sent"
        );
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    /// The refusal is the whole point: a misspelled capability is dropped from the node names while
    /// a known one beside it survives — so a typo cannot be read back as "this board has none".
    #[test]
    fn a_typod_capability_is_refused_and_a_known_one_survives() {
        let block = serde_json::json!({
            "capabilities": {
                "radio": { "wiffi": {}, "wifi": { "standard": "802.11ax" } }
            }
        });
        let got = seeder_input(&block);
        assert_eq!(
            got["capabilities"],
            serde_json::json!(["radio.wifi"]),
            "`radio.wiffi` is refused; `radio.wifi` survives"
        );

        // A block that is entirely misspelled refuses everything and names nothing — which is the
        // distinction that matters: nothing, not a wrong something.
        let bad = serde_json::json!({ "capabilities": { "radio": { "wiffi": {} } } });
        let got = seeder_input(&bad);
        assert!(
            got.get("capabilities").is_none(),
            "all refused, nothing named: {got}"
        );
    }
}
