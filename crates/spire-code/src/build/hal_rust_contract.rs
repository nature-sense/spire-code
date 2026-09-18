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
//! It is wired into `hal_missing_impls` **beside** the C++ map, not into it: the proven C++
//! path is what the whole HAL workflow runs on today, and a sibling that produces the same type
//! cannot put it at risk. The embedded-HAL layout (`crates/<prefix>-hal`, one backend crate per
//! family) and the C++-style one (`hal/api`, `hal/implementations/<plat>`) are both discovered,
//! so a project mid-migration measures everything it has.

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

/// A structural Rust syntax problem, in the same shape `cpp_syntax_check` reports one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustSyntaxError {
    pub line: u32,
    pub col: u32,
    pub kind: String,
    pub context: String,
}

/// The verdict of [`rust_syntax_check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustSyntaxReport {
    pub ok: bool,
    pub errors: Vec<RustSyntaxError>,
}

/// Structural syntax check with the same `tree-sitter-rust` CST the drift measure parses.
///
/// Structural only — a type error still needs `cargo`. What it is for is the moment before a
/// generated backend is written: a file that does not parse would otherwise land in a crate the
/// host `cargo test` builds, turning a bad generation into a build failure the user has to read.
pub fn rust_syntax_check(content: &str) -> RustSyntaxReport {
    let mut parser = rust_parser();
    let Some(tree) = parser.parse(content, None) else {
        return RustSyntaxReport {
            ok: false,
            errors: Vec::new(),
        };
    };
    let root = tree.root_node();
    if !root.has_error() {
        return RustSyntaxReport {
            ok: true,
            errors: Vec::new(),
        };
    }
    let mut errors = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            let (row, col) = (node.start_position().row, node.start_position().column);
            let context = content
                .lines()
                .nth(row)
                .map(|line| line.trim().to_string())
                .unwrap_or_default();
            errors.push(RustSyntaxError {
                line: row as u32 + 1,
                col: col as u32 + 1,
                kind: node.kind().to_string(),
                context,
            });
            continue; // do not descend into erroneous nodes
        }
        for child in named_children(node) {
            stack.push(child);
        }
    }
    RustSyntaxReport { ok: false, errors }
}

/// Traits whose `impl` blocks are still **placeholders** — a body containing `unimplemented!()`.
///
/// The drift measure above counts a declared method as provided whatever its body, which is what
/// makes a scaffolded backend "look implemented": every required method is there and none of them
/// does anything. This is the Rust counterpart of the C++ `SPIRE-HAL-STUB` sentinel, and it is
/// deliberately the macro rather than a comment of our own: `unimplemented!()` *is* the statement
/// "not implemented", it panics loudly if reached, and it cannot be deleted by tidying up
/// comments.
pub fn placeholder_impls_rust(content: &str) -> Vec<String> {
    let mut parser = rust_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_placeholder_impls(tree.root_node(), content, &mut out);
    out
}

fn collect_placeholder_impls(node: Node, content: &str, out: &mut Vec<String>) {
    if node.kind() == "impl_item" {
        if let (Some(body), Some(trait_node)) = (
            node.child_by_field_name("body"),
            node.child_by_field_name("trait"),
        ) {
            let body_text = body.utf8_text(content.as_bytes()).unwrap_or("");
            if body_text.contains("unimplemented!") {
                let name = trait_node
                    .utf8_text(content.as_bytes())
                    .unwrap_or("")
                    .trim()
                    .rsplit("::")
                    .next()
                    .unwrap_or("")
                    .to_string();
                out.push(name);
            }
        }
    }
    for child in named_children(node) {
        collect_placeholder_impls(child, content, out);
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// The board contract — what a backend owes when the traits are `embedded-hal`'s
// ─────────────────────────────────────────────────────────────────────────────────────

/// The interface key a backend's **board** is measured under.
///
/// With the peripheral traits coming from `embedded-hal`, a scaffolded project has no contract
/// traits of its own to drift from: the compiler enforces those. What its backend still owes is
/// **constructors** — the board's own facts (which pin, which polarity, which delay), which nothing
/// outside the backend can supply. That is a smaller contract than the one it replaced, and it is a
/// real one: it is what a second family has to reproduce, and what a generated application calls.
pub const BOARD_INTERFACE: &str = "board";

/// The type the constructors hang off.
///
/// A type, deliberately not a trait: a `trait Board` would be a contract of our invention again, one
/// level up, and would need exactly the machinery this measure replaced.
pub const BOARD_TYPE: &str = "Board";

/// The constructors every backend owes — and nothing more. A method is added here when a *second*
/// family needs it, the same rule the contract traits used to follow.
pub const BOARD_METHODS: &[&str] = &["led", "delay"];

/// Methods of every **inherent** `impl <Type> { … }` in `content` → `(type, methods)`.
///
/// `extract_impl_methods_rust` reads `impl Trait for Type`, which is what a contract trait produces
/// and therefore what the drift measure has always counted. A board's constructors are inherent
/// (`impl Board { fn led(…) }`), so they need their own reading — without it a scaffolded backend
/// reports as having no implementation of anything, and the fill queue stays empty.
pub fn extract_inherent_methods_rust(content: &str) -> Vec<(String, Vec<String>)> {
    let mut parser = rust_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    collect_inherent_impls(tree.root_node(), content, &mut out);
    out
}

fn collect_inherent_impls(node: Node, content: &str, out: &mut Vec<(String, Vec<String>)>) {
    // The `trait` field is what tells the two `impl` forms apart; absent means inherent.
    if node.kind() == "impl_item" && node.child_by_field_name("trait").is_none() {
        if let (Some(type_node), Some(body)) = (
            node.child_by_field_name("type"),
            node.child_by_field_name("body"),
        ) {
            let type_name = type_node
                .utf8_text(content.as_bytes())
                .unwrap_or("")
                .trim()
                .to_string();
            let mut methods = Vec::new();
            for child in named_children(body) {
                if child.kind() == "function_item" {
                    if let Some(name) = child.child_by_field_name("name") {
                        if let Ok(text) = name.utf8_text(content.as_bytes()) {
                            methods.push(text.trim().to_string());
                        }
                    }
                }
            }
            out.push((type_name, methods));
        }
    }
    for child in named_children(node) {
        collect_inherent_impls(child, content, out);
    }
}

/// Method names whose body is still a placeholder — `unimplemented!()` or `todo!()`.
///
/// `placeholder_impls_rust` answers the same question for a whole `impl Trait for Type` block, which
/// is enough when every owed method comes from a trait. A board's constructors are inherent methods,
/// so it has to be asked per method — and `todo!()` counts, because a stub may be saying only "the
/// signature is here, the body is not".
pub fn placeholder_methods_rust(content: &str) -> Vec<String> {
    let mut parser = rust_parser();
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_placeholder_methods(tree.root_node(), content, &mut out);
    out
}

fn collect_placeholder_methods(node: Node, content: &str, out: &mut Vec<String>) {
    if node.kind() == "function_item" {
        if let Some(body) = node.child_by_field_name("body") {
            let text = body.utf8_text(content.as_bytes()).unwrap_or("");
            if text.contains("unimplemented!") || text.contains("todo!") {
                if let Some(name) = node.child_by_field_name("name") {
                    if let Ok(name) = name.utf8_text(content.as_bytes()) {
                        out.push(name.trim().to_string());
                    }
                }
            }
        }
    }
    for child in named_children(node) {
        collect_placeholder_methods(child, content, out);
    }
}

/// Every `.rs` file under `dir`, recursively, as source text.
///
/// Recursive because a backend starts as the scaffold's one `lib.rs` and the fill may move the board
/// into a module of its own (the fill's own file lookup allows exactly that), so "the file that
/// declares the board" is not known in advance.
fn rust_sources(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_sources(&path));
        } else if path.extension().and_then(|x| x.to_str()) == Some("rs") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.push(content);
            }
        }
    }
    out
}

/// What one backend crate's board still owes — or `None` when it declares no `Board` at all.
///
/// `None` rather than "everything missing" on purpose: a project without a board (a hand-written
/// HAL, or one mid-migration from the C++ layout) must not be reported as owing a constructor it
/// never had. Only a backend that has a board owes one.
fn board_coverage(src_dir: &Path) -> Option<HalInterfaceCoverage> {
    let sources = rust_sources(src_dir);
    let mut has_impl = false;
    let mut provided: Vec<String> = Vec::new();
    for content in &sources {
        for (type_name, methods) in extract_inherent_methods_rust(content) {
            if type_name == BOARD_TYPE {
                has_impl = true;
                provided.extend(methods);
            }
        }
    }
    if !has_impl {
        return None;
    }
    let placeholders: Vec<String> = sources
        .iter()
        .flat_map(|c| placeholder_methods_rust(c))
        .collect();
    let missing: Vec<String> = BOARD_METHODS
        .iter()
        .filter(|method| !provided.iter().any(|p| p == *method))
        .map(|method| (*method).to_string())
        .collect();
    // A placeholder is not coverage, one level down from the trait-impl rule below: the scaffold's
    // stub declares every constructor and implements none, and without this a fresh backend would
    // report complete exactly when the fill queue should be full.
    let is_stub = !BOARD_METHODS.is_empty()
        && BOARD_METHODS
            .iter()
            .all(|method| placeholders.iter().any(|p| p == method));
    Some(HalInterfaceCoverage {
        implemented: missing.is_empty() && !is_stub,
        has_impl,
        is_stub,
        missing,
        // As for the traits: the fill prompt carries the file and the re-exported traits instead, so
        // a signature list would be a thinner second copy of what the model is given.
        missing_sigs: Vec::new(),
        drifted: Vec::new(),
    })
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

/// A contract stem and the traits declared in its file: `time.rs` → `[("DelayMs", ["delay_ms"])]`.
///
/// The trait names are carried alongside the stem because the stem is only the *file's* name. The
/// reference workspace's `time.rs` declares `DelayMs`, and matching an impl against the stem alone
/// reports an implementation that exists as missing — the one failure that makes a fill rewrite
/// what is already there.
pub type RustContract = Vec<(String, Vec<String>)>;

/// The **embedded-HAL** layout: a contract crate (`crates/<prefix>-hal`) and one backend crate
/// per board family (`crates/<prefix>-hal-<family>`) — the shape the `embedded-hal` project type
/// scaffolds.
///
/// Naming is the discovery here, exactly as it is for the C++ path (`hal/implementations/<plat>`)
/// and for the same reason: the crate *is* the backend, so its name is the only place the family
/// is written down. `-std` is excluded **by convention**: it is the shared executor crate (the
/// `Spawner`/`Mailbox`/`Actor` infrastructure, present for any std family), not a board family,
/// so measuring it as a coverage platform would report infrastructure traits as board gaps.
///
/// Returns `(contract stems → their traits, family → its `src` directory)`.
pub fn embedded_hal_layout(
    root: &Path,
) -> (BTreeMap<String, RustContract>, BTreeMap<String, PathBuf>) {
    let mut contracts: BTreeMap<String, RustContract> = BTreeMap::new();
    let mut backends: BTreeMap<String, PathBuf> = BTreeMap::new();

    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return (contracts, backends);
    };
    // Read the crate names once: the contract crate is `<prefix>-hal`, and its backends are
    // found by that prefix rather than by a second naming rule.
    let names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();

    for name in &names {
        if !name.ends_with("-hal") {
            continue;
        }
        // The contract's traits: `crates/<prefix>-hal/src/hal/*.rs`, stem-keyed like the C++ set.
        let trait_dir = root.join("crates").join(name).join("src").join("hal");
        if let Ok(entries) = std::fs::read_dir(&trait_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) != Some("rs") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let traits: RustContract = required_trait_methods_rust(&content)
                    .into_iter()
                    .filter(|(_, methods)| !methods.is_empty())
                    .collect();
                // `hal/mod.rs` re-exports rather than declares, so it contributes nothing.
                if traits.is_empty() {
                    continue;
                }
                contracts.insert(stem.to_string(), traits);
            }
        }
        // Its backends: `crates/<prefix>-hal-<family>/src`.
        for candidate in &names {
            let Some(family) = candidate.strip_prefix(&format!("{name}-")) else {
                continue;
            };
            if family == "std" {
                continue;
            }
            let src = root.join("crates").join(candidate).join("src");
            if src.is_dir() {
                backends.insert(family.to_string(), src);
            }
        }
    }

    (contracts, backends)
}

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
    //    trait happens to be called. The traits are kept with it, because the key is a file name
    //    and the match has to be against the trait.
    let mut contracts: BTreeMap<String, RustContract> = BTreeMap::new();
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
            // A file with no trait, or a trait with no required methods, is not a contract.
            let traits: RustContract = required_trait_methods_rust(&content)
                .into_iter()
                .filter(|(_, methods)| !methods.is_empty())
                .collect();
            if traits.is_empty() {
                continue;
            }
            contracts.insert(stem.to_string(), traits);
        }
    }
    // The embedded-HAL layout contributes contracts of its own: same stem-keyed shape, different
    // tree (`crates/<prefix>-hal/src/hal/*.rs`). Merged rather than branched so a project mid-way
    // between the two layouts still measures everything it has.
    let (rust_contracts, rust_backends) = embedded_hal_layout(root);
    contracts.extend(rust_contracts);
    // A scaffolded project has **no contract traits of its own** — the traits are `embedded-hal`'s —
    // so the contract set is legitimately empty and only its backends' boards are measured. The
    // early return therefore waits for both to be empty, or a freshly scaffolded HAL would report
    // nothing at all and the fill queue would never fill.
    if contracts.is_empty() && rust_backends.is_empty() {
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

    // 2b. embedded-HAL backends: one crate per board family, keyed by the family the crate name
    //     names. Insert-if-absent like the C++ discovery above, so a project that has both a
    //     `hal/implementations/esp32` and a `crates/…-hal-esp32` measures the canonical one.
    for (family, dir) in rust_backends {
        platform_dirs.entry(family).or_insert(dir);
    }

    // 3. Coverage per platform × interface.
    let mut coverage: BTreeMap<String, BTreeMap<String, HalInterfaceCoverage>> = BTreeMap::new();
    for (plat, dir) in platform_dirs {
        // Every trait this platform implements and what it provides — read once per platform
        // rather than once per contract.
        let mut impls: Vec<(String, Vec<String>)> = Vec::new();
        // Traits whose impl bodies are still `unimplemented!()` — the scaffold's placeholders.
        let mut placeholders: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|x| x.to_str()) != Some("rs") {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    impls.extend(extract_impl_methods_rust(&content));
                    placeholders.extend(placeholder_impls_rust(&content));
                }
            }
        }
        // The match is **trait-name first, stem second**. The stem is a file name: this
        // workspace's `time.rs` declares `DelayMs` and its `camera_hal.rs` would declare
        // `CameraHal`, so matching the stem alone reports impls that exist as missing — and the
        // fill flow then rewrites what is already there. The stem stays as the fallback for the
        // case where an impl is written for the interface rather than for the trait.
        let matches = |impl_trait: &str, stem: &str, traits: &RustContract| -> bool {
            impl_trait.eq_ignore_ascii_case(stem)
                || traits
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(impl_trait))
        };
        let provided_for = |stem: &str, traits: &RustContract| -> Vec<String> {
            impls
                .iter()
                .filter(|(name, _)| matches(name, stem, traits))
                .flat_map(|(_, methods)| methods.clone())
                .collect()
        };

        let mut iface_map: BTreeMap<String, HalInterfaceCoverage> = BTreeMap::new();
        for (stem, traits) in &contracts {
            let required: Vec<String> = traits
                .iter()
                .flat_map(|(_, methods)| methods.clone())
                .collect();
            let provided = provided_for(stem, traits);
            let missing: Vec<String> = required
                .iter()
                .filter(|method| !provided.contains(method))
                .cloned()
                .collect();
            // A placeholder is not coverage. The scaffold's stub declares every required method
            // and implements none of them, so "an impl exists" is true while "implemented" must
            // still be false — this is the Rust reading of the C++ `SPIRE-HAL-STUB` sentinel, and
            // without it a freshly scaffolded backend would report as complete and the fill queue
            // would be empty exactly when it should be full.
            let is_stub = placeholders.iter().any(|name| matches(name, stem, traits));
            iface_map.insert(
                stem.clone(),
                HalInterfaceCoverage {
                    implemented: missing.is_empty() && !is_stub,
                    // `has_impl` asks whether the trait is implemented AT ALL, not whether any
                    // method was found: `impl Led for X {}` is a real (partial) impl, and
                    // conflating it with "no impl" would send the fill flow off to scaffold a
                    // type that already exists. Its own test caught exactly that.
                    has_impl: impls.iter().any(|(name, _)| matches(name, stem, traits)),
                    is_stub,
                    missing,
                    // Signatures are not collected here: the Rust fill prompt carries the whole
                    // contract file instead, so a signature list would be a second, thinner copy
                    // of what the model is given. Left empty rather than guessed at.
                    missing_sigs: Vec::new(),
                    drifted: Vec::new(),
                },
            );
        }
        // A Rust backend also owes its **board** — the constructors nothing else can supply. Measured
        // both when the project declares no contract traits of its own (the scaffolded shape) and
        // when it does, because either way it is something a second family must reproduce.
        // `None` when the directory declares no `Board`, so a C++ platform — or a hand-written HAL
        // with no board — is never asked for a constructor it never had.
        if let Some(board) = board_coverage(&dir) {
            iface_map.insert(BOARD_INTERFACE.to_string(), board);
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

    /// Only the trait whose *own* impl block is a placeholder is reported as one. The C++ sentinel
    /// can be per file because a file is one interface; a Rust backend file holds every trait it
    /// implements, so "pending" has to be per `impl`.
    #[test]
    fn only_the_placeholder_trait_is_reported_as_one() {
        let src = "impl Led for GpioLed {\n    fn set(&mut self, _on: bool) {\n        unimplemented!(\"set\")\n    }\n}\n\n\
                   impl DelayMs for FamilyDelay {\n    fn delay_ms(&mut self, _ms: u32) {}\n}\n";
        assert_eq!(placeholder_impls_rust(src), vec!["Led".to_string()]);
    }

    /// …and a backend file that has been filled in reports no placeholder at all.
    #[test]
    fn a_filled_impl_is_not_a_placeholder() {
        let src = "impl Led for GpioLed {\n    fn set(&mut self, on: bool) {\n        let _ = on;\n    }\n}\n";
        assert!(placeholder_impls_rust(src).is_empty());
    }

    /// The **embedded-HAL** layout: a contract crate (`crates/<prefix>-hal/src/hal/*.rs`) and one
    /// backend crate per family (`crates/<prefix>-hal-<family>`). This is what the `embedded-hal`
    /// project type scaffolds — the C++ layout the measure was written for does not exist there, so
    /// without this discovery a scaffolded project would report zero coverage rows, i.e. nothing to
    /// fill, exactly when every file is a stub.
    #[test]
    fn the_embedded_hal_layout_is_measured_by_crate_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/demo-hal/src/hal")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/led.rs"),
            "pub trait Led {\n    fn set(&mut self, on: bool);\n}\n",
        )
        .unwrap();
        // `hal/mod.rs` re-exports; it declares no contract and must not become an interface.
        std::fs::write(
            root.join("crates/demo-hal/src/hal/mod.rs"),
            "pub mod led;\npub use led::Led;\n",
        )
        .unwrap();
        // The scaffold's stub verbatim: the trait IS implemented and the method IS declared, so
        // syntax alone would call this complete.
        std::fs::create_dir_all(root.join("crates/demo-hal-esp32/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/lib.rs"),
            "impl Led for GpioLed {\n    fn set(&mut self, _on: bool) {\n        unimplemented!(\"GpioLed::set\")\n    }\n}\n",
        )
        .unwrap();
        // The shared executor is a crate of the same prefix and must not become a platform.
        std::fs::create_dir_all(root.join("crates/demo-hal-std/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-std/src/lib.rs"),
            "// the executor\n",
        )
        .unwrap();

        let map = rust_platform_coverage_map(root);
        assert_eq!(
            map.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["esp32"],
            "one family, and `std` is not one: {map:?}"
        );

        let led = &map["esp32"]["led"];
        assert!(led.has_impl, "the backend declares the impl");
        assert!(led.is_stub, "…and its body is a placeholder");
        assert!(
            !led.implemented,
            "a placeholder is not coverage — the fill queue must not be empty here"
        );
        assert!(
            led.missing.is_empty(),
            "the method is declared, just not written: {:?}",
            led.missing
        );
    }

    /// Once the body is real the same file is coverage — the placeholder is the *body*, so filling
    /// it is what clears the flag (nothing has to remember to remove a marker).
    #[test]
    fn writing_the_body_clears_the_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/demo-hal/src/hal")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/led.rs"),
            "pub trait Led {\n    fn set(&mut self, on: bool);\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/demo-hal-rp2040/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-rp2040/src/lib.rs"),
            "impl Led for GpioLed {\n    fn set(&mut self, on: bool) {\n        let _ = on;\n    }\n}\n",
        )
        .unwrap();

        let cov = &rust_platform_coverage_map(root)["rp2040"]["led"];
        assert!(!cov.is_stub, "a real body is not a placeholder");
        assert!(cov.implemented, "and it counts as coverage");
        assert!(cov.has_impl);
    }

    /// **The stem is a file name, not the trait's name.** The reference workspace's `time.rs`
    /// declares `DelayMs`, and this was caught by running the fill plan against that workspace:
    /// matching the stem alone reported a real implementation as `none` — the one failure that
    /// makes a fill rewrite code that is already there. Both directions are pinned here because
    /// the scaffold emits this exact pair (`hal/time.rs` declaring `DelayMs`).
    #[test]
    fn a_trait_whose_name_differs_from_its_stem_still_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("crates/demo-hal/src/hal")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/time.rs"),
            "pub trait DelayMs {\n    fn delay_ms(&mut self, ms: u32);\n}\n",
        )
        .unwrap();

        // One family has it written, the other still has the scaffold's placeholder.
        std::fs::create_dir_all(root.join("crates/demo-hal-esp32/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/time.rs"),
            "impl DelayMs for FreeRtosDelay {\n    fn delay_ms(&mut self, ms: u32) {\n        let _ = ms;\n    }\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/demo-hal-rp2040/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-rp2040/src/lib.rs"),
            "impl DelayMs for FamilyDelay {\n    fn delay_ms(&mut self, _ms: u32) {\n        unimplemented!(\"FamilyDelay::delay_ms\")\n    }\n}\n",
        )
        .unwrap();

        let map = rust_platform_coverage_map(root);

        let esp32 = &map["esp32"]["time"];
        assert!(
            esp32.has_impl,
            "the impl exists, whatever the file is called: {esp32:?}"
        );
        assert!(esp32.implemented, "and it is written: {esp32:?}");
        assert!(!esp32.is_stub);

        let rp2040 = &map["rp2040"]["time"];
        assert!(rp2040.has_impl, "the placeholder is still an impl");
        assert!(
            rp2040.is_stub,
            "named by its trait, not its stem: {rp2040:?}"
        );
        assert!(!rp2040.implemented);
    }

    /// A project with no Rust contracts contributes an empty map, so merging it into the C++
    /// coverage is a no-op rather than a source of phantom interfaces.
    #[test]
    fn a_project_without_rust_contracts_contributes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(rust_platform_coverage_map(tmp.path()).is_empty());
    }
}
