//! Plan-then-apply HAL gap filling.
//! `plan()` is read-only; `apply()` executes an approved plan.
//! Gaps: "none" -> scaffold a NEW class (header + definition PAIR);
//!        "partial" -> add missing method bodies to an EXISTING .cpp.
//!
//! Every `hal_fill_plan` item carries the exact files it would write (a
//! `.hpp` declaration + `.cpp` definition pair for `none`), so the UI can
//! preview the files linter-style before applying — `apply()` renders through
//! the same generators to guarantee plan == apply.

use serde_json::json;
use std::path::Path;

/// Deriving impl class names + module-pair filenames is shared with the
/// semantic `hal_generate_impl` path (`generic_helpers::hal_impl_class_name`
/// / `generic_helpers::resolve_hal_impl_names`) so the fill scaffolding and
/// the LLM generation always agree on the concrete class + file names.
///

/// Render the full **definition** source a fill item would write (read-only)
/// — for a NEW class (`none`) this is the concrete `.cpp` (out-of-class
/// bodies); for a partial gap it is the `.gap.cpp` with only missing methods.
///
/// Both `plan()` (for the preview `content` field) and `apply()` (for the
/// actual write) go through this helper, so the file the user reviews is
/// byte-for-byte the file that lands on disk.
fn render_fill_item(root: &Path, item: &serde_json::Value) -> Option<String> {
    let plat = item.get("platform").and_then(|v| v.as_str())?;
    let iface = item.get("interface").and_then(|v| v.as_str())?;
    let kind = item.get("kind").and_then(|v| v.as_str())?;

    // Contract methods come DIRECTLY from the tree-sitter-C++ AST
    // (`extract_contract_methods_cpp`) — the same source `plan()` uses.
    // The historical `summarize_hal_header` → `parse_hal_contract_summary`
    // string round-trip was line-based and broke on multi-line parameter
    // lists (e.g. video_scaler::scale_nv12), producing "no classes".
    let header = root.join("hal").join("api").join(format!("{iface}.hpp"));
    let content = std::fs::read_to_string(&header).ok()?;
    let classes =
        crate::build::generic_helpers::extract_contract_methods_cpp(&content);
    let (class_name, methods) = classes.first()?;

    let src = if kind == "none" {
        // NEW class: the concrete class name is the impl-class (not the
        // contract's abstract name). Prefer the `class_name` the PLAN assigned
        // (variant-resolved) so plan == apply exactly; fall back to the
        // derived `<Interface><Platform>` for safety.
        let concrete = item
            .get("class_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| crate::build::generic_helpers::hal_impl_class_name(iface, plat));
        // The concrete class is declared in `namespace hal { … }` by the IMPL
        // header (`<iface>_<plat>.hpp`), which itself includes the contract
        // header. Emit the definitions INSIDE `namespace hal { … }` (matching
        // how the real ai-traps impls write `bool CameraHalRpi5::init(...)`),
        // and include the impl header (the class lives there, not in the
        // contract header). This keeps the pair byte-for-byte compileable.
        let sentinel = crate::build::generic_helpers::SPIRE_HAL_STUB_SENTINEL;
        let mut src = format!(
            "// {sentinel}: {iface}.cpp — {plat} implementation pending.\n\
             // Replace the TODO bodies below with a real implementation.\n\
             #pragma message(\"SPIRE HAL stub needs implementation: {iface}.cpp ({plat})\")\n\n"
        );
        let impl_hpp = std::path::Path::new(
            item.get("create_file").and_then(|v| v.as_str()).unwrap_or(""),
        )
        .with_extension("hpp")
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("{iface}.hpp"));
        src.push_str(&format!("#include \"{impl_hpp}\"\n\n"));
        src.push_str("namespace hal {\n\n");
        for m in methods {
            let ret = m.return_type.trim();
            let ret_void = ret.is_empty() || ret == "void";
            src.push_str(&format!(
                "{ret} {concrete}::{}({}) {{\n    /* TODO: implement for {plat} */\n",
                m.name, m.params
            ));
            if !ret_void {
                src.push_str("    return {};\n");
            }
            src.push_str("}\n\n");
        }
        src.push_str("} // namespace hal\n");
        src
    } else {
        // Partial-gap stubs carry the SAME machine-detectable sentinel +
        // compile-time `#pragma message` as full placeholders, so the
        // coverage/fill queue still reports them as "needs implementation"
        // until every missing method is actually filled.
        let sentinel = crate::build::generic_helpers::SPIRE_HAL_STUB_SENTINEL;
        let mut s = format!(
            "// {sentinel}: missing methods for {iface}.cpp — {plat} implementation pending.\n\
             // Replace the TODO bodies below with a real implementation.\n\
             #pragma message(\"SPIRE HAL stub needs implementation: {iface}.cpp ({plat}) — missing methods\")\n\n"
        );
        s.push_str(&format!("#include \"{iface}.hpp\"\n\n"));
        if let Some(sigs) = item.get("missing_sigs").and_then(|v| v.as_array()) {
            for ms in sigs {
                let name = ms.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let ret = ms.get("return_type").and_then(|v| v.as_str()).unwrap_or("");
                let params = ms.get("params").and_then(|v| v.as_str()).unwrap_or("");
                let void = ret.trim().is_empty() || ret.trim() == "void";
                s.push_str(&format!(
                    "{ret} {class_name}::{name}({params}) {{\n    /* TODO: implement for {plat} */\n"
                ));
                if !void {
                    s.push_str("    return {};\n");
                }
                s.push_str("}\n\n");
            }
        }
        s
    };
    Some(src)
}

/// Build a fill plan per platform/interface. Read-only — includes the exact
/// `content` each item would write so the UI can preview before applying.
pub fn plan(root: &Path, platform: &str, interfaces: &[String]) -> serde_json::Value {
    let coverage =
        crate::build::generic_helpers::hal_platform_coverage_map(root);
    let mut plan: Vec<serde_json::Value> = Vec::new();
    let mut plats: Vec<&String> = if platform.is_empty() {
        coverage.keys().collect()
    } else {
        coverage.keys().filter(|k| *k == platform).collect()
    };
    plats.sort();
    for plat in plats {
        let Some(ifaces) = coverage.get(plat) else { continue };
        let mut names: Vec<&String> = ifaces.keys().collect();
        names.sort();
        for iface in names {
            if !interfaces.is_empty() && !interfaces.contains(iface) {
                continue;
            }
            let cov = &ifaces[iface];
            if cov.implemented {
                continue;
            }
            let kind = if cov.has_impl { "partial" } else { "none" };
            let sigs: Vec<serde_json::Value> = cov
                .missing_sigs
                .iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "return_type": m.return_type,
                        "params": m.params,
                    })
                })
                .collect();
            let impl_dir = root.join("hal").join("implementations").join(plat);
            // Variant-aware naming for NEW classes: resolve (class, cpp, hpp)
            // filename tokens against the platform dir (no `_stub` suffix; the
            // SPIRE-HAL-STUB sentinel inside the files marks them pending).
            let (class_name, fname_cpp, fname_hpp) =
                if kind == "none" {
                    let (cn, cpp, hpp) = crate::build::generic_helpers::resolve_hal_impl_names(iface, plat, &impl_dir);
                    (cn, cpp, hpp)
                } else {
                    (crate::build::generic_helpers::hal_impl_class_name(iface, plat), format!("{iface}_gap.cpp"), String::new())
                };
            let create_file = impl_dir.join(&fname_cpp).to_string_lossy().to_string();
            let mut item = json!({
                "platform": plat,
                "interface": iface,
                "kind": kind,
                "action": if kind == "none" { "scaffold_new_class" } else { "add_missing_methods" },
                "create_file": create_file,
                "class_name": class_name,
                "missing": cov.missing,
                "missing_sigs": sigs,
                "meson_wiring": true,
            });
            // Render the DEFINITION (may be absent if header missing/no classes
            // — the apply step reports the same failure).
            if let Some(src) = render_fill_item(root, &item) {
                item["content"] = json!(src);
            }
            // NEW class → also render the concrete DECLARATION header so the
            // pair is previewed/written atomically (the agreed module-pair
            // model: contract .hpp immutable, impl .hpp + .cpp one unit).
            if kind == "none" {
                if let Ok(content) = std::fs::read_to_string(
                    root.join("hal").join("api").join(format!("{iface}.hpp"))
                ) {
                    let classes = crate::build::generic_helpers::extract_contract_methods_cpp(&content);
                    // The concrete class derives the CONTRACT's abstract class.
                    // Prefer the HalModule-derived base (canonical), falling back
                    // to the first declared abstract class (simplified contracts
                    // without the hal::HalModule base used in tests).
                    let base = crate::build::generic_helpers::extract_cpp_base_classes(&content)
                        .into_iter()
                        .find(|(_, bases)| bases.iter().any(|b| b == "HalModule"))
                        .map(|(name, _)| name)
                        .or_else(|| classes.first().map(|(name, _)| name.clone()));
                    if let Some(base) = base {
                        let methods: Vec<_> = classes.iter().flat_map(|(_, ms)| ms.clone()).collect();
                        let token = item
                            .get("class_name")
                            .and_then(|v| v.as_str())
                            .map(ToOwned::to_owned)
                            .unwrap_or_else(|| crate::build::generic_helpers::hal_impl_class_name(iface, plat));
                        let hpp_path = impl_dir.join(&fname_hpp).to_string_lossy().to_string();
                        let hpp = crate::build::generic_helpers::generate_hal_module_header(
                            iface,
                            &token,
                            &base,
                            &methods,
                            plat,
                        );
                        item["declaration_path"] = json!(hpp_path);
                        item["declaration_content"] = json!(hpp);
                    }
                }
            }
            plan.push(item);
        }
    }
    json!({
        "plan": plan,
        "note": "Review before applying; hal_fill_apply executes this plan."
    })
}

/// Write the `files(...)` source-list section for a platform's ACTUAL written
/// `.cpp` file names (e.g. `classifier_hal_rpi5.cpp`, `video_scaler_gap.cpp`).
///
/// Unlike `hal_meson_var_section` (which still targets legacy `_stub.cpp`
/// placeholders from `hal_add_target`/`hal_add_platform`), this emits the real
/// implementation files the plan wrote, so the build compiles the pair/patch
/// output instead of a non-existent stub.
fn hal_meson_files_section(platform: &str, cpp_file_names: &[String]) -> String {
    let entries: Vec<String> = cpp_file_names
        .iter()
        .map(|name| format!("        'implementations/{platform}/{name}',"))
        .collect();
    format!(
        "hal_impl_{platform}_sources = files(\n{}\n    )\n",
        entries.join("\n")
    )
}

/// Execute an approved plan. Returns written/failures/analysis status.
pub async fn apply(
    root: &Path,
    plan: &serde_json::Value,
    analyze: futures::future::BoxFuture<'_, Result<(), String>>,
) -> serde_json::Value {
    let mut written: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    // Platform → actual `.cpp` FILE NAMES written (not stems, so the meson
    // section references the real files: `classifier_hal_rpi5.cpp` for a new
    // class pair, `video_scaler_gap.cpp` for a partial gap).
    let mut plat_cpp_files: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    if let Some(items) = plan.as_array() {
        for item in items {
            let Some(plat) = item.get("platform").and_then(|v| v.as_str()) else { continue };
            let Some(iface) = item.get("interface").and_then(|v| v.as_str()) else { continue };
            let Some(create) = item.get("create_file").and_then(|v| v.as_str()) else { continue };
            let target = Path::new(create);
            // Render through the SAME helper as plan() — the previewed
            // content is exactly what gets written.
            let Some(src) = render_fill_item(root, item) else {
                failures.push(format!("{iface}: header not found or no classes"));
                continue;
            };
            match std::fs::write(&target, src) {
                Ok(()) => written.push(create.to_string()),
                Err(e) => failures.push(format!("{create}: {e}")),
            }
            // Module pair: for a NEW class (`kind == "none"`) the plan also
            // carried a concrete DECLARATION header — write it atomically with
            // the definition so the pair lands together.
            if item.get("kind").and_then(|v| v.as_str()) == Some("none") {
                if let (Some(hpp_path), Some(hpp_content)) = (
                    item.get("declaration_path").and_then(|v| v.as_str()),
                    item.get("declaration_content").and_then(|v| v.as_str()),
                ) {
                    match std::fs::write(Path::new(hpp_path), hpp_content) {
                        Ok(()) => written.push(hpp_path.to_string()),
                        Err(e) => failures.push(format!("{hpp_path}: {e}")),
                    }
                }
            }
            // Record the actual .cpp FILE NAME (basename) for meson wiring.
            if let Some(fname) = target.file_name().and_then(|s| s.to_str()) {
                let fname = fname.to_string();
                let list = plat_cpp_files.entry(plat.to_string()).or_default();
                if !list.contains(&fname) {
                    list.push(fname);
                }
            }
        }
    }
    // Wire hal/meson.build with each platform's ACTUAL written .cpp files
    // (idempotent: only append when the platform section is absent).
    for (plat, mut files) in plat_cpp_files {
        files.sort();
        let section = hal_meson_files_section(&plat, &files);
        let meson_path = root.join("hal").join("meson.build");
        if let Ok(existing) = std::fs::read_to_string(&meson_path) {
            if !existing.contains(&format!("hal_impl_{plat}_sources")) {
                let _ = std::fs::write(&meson_path, format!("{existing}\n{section}"));
            }
        }
    }
    let analysis_status = match analyze.await {
        Ok(()) => "re-analyzed".to_string(),
        Err(e) => format!("re-analyze failed: {e}"),
    };
    json!({ "written": written, "failures": failures, "analysis": analysis_status })
}
