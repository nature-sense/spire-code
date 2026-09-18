// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! The **Rust** HAL fill: what a scaffolded backend still owes, and the two halves that act on it —
//! [`plan`] (read-only, the reviewed artefact) and [`apply`] (one model call per item, gated before
//! anything is written).
//!
//! The plan differs from the C++ one in one structural way — a C++ implementation is a file per
//! interface, while a Rust backend implements *every* contract trait (the traits are modules, not
//! classes) and may hold them in several files. The unit of work is therefore the **file**: an item
//! names one backend file, what it still owes, and the prompt that asks for it.
//!
//! The prompt is the point of the plan. Everything an implementation needs and cannot discover from
//! the file itself is injected here — the vendor crate, the runtime (std or not) and the platform's
//! own `library_hints` — because the failure mode of a generated backend is not a missing method, it
//! is a method written against the wrong API for that chip.
//!
//! Pending work comes from [`crate::build::hal_rust_contract::rust_platform_coverage_map`], the
//! same measure the UI's HAL indicators read. A scaffolded backend reports as pending even though
//! its stub declares every method: the bodies are `unimplemented!()`, which the measure counts as
//! a placeholder rather than an implementation.

use crate::build::embedded_hal_scaffold::{family_spec, FamilySpec};
use crate::platform::Platform;
use serde_json::json;
use spire_core::subsystems::llm::llm::{LlmMessage, LlmModelRole};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

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
    /// Where that source came from, for the prompt's block header: a contract trait is
    /// `crates/<hal>/src/hal/led.rs`, and a board is the contract's `lib.rs` — the seam that
    /// re-exports the traits a constructor has to return.
    source_label: String,
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
    /// The backend manifest's `[dependencies]` block, verbatim.
    ///
    /// Verbatim because it is what the compiler will enforce: the model may use these crates and no
    /// others, and telling it the *list* beats telling it a count (the count differs per family —
    /// esp-idf-hal re-exports the sys crate, rp2040-hal does not re-export `embedded-hal`/`nb`).
    dependencies: &'a str,
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

/// One pending trait of a plan item, as the item carries it back in.
///
/// The plan is JSON on its way out and back (the UI reviews it), so the apply side reads what it
/// wrote rather than sharing a struct: an item that has been through a user's editor must be
/// validated for the same reason a request is.
struct PlannedTrait {
    interface: String,
    trait_name: String,
    methods: Vec<String>,
}

fn planned_traits(item: &serde_json::Value) -> Vec<PlannedTrait> {
    item.get("pending")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    Some(PlannedTrait {
                        interface: entry.get("interface")?.as_str()?.to_string(),
                        trait_name: entry.get("trait")?.as_str()?.to_string(),
                        methods: entry
                            .get("methods")
                            .and_then(|m| m.as_array())
                            .map(|m| {
                                m.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `impl` lines of an answer, for a refusal that has to be actionable.
///
/// A gate refusal is the only thing a reader gets — the answer itself is dropped — so a reason like
/// "no `impl Led for …`" is a dead end: was the impl renamed, turned into an inherent one, written
/// under another path? Three lines answer that without dumping a file into a UI (or a log) that had
/// no way to show one.
fn impl_lines(source: &str) -> String {
    let impls: Vec<&str> = source
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("impl"))
        .take(3)
        .collect();
    if impls.is_empty() {
        return String::new();
    }
    format!(" (the answer's impls: {})", impls.join(" | "))
}

/// The gate between a model's answer and the file it would become.
///
/// Both halves are what the plan promised: every pending item is implemented, and no placeholder
/// is left behind. A generation failing either is reported rather than written — a file that parses
/// but still says `unimplemented!()` looks finished to every later reader, and the whole point of
/// the placeholder convention is that it does not.
///
/// "Pending item" covers both forms a backend can owe: a **contract trait** (`impl Trait for Type`)
/// and the **board's constructors** (inherent methods on `Board`). They are read by different
/// extractors, which is why both are consulted here and below.
fn verify_generated(source: &str, pending: &[PlannedTrait]) -> Result<(), String> {
    let traits = crate::build::hal_rust_contract::extract_impl_methods_rust(source);
    let inherent = crate::build::hal_rust_contract::extract_inherent_methods_rust(source);
    let stub_traits = crate::build::hal_rust_contract::placeholder_impls_rust(source);
    let stub_methods = crate::build::hal_rust_contract::placeholder_methods_rust(source);
    let declared = traits.iter().chain(inherent.iter());

    for owed in pending {
        let is_declared = traits
            .iter()
            .chain(inherent.iter())
            .any(|(name, _)| name.eq_ignore_ascii_case(&owed.trait_name));
        if !is_declared {
            return Err(format!(
                "the generated file declares no `{}` — it was in the prompt and must stay{}",
                owed.trait_name,
                impl_lines(source)
            ));
        }
        let provided: Vec<String> = declared
            .clone()
            .filter(|(name, _)| name.eq_ignore_ascii_case(&owed.trait_name))
            .flat_map(|(_, methods)| methods.clone())
            .collect();
        let missing: Vec<&str> = owed
            .methods
            .iter()
            .filter(|method| !provided.contains(method))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "`{}` still has no {}",
                owed.trait_name,
                missing.join(", ")
            ));
        }
        // A placeholder is one of two things: a whole `impl Trait for Type` block whose body is
        // still `unimplemented!()`, or an individual method whose body is (a board constructor, or
        // a `todo!()`). Either one means the item is not done.
        let stub_trait = stub_traits
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&owed.trait_name));
        let stub_method = owed
            .methods
            .iter()
            .any(|method| stub_methods.iter().any(|name| name == method));
        if stub_trait || stub_method {
            return Err(format!(
                "`{}` still contains a placeholder (`unimplemented!()`/`todo!()`) — it is to be \
                 replaced, not kept",
                owed.trait_name
            ));
        }
    }
    Ok(())
}

/// Generate-and-write the plan: one model call and one gated write per item.
///
/// `plan` is what [`plan`] returned — the wrapper object or its `plan` array, since it has been
/// through a UI that may hand back either. Without an LLM this refuses by name and writes nothing:
/// the plan is a prompt, and a prompt with no model behind it is not an implementation.
pub(crate) async fn apply(
    root: &Path,
    plan: &serde_json::Value,
    llm_tx: &Option<mpsc::Sender<LlmMessage>>,
) -> serde_json::Value {
    let Some(llm_tx) = llm_tx.as_ref() else {
        return json!({
            "error": "embedded_hal_fill_apply: LLM unavailable — the build manager is not connected \
                      to the LLM service (wiring, not a missing API key)"
        });
    };
    let items: Vec<serde_json::Value> = match plan {
        serde_json::Value::Array(items) => items.clone(),
        other => other
            .get("plan")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default(),
    };
    if items.is_empty() {
        return json!({
            "error": "embedded_hal_fill_apply: the plan has no items — run embedded_hal_fill_plan first"
        });
    }

    let mut applied: Vec<serde_json::Value> = Vec::new();
    let mut failures: Vec<serde_json::Value> = Vec::new();
    for item in &items {
        match apply_item(root, item, llm_tx).await {
            Ok(entry) => applied.push(entry),
            Err((file, reason)) => failures.push(json!({ "file": file, "reason": reason })),
        }
    }

    json!({
        "applied": applied,
        "failures": failures,
        "note": "Only backend files under `crates/` are written; the contract is never edited. \
                 Each write is followed by a re-measure, so `interfaces_still_pending` is the \
                 honest verdict rather than a claim.",
    })
}

/// Where an item will be written, what it owes, and the prompt it was planned from — the three
/// things both applying and repairing need, checked once.
fn item_target(
    root: &Path,
    item: &serde_json::Value,
) -> Result<(PathBuf, Vec<PlannedTrait>, String), (String, String)> {
    let file = item
        .get("file")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if file.is_empty() {
        return Err((file, "the plan item has no `file`".to_string()));
    }
    let path = PathBuf::from(&file);
    // The path comes from the plan and the plan came from disk — but this is the one value that
    // decides where a model's output lands, so it is checked rather than trusted.
    let inside = path
        .strip_prefix(root)
        .is_ok_and(|rel| rel.starts_with("crates") && path.is_file());
    if !inside {
        return Err((
            file,
            "refused: not an existing file under `crates/` in this project".to_string(),
        ));
    }
    let pending = planned_traits(item);
    if pending.is_empty() {
        return Err((
            file,
            "the plan item owes no trait — nothing to generate".to_string(),
        ));
    }
    let prompt = item
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if prompt.trim().is_empty() {
        return Err((file, "the plan item carries no prompt".to_string()));
    }
    Ok((path, pending, prompt.to_string()))
}

/// One item: check the path, ask the model, gate the answer, write, re-measure.
///
/// The error carries the file, so a failure names where it happened even when the reason is about
/// the model's answer rather than about the file.
async fn apply_item(
    root: &Path,
    item: &serde_json::Value,
    llm_tx: &mpsc::Sender<LlmMessage>,
) -> Result<serde_json::Value, (String, String)> {
    let (path, pending, prompt) = item_target(root, item)?;
    let file = path.to_string_lossy().to_string();

    let source = generate(llm_tx, &prompt, &pending)
        .await
        .map_err(|e| (file.clone(), e))?;
    std::fs::write(&path, &source).map_err(|e| (file.clone(), format!("write failed: {e}")))?;

    // Re-measure instead of declaring success: the same map the UI reads decides whether the item
    // is done, so a write that did not satisfy the contract shows up as still pending.
    let family = item
        .get("family")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let coverage = crate::build::hal_rust_contract::rust_platform_coverage_map(root);
    let done = |interface: &str| {
        coverage
            .get(family)
            .and_then(|ifaces| ifaces.get(interface))
            .map(|cov| cov.implemented)
            .unwrap_or(false)
    };
    let (done_ifaces, still_pending): (Vec<&str>, Vec<&str>) = pending
        .iter()
        .map(|owed| owed.interface.as_str())
        .partition(|interface| done(interface));

    Ok(json!({
        "family": family,
        "file": file,
        "interfaces_done": done_ifaces,
        "interfaces_still_pending": still_pending,
    }))
}

/// The file inside a model's answer: the first fenced block if there is one, else the answer.
///
/// Not `strip_code_fences`: that one handles a fence only at the very start of the answer and only
/// a `cpp` tag. That is enough where its verdict is advisory, but not here — a preamble ("Sure,
/// here it is:") or a ```rust tag would leave the fences in, and the syntax gate would then refuse
/// every answer. The fence is a presentation detail; the file is what is between the fences.
fn code_block(text: &str) -> String {
    let trimmed = text.trim();
    let Some(start) = trimmed.find("```") else {
        return trimmed.to_string();
    };
    // Skip the opening fence's own line, whose tag may be `rust`, `rs` or nothing at all.
    let after_open = &trimmed[start + 3..];
    let body_start = after_open
        .find('\n')
        .map(|i| i + 1)
        .unwrap_or(after_open.len());
    let body = &after_open[body_start..];
    match body.find("```") {
        Some(end) => body[..end].trim().to_string(),
        // An unterminated fence: the rest of the answer is the file. A truncated answer is the
        // case that happens in, and the gate still judges whatever arrived.
        None => body.trim().to_string(),
    }
}

/// Ask the model for the file, retrying what the C++ path retries — once on truncation, twice on a
/// parse error — **and once on a gate refusal**.
///
/// The gate retry is the one a live run asked for: a model that returns only the methods it changed
/// (a plausible reading of "write the pending methods") or that keeps one `unimplemented!()` gets
/// told *which* trait and method failed, in the words of the gate, instead of the run ending with a
/// refusal the user cannot act on. Two such answers in a row still end as a refusal — the gate is
/// the authority, and a retry loop that kept trying would be a rate limit, not a fix.
async fn generate(
    llm_tx: &mpsc::Sender<LlmMessage>,
    prompt: &str,
    pending: &[PlannedTrait],
) -> Result<String, String> {
    let mut prompt = prompt.to_string();
    for attempt in 0..3u32 {
        let (reply_to, reply) = oneshot::channel();
        if llm_tx
            .send(LlmMessage::Complete {
                prompt: prompt.clone(),
                role: LlmModelRole::Coding,
                reply_to,
            })
            .await
            .is_err()
        {
            return Err("LLM channel closed".to_string());
        }
        let text = match reply.await {
            Ok(Ok(text)) => text,
            Ok(Err(e)) => {
                let message = e.to_string();
                if message.contains("truncated") && attempt == 0 {
                    prompt.push_str(
                        "\n\nYour previous response was truncated. Return the file again, more \
                         concisely: no commentary, minimal comments, compact code.",
                    );
                    continue;
                }
                return Err(format!("LLM failed: {message}"));
            }
            Err(_) => return Err("LLM reply lost".to_string()),
        };

        let source = code_block(&text);
        let check = crate::build::hal_rust_contract::rust_syntax_check(&source);
        if check.ok {
            // Parses — now the gate, which is the other half of "is this usable": an answer can be
            // valid Rust and still not be what was asked for.
            match verify_generated(&source, pending) {
                Ok(()) => return Ok(source),
                Err(reason) => {
                    if attempt < 2 {
                        prompt.push_str(&format!(
                            "\n\nYour previous answer was refused: {reason}\nReturn the COMPLETE \
                             file again, every pending method implemented and no `unimplemented!()` \
                             left (no fences)."
                        ));
                        continue;
                    }
                    return Err(reason);
                }
            }
        }
        let hints: Vec<String> = check
            .errors
            .iter()
            .map(|e| format!("line {} col {}: {} ({})", e.line, e.col, e.kind, e.context))
            .collect();
        if attempt < 2 {
            prompt.push_str(&format!(
                "\n\nThe file did not parse as Rust:\n{}\nFix them and return the complete file \
                 again (no fences).",
                hints.join("; ")
            ));
        } else {
            return Err(format!(
                "the generated file does not parse as Rust: {}",
                hints.join("; ")
            ));
        }
    }
    Err("no answer from the model".to_string())
}

/// A backend manifest's `[dependencies]` block, verbatim, or an empty string when it cannot be read.
///
/// Verbatim on purpose: it is the list the compiler enforces, and paraphrasing it — or counting it —
/// is how a prompt tells a model to use a crate that is not there. Empty is not an error: a manifest
/// always has the block, and if it does not, the prompt simply says nothing about dependencies
/// rather than inventing a list.
fn backend_dependencies(crate_dir: &Path) -> String {
    let Ok(manifest) = std::fs::read_to_string(crate_dir.join("Cargo.toml")) else {
        return String::new();
    };
    let mut out = String::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == "[dependencies]";
            if inside {
                out.push_str("[dependencies]\n");
            }
            continue;
        }
        if inside && !trimmed.is_empty() && !trimmed.starts_with('#') {
            out.push_str("  ");
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out
}

/// Repair a written backend that does not compile: one more model call, with the compiler's own
/// errors in the prompt, then the same gate as any other answer.
///
/// This is the loop a live run pointed at. The gate above is **structural** — it decides whether an
/// answer is the file it claims to be — and only `cargo` can say whether that file builds, so the
/// compiler's words are the one piece of feedback that makes a wrong-but-plausible answer
/// recoverable. The errors are passed through verbatim (truncated): paraphrase would lose the line
/// numbers and the type names, which are exactly what the model needs.
///
/// One round only. A second failed answer is reported, not retried again — the caller decides
/// whether to spend another — and the file is left as the compiler saw it, so a repair that fails
/// never leaves the backend in a state nobody has built.
pub(crate) async fn repair(
    root: &Path,
    item: &serde_json::Value,
    errors: &str,
    llm_tx: &mpsc::Sender<LlmMessage>,
) -> Result<String, String> {
    let (path, pending, prompt) = item_target(root, item)
        .map_err(|(file, reason)| format!("cannot repair {file}: {reason}"))?;

    // Enough of the compiler's output to act on, and no more: a `cargo` failure on one crate is
    // usually a handful of errors, but a broken dependency graph can produce thousands of lines.
    let mut kept: String = errors.lines().take(80).collect::<Vec<_>>().join("\n");
    if errors.lines().count() > 80 {
        kept.push_str("\n… (more errors omitted)");
    }

    let repair_prompt = format!(
        "{prompt}\n\nThe file you wrote does not compile. The compiler said:\n\n{kept}\n\n\
         Return the COMPLETE corrected file — every pending method implemented, using only the \
         crates in the manifest above and the API as the errors describe it (no fences)."
    );
    let source = generate(llm_tx, &repair_prompt, &pending).await?;
    std::fs::write(&path, &source).map_err(|e| format!("write failed: {e}"))?;
    Ok(path.to_string_lossy().to_string())
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
            // Both impl forms: a contract trait is `impl Trait for Type`, while the board's
            // constructors are inherent (`impl Board`) — and the board is what a backend owes now.
            let declares = crate::build::hal_rust_contract::extract_impl_methods_rust(&content)
                .iter()
                .chain(
                    crate::build::hal_rust_contract::extract_inherent_methods_rust(&content).iter(),
                )
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

    p.push_str("THE CONTRACT — implement it, never edit it\n");
    for pending in f.pending {
        p.push_str(&format!(
            "--- {} ---\n{}\n",
            pending.source_label,
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
        "1. Return the COMPLETE file — every line it should have on disk — with only the pending\n\
         \x20  method bodies written. Everything else (the structs, their field types, the executor\n\
         \x20  block, the doc comments) stays exactly as it is. An answer containing only the\n\
         \x20  methods you changed is not a file and will be refused.\n\
         2. The `unimplemented!()` bodies must be gone. Do not leave one behind and do not add a\n\
         \x20  new one: the workspace measures placeholders, so an `unimplemented!()` left in place\n\
         \x20  reports this backend as unfinished.\n",
    );
    p.push_str(&format!(
        "3. Build every hardware access on the crates this backend already depends on:\n\n{}\n\
         \x20  Add no dependency and reach for no other API: the build will fail on anything else.\n\
         \x20  If a HAL method needs a trait in scope, that trait's crate is in the list above.\n",
        f.dependencies.trim_end()
    ));
    p.push_str(
        "4. The peripheral traits are `embedded-hal`'s, re-exported by the contract crate — return\n\
         \x20  those (`OutputPin`, `DelayNs`, …) from the constructors, and keep every vendor type\n\
         \x20  inside this crate. Do not add a trait of your own: a driver crate could not use it.\n",
    );
    if !f.spec.uses_std_executor {
        p.push_str(
            "5. This crate is `no_std`: no `std::`, no `println!`, no heap unless the vendor API\n\
             \x20  hands you an allocation it owns.\n",
        );
    }
    if f.pending.iter().any(|p| p.status == "none") {
        p.push_str(
            "6. A constructor the stub does not declare yet needs the right **signature** for this\n\
             \x20  vendor: take its pin type and return its own driver (it already implements the\n\
             \x20  `embedded-hal` trait). Name no type of ours in a signature.\n",
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
            // A contract trait lives in the contract crate (`src/hal/<stem>.rs`); the **board** does
            // not — it is the seam's own shape, so its source is the contract's `lib.rs` (the
            // re-export a constructor has to return a trait *from*) and its methods are the
            // constructors every family owes.
            let is_board = interface == crate::build::hal_rust_contract::BOARD_INTERFACE;
            let source_path = if is_board {
                root.join("crates")
                    .join(hal_crate)
                    .join("src")
                    .join("lib.rs")
            } else {
                root.join("crates")
                    .join(hal_crate)
                    .join("src")
                    .join("hal")
                    .join(format!("{interface}.rs"))
            };
            let source = std::fs::read_to_string(&source_path).unwrap_or_default();
            let source_label = relative(root, &source_path);
            let (trait_name, required): (String, Vec<String>) = if is_board {
                (
                    crate::build::hal_rust_contract::BOARD_TYPE.to_string(),
                    crate::build::hal_rust_contract::BOARD_METHODS
                        .iter()
                        .map(|m| (*m).to_string())
                        .collect(),
                )
            } else {
                let traits = crate::build::hal_rust_contract::required_trait_methods_rust(&source);
                let name = traits
                    .first()
                    .map(|(name, _)| name.clone())
                    .unwrap_or_else(|| interface.clone());
                let methods = traits
                    .iter()
                    .flat_map(|(_, methods)| methods.clone())
                    .collect();
                (name, methods)
            };
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
                source_label,
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
        // The dependencies the compiler will enforce, read from the manifest the scaffold wrote —
        // so the prompt cannot drift from it, and a family that needs three crates says three.
        let dependencies = match src_dir.parent() {
            Some(crate_dir) => backend_dependencies(crate_dir),
            None => String::new(),
        };

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
                dependencies: &dependencies,
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

    /// A project that **authors a trait of its own** — the path a project takes when one of the
    /// ecosystem's traits is not narrow enough (a driver's own abstraction, say). It is a supported
    /// path, and the measure still reads it, so it keeps its own fixture.
    ///
    /// For the shape the *scaffold* emits, see [`scaffolded_project`].
    fn project(root: &Path, led_body: &str) {
        std::fs::create_dir_all(root.join("crates/demo-hal/src/hal")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/hal/led.rs"),
            "/// A single binary output.\n\
             pub trait Led {\n    /// Required.\n    fn set(&mut self, on: bool);\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/demo-hal-esp32/src")).unwrap();
        // The manifest matters: it is what the prompt shows the model as the crates it may use.
        std::fs::write(
            root.join("crates/demo-hal-esp32/Cargo.toml"),
            "[package]\nname = \"demo-hal-esp32\"\n\n[dependencies]\n\
             demo-hal = { path = \"../demo-hal\" }\nesp-idf-hal = \"0.47\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/lib.rs"),
            format!(
                "use demo_hal::hal::Led;\n\npub struct GpioLed;\n\n\
                 impl Led for GpioLed {{\n    fn set(&mut self, _on: bool) {{\n{led_body}\n    }}\n}}\n"
            ),
        )
        .unwrap();
    }

    /// A project in the shape the **scaffold** emits: the contract crate is the seam (`embedded-hal`
    /// re-exported, no authored traits), and each backend is a `Board` whose constructors are
    /// `unimplemented!()` — returning a concrete stand-in so the project builds before the fill runs.
    fn scaffolded_project(root: &Path) {
        std::fs::create_dir_all(root.join("crates/demo-hal/src")).unwrap();
        std::fs::write(
            root.join("crates/demo-hal/src/lib.rs"),
            "//! The seam: the actor contract, plus `embedded-hal` re-exported.\n\n\
             pub mod actor;\n\npub use embedded_hal;\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("crates/demo-hal-esp32/src")).unwrap();
        // The manifest matters: it is what the prompt shows the model as the crates it may use, and
        // it is where the version pin the compiler will enforce comes from.
        std::fs::write(
            root.join("crates/demo-hal-esp32/Cargo.toml"),
            "[package]\nname = \"demo-hal-esp32\"\n\n[dependencies]\n\
             demo-hal = { path = \"../demo-hal\" }\nesp-idf-hal = \"0.47\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("crates/demo-hal-esp32/src/lib.rs"),
            "use demo_hal::embedded_hal::delay::DelayNs;\n\
             use demo_hal::embedded_hal::digital::{ErrorType, OutputPin};\n\n\
             pub struct Board;\n\n\
             pub struct UnimplementedLed;\n\
             impl ErrorType for UnimplementedLed {\n    type Error = core::convert::Infallible;\n}\n\
             impl OutputPin for UnimplementedLed {\n    \
             fn set_high(&mut self) -> Result<(), Self::Error> {\n        \
             unimplemented!(\"Board::led has not been written yet\")\n    }\n    \
             fn set_low(&mut self) -> Result<(), Self::Error> {\n        \
             unimplemented!(\"Board::led has not been written yet\")\n    }\n}\n\n\
             pub struct UnimplementedDelay;\n\
             impl DelayNs for UnimplementedDelay {\n    fn delay_ns(&mut self, _ns: u32) {\n        \
             unimplemented!(\"Board::delay has not been written yet\")\n    }\n}\n\n\
             impl Board {\n    \
             pub fn led() -> UnimplementedLed {\n        unimplemented!(\"Board::led\")\n    }\n\n    \
             pub fn delay() -> UnimplementedDelay {\n        unimplemented!(\"Board::delay\")\n    }\n}\n",
        )
        .unwrap();
    }

    /// A backend split into modules — the shape the reference workspace uses, and the shape a
    /// filled backend tends towards — is planned against the file that holds each impl, not against
    /// a `lib.rs` that only re-exports.
    #[test]
    fn a_split_backend_names_the_file_that_holds_the_impl() {
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let reg = registry();
        let _env = crate::platform::PlatformDirGuard::set(reg.path());

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        scaffolded_project(root);

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

        // What a scaffolded backend owes is its **board**, under the interface key `board`, and the
        // prompt's contract block is the seam it has to return traits from — not a trait file, which
        // a scaffolded project does not have.
        let pending = item["pending"].as_array().unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0]["interface"], "board");
        assert_eq!(pending[0]["trait"], "Board");
        assert_eq!(pending[0]["status"], "stub");
        assert_eq!(
            pending[0]["methods"],
            serde_json::json!(["led", "delay"]),
            "a stub owes every constructor"
        );

        let prompt = item["prompt"].as_str().unwrap();
        assert!(
            prompt.contains("esp-idf-hal = \"0.47\""),
            "the dependencies the compiler will enforce, verbatim: {prompt}"
        );
        assert!(
            prompt.contains("demo-hal = { path = \"../demo-hal\" }"),
            "including the contract crate itself: {prompt}"
        );
        assert!(
            prompt.contains("crate    demo-hal-esp32"),
            "the crate being edited: {prompt}"
        );
        assert!(
            prompt.contains("TIMG0 is the delay"),
            "the platform's own hints: {prompt}"
        );
        assert!(
            prompt.contains("pub use embedded_hal;"),
            "the contract itself, not a paraphrase — the seam a constructor returns traits from: {prompt}"
        );
        assert!(
            prompt.contains("crates/demo-hal-esp32/src/lib.rs"),
            "the file to edit: {prompt}"
        );
        assert!(
            prompt.contains("board [stub]: Board — led, delay"),
            "what is pending: {prompt}"
        );
        assert!(
            prompt.contains("Return the COMPLETE file"),
            "the output contract — a whole file, not a patch of methods (the reading a live run \
             showed a model taking): {prompt}"
        );
        assert!(
            prompt.contains("The `unimplemented!()` bodies must be gone"),
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
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let _lock = crate::PLATFORM_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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

    /// The gate: what a model's answer must be before it becomes the file.
    ///
    /// These are the three ways a generation looks plausible and is not — a kept placeholder, a
    /// dropped impl, a method it forgot — and each is refused with a reason naming the trait.
    #[test]
    fn the_gate_refuses_a_generation_that_would_look_finished() {
        let owed = vec![PlannedTrait {
            interface: "board".to_string(),
            trait_name: "Board".to_string(),
            methods: vec!["led".to_string(), "delay".to_string()],
        }];
        let real = "use demo_hal::embedded_hal::delay::DelayNs;\n\
                    use demo_hal::embedded_hal::digital::OutputPin;\n\n\
                    pub struct Board;\n\n\
                    impl Board {\n    \
                    pub fn led() -> impl OutputPin {\n        BoardLed\n    }\n\n    \
                    pub fn delay() -> impl DelayNs {\n        BoardDelay\n    }\n}\n";
        assert_eq!(verify_generated(real, &owed), Ok(()), "a real body passes");

        let kept = real.replace("BoardLed", "unimplemented!(\"Board::led\")");
        let reason =
            verify_generated(&kept, &owed).expect_err("a kept placeholder must be refused");
        assert!(reason.contains("unimplemented!()"), "{reason}");
        assert!(
            reason.contains("Board"),
            "the refusal names the type: {reason}"
        );

        let dropped = "pub struct Board;\n";
        let reason = verify_generated(dropped, &owed).expect_err("a dropped impl must be refused");
        assert!(reason.contains("declares no `Board`"), "{reason}");

        let empty_impl = "pub struct Board;\nimpl Board {}\n";
        let reason =
            verify_generated(empty_impl, &owed).expect_err("a missing method must be refused");
        assert!(reason.contains("still has no led"), "{reason}");
    }

    /// Every pending trait is gated, not just the first: a file that satisfies `Led` and leaves
    /// `DelayMs` a placeholder is the partial case the flow must keep reporting.
    #[test]
    fn the_gate_covers_every_pending_trait() {
        let owed = vec![
            PlannedTrait {
                interface: "led".to_string(),
                trait_name: "Led".to_string(),
                methods: vec!["set".to_string()],
            },
            PlannedTrait {
                interface: "time".to_string(),
                trait_name: "DelayMs".to_string(),
                methods: vec!["delay_ms".to_string()],
            },
        ];
        let src = "impl Led for GpioLed {\n    fn set(&mut self, on: bool) {\n        let _ = on;\n    }\n}\n\n\
                   impl DelayMs for FamilyDelay {\n    fn delay_ms(&mut self, _ms: u32) {\n        unimplemented!(\"delay\")\n    }\n}\n";
        let reason = verify_generated(src, &owed).expect_err("the second trait is still pending");
        assert!(reason.contains("DelayMs"), "{reason}");
    }

    /// The answer's fences come off whatever the model says around them: a preamble, a `rust` tag,
    /// a bare fence, or no fence at all. Getting this wrong is not cosmetic — the syntax gate would
    /// refuse every answer, which is how it was caught.
    #[test]
    fn the_file_is_taken_from_the_answer_whatever_wraps_it() {
        let body = "impl Led for GpioLed {\n    fn set(&mut self, _on: bool) {}\n}";
        for answer in [
            format!("Sure, here it is:\n\n```rust\n{body}\n```\n"),
            format!("```\n{body}\n```"),
            format!("```rs\n{body}\n```\n\nThat should do it."),
            body.to_string(),
        ] {
            assert_eq!(code_block(&answer), body, "answer: {answer:?}");
        }
        // Truncated mid-answer: what arrived is still the file, and the gate judges it.
        assert_eq!(
            code_block(&format!("```rust\n{body}")),
            body,
            "an unterminated fence is not a reason to fall back to the whole answer"
        );
    }

    /// A fake model that answers `answer` and **records** the prompts it was given — which is how a
    /// test can assert what a repair actually sent.
    fn recording_llm(
        answer: &'static str,
        said: Arc<Mutex<Vec<String>>>,
    ) -> mpsc::Sender<LlmMessage> {
        let (tx, mut rx) = mpsc::channel(4);
        let said = said.clone();
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if let LlmMessage::Complete {
                    prompt, reply_to, ..
                } = message
                {
                    said.lock().unwrap().push(prompt);
                    let _ = reply_to.send(Ok(answer.to_string()));
                }
            }
        });
        tx
    }

    /// A repair is one more model call carrying the compiler's **own words**, and the same gate as
    /// any other answer — so a "fixed" file that still is not the file it claims to be is refused
    /// rather than written.
    #[tokio::test]
    async fn a_repair_carries_the_compilers_errors_and_passes_the_same_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        unimplemented!(\"GpioLed::set\")");
        let out = plan(root, None);
        let item = &out["plan"][0];

        let said = Arc::new(Mutex::new(Vec::new()));
        let llm_tx = recording_llm(
            "```rust\nimpl Led for GpioLed {\n    fn set(&mut self, on: bool) {\n        let _ = on;\n    }\n}\n```",
            said.clone(),
        );

        let errors = "error[E0107]: struct takes 3 generic arguments\n  --> src/lib.rs:12:10";
        let file = repair(root, item, errors, &llm_tx)
            .await
            .expect("the repaired answer passes the gate");

        assert!(file.ends_with("src/lib.rs"), "{file}");
        let prompts = said.lock().unwrap();
        assert_eq!(prompts.len(), 1, "one repair call, not a loop");
        assert!(
            prompts[0].contains("does not compile"),
            "the repair says what happened: {}",
            prompts[0]
        );
        assert!(
            prompts[0].contains("takes 3 generic arguments"),
            "and quotes the compiler rather than paraphrasing it: {}",
            prompts[0]
        );
        assert!(
            prompts[0].contains("Return the COMPLETE corrected file"),
            "the output contract survives a repair: {}",
            prompts[0]
        );
        drop(prompts);

        let written =
            std::fs::read_to_string(root.join("crates/demo-hal-esp32/src/lib.rs")).unwrap();
        assert!(written.contains("let _ = on;"), "{written}");
    }

    /// The same gate, on a repair that answers with a placeholder: refused, and the file keeps what
    /// it had — a repair cannot leave a backend worse than it found it.
    #[tokio::test]
    async fn a_repair_that_answers_with_a_placeholder_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        let _ = _on;");
        let out = plan(root, None);
        // The item is planned from the stub; `plan` only reports what still owes work, so drive the
        // repair from the item the plan would have produced before the file was written.
        let _ = out;
        let item = json!({
            "family": "esp32",
            "file": root.join("crates/demo-hal-esp32/src/lib.rs").to_string_lossy(),
            "platform": "esp32c6",
            "pending": [{
                "interface": "led", "trait": "Led", "status": "stub", "methods": ["set"]
            }],
            "prompt": "…",
        });
        let before =
            std::fs::read_to_string(root.join("crates/demo-hal-esp32/src/lib.rs")).unwrap();

        let said = Arc::new(Mutex::new(Vec::new()));
        let llm_tx = recording_llm(
            "```rust\nimpl Led for GpioLed {\n    fn set(&mut self, _on: bool) {\n        unimplemented!(\"set\")\n    }\n}\n```",
            said,
        );

        let reason = repair(root, &item, "error[E0432]: unresolved import", &llm_tx)
            .await
            .expect_err("a placeholder is not a repair");
        assert!(reason.contains("unimplemented!()"), "{reason}");
        assert_eq!(
            std::fs::read_to_string(root.join("crates/demo-hal-esp32/src/lib.rs")).unwrap(),
            before,
            "the file is untouched"
        );
    }

    /// Without an LLM the apply refuses by name and writes nothing — the plan is a prompt, and a
    /// prompt with no model behind it is not an implementation.
    #[tokio::test]
    async fn apply_without_an_llm_refuses_rather_than_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        unimplemented!(\"GpioLed::set\")");
        let before =
            std::fs::read_to_string(root.join("crates/demo-hal-esp32/src/lib.rs")).unwrap();

        let out = plan(root, None);
        let result = apply(root, &out, &None).await;

        let err = result["error"].as_str().expect("an error, not a write");
        assert!(err.contains("LLM unavailable"), "{err}");
        assert_eq!(
            std::fs::read_to_string(root.join("crates/demo-hal-esp32/src/lib.rs")).unwrap(),
            before,
            "the file is untouched"
        );
    }

    /// The path in an item decides where a model's output lands, so it is checked rather than
    /// trusted: only an existing file under `crates/` in this project is written.
    #[tokio::test]
    async fn apply_refuses_a_plan_item_pointing_outside_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        project(root, "        unimplemented!(\"GpioLed::set\")");
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("lib.rs");
        std::fs::write(&victim, "// not a backend\n").unwrap();

        // A live sender: the refusal has to happen before any model call.
        let (llm_tx, _llm_rx) = mpsc::channel(1);
        let item = json!({
            "family": "esp32",
            "file": victim.to_string_lossy(),
            "pending": [{"interface": "led", "trait": "Led", "status": "stub", "methods": ["set"]}],
            "prompt": "…",
        });
        let result = apply(root, &json!({ "plan": [item] }), &Some(llm_tx)).await;

        let failures = result["failures"].as_array().expect("failures");
        assert_eq!(failures.len(), 1, "{result}");
        assert!(
            failures[0]["reason"]
                .as_str()
                .unwrap()
                .contains("under `crates/`"),
            "{result}"
        );
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "// not a backend\n",
            "nothing outside the project is written"
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
