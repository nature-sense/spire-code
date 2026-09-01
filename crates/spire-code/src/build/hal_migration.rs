//! HAL layout migration — convert the **legacy** ai-traps HAL layout
//! (`toolkit/src/hal/api/*.hpp` contracts + `<platform>/hal/*` impls) into the
//! **canonical** `hal/` container layout (`hal/api/*.hpp` contracts +
//! `hal/implementations/<platform>/*` impls).
//!
//! The transform is deterministic and mechanical: move contract headers,
//! move per-platform implementations (keeping the implementation filename so
//! the analyzer's interface↔impl stem matching still resolves), emit
//! `hal/meson.build` with the `hal_impl_<plat>_sources` source lists, and
//! wire `subdir('hal')` into the top-level `meson.build`.
//!
//! Two phases — `migrate_hal_plan` (read-only dry run) and `migrate_hal_apply`
//! (executes the plan). See `docs/hal-migration.md`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Which HAL layout the project currently uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HalLayout {
    /// Everything already in the canonical `hal/` container.
    Canonical,
    /// Everything still in the legacy ai-traps positions.
    Legacy,
    /// Both layouts present (half-migrated); only the legacy files move.
    Mixed,
    /// No HAL layout detected — nothing to migrate.
    None,
}

impl HalLayout {
    pub fn as_str(&self) -> &'static str {
        match self {
            HalLayout::Canonical => "canonical",
            HalLayout::Legacy => "legacy",
            HalLayout::Mixed => "mixed",
            HalLayout::None => "none",
        }
    }
}

/// A single file move (relative paths).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMove {
    pub from: String,
    pub to: String,
}

/// A file to write during apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFile {
    pub path: String,
    pub content: String,
}

/// A textual edit to a build file (best-effort, applied via string replace).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildFileEdit {
    pub file: String,
    pub before: String,
    pub after: String,
}

/// The migration plan (dry-run result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalMigrationPlan {
    pub layout: HalLayout,
    pub layout_name: String,
    pub moves: Vec<FileMove>,
    pub write_files: Vec<WriteFile>,
    pub build_file_edits: Vec<BuildFileEdit>,
    pub conflicts: Vec<String>,
    /// Why the layout was classified the way it was (which paths matched).
    pub reasons: Vec<String>,
    pub can_apply: bool,
    /// Human notes (e.g. per-platform meson.build currently references the
    /// legacy `*_hal_sources` var and should switch to `hal_impl_<plat>_sources`).
    pub notes: Vec<String>,
}

/// Result of applying a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    pub applied_moves: Vec<String>,
    pub written_files: Vec<String>,
    pub applied_edits: Vec<String>,
    /// Empty legacy directories removed after the moves (best-effort).
    pub cleanup: Vec<String>,
    pub errors: Vec<String>,
}

/// A single HAL sanity issue with a concrete corrective suggestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalIssue {
    /// "error" (blocks a healthy HAL setup) or "warning".
    pub severity: String,
    /// Human-readable issue (e.g. "missing contract dir").
    pub title: String,
    /// Which path / contract / platform the issue concerns.
    pub path: String,
    /// What to do to correct it.
    pub suggested_fix: String,
}

/// Result of `hal_sanity_check` — zero issues => `status == "ok"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalSanityReport {
    pub status: String, // "ok" | "issues"
    pub layout: String,
    pub issues: Vec<HalIssue>,
    /// Platform dirs discovered at the top level (excluding known dirs).
    pub platforms: Vec<String>,
    /// Contract stems found under hal/api/ or toolkit/src/hal/api/.
    pub interfaces: Vec<String>,
}


fn root_has_file(root: &Path, rel: &str) -> bool {
    root.join(rel).exists()
}

fn hpp_files(root: &Path, rel_dir: &str) -> Vec<PathBuf> {
    let dir = root.join(rel_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x == "hpp") {
            out.push(p);
        }
    }
    out
}

/// Detect which HAL layout (if any) the project uses.
///
/// A project is only `Canonical` when NO legacy markers remain:
/// - no `*.hpp` contract headers under `toolkit/src/hal/api/`,
/// - no leftover files (e.g. an orphaned `config_loader.cpp`) under
///   `toolkit/src/hal/api/`,
/// - no `<plat>/hal/*` implementation directories,
/// - no `<plat>/meson.build` with stale legacy `hal/…` / `*_hal_sources` wiring.
///
/// Any of these classify the project `Mixed` (canonical + legacy leftovers) or
/// `Legacy` when the canonical side is absent entirely.
pub fn detect_hal_layout(root: &Path) -> HalLayout {
    let canonical_contracts = hpp_files(root, "hal/api");
    let has_canonical_contracts = !canonical_contracts.is_empty();
    let canonical_impls = root_has_file(root, "hal/implementations");
    let has_canonical = has_canonical_contracts || canonical_impls;

    // Legacy markers — includes leftover non-`.hpp` files (e.g. a `.cpp` from
    // a partial migration) in the legacy contract dir.
    let legacy_api_dir = root.join("toolkit/src/hal/api");
    let legacy_contracts = hpp_files(root, "toolkit/src/hal/api");
    let legacy_leftovers = dir_has_any_file(&legacy_api_dir);
    let legacy_impls = has_legacy_platform_impls(root);
    let stale_wiring = has_stale_platform_hal_refs(root);
    let has_legacy = !legacy_contracts.is_empty() || legacy_leftovers || legacy_impls || stale_wiring;

    match (has_canonical, has_legacy) {
        (true, false) => HalLayout::Canonical,
        (false, true) => HalLayout::Legacy,
        (true, true) => HalLayout::Mixed,
        (false, false) => HalLayout::None,
    }
}

/// True when any top-level platform directory (other than toolkit/hal/build
/// dirs) contains a `hal/` subdirectory with implementation sources.
fn has_legacy_platform_impls(root: &Path) -> bool {
    // Scan top-level directories (skip known non-platform dirs).
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    let skip = ["toolkit", "hal", "build", "build-native", "subprojects", ".git"];
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if skip.contains(&name) || name.starts_with(".") {
            continue;
        }
        let hal_dir = p.join("hal");
        if hal_dir.is_dir() {
            // A legacy impl dir counts only if it has source files.
            if std::fs::read_dir(&hal_dir)
                .map(|rd| rd.filter_map(|x| x.ok()).any(|x| {
                    x.path()
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e == "cpp" || e == "c")
                }))
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// True when a directory contains at least one entry (a leftover `.cpp` in the
/// legacy contract dir is still a legacy marker, even without `.hpp` files).
fn dir_has_any_file(dir: &Path) -> bool {
    std::fs::read_dir(dir).map(|rd| rd.flatten().next().is_some()).unwrap_or(false)
}

/// True when `<plat>/meson.build` still references the legacy layout: source
/// files under a `hal/` subdir, or a legacy `<plat>_hal_sources` variable.
fn platform_meson_has_legacy_hal(plat_meson: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(plat_meson) else { return false };
    // Strip `#` comments so `# hal/...` prose never false-positives.
    let stripped = Regex::new(r"(?m)#.*$").unwrap().replace_all(&text, "");
    let hal_files = Regex::new(r#"['"]hal/[^'"]+\.(?:c|cpp|cc|cxx)['"]"#).unwrap();
    if hal_files.is_match(&stripped) {
        return true;
    }
    // Legacy inline declaration: `<plat>_hal_sources = files('hal/...')` or
    // `+= files(...)`. A READ assignment like
    // `rpi5_hal_sources = hal_impl_rpi5_sources` (canonical alias) is NOT legacy.
    let legacy_var = Regex::new(r"(?m)^\s*\w+_hal_sources\s*(?:=|\+=)\s*files\s*\(")
        .unwrap();
    legacy_var.is_match(&stripped)
}

/// Rewrite a platform `<plat>/meson.build` from the legacy inline
/// `<plat>_hal_sources = files('hal/...')` wiring to the canonical centralized
/// `hal_impl_<plat>_sources` variable (declared in `hal/meson.build`).
///
/// Handles the two ai-traps shapes:
/// - `<var> = files('hal/a.cpp', ...)` and `<var> += files('hal/a.cpp', ...)`
/// - the rpi5-style `foreach s : ['hal/...'] … <var> += files(s) … endforeach`
///
/// Variable references `+ <var>` are rewritten to `+ hal_impl_<plat>_sources`,
/// and a now-empty `if platform == '<plat>' … endif` guard (whose body was
/// entirely HAL source additions) is collapsed.
fn rewrite_platform_meson(plat: &str, text: &str) -> String {
    let legacy_var = format!("{}_hal_sources", plat);
    let escaped = regex::escape(&legacy_var);

    // 1. Remove the empty init (`<var> = files()`).
    let empty_init = Regex::new(&format!(r"(?m)^[ \t]*{}\s*=\s*files\(\)[ \t]*$\n?", escaped))
        .unwrap();
    let mut out = empty_init.replace_all(text, "").to_string();

    // 2. Remove `<var> = files(...)` and `<var> += files(...)` declarations,
    //    including multi-line `files(` blocks (paths contain no parens, so a
    //    non-greedy `\)` stop is safe).
    let decl_re =
        Regex::new(&format!(r"(?ms)^[ \t]*{}\s*(?:=|\+=)\s*files\(.*?\)[ \t]*$\n?", escaped))
            .unwrap();
    out = decl_re.replace_all(&out, "").to_string();

    // 3. Remove the rpi5-style `foreach … [ … 'hal/…' … ]` block (its body
    //    appended legacy sources). `(?s)` lets `.` span newlines; `^`/`$` with
    //    `(?m)` anchor to line boundaries for the endforeach terminator.
    let foreach_re = Regex::new(
        r#"(?ms)^[ \t]*foreach\s+\w+\s*:\s*\[[^\]]*['"]hal/[^'"]+['"][^\]]*\]\s*\n(?:[^\n]*\n)*?^[ \t]*endforeach\s*\n"#,
    )
    .unwrap();
    out = foreach_re.replace_all(&out, "").to_string();

    // 4. Replace `+ <legacy_var>` references with `+ hal_impl_<plat>_sources`.
    let ref_re =
        Regex::new(&format!(r"\+[ \t]*{}\b", escaped)).unwrap();
    out = ref_re
        .replace_all(&out, format!("+ hal_impl_{}_sources", plat).as_str())
        .to_string();

    // 5. Collapse a now-empty `if platform == '<plat>' … endif` guard (body
    //    was entirely HAL source additions). Remove repeated empty guards.
    loop {
        let before = out.clone();
        let empty_guard = Regex::new(
            &format!(
                r#"(?ms)^[ \t]*if\s+platform\s*==\s*['"][^'"]*{plat}[^'"]*['"]\s*(?:then)?\s*\n\s*endif\s*\n"#
            ),
        )
        .unwrap();
        out = empty_guard.replace_all(&out, "").to_string();
        if out == before {
            break;
        }
    }

    // 6. Remove leading blank lines accidentally left by the deletions.
    let trim = Regex::new(r"(?m)^[ \t]*\n(?:[ \t]*\n)+").unwrap();
    out = trim.replace_all(&out, "\n").to_string();
    out
}

/// True when any top-level platform dir's meson.build still references the
/// legacy per-platform HAL layout (stale wiring after a partial migration).
fn has_stale_platform_hal_refs(root: &Path) -> bool {
    let skip = ["toolkit", "hal", "build", "build-native", "subprojects", ".git"];
    let Ok(entries) = std::fs::read_dir(root) else { return false };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() { continue; }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if skip.contains(&name) || name.starts_with(".") { continue; }
        if platform_meson_has_legacy_hal(&p.join("meson.build")) {
            return true;
        }
    }
    false
}

/// Reuse the same source-list variable shape `hal_add_target` generates:
/// `hal_impl_<plat>_sources = files('implementations/<plat>/<file>', ...)`.
pub fn hal_meson_var_section(platform: &str, impl_files: &[String]) -> String {
    let quoted: Vec<String> = impl_files
        .iter()
        .map(|f| format!("'implementations/{}/{}'", platform, rel_name(f)))
        .collect();
    format!("hal_impl_{}_sources = files({})", platform, quoted.join(", "))
}

fn rel_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(p)
        .to_string()
}

/// Build a migration plan (read-only). Never touches disk.
pub fn migrate_hal_plan(root: &Path) -> Result<HalMigrationPlan, String> {
    let layout = detect_hal_layout(root);
    let mut plan = HalMigrationPlan {
        layout,
        layout_name: layout.as_str().to_string(),
        moves: Vec::new(),
        write_files: Vec::new(),
        build_file_edits: Vec::new(),
        conflicts: Vec::new(),
        reasons: Vec::new(),
        can_apply: false,
        notes: Vec::new(),
    };

    // Record which exact paths drove the classification.
    for c in hpp_files(root, "hal/api") {
        plan.reasons.push(format!("canonical contracts: {}", c.display()));
    }
    if root.join("hal/implementations").is_dir() {
        plan.reasons.push("canonical impls: hal/implementations/".to_string());
    }
    for c in hpp_files(root, "toolkit/src/hal/api") {
        plan.reasons.push(format!("legacy contracts: {}", c.display()));
    }
    let skip = vec!["toolkit", "hal", "build", "build-native", "subprojects", ".git"];
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() { continue; }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
            if skip.contains(&name) || name.starts_with(".") { continue; }
            let hal_dir = p.join("hal");
            if hal_dir.is_dir() {
                plan.reasons.push(format!("legacy impls: {}/hal/", name));
            }
        }
    }

    if layout == HalLayout::Canonical {
        plan.can_apply = false;
        plan.notes.push("Project is already in the canonical hal/ layout.".to_string());
        return Ok(plan);
    }
    if layout == HalLayout::None {
        plan.can_apply = false;
        plan.notes.push("No HAL layout detected — nothing to migrate.".to_string());
        return Ok(plan);
    }
    if layout == HalLayout::Mixed {
        plan.notes.push("Both layouts present — only the legacy files will move.".to_string());
    }

    // 1. Contracts: toolkit/src/hal/api/*.hpp → hal/api/.
    for contract in hpp_files(root, "toolkit/src/hal/api") {
        let name = rel_name(&contract.to_string_lossy());
        let to = PathBuf::from("hal/api").join(&name);
        let from = contract.strip_prefix(root).unwrap_or(&contract);
        if root.join(&to).exists() {
            plan.conflicts.push(format!("{} already exists", to.display()));
        } else {
            plan.moves.push(FileMove {
                from: from.to_string_lossy().to_string(),
                to: to.to_string_lossy().to_string(),
            });
        }
    }

    // 1b. Orphan non-header sources in the legacy contract dir (e.g. a
    //     `config_loader.cpp` left over from a partial migration) are NOT
    //     contracts and must not stay under toolkit/src/hal/api/. They are
    //     shared toolkit sources, so relocate them to the shared `toolkit/src/`
    //     root (the toolkit include path already exposes `src` and `src/hal/api`,
    //     so `#include "config_loader.hpp"` keeps resolving).
    if let Ok(entries) = std::fs::read_dir(root.join("toolkit/src/hal/api")) {
        for e in entries.flatten() {
            let ep = e.path();
            let Some(ext) = ep.extension().and_then(|x| x.to_str()) else { continue };
            if ext != "cpp" && ext != "c" && ext != "cc" && ext != "cxx" {
                continue;
            }
            let fname = rel_name(&ep.to_string_lossy());
            let to = PathBuf::from("toolkit").join("src").join(&fname);
            let from = ep.strip_prefix(root).unwrap_or(&ep);
            if root.join(&to).exists() {
                plan.conflicts.push(format!("{} already exists", to.display()));
            } else {
                plan.moves.push(FileMove {
                    from: from.to_string_lossy().to_string(),
                    to: to.to_string_lossy().to_string(),
                });
            }
        }
    }

    // 2. Per-platform implementations: <plat>/hal/*.{cpp,c} →
    //    hal/implementations/<plat>/ (same filename).
    let skip = ["toolkit", "hal", "build", "build-native", "subprojects", ".git"];
    let mut platform_impls: Vec<(String, Vec<String>)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Err("Cannot read project root".to_string());
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if skip.contains(&name) || name.starts_with(".") {
            continue;
        }
        let hal_dir = p.join("hal");
        if !hal_dir.is_dir() {
            continue;
        }
        let mut files: Vec<String> = Vec::new();
        let Ok(rd) = std::fs::read_dir(&hal_dir) else { continue };
        for fe in rd.flatten() {
            let fp = fe.path();
            let ext = fp.extension().and_then(|x| x.to_str()).unwrap_or("");
            if ext == "cpp" || ext == "c" {
                let fname = rel_name(&fp.to_string_lossy());
                let to = PathBuf::from("hal/implementations")
                    .join(name)
                    .join(&fname);
                let from = fp.strip_prefix(root).unwrap_or(&fp);
                if root.join(&to).exists() {
                    plan.conflicts.push(format!("{} already exists", to.display()));
                } else {
                    plan.moves.push(FileMove {
                        from: from.to_string_lossy().to_string(),
                        to: to.to_string_lossy().to_string(),
                    });
                    files.push(fname);
                    // Keep the platform meson.build note for the human.
                }
            }
        }
        if !files.is_empty() {
            platform_impls.push((name.to_string(), files));
            plan.notes.push(format!(
                "{}meson.build: switch the legacy *_hal_sources list to hal_impl_{}_sources (now declared in hal/meson.build)",
                name, name
            ));
        }
    }

    // 3. hal/meson.build with the per-platform source lists.
    if !platform_impls.is_empty() {
        let mut content = String::new();
        for (plat, files) in &platform_impls {
            content.push_str(&hal_meson_var_section(plat, files));
            content.push('\n');
        }
        plan.write_files.push(WriteFile {
            path: "hal/meson.build".to_string(),
            content,
        });
    }

    // 4. Top-level meson.build: ensure subdir('hal') is wired.
    let top = root.join("meson.build");
    if top.exists() {
        if let Ok(text) = std::fs::read_to_string(&top) {
            if !text.contains("subdir('hal')") && !text.contains("subdir(\"hal\")") {
                // Insert after the first project(...) line, else at the top.
                let after = if let Some(idx) = text.find("project(") {
                    let end = text[idx..].find('\n').map(|off| idx + off + 1).unwrap_or(idx);
                    let mut s = text.clone();
                    s.insert_str(end, "subdir('hal')\n");
                    s
                } else {
                    format!("subdir('hal')\n{}", text)
                };
                plan.build_file_edits.push(BuildFileEdit {
                    file: "meson.build".to_string(),
                    before: text,
                    after: after.clone(),
                });
                // Store the resulting full content so apply can write directly.
                // (BuildFileEdit stores before/after; apply replaces the file.)
            }
        }
    }

    // 5. Rewrite stale per-platform meson.build wiring: replace the legacy
    //    inline `<plat>_hal_sources = files('hal/...')` declarations with the
    //    centralized `hal_impl_<plat>_sources` variable from hal/meson.build.
    //    This is what docs/hal-migration.md promises as `build_file_edits`.
    let skip = ["toolkit", "hal", "build", "build-native", "subprojects", ".git"];
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() { continue; }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
            if skip.contains(&name) || name.starts_with(".") { continue; }
            let plat_meson = p.join("meson.build");
            if !plat_meson.exists() { continue; }
            let Ok(text) = std::fs::read_to_string(&plat_meson) else { continue };
            if platform_meson_has_legacy_hal(&plat_meson) {
                let rewritten = rewrite_platform_meson(name, &text);
                if rewritten != text {
                    plan.build_file_edits.push(BuildFileEdit {
                        file: format!("{name}/meson.build"),
                        before: text,
                        after: rewritten,
                    });
                    plan.reasons.push(format!("stale {} meson.build rewired", name));
                }
            }
        }
    }

    plan.can_apply = !plan.conflicts.is_empty() == false && !plan.moves.is_empty();
    if plan.moves.is_empty() && plan.write_files.is_empty() {
        plan.can_apply = false;
        plan.notes.push("Nothing to migrate.".to_string());
    }
    Ok(plan)
}

/// Execute a plan: apply moves, write files, apply build-file edits.
pub fn migrate_hal_apply(root: &Path, plan: &HalMigrationPlan) -> Result<MigrationResult, String> {
    let mut result = MigrationResult {
        applied_moves: Vec::new(),
        written_files: Vec::new(),
        applied_edits: Vec::new(),
        cleanup: Vec::new(),
        errors: Vec::new(),
    };

    // 1. Moves.
    for m in &plan.moves {
        let from = root.join(&m.from);
        let to = root.join(&m.to);
        if !from.exists() {
            result
                .errors
                .push(format!("missing source {}", m.from));
            continue;
        }
        if let Some(parent) = to.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                result.errors.push(format!("mkdir {}: {}", parent.display(), e));
                continue;
            }
        }
        if to.exists() {
            result.errors.push(format!("conflict: {} already exists", m.to));
            continue;
        }
        match std::fs::rename(&from, &to) {
            Ok(()) => result.applied_moves.push(format!("{} -> {}", m.from, m.to)),
            Err(e) => result.errors.push(format!("move {}: {}", m.from, e)),
        }
    }

    // 2. Writes.
    for w in &plan.write_files {
        let path = root.join(&w.path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, &w.content) {
            Ok(()) => result.written_files.push(w.path.clone()),
            Err(e) => result.errors.push(format!("write {}: {}", w.path, e)),
        }
    }

    // 3b. Clean up now-empty legacy directories (best-effort).
    let legacy_hal_dirs: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        let skip = ["toolkit", "hal", "build", "build-native", "subprojects", ".git"];
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let p = e.path();
                if !p.is_dir() { continue; }
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
                if skip.contains(&name) || name.starts_with(".") { continue; }
                v.push(p.join("hal"));
            }
        }
        v.push(root.join("toolkit/src/hal/api"));
        v.push(root.join("toolkit/src/hal"));
        v
    };
    for dir in legacy_hal_dirs {
        if dir.is_dir() {
            // Only remove if truly empty.
            let empty = std::fs::read_dir(&dir).map(|rd| rd.count() == 0).unwrap_or(false);
            if empty {
                let rel = dir.strip_prefix(root).unwrap_or(&dir).to_string_lossy().to_string();
                match std::fs::remove_dir(&dir) {
                    Ok(()) => result.cleanup.push(rel),
                    Err(_) => { /* best-effort */ }
                }
            }
        }
    }

    // 3. Build-file edits (full-file replacement using before/after).
    for edit in &plan.build_file_edits {
        let path = root.join(&edit.file);
        match std::fs::read_to_string(&path) {
            Ok(current) if current == edit.before => match std::fs::write(&path, &edit.after) {
                Ok(()) => result.applied_edits.push(edit.file.clone()),
                Err(e) => result.errors.push(format!("edit {}: {}", edit.file, e)),
            },
            Ok(_) => result
                .errors
                .push(format!("edit {}: file changed since plan", edit.file)),
            Err(e) => result.errors.push(format!("read {}: {}", edit.file, e)),
        }
    }

    Ok(result)
}

/// Sanity-check a HAL project's structural completeness. On first open this
/// verifies every required component is present and returns a corrective
/// action for anything missing. Supports BOTH layouts: canonical is validated
/// against the hal/ container; legacy is validated against toolkit/src/hal/api
/// + <plat>/hal/ (and the report notes migration as the recommended fix when
/// the project is canonical-but-has-legacy-leftovers, or vice versa).
pub fn hal_sanity_check(root: &Path) -> HalSanityReport {
    let layout = detect_hal_layout(root);
    let mut issues: Vec<HalIssue> = Vec::new();
    let mut interfaces: Vec<String> = Vec::new();
    let platforms = top_level_platforms(root);

    // 1. Contracts present? Only abstract-class headers (`= 0` pure-virtual
    //    methods) count as interfaces. Struct headers (frame_buffer.hpp),
    //    free-function headers (config_loader.hpp) and impl files are NOT
    //    contracts and are flagged below.
    let contracts_dir = match layout {
        HalLayout::Legacy | HalLayout::Mixed => root.join("toolkit/src/hal/api"),
        _ => root.join("hal/api"),
    };
    let contract_hpps = hpp_files(root, rel_of(&contracts_dir, root));
    use crate::build::generic_helpers::{classify_hal_header, HalHeaderKind};
    let mut real_contracts: Vec<&PathBuf> = Vec::new();
    for c in &contract_hpps {
        if classify_hal_header(c) == HalHeaderKind::Contract {
            real_contracts.push(c);
            if let Some(stem) = c.file_stem().and_then(|x| x.to_str()) {
                interfaces.push(stem.to_string());
            }
        }
    }
    // Non-contract header in the contract dir. Pure-data headers (structs,
    // frame_buffer.hpp, types.hpp) are legitimate shared support headers and
    // produce NO warning. Only headers without any class/struct (free
    // functions) or unparseable files are flagged as misplaced.
    for c in &contract_hpps {
        if classify_hal_header(c) == HalHeaderKind::Contract {
            continue;
        }
        if classify_hal_header(c) == HalHeaderKind::DataOnly {
            continue; // pure-data definition — legitimate, no warning
        }
        if let Some(name) = c.file_name().and_then(|x| x.to_str()) {
            let rel = c.strip_prefix(root).unwrap_or(c).to_string_lossy().to_string();
            issues.push(HalIssue {
                severity: "warning".to_string(),
                title: "Non-contract header in HAL api dir".to_string(),
                path: rel,
                suggested_fix: format!(
                    "{} has no pure-virtual methods — move it out of the contract dir (hal/api/) or make it an abstract-class contract.",
                    name
                ),
            });
        }
    }
    // Orphan `.cpp`/`.c` in the (now canonical) legacy contract dir: an impl
    // left behind by a partial migration.
    if root.join("toolkit/src/hal/api").is_dir() {
        if let Ok(entries) = std::fs::read_dir(root.join("toolkit/src/hal/api")) {
            for e in entries.flatten() {
                let ep = e.path();
                let Some(ext) = ep.extension().and_then(|x| x.to_str()) else { continue };
                if ext == "cpp" || ext == "c" || ext == "cc" || ext == "cxx" {
                    let rel = "toolkit/src/hal/api".to_string()
                        + "/"
                        + ep.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                    issues.push(HalIssue {
                        severity: "warning".to_string(),
                        title: "Implementation file left in legacy contract dir".to_string(),
                        path: rel,
                        suggested_fix: "Move it under hal/implementations/<plat>/ (or remove it) — an impl is not a contract header.".to_string(),
                    });
                }
            }
        }
    }
    if real_contracts.is_empty() {
        issues.push(HalIssue {
            severity: "error".to_string(),
            title: "No HAL contracts".to_string(),
            path: contracts_dir.to_string_lossy().to_string(),
            suggested_fix: "Create hal/api/<capability>_hal.hpp abstract-class headers (one per hardware capability).".to_string(),
        });
    }

    // 2. Implementations present + platform coverage.
    if platforms.is_empty() {
        issues.push(HalIssue {
            severity: "warning".to_string(),
            title: "No platform directories".to_string(),
            path: root.to_string_lossy().to_string(),
            suggested_fix: "Add per-platform directories (rpi5/, rock3c/, ...) with HAL implementations.".to_string(),
        });
    } else {
        for plat in &platforms {
            let impl_dir = match layout {
                HalLayout::Legacy | HalLayout::Mixed => root.join(plat).join("hal"),
                _ => root.join("hal/implementations").join(plat),
            };
            let has_impls = impl_dir.is_dir()
                && std::fs::read_dir(&impl_dir)
                    .map(|rd| rd.filter_map(|x| x.ok()).any(|x| {
                        x.path().extension().and_then(|e| e.to_str()).is_some_and(|e| e == "cpp" || e == "c")
                    }))
                    .unwrap_or(false);
            if !has_impls {
                issues.push(HalIssue {
                    severity: "error".to_string(),
                    title: format!("Missing HAL implementation for platform '{}'", plat),
                    path: impl_dir.to_string_lossy().to_string(),
                    suggested_fix: format!(
                        "Add hal/implementations/{}/<stem>_<impl>.cpp (or legacy {}hal/) for each interface.",
                        plat, plat
                    ),
                });
            }
        }
    }

    // 3. hal/meson.build wiring.
    let meson_hal = match layout {
        HalLayout::Legacy => None,
        _ => Some(root.join("hal/meson.build")),
    };
    if let Some(f) = meson_hal {
        if !f.exists() {
            issues.push(HalIssue {
                severity: "error".to_string(),
                title: "Missing hal/meson.build".to_string(),
                path: f.to_string_lossy().to_string(),
                suggested_fix: "Generate hal/meson.build with hal_impl_<plat>_sources = files('implementations/<plat>/...').".to_string(),
            });
        } else if let Ok(text) = std::fs::read_to_string(&f) {
            for plat in &platforms {
                let var = format!("hal_impl_{}_sources", plat);
                if !text.contains(&var) {
                    issues.push(HalIssue {
                        severity: "warning".to_string(),
                        title: format!("Missing {} wiring", var),
                        path: f.to_string_lossy().to_string(),
                        suggested_fix: format!("Declare {} = files('implementations/{}/<stem>_<impl>.cpp').", var, plat),
                    });
                }
            }
        }
    }

    // 4. Top-level meson.build wires subdir('hal') (canonical only).
    if layout != HalLayout::Legacy {
        let top = root.join("meson.build");
        if top.exists() {
            if let Ok(text) = std::fs::read_to_string(&top) {
                if !text.contains("subdir('hal')") && !text.contains("subdir(\"hal\")") {
                    issues.push(HalIssue {
                        severity: "warning".to_string(),
                        title: "Top-level meson.build missing subdir('hal')".to_string(),
                        path: "meson.build".to_string(),
                        suggested_fix: "Add subdir('hal') after project(...).".to_string(),
                    });
                }
            }
        }
    }

    // 5. Legacy leftovers when canonical (or canonical leftovers when legacy).
    match layout {
        HalLayout::Mixed => {
            issues.push(HalIssue {
                severity: "warning".to_string(),
                title: "Mixed HAL layout".to_string(),
                path: root.to_string_lossy().to_string(),
                suggested_fix: "Run the HAL migration tool (hal_migrate_plan/hal_migrate_apply) to canonicalise.".to_string(),
            });
        }
        _ if root.join("toolkit/src/hal/api").is_dir() || root.join("hal/api").is_dir() => {
            if layout == HalLayout::Canonical && root.join("toolkit/src/hal/api").exists() {
                issues.push(HalIssue {
                    severity: "warning".to_string(),
                    title: "Legacy contract dir left behind".to_string(),
                    path: "toolkit/src/hal/api".to_string(),
                    suggested_fix: "Remove the emptied legacy dir or re-run migrations cleanup.".to_string(),
                });
            }
        }
        _ => {}
    }

    let status = if issues.is_empty() { "ok" } else { "issues" };
    HalSanityReport {
        status: status.to_string(),
        layout: layout.as_str().to_string(),
        issues,
        platforms,
        interfaces,
    }
}

fn rel_of(p: &Path, root: &Path) -> &'static str {
    // hpp_files takes a relative dir; fall back to the raw path between markers.
    Box::leak(p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned().into_boxed_str())
}

/// Discovered target platforms: canonical `hal/implementations/*` subdirs,
/// plus legacy-style top-level dirs that contain source files.
fn top_level_platforms(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    let skip = ["toolkit", "hal", "build", "build-native", "subprojects", ".git"];

    // Canonical: hal/implementations/<plat>/
    if let Ok(entries) = std::fs::read_dir(root.join("hal/implementations")) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    if !name.starts_with(".") && !v.contains(&name.to_string()) {
                        v.push(name.to_string());
                    }
                }
            }
        }
    }

    // Legacy: top-level <plat>/ dirs containing source files.
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() { continue; }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
            if skip.contains(&name) || name.starts_with(".") || v.contains(&name.to_string()) { continue; }
            // Only directories that look like platform targets (contain sources).
            let looks_platform = std::fs::read_dir(&p)
                .map(|rd| rd.filter_map(|x| x.ok()).any(|x| x.path().is_file()))
                .unwrap_or(false);
            if looks_platform {
                v.push(name.to_string());
            }
        }
    }
    v
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    const CONTRACT_HPP: &str = "class CameraHAL {\npublic:\n    virtual bool start() = 0;\n};\n";

    /// Build a canonical fixture: hal/api/camera_hal.hpp + implementations.
    fn canonical_fixture(root: &Path) {
        write(root, "meson.build", "project('demo', 'cpp')\nsubdir('hal')\n");
        write(root, "hal/api/camera_hal.hpp", CONTRACT_HPP);
        write(root, "hal/implementations/rpi5/camera_hal_imx219.cpp", "// impl\n");
        write(root, "hal/meson.build", "hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n");
    }
    /// Build a legacy fixture: toolkit/src/hal/api/*.hpp + <plat>/hal/*.
    fn legacy_fixture(root: &Path) {
        write(root, "meson.build", "project('demo', 'cpp')\nsubdir('toolkit')\nsubdir('rpi5')\n");
        write(root, "toolkit/src/hal/api/camera_hal.hpp", CONTRACT_HPP);
        write(root, "toolkit/meson.build", "toolkit_sources = files('src/a.cpp')\n");
        write(root, "rpi5/meson.build", "rpi5_hal_sources = files('hal/camera_hal_imx219.cpp')\nexecutable('demo-rpi5', 'main.cpp' + rpi5_hal_sources)\n");
        write(root, "rpi5/hal/camera_hal_imx219.cpp", "// impl\n");
    }

    /// Build a half-migrated fixture mirroring ai-traps before cleanup:
    /// canonical hal/ container present, but stale legacy markers remain —
    /// an orphaned config_loader.cpp in toolkit/src/hal/api/ and a stale
    /// rpi5/meson.build still declaring rpi5_hal_sources = files('hal/...').
    fn stale_wiring_fixture(root: &Path) {
        write(root, "meson.build", "project('demo', 'cpp')\nsubdir('hal')\nsubdir('toolkit')\nsubdir('rpi5')\n");
        write(root, "hal/api/camera_hal.hpp", CONTRACT_HPP);
        write(root, "hal/meson.build", "hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n");
        write(root, "hal/implementations/rpi5/camera_hal_imx219.cpp", "// impl\n");
        write(root, "toolkit/src/hal/api/config_loader.cpp", "// orphan impl\n");
        write(root, "toolkit/meson.build", "toolkit_sources = files('src/a.cpp')\n");
        write(root, "rpi5/meson.build", "rpi5_hal_sources = files('hal/camera_hal_imx219.cpp')\nexecutable('demo-rpi5', 'main.cpp' + rpi5_hal_sources)\n");
        write(root, "rpi5/main.cpp", "int main(){}\n");
    }

    #[test]
    fn detects_canonical() {
        let dir = tempfile::tempdir().unwrap();
        canonical_fixture(dir.path());
        assert_eq!(detect_hal_layout(dir.path()), HalLayout::Canonical);
    }

    #[test]
    fn detects_legacy() {
        let dir = tempfile::tempdir().unwrap();
        legacy_fixture(dir.path());
        assert_eq!(detect_hal_layout(dir.path()), HalLayout::Legacy);
    }

    #[test]
    fn detects_none_on_plain_project() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "meson.build", "project('x', 'c')\n");
        assert_eq!(detect_hal_layout(dir.path()), HalLayout::None);
    }

    #[test]
    fn plan_for_canonical_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        canonical_fixture(dir.path());
        let plan = migrate_hal_plan(dir.path()).unwrap();
        assert_eq!(plan.layout, HalLayout::Canonical);
        assert!(!plan.can_apply);
        assert!(plan.moves.is_empty());
    }

    #[test]
    fn plan_for_legacy_moves_contracts_and_impls() {
        let dir = tempfile::tempdir().unwrap();
        legacy_fixture(dir.path());
        let plan = migrate_hal_plan(dir.path()).unwrap();
        assert_eq!(plan.layout, HalLayout::Legacy);
        assert!(plan.can_apply);

        let moves: Vec<&str> = plan.moves.iter().map(|m| m.from.as_str()).collect();
        assert!(
            moves.contains(&"toolkit/src/hal/api/camera_hal.hpp"),
            "contract move missing: {moves:?}"
        );
        assert!(
            moves.contains(&"rpi5/hal/camera_hal_imx219.cpp"),
            "impl move missing: {moves:?}"
        );
        assert!(
            plan.moves.iter().any(|m| m.to == "hal/api/camera_hal.hpp"),
            "contract dest wrong: {:?}",
            plan.moves
        );
        assert!(
            plan.moves
                .iter()
                .any(|m| m.to == "hal/implementations/rpi5/camera_hal_imx219.cpp"),
            "impl dest wrong: {:?}",
            plan.moves
        );

        // hal/meson.build includes the wiring.
        let meson = &plan.write_files[0];
        assert_eq!(meson.path, "hal/meson.build");
        assert!(
            meson.content.contains("hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')"),
            "meson content: {}",
            meson.content
        );

        // Top-level meson.build gets subdir('hal').
        assert!(!plan.build_file_edits.is_empty());
        assert!(
            plan.build_file_edits[0].after.contains("subdir('hal')"),
            "edit: {:?}",
            plan.build_file_edits[0]
        );
    }

    #[test]
    fn apply_migrates_legacy_project() {
        let dir = tempfile::tempdir().unwrap();
        legacy_fixture(dir.path());
        let plan = migrate_hal_plan(dir.path()).unwrap();
        assert!(plan.can_apply);

        let result = migrate_hal_apply(dir.path(), &plan).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        // Files moved.
        assert!(!dir.path().join("toolkit/src/hal/api/camera_hal.hpp").exists());
        assert!(dir.path().join("hal/api/camera_hal.hpp").exists());
        assert!(!dir.path().join("rpi5/hal/camera_hal_imx219.cpp").exists());
        assert!(dir
            .path()
            .join("hal/implementations/rpi5/camera_hal_imx219.cpp")
            .exists());
        // hal/meson.build written.
        assert!(dir.path().join("hal/meson.build").exists());
        // Top-level meson.build rewritten.
        let top = std::fs::read_to_string(dir.path().join("meson.build")).unwrap();
        assert!(top.contains("subdir('hal')"), "top: {top}");
        // Cleanup: legacy dirs removed.
        assert!(!dir.path().join("rpi5/hal").exists(), "rpi5/hal left behind");
        assert!(!dir.path().join("toolkit/src/hal/api").exists(), "legacy api left behind");
        assert!(result.cleanup.iter().any(|c| c.contains("rpi5/hal") || c.contains("hal/api")), "cleanup: {:?}", result.cleanup);
    }

    #[test]
    fn sanity_ok_for_healthy_canonical() {
        let dir = tempfile::tempdir().unwrap();
        canonical_fixture(dir.path());
        let report = hal_sanity_check(dir.path());
        assert_eq!(report.status, "ok", "issues: {:?}", report.issues);
        assert!(report.interfaces.contains(&"camera_hal".to_string()), "interfaces: {:?}", report.interfaces);
    }

    #[test]
    fn sanity_flags_missing_contract() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "meson.build", "project('d', 'cpp')\nsubdir('hal')\n");
        write(dir.path(), "hal/implementations/rpi5/camera_hal_imx219.cpp", "// i\n");
        write(dir.path(), "hal/meson.build", "hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n");
        let report = hal_sanity_check(dir.path());
        assert_eq!(report.status, "issues");
        assert!(report.issues.iter().any(|i| i.title.contains("No HAL contracts")));
    }

    #[test]
    fn sanity_flags_missing_impl_for_platform() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "meson.build", "project('d', 'cpp')\nsubdir('hal')\n");
        write(dir.path(), "hal/api/camera_hal.hpp", CONTRACT_HPP);
        write(dir.path(), "hal/meson.build", "\n");
        // rpi5 dir with sources but no hal/implementations/rpi5.
        write(dir.path(), "rpi5/main.cpp", "int main(){}\n");
        let report = hal_sanity_check(dir.path());
        assert!(report.issues.iter().any(|i| i.title.contains("Missing HAL implementation")), "issues: {:?}", report.issues);
    }

    #[test]
    fn sanity_flags_legacy_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        canonical_fixture(dir.path());
        // Simulate the leave-behind the migration used to create.
        write(dir.path(), "rpi5/hal/stale.cpp", "// stale\n");
        let report = hal_sanity_check(dir.path());
        assert_eq!(report.status, "issues");
        assert!(report.issues.iter().any(|i| i.title.contains("Mixed HAL layout") || i.title.contains("legacy")), "issues: {:?}", report.issues);
    }

    /// A stale-wiring project (canonical container + orphaned legacy `.cpp` +
    /// legacy `<plat>/meson.build` wiring) must be detected `Mixed`, never
    /// `Canonical` — previously this exact breakage was invisible.
    #[test]
    fn detects_mixed_stale_wiring_fixture() {
        let dir = tempfile::tempdir().unwrap();
        stale_wiring_fixture(dir.path());
        assert_eq!(detect_hal_layout(dir.path()), HalLayout::Mixed);
    }

    /// The migration plan for a stale-wiring project must contain a real
    /// `rpi5/meson.build` build-file edit switching to `hal_impl_rpi5_sources`,
    /// not just a human note.
    #[test]
    fn plan_for_stale_wiring_rewrites_platform_meson() {
        let dir = tempfile::tempdir().unwrap();
        stale_wiring_fixture(dir.path());
        let plan = migrate_hal_plan(dir.path()).unwrap();
        assert_eq!(plan.layout, HalLayout::Mixed);

        let rpi5_edit = plan
            .build_file_edits
            .iter()
            .find(|e| e.file == "rpi5/meson.build")
            .expect("rpi5/meson.build must be in the plan edits");
        assert!(
            rpi5_edit.after.contains("hal_impl_rpi5_sources"),
            "rewritten rpi5 meson must reference the centralized var: {}",
            rpi5_edit.after
        );
        assert!(
            !rpi5_edit.after.contains("rpi5_hal_sources = files"),
            "legacy declaration must be removed: {}",
            rpi5_edit.after
        );
    }

    /// Apply on the stale-wiring fixture removes the orphan legacy `.cpp` and
    /// rewrites rpi5/meson.build to the centralized source var.
    #[test]
    fn apply_cleans_stale_wiring() {
        let dir = tempfile::tempdir().unwrap();
        stale_wiring_fixture(dir.path());
        let plan = migrate_hal_plan(dir.path()).unwrap();
        let result = migrate_hal_apply(dir.path(), &plan).unwrap();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // Orphan legacy impl moved/cleaned (the dir was removed when emptied).
        let rpi5 = std::fs::read_to_string(dir.path().join("rpi5/meson.build")).unwrap();
        assert!(
            rpi5.contains("hal_impl_rpi5_sources"),
            "rpi5 meson after apply: {rpi5}"
        );
        assert!(!rpi5.contains("rpi5_hal_sources = files"), "rpi5 meson after apply: {rpi5}");

        // After apply + the legacy dir cleanup, re-detect must no longer be Mixed.
        assert_eq!(detect_hal_layout(dir.path()), HalLayout::Canonical, "project should be canonical after apply");
    }

    /// `hal_sanity_check` must flag BOTH the orphaned legacy `.cpp` and stale
    /// platform wiring so a half-migrated project is never reported healthy.
    #[test]
    fn sanity_flags_stale_wiring_and_orphan_cpp() {
        let dir = tempfile::tempdir().unwrap();
        stale_wiring_fixture(dir.path());
        let report = hal_sanity_check(dir.path());
        assert_eq!(report.status, "issues");
        assert!(
            report.issues.iter().any(|i| i.title.contains("Implementation file left in legacy contract dir")),
            "issues: {:?}",
            report.issues
        );
        assert!(
            report.issues.iter().any(|i| i.title.contains("Mixed HAL layout")),
            "issues: {:?}",
            report.issues
        );
    }

    /// Pure-data struct headers (frame_buffer.hpp) in `hal/api` are NOT HAL
    /// interfaces AND produce NO sanity warning (they're legitimate shared
    /// support headers — this is the case the user reported). Only headers
    /// with no class/struct at all (free functions) are flagged misplaced.
    #[test]
    fn sanity_accepts_data_only_struct_headers_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "meson.build", "project('d', 'cpp')\nsubdir('hal')\n");
        write(dir.path(), "hal/api/camera_hal.hpp", CONTRACT_HPP);
        write(dir.path(), "hal/api/frame_buffer.hpp", "struct FrameBuffer { int w = 0; };\n");
        write(dir.path(), "hal/implementations/rpi5/camera_hal_imx219.cpp", "// impl\n");
        write(dir.path(), "hal/meson.build", "hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n");

        let report = hal_sanity_check(dir.path());
        assert!(
            report.interfaces.contains(&"camera_hal".to_string()),
            "interfaces: {:?}",
            report.interfaces
        );
        assert!(
            !report.interfaces.contains(&"frame_buffer".to_string()),
            "frame_buffer is not a contract: {:?}",
            report.interfaces
        );
        // DataOnly headers no longer warn — status must be healthy.
        assert_eq!(report.status, "ok", "issues: {:?}", report.issues);
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.title.contains("Non-contract header in HAL api dir")),
            "issues: {:?}",
            report.issues
        );
    }

    /// A free-function header (no class/struct) in the contract dir is still
    /// flagged as misplaced — it genuinely doesn't belong in hal/api.
    #[test]
    fn sanity_flags_free_function_header_in_api_dir() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "meson.build", "project('d', 'cpp')\nsubdir('hal')\n");
        write(dir.path(), "hal/api/camera_hal.hpp", CONTRACT_HPP);
        write(dir.path(), "hal/api/misc_helpers.hpp", "// free functions, no class\nint helper();\n");
        write(dir.path(), "hal/implementations/rpi5/camera_hal_imx219.cpp", "// impl\n");
        write(dir.path(), "hal/meson.build", "hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n");

        let report = hal_sanity_check(dir.path());
        assert_eq!(report.status, "issues");
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.title.contains("Non-contract header in HAL api dir")),
            "issues: {:?}",
            report.issues
        );
        assert!(
            !report.interfaces.contains(&"misc_helpers".to_string()),
            "interfaces: {:?}",
            report.interfaces
        );
    }
}
