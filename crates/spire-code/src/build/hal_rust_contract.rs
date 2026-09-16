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
}
