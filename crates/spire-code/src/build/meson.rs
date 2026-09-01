// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Meson build module — meson.build analysis and build/test execution.

use async_trait::async_trait;
use std::path::Path;

use super::generic_helpers::{
    extract_cpp_base_classes, extract_cpp_method_definitions_ts, parse_cpp_source_file_std,
    parse_source_file_std,
};
use super::{
    BuildModuleMessage, BuildOptions, BuildOutput, ModuleCapability, TestOptions,
};

use super::generic_helpers::run_cmd;
use crate::Actor;
use spire_core::build_types::{BuildMetadata, BuildTarget};

/// Strip ANSI SGR escape sequences from compiler output (e.g. `\x1b[31m`).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_esc = false;
    for c in text.chars() {
        if c == '\u{1b}' {
            in_esc = true;
            continue;
        }
        if in_esc {
            // SGR sequences end with a letter (m), CSI with a range of codes.
            if c.is_ascii_alphabetic() {
                in_esc = false;
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Static Meson build module.
pub struct MesonBuildModule;

impl MesonBuildModule {
    pub fn new() -> Self {
        Self
    }

    fn analyze(&self, path: &Path) -> Result<BuildMetadata, String> {
        let mut content = std::fs::read_to_string(path.join("meson.build"))
            .map_err(|e| format!("Failed to read meson.build: {e}"))?;

        // Strip comments so `dependency(...)` inside comments is ignored.
        // (Meson uses `#` for line comments.)
        let comment_re = regex::Regex::new(r"(?m)#.*$").unwrap();
        content = comment_re.replace_all(&content, "").to_string();

        // Aggregate the root meson.build plus every subdir('X') meson.build so
        // dependency()/find_library()/executable() calls in platform subdir
        // files (e.g. rpi5/meson.build) are all visible to the parsers below.
        // Track section [start, end) offsets so per-target deps can be scoped
        // to the platform subdir that declares the executable — identically-
        // named vars (core_deps, platform_deps) exist in EVERY platform file
        // and must NOT bleed across targets.
        let mut aggregated = content.clone();
        let mut sections: Vec<(String, usize, usize)> = Vec::new(); // (subdir, start, end)
        let subdir_re = regex::Regex::new(r#"subdir\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
        for cap in subdir_re.captures_iter(&content) {
            if let Some(m) = cap.get(1) {
                // Subdir paths are relative to the directory of the parent
                // meson.build; at the scan root they descend from `path`.
                let sub = m.as_str().trim();
                if sub.is_empty() || sub.starts_with("..") {
                    continue;
                }
                let sub_meson = path.join(sub).join("meson.build");
                if let Ok(sub_content) = std::fs::read_to_string(&sub_meson) {
                    let sub_stripped = comment_re.replace_all(&sub_content, "");
                    let start = aggregated.len();
                    aggregated.push('\n');
                    aggregated.push_str(&sub_stripped);
                    sections.push((sub.to_string(), start, aggregated.len()));
                }
            }
        }

        // Extract `project('name', ...)` — the canonical project identity.
        let name = regex::Regex::new(r#"project\s*\(\s*['"]([^'"]+)['"]"#)
            .unwrap()
            .captures(&content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        // Extract `version : 'x.y.z'` from the project() call.
        let version = regex::Regex::new(r#"version\s*:\s*['"]([^'"]+)['"]"#)
            .unwrap()
            .captures(&content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        // Extract dependencies from Meson's functional API:
        //   dependency('libcamera', ...)
        //   find_library('tensorflow-lite', ...)
        //   declare_dependency(...)  (skipped — internal)
        let mut deps: Vec<spire_core::build_types::Dependency> = Vec::new();
        let dep_re = regex::Regex::new(r#"(?:dependency|find_library)\s*\(\s*['"]([^'"]+)['"]"#)
            .unwrap();
        let mut seen = std::collections::HashSet::new();
        for cap in dep_re.captures_iter(&aggregated) {
            if let Some(m) = cap.get(1) {
                let dep = m.as_str().trim().to_string();
                if dep.is_empty() || seen.contains(&dep) {
                    continue;
                }
                seen.insert(dep.clone());
                deps.push(spire_core::build_types::Dependency {
                    name: dep,
                    version_req: None,
                    kind: None,
                    ..Default::default()
                });
            }
        }

        // Extract build targets: executable('name', ...), library('name', ...),
        // shared_library(...), static_library(...), both_libraries(...).
        // These appear in platform subdir meson.build files (e.g. rpi5/meson.build)
        // and in the top-level meson.build. The names are what `meson compile
        // -C <builddir> <target>` accepts, letting the UI build one platform
        // target at a time.
        //
        // Multi-target Meson projects put `executable('ai-trap-rpi5', ...)` in
        // the platform subdir files (included via `subdir('rpi5')` from root), so
        // we must ALSO parse every subdir's meson.build — otherwise the root-only
        // parse reports zero targets.
        let tgt_kinds: &[(&str, &str)] = &[
            (r"executable", "executable"),
            (r"library", "library"),
            (r"shared_library", "shared_library"),
            (r"static_library", "static_library"),
            (r"both_libraries", "both_libraries"),
            (r"jar", "jar"),
            (r"shared_module", "shared_module"),
        ];
        let mut targets: Vec<BuildTarget> = Vec::new();
        let mut seen_targets = std::collections::HashSet::new();

        // Source-file regexes: every `'path.cpp'` literal and Meson `files(...)`
        // variable references appear in the executable declaration block.
        let file_literal_re =
            regex::Regex::new(r#"['"]([^'"]+\.(?:c|cpp|cc|cxx))['"]"#).unwrap();
        let var_ref_re = regex::Regex::new(r#"(?m)([a-z_][a-z0-9_]*)\s*[+]"#).unwrap();

        for (func, kind) in tgt_kinds {
            // `library` must NOT match inside `find_library(...)` / `shared_library(...)`
            // — those are dependency lookups or handled by their own kind. Use a
            // word-boundary so only a standalone `library('name', ...)` matches.
            // (Rust's regex crate lacks lookbehind, so emulate it with a boundary.)
            let (re, cap_idx): (regex::Regex, usize) = if *func == "library" {
                (
                    regex::Regex::new(
                        r#"(?:^|[^[:alnum:]_])library\s*\(\s*['"]([^'"]+)['"]"#,
                    )
                    .unwrap(),
                    1,
                )
            } else {
                (
                    regex::Regex::new(&format!(r#"{func}\s*\(\s*['"]([^'"]+)['"]"#)).unwrap(),
                    1,
                )
            };
            for cap in re.captures_iter(&aggregated) {
                if let Some(m) = cap.get(cap_idx) {
                    let name = m.as_str().trim().to_string();
                    if name.is_empty() || seen_targets.contains(&name) {
                        continue;
                    }
                    seen_targets.insert(name.clone());

                    // The executable's section = the platform subdir meson.build
                    // (or root content if the target is declared at root). All
                    // variable expansion for BOTH sources and deps is scoped to
                    // this section so identically-named vars (app_sources,
                    // core_deps, platform_deps) don't bleed across platforms.
                    let target_start = cap.get(0).map(|m| m.start()).unwrap_or(0);
                    let (sec_start, sec_end, platform) =
                        if let Some((sub, s, e)) = sections
                            .iter()
                            .find(|(_, s, e)| target_start >= *s && target_start < *e)
                        {
                            (*s, *e, sub.clone())
                        } else {
                            (0, target_start, "host".to_string())
                        };
                    let section = &aggregated[sec_start..sec_end];

                    // ── Source file extraction ──────────────────────────────
                    // Parse the source files from the executable/library
                    // declaration block. ai-traps uses:
                    //
                    //   executable('ai-trap-rock3c',
                    //     app_sources + rock3c_hal_sources + all_sources, ...)
                    //
                    // so we collect every `'x.cpp'` literal in the block plus
                    // the expanded `files('a.cpp', ...)` definitions of every
                    // variable referenced in the block.
                    let mut source_files: Vec<String> = Vec::new();
                    // Files contributed by the shared toolkit (`toolkit_sources`
                    // etc.) — tracked separately so `source_units` can classify
                    // role = Shared (vs App/HalImplementation for platform files).
                    let mut shared_files: Vec<String> = Vec::new();
                    let start = cap.get(0).map(|m| m.end()).unwrap_or(0);
                    let block = match aggregated[start..].find(')') {
                        Some(end) => &aggregated[start..start + end],
                        None => &aggregated[start..],
                    };

                    // Direct file literals in the executable(...) block.
                    for fcap in file_literal_re.captures_iter(block) {
                        if let Some(fm) = fcap.get(1) {
                            let p = fm.as_str().trim().to_string();
                            if !p.is_empty() && !source_files.contains(&p) {
                                source_files.push(p);
                            }
                        }
                    }

                    // Expand `files(...)` variable definitions referenced in the
                    // block. Handles BOTH direct assignment (`=` for rock3c's
                    // `rock3c_hal_sources = files(...)`) AND conditional append
                    // (`+=` for rpi5's `rpi5_hal_sources += files(...)` inside
                    // `if platform == 'rpi5'`), so each platform's HAL files are
                    // captured.
                    for vcap in var_ref_re.captures_iter(block) {
                        if let Some(vm) = vcap.get(1) {
                            let v = vm.as_str().trim().to_string();
                            let var_re = regex::Regex::new(&format!(
                                r#"(?m)^\s*{v}\s*(?:=|\+=)\s*files\(\s*([^)]+)\)"#
                            ))
                            .unwrap();
                            // Scope to the platform section so identically-named
                            // source vars (e.g. app_sources in rpi5 vs rock3c)
                            // don't cross-contaminate.
                            for vc in var_re.captures_iter(section) {
                                if let Some(g) = vc.get(1) {
                                    for fcap in file_literal_re.captures_iter(g.as_str()) {
                                        if let Some(fm) = fcap.get(1) {
                                            let p = fm.as_str().trim().to_string();
                                            if !p.is_empty() && !source_files.contains(&p) {
                                                source_files.push(p);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Include shared toolkit variables (defined in the toolkit/
                    // subdir, NOT inside this platform's section). These are
                    // intentionally resolved from the whole aggregated content.
                    // `hal_impl_*_sources` come from the hal/ subdir (the
                    // container layout) — outside this platform's section, like
                    // the toolkit vars — so they too resolve from `aggregated`.
                    let mut shared_var_names: Vec<String> = vec![
                        "toolkit_sources".to_string(),
                        "wifi_provisioning_source".to_string(),
                        "mjpeg_bridge_sources".to_string(),
                    ];
                    // Any var referenced by the block that's assigned a
                    // files(...) anywhere in aggregated and NOT already covered
                    // by the section-scoped expansion is a shared var (e.g. the
                    // hal/ container's hal_impl_<plat>_sources). A var whose
                    // files(...) definition lives IN THIS platform's section
                    // (e.g. `app_sources = files('main.cpp', ...)`) is
                    // platform-local — the section-scoped expansion already
                    // resolved it, and re-adding it as "shared" would move the
                    // platform's own files into shared_files, dropping the App
                    // source unit and the platform directory from the domain's
                    // file list.
                    for vcap in var_ref_re.captures_iter(block) {
                        if let Some(vm) = vcap.get(1) {
                            let v = vm.as_str().trim().to_string();
                            if shared_var_names.contains(&v) {
                                continue;
                            }
                            // Skip vars assigned files(...) inside the platform
                            // section — they are platform-scoped, not shared.
                            let section_assigned = regex::Regex::new(&format!(
                                r#"(?m)^\s*{v}\s*(?:=|\+=)\s*files\(\s*([^)]+)\)"#
                            ))
                            .unwrap()
                            .is_match(section);
                            if section_assigned {
                                continue;
                            }
                            if regex::Regex::new(&format!(
                                r#"(?m)^\s*{v}\s*=\s*files\(\s*([^)]+)\)"#
                            ))
                            .unwrap()
                            .is_match(&aggregated)
                            {
                                shared_var_names.push(v);
                            }
                        }
                    }
                    for var_name in shared_var_names {
                        let is_hal_var = var_name.starts_with("hal_impl_");
                        let var_re = regex::Regex::new(&format!(
                            r#"(?m)^\s*{var_name}\s*=\s*files\(\s*([^)]+)\)"#
                        ))
                        .unwrap();
                        for vc in var_re.captures_iter(&aggregated) {
                            if let Some(g) = vc.get(1) {
                                for fcap in file_literal_re.captures_iter(g.as_str()) {
                                    if let Some(fm) = fcap.get(1) {
                                        let p = fm.as_str().trim().to_string();
                                        if !p.is_empty() && !source_files.contains(&p) {
                                            source_files.push(p.clone());
                                        }
                                        // HAL implementation files are per-platform
                                        // HalImplementation sources, NOT shared —
                                        // only toolkit vars are role=Shared.
                                        if !is_hal_var && !p.is_empty() && !shared_files.contains(&p)
                                        {
                                            shared_files.push(p);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Sort + dedupe for deterministic output.
                    source_files.sort();
                    source_files.dedup();
                    let platform_name = platform.clone();

                    // ── Per-target dependency extraction ───────────────────
                    // Dependencies exist SEPARATELY per platform, exactly like
                    // source files: rock3c links rknnrt/mpp/rga, rpi5 links
                    // libcamera/tflite/edgetpu — plus the shared core deps.
                    // Resolve the executable's `dependencies:` expression to
                    // the concrete dependency/find_library names, scoped to
                    // the platform section (same section used for sources).
                    let target_deps = resolve_target_deps(&aggregated, section, block);

                    // Classify this platform target's source files into
                    // App / HalImplementation / Shared source units. The HAL
                    // contract + platform implementation become first-class,
                    // mirroring the product model: app code, HAL impl code,
                    // and shared toolkit.
                    let mut app_paths: Vec<String> = Vec::new();
                    let mut hal_paths: Vec<String> = Vec::new();
                    for f in &source_files {
                        if shared_files.contains(f) {
                            continue;
                        }
                        // HAL impl files: legacy layout `<plat>/hal/*`, container
                        // layout `implementations/<plat>/*`.
                        if f.contains("hal/") || f.contains("implementations/") {
                            hal_paths.push(f.clone());
                        } else {
                            app_paths.push(f.clone());
                        }
                    }
                    let mut units = Vec::new();
                    if !app_paths.is_empty() {
                        units.push(spire_core::build_types::SourceUnit {
                            role: spire_core::build_types::SourceRole::App,
                            path: platform_name.clone(),
                            language: "C++".to_string(),
                        });
                    }
                    if !hal_paths.is_empty() {
                        units.push(spire_core::build_types::SourceUnit {
                            role: spire_core::build_types::SourceRole::HalImplementation,
                            path: format!("{}/hal", platform_name),
                            language: "C++".to_string(),
                        });
                    }
                    if !shared_files.is_empty() {
                        units.push(spire_core::build_types::SourceUnit {
                            role: spire_core::build_types::SourceRole::Shared,
                            path: "toolkit".to_string(),
                            language: "C++".to_string(),
                        });
                    }

                    targets.push(BuildTarget {
                        name,
                        kind: vec![kind.to_string()],
                        source_path: None,
                        source_files,
                        dependencies: target_deps,
                        platform,
                        source_kind: spire_core::build_types::SourceKind::Composite,
                        source_units: units,
                        ..Default::default()
                    });
                }
            }
        }

        // Parse meson_options.txt for a platform option:
        //   option('platform', type: 'string', value: 'host',
        //          description: 'Target platform. Valid values: host, rpi5')
        let mut platform_targets: Vec<String> = Vec::new();
        if let Ok(opts) = std::fs::read_to_string(path.join("meson_options.txt")) {
            if let Some(caps) =
                regex::Regex::new(r#"option\s*\(\s*['"]platform['"]"#).unwrap().captures(&opts)
            {
                let _ = caps; // marker found; now scrape the documented values
                // Match the description line "Valid values: host, rpi5"
                if let Some(vals) = regex::Regex::new(
                    r"(?i)valid\s+values\s*:\s*([A-Za-z0-9_ ,\-]+)",
                ).unwrap().captures(&opts) {
                    let v = vals.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                    platform_targets = v
                        .split(',')
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                }
                // Fallback: default value "value: 'host'"
                if platform_targets.is_empty() {
                    if let Some(dv) = regex::Regex::new(
                        r#"option\s*\(\s*['"]platform['"][^)]*value\s*:\s*['"]([^'"]+)['"]"#,
                    ).unwrap().captures(&opts) {
                        if let Some(m) = dv.get(1) {
                            platform_targets.push(m.as_str().to_string());
                        }
                    }
                }
            }
        }

        // ── HAL contract discovery (first-class abstraction) ───────────
        // Two supported layouts:
        //   NEW container:  hal/api/*.hpp          + hal/implementations/<plat>/*
        //   LEGACY ai-traps: toolkit/src/hal/api/*.hpp + <plat>/hal/*
        // There is one `HalInterface` per header stem (camera_hal, h264_encoder,
        // …); `implementations` lists every platform that ships an impl of it.
        // `issues` carry non-fatal diagnostics for missing/orphan/duplicate
        // implementations so import can surface structural problems.
        let (hal_interfaces, issues) = detect_hal_interfaces(path, &targets);

        // Scope shared vs platform deps: `core_deps` (shared) are resolved
        // per-target and appear on every platform; the rest are platform-only.
        // A dep present on >1 target is shared.
        for t in targets.iter_mut() {
            for d in t.dependencies.iter_mut() {
                d.scope = Some("platform".to_string());
            }
        }
        let shared_names: Vec<String> = if targets.len() > 1 {
            let first = &targets[0].dependencies;
            first
                .iter()
                .filter(|d| targets.iter().skip(1).all(|t| {
                    t.dependencies.iter().any(|td| td.name == d.name)
                }))
                .map(|d| d.name.clone())
                .collect()
        } else {
            Vec::new()
        };
        for t in targets.iter_mut() {
            for d in t.dependencies.iter_mut() {
                if shared_names.contains(&d.name) {
                    d.scope = Some("shared".to_string());
                }
            }
        }

        // Structural shape of this Meson project — drives how the UI presents
        // the tree:
        //   Hal           → hal_interfaces present (contracts + virtual per-platform targets)
        //   SingleSource  → multiple platform_targets but no HAL contracts
        //   Native        → single host project
        let structure = if !hal_interfaces.is_empty() {
            spire_core::build_types::ProjectStructure::Hal
        } else if platform_targets.iter().any(|p| p != "host") {
            spire_core::build_types::ProjectStructure::SingleSource
        } else {
            spire_core::build_types::ProjectStructure::Native
        };

        // ── Domain projection ─────────────────────────────────────────────
        // Named slices the UI selects and the LLM edits within. For the Hal
        // shape: `common` (toolkit + HAL contracts) + one platform domain per
        // non-host target (that platform's app + HAL impl, its deps, and its
        // build spec). Single-host projects get a single `common` domain only
        // when the project is actually composite; Native projects get none
        // (their filesystem subproject tree is the correct view).
        let domains: Vec<spire_core::build_types::ProjectDomain> = if structure
            == spire_core::build_types::ProjectStructure::Native
        {
            Vec::new()
        } else {
            use spire_core::build_types::{DomainEditability, ProjectDomain, SourceRole};

            let mut domains: Vec<ProjectDomain> = Vec::new();

            // ── common: shared toolkit + HAL contract headers ──────────
            let mut common_files: Vec<String> = Vec::new();
            let mut common_contracts: Vec<String> = Vec::new();
            // Contract headers from hal_interfaces (canonical + legacy paths).
            for i in &hal_interfaces {
                common_contracts.push(i.header_path.clone());
                common_files.push(i.header_path.clone());
            }
            // Shared source units across all targets (e.g. `toolkit`).
            for t in &targets {
                for u in &t.source_units {
                    if u.role == SourceRole::Shared && !common_files.contains(&u.path) {
                        common_files.push(u.path.clone());
                    }
                }
            }
            if !common_files.is_empty() || !common_contracts.is_empty() {
                // `common` owns NO dependencies. It is a shared-context slice
                // (toolkit + contracts), not a buildable target — every dep is
                // resolved per platform target, and shared deps already appear
                // on each platform's domain. Attaching them here made the
                // "common" row in the UI list the whole project's dependencies.
                domains.push(ProjectDomain {
                    id: "common".to_string(),
                    name: "Common".to_string(),
                    kind: "common".to_string(),
                    files: common_files,
                    dependencies: Vec::new(),
                    build_spec: None,
                    // `common` is shared, editable toolkit + contracts — NOT
                    // read-only. Contract changes go through the HAL tools, but
                    // the shared sources are fair game for edits.
                    editability: DomainEditability::Shared,
                    contracts: common_contracts,
                });
            }

            // ── platform: one domain per non-host composite target ──────
            for t in targets.iter().filter(|t| t.platform != "host") {
                // Clean, DIRECTORY-level slices of THIS platform only:
                //   App               → "<plat>/"
                //   HalImplementation → "hal/implementations/<plat>/"
                // Shared (`toolkit/`) and the contract headers belong to
                // `common` alone — never duplicated into platform domains.
                // (Source files are meson-relative and can't be mapped to the
                // tree without the container paths, so the domains carry the
                // two directories the UI shows.)
                let mut plat_files: Vec<String> = Vec::new();
                let mut has_hal_impl = false;
                for u in &t.source_units {
                    let p: Option<String> = match u.role {
                        SourceRole::App => Some(t.platform.clone()),
                        SourceRole::HalImplementation => {
                            has_hal_impl = true;
                            Some(format!("hal/implementations/{}", t.platform))
                        }
                        _ => None,
                    };
                    if let Some(p) = p {
                        if !plat_files.contains(&p) {
                            plat_files.push(p);
                        }
                    }
                }
                // Fallback: a platform target that references implementations/
                // sources still gets its container dir listed.
                if !has_hal_impl {
                    let p = format!("hal/implementations/{}", t.platform);
                    if !plat_files.contains(&p) {
                        plat_files.push(p);
                    }
                }
                let mut plat_deps: Vec<spire_core::build_types::Dependency> = Vec::new();
                for d in &t.dependencies {
                    if d.scope.as_deref() != Some("shared") {
                        plat_deps.push(d.clone());
                    }
                }
                domains.push(ProjectDomain {
                    id: t.platform.clone(),
                    name: t.platform.clone(),
                    kind: "platform".to_string(),
                    files: plat_files,
                    dependencies: plat_deps,
                    build_spec: t.build_spec.clone(),
                    editability: DomainEditability::Fillable,
                    contracts: Vec::new(),
                });
            }

            domains
        };

        Ok(BuildMetadata {
            project_name: name,
            description: None,
            version,
            config_files: vec!["meson.build".to_string()],
            project_path: Some(path.to_string_lossy().to_string()),
            build_system: "Meson".to_string(),
            project_type: "Meson_project".to_string(),
            dependencies: deps,
            platform_targets,
            structure,
            domains,
            targets,
            hal_interfaces,
            issues,
            ..Default::default()
        })
    }

    /// Resolve the Meson build directory: prefer an existing one discovered via
    /// compile_commands.json (e.g. `build-native` at the project root), otherwise
    /// fall back to the conventional `builddir`.
    fn build_dir(&self, path: &Path, platform: Option<&str>) -> String {
        // A platform selection (e.g. "rpi5") should pick build-rpi5 FIRST —
        // don't fall back to build-native just because it has a compile DB.
        if let Some(plat) = platform {
            let wanted = format!("build-{plat}");
            let found = self.find_named_build_dir(path, &wanted);
            if !found.as_os_str().is_empty() {
                return found.to_string_lossy().to_string();
            }
            return wanted;
        }
        let found = self.find_compile_db_dir(path);
        if !found.as_os_str().is_empty() {
            // Absolute path — Meson may run from a subproject cwd (e.g. toolkit/)
            // while the build dir lives at the project root (build-native),
            // so `-C` must be absolute to resolve correctly.
            found.to_string_lossy().to_string()
        } else {
            // No discovered build dir: use the conventional relative name.
            "builddir".to_string()
        }
    }

    async fn build(&self, path: &Path, opts: &BuildOptions) -> Result<BuildOutput, String> {
        let dir = self.build_dir(path, opts.platform.as_deref());

        // ── Cross-compilation: generate the Meson cross file from the
        // platform registry (`~/.spire/platforms/*.yaml` seed; the graph is
        // the canonical store but the YAML seed is source-of-truth for the
        // toolchain). When a platform resolves, write `<build-<id>>/<id>-cross.txt`
        // and provision the build dir with `meson setup … --cross-file` if it
        // doesn't exist yet; otherwise fall back to the existing build dir.
        if let Some(plat) = opts.platform.as_deref() {
            if let Some(spec) = spire_core::platform::CrossSpec::for_platform(plat) {
                // Sanity gate: fail fast when the target's sysroot is missing or
                // unpopulated instead of writing a cross file with
                // `--sysroot=<nonexistent>` and letting `meson setup` fail late
                // with a confusing toolchain error.
                if let Some(platform_def) = spire_core::build_types::Platform::from_registry(plat) {
                    let (ok, reason) = platform_def.sysroot_ok();
                    if !ok {
                        return Err(format!(
                            "platform '{plat}' cross-build blocked: {reason}. \
                             Populate the target sysroot (e.g. {}/usr) or fix the \
                             platform registry seed before cross-compiling.",
                            platform_def.sysroot.root
                        ));
                    }
                }
                if let Some(cross_content) = spec.meson_cross_file {
                    let cross_dir = path.join(&dir);
                    let _ = std::fs::create_dir_all(&cross_dir);
                    let cross_path = cross_dir.join(format!("{plat}-cross.txt"));
                    if let Err(e) = std::fs::write(&cross_path, &cross_content) {
                        return Err(format!("failed to write cross file {}: {e}", cross_path.display()));
                    }
                    let abs_cross = cross_path.to_string_lossy().to_string();

                    // If the build dir isn't configured yet, run `meson setup`.
                    let prod_path = path.join(&dir);
                    if !prod_path.join("build.ninja").exists()
                        && !prod_path.join("meson-info").exists()
                    {
                        let setup_args = vec![
                            "setup".to_string(),
                            dir.clone(),
                            path.to_string_lossy().to_string(),
                            format!("--cross-file={abs_cross}"),
                            format!("-Dplatform={plat}"),
                        ];
                        let refs: Vec<&str> = setup_args.iter().map(|s| s.as_str()).collect();
                        let setup = run_cmd(path, "meson", &refs).await?;
                        // Then compile in it.
                        let mut args = vec!["compile".to_string(), "-C".to_string(), dir.clone()];
                        if let Some(target) = &opts.target {
                            args.push(target.clone());
                        }
                        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                        let build = run_cmd(path, "meson", &refs).await?;
                        return Ok(BuildOutput {
                            success: setup.success && build.success,
                            command: format!(
                                "meson setup … --cross-file={abs_cross} && meson compile -C {dir}"
                            ),
                            duration_secs: setup.duration_secs + build.duration_secs,
                            output: format!("{}\n{}", setup.output, build.output),
                            exit_code: if setup.success && build.success {
                                Some(0)
                            } else {
                                build.exit_code.or(setup.exit_code)
                            },
                        });
                    }
                    // Existing build dir: just compile; cross file already
                    // written (same content) so any re-setup uses the registry
                    // definition.
                }
            }
        }

        let mut args = vec!["compile".to_string(), "-C".to_string(), dir.clone()];
        if let Some(target) = &opts.target {
            args.push(target.clone());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_cmd(path, "meson", &refs).await
    }

    async fn test(&self, path: &Path, _opts: &TestOptions) -> Result<BuildOutput, String> {
        let dir = self.build_dir(path, None);
        run_cmd(path, "meson", &["test", "-C", &dir]).await
    }

    async fn clean(&self, path: &Path) -> Result<BuildOutput, String> {
        // NEVER delete a build dir we discovered via compile_commands.json —
        // that is typically a project-root shared build dir (e.g. build-native)
        // that both subprojects and the actual developer workflow use; deleting
        // it would destroy the real build state. Instead, invoke meson's own
        // clean on it (removes build artifacts, keeps the configured build dir).
        let build_dir = self.find_compile_db_dir(path);
        if !build_dir.as_os_str().is_empty() {
            return run_cmd(path, "meson", &["compile", "-C", build_dir.to_str().unwrap_or(""), "--clean"]).await;
        }
        // No discovered build dir: use the conventional relative builddir.
        let dir = path.join("builddir");
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("Failed to remove {}: {}", dir.display(), e))?;
        }
        Ok(BuildOutput {
            success: true,
            command: format!("rm -rf {}", dir.display()),
            duration_secs: 0.0,
            output: format!("Removed {}", dir.display()),
            exit_code: Some(0),
        })
    }

    /// Collect C/C++ source files for lint/format/fix. Prefers the files that
    /// appear in Meson's `compile_commands.json` (the canonical build set) so
    /// lint/analyze matches exactly what the build compiles — otherwise every
    /// source file in the tree (including platform-specific ones not built on
    /// this host) fails on missing headers. Falls back to a recursive walk when
    /// no compile DB exists.
    fn source_files(&self, path: &Path) -> Vec<String> {
        let db = self.load_compile_commands(path);
        if !db.is_empty() {
            let mut files: Vec<String> = db.keys().cloned().collect();
            files.sort();
            return files;
        }
        const EXTS: &[&str] = &["c", "cc", "cpp", "cxx"];
        let mut out = Vec::new();
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                let ep = entry.path();
                if ep.is_dir() {
                    let nm = ep
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if nm == "builddir" || nm.starts_with("build") || nm.starts_with('.') {
                        continue;
                    }
                    stack.push(ep);
                } else if let Some(ext) = ep.extension().and_then(|e| e.to_str()) {
                    if EXTS.contains(&ext) {
                        out.push(ep.to_string_lossy().to_string());
                    }
                }
            }
        }
        out.sort();
        out
    }

    /// Resolve a tool binary, falling back to known macOS/Xcode locations
    /// (clang-format lives in CommandLineTools but is often not on PATH).
    fn tool_path(&self, name: &str) -> String {
        let candidates = [
            name.to_string(),
            format!("/Library/Developer/CommandLineTools/usr/bin/{name}"),
            format!("/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/{name}"),
            format!("/Applications/Xcode-beta.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/{name}"),
            format!("/opt/homebrew/bin/{name}"),
            format!("/usr/local/bin/{name}"),
        ];
        for c in &candidates {
            if std::path::Path::new(c).exists() {
                return c.clone();
            }
        }
        name.to_string()
    }

    /// Lint C/C++ sources using the clang static analyzer (`clang --analyze`).
    /// Uses any `build*/compile_commands.json` (Meson generates it) to supply
    /// real per-file include flags so project headers resolve; otherwise the
    /// analyzer logs missing-header errors instead of silently finding nothing.
    async fn lint(&self, path: &Path, platform: Option<&str>) -> Result<BuildOutput, String> {
        let db_dir = if let Some(plat) = platform {
            self.find_named_build_dir(path, &format!("build-{plat}"))
        } else {
            self.find_compile_db_dir(path)
        };
        let db = self.load_compile_commands_from(db_dir.clone());
        let files = self.source_files(path);
        if files.is_empty() {
            return Ok(BuildOutput {
                success: true,
                command: "clang --analyze".to_string(),
                duration_secs: 0.0,
                output: "No C/C++ source files to lint".to_string(),
                exit_code: Some(0),
            });
        }
        let mut success = true;
        let mut output_lines: Vec<String> = Vec::new();
        for file in &files {
            let (program, args) = self.analyzer_for_file(file, &db, &db_dir);
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            match run_cmd(path, &program, &arg_refs).await {
                Ok(o) => {
                    let t = strip_ansi(&o.output).trim().to_string();
                    if !t.is_empty() {
                        output_lines.push(t);
                    }
                    if !o.success {
                        success = false;
                    }
                }
                Err(e) => {
                    output_lines.push(e);
                    success = false;
                }
            }
        }
        Ok(BuildOutput {
            success,
            command: "clang --analyze (static analyzer)".to_string(),
            duration_secs: 0.0,
            output: if output_lines.is_empty() {
                format!(
                    "clang static analyzer: analyzed {} files; no issues reported",
                    files.len()
                )
            } else {
                format!(
                    "clang static analyzer: analyzed {} files\n{}",
                    files.len(),
                    output_lines.join("\n---\n")
                )
            },
            exit_code: Some(if success { 0 } else { 1 }),
        })
    }

    /// Load `compile_commands.json` from any `build*` subdir (in this dir or
    /// any ancestor): file→flags map. Meson typically writes the build dir at
    /// the project root while subprojects are nested below it.
    fn load_compile_commands(
        &self,
        path: &Path,
    ) -> std::collections::HashMap<String, (Vec<String>, String)> {
        let mut search = path.to_path_buf();
        loop {
            let map = self.load_compile_commands_in(&search);
            if !map.is_empty() {
                return map;
            }
            if !search.pop() {
                break;
            }
        }
        std::collections::HashMap::new()
    }

    /// Load compile_commands.json from a specific build dir (path = the dir
    /// containing the compile DB, or "" to use the legacy walk-up discovery).
    fn load_compile_commands_from(
        &self,
        build_dir: std::path::PathBuf,
    ) -> std::collections::HashMap<String, (Vec<String>, String)> {
        if build_dir.as_os_str().is_empty() {
            return self.load_compile_commands(std::path::Path::new("/"));
        }
        let mut map = std::collections::HashMap::new();
        let db_path = build_dir.join("compile_commands.json");
        let Ok(content) = std::fs::read_to_string(&db_path) else {
            return map;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
            return map;
        };
        let Some(arr) = v.as_array() else { return map };
        for ent in arr {
            let (Some(file), Some(cmd)) = (
                ent.get("file").and_then(|v| v.as_str()),
                ent.get("command").and_then(|v| v.as_str()),
            ) else { continue };
            let dir = ent.get("directory").and_then(|v| v.as_str()).unwrap_or("");
            let canon = if file.starts_with('/') {
                file.to_string()
            } else if !dir.is_empty() {
                std::path::Path::new(dir).join(file).to_string_lossy().to_string()
            } else {
                build_dir.join(file).to_string_lossy().to_string()
            };
            let flags: Vec<String> = cmd
                .split_whitespace()
                .skip(1)
                .filter(|t| {
                    t.starts_with("-I") || t.starts_with("-D") || t.starts_with("-std=")
                        || t.starts_with("-W") || t.starts_with("-f") || t.starts_with("-m")
                        || t.starts_with("-isystem") || t.starts_with("-isysroot")
                })
                .map(|t| {
                    if !dir.is_empty() && t.starts_with("-I") && t.len() > 2 {
                        let p = &t[2..];
                        if !p.starts_with('/') {
                            let abs = std::path::Path::new(dir).join(p);
                            format!("-I{}", abs.to_string_lossy())
                        } else {
                            t.to_string()
                        }
                    } else {
                        t.to_string()
                    }
                })
                .collect();
            let comp_tokens: Vec<&str> = cmd.split_whitespace().collect();
            let compiler = if let Some(first) = comp_tokens.first() {
                if first.ends_with("sccache") && comp_tokens.len() > 1 {
                    comp_tokens[1].to_string()
                } else {
                    first.to_string()
                }
            } else {
                "c++".to_string()
            };
            map.insert(canon, (flags, compiler));
        }
        map
    }

    /// Build dir where the compile database was found ("" if none).
    fn find_compile_db_dir(&self, path: &Path) -> std::path::PathBuf {
        self.find_named_build_dir(path, "")
    }

    /// Find a build dir by name. Empty  = any  dir with a
    /// compile_commands.json (legacy behavior); non-empty = exact name match.
    fn find_named_build_dir(&self, path: &Path, wanted: &str) -> std::path::PathBuf {
        let mut search = path.to_path_buf();
        loop {
            let Ok(rd) = std::fs::read_dir(&search) else { return std::path::PathBuf::new() };
            for entry in rd.flatten() {
                let ep = entry.path();
                let name = ep
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if ep.is_dir()
                    && name.starts_with("build")
                    && ep.join("compile_commands.json").exists()
                    && (wanted.is_empty() || name == wanted)
                {
                    return ep;
                }
            }
            if !search.pop() {
                break;
            }
        }
        std::path::PathBuf::new()
    }

    fn load_compile_commands_in(
        &self,
        path: &Path,
    ) -> std::collections::HashMap<String, (Vec<String>, String)> {
        let mut map = std::collections::HashMap::new();
        let Ok(rd) = std::fs::read_dir(path) else { return map };
        for entry in rd.flatten() {
            let ep = entry.path();
            let name = ep
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if !ep.is_dir() || !name.starts_with("build") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(ep.join("compile_commands.json")) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let Some(arr) = v.as_array() else { continue };
            for ent in arr {
                let (Some(file), Some(cmd)) = (
                    ent.get("file").and_then(|v| v.as_str()),
                    ent.get("command").and_then(|v| v.as_str()),
                ) else { continue };
                let dir = ent.get("directory").and_then(|v| v.as_str()).unwrap_or("");
                let canon = if file.starts_with('/') {
                    file.to_string()
                } else if !dir.is_empty() {
                    std::path::Path::new(dir).join(file).to_string_lossy().to_string()
                } else {
                    path.join(file).to_string_lossy().to_string()
                };
                let flags: Vec<String> = cmd
                    .split_whitespace()
                    .skip(1)
                    .filter(|t| {
                        t.starts_with("-I") || t.starts_with("-D") || t.starts_with("-std=")
                            || t.starts_with("-W") || t.starts_with("-f") || t.starts_with("-m")
                            || t.starts_with("-isystem") || t.starts_with("-isysroot")
                    })
                    .map(|t| {
                        // -I paths are relative to the BUILD dir; make them
                        // absolute so lint works regardless of the cwd.
                        if !dir.is_empty() && t.starts_with("-I") && t.len() > 2 {
                            let p = &t[2..];
                            if !p.starts_with('/') {
                                let abs = std::path::Path::new(dir).join(p);
                                format!("-I{}", abs.to_string_lossy())
                            } else {
                                t.to_string()
                            }
                        } else {
                            t.to_string()
                        }
                    })
                    .collect();
                // The first token of the compile command is the compiler
                // (often wrapped by sccache: "sccache c++ ..."). Resolve it so
                // lint runs with the same compiler the build uses — otherwise
                // clang/clang++ may not find the C++ standard library headers.
                let comp_tokens: Vec<&str> = cmd.split_whitespace().collect();
                let compiler = if let Some(first) = comp_tokens.first() {
                    if first.ends_with("sccache") && comp_tokens.len() > 1 {
                        comp_tokens[1].to_string() // real compiler behind sccache
                    } else {
                        first.to_string()
                    }
                } else {
                    "c++".to_string()
                };
                map.insert(canon, (flags, compiler));
            }
        }
        map
    }

    /// Pick the build compiler + per-file flags for `--analyze` from the db.
    /// `db_dir` is the build dir whose compile_commands.json we loaded; if
    /// the db is missing we still analyze with the toolchain clang, adding
    /// -stdlib=libc++ so Apple clang --analyze can find C++ standard headers.
    fn analyzer_for_file(
        &self,
        file: &str,
        db: &std::collections::HashMap<String, (Vec<String>, String)>,
        db_dir: &std::path::Path,
    ) -> (String, Vec<String>) {
        let mut flags: Vec<String> = Vec::new();
        let mut db_compiler: Option<String> = None;
        if let Some((f, c)) = db.get(file) {
            flags = f.clone();
            db_compiler = Some(c.clone());
        } else {
            for (k, v) in db.iter() {
                if k.ends_with(file) || file.ends_with(k) {
                    flags = v.0.clone();
                    db_compiler = Some(v.1.clone());
                    break;
                }
            }
        }
        let is_cpp = file.ends_with(".cpp")
            || file.ends_with(".cc")
            || file.ends_with(".cxx")
            || file.ends_with(".c++");
        // Prefer the compiler the build actually uses (e.g. homebrew c++ via
        // sccache) — it knows where its own C++ standard headers live.
        let program = match db_compiler {
            Some(c) => c,
            None => self.tool_path(if is_cpp { "clang++" } else { "clang" }),
        };
        let mut args: Vec<String> = vec![
            "--analyze".to_string(),
            // The compile DB has -fdiagnostics-color=always; disable so the
            // captured output (and the diagnostics parser) has no ANSI codes.
            "-fno-diagnostics-color".to_string(),
        ];
        // Prefer flags from the compile database (already absolutized).
        // Fall back to db_dir + libc++ stdlib so headers resolve.
        let mut effective = flags;
        if effective.is_empty() {
            if let Some(dir) = db_dir.to_str() {
                effective.push(format!("-I{dir}"));
            }
            if is_cpp {
                effective.push("-stdlib=libc++".to_string());
            }
        }
        args.extend(effective);
        args.push(file.to_string());
        (program, args)
    }

    /// Check formatting with clang-format (--dry-run --Werror).
    async fn format(&self, path: &Path) -> Result<BuildOutput, String> {
        let files = self.source_files(path);
        if files.is_empty() {
            return Ok(BuildOutput {
                success: true,
                command: "clang-format".to_string(),
                duration_secs: 0.0,
                output: "No C/C++ source files to format".to_string(),
                exit_code: Some(0),
            });
        }
        let refs: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        let mut args: Vec<&str> = vec!["--dry-run", "--Werror"];
        args.extend(refs);
        let tool = self.tool_path("clang-format");
        run_cmd(path, &tool, &args).await
    }

    /// Auto-fix format issues by rewriting files in place with clang-format.
    async fn fix(&self, path: &Path) -> Result<BuildOutput, String> {
        let files = self.source_files(path);
        if files.is_empty() {
            return Ok(BuildOutput {
                success: true,
                command: "clang-tidy".to_string(),
                duration_secs: 0.0,
                output: "No C/C++ source files to fix".to_string(),
                exit_code: Some(0),
            });
        }
        let files_owned = files;
        let refs: Vec<&str> = files_owned.iter().map(|s| s.as_str()).collect();
        let mut args: Vec<&str> = vec!["-i"];
        args.extend(refs);
        let tool = self.tool_path("clang-format");
        run_cmd(path, &tool, &args).await
    }

    /// Scaffold a C/C++ Meson project, optionally as a multi-platform layout.
    ///
    /// `platforms` uses registry ids. When the list contains any id other than
    /// `"host"`, the ai-traps-style multi-platform layout is emitted: a root
    /// `meson.build` with `subdir('<id>')` per platform, a `meson_options.txt`
    /// `option('platform', …, values: [...])`, and a per-platform subdir
    /// `meson.build` declaring `executable('<name>-<id>', …)`. The cross files
    /// are generated at build time from the registry (not scaffolded).
    /// Otherwise the legacy single-target scaffold is produced unchanged.
    fn scaffold_layout(
        &self,
        project_name: &str,
        _goal: &str,
        platforms: &[String],
        structure: spire_core::build_types::ProjectStructure,
    ) -> Result<super::ScaffoldOutput, String> {
        let cross: Vec<&String> = platforms.iter().filter(|p| *p != "host").collect();
        if cross.is_empty() {
            // Legacy single-target scaffold.
            let bc = r#"project('__P__', 'c')

executable('__P__', 'src/main.c')
"#.replace("__P__", project_name);
            let sc = r#"#include <stdio.h>
int main() {
    printf("Hello from __P__!\n");
    return 0;
}
"#.replace("__P__", project_name);
            return Ok(super::ScaffoldOutput {
                build_file: "meson.build".to_string(),
                build_content: bc,
                source_dir: "src".to_string(),
                source_file: "main.c".to_string(),
                source_content: sc,
                files: Vec::new(),
                platform_targets: platforms.to_vec(),
                structure,
                ..Default::default()
            });
        }

        // Multi-platform layout mirroring ai-traps: root + per-platform subdir.
        // `Hal` additionally emits the `hal/` container (contract headers +
        // per-platform implementations) as the ai-traps restructuring describes.
        let mut files = Vec::new();
        if structure == spire_core::build_types::ProjectStructure::Hal {
            files.push(super::ScaffoldFile {
                path: "hal/api/README.md".to_string(),
                content: "HAL contract headers live here (e.g. camera_hal.hpp).\n".to_string(),
                structural: true,
                ..Default::default()
            });
            files.push(super::ScaffoldFile {
                path: "hal/meson.build".to_string(),
                content: format!(
                    "hal_impl_{0}_sources = files('implementations/{0}/gpio_hal_stub.cpp')\n",
                    cross[0]
                ),
                structural: true,
                ..Default::default()
            });
            files.push(super::ScaffoldFile {
                path: format!("hal/implementations/{}/gpio_hal_stub.cpp", cross[0]),
                content: "#include \"gpio_hal.hpp\"\n// TODO: implement for this platform\n"
                    .to_string(),
                structural: false,
                fill_role: Some(spire_core::build_types::SourceRole::HalImplementation),
            });
        }

        // Root meson.build: project() + subdir() per platform.
        let mut root = format!("project('{project_name}', 'cpp')\n\n");
        for plat in &cross {
            root.push_str(&format!("subdir('{plat}')\n"));
        }
        files.push(super::ScaffoldFile {
            path: "meson.build".to_string(),
            content: root,
            structural: true,
            ..Default::default()
        });

        // meson_options.txt declaring the platform option (structural — the
        // platform value list is locked; only per-leaf deps are fillable).
        let mut values: Vec<String> = Vec::new();
        if platforms.iter().any(|p| p == "host") {
            values.push("host".to_string());
        }
        values.extend(cross.iter().map(|p| p.to_string()));
        let values_line = values.iter().cloned().collect::<Vec<_>>().join(", ");
        files.push(super::ScaffoldFile {
            path: "meson_options.txt".to_string(),
            content: format!(
                "option('platform', type: 'string', value: '{}',\n       description: 'Target platform. Valid values: {}')\n",
                values.first().cloned().unwrap_or_else(|| "host".to_string()),
                values_line
            ),
            structural: true,
            ..Default::default()
        });

        // Per-platform subdir meson.build with shared core_deps + platform deps
        // and the executable() target (the analyzer + builder already handle
        // this layout — per-platform deps stay scoped by section).
        for plat in &cross {
            files.push(super::ScaffoldFile {
                path: format!("{plat}/meson.build"),
                content: format!(
                    "cpp = meson.get_compiler('cpp')\n\
                     core_deps = []\n\
                     platform_deps = []\n\
                     executable('{project_name}-{plat}',\n\
                       'main.cpp',\n\
                       dependencies: core_deps + platform_deps)\n"
                ),
                // Build wiring is structural; dep names change via the
                // declare_dependencies tool (platform_deps section).
                structural: true,
                ..Default::default()
            });
            // Shared main.cpp per platform (each subdir compiles its own).
            files.push(super::ScaffoldFile {
                path: format!("{plat}/main.cpp"),
                content: format!(
                    "#include <iostream>\nint main() {{\n    std::cout << \"Hello from {project_name}-{plat}!\" << std::endl;\n    return 0;\n}}\n"
                ),
                // Source stub — fillable by the LLM.
                structural: false,
                ..Default::default()
            });
        }

        // Fill roots: each platform subdir is a writable source leaf. The HAL
        // container's per-platform implementation dir is fillable too. New
        // subdirectories under a leaf are allowed; new subdir() at root is not.
        let mut fill_roots: Vec<String> = cross.iter().map(|p| p.to_string()).collect();
        if structure == spire_core::build_types::ProjectStructure::Hal {
            fill_roots.push(format!("hal/implementations/{}", cross[0]));
        }
        if cross.is_empty() {
            fill_roots = vec!["src".to_string()];
        }
        // Dependency sections: per-platform meson.build platform_deps.
        let dependency_sections: Vec<String> =
            cross.iter().map(|p| format!("{p}/meson.build")).collect();

        Ok(super::ScaffoldOutput {
            build_file: "meson.build".to_string(),
            build_content: files
                .iter()
                .find(|f| f.path == "meson.build")
                .map(|f| f.content.clone())
                .unwrap_or_default(),
            source_dir: cross
                .first()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "src".to_string()),
            source_file: "main.cpp".to_string(),
            source_content: String::new(),
            files,
            platform_targets: platforms.to_vec(),
            fill_roots,
            dependency_sections,
            structure,
            ..Default::default()
        })
    }
}

/// Detect the HAL contract headers and their per-platform implementations.
///
/// Layouts supported:
/// - New container: `hal/api/*.hpp` (contract) with `hal/implementations/<plat>/*`
/// - Legacy ai-traps: `toolkit/src/hal/api/*.hpp` with `<plat>/hal/*`
///
/// Only **abstract-class** headers count as contracts: a header must declare
/// at least one pure-virtual method (`= 0`). Struct headers (frame_buffer.hpp),
/// free-function headers (config_loader.hpp) and impl files are NOT HAL
/// interfaces, so `hal_add_target` never generates bogus placeholders for them.
///
/// Returns the interface list plus non-fatal `BuildIssue`s for structural
/// problems (missing impl, orphan impl, duplicate impl).
/// Resolve the impl directory for a platform (canonical or legacy layout).
fn dir_for(plat: &str, root: &Path) -> std::path::PathBuf {
    let canonical = root.join("hal").join("implementations").join(plat);
    if canonical.is_dir() {
        return canonical;
    }
    root.join(plat).join("hal")
}

fn detect_hal_interfaces(
    root: &Path,
    targets: &[BuildTarget],
) -> (Vec<spire_core::build_types::HalInterface>, Vec<spire_core::build_types::BuildIssue>) {
    let mut interfaces: Vec<spire_core::build_types::HalInterface> = Vec::new();
    let mut issues: Vec<spire_core::build_types::BuildIssue> = Vec::new();

    // Interface headers — scan both container layouts; prefer the new one.
    let mut header_dirs: Vec<std::path::PathBuf> = Vec::new();
    let new_api = root.join("hal").join("api");
    let legacy_api = root.join("toolkit").join("src").join("hal").join("api");
    if new_api.is_dir() {
        header_dirs.push(new_api.clone());
    }
    if legacy_api.is_dir() {
        header_dirs.push(legacy_api.clone());
    }

    /// A contract header declares at least one pure-virtual method: a
    /// `virtual … = 0`. Struct headers with default-member inits (`int w = 0;`)
    /// are excluded because the `= 0` is not preceded by `virtual`.
    fn is_contract_header(path: &Path) -> bool {
        use crate::build::generic_helpers::{classify_hal_header, HalHeaderKind};
        classify_hal_header(path) == HalHeaderKind::Contract
    }

    // Implementation dirs — new: hal/implementations/<plat>; legacy: <plat>/hal.
    let mut impl_dir_by_platform: std::collections::HashMap<String, std::path::PathBuf> =
        std::collections::HashMap::new();
    let new_impl_root = root.join("hal").join("implementations");
    if new_impl_root.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&new_impl_root) {
            for e in entries.flatten() {
                if e.path().is_dir() {
                    if let Some(plat) = e.file_name().to_str().map(|s| s.to_string()) {
                        impl_dir_by_platform.insert(plat, e.path());
                    }
                }
            }
        }
    }
    for t in targets {
        let plat = &t.platform;
        let legacy = root.join(plat).join("hal");
        if legacy.is_dir() {
            impl_dir_by_platform
                .entry(plat.clone())
                .or_insert_with(|| legacy.clone());
        }
    }

    // Build a set of interface stems from headers — only abstract-class
    // headers (≥1 pure-virtual `= 0` method) count as contracts.
    let mut stem_to_header: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for dir in &header_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let ep = e.path();
            let Some(ext) = ep.extension().and_then(|x| x.to_str()) else {
                continue;
            };
            if ext != "hpp" && ext != "h" {
                continue;
            }
            if !is_contract_header(&ep) {
                continue;
            }
            let Some(stem) = ep.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            stem_to_header
                .entry(stem.clone())
                .or_insert_with(|| ep.to_string_lossy().to_string());
        }
    }

    for (stem, header_path) in &stem_to_header {
        let stem = stem.clone();
        let header_path = header_path.clone();
        // Relative path for header_path (prefer shortest to one of the api dirs).
        let rel = std::path::Path::new(&header_path)
            .strip_prefix(root)
            .unwrap_or(std::path::Path::new(&header_path))
            .to_string_lossy()
            .to_string();
        // Gather implementations by platform using the C++ AST: an impl class
        // that public-inherits the interface class counts as an implementation
        // regardless of filename. A filename-stem fallback is kept only for
        // files that can't be parsed.
        //
        // The interface header's pure-virtual methods are the contract method
        // set; a platform with a subclass missing some of them gets a
        // structured "missing methods" issue for the LLM.
        let mut implementations: Vec<String> = Vec::new();
        let mut by_plat: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut contract_methods: Vec<String> = Vec::new();
        // The contract method set comes from the same tree-sitter-C++ CST the
        // coverage/fill queue uses (`extract_contract_methods_cpp`), so the
        // analyzer and the missing-implementation queue always agree.
        if let Ok(src) = std::fs::read_to_string(std::path::Path::new(&header_path)) {
            for (_, methods) in crate::build::generic_helpers::extract_contract_methods_cpp(&src) {
                for m in methods {
                    if !contract_methods.contains(&m.name) {
                        contract_methods.push(m.name);
                    }
                }
            }
        }

        for (plat, dir) in &impl_dir_by_platform {
            let mut matched: Vec<String> = Vec::new();
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            for e in entries.flatten() {
                let ep = e.path();
                let en = ep.file_name().unwrap_or_default().to_string_lossy().to_string();
                let ext = ep.extension().and_then(|x| x.to_str()).unwrap_or("");
                if ext != "cpp" && ext != "cxx" && ext != "cc" && ext != "c" && ext != "hpp" && ext != "h" {
                    continue;
                }
                // AST inheritance match: the file defines a class whose base
                // list contains the interface stem.
                let ast_match = std::fs::read_to_string(&ep)
                    .ok()
                    .map(|src| {
                        extract_cpp_base_classes(&src)
                            .iter()
                            .any(|(_, bases)| bases.iter().any(|b| b == &stem))
                    })
                    .unwrap_or(false);
                // Method-coverage match: file defines (inline or out-of-line)
                // at least one of the interface's pure-virtual methods. Catches
                // impl dirs holding only `Class::method` defs (no class decl).
                let method_match = std::fs::read_to_string(&ep)
                    .ok()
                    .map(|src| {
                        if contract_methods.is_empty() {
                            return false;
                        }
                        let impls = extract_cpp_method_definitions_ts(&src);
                        let defined: std::collections::BTreeSet<&str> =
                            impls.iter().map(|m| m.name.as_str()).collect();
                        contract_methods.iter().any(|m| defined.contains(m.as_str()))
                    })
                    .unwrap_or(false);
                // Filename fallback: stem prefix (e.g. camera_hal_imx219.cpp).
                let name_match = en.starts_with(&format!("{stem}.")) || en.starts_with(&format!("{stem}_"));
                if ast_match || method_match || name_match {
                    matched.push(en.clone());
                }
            }
                // Legacy naming: impls directly in <plat>/hal or via the platform dir.
                let plat_hdr_dir = root.join(plat).join("hal");
                if plat_hdr_dir.is_dir() && plat_hdr_dir != *dir {
                    if let Ok(entries) = std::fs::read_dir(&plat_hdr_dir) {
                        for e in entries.flatten() {
                            let ep = e.path();
                            let en = ep.file_name().unwrap_or_default().to_string_lossy().to_string();
                            let ext = ep.extension().and_then(|x| x.to_str()).unwrap_or("");
                            if ext != "cpp" && ext != "cxx" && ext != "cc" && ext != "c" {
                                continue;
                            }
                            let ast_match = std::fs::read_to_string(&ep)
                                .ok()
                                .map(|src| {
                                    extract_cpp_base_classes(&src)
                                        .iter()
                                        .any(|(_, bases)| bases.iter().any(|b| b == &stem))
                                })
                                .unwrap_or(false);
                            let method_match = std::fs::read_to_string(&ep).ok().map(|src| {
                                if contract_methods.is_empty() { return false; }
                                let impls = extract_cpp_method_definitions_ts(&src);
                                let defined: std::collections::BTreeSet<&str> =
                                    impls.iter().map(|m| m.name.as_str()).collect();
                                contract_methods.iter().any(|m| defined.contains(m.as_str()))
                            }).unwrap_or(false);
                            let name_match = en.starts_with(&format!("{stem}.")) || en.starts_with(&format!("{stem}_"));
                            if ast_match || method_match || name_match {
                                matched.push(en.clone());
                            }
                        }
                    }
                }
            if !matched.is_empty() {
                by_plat.insert(plat.clone(), matched);
            }
        }

        // For each platform with a match, diff the contract's pure-virtual
        // methods against the impl's defined methods (aggregated across all
        // matching files) — report the missing method names for the LLM.
        let mut missing_methods_by_plat: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (plat, files) in &by_plat {
            if files.len() > 1 {
                issues.push(spire_core::build_types::BuildIssue {
                    severity: "warning".to_string(),
                    kind: "duplicate_implementation".to_string(),
                    message: format!(
                        "HAL interface {stem} has {} implementations for {plat}: {}",
                        files.len(),
                        files.join(", ")
                    ),
                });
            }
            implementations.push(plat.clone());
            // Aggregate defined method names from all matching files using the
            // same tree-sitter-C++ CST the coverage/fill queue uses, so the
            // analyzer's "missing methods" diagnostics match the queue exactly.
            let mut defined: std::collections::BTreeSet<String> = Default::default();
            for f in files {
                let p = dir_for(plat, root).join(f);
                if let Ok(src) = std::fs::read_to_string(&p) {
                    for m in extract_cpp_method_definitions_ts(&src) {
                        defined.insert(m.name);
                    }
                }
            }
            let missing: Vec<String> = contract_methods
                .iter()
                .filter(|m| !defined.contains(*m))
                .cloned()
                .collect();
            if !missing.is_empty() {
                missing_methods_by_plat.insert(plat.clone(), missing.clone());
            }
        }

        // Missing-at-all + partially-missing diagnostics.
        for t in targets {
            if t.platform == "host" {
                continue;
            }
            if implementations.contains(&t.platform) {
                if let Some(missing) = missing_methods_by_plat.get(&t.platform) {
                    issues.push(spire_core::build_types::BuildIssue {
                        severity: "warning".to_string(),
                        kind: "missing_implementation".to_string(),
                        message: format!(
                            "HAL interface {stem} for platform {} missing methods: {}",
                            t.platform,
                            missing.join(", ")
                        ),
                    });
                }
            } else {
                issues.push(spire_core::build_types::BuildIssue {
                    severity: "warning".to_string(),
                    kind: "missing_implementation".to_string(),
                    message: format!(
                        "HAL interface {stem} has no implementation for platform {}",
                        t.platform
                    ),
                });
            }
        }

        interfaces.push(spire_core::build_types::HalInterface {
            name: stem.clone(),
            header_path: rel,
            implementations,
        });
    }

    // Orphan implementations: impl files with no matching interface header.
    for (plat, dir) in &impl_dir_by_platform {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let en = e.file_name().to_string_lossy().to_string();
            if !en.ends_with(".cpp") && !en.ends_with(".cxx") && !en.ends_with(".cc") {
                continue;
            }
            let Some(stem) = en.split('.').next().map(String::from) else { continue };
            let has_match = stem_to_header
                .keys()
                .any(|s| stem.starts_with(&format!("{s}_")) || stem == *s);
            if !has_match {
                issues.push(spire_core::build_types::BuildIssue {
                    severity: "warning".to_string(),
                    kind: "orphan_implementation".to_string(),
                    message: format!("HAL implementation {en} in {plat} has no interface header"),
                });
            }
        }
    }

    // If no HAL interfaces were found, clear the issues to avoid false
    // warnings on plain Meson projects (no HAL concept).
    if interfaces.is_empty() {
        issues.clear();
    }

    (interfaces, issues)
}

/// Resolve the concrete dependency names for a build target from the
/// executable(...) parameters block (`block`) within the platform section
/// (`section` = the contiguous region of `aggregated` containing the target's
/// subdir meson.build). Dependencies are per-platform (rock3c: rknnrt/mpp/rga;
/// rpi5: libcamera/tflite/edgetpu), so the walk is scoped to that section —
/// otherwise identically-named `core_deps`/`platform_deps` vars from other
/// platforms bleed into the result.
fn resolve_target_deps(
    _aggregated: &str,
    section: &str,
    block: &str,
) -> Vec<spire_core::build_types::Dependency> {
    let dep_call_re =
        regex::Regex::new(r#"(?:dependency|find_library)\s*\(\s*['"]([^'"]+)['"]"#).unwrap();
    let var_assign_re = regex::Regex::new(r#"(?m)^\s*([a-z_][a-z0-9_]*)\s*(?:=|\+=)\s*\[([^\]]*)\]"#)
        .unwrap();
    let var_ref_re = regex::Regex::new(r#"(?m)([a-z_][a-z0-9_]*)"#).unwrap();

    let mut resolved: Vec<spire_core::build_types::Dependency> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Directly-invoked deps in the executable(...) block.
    for cap in dep_call_re.captures_iter(block) {
        if let Some(m) = cap.get(1) {
            let name = m.as_str().trim().to_string();
            if name.is_empty() || seen.contains(&name) {
                continue;
            }
            seen.insert(name.clone());
            resolved.push(spire_core::build_types::Dependency {
                name,
                ..Default::default()
            });
        }
    }

    // Expand the dependency variables referenced by the block (e.g.
    // `core_deps + platform_deps` in the `dependencies:` argument). Their
    // array assignments look like:
    //
    //   core_deps = []
    //   foreach d : [nlohmann_json_dep, yaml_cpp_dep, ...]
    //     if d.found()
    //       core_deps += d
    //     endif
    //   endforeach
    //
    //   platform_deps = []
    //   ...
    //   platform_deps += [rknn_dep, mpp_dep, rga_dep]
    //
    // Each element is itself a dependency variable (`rknn_dep = cpp.find_library('rknnrt', ...)`
    // or `nlohmann_json_dep = dependency('nlohmann_json', ...)`). We resolve the
    // chain: variable → (assignment | dependency call | find_library call).
    let dep_var_re = regex::Regex::new(
        r#"(?m)^\s*([a-z_][a-z0-9_]*)\s*=\s*(?:(?:cpp|cc|meson)\s*\.\s*)?(?:dependency|find_library)\s*\(\s*['"]([^'"]+)['"]"#,
    )
    .unwrap();
    let mut declared: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for cap in var_assign_re.captures_iter(section) {
        let (Some(var_m), Some(g)) = (cap.get(1), cap.get(2)) else { continue };
        let var = var_m.as_str().trim().to_string();
        let mut names: Vec<String> = Vec::new();
        for vc in var_ref_re.captures_iter(g.as_str()) {
            if let Some(vm) = vc.get(1) {
                names.push(vm.as_str().trim().to_string());
            }
        }
        // Filter: only keep names that are actual dependency variables (they
        // end with `_dep` or `_deps` or are assigned from a dep call).
        names.retain(|n| {
            n.ends_with("_dep")
                || n.ends_with("_deps")
                || n.ends_with("_dependency")
                || n.ends_with("_dependencies")
        });
        if !names.is_empty() {
            declared.insert(var, names);
        }
    }

    // Map dependency variable names → the concrete dep/find_library name.
    let mut dep_var_to_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for cap in dep_var_re.captures_iter(section) {
        let (Some(vm), Some(nm)) = (cap.get(1), cap.get(2)) else { continue };
        dep_var_to_name.insert(vm.as_str().trim().to_string(), nm.as_str().trim().to_string());
    }

    // Collect all declaration names referenced in the executable block.
    let mut pending: Vec<String> = Vec::new();
    for vc in var_ref_re.captures_iter(block) {
        if let Some(vm) = vc.get(1) {
            let n = vm.as_str().trim().to_string();
            if !n.is_empty() && !pending.contains(&n) {
                pending.push(n);
            }
        }
    }
    while let Some(var) = pending.pop() {
        if let Some(dep_name) = dep_var_to_name.get(&var) {
            if !seen.contains(dep_name) {
                seen.insert(dep_name.clone());
                resolved.push(spire_core::build_types::Dependency {
                    name: dep_name.clone(),
                    ..Default::default()
                });
            }
        }
        if let Some(list) = declared.get(&var) {
            for inner in list {
                if !pending.contains(inner) {
                    pending.push(inner.clone());
                }
            }
        }
    }

    // Sort for deterministic output.
    resolved.sort_by(|a, b| a.name.cmp(&b.name));
    resolved
}

impl Default for MesonBuildModule {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Multi-target Meson layout mirroring ai-traps: root project() with
    /// subdir('rpi5')/subdir('rock3c'); the executable() targets live in the
    /// platform subdir meson.build files.
    fn write_multi_target_project(tmp: &std::path::Path) {
        std::fs::create_dir_all(tmp.join("rpi5")).unwrap();
        std::fs::create_dir_all(tmp.join("rock3c")).unwrap();
        std::fs::File::create(tmp.join("meson.build"))
            .unwrap()
            .write_all(
                b"project('ai-traps', 'cpp')\nsubdir('rpi5')\nsubdir('rock3c')\n",
            )
            .unwrap();
        std::fs::File::create(tmp.join("rpi5/meson.build"))
            .unwrap()
            .write_all(b"executable('ai-trap-rpi5', 'main.cpp')\n")
            .unwrap();
        std::fs::File::create(tmp.join("rock3c/meson.build"))
            .unwrap()
            .write_all(b"executable('ai-trap-rock3c', 'main.cpp')\n")
            .unwrap();
    }

    #[test]
    fn analyze_extracts_targets_from_subdir_meson_builds() {
        let tmp = tempfile::tempdir().unwrap();
        write_multi_target_project(tmp.path());

        let meta = MesonBuildModule::new().analyze(tmp.path()).unwrap();

        let names: Vec<&str> = meta.targets.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"ai-trap-rpi5"),
            "expected ai-trap-rpi5 target, got: {names:?}"
        );
        assert!(
            names.contains(&"ai-trap-rock3c"),
            "expected ai-trap-rock3c target, got: {names:?}"
        );
    }

    #[test]
    fn analyze_skips_subdirs_without_project() {
        let tmp = tempfile::tempdir().unwrap();
        write_multi_target_project(tmp.path());

        let meta = MesonBuildModule::new().analyze(tmp.path()).unwrap();

        // Only the root is a project: name must be ai-traps and exactly one
        // build system entry (no phantom subproject for rpi5/rock3c).
        assert_eq!(meta.build_system, "Meson");
        assert_eq!(meta.project_name.as_deref(), Some("ai-traps"));
    }

    /// Dependencies must be PER-TARGET like source files: rock3c links
    /// rknnrt/mpp/rga, rpi5 links libcamera/tflite/edgetpu — never merged.
    #[test]
    fn analyze_keeps_dependencies_separate_per_platform() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("rpi5")).unwrap();
        std::fs::create_dir_all(tmp.path().join("rock3c")).unwrap();
        std::fs::File::create(tmp.path().join("meson.build"))
            .unwrap()
            .write_all(
                b"project('ai-traps', 'cpp')\nsubdir('rpi5')\nsubdir('rock3c')\n",
            )
            .unwrap();

        // rpi5 platform: shared core deps + libcamera/tflite/edgetpu.
        std::fs::File::create(tmp.path().join("rpi5/meson.build"))
            .unwrap()
            .write_all(
                b"cpp = meson.get_compiler('cpp')\n\
                  nlohmann_json_dep = dependency('nlohmann_json', required: false)\n\
                  yaml_cpp_dep      = dependency('yaml-cpp', required: false)\n\
                  core_deps = [nlohmann_json_dep, yaml_cpp_dep]\n\
                  libcamera_dep = dependency('libcamera', required: true)\n\
                  tflite_dep    = cpp.find_library('tensorflow-lite', required: true)\n\
                  edgetpu_dep   = cpp.find_library('edgetpu', required: true)\n\
                  platform_deps = [libcamera_dep, tflite_dep, edgetpu_dep]\n\
                  executable('ai-trap-rpi5', 'main.cpp',\n\
                    dependencies: core_deps + platform_deps)\n",
            )
            .unwrap();

        // rock3c platform: shared core deps + rknnrt/mpp/rga.
        std::fs::File::create(tmp.path().join("rock3c/meson.build"))
            .unwrap()
            .write_all(
                b"cpp = meson.get_compiler('cpp')\n\
                  nlohmann_json_dep = dependency('nlohmann_json', required: false)\n\
                  yaml_cpp_dep      = dependency('yaml-cpp', required: false)\n\
                  core_deps = [nlohmann_json_dep, yaml_cpp_dep]\n\
                  rknn_dep  = cpp.find_library('rknnrt', required: true)\n\
                  mpp_dep   = cpp.find_library('rockchip_mpp', required: true)\n\
                  rga_dep   = cpp.find_library('rga', required: true)\n\
                  platform_deps = [rknn_dep, mpp_dep, rga_dep]\n\
                  executable('ai-trap-rock3c', 'main.cpp',\n\
                    dependencies: core_deps + platform_deps)\n",
            )
            .unwrap();

        let meta = MesonBuildModule::new().analyze(tmp.path()).unwrap();

        let rpi5 = meta
            .targets
            .iter()
            .find(|t| t.name == "ai-trap-rpi5")
            .expect("ai-trap-rpi5 target");
        let rpi5_deps: Vec<&str> = rpi5
            .dependencies
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert!(
            rpi5_deps.contains(&"libcamera"),
            "rpi5 missing libcamera — got {rpi5_deps:?}"
        );
        assert!(
            rpi5_deps.contains(&"tensorflow-lite"),
            "rpi5 missing tensorflow-lite — got {rpi5_deps:?}"
        );
        assert!(
            !rpi5_deps.contains(&"rknnrt"),
            "rpi5 must NOT contain rock3c-only rknnrt — got {rpi5_deps:?}"
        );

        let rock3c = meta
            .targets
            .iter()
            .find(|t| t.name == "ai-trap-rock3c")
            .expect("ai-trap-rock3c target");
        let rock3c_deps: Vec<&str> = rock3c
            .dependencies
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert!(
            rock3c_deps.contains(&"rknnrt"),
            "rock3c missing rknnrt — got {rock3c_deps:?}"
        );
        assert!(
            rock3c_deps.contains(&"rockchip_mpp"),
            "rock3c missing rockchip_mpp — got {rock3c_deps:?}"
        );
        assert!(
            rock3c_deps.contains(&"rga"),
            "rock3c missing rga — got {rock3c_deps:?}"
        );
        assert!(
            !rock3c_deps.contains(&"libcamera"),
            "rock3c must NOT contain rpi5-only libcamera — got {rock3c_deps:?}"
        );
        // Shared core deps still appear on BOTH targets.
        assert!(
            rpi5_deps.contains(&"yaml-cpp") && rock3c_deps.contains(&"yaml-cpp"),
            "shared yaml-cpp must be on both targets — rpi5 {rpi5_deps:?} rock3c {rock3c_deps:?}"
        );
    }

    /// `find_library('rknnrt', ...)` is a DEPENDENCY lookup — it must never be
    /// treated as a `library` build target.
    /// rpi5's HAL sources use `rpi5_hal_sources += files(...)` inside an
    /// `if platform == 'rpi5'` block — the parser must capture those.
    #[test]
    fn analyze_captures_conditional_files_append_hal_sources() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("rpi5")).unwrap();
        std::fs::create_dir_all(tmp.path().join("toolkit")).unwrap();
        std::fs::create_dir_all(tmp.path().join("rpi5/hal")).unwrap();
        std::fs::File::create(tmp.path().join("meson.build"))
            .unwrap()
            .write_all(
                b"project('ai-traps', 'cpp')\nsubdir('toolkit')\nsubdir('rpi5')\n",
            )
            .unwrap();
        std::fs::File::create(tmp.path().join("toolkit/meson.build"))
            .unwrap()
            .write_all(b"toolkit_sources = files('src/actors/camera/camera_actor.cpp')\n")
            .unwrap();
        // Mirrors rpi5/meson.build: HAL files appended inside `if`.
        std::fs::File::create(tmp.path().join("rpi5/meson.build"))
            .unwrap()
            .write_all(
                b"platform = get_option('platform')\n\
                  rpi5_hal_sources = files()\n\
                  if platform == 'rpi5'\n\
                    rpi5_hal_sources += files('hal/camera_hal_rpi5.cpp',\n\
                                              'hal/h264_encoder_rpi5.cpp')\n\
                  endif\n\
                  app_sources = files('main.cpp', 'rpi5_detection_pipeline.cpp')\n\
                  executable('ai-trap-rpi5', app_sources + rpi5_hal_sources + toolkit_sources)\n",
            )
            .unwrap();

        let meta = MesonBuildModule::new().analyze(tmp.path()).unwrap();
        let rpi5 = meta
            .targets
            .iter()
            .find(|t| t.name == "ai-trap-rpi5")
            .expect("ai-trap-rpi5 target");
        assert!(
            rpi5.source_files.contains(&"hal/camera_hal_rpi5.cpp".to_string()),
            "missing camera_hal_rpi5.cpp — got {:?}",
            rpi5.source_files
        );
        assert!(
            rpi5.source_files.contains(&"hal/h264_encoder_rpi5.cpp".to_string()),
            "missing h264_encoder_rpi5.cpp — got {:?}",
            rpi5.source_files
        );
        assert!(
            rpi5.source_files.contains(&"src/actors/camera/camera_actor.cpp".to_string()),
            "missing shared toolkit source — got {:?}",
            rpi5.source_files
        );
    }

    /// New HAL container layout: `hal/api/*.hpp` contracts with
    /// `hal/implementations/<plat>/*` per-platform implementations. The
    /// analyzer discovers the interface set, maps implementations by stem,
    /// classifies source units, and reports missing implementations on the
    /// platform targets that lack them.
    #[test]
    fn analyze_discovers_hal_interfaces_in_container_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("rpi5")).unwrap();
        std::fs::create_dir_all(root.join("rock3c")).unwrap();
        std::fs::create_dir_all(root.join("toolkit")).unwrap();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rock3c")).unwrap();
        std::fs::File::create(root.join("meson.build"))
            .unwrap()
            .write_all(
                b"project('ai-traps', 'cpp')\nsubdir('toolkit')\nsubdir('hal')\nsubdir('rpi5')\nsubdir('rock3c')\n",
            )
            .unwrap();
        // Interface headers (abstract-class contract set).
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            "class CameraHAL {\npublic:\n    virtual bool start() = 0;\n};\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/api/h264_encoder.hpp"),
            "class H264Encoder {\npublic:\n    virtual bool encode() = 0;\n};\n",
        )
        .unwrap();
        // Shared toolkit source.
        std::fs::File::create(root.join("toolkit/meson.build")).unwrap().write_all(
            b"toolkit_sources = files('src/pipeline/base.cpp')\n",
        ).unwrap();
        // hal/meson.build: variables only — per-platform implementation file
        // lists (paths relative to the hal/ dir, i.e. implementations/<plat>/…).
        std::fs::File::create(root.join("hal/meson.build")).unwrap().write_all(
            b"hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n\
              hal_impl_rock3c_sources = files('implementations/rock3c/camera_hal_ov5647.cpp',\n\
                                               'implementations/rock3c/h264_encoder_mpp.cpp')\n",
        ).unwrap();
        // rpi5 platform: implements camera_hal only (h264_encoder missing).
        std::fs::write(root.join("hal/implementations/rpi5/camera_hal_imx219.cpp"), "").unwrap();
        std::fs::File::create(root.join("rpi5/meson.build")).unwrap().write_all(
            b"executable('ai-trap-rpi5', 'main.cpp' + hal_impl_rpi5_sources + toolkit_sources)\n",
        ).unwrap();
        // rock3c platform: implements camera_hal + h264_encoder.
        std::fs::write(root.join("hal/implementations/rock3c/camera_hal_ov5647.cpp"), "").unwrap();
        std::fs::write(root.join("hal/implementations/rock3c/h264_encoder_mpp.cpp"), "").unwrap();
        std::fs::File::create(root.join("rock3c/meson.build")).unwrap().write_all(
            b"executable('ai-trap-rock3c', 'main.cpp' + hal_impl_rock3c_sources + toolkit_sources)\n",
        ).unwrap();

        let meta = MesonBuildModule::new().analyze(root).unwrap();

        // HAL interfaces discovered from hal/api.
        let names: Vec<&str> = meta.hal_interfaces.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"camera_hal"), "interfaces: {names:?}");
        assert!(names.contains(&"h264_encoder"), "interfaces: {names:?}");

        // Implementation mapping by stem.
        let camera = meta.hal_interfaces.iter().find(|i| i.name == "camera_hal").unwrap();
        let h264 = meta.hal_interfaces.iter().find(|i| i.name == "h264_encoder").unwrap();
        assert!(camera.implementations.contains(&"rpi5".to_string()), "camera impls: {:?}", camera.implementations);
        assert!(camera.implementations.contains(&"rock3c".to_string()));
        assert!(h264.implementations.contains(&"rock3c".to_string()));
        assert!(!h264.implementations.contains(&"rpi5".to_string()));

        // Missing implementation diagnostic for rpi5 on h264_encoder.
        let missing: Vec<&str> = meta.issues.iter().filter(|i| i.kind == "missing_implementation").map(|i| i.message.as_str()).collect();
        assert!(
            missing.iter().any(|m| m.contains("h264_encoder") && m.contains("rpi5")),
            "expected rpi5 missing h264_encoder, got: {missing:?}"
        );

        // Source-unit classification: rpi5 target has App + HalImplementation
        // (its hal/ file) + Shared toolkit.
        let rpi5 = meta.targets.iter().find(|t| t.name == "ai-trap-rpi5").unwrap();
        let roles: Vec<&str> = rpi5.source_units.iter().map(|u| match u.role {
            spire_core::build_types::SourceRole::App => "app",
            spire_core::build_types::SourceRole::HalImplementation => "hal_implementation",
            spire_core::build_types::SourceRole::Shared => "shared",
            _ => "other",
        }).collect();
        assert!(roles.contains(&"app"), "roles: {roles:?}");
        assert!(roles.contains(&"hal_implementation"), "roles: {roles:?}");
        assert!(roles.contains(&"shared"), "roles: {roles:?}");
    }

    /// Legacy ai-traps layout (toolkit/src/hal/api + <plat>/hal) is recognized
    /// too, so nothing breaks before the hal/ container restructure lands.
    #[test]
    fn analyze_discovers_hal_interfaces_in_legacy_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("rpi5")).unwrap();
        std::fs::create_dir_all(root.join("toolkit/src/hal/api")).unwrap();
        std::fs::create_dir_all(root.join("rpi5/hal")).unwrap();
        std::fs::File::create(root.join("meson.build")).unwrap().write_all(
            b"project('ai-traps', 'cpp')\nsubdir('toolkit')\nsubdir('rpi5')\n",
        ).unwrap();
        std::fs::write(
            root.join("toolkit/src/hal/api/camera_hal.hpp"),
            "class CameraHAL {\npublic:\n    virtual bool start() = 0;\n};\n",
        )
        .unwrap();
        std::fs::write(root.join("rpi5/hal/camera_hal_imx219.cpp"), "").unwrap();
        std::fs::File::create(root.join("toolkit/meson.build")).unwrap().write_all(
            b"toolkit_sources = files('src/pipeline/base.cpp')\n",
        ).unwrap();
        std::fs::File::create(root.join("rpi5/meson.build")).unwrap().write_all(
            b"cam = files('hal/camera_hal_imx219.cpp')\n\
              executable('ai-trap-rpi5', 'main.cpp' + cam + toolkit_sources)\n",
        ).unwrap();

        let meta = MesonBuildModule::new().analyze(root).unwrap();
        assert_eq!(meta.hal_interfaces.len(), 1);
        assert_eq!(meta.hal_interfaces[0].name, "camera_hal");
        assert!(meta.hal_interfaces[0].implementations.contains(&"rpi5".to_string()));
    }

    /// Run the real analyzer + HAL sanity check against the ai-traps project
    /// (`/Users/steve/naturesense/ai-traps`). Enabled only when the
    /// `SPIRE_AI_TRAPS_INTEGRATION` env var is set so normal CI runs stay
    /// machine-independent. Verifies the post-cleanup canonical layout is
    /// detected and that legacy/contract issues are not misreported.
    #[test]
    fn analyze_real_ai_traps_project() {
        let Ok(root) = std::env::var("SPIRE_AI_TRAPS_INTEGRATION") else {
            eprintln!("skipped: set SPIRE_AI_TRAPS_INTEGRATION=/abs/path/ai-traps");
            return;
        };
        let root = std::path::PathBuf::from(root);
        assert!(root.join("meson.build").exists(), "ai-traps meson.build missing at {root:?}");
        assert!(root.join("hal/meson.build").exists(), "hal/meson.build missing at {root:?}");

        // ── 1. Analyzer HAL interface discovery ──────────────────────────
        let meta = MesonBuildModule::new().analyze(&root).unwrap();
        let names: Vec<&str> = meta.hal_interfaces.iter().map(|i| i.name.as_str()).collect();
        // Non-contract headers (types, frame_buffer) must NOT be interfaces.
        assert!(names.contains(&"camera_hal"), "interfaces: {names:?}");
        assert!(names.contains(&"h264_encoder"), "interfaces: {names:?}");
        assert!(!names.contains(&"types"), "types.hpp is not a contract: {names:?}");
        assert!(!names.contains(&"frame_buffer"), "frame_buffer.hpp is not a contract: {names:?}");
        assert!(!names.contains(&"config_loader"), "config_loader.hpp is not a contract: {names:?}");

        // Both platform targets resolved with HAL impl source units.
        let targets: Vec<&str> = meta.targets.iter().map(|t| t.name.as_str()).collect();
        assert!(targets.contains(&"ai-trap-rpi5"), "targets: {targets:?}");
        assert!(targets.contains(&"ai-trap-rock3c"), "targets: {targets:?}");

        // Regression (user-reported): `mpp_h264_encoder.cpp` in
        // hal/implementations/rock3c must satisfy the h264_encoder interface
        // via out-of-line method definitions (no filename prefix match), and
        // rock3c must NOT be reported missing h264_encoder methods.
        let h264 = meta
            .hal_interfaces
            .iter()
            .find(|i| i.name == "h264_encoder")
            .expect("h264_encoder interface");
        assert!(
            h264.implementations.contains(&"rock3c".to_string()),
            "h264_encoder must be implemented for rock3c: {:?}",
            h264.implementations
        );
        let missing_msgs: Vec<&str> = meta
            .issues
            .iter()
            .filter(|i| i.kind == "missing_implementation")
            .map(|i| i.message.as_str())
            .collect();
        assert!(
            !missing_msgs
                .iter()
                .any(|m| m.contains("h264_encoder") && m.contains("rock3c")),
            "spurious h264_encoder/rock3c missing method issue: {missing_msgs:?}"
        );


        // Platform DOMAINS list their app dir + HAL impl dir, so the UI shows
        // main.cpp / platform pipeline files in Sources (regression: app_sources
        // was misclassified as shared, dropping the <plat>/ dir). App target
        // sources must NOT be flagged shared.
        for plat in ["rpi5", "rock3c"] {
            let domain = meta
                .domains
                .iter()
                .find(|d| d.id == plat)
                .unwrap_or_else(|| panic!("missing platform domain {plat}: {:?}", meta.domains.iter().map(|d| d.id.as_str()).collect::<Vec<_>>()));
            assert!(
                domain.files.iter().any(|f| f == plat),
                "{plat} domain must list the app dir: {:?}",
                domain.files
            );
            assert!(
                domain.files.iter().any(|f| f == &format!("hal/implementations/{plat}")),
                "{plat} domain must list the HAL impl dir: {:?}",
                domain.files
            );
        }

        // ── 2. Migration/sanity: the cleaned project is canonical ────────
        let report = crate::build::hal_migration::hal_sanity_check(&root);
        assert_eq!(report.layout, "canonical", "layout after cleanup: {:?}", report.issues);
        // Pure-data headers (types.hpp, frame_buffer.hpp) must NOT be flagged —
        // this is the bug the user reported.
        assert_eq!(
            report.status, "ok",
            "sanity must be healthy — no warnings for pure-data headers: {:?}",
            report.issues
        );
        assert!(
            !report
                .issues
                .iter()
                .any(|i| i.title.contains("Non-contract header")),
            "pure-data struct headers must not warn: {:?}",
            report.issues
        );
        let plan = crate::build::hal_migration::migrate_hal_plan(&root).unwrap();
        assert_eq!(plan.layout_name, "canonical", "expected no-op plan");
        assert!(!plan.can_apply, "canonical project must not be migratable");
        assert!(plan.moves.is_empty(), "no file moves expected: {:?}", plan.moves);
    }

    /// Plain Meson projects without a hal/ layout produce no HAL interfaces
    /// and no spurious diagnostics.
    #[test]
    fn analyze_no_hal_on_plain_meson_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::File::create(root.join("meson.build")).unwrap().write_all(
            b"project('plain', 'cpp')\nexecutable('plain', 'main.cpp')\n",
        ).unwrap();
        let meta = MesonBuildModule::new().analyze(root).unwrap();
        assert!(meta.hal_interfaces.is_empty());
        assert!(meta.issues.is_empty());
    }

    /// The analyzer projects named `domains` for a Hal project: `common`
    /// (contract headers + shared toolkit, NO deps), plus one platform domain
    /// per target (`rpi5`, `rock3c`) carrying app + HAL impl pieces and
    /// platform-scoped deps. Native projects get no domains (their filesystem
    /// subproject tree is the correct view).
    #[test]
    fn analyze_projects_hal_domains() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rock3c")).unwrap();
        std::fs::create_dir_all(root.join("toolkit")).unwrap();
        std::fs::create_dir_all(root.join("rpi5")).unwrap();
        std::fs::create_dir_all(root.join("rock3c")).unwrap();
        std::fs::File::create(root.join("meson.build")).unwrap().write_all(
            b"project('ai-traps', 'cpp')\nsubdir('toolkit')\nsubdir('hal')\nsubdir('rpi5')\nsubdir('rock3c')\n",
        ).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            "class CameraHAL {\npublic:\n    virtual bool start() = 0;\n};\n",
        ).unwrap();
        std::fs::write(
            root.join("hal/api/h264_encoder.hpp"),
            "class H264Encoder {\npublic:\n    virtual bool encode() = 0;\n};\n",
        ).unwrap();
        std::fs::File::create(root.join("toolkit/meson.build")).unwrap().write_all(
            b"toolkit_sources = files('src/pipeline/base.cpp')\n",
        ).unwrap();
        std::fs::File::create(root.join("hal/meson.build")).unwrap().write_all(
            b"hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n\
              hal_impl_rock3c_sources = files('implementations/rock3c/camera_hal_ov5647.cpp',\n\
                                               'implementations/rock3c/h264_encoder_mpp.cpp')\n",
        ).unwrap();
        std::fs::write(root.join("hal/implementations/rpi5/camera_hal_imx219.cpp"), "").unwrap();
        std::fs::write(root.join("hal/implementations/rock3c/camera_hal_ov5647.cpp"), "").unwrap();
        std::fs::write(root.join("hal/implementations/rock3c/h264_encoder_mpp.cpp"), "").unwrap();
        // Per-target deps: rpi5 libcamera, rock3c rknn — plus shared yaml-cpp.
        std::fs::File::create(root.join("rpi5/meson.build")).unwrap().write_all(
            b"yaml_cpp_dep = dependency('yaml-cpp', required: false)\n\
              libcamera_dep = dependency('libcamera', required: true)\n\
              core_deps = [yaml_cpp_dep]\n\
              platform_deps = [libcamera_dep]\n\
              executable('ai-trap-rpi5', 'main.cpp' + hal_impl_rpi5_sources + toolkit_sources,\n\
                dependencies: core_deps + platform_deps)\n",
        ).unwrap();
        std::fs::File::create(root.join("rock3c/meson.build")).unwrap().write_all(
            b"yaml_cpp_dep = dependency('yaml-cpp', required: false)\n\
              rknn_dep = cpp.find_library('rknnrt', required: true)\n\
              core_deps = [yaml_cpp_dep]\n\
              platform_deps = [rknn_dep]\n\
              executable('ai-trap-rock3c', 'main.cpp' + hal_impl_rock3c_sources + toolkit_sources,\n\
                dependencies: core_deps + platform_deps)\n",
        ).unwrap();

        let meta = MesonBuildModule::new().analyze(root).unwrap();
        assert_eq!(meta.structure, spire_core::build_types::ProjectStructure::Hal);
        let ids: Vec<&str> = meta.domains.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains(&"common"), "domains: {ids:?}");
        assert!(ids.contains(&"rpi5"), "domains: {ids:?}");
        assert!(ids.contains(&"rock3c"), "domains: {ids:?}");

        let common = meta.domains.iter().find(|d| d.id == "common").unwrap();
        // Contracts are part of the common domain.
        assert!(common.contracts.iter().any(|c| c.contains("camera_hal")), "contracts: {:?}", common.contracts);
        assert!(common.contracts.iter().any(|c| c.contains("h264_encoder")), "contracts: {:?}", common.contracts);
        // Shared toolkit path is in common files.
        assert!(common.files.iter().any(|f| f == "toolkit"), "common files: {:?}", common.files);
        // `common` owns NO dependencies — deps are per-platform only.
        assert!(
            common.dependencies.is_empty(),
            "common must not carry dependencies: {:?}",
            common.dependencies
        );

        let rpi5 = meta.domains.iter().find(|d| d.id == "rpi5").unwrap();
        assert_eq!(rpi5.kind, "platform");
        // Clean DIRECTORY-level slices: rpi5 app dir + rpi5 HAL impl dir.
        assert!(
            rpi5.files.iter().any(|f| f == "rpi5"),
            "rpi5 files must list the app dir: {:?}",
            rpi5.files
        );
        assert!(
            rpi5.files.iter().any(|f| f == "hal/implementations/rpi5"),
            "rpi5 files must list the HAL impl dir: {:?}",
            rpi5.files
        );
        // No shared-toolkit leakage into platform domains.
        assert!(
            !rpi5.files.iter().any(|f| f.contains("toolkit")),
            "rpi5 must not carry toolkit files: {:?}",
            rpi5.files
        );
        let rpi5_dep_names: Vec<&str> = rpi5.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert!(rpi5_dep_names.contains(&"libcamera"), "rpi5 deps: {rpi5_dep_names:?}");
        assert!(!rpi5_dep_names.contains(&"rknnrt"), "rpi5 must not carry rock3c deps: {rpi5_dep_names:?}");

        let rock3c = meta.domains.iter().find(|d| d.id == "rock3c").unwrap();
        assert!(
            rock3c.files.iter().any(|f| f == "rock3c"),
            "rock3c files must list the app dir: {:?}",
            rock3c.files
        );
        assert!(
            rock3c.files.iter().any(|f| f == "hal/implementations/rock3c"),
            "rock3c files must list the HAL impl dir: {:?}",
            rock3c.files
        );
        assert!(
            !rock3c.files.iter().any(|f| f.contains("toolkit")),
            "rock3c must not carry toolkit files: {:?}",
            rock3c.files
        );
        let rock3c_dep_names: Vec<&str> = rock3c.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert!(rock3c_dep_names.contains(&"rknnrt"), "rock3c deps: {rock3c_dep_names:?}");
        assert!(!rock3c_dep_names.contains(&"libcamera"), "rock3c must not carry rpi5 deps: {rock3c_dep_names:?}");
        // `common` is shared/editable (not read-only).
        assert_eq!(
            common.editability,
            spire_core::build_types::DomainEditability::Shared,
            "common must be shared/editable"
        );
    }

    /// Regression: a platform-local `app_sources = files('main.cpp', ...)`
    /// variable must NOT be re-classified as "shared" just because it's
    /// referenced with `+` in the executable block and assigned `files(...)`.
    /// Before the fix, `app_sources` leaked into `shared_files`, which
    /// emptied the target's App source unit — dropping the `<plat>/` directory
    /// from the platform domain's file list so the UI showed only the HAL
    /// implementations (never `main.cpp` / platform pipeline files).
    #[test]
    fn analyze_platform_app_sources_stay_app_not_shared() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::create_dir_all(root.join("toolkit")).unwrap();
        std::fs::create_dir_all(root.join("rpi5")).unwrap();
        std::fs::File::create(root.join("meson.build")).unwrap().write_all(
            b"project('ai-traps', 'cpp')\nsubdir('toolkit')\nsubdir('hal')\nsubdir('rpi5')\n",
        ).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            "class CameraHAL {\npublic:\n    virtual bool start() = 0;\n};\n",
        ).unwrap();
        std::fs::write(root.join("hal/implementations/rpi5/camera_hal_imx219.cpp"), "").unwrap();
        std::fs::File::create(root.join("toolkit/meson.build")).unwrap().write_all(
            b"toolkit_sources = files('src/pipeline/base.cpp')\n",
        ).unwrap();
        std::fs::File::create(root.join("hal/meson.build")).unwrap().write_all(
            b"hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n",
        ).unwrap();
        // Real ai-traps shape: `app_sources` is platform-local (assigned in
        // THIS section), `rpi5_hal_sources` aliases the hal/ container var,
        // and the executable() mixes all three with `+`.
        std::fs::File::create(root.join("rpi5/meson.build")).unwrap().write_all(
            b"app_sources = files('main.cpp', 'rpi5_detection_pipeline.cpp')\n\
              rpi5_hal_sources = hal_impl_rpi5_sources\n\
              executable('ai-trap-rpi5', app_sources + rpi5_hal_sources + toolkit_sources)\n",
        ).unwrap();
        std::fs::write(root.join("rpi5/main.cpp"), "int main(){}\n").unwrap();
        std::fs::write(root.join("rpi5/rpi5_detection_pipeline.cpp"), "// pipeline\n").unwrap();

        let meta = MesonBuildModule::new().analyze(root).unwrap();

        // The ai-trap-rpi5 target carries its App sources…
        let rpi5 = meta.targets.iter().find(|t| t.name == "ai-trap-rpi5").unwrap();
        assert!(
            rpi5.source_files.contains(&"main.cpp".to_string()),
            "target source_files missing main.cpp: {:?}",
            rpi5.source_files
        );
        assert!(
            rpi5.source_files.contains(&"rpi5_detection_pipeline.cpp".to_string()),
            "target source_files missing rpi5_detection_pipeline.cpp: {:?}",
            rpi5.source_files
        );
        // …and gets an App source unit (not Shared).
        let app_unit = rpi5.source_units.iter().any(|u| {
            u.role == spire_core::build_types::SourceRole::App
        });
        assert!(
            app_unit,
            "platform target must have an App source unit: {:?}",
            rpi5.source_units
        );
        let shared_unit = rpi5.source_units.iter().any(|u| {
            u.role == spire_core::build_types::SourceRole::Shared
        });
        assert!(
            shared_unit,
            "platform target must still have a Shared toolkit unit: {:?}",
            rpi5.source_units
        );

        // The platform domain lists the app dir + the HAL impl dir — the UI
        // then shows main.cpp / rpi5_detection_pipeline.cpp in Sources.
        let domain = meta.domains.iter().find(|d| d.id == "rpi5").unwrap();
        assert!(
            domain.files.iter().any(|f| f == "rpi5"),
            "rpi5 domain must list the app dir: {:?}",
            domain.files
        );
        assert!(
            domain.files.iter().any(|f| f == "hal/implementations/rpi5"),
            "rpi5 domain must list the HAL impl dir: {:?}",
            domain.files
        );
        assert!(
            !domain.files.iter().any(|f| f.contains("toolkit")),
            "rpi5 domain must not carry toolkit files: {:?}",
            domain.files
        );
    }

    /// The analyzer classifies the project's structural shape so the UI can
    /// pick the right tree presentation:
    /// - plain single-host Meson project → Native
    /// - multi-platform Meson WITHOUT HAL contracts → SingleSource
    /// - ai-traps (multi-platform + hal/api contracts) → Hal
    #[test]
    fn analyze_classifies_structure_native_single_and_hal() {
        // ── Native: one host project, no platform option ────────────────
        let native = tempfile::tempdir().unwrap();
        std::fs::File::create(native.path().join("meson.build"))
            .unwrap()
            .write_all(b"project('plain', 'cpp')\nexecutable('plain', 'main.cpp')\n")
            .unwrap();
        let meta = MesonBuildModule::new().analyze(native.path()).unwrap();
        assert_eq!(
            meta.structure,
            spire_core::build_types::ProjectStructure::Native,
            "plain meson must be Native"
        );

        // ── SingleSource: multi-platform meson_options, no HAL contracts ──
        let single = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(single.path().join("rpi5")).unwrap();
        std::fs::create_dir_all(single.path().join("rock3c")).unwrap();
        std::fs::File::create(single.path().join("meson.build")).unwrap().write_all(
            b"project('single', 'cpp')\nsubdir('rpi5')\nsubdir('rock3c')\n",
        ).unwrap();
        std::fs::File::create(single.path().join("meson_options.txt")).unwrap().write_all(
            b"option('platform', type: 'string', value: 'host',\n       description: 'Valid values: host, rpi5, rock3c')\n",
        ).unwrap();
        std::fs::File::create(single.path().join("rpi5/meson.build")).unwrap().write_all(
            b"executable('single-rpi5', 'main.cpp')\n",
        ).unwrap();
        std::fs::File::create(single.path().join("rock3c/meson.build")).unwrap().write_all(
            b"executable('single-rock3c', 'main.cpp')\n",
        ).unwrap();
        let meta = MesonBuildModule::new().analyze(single.path()).unwrap();
        assert_eq!(
            meta.structure,
            spire_core::build_types::ProjectStructure::SingleSource,
            "multi-platform no-HAL meson must be SingleSource"
        );

        // ── Hal: ai-traps container layout (hal/api contracts + platform impls)
        let hal = tempfile::tempdir().unwrap();
        let root = hal.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::create_dir_all(root.join("rpi5")).unwrap();
        std::fs::File::create(root.join("meson.build")).unwrap().write_all(
            b"project('ai-traps', 'cpp')\nsubdir('hal')\nsubdir('rpi5')\n",
        ).unwrap();
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            "class CameraHAL {\npublic:\n    virtual bool start() = 0;\n};\n",
        )
        .unwrap();
        std::fs::write(root.join("hal/implementations/rpi5/camera_hal_imx219.cpp"), "").unwrap();
        std::fs::File::create(root.join("hal/meson.build")).unwrap().write_all(
            b"hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n",
        ).unwrap();
        std::fs::File::create(root.join("rpi5/meson.build")).unwrap().write_all(
            b"executable('ai-trap-rpi5', 'main.cpp' + hal_impl_rpi5_sources)\n",
        ).unwrap();
        let meta = MesonBuildModule::new().analyze(root).unwrap();
        assert_eq!(
            meta.structure,
            spire_core::build_types::ProjectStructure::Hal,
            "hal-layout meson must be Hal"
        );
    }

    /// Non-contract headers (structs, free functions) in `hal/api` must NOT be
    /// reported as HAL interfaces, so `hal_add_target` never generates bogus
    /// placeholder implementations for them.
    #[test]
    fn analyze_excludes_non_contract_headers_from_hal_interfaces() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("hal/api")).unwrap();
        std::fs::create_dir_all(root.join("hal/implementations/rpi5")).unwrap();
        std::fs::create_dir_all(root.join("rpi5")).unwrap();
        std::fs::File::create(root.join("meson.build"))
            .unwrap()
            .write_all(b"project('ai-traps', 'cpp')\nsubdir('hal')\nsubdir('rpi5')\n")
            .unwrap();
        // A real abstract-class contract.
        std::fs::write(
            root.join("hal/api/camera_hal.hpp"),
            "class CameraHAL {\npublic:\n    virtual bool start() = 0;\n};\n",
        )
        .unwrap();
        // Non-contract headers that previously polluted the interface set.
        std::fs::write(
            root.join("hal/api/frame_buffer.hpp"),
            "struct FrameBuffer { int w = 0; };\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/api/config_loader.hpp"),
            "// free functions, no pure-virtual\n",
        )
        .unwrap();
        std::fs::write(
            root.join("hal/implementations/rpi5/camera_hal_imx219.cpp"),
            "// impl\n",
        )
        .unwrap();
        std::fs::File::create(root.join("hal/meson.build"))
            .unwrap()
            .write_all(
                b"hal_impl_rpi5_sources = files('implementations/rpi5/camera_hal_imx219.cpp')\n",
            )
            .unwrap();
        std::fs::write(root.join("rpi5/main.cpp"), "int main(){}\n").unwrap();
        std::fs::File::create(root.join("rpi5/meson.build"))
            .unwrap()
            .write_all(
                b"executable('ai-trap-rpi5', 'main.cpp' + hal_impl_rpi5_sources)\n",
            )
            .unwrap();

        let meta = MesonBuildModule::new().analyze(root).unwrap();
        let names: Vec<&str> = meta.hal_interfaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["camera_hal"], "interfaces: {names:?}");
        assert!(!names.contains(&"frame_buffer"), "frame_buffer is not a contract: {names:?}");
        assert!(!names.contains(&"config_loader"), "config_loader is not a contract: {names:?}");
    }

    #[test]
    fn scaffold_layout_host_returns_legacy_single() {
        let out = MesonBuildModule::new()
            .scaffold_layout(
                "demo",
                "goal",
                &["host".to_string()],
                spire_core::build_types::ProjectStructure::Native,
            )
            .unwrap();
        assert!(out.files.is_empty());
        assert_eq!(out.build_file, "meson.build");
        assert!(out.build_content.contains("project('demo', 'c')"));
        assert!(out.source_file.ends_with("main.c"));
        assert_eq!(out.platform_targets, vec!["host".to_string()]);
    }

    #[test]
    fn scaffold_layout_multi_emits_platform_files() {
        let out = MesonBuildModule::new()
            .scaffold_layout(
                "demo",
                "goal",
                &["rpi5".to_string(), "rock3c".to_string()],
                spire_core::build_types::ProjectStructure::SingleSource,
            )
            .unwrap();
        // Root + meson_options.txt + per-platform meson.build + main.cpp.
        assert!(out.files.iter().any(|f| f.path == "meson.build"));
        assert!(out.files.iter().any(|f| f.path == "meson_options.txt"));
        assert!(out.files.iter().any(|f| f.path == "rpi5/meson.build"));
        assert!(out.files.iter().any(|f| f.path == "rock3c/meson.build"));
        assert!(out.files.iter().any(|f| f.path == "rpi5/main.cpp"));
        assert!(out.files.iter().any(|f| f.path == "rock3c/main.cpp"));
        assert_eq!(out.platform_targets, vec!["rpi5".to_string(), "rock3c".to_string()]);
        // meson_options declares both platforms.
        let opts = out.files.iter().find(|f| f.path == "meson_options.txt").unwrap();
        assert!(opts.content.contains("rpi5"));
        assert!(opts.content.contains("rock3c"));
        // Root declares subdir('rpi5') + subdir('rock3c').
        let root = out.files.iter().find(|f| f.path == "meson.build").unwrap();
        assert!(root.content.contains("subdir('rpi5')"));
        assert!(root.content.contains("subdir('rock3c')"));
    }

    #[test]
    fn scaffolding_multi_platform_roundtrips_through_analyze() {
        // Write the scaffolded files to a temp dir, then verify the existing
        // analyzer sees both per-platform targets (the scaffold round-trips
        // through the build system's own parser/builder).
        let tmp = tempfile::tempdir().unwrap();
        let out = MesonBuildModule::new()
            .scaffold_layout(
                "demo",
                "goal",
                &["rpi5".to_string(), "rock3c".to_string()],
                spire_core::build_types::ProjectStructure::SingleSource,
            )
            .unwrap();
        for f in out.files {
            let p = tmp.path().join(&f.path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, &f.content).unwrap();
        }
        let meta = MesonBuildModule::new().analyze(tmp.path()).unwrap();
        let names: Vec<String> = meta.targets.iter().map(|t| t.name.clone()).collect();
        assert!(names.contains(&"demo-rpi5".to_string()), "targets: {names:?}");
        assert!(names.contains(&"demo-rock3c".to_string()), "targets: {names:?}");
        assert_eq!(meta.platform_targets, vec!["rpi5".to_string(), "rock3c".to_string()]);
    }

    #[test]
    fn analyze_does_not_treat_find_library_as_target() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("rpi5")).unwrap();
        std::fs::File::create(tmp.path().join("meson.build"))
            .unwrap()
            .write_all(b"project('ai-traps', 'cpp')\nsubdir('rpi5')\n")
            .unwrap();
        // Exactly the ai-traps shape: executable + find_library dependency calls.
        std::fs::File::create(tmp.path().join("rpi5/meson.build"))
            .unwrap()
            .write_all(
                b"cpp = meson.get_compiler('cpp')\n\
                  executable('ai-trap-rpi5', 'main.cpp')\n\
                  dependency('yaml-cpp', required: false)\n\
                  cpp.find_library('rknnrt', dirs: ['/opt/sysroot/lib'], required: true)\n\
                  cpp.find_library('rockchip_mpp', dirs: ['/opt/sysroot/lib'], required: true)\n",
            )
            .unwrap();

        let meta = MesonBuildModule::new().analyze(tmp.path()).unwrap();

        let target_names: Vec<&str> = meta.targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            target_names,
            vec!["ai-trap-rpi5"],
            "find_library deps must not appear as build targets, got: {target_names:?}"
        );

        // …but the deps ARE still extracted as dependencies.
        let dep_names: Vec<&str> = meta.dependencies.iter().map(|d| d.name.as_str()).collect();
        assert!(dep_names.contains(&"rknnrt"), "deps: {dep_names:?}");
        assert!(dep_names.contains(&"rockchip_mpp"), "deps: {dep_names:?}");
        assert!(dep_names.contains(&"yaml-cpp"), "deps: {dep_names:?}");
    }
}

#[async_trait]
impl Actor for MesonBuildModule {
    type Message = BuildModuleMessage;

    async fn handle(&mut self, msg: Self::Message) {
        match msg {
            BuildModuleMessage::DescribeCapabilities { reply_to } => {
                let _ = reply_to.send(ModuleCapability {
                    name: "meson".to_string(),
                    config_files: vec!["meson.build".to_string()],
                    build_system: "Meson".to_string(),
                    language: "C/C++".to_string(),
                    source_extensions: vec![
                        "c".to_string(),
                        "cpp".to_string(),
                        "cc".to_string(),
                        "cxx".to_string(),
                        "h".to_string(),
                        "hpp".to_string(),
                    ],
                    mcp_servers: vec![],
                });
            }
            BuildModuleMessage::Analyze { path, reply_to } => {
                let _ = reply_to.send(self.analyze(&path));
            }
            BuildModuleMessage::Build {
                path,
                opts,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.build(&path, &opts).await);
            }

            BuildModuleMessage::BuildStreaming {
                path,
                metadata: _metadata,
                opts,
                build_spec: _,
                event_tx,
                reply_to,
            } => {
                // Not yet streaming for this module — fall back to a batch build
                // and emit a single synthetic "finished" event.
                let result = self.build(&path, &opts).await;
                let _ = event_tx.send(super::BuildEvent {
                    line: format!("Finished {} in {:?}s", path.display(), result.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
                let _ = reply_to.send(result);
            }
            BuildModuleMessage::Test {
                path,
                opts,
                reply_to,
                ..
            } => {
                let _ = reply_to.send(self.test(&path, &opts).await);
            }

            BuildModuleMessage::Clean { path, reply_to, .. } => {
                let _ = reply_to.send(self.clean(&path).await);
            }

            BuildModuleMessage::Lint { path, platform, reply_to, .. } => {
                let _ = reply_to.send(self.lint(&path, platform.as_deref()).await);
            }

            BuildModuleMessage::Format { path, reply_to, .. } => {
                let _ = reply_to.send(self.format(&path).await);
            }
            BuildModuleMessage::Fix { path, reply_to, .. } => {
                let _ = reply_to.send(self.fix(&path).await);
            }
            BuildModuleMessage::LintStreaming {
                path,
                platform,
                event_tx,
                reply_to,
                ..
            } => {
                // Stream per-file analyzer results as they complete so the UI
                // shows incremental progress (not a single late batch).
                // Platform-aware compile DB: when a platform is selected (e.g.
                // "rpi5"), prefer build-rpi5/compile_commands.json so lint uses
                // the cross-compiled file set + flags.
                let db_dir = if let Some(plat) = platform {
                    self.find_named_build_dir(&path, &format!("build-{plat}"))
                } else {
                    self.find_compile_db_dir(&path)
                };
                let db = self.load_compile_commands_from(db_dir.clone());
                let files = self.source_files(&path);
                let mut success = true;
                let mut output_lines: Vec<String> = Vec::new();
                for file in &files {
                    let (program, args) = self.analyzer_for_file(file, &db, &db_dir);
                    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    match run_cmd(&path, &program, &arg_refs).await {
                        Ok(o) => {
                            let t = strip_ansi(&o.output).trim().to_string();
                            if !t.is_empty() {
                                output_lines.push(t.clone());
                            }
                            if !o.success {
                                success = false;
                            }
                            let line = if t.is_empty() {
                                format!("\u{5b}Lint] {file}: no issues")
                            } else {
                                format!("\u{5b}Lint] {file}:\n{t}")
                            };
                            let _ = event_tx.send(super::BuildEvent {
                                line,
                                level: if success { "info" } else { "error" }.to_string(),
                                target: Some(file.clone()),
                                file: Some(file.clone()),
                                line_number: None,
                                message: None,
                                detail: None,
                            });
                        }
                        Err(e) => {
                            output_lines.push(e.clone());
                            success = false;
                            let _ = event_tx.send(super::BuildEvent {
                                line: format!("\u{5b}Lint] {file}: {e}"),
                                level: "error".to_string(),
                                target: Some(file.clone()),
                                file: Some(file.clone()),
                                line_number: None,
                                message: None,
                                detail: None,
                            });
                        }
                    }
                }
                let _ = event_tx.send(super::BuildEvent {
                    line: format!("Finished lint {}", path.display()),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
                let _ = reply_to.send(Ok(BuildOutput {
                    success,
                    command: "clang --analyze (static analyzer)".to_string(),
                    duration_secs: 0.0,
                    output: if output_lines.is_empty() {
                        format!(
                            "clang static analyzer: analyzed {} files; no issues reported",
                            files.len()
                        )
                    } else {
                        format!(
                            "clang static analyzer: analyzed {} files\n{}",
                            files.len(),
                            output_lines.join("\n---\n")
                        )
                    },
                    exit_code: Some(if success { 0 } else { 1 }),
                }));
            }

            BuildModuleMessage::FixStreaming {
                path,
                event_tx,
                reply_to,
                ..
            } => {
                let result = self.fix(&path).await;
                let _ = event_tx.send(super::BuildEvent {
                    line: format!("Finished fix {} in {:?}s", path.display(), result.as_ref().map(|o| o.duration_secs).unwrap_or(0.0)),
                    level: "finished".to_string(),
                    target: None,
                    file: None,
                    line_number: None,
                    message: None,
                    detail: None,
                });
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::ParseSourceFile {
                file_path,
                reply_to,
            } => {
                // C/C++ headers (HAL contracts, .hpp/.h) get the method-level
                // extractor so class-method nodes + child edges + pure-virtual
                // markers land in the AST graph (the HAL contract tooling
                // derives its method set from there).
                let path = std::path::PathBuf::from(&file_path);
                let is_header = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e == "hpp" || e == "h")
                    .unwrap_or(false);
                let result = if is_header {
                    parse_cpp_source_file_std(&path)
                } else {
                    parse_source_file_std(&path, "C/C++")
                };
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::ScaffoldBuildConfig {
                project_name,
                goal,
                platforms,
                structure,
                embedded: _,
                reply_to,
            } => {
                let result =
                    self.scaffold_layout(&project_name, &goal, &platforms, structure);
                let _ = reply_to.send(result);
            }

            BuildModuleMessage::CallTool { reply_to, .. } => {
                let _ = reply_to
                    .send(serde_json::json!({ "error": "meson module CallTool not yet wired" }));
            }
        }
    }
}
