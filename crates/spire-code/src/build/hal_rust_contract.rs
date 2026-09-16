// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Rust HAL **contracts**, for the drift machinery.
//!
//! The C++ HAL models a contract as an abstract class and its implementation as a
//! per-platform subclass; the Rust HAL models them as a `trait` and an `impl`. The drift
//! question is identical in both — *which methods does the contract require that this
//! implementation does not provide?* — so this module answers it in the same shape
//! `extract_contract_methods_cpp` answers it for C++.
//!
//! `trait_item` and `impl_item` were already mapped in `ast_parser::rust_language_config`, and
//! `tree-sitter-rust` was already a dependency, so this is wiring rather than new machinery.
//!
//! It is deliberately not yet wired into `hal_missing_impls`: that path is proven against C++
//! and should be extended with the Rust branch beside it rather than rewritten around it.

use tree_sitter::Node;

fn rust_parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    let _ = parser.set_language(&tree_sitter_rust::LANGUAGE.into());
    parser
}

fn named_children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.is_named())
        .collect()
}

/// Every `trait` in `content` with the methods an `impl` **must** provide.
///
/// Only methods with no default body count. A provided method (with a default body) is
/// satisfied by the trait itself, so reporting it as missing would be a false positive —
/// and worse, a false positive the fill path would then try to generate.
pub fn required_trait_methods_rust(content: &str) -> Vec<(String, Vec<String>)> {
    let mut parser = rust_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    collect_traits(tree.root_node(), content, &mut out);
    out
}

fn collect_traits(node: Node, content: &str, out: &mut Vec<(String, Vec<String>)>) {
    if node.kind() == "trait_item" {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content.as_bytes()).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "(unnamed)".to_string());
        let mut required = Vec::new();
        if let Some(body) = node.child_by_field_name("body") {
            for child in named_children(body) {
                // `function_signature_item` is a declaration with no body; a
                // `function_item` inside a trait has one, i.e. it is a default.
                if child.kind() != "function_signature_item" {
                    continue;
                }
                if let Some(method) = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(content.as_bytes()).ok())
                {
                    required.push(method.trim().to_string());
                }
            }
        }
        out.push((name, required));
    }
    for child in named_children(node) {
        collect_traits(child, content, out);
    }
}

/// Every `impl Trait for Type` in `content` → the trait's name and the methods it provides.
///
/// Inherent impls (`impl Foo`) are skipped: they implement nothing but themselves, so they
/// cannot satisfy a contract.
pub fn extract_impl_methods_rust(content: &str) -> Vec<(String, Vec<String>)> {
    let mut parser = rust_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    collect_impls(tree.root_node(), content, &mut out);
    out
}

fn collect_impls(node: Node, content: &str, out: &mut Vec<(String, Vec<String>)>) {
    if node.kind() == "impl_item" {
        if let Some(trait_node) = node.child_by_field_name("trait") {
            let trait_name = trait_node
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .trim()
                // `impl hal::Led for X` must match the trait `Led`, so take the last
                // path segment rather than the whole path.
                .rsplit("::")
                .next()
                .unwrap_or("")
                .to_string();
            let mut provided = Vec::new();
            if let Some(body) = node.child_by_field_name("body") {
                for child in named_children(body) {
                    if child.kind() != "function_item" {
                        continue;
                    }
                    if let Some(method) = child
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(content.as_bytes()).ok())
                    {
                        provided.push(method.trim().to_string());
                    }
                }
            }
            out.push((trait_name, provided));
        }
    }
    for child in named_children(node) {
        collect_impls(child, content, out);
    }
}

/// **The drift measure for Rust**: contract methods with no implementation.
///
/// A trait with no `impl` at all reports all of its required methods, which is the case the
/// contract cascade exists for (an interface declared, nothing implementing it).
pub fn missing_trait_methods_rust(
    contract: &str,
    implementation: &str,
) -> Vec<(String, Vec<String>)> {
    let impls = extract_impl_methods_rust(implementation);
    let mut out = Vec::new();

    for (trait_name, required) in required_trait_methods_rust(contract) {
        let provided = impls
            .iter()
            .find(|(name, _)| name == &trait_name)
            .map(|(_, methods)| methods.clone())
            .unwrap_or_default();
        let missing: Vec<String> = required
            .into_iter()
            .filter(|method| !provided.contains(method))
            .collect();
        if !missing.is_empty() {
            out.push((trait_name, missing));
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Coverage, in the shape `hal_missing_impls` already consumes
// ─────────────────────────────────────────────────────────────────────────────────────

use crate::build::generic_helpers::HalInterfaceCoverage;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The Rust HAL's coverage map, in the same shape as `hal_platform_coverage_map`.
///
/// A **sibling** to that C++ function rather than a branch inside it. The C++ path is proven
/// and carries the whole HAL workflow; the embedded work must not put it at risk to add
/// itself, so the two produce the same type and the caller merges them. A project mid-
/// migration can legitimately have both.
pub fn rust_platform_coverage_map(
    root: &Path,
) -> BTreeMap<String, BTreeMap<String, HalInterfaceCoverage>> {
    // 1. Contracts: `hal/api/*.rs` (and the toolkit mirror). The file STEM is the interface
    //    key, matching the C++ convention — so `led.rs` is the `led` interface whatever its
    //    trait happens to be called.
    let mut contracts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for dir in [
        root.join("hal").join("api"),
        root.join("toolkit").join("src").join("hal").join("api"),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("rs") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let required: Vec<String> = required_trait_methods_rust(&content)
                .into_iter()
                .flat_map(|(_, methods)| methods)
                .collect();
            // A file with no trait, or a trait with no required methods, is not a contract.
            if required.is_empty() {
                continue;
            }
            contracts.insert(stem.to_string(), required);
        }
    }
    if contracts.is_empty() {
        return BTreeMap::new();
    }

    // 2. Platform dirs — the same discovery the C++ path uses: the canonical
    //    `hal/implementations/<plat>/`, plus the legacy top-level `<plat>/hal/`.
    let mut platform_dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    let canonical = root.join("hal").join("implementations");
    if let Ok(entries) = std::fs::read_dir(&canonical) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            if let Some(plat) = entry.file_name().to_str() {
                if !plat.starts_with('.') {
                    platform_dirs
                        .entry(plat.to_string())
                        .or_insert_with(|| entry.path());
                }
            }
        }
    }
    let skip = [
        "toolkit",
        "hal",
        "build",
        "build-native",
        "subprojects",
        ".git",
    ];
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Hidden dirs and meson build dirs (`build-rpi5`) are not platforms.
            if skip.contains(&name) || name.starts_with('.') || name.starts_with("build") {
                continue;
            }
            let legacy = path.join("hal");
            if legacy.is_dir() {
                platform_dirs.entry(name.to_string()).or_insert(legacy);
            }
        }
    }

    // 3. Coverage per platform × interface.
    let mut coverage: BTreeMap<String, BTreeMap<String, HalInterfaceCoverage>> = BTreeMap::new();
    for (plat, dir) in platform_dirs {
        // Every trait this platform implements and what it provides — read once per platform
        // rather than once per contract.
        let mut impls: Vec<(String, Vec<String>)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) != Some("rs") {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    impls.extend(extract_impl_methods_rust(&content));
                }
            }
        }
        // The trait may be capitalised while the stem is not (`led.rs` → `trait Led`), so the
        // match is case-insensitive. The C++ path gets the same freedom from file stems.
        let provided_for = |stem: &str| -> Vec<String> {
            impls
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(stem))
                .flat_map(|(_, methods)| methods.clone())
                .collect()
        };

        let mut iface_map: BTreeMap<String, HalInterfaceCoverage> = BTreeMap::new();
        for (stem, required) in &contracts {
            let provided = provided_for(stem);
            let missing: Vec<String> = required
                .iter()
                .filter(|method| !provided.contains(method))
                .cloned()
                .collect();
            iface_map.insert(
                stem.clone(),
                HalInterfaceCoverage {
                    implemented: missing.is_empty(),
                    // `has_impl` asks whether the trait is implemented AT ALL, not whether any
                    // method was found: `impl Led for X {}` is a real (partial) impl, and
                    // conflating it with "no impl" would send the fill flow off to scaffold a
                    // type that already exists. Its own test caught exactly that.
                    has_impl: impls
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case(stem)),
                    is_stub: false,
                    missing,
                    // No Rust fill path exists yet, so signatures are not collected. Leaving
                    // them empty is honest; inventing blank ones would look like a contract
                    // that had been read and found complete.
                    missing_sigs: Vec::new(),
                    drifted: Vec::new(),
                },
            );
        }
        coverage.insert(plat, iface_map);
    }
    coverage
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A contract in the shape `spire-hal` actually uses: one required method, one provided.
    const CONTRACT: &str = r#"
/// A single binary output.
pub trait Led {
    /// Required — every board must implement this.
    fn set(&mut self, on: bool);
    /// Provided — the trait satisfies this itself, so an impl owes nothing.
    fn toggle(&mut self, on: bool) {
        let _ = on;
    }
}
"#;

    #[test]
    fn a_complete_impl_is_not_drift() {
        let imp = "impl Led for GpioLed { fn set(&mut self, _on: bool) {} }";
        assert!(
            missing_trait_methods_rust(CONTRACT, imp).is_empty(),
            "a complete impl has no drift"
        );
    }

    #[test]
    fn a_missing_method_is_drift() {
        // `set` is not implemented — the C++ path calls this a partial gap.
        let drift = missing_trait_methods_rust(CONTRACT, "impl Led for GpioLed {}");
        assert_eq!(drift.len(), 1, "{drift:?}");
        assert_eq!(drift[0].0, "Led");
        assert_eq!(drift[0].1, vec!["set".to_string()]);
    }

    #[test]
    fn a_trait_with_no_impl_reports_every_required_method() {
        // The cascade's starting state: a contract on disk and nothing implementing it.
        let drift = missing_trait_methods_rust(CONTRACT, "// nothing yet");
        assert_eq!(drift.len(), 1, "{drift:?}");
        assert_eq!(drift[0].1, vec!["set".to_string()]);
    }

    #[test]
    fn a_provided_method_is_never_drift() {
        // `toggle` has a default body, so omitting it is complete. Reporting it would be a
        // false positive — and one the fill path would then try to generate code for.
        let imp = "impl Led for X { fn set(&mut self, _on: bool) {} }";
        assert!(
            missing_trait_methods_rust(CONTRACT, imp).is_empty(),
            "a default method is not owed"
        );
    }

    #[test]
    fn an_inherent_impl_does_not_satisfy_a_trait() {
        // `impl GpioLed` implements nothing but itself, so the contract is still unmet.
        let imp = "impl GpioLed { fn set(&mut self, _on: bool) {} }";
        let drift = missing_trait_methods_rust(CONTRACT, imp);
        assert_eq!(
            drift,
            vec![("Led".to_string(), vec!["set".to_string()])],
            "an inherent impl must not be mistaken for the trait"
        );
    }

    #[test]
    fn a_qualified_trait_path_matches_the_bare_trait_name() {
        // Real code writes `impl hal::Led for ...`; it must match `trait Led`.
        let imp = "impl hal::Led for GpioLed { fn set(&mut self, _on: bool) {} }";
        assert!(missing_trait_methods_rust(CONTRACT, imp).is_empty());
    }

    #[test]
    fn several_traits_are_measured_separately() {
        let contract = "trait A { fn a(&self); } trait B { fn b(&self); }";
        let imp = "impl B for X { fn b(&self) {} }";
        let drift = missing_trait_methods_rust(contract, imp);
        assert_eq!(drift.len(), 1, "only A is unimplemented: {drift:?}");
        assert_eq!(drift[0].0, "A");
    }

    /// The coverage map on a real temp project — this is the shape `hal_missing_impls`
    /// renders, so it is the contract between the Rust branch and the rest of Spire.
    #[test]
    fn a_rust_contract_becomes_coverage_the_way_a_cpp_one_does() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::write(
            root.join("hal/api/led.rs"),
            "pub trait Led {\n    fn set(&mut self, on: bool);\n}\n",
        )
        .unwrap();

        // One platform that declares the trait and implements nothing: the PARTIAL case,
        // which the fill flow adds methods to.
        let partial = root.join("hal/implementations/esp32c6");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("led.rs"), "impl Led for GpioLed {}\n").unwrap();

        // And one that is complete.
        let complete = root.join("hal/implementations/host");
        std::fs::create_dir_all(&complete).unwrap();
        std::fs::write(
            complete.join("led.rs"),
            "impl Led for FakeLed {\n    fn set(&mut self, _on: bool) {}\n}\n",
        )
        .unwrap();

        let map = rust_platform_coverage_map(root);

        let c6 = &map["esp32c6"]["led"];
        assert_eq!(c6.missing, vec!["set".to_string()], "the gap must be named");
        assert!(c6.has_impl, "a file exists, so this is partial, not absent");
        assert!(!c6.implemented, "a declared-but-empty impl is not coverage");

        let host = &map["host"]["led"];
        assert!(host.missing.is_empty(), "{:?}", host.missing);
        assert!(host.implemented, "a complete impl must count");
        assert!(host.has_impl);

        // The interface key is the file STEM, matching the C++ convention — which is exactly
        // what lets both languages land in one map.
        assert!(map["esp32c6"].contains_key("led"));
    }

    /// A project with no Rust contracts contributes an empty map, so merging it into the C++
    /// coverage is a no-op rather than a source of phantom interfaces.
    #[test]
    fn a_project_without_rust_contracts_contributes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(rust_platform_coverage_map(tmp.path()).is_empty());
    }
}
