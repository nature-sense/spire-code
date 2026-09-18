//! Authoring the two halves of an embedded-HAL project that are not a *backend*: its
//! **contracts** (the traits) and its **platforms** (a backend crate for another board).
//!
//! The Rust analogues of the C++ `hal_validate_contract`, `hal_write_contract` and
//! `hal_add_platform`, and they keep that trio's shape: validate before anything touches disk, and
//! make the project's own structure the thing that is edited rather than a parallel copy of it.
//!
//! What differs is where the wiring lives. The C++ project wires a new contract and a new platform
//! through `meson.build` files; here a contract file is only visible once `hal/mod.rs` declares and
//! re-exports it, and a backend crate only exists once the workspace manifest lists it as a member.
//! Both of those are *inside this module's job*, because a file that nothing declares is a file the
//! drift measure cannot see — the failure would look like "the measure finds nothing", not like
//! "the write forgot to wire it".

use std::path::Path;

use serde_json::json;

use crate::build::embedded_hal_scaffold::{family_spec, FamilySpec};
use crate::build::hal_rust_contract::{
    extract_impl_methods_rust, required_trait_methods_rust, rust_syntax_check,
};

/// A parsed contract, as JSON: the traits and the methods an `impl` must provide.
///
/// The methods are the *required* ones (no default body), because those are what the measure counts
/// and what the fill must produce — a provided method is satisfied by the trait itself.
fn traits_json(traits: &[(String, Vec<String>)]) -> serde_json::Value {
    json!(traits
        .iter()
        .map(|(name, methods)| json!({ "trait": name, "methods": methods }))
        .collect::<Vec<_>>())
}

/// Validate a contract file's **source** — the gate `_write_contract` passes before disk.
///
/// The rules are not a style opinion: each one is a way a contract can be written such that the
/// measure, the fill or a backend cannot use it. A file that fails any of them is refused with the
/// reason, which is the point — an unvalidated contract becomes every backend's problem later.
pub(crate) fn validate_contract(content: &str) -> Result<serde_json::Value, String> {
    if content.trim().is_empty() {
        return Err("the contract is empty".to_string());
    }
    // 1. It must parse. The measure uses this same parser, so a file that does not parse is one the
    //    measure would silently report as declaring nothing.
    let syntax = rust_syntax_check(content);
    if !syntax.ok {
        let first = syntax
            .errors
            .first()
            .map(|e| format!("line {}:{}: {}", e.line, e.col, e.context))
            .unwrap_or_else(|| "unknown position".to_string());
        return Err(format!(
            "the contract does not parse ({first}); {} problem(s) in total",
            syntax.errors.len()
        ));
    }

    // 2. It must declare a trait with at least one **required** method. A trait whose methods all
    //    have default bodies needs no implementation, so the measure skips it — a contract like that
    //    would read as valid and behave as absent.
    let traits = required_trait_methods_rust(content);
    if traits.is_empty() {
        return Err(
            "no `trait` found: a contract file declares the methods a backend must provide"
                .to_string(),
        );
    }
    let with_methods: Vec<(String, Vec<String>)> = traits
        .iter()
        .filter(|(_, methods)| !methods.is_empty())
        .cloned()
        .collect();
    if with_methods.is_empty() {
        return Err(format!(
            "every trait here has only defaulted methods ({}) — nothing for a backend to \
             implement, so the drift measure would not see this contract at all",
            traits
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // 3. No `impl Trait for Type`. An interface file declares; implementations live in a backend
    //    crate. An early `impl` in the contract crate would satisfy the measure from the wrong
    //    place, marking a board-agnostic stub as the board's implementation.
    let impls = extract_impl_methods_rust(content);
    if let Some((trait_name, _)) = impls.first() {
        return Err(format!(
            "this file implements `{trait_name}`; a contract declares traits and a backend \
             implements them — an implementation here would be reported as every board's"
        ));
    }

    // 4. Two traits with one name in one interface are ambiguous to `use hal::{…}` at the call site,
    //    which is exactly how a backend and a firmware actor reach them.
    let mut seen: Vec<&str> = Vec::new();
    for (name, _) in &with_methods {
        if seen.contains(&name.as_str()) {
            return Err(format!("`{name}` is declared twice in one contract"));
        }
        seen.push(name);
    }

    Ok(json!({
        "valid": true,
        "traits": traits_json(&with_methods),
        "trait_count": with_methods.len(),
        "method_count": with_methods.iter().map(|(_, m)| m.len()).sum::<usize>(),
    }))
}

/// The project's contract crate: `crates/<prefix>-hal` with a `src/hal` directory.
///
/// Found by the same rule the measure uses (`-hal` suffix, `src/hal` present) rather than a second
/// naming convention, so the thing written and the thing measured cannot disagree about which crate
/// holds contracts. Returns its directory name and the `snake_case` id its own code uses.
fn contract_crate_of(root: &Path) -> Result<(String, String), String> {
    let crates = root.join("crates");
    let entries = std::fs::read_dir(&crates)
        .map_err(|e| format!("no `crates/` directory under {}: {e}", root.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // The contract crate is `<prefix>-hal` with a `src`. **Not** `src/hal`: a scaffolded project
        // has no authored traits — the peripheral ones are `embedded-hal`'s — so `src/hal` only
        // appears once someone *writes* a trait of their own, and requiring it here would make the
        // crate invisible to the very tool that writes the first one.
        if !name.ends_with("-hal") || !path.join("src").is_dir() {
            continue;
        }
        return Ok((name.to_string(), name.replace('-', "_")));
    }
    Err(format!(
        "no contract crate (`crates/*-hal` with a `src`) under {}",
        root.display()
    ))
}

/// A file stem that can be a module name, or a refusal naming what was wrong with it.
///
/// Sanitized rather than trusted: the stem becomes a file name *and* a `mod` declaration, so a name
/// like `my traits` would produce a file nothing can declare.
fn module_stem(filename: &str) -> Result<String, String> {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .replace('-', "_");
    if stem.is_empty() {
        return Err("no file name given".to_string());
    }
    let mut chars = stem.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let rest_ok = stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !first_ok || !rest_ok {
        return Err(format!(
            "'{filename}' is not usable as a Rust module name (letters, digits and `_`, not \
             starting with a digit)"
        ));
    }
    // `mod.rs` is the module listing itself — writing a contract over it would delete the project.
    if stem == "mod" {
        return Err("`mod.rs` is the module list, not a contract".to_string());
    }
    Ok(stem)
}

/// Add `pub mod <stem>;` and `pub use <stem>::<Trait>;` to `hal/mod.rs`, if they are not there.
///
/// Inserted beside the existing declarations rather than appended, because the two groups (what the
/// module declares, what it re-exports) are what a reader scans — and because a re-export appended
/// after an unrelated line is a line people delete by accident. Returns whether anything changed.
fn wire_module(
    mod_path: &Path,
    stem: &str,
    traits: &[(String, Vec<String>)],
) -> Result<bool, String> {
    // A scaffolded project has **no** `src/hal` at all — its peripheral traits are `embedded-hal`'s —
    // so the first trait a project authors brings the module with it. The header states the rule the
    // file exists to keep, so it reads as though it had always been there.
    let content = match std::fs::read_to_string(mod_path) {
        Ok(content) => content,
        // The `e` is not needed here: the guard already said the file is absent, which is not an
        // error — it is the state a scaffolded project starts in.
        Err(_) if !mod_path.exists() => {
            "//! The traits this project authors.\n\
             //!\n\
             //! `embedded-hal`'s are re-exported at the crate root, and they are the ones a driver\n\
             //! can be used with. A trait belongs *here* only when it is narrower than the\n\
             //! ecosystem's — a driver's own abstraction, say — and it is added when a second\n\
             //! backend needs it, not in anticipation.\n\n"
                .to_string()
        }
        Err(e) => return Err(format!("cannot read {}: {e}", mod_path.display())),
    };
    let mod_line = format!("pub mod {stem};");
    let mut changed = false;
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

    if !lines.iter().any(|l| l.trim() == mod_line) {
        // After the last `pub mod`, so the declarations stay one group; if there are none, at the end
        // of the file (still valid Rust, and the honest place for the first one).
        let at = lines
            .iter()
            .rposition(|l| l.trim_start().starts_with("pub mod "))
            .map(|i| i + 1)
            .unwrap_or(lines.len());
        lines.insert(at, mod_line);
        changed = true;
    }

    for (name, _) in traits {
        let use_line = format!("pub use {stem}::{name};");
        if lines.iter().any(|l| l.trim() == use_line) {
            continue;
        }
        let at = lines
            .iter()
            .rposition(|l| l.trim_start().starts_with("pub use "))
            .map(|i| i + 1)
            .unwrap_or(lines.len());
        lines.insert(at, use_line);
        changed = true;
    }

    if changed {
        let mut out = lines.join("\n");
        out.push('\n');
        std::fs::write(mod_path, out)
            .map_err(|e| format!("cannot write {}: {e}", mod_path.display()))?;
    }
    Ok(changed)
}

/// Write a validated contract into the contract crate — the Rust `_write_contract`.
///
/// Returns the same `valid`/`summary` shape the C++ tool does, plus the two things that make the
/// write *usable*: the module was declared and re-exported (`wired`), and the traits that are now
/// visible to the measure.
///
/// Two refusals matter more than the happy path. A file that already exists with **different**
/// content is not overwritten: a contract is what every backend implements, so replacing it silently
/// changes an interface other crates compile against — editing it is a deliberate act, not a side
/// effect of authoring. And an **identical** file is a no-op rather than an error, so a caller that
/// retries an interrupted authoring does not have to know whether the first attempt landed.
pub(crate) fn write_contract(
    root: &Path,
    filename: &str,
    content: &str,
) -> Result<serde_json::Value, String> {
    let validated = validate_contract(content)?;
    let stem = module_stem(filename)?;
    let (crate_name, _) = contract_crate_of(root)?;

    let hal_dir = root
        .join("crates")
        .join(&crate_name)
        .join("src")
        .join("hal");
    let target = hal_dir.join(format!("{stem}.rs"));
    // `src/hal` does **not** exist in a scaffolded project: its traits are `embedded-hal`'s, so the
    // directory appears with the first trait a project authors — which is this call, and this is the
    // one place that knows it is about to exist.
    std::fs::create_dir_all(&hal_dir)
        .map_err(|e| format!("cannot create {}: {e}", hal_dir.display()))?;
    // The validated summary already carries the traits, so they are read once and returned as they
    // were validated rather than recomputed from the same string.
    let with_methods: Vec<(String, Vec<String>)> = validated["traits"]
        .as_array()
        .map(|traits| {
            traits
                .iter()
                .map(|t| {
                    (
                        t["trait"].as_str().unwrap_or_default().to_string(),
                        t["methods"]
                            .as_array()
                            .map(|m| {
                                m.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut result = validated;

    if let Ok(existing) = std::fs::read_to_string(&target) {
        if existing != content {
            return Err(format!(
                "{} already exists and differs; editing a contract changes what every backend \
                 implements, so do it deliberately (write the file yourself or remove it first)",
                target.display()
            ));
        }
        // An identical file is a no-op, so a caller retrying an interrupted authoring does not have
        // to know whether the first attempt landed — but the module list is still checked, because
        // a half-wired project is exactly what a retry is for.
        let wired = wire_module(&hal_dir.join("mod.rs"), &stem, &with_methods)?;
        result["written"] = json!(target.to_string_lossy());
        result["unchanged"] = json!(true);
        result["wired"] = json!(wired);
        return Ok(result);
    }

    std::fs::write(&target, content)
        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
    let wired = wire_module(&hal_dir.join("mod.rs"), &stem, &with_methods)?;
    result["written"] = json!(target.to_string_lossy());
    result["wired"] = json!(wired);
    Ok(result)
}

/// Add a board family to an existing embedded-HAL project — the Rust `_add_platform`.
///
/// The backend crate is emitted by the **scaffold's own** function, not by a second implementation
/// here: a new platform must produce exactly what the wizard would have produced had the board been
/// chosen at creation, including the pinned dependency versions that took a compiler to discover.
///
/// What this adds over the scaffold is the two things an *existing* project needs — the refusal to
/// duplicate a family, and the workspace **member** line. Without the member, cargo would not build
/// the crate and the drift measure would not see it, and the failure would present as "the new
/// backend does nothing", not as "the add forgot a line".
pub(crate) fn add_platform(root: &Path, platform_id: &str) -> Result<serde_json::Value, String> {
    let (crate_name, hal_id) = contract_crate_of(root)?;
    // The scaffold names a backend `<hal>-<family>` where `<hal>` is the contract crate's *full*
    // name (`weather-hal-rp2040`), so the sibling naming is inherited rather than re-derived.
    let hal = crate_name.clone();

    let platform = crate::platform::Platform::from_registry(platform_id)
        .ok_or_else(|| format!("unknown platform '{platform_id}' (see ~/.spire/platforms)"))?;
    if !platform.is_embedded() {
        return Err(format!(
            "platform '{platform_id}' is not an embedded platform (os '{}'); this project type \
             needs boards it can build and flash",
            platform.os
        ));
    }
    let family = platform
        .family
        .clone()
        .ok_or_else(|| format!("platform '{platform_id}' names no `family`, so no backend fits"))?;
    let spec = family_spec(&family).ok_or_else(|| {
        format!("no backend is known for family '{family}' (platform '{platform_id}')")
    })?;

    let backend_dir = root.join("crates").join(format!("{hal}-{family}"));
    if backend_dir.exists() {
        return Err(format!(
            "family '{family}' already has a backend crate ({})",
            backend_dir.display()
        ));
    }

    // The workspace member list is what makes the crate part of the project. Read it before writing
    // anything, so a project whose manifest is unreadable is refused rather than half-edited.
    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let member = format!("crates/{hal}-{family}");
    if manifest.contains(&format!("\"{member}\"")) {
        return Err(format!(
            "`{member}` is already a workspace member — the manifest and the crate directory \
             disagree, so nothing was written"
        ));
    }

    let files = crate::build::embedded_hal_scaffold::backend_files(&hal, &hal_id, &family, &spec);
    let mut written: Vec<String> = Vec::new();
    for file in &files {
        let target = root.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&target, &file.content)
            .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        written.push(file.path.clone());
    }

    // Beside the other members, so the list stays one list.
    let mut lines: Vec<String> = manifest.lines().map(str::to_string).collect();
    let member_line = format!("    \"{member}\",");
    let at = lines
        .iter()
        .rposition(|l| {
            let t = l.trim();
            t.starts_with('"') && t.ends_with("\",")
        })
        .map(|i| i + 1)
        .unwrap_or(lines.len());
    lines.insert(at, member_line);
    let mut out = lines.join("\n");
    out.push('\n');
    std::fs::write(&manifest_path, out)
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;

    // The README lists each family and how it is built. A board added later must extend that
    // section rather than leave it describing only the boards that existed at creation — and the
    // block comes from the same per-family data the scaffold renders from, so the two cannot
    // disagree about the command. A README that cannot be read or found is *reported*, not
    // guessed at: appending a build command to a file whose shape is unknown would be worse than
    // saying so.
    let readme_note = match extend_readme(root, &hal, &family, &spec) {
        Ok(()) => format!("README.md: added this family's crate and build command"),
        Err(reason) => format!(
            "README.md was not updated ({reason}) — add the `{family}` backend's build command there"
        ),
    };

    Ok(json!({
        "platform": platform_id,
        "family": family,
        "vendor_crate": spec.vendor_crate,
        "crate": format!("{hal}-{family}"),
        "written": written,
        "workspace_member": member,
        "note": readme_note,
    }))
}

/// Add this family to an existing README: a bullet in the crate list, and its build block inside
/// the "Build a backend" shell fence.
///
/// Both insertions are anchored on the structure the README template produces — the last bullet of
/// the crate list, and the closing fence of the last ```sh block — because there is no parser here
/// and a wrong guess would corrupt a file the user reads. A README that does not match that shape is
/// refused with a reason, which the caller reports as a note.
fn extend_readme(root: &Path, hal: &str, family: &str, spec: &FamilySpec) -> Result<(), String> {
    let path = root.join("README.md");
    let content = std::fs::read_to_string(&path).map_err(|e| format!("cannot read it: {e}"))?;
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

    let bullet = format!("- `crates/{hal}-{family}` — the {family} backend");
    if lines.iter().any(|l| l.trim() == bullet) {
        return Ok(()); // already described: a retry must not duplicate the list
    }
    let Some(last_bullet) = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("- `crates/"))
    else {
        return Err("its crate list was not found".to_string());
    };
    lines.insert(last_bullet + 1, bullet);

    // The closing fence of the last ```sh block: insert before it, so the block stays one block.
    let block = crate::build::embedded_hal_scaffold::readme_family_block(hal, family, spec);
    let Some(close) = lines
        .iter()
        .rposition(|l| l.trim() == "```")
        .filter(|close| lines[..*close].iter().any(|l| l.trim() == "```sh"))
    else {
        return Err("its build-command block was not found".to_string());
    };
    let mut inserted: Vec<String> = vec![String::new()];
    inserted.extend(block.lines().map(str::to_string));
    lines.splice(close..close, inserted);

    let mut out = lines.join("\n");
    out.push('\n');
    std::fs::write(&path, out).map_err(|e| format!("cannot write it: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scaffolded project **and** the registry it was scaffolded from.
    ///
    /// `Platform::from_registry` reads the process-global `SPIRE_PLATFORM_DIR`, and cargo runs these
    /// tests in parallel, so the registry is a fixture: the crate-wide lock is held for the test's
    /// duration and the variable is restored on drop. Without that these tests pass one at a time and
    /// fail together, which is exactly how it was found.
    struct Fixture {
        project: tempfile::TempDir,
        _registry: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let lock = crate::PLATFORM_DIR_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = std::env::var("SPIRE_PLATFORM_DIR").ok();
            let registry = tempfile::tempdir().expect("registry dir");
            for (file, entry) in [
                (
                    "rp2040.yaml",
                    "id: rp2040\nname: Raspberry Pi Pico\nos: rp2040\nfamily: rp2040\n\
                     library_hints: Cortex-M0+.\n\
                     architecture:\n  cpu_family: arm\n  cpu: rp2040\n  endian: little\n  \
                     target_triple: thumbv6m-none-eabi\n\
                     rust:\n  target: thumbv6m-none-eabi\n  idf_target: RP2040\n  flash: picotool\n",
                ),
                (
                    "esp32c6.yaml",
                    "id: esp32c6\nname: ESP32-C6\nos: esp-idf\nfamily: esp32\n\
                     library_hints: RISC-V RV32IMAC via esp-idf-hal.\n\
                     architecture:\n  cpu_family: riscv\n  cpu: esp32c6\n  endian: little\n  \
                     target_triple: riscv32imac-esp-espidf\n\
                     rust:\n  target: riscv32imac-esp-espidf\n  idf_target: esp32c6\n  flash: espflash\n",
                ),
                (
                    "rpi5.yaml",
                    "id: rpi5\nname: Raspberry Pi 5\nos: linux\n\
                     library_hints: A host Linux board — no backend crate to fill.\n\
                     architecture:\n  cpu_family: arm\n  cpu: cortex-a76\n  endian: little\n  \
                     target_triple: aarch64-unknown-linux-gnu\n",
                ),
                (
                    "esp32s3.yaml",
                    "id: esp32s3\nname: ESP32-S3\nos: esp-idf\nfamily: esp32\n\
                     library_hints: Xtensa LX7 via esp-idf-hal.\n\
                     architecture:\n  cpu_family: xtensa\n  cpu: esp32s3\n  endian: little\n  \
                     target_triple: xtensa-esp32s3-espidf\n\
                     rust:\n  target: xtensa-esp32s3-espidf\n  idf_target: esp32s3\n  flash: espflash\n",
                ),
            ] {
                std::fs::write(registry.path().join(file), entry).expect("registry entry");
            }
            std::env::set_var("SPIRE_PLATFORM_DIR", registry.path());

            // Scaffolded by the real emitter, so the fixture cannot drift from what users get.
            let out = crate::build::embedded_hal_scaffold::embedded_hal_scaffold(
                name,
                &["rp2040".to_string()],
            )
            .expect("the scaffold knows rp2040");
            let project = tempfile::tempdir().expect("project dir");
            for file in &out.files {
                let target = project.path().join(&file.path);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(&target, &file.content).unwrap();
            }

            Self {
                project,
                _registry: registry,
                _lock: lock,
                previous,
            }
        }

        fn root(&self) -> &Path {
            self.project.path()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            match &self.previous {
                Some(prev) => std::env::set_var("SPIRE_PLATFORM_DIR", prev),
                None => std::env::remove_var("SPIRE_PLATFORM_DIR"),
            }
        }
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).expect("the file was written")
    }

    #[test]
    fn a_contract_is_validated_against_what_the_measure_needs() {
        // The scaffold's contract crate has no authored traits to validate — they are `embedded-hal`'s
        // — so the validation is exercised on a trait of the kind a project *would* author: narrower
        // than the ecosystem's, one required method, and nothing else in the file.
        let f = Fixture::new("weather");
        let sensor = "//! A sensor on this family's bus.\n\
                      pub trait Sensor {\n    fn read_celsius(&mut self) -> i32;\n}\n";
        let ok = validate_contract(sensor).expect("a project-authored contract validates");
        assert_eq!(ok["valid"], json!(true));
        assert_eq!(ok["traits"][0]["trait"], json!("Sensor"));
        assert_eq!(ok["traits"][0]["methods"][0], json!("read_celsius"));
        // And the scaffolded crate really has none, which is the shape this change introduced.
        let scaffolded = read(f.root(), "crates/weather-hal/src/lib.rs");
        assert!(
            scaffolded.contains("pub use embedded_hal;"),
            "the seam re-exports the traits instead of declaring them: {scaffolded}"
        );

        // A trait whose methods are all defaulted needs no implementation, so the measure skips it:
        // accepting it would hand back a contract that behaves as if it were absent.
        let defaulted =
            "pub trait Led {\n    fn set(&mut self, on: bool) {\n        let _ = on;\n    }\n}\n";
        let err = validate_contract(defaulted).expect_err("nothing to implement");
        assert!(err.contains("only defaulted methods"), "{err}");

        // An implementation in an interface file would be reported as every board's.
        let implemented = "pub trait Led {\n    fn set(&mut self, on: bool);\n}\n\
                           impl Led for Noop {\n    fn set(&mut self, _on: bool) {}\n}\n";
        let err = validate_contract(implemented).expect_err("no impls in a contract");
        assert!(err.contains("implements `Led`"), "{err}");

        // Two traits with one name are ambiguous at the `use hal::{…}` every backend writes.
        let twice = "pub trait Led {\n    fn set(&mut self, on: bool);\n}\n\
                     pub trait Led {\n    fn get(&self) -> bool;\n}\n";
        let err = validate_contract(twice).expect_err("one name, once");
        assert!(err.contains("declared twice"), "{err}");

        // A syntax error names the line, because that is the only actionable part of it.
        let broken = "pub trait Led {\n    fn set(&mut self, on: bool)\n}\n";
        let err = validate_contract(broken).expect_err("does not parse");
        assert!(err.contains("does not parse"), "{err}");

        let err = validate_contract("   ").expect_err("empty");
        assert!(err.contains("empty"), "{err}");
    }

    /// The write is only useful if the measure can see the result: `hal/mod.rs` declares and
    /// re-exports it, and `embedded_hal_layout` — the function the drift measure uses — then reports
    /// the interface.
    #[test]
    fn a_written_contract_is_declared_and_then_visible_to_the_measure() {
        let f = Fixture::new("weather");
        let root = f.root();
        let before = crate::build::hal_rust_contract::embedded_hal_layout(root);
        assert_eq!(
            before.0.len(),
            0,
            "a scaffolded contract authors no traits of its own — the peripheral ones are \
             `embedded-hal`'s, and what a backend owes is its board"
        );

        let source = "//! A sensor on this family's bus.\n\
                      pub trait Sensor {\n    fn read_celsius(&mut self) -> i32;\n}\n";
        let out = write_contract(root, "sensor.rs", source).expect("written");
        assert_eq!(out["valid"], json!(true));
        assert_eq!(out["wired"], json!(true));
        assert_eq!(out["traits"][0]["trait"], json!("Sensor"));

        let module = read(root, "crates/weather-hal/src/hal/mod.rs");
        assert!(module.contains("pub mod sensor;"), "{module}");
        assert!(module.contains("pub use sensor::Sensor;"), "{module}");
        // The two groups stay groups: every declaration precedes every re-export. That is the
        // invariant the insertion rule exists to keep as more traits arrive, and it is checkable with
        // one trait — there is no `led::Led` to compare against any more, because the peripheral
        // traits are `embedded-hal`'s.
        let last_mod = module.rfind("pub mod ").expect("a declaration");
        let first_use = module.find("pub use ").expect("a re-export");
        assert!(
            last_mod < first_use,
            "the module list stays one list:\n{module}"
        );

        let after = crate::build::hal_rust_contract::embedded_hal_layout(root);
        assert_eq!(
            after.0.get("sensor"),
            Some(&vec![(
                "Sensor".to_string(),
                vec!["read_celsius".to_string()]
            )]),
            "the measure must see what was written"
        );

        // Writing the same content again is a no-op, so a retry does not have to know whether the
        // first attempt landed.
        let again = write_contract(root, "sensor", source).expect("idempotent");
        assert_eq!(again["unchanged"], json!(true));
        assert_eq!(again["wired"], json!(false), "nothing left to wire");
        assert_eq!(read(root, "crates/weather-hal/src/hal/mod.rs"), module);

        // Different content for a contract other crates compile against is refused, and the file on
        // disk is untouched.
        let err = write_contract(
            root,
            "sensor.rs",
            "pub trait Sensor {\n    fn read(&mut self);\n}\n",
        )
        .expect_err("no silent contract replacement");
        assert!(err.contains("already exists and differs"), "{err}");
        assert_eq!(read(root, "crates/weather-hal/src/hal/sensor.rs"), source);

        // The module list itself is not a contract, and a stem that is not an identifier would name
        // a file nothing can declare.
        let err = write_contract(root, "mod.rs", source).expect_err("mod.rs is the list");
        assert!(err.contains("module list"), "{err}");
        let err = write_contract(root, "2sensor.rs", source).expect_err("not an identifier");
        assert!(err.contains("module name"), "{err}");
    }

    /// A second family arrives as the crate the scaffold would have produced, *and* as a workspace
    /// member — the line without which cargo ignores it and the measure never sees it.
    #[test]
    fn a_second_platform_becomes_a_backend_crate_and_a_workspace_member() {
        let f = Fixture::new("weather");
        let root = f.root();
        assert!(!root.join("crates/weather-hal-esp32").exists());

        let added = add_platform(root, "esp32c6").expect("esp32c6 is a known family");
        assert_eq!(added["family"], json!("esp32"));
        assert_eq!(added["crate"], json!("weather-hal-esp32"));
        assert_eq!(added["vendor_crate"], json!("esp-idf-hal"));

        let manifest = read(root, "Cargo.toml");
        assert!(
            manifest.contains("\"crates/weather-hal-esp32\","),
            "the member line is what makes the crate exist:\n{manifest}"
        );
        assert!(
            manifest.contains("default-members = [\"crates/weather-hal\", \"crates/weather-hal-std\"]"),
            "the new backend must not become a default member (a host test cannot build it):\n{manifest}"
        );

        // The crate is the scaffold's emitter output: the same pinned deps and the same
        // `unimplemented!()` stubs — one code path, not a second copy of it.
        let backend = read(root, "crates/weather-hal-esp32/Cargo.toml");
        assert!(backend.contains("esp-idf-hal = \"0.47\""), "{backend}");
        let source = read(root, "crates/weather-hal-esp32/src/lib.rs");
        assert!(
            source.contains("unimplemented!(\"Board::led\")"),
            "{source}"
        );

        // And the measure sees a second backend family, which is what makes the new board show up
        // as work to do rather than as nothing at all.
        let (_, backends) = crate::build::hal_rust_contract::embedded_hal_layout(root);
        assert!(backends.contains_key("rp2040"), "{backends:?}");
        assert!(backends.contains_key("esp32"), "{backends:?}");

        // The README grows with the project: the crate list gains the family, and the *same*
        // per-family build block the scaffold renders appears in the build section — so the one
        // document a user reads by hand is not left describing only the boards chosen at creation.
        let readme = read(root, "README.md");
        assert!(
            readme.contains("- `crates/weather-hal-rp2040` — the rp2040 backend"),
            "the board it was created with is still listed:\n{readme}"
        );
        assert!(
            readme.contains("- `crates/weather-hal-esp32` — the esp32 backend"),
            "the added board is listed:\n{readme}"
        );
        assert!(
            readme.contains("cargo build --target thumbv6m-none-eabi -p weather-hal-rp2040"),
            "its own build command:\n{readme}"
        );
        assert!(
            readme.contains("MCU=esp32 cargo build --target xtensa-esp32-espidf"),
            "and the new family's, inside the same fence:\n{readme}"
        );
        assert!(
            readme.contains("MCU=esp32 cargo build --target xtensa-esp32-espidf \\\n    -Zbuild-std=std,panic_abort -p weather-hal-esp32"),
            "with the crate line actually completed:\n{readme}"
        );
        assert!(
            !readme.contains("__CRATE__") && !readme.contains("__BUILDS__"),
            "no template placeholder survives into the user's README:\n{readme}"
        );
        assert_eq!(
            readme.matches("MCU=esp32 cargo build").count(),
            1,
            "a retry must not duplicate the block:\n{readme}"
        );

        // A family that is already there is refused, so a second call cannot fork the crate.
        let err = add_platform(root, "esp32s3").expect_err("same family, already present");
        assert!(err.contains("already has a backend crate"), "{err}");

        // So is a platform that is not a board — a Linux target has no backend to fill.
        let err = add_platform(root, "rpi5").expect_err("not embedded");
        assert!(err.contains("not an embedded platform"), "{err}");

        let err = add_platform(root, "no-such-board").expect_err("unknown id");
        assert!(err.contains("unknown platform"), "{err}");
    }

    /// A project whose README is missing or unrecognizable is **reported**, not guessed at: the tool
    /// still adds the crate, and the note says exactly what it could not do.
    #[test]
    fn a_readme_that_cannot_be_extended_is_reported_rather_than_guessed_at() {
        let f = Fixture::new("weather");
        let root = f.root();
        std::fs::remove_file(root.join("README.md")).unwrap();
        let added = add_platform(root, "esp32c6").expect("the crate is still added");
        let note = added["note"].as_str().unwrap_or_default();
        assert!(
            note.contains("README.md was not updated"),
            "the note must say what did not happen: {added}"
        );
        assert!(
            note.contains("cannot read it"),
            "and why, in the README's own terms: {added}"
        );
        assert!(
            root.join("crates/weather-hal-esp32/src/lib.rs").exists(),
            "the crate itself is unaffected"
        );
    }

    /// A project with no contract crate is refused by name rather than half-written: every write
    /// here is relative to that crate.
    #[test]
    fn a_project_without_a_contract_crate_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("crates/weather-std")).unwrap();
        let err = add_platform(tmp.path(), "rp2040").expect_err("no contract crate");
        assert!(err.contains("no contract crate"), "{err}");
        let err = write_contract(
            tmp.path(),
            "sensor.rs",
            "pub trait Sensor {\n    fn read(&mut self);\n}\n",
        )
        .expect_err("no contract crate");
        assert!(err.contains("no contract crate"), "{err}");
    }
}
