// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! Platform codec — the typed `crate::platform::Platform` ↔ generic registry JSON
//! (`{ "id", "name", "properties": {flat map} }`) conversions that cross the
//! `spire-knowledge` crate boundary. The knowledge store only deals with the
//! generic JSON; the platform YAML schema + typed view live in `crate::platform`.

use crate::platform::{
    Platform, PlatformArchitecture, PlatformDeploy, PlatformDevice, PlatformDeviceMcp,
    PlatformRust, PlatformSysroot, PlatformToolchain,
};

/// Serialize a platform definition into the registry JSON shape the knowledge
/// crate stores as a `Platform` node.
pub fn platform_to_registry_json(p: &Platform) -> serde_json::Value {
    let mut props = serde_json::Map::new();
    let str_list = |v: &[String]| v.iter().map(|s| serde_json::json!(s)).collect::<Vec<_>>();

    props.insert("os".into(), serde_json::json!(p.os));
    props.insert(
        "cpu_family".into(),
        serde_json::json!(p.architecture.cpu_family),
    );
    props.insert("cpu".into(), serde_json::json!(p.architecture.cpu));
    props.insert("endian".into(), serde_json::json!(p.architecture.endian));
    props.insert(
        "target_triple".into(),
        serde_json::json!(p.architecture.target_triple),
    );
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
    props.insert(
        "c_args_extra".into(),
        serde_json::json!(str_list(&p.toolchain.c_args_extra)),
    );
    props.insert(
        "cpp_args_extra".into(),
        serde_json::json!(str_list(&p.toolchain.cpp_args_extra)),
    );
    props.insert(
        "linker_args_extra".into(),
        serde_json::json!(str_list(&p.toolchain.linker_args_extra)),
    );
    props.insert(
        "needs_exe_wrapper".into(),
        serde_json::json!(p.toolchain.needs_exe_wrapper),
    );
    props.insert("sysroot_root".into(), serde_json::json!(p.sysroot.root));
    props.insert(
        "sysroot_lib_dirs".into(),
        serde_json::json!(str_list(&p.sysroot.lib_dirs)),
    );
    props.insert(
        "sysroot_include_dirs".into(),
        serde_json::json!(str_list(&p.sysroot.include_dirs)),
    );
    props.insert(
        "sysroot_pkg_config_libdir".into(),
        serde_json::json!(str_list(&p.sysroot.pkg_config_libdir)),
    );

    // Device access (`device:` in the YAML) — optional on-hardware endpoint.
    if let Some(device) = &p.device {
        if let Some(mcp) = &device.mcp {
            props.insert("device_mcp_url".into(), serde_json::json!(mcp.url));
            if let Some(token) = &mcp.token {
                props.insert("device_mcp_token".into(), serde_json::json!(token));
            }
        }
        if let Some(deploy) = &device.deploy {
            props.insert("device_deploy_dest".into(), serde_json::json!(deploy.dest));
        }
    }

    // Board family + the Rust toolchain (`os: "esp-idf"`). Both optional, so a C platform
    // carries neither and its stored shape is unchanged.
    if let Some(family) = &p.family {
        props.insert("family".into(), serde_json::json!(family));
    }
    if let Some(rust) = &p.rust {
        props.insert("rust_target".into(), serde_json::json!(rust.target));
        if let Some(idf) = &rust.idf_target {
            props.insert("rust_idf_target".into(), serde_json::json!(idf));
        }
        if let Some(flash) = &rust.flash {
            props.insert("rust_flash".into(), serde_json::json!(flash));
        }
    }

    // The wizard and the Rust HAL fill prompt both read this from the platform, so it is
    // persisted like any other field rather than left to a second parse of the YAML.
    if let Some(hints) = &p.library_hints {
        props.insert("library_hints".into(), serde_json::json!(hints));
    }
    // The board's chip travels with it, so a graph round-trip cannot lose the
    // declaration and silently fall back to deriving one.
    if let Some(chip) = &p.chip {
        props.insert("chip".into(), serde_json::json!(chip));
    }

    serde_json::json!({
        "id": p.id,
        "name": p.name,
        "properties": props,
    })
}

/// Rebuild a `crate::platform::Platform` from the generic registry JSON node the
/// knowledge crate returns for `Platform` nodes.
pub fn platform_json_to_spire(node: &serde_json::Value) -> Option<Platform> {
    let id = node.get("id")?.as_str()?.to_string();
    let name = node
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let props = node.get("properties").and_then(|v| v.as_object())?;
    let get_str = |k: &str| {
        props
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
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
        device: {
            let mcp_url = get_str("device_mcp_url");
            let deploy_dest = get_str("device_deploy_dest");
            let mcp = (!mcp_url.trim().is_empty()).then(|| PlatformDeviceMcp {
                url: mcp_url,
                token: get_opt("device_mcp_token"),
            });
            let deploy =
                (!deploy_dest.trim().is_empty()).then_some(PlatformDeploy { dest: deploy_dest });
            if mcp.is_none() && deploy.is_none() {
                None
            } else {
                Some(PlatformDevice { mcp, deploy })
            }
        },
        family: get_opt("family"),
        chip: get_opt("chip"),
        // An empty `rust_target` means "no Rust toolchain", not a platform with a blank
        // one — the `device` block above follows the same rule.
        rust: {
            let target = get_str("rust_target");
            (!target.trim().is_empty()).then(|| PlatformRust {
                target,
                idf_target: get_opt("rust_idf_target"),
                flash: get_opt("rust_flash"),
            })
        },
        library_hints: get_opt("library_hints"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{
        Platform, PlatformArchitecture, PlatformRust, PlatformSysroot, PlatformToolchain,
    };

    /// An embedded variant, as the struct the CLI and the seed loader produce.
    fn esp32c6() -> Platform {
        Platform {
            id: "esp32c6".into(),
            name: "ESP32-C6".into(),
            os: "esp-idf".into(),
            architecture: PlatformArchitecture {
                cpu_family: "riscv".into(),
                cpu: "esp32c6".into(),
                endian: "little".into(),
                target_triple: "riscv32imac-esp-espidf".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            device: None,
            family: Some("esp32".into()),
            chip: None,
            rust: Some(PlatformRust {
                target: "riscv32imac-esp-espidf".into(),
                idf_target: Some("esp32c6".into()),
                flash: Some("espflash".into()),
            }),
            library_hints: Some(
                "RISC-V RV32IMAC via esp-idf-hal; no std-vs-no_std choice to make.".into(),
            ),
        }
    }

    fn rpi5() -> Platform {
        Platform {
            id: "rpi5".into(),
            name: "Raspberry Pi 5".into(),
            os: "linux".into(),
            architecture: PlatformArchitecture {
                cpu_family: "aarch64".into(),
                cpu: "armv8-a".into(),
                endian: "little".into(),
                target_triple: "aarch64-linux-gnu".into(),
                march: None,
            },
            toolchain: PlatformToolchain::default(),
            sysroot: PlatformSysroot::default(),
            device: None,
            family: None,
            chip: None,
            rust: None,
            library_hints: None,
        }
    }

    /// The codec is the graph boundary: anything it drops, the rest of the app cannot see.
    /// A variant's family, Rust toolchain and library hints must survive the trip BOTH ways,
    /// stored as individual typed properties — never a nested JSON fragment, per this module's
    /// rule.
    #[test]
    fn an_esp32_variant_round_trips_through_the_registry_shape() {
        let json = platform_to_registry_json(&esp32c6());
        let props = json.get("properties").expect("properties");

        assert_eq!(props.get("family").expect("family"), "esp32");
        assert_eq!(
            props.get("rust_target").expect("rust_target"),
            "riscv32imac-esp-espidf"
        );
        assert_eq!(props.get("rust_idf_target").expect("idf target"), "esp32c6");
        assert_eq!(props.get("rust_flash").expect("flash"), "espflash");
        assert!(
            props.get("rust").is_none(),
            "individual typed properties, not a blob: {props}"
        );
        assert_eq!(
            props.get("library_hints").expect("library hints"),
            "RISC-V RV32IMAC via esp-idf-hal; no std-vs-no_std choice to make."
        );

        let back = platform_json_to_spire(&json).expect("round trip");
        assert_eq!(back.family.as_deref(), Some("esp32"));
        let rust = back.rust.expect("the rust toolchain must survive");
        assert_eq!(rust.target, "riscv32imac-esp-espidf");
        assert_eq!(rust.idf_target.as_deref(), Some("esp32c6"));
        assert_eq!(rust.flash.as_deref(), Some("espflash"));
        assert_eq!(
            back.library_hints.as_deref(),
            Some("RISC-V RV32IMAC via esp-idf-hal; no std-vs-no_std choice to make."),
            "the wizard reads the hint off the platform, so it must survive the graph"
        );
    }

    /// A C platform carries neither field, so the shape the existing registry already holds
    /// is unchanged by this work — which is what makes it safe to land.
    #[test]
    fn a_c_platform_stores_no_embeddable_properties() {
        let json = platform_to_registry_json(&rpi5());
        let props = json.get("properties").expect("properties");
        assert!(props.get("family").is_none(), "{props}");
        assert!(props.get("rust_target").is_none(), "{props}");
        assert!(props.get("library_hints").is_none(), "{props}");

        let back = platform_json_to_spire(&json).expect("round trip");
        assert!(back.family.is_none(), "no family is not a blank family");
        assert!(
            back.library_hints.is_none(),
            "no hints is not an empty hint"
        );
        assert!(
            back.rust.is_none(),
            "an empty target is no toolchain at all"
        );
    }
}
