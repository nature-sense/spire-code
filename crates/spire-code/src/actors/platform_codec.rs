// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Platform codec — the typed `spire_core::build_types::Platform` ↔ generic registry JSON
//! (`{ "id", "name", "properties": {flat map} }`) conversions that cross the
//! `spire-knowledge` crate boundary. The knowledge store only deals with the
//! generic JSON; the platform YAML schema + typed view live here in spire-core.

use spire_core::build_types::{Platform, PlatformArchitecture, PlatformSysroot, PlatformToolchain};

/// Serialize a platform definition into the registry JSON shape the knowledge
/// crate stores as a `Platform` node.
pub fn platform_to_registry_json(p: &Platform) -> serde_json::Value {
    let mut props = serde_json::Map::new();
    let str_list = |v: &[String]| v.iter().map(|s| serde_json::json!(s)).collect::<Vec<_>>();

    props.insert("os".into(), serde_json::json!(p.os));
    props.insert("cpu_family".into(), serde_json::json!(p.architecture.cpu_family));
    props.insert("cpu".into(), serde_json::json!(p.architecture.cpu));
    props.insert("endian".into(), serde_json::json!(p.architecture.endian));
    props.insert("target_triple".into(), serde_json::json!(p.architecture.target_triple));
    if let Some(m) = &p.architecture.march {
        props.insert("march".into(), serde_json::json!(m));
    }
    props.insert("c_compiler".into(), serde_json::json!(p.toolchain.c));
    props.insert("cpp_compiler".into(), serde_json::json!(p.toolchain.cpp));
    props.insert("ar".into(), serde_json::json!(p.toolchain.ar));
    props.insert("strip".into(), serde_json::json!(p.toolchain.strip));
    if let Some(ld) = &p.toolchain.ld {
        props.insert("ld".into(), serde_json::json!(ld));
    }
    if let Some(pkg) = &p.toolchain.pkgconfig {
        props.insert("pkgconfig".into(), serde_json::json!(pkg));
    }
    props.insert("c_args_extra".into(), serde_json::json!(str_list(&p.toolchain.c_args_extra)));
    props.insert("cpp_args_extra".into(), serde_json::json!(str_list(&p.toolchain.cpp_args_extra)));
    props.insert("linker_args_extra".into(), serde_json::json!(str_list(&p.toolchain.linker_args_extra)));
    props.insert("needs_exe_wrapper".into(), serde_json::json!(p.toolchain.needs_exe_wrapper));
    props.insert("sysroot_root".into(), serde_json::json!(p.sysroot.root));
    props.insert("sysroot_lib_dirs".into(), serde_json::json!(str_list(&p.sysroot.lib_dirs)));
    props.insert("sysroot_include_dirs".into(), serde_json::json!(str_list(&p.sysroot.include_dirs)));
    props.insert("sysroot_pkg_config_libdir".into(), serde_json::json!(str_list(&p.sysroot.pkg_config_libdir)));

    serde_json::json!({
        "id": p.id,
        "name": p.name,
        "properties": props,
    })
}

/// Rebuild a `spire_core::build_types::Platform` from the generic registry JSON node the
/// knowledge crate returns for `Platform` nodes.
pub fn platform_json_to_spire(node: &serde_json::Value) -> Option<Platform> {
    let id = node.get("id")?.as_str()?.to_string();
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let props = node.get("properties").and_then(|v| v.as_object())?;
    let get_str = |k: &str| props.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let get_opt = |k: &str| props.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
    let get_list = |k: &str| {
        props
            .get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    Some(Platform {
        id,
        name,
        os: get_str("os"),
        architecture: PlatformArchitecture {
            cpu_family: get_str("cpu_family"),
            cpu: get_str("cpu"),
            endian: get_str("endian"),
            target_triple: get_str("target_triple"),
            march: get_opt("march"),
        },
        toolchain: PlatformToolchain {
            c: get_str("c_compiler"),
            cpp: get_str("cpp_compiler"),
            ar: get_str("ar"),
            strip: get_str("strip"),
            ld: get_opt("ld"),
            pkgconfig: get_opt("pkgconfig"),
            c_args_extra: get_list("c_args_extra"),
            cpp_args_extra: get_list("cpp_args_extra"),
            linker_args_extra: get_list("linker_args_extra"),
            needs_exe_wrapper: props
                .get("needs_exe_wrapper")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        },
        sysroot: PlatformSysroot {
            root: get_str("sysroot_root"),
            lib_dirs: get_list("sysroot_lib_dirs"),
            include_dirs: get_list("sysroot_include_dirs"),
            pkg_config_libdir: get_list("sysroot_pkg_config_libdir"),
        },
    })
}
