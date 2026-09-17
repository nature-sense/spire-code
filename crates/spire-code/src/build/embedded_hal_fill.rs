// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **Rust** HAL fill plan: what a scaffolded backend still owes, and the prompt that asks for
//! it.
//!
//! Read-only, as `hal_fill::plan` is and for the same reason: the work is reviewed before anything
//! is written. It differs from the C++ plan in one structural way — a C++ implementation is a file
//! per interface, while a Rust backend implements *every* contract trait (the traits are modules,
//! not classes) and may hold them in several files. The unit of work is therefore the **file**: an
//! item names one backend file, what it still owes, and the prompt that asks for it.
//!
//! The prompt is the point of the plan. Everything an implementation needs and cannot discover
//! from the file itself is injected here — the vendor crate, the runtime (std or not) and the
//! platform's own `library_hints` — because the failure mode of a generated backend is not a
//! missing method, it is a method written against the wrong API for that chip.
//!
//! Pending work comes from [`crate::build::hal_rust_contract::rust_platform_coverage_map`], the
//! same measure the UI's HAL indicators read. A scaffolded backend reports as pending even though
//! its stub declares every method: the bodies are `unimplemented!()`, which the measure counts as
//! a placeholder rather than an implementation.

use crate::build::embedded_hal_scaffold::{family_spec, FamilySpec};
use crate::platform::Platform;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One contract trait this backend still owes, with the state that decides how it is presented.
struct Pending {
    /// Contract file stem — the interface key the rest of Spire uses (`led`, `time`).
    interface: String,
    /// The trait as the contract declares it (`led.rs` → `Led`).
    trait_name: String,
    /// `none` (no impl at all) · `stub` (the methods are declared, the bodies are placeholders) ·
    /// `partial` (some methods genuinely written, some not).
    status: &'static str,
    /// The methods to write. For a stub that is every required method — none of them is written
    /// yet, however much the file looks like it implements them.
    methods: Vec<String>,
    /// The contract's own source, injected so the model implements the trait it was given rather
    /// than a reconstituted idea of it.
    source: String,
    /// The file of the backend that declares this trait's impl. A backend starts as the scaffold's
    /// one `lib.rs` but does not have to stay one file; the fill must name the file that actually
    /// holds the impl, or hand the model the wrong one.
    file: PathBuf,
}

/// The facts one backend's prompt is rendered from.
///
/// A struct rather than a `serde_json::Value` so the renderer stays pure and can be exercised
/// without a project on disk.
struct FillFacts<'a> {
    root: &'a Path,
    hal_crate: &'a str,
    backend_crate: &'a str,
    family: &'a str,
    file: &'a Path,
    /// The file's current source — what the pending bodies are being written into.
    file_source: &'a str,
    spec: &'a FamilySpec,
    /// `(label, value)` — the board's identity, or just the family when the registry has no record
    /// for it.
    profile: &'a [(String, String)],
    /// The platform's `library_hints`, already resolved. Empty is a real possibility and the prompt
    /// says so rather than pretending the board needs no guidance.
    hints: &'a str,
    pending: &'a [Pending],
}

fn dir_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

/// The relative path shown in the prompt: an absolute path in a prompt reads as noise, and the
/// model is editing one file whose name is unambiguous from the workspace root.
/// The relative path shown in the prompt: an absolute path in a prompt reads as noise, and the
/// model is editing one file whose name is unambiguous from the workspace root.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

/// The registry record whose hints describe this family's hardware.
///
/// The scaffold collapses the wizard's selection to families, so after scaffolding the family is
/// what the project still names. The *hints* must nevertheless be one board's: "how to reach the
/// LED on this chip" is a variant fact, which is why a platform id is preferred and only the
/// family is used to find a stand-in.
fn platform_for(family: &str, id: Option<&str>) -> Option<Platform> {
    if let Some(id) = id {
        return Platform::from_registry(id).filter(|p| p.family.as_deref() == Some(family));
    }
    Platform::load_directory(Platform::default_platform_dir())
        .ok()?
        .into_iter()
        .find(|p| p.is_embedded() && p.family.as_deref() == Some(family))
}

/// The backend file that declares `trait_name`'s impl, or the crate's `lib.rs` when none does yet.
///
/// A backend starts as the one file the scaffold writes (`lib.rs`, every trait in it) but does not
/// have to stay one: the reference workspace splits its traits into modules, and a fill that
/// follows it would too. The fallback matters for the `none` status — nothing declares the trait
/// yet, so `lib.rs` is where it goes, beside the other impls.
fn impl_file(src_dir: &Path, trait_name: &str) -> PathBuf {
    let mut files: Vec<PathBuf> = std::fs::read_dir(src_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|x| x.to_str()) == Some("rs"))
        .collect();
    files.sort();

    let is_lib = |path: &Path| path.file_name().and_then(|n| n.to_str()) == Some("lib.rs");
    // `lib.rs` last: it usually re-exports the modules, so a trait declared *in* it is a trait
    // nothing has been split out of.
    for path in files.iter().filter(|p| !is_lib(p)) {
        if let Ok(content) = std::fs::read_to_string(path) {
            let declares = crate::build::hal_rust_contract::extract_impl_methods_rust(&content)
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(trait_name));
            if declares {
                return path.clone();
            }
        }
    }
    files
        .into_iter()
        .find(|p| is_lib(p))
        .unwrap_or_else(|| src_dir.join("lib.rs"))
}

/// The prompt's hardware-profile lines: what the board is, and what the backend may use.
fn profile_lines(
    family: &str,
    record: Option<&Platform>,
    spec: &FamilySpec,
) -> Vec<(String, String)> {
    let mut out = vec![("family".to_string(), family.to_string())];
    if let Some(p) = record {
        out.push(("board".to_string(), format!("{} ({})", p.name, p.id)));
        if let Some(rust) = &p.rust {
            out.push((
                "chip".to_string(),
                rust.idf_target.clone().unwrap_or_else(|| p.id.clone()),
            ));
            out.push(("target".to_string(), rust.target.clone()));
        }
        out.push(("os".to_string(), p.os.clone()));
    }
    out.push((
        "runtime".to_string(),
        if spec.uses_std_executor {
            "std — the shared executor crate".to_string()
        } else {
            "no_std — this crate supplies its own `Spawner`".to_string()
        },
    ));
    out.push(("vendor".to_string(), spec.vendor_crate.to_string()));
    out
}

/// Render the fill prompt for one backend.
///
/// The order is the order the work is done in: what the file is, what the board is, what the
/// platform says about it, what the contract requires, what is pending, and the rules that keep the
/// answer inside this file and this vendor's API.
fn render_prompt(f: &FillFacts<'_>) -> String {
    let mut p = String::new();
    p.push_str(&format!(
        "Implement the `{}` backend of the `{}` embedded-HAL workspace at {}.\n\n",
        f.family,
        f.hal_crate,
        f.root.display()
    ));

    p.push_str("FILE TO EDIT (the only file you may write):\n");
    p.push_str(&format!("  {}\n\n", relative(f.root, f.file)));

    p.push_str("BACKEND\n");
    // The crate is named explicitly: the model is editing one of two similarly-named siblings
    // (`demo-hal-esp32` next to the contract's `demo-hal`), and the wrong one compiles nowhere.
    p.push_str(&format!("  {:<9}{}\n", "crate", f.backend_crate));
    for (label, value) in f.profile {
        p.push_str(&format!("  {label:<9}{value}\n"));
    }
    p.push('\n');

    p.push_str("HARDWARE NOTES (this platform's own)\n");
    if f.hints.trim().is_empty() {
        p.push_str("  (none recorded for this platform)\n\n");
    } else {
        p.push_str(&format!("{}\n\n", f.hints.trim()));
    }

    p.push_str("THE CONTRACTS — implement these, never edit them\n");
    for pending in f.pending {
        p.push_str(&format!(
            "--- crates/{}/src/hal/{}.rs ---\n{}\n",
            f.hal_crate,
            pending.interface,
            pending.source.trim_end()
        ));
    }
    p.push('\n');

    p.push_str("PENDING IN THIS FILE\n");
    for pending in f.pending {
        p.push_str(&format!(
            "  {} [{}]: {} — {}\n",
            pending.interface,
            pending.status,
            pending.trait_name,
            pending.methods.join(", ")
        ));
    }
    p.push('\n');

    p.push_str("CURRENT FILE\n");
    p.push_str(f.file_source.trim_end());
    p.push_str("\n\n");

    p.push_str("RULES\n");
    p.push_str(
        "1. Write the pending methods and nothing else. Every other item — the structs, their\n\
         \x20  field types, the executor block — stays exactly as it is.\n\
         2. Replace the `unimplemented!()` bodies. Do not leave one behind and do not add a new\n\
         \x20  one: the workspace measures placeholders, so an `unimplemented!()` left in place\n\
         \x20  reports this backend as unfinished.\n",
    );
    p.push_str(&format!(
        "3. Build every hardware access on `{}`. Add no dependency and reach for no other API:\n\
         \x20  this crate's manifest lists exactly one, and the build will fail on anything else.\n",
        f.spec.vendor_crate
    ));
    p.push_str(
        "4. The `hal` module is the contract, shared by every backend. Implement it as declared —\n\
         \x20  same names, same types, same semantics as its doc comments — and change nothing in\n\
         \x20  it, not even to make an implementation easier.\n",
    );
    if !f.spec.uses_std_executor {
        p.push_str(
            "5. This crate is `no_std`: no `std::`, no `println!`, no heap unless the vendor API\n\
             \x20  hands you an allocation it owns.\n",
        );
    }
    if f.pending.iter().any(|p| p.status == "none") {
        p.push_str(
            "6. A trait with no impl yet needs a concrete type for this family and its\n\
             \x20  `impl <Trait> for <Type>` declared in this file, matching the struct shape the\n\
             \x20  other traits here already use.\n",
        );
    }
    p.push_str(
        "7. If this board cannot do what a method's doc promises, reply saying so instead of\n\
         \x20  writing an approximation. A stub that fails loudly beats one that lies.\n",
    );

    p
}

/// The fill plan for every backend crate in `root` that still owes something.
///
/// `platform` is a registry id (the board the wizard selected); it supplies the prompt's hardware
/// profile and hints. Absent — or a variant of another family — the family's first registry record
/// stands in, because one backend serves a whole family and the prompt still needs *a* board's
/// facts.
pub(crate) fn plan(root: &Path, platform: Option<&str>) -> serde_json::Value {
    let coverage = crate::build::hal_rust_contract::rust_platform_coverage_map(root);
    let (_contracts, backends) = crate::build::hal_rust_contract::embedded_hal_layout(root);

    let mut plan: Vec<serde_json::Value> = Vec::new();
    let mut refused: Vec<serde_json::Value> = Vec::new();

    // A BTreeMap, so families come out sorted and the plan is stable between runs.
    for (family, src_dir) in &backends {
        // The crate directory carries both names: `<contract>-<family>`.
        let Some(backend_crate) = src_dir.parent().and_then(dir_name) else {
            continue;
        };
        let Some(hal_crate) = backend_crate.strip_suffix(&format!("-{family}")) else {
            continue;
        };

        let mut pending: Vec<Pending> = Vec::new();
        for (interface, cov) in coverage.get(family).into_iter().flatten() {
            if cov.implemented {
                continue;
            }
            let source = std::fs::read_to_string(
                root.join("crates")
                    .join(hal_crate)
                    .join("src")
                    .join("hal")
                    .join(format!("{interface}.rs")),
            )
            .unwrap_or_default();
            let traits = crate::build::hal_rust_contract::required_trait_methods_rust(&source);
            let trait_name = traits
                .first()
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| interface.clone());
            let required: Vec<String> = traits
                .iter()
                .flat_map(|(_, methods)| methods.clone())
                .collect();
            let (status, methods) = if !cov.has_impl {
                ("none", required)
            } else if cov.is_stub {
                ("stub", required)
            } else {
                ("partial", cov.missing.clone())
            };
            pending.push(Pending {
                interface: interface.clone(),
                trait_name: trait_name.clone(),
                status,
                methods,
                source,
                file: impl_file(src_dir, &trait_name),
            });
        }
        if pending.is_empty() {
            continue;
        }

        // One item per FILE, not per family: a backend starts as the scaffold's single `lib.rs`
        // but need not stay one file, and an item whose `file` is not the file holding the impl
        // would point the model at the wrong place.
        let mut by_file: BTreeMap<PathBuf, Vec<Pending>> = BTreeMap::new();
        for owed in pending {
            by_file.entry(owed.file.clone()).or_default().push(owed);
        }

        // Refusing by name, as the scaffold does: a prompt without the vendor facts would ask for
        // an API invented from a family name, and that is the one outcome worse than a stub.
        let Some(spec) = family_spec(family) else {
            refused.push(json!({
                "family": family,
                "crate": backend_crate,
                "reason": "no vendor facts for this family — a fill prompt would have to invent the vendor API",
            }));
            continue;
        };

        let record = platform_for(family, platform);
        let hints = record
            .as_ref()
            .map(|p| crate::build::generic_helpers::hal_platform_library_hints(&p.id))
            .unwrap_or_default();
        let profile = profile_lines(family, record.as_ref(), &spec);

        for (file, pending) in &by_file {
            let file_source = std::fs::read_to_string(file).unwrap_or_default();
            let facts = FillFacts {
                root,
                hal_crate,
                backend_crate,
                family,
                file,
                file_source: &file_source,
                spec: &spec,
                profile: &profile,
                hints: &hints,
                pending,
            };

            plan.push(json!({
                "family": family,
                "crate": backend_crate,
                "contract_crate": hal_crate,
                "file": file.to_string_lossy(),
                "platform": record.as_ref().map(|p| p.id.clone()),
                "vendor_crate": spec.vendor_crate,
                "kind": if pending.iter().any(|p| p.status != "none") {
                    "fill_existing_impl"
                } else {
                    "scaffold_impls"
                },
                "pending": pending
                    .iter()
                    .map(|p| json!({
                        "interface": p.interface,
                        "trait": p.trait_name,
                        "status": p.status,
                        "methods": p.methods,
                    }))
                    .collect::<Vec<_>>(),
                "prompt": render_prompt(&facts),
            }));
        }
    }

    json!({
        "plan": plan,
        "refused": refused,
        "note": "Read-only. Each item is one backend file with its pending traits and the prompt \
                 that asks for them; the Rust fill apply step (which would send `prompt` to the \
                 model and write `file`) is not implemented yet.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hermetic registry: the plan must not read the machine's `~/.spire/platforms`, and a test
    /// must be able to name a board (an rp2040) this machine may not have.
    fn registry() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("esp32c6.yaml"),
            "id: esp32c6\nname: Espressif ESP32-C6\nos: esp-idf\nfamily: esp32\n\
             library_hints: |\n  esp-idf-hal: call `peripherals()` once; TIMG0 is the delay.\n\
             architecture:\n  cpu_family: riscv32\n  cpu: riscv32\n  endian: little\n  \
             target_triple: riscv32imac-esp-espidf\n\
             rust:\n  target: riscv32imac-esp-espidf\n  idf_target: esp32c6\n",
        )
        .unwrap();
        dir
    }

    /// A project in the shape the scaffold emits: the contract crate with one trait per file, and
    /// one backend crate per family whose bodies are `led_body`.
    fn project(root: &Path, led_body: &str) {
        std::fs::create_dir_all(root.join("crates/demo-hal/src/hal")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/led.rs"),
            "/// A single binary output.\n\
             pub trait Led {\n    /// Required.\n    fn set(&mut self, on: bool);\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/demo-hal-esp32/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/lib.rs"),
            format!(
                "use demo_hal::hal::Led;\n\npub struct GpioLed;\n\n\
                 impl Led for GpioLed {{\n    fn set(&mut self, _on: bool) {{\n{led_body}\n    }}\n}}\n"
            ),
        )
        .unwrap();
    }

    /// A backend split into modules — the shape the reference workspace uses, and the shape a
    /// filled backend tends towards — is planned against the file that holds each impl, not against
    /// a `lib.rs` that only re-exports.
    #[test]
    fn a_split_backend_names_the_file_that_holds_the_impl() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry();
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        unimplemented!(\"GpioLed::set\")");
        // Move the impl out of `lib.rs` into `led.rs`, leaving `lib.rs` as a re-export.
        let stub = std::fs::read_to_string(root.join("crates/demo-hal-esp32/src/lib.rs")).unwrap();
        let impl_part = stub
            .split_once("pub struct GpioLed;")
            .expect("the stub has a struct")
            .1;
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/led.rs"),
            format!("use demo_hal::hal::Led;\n\npub struct GpioLed;{impl_part}"),
        )
        .unwrap();
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/lib.rs"),
            "pub mod led;\npub use led::GpioLed;\n",
        )
        .unwrap();

        let out = plan(root, None);
        let items = out["plan"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{out:?}");
        assert!(
            items[0]["file"].as_str().unwrap().ends_with("src/led.rs"),
            "the file with the impl, not the re-export: {out:?}"
        );
        assert!(
            items[0]["prompt"]
                .as_str()
                .unwrap()
                .contains("crates/demo-hal-esp32/src/led.rs"),
            "and the prompt names that file"
        );
    }

    /// **The scaffold's own output is the plan's first input.** Every stub method is declared, so
    /// drift alone would call the backend complete and the fill queue would be empty; the status
    /// has to come from the placeholder bodies.
    #[test]
    fn a_scaffolded_backend_is_one_pending_plan_item() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry();
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        unimplemented!(\"GpioLed::set\")");

        let out = plan(root, None);
        let items = out["plan"].as_array().unwrap();
        assert_eq!(items.len(), 1, "one family owes work: {out:?}");

        let item = &items[0];
        assert_eq!(item["family"], "esp32");
        assert_eq!(item["crate"], "demo-hal-esp32");
        assert_eq!(item["contract_crate"], "demo-hal");
        assert_eq!(item["vendor_crate"], "esp-idf-hal");
        assert_eq!(item["kind"], "fill_existing_impl");
        assert!(item["file"]
            .as_str()
            .unwrap()
            .ends_with("demo-hal-esp32/src/lib.rs"));

        // The board comes from the family when the caller names none, so the hints are one
        // board's — the chip the pilot build runs on.
        assert_eq!(item["platform"], "esp32c6");

        let pending = item["pending"].as_array().unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0]["interface"], "led");
        assert_eq!(pending[0]["trait"], "Led");
        assert_eq!(pending[0]["status"], "stub");
        assert_eq!(
            pending[0]["methods"],
            serde_json::json!(["set"]),
            "a stub owes every required method"
        );

        let prompt = item["prompt"].as_str().unwrap();
        assert!(prompt.contains("`esp-idf-hal`"), "the vendor API: {prompt}");
        assert!(
            prompt.contains("crate    demo-hal-esp32"),
            "the crate being edited: {prompt}"
        );
        assert!(
            prompt.contains("TIMG0 is the delay"),
            "the platform's own hints: {prompt}"
        );
        assert!(
            prompt.contains("pub trait Led"),
            "the contract itself, not a paraphrase: {prompt}"
        );
        assert!(
            prompt.contains("crates/demo-hal-esp32/src/lib.rs"),
            "the file to edit: {prompt}"
        );
        assert!(
            prompt.contains("led [stub]: Led — set"),
            "what is pending: {prompt}"
        );
        assert!(
            prompt.contains("Replace the `unimplemented!()` bodies"),
            "the placeholder convention: {prompt}"
        );
        // A std family gets no `no_std` rule: telling a model not to use `std` where `std::thread`
        // is the executor would be the opposite of the truth.
        assert!(
            !prompt.contains("This crate is `no_std`"),
            "std family: {prompt}"
        );
    }

    /// Once the body is real there is nothing to plan — the same measure that filled the queue
    /// empties it.
    #[test]
    fn a_filled_backend_is_not_in_the_plan() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry();
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        let _ = _on;");

        let out = plan(root, None);
        assert!(
            out["plan"].as_array().unwrap().is_empty(),
            "a written backend is not work: {out:?}"
        );
        assert!(out["refused"].as_array().unwrap().is_empty());
    }

    /// A family the scaffold has no vendor facts for is **refused by name**, not handed a prompt
    /// that would have to invent the API. The scaffold refuses to create such a crate, so this is
    /// the case where one was written by hand.
    #[test]
    fn an_unknown_family_is_refused_rather_than_prompted() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry();
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        unimplemented!(\"GpioLed::set\")");
        // A second backend whose family this scaffold does not know.
        std::fs::create_dir_all(root.join("crates/demo-hal-nordic/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal-nordic/src/lib.rs"),
            "impl Led for X {\n    fn set(&mut self, _on: bool) {\n        unimplemented!(\"set\")\n    }\n}\n",
        )
        .unwrap();

        let out = plan(root, None);
        assert_eq!(
            out["plan"].as_array().unwrap().len(),
            1,
            "only the known family is planned: {out:?}"
        );
        let refused = out["refused"].as_array().unwrap();
        assert_eq!(refused.len(), 1, "{out:?}");
        assert_eq!(refused[0]["family"], "nordic");
        assert_eq!(refused[0]["crate"], "demo-hal-nordic");
        assert!(
            refused[0]["reason"].as_str().unwrap().contains("invent"),
            "the refusal says why: {refused:?}"
        );
    }

    /// A platform id from another family does not supply the hints: the profile would then describe
    /// a board this backend cannot be built for. The prompt says the platform recorded nothing,
    /// which is what the model should see.
    #[test]
    fn a_platform_of_another_family_contributes_no_hints() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK.lock().unwrap();
        let reg = registry();
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        unimplemented!(\"GpioLed::set\")");

        let out = plan(root, Some("rpi5"));
        let item = &out["plan"][0];
        let prompt = item["prompt"].as_str().unwrap();
        assert!(item["platform"].is_null(), "{item:?}");
        assert!(
            prompt.contains("(none recorded for this platform)"),
            "an empty profile is stated, not hidden: {prompt}"
        );
        assert!(
            !prompt.contains("TIMG0 is the delay"),
            "and no other board's hints leak in: {prompt}"
        );
    }

    /// Print the plan for a REAL embedded-HAL workspace, so the measure and the prompt can be read
    /// against files nobody wrote for a test.
    ///
    /// Ignored by default because it reads a project outside the target directory, which is the
    /// caller's decision rather than a test's:
    ///
    /// ```sh
    /// SPIRE_FILL_PLAN_ROOT=../spire-hal cargo test -p spire-code --lib \
    ///     dump_fill_plan_for_a_real_project -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "reads a real project from SPIRE_FILL_PLAN_ROOT"]
    fn dump_fill_plan_for_a_real_project() {
        let Ok(root) = std::env::var("SPIRE_FILL_PLAN_ROOT") else {
            println!("set SPIRE_FILL_PLAN_ROOT to a project root");
            return;
        };
        let out = plan(std::path::Path::new(&root), None);
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    }
}
