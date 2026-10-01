// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NatureSense

//! End-to-end test for the **resolved profile** (`capabilities::resolved_profile`): seed a small
//! registry through the real writer (`platform_seed_payload` → `BootstrapPlatforms`), then read the
//! profile back and check that the values the seeder stored on the edges come out intact — the
//! board's own (`interface`), the chip's (its `provides` edge), and a companion's (`role`).
//!
//! The graph is the same actor the app runs, so this is the reader half of the seeder's contract:
//! the writer is proven by `platform_seed_tests` in spire-core, and this proves the values it stored
//! are legible to the query a codegen step will call.

use spire_code::actors::platform_codec::platform_seed_payload;
use spire_code::capabilities::resolved_profile;
use spire_code::platform::Platform;
use spire_core::actors::{Actor, MemoryGraphActor, MemoryGraphMessage};
use tokio::sync::{mpsc, oneshot};

/// A `MemoryGraphActor` over a fresh in-memory graph — the same shape spire-core's own seeder test
/// uses. The store is removed first: `InitializeInMemory` recovers an existing snapshot, so a
/// leftover directory would make a re-run read the previous run's graph.
async fn spawn_graph() -> mpsc::Sender<MemoryGraphMessage> {
    let dir = std::env::temp_dir().join(format!("spire-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let (tx, rx) = mpsc::channel(64);
    let _join = MemoryGraphActor::new().spawn(rx);
    let (t, r) = oneshot::channel();
    tx.send(MemoryGraphMessage::InitializeInMemory {
        data_dir: dir,
        reply_to: t,
    })
    .await
    .expect("send init");
    r.await.expect("init reply").expect("init ok");
    tx
}

#[tokio::test]
async fn the_resolved_profile_reads_the_seeded_capability_graph() {
    let tmp = tempfile::tempdir().unwrap();
    let (boards, chips, platforms) = (
        tmp.path().join("boards"),
        tmp.path().join("chips"),
        tmp.path().join("platforms"),
    );
    for dir in [&boards, &chips, &platforms] {
        std::fs::create_dir_all(dir).unwrap();
    }

    // A chip that declares what its silicon does — the `provides` side.
    std::fs::write(
        chips.join("esp32p4.yaml"),
        "id: esp32p4\nname: ESP32-P4\nos: esp-idf\ncapabilities:\n  media:\n    display: \
         { interface: mipi-dsi }\n",
    )
    .unwrap();
    // A companion chip the board carries.
    std::fs::write(
        chips.join("esp32c5.yaml"),
        "id: esp32c5\nname: ESP32-C5\nos: esp-hal\ncapabilities:\n  radio:\n    wifi: \
         { standard: \"802.11ax\" }\n",
    )
    .unwrap();
    // The board: a display realized through its host chip (`via`), a battery it realizes itself,
    // its wiring, and the companion silicon it carries.
    std::fs::write(
        boards.join("m5.yaml"),
        "id: m5\nname: M5\nos: esp-idf\nchip: esp32p4\nrealized:\n  media:\n    display: \
         { interface: mipi-dsi, via: esp32p4 }\n  power:\n    battery: { charger: true }\n\
         pins:\n  led: { pin: GPIO8, addressable: true }\n  grove:\n    a: { i2c: [GPIO2, GPIO1] }\n\
         companions:\n  - chip: esp32c5\n    role: radio\n",
    )
    .unwrap();

    let previous = std::env::var("SPIRE_PLATFORM_DIR").ok();
    std::env::set_var("SPIRE_PLATFORM_DIR", &platforms);

    // Seed through the real writer: the same payload the app and the CLI send.
    let graph = spawn_graph().await;
    let payload: Vec<serde_json::Value> = Platform::load_registry()
        .unwrap()
        .iter()
        .map(platform_seed_payload)
        .collect();
    let (t, r) = oneshot::channel();
    graph
        .send(MemoryGraphMessage::BootstrapPlatforms {
            platforms: payload,
            reply_to: t,
        })
        .await
        .expect("send bootstrap");
    r.await.expect("bootstrap reply").expect("bootstrap ok");

    // ── The read ──
    let profile = resolved_profile(&graph, "m5").await.expect("profile");
    assert_eq!(profile["board"], "m5");

    let realizes = profile["realizes"].as_array().expect("realizes");
    let display = realizes
        .iter()
        .find(|x| x["capability"] == "media.display")
        .expect("the board realizes its display");
    assert_eq!(display["properties"]["interface"], "mipi-dsi");
    assert_eq!(display["via"], "esp32p4");
    assert_eq!(
        display["provides"]["interface"], "mipi-dsi",
        "the chip's own value, off its `provides` edge: {display}"
    );

    // A realization the board does itself has no `via` and so no chip values.
    let battery = realizes
        .iter()
        .find(|x| x["capability"] == "power.battery")
        .expect("the board realizes its battery");
    assert_eq!(battery["properties"]["charger"], true);
    assert!(battery["via"].is_null());
    assert!(battery["provides"].is_null());

    // The companion silicon, with everything written beside it.
    let carries = profile["carries"].as_array().expect("carries");
    assert!(
        carries
            .iter()
            .any(|c| c["chip"] == "esp32c5" && c["properties"]["role"] == "radio"),
        "the board carries its radio companion: {carries:?}"
    );

    // Wiring, read back from its own `Pin` nodes: a leaf function keeps its assignment, and a
    // grouping flattens to dotted paths (`grove.a`), a list value included.
    let pins = profile["pins"].as_array().expect("pins");
    let led = pins
        .iter()
        .find(|p| p["function"] == "led")
        .expect("the led function");
    assert_eq!(led["properties"]["pin"], "GPIO8");
    assert_eq!(led["properties"]["addressable"], true);
    let grove_a = pins
        .iter()
        .find(|p| p["function"] == "grove.a")
        .expect("grove.a");
    assert_eq!(
        grove_a["properties"]["i2c"],
        serde_json::json!(["GPIO2", "GPIO1"])
    );

    match previous {
        Some(p) => std::env::set_var("SPIRE_PLATFORM_DIR", p),
        None => std::env::remove_var("SPIRE_PLATFORM_DIR"),
    }
}
