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
