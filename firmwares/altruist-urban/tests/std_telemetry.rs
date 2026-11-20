///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////

//! The `telemetry` and `connectivity` services on the host: the two packet profile slots and the
//! two route slots are edited with ordinary Actions of a real simulator process through the real
//! `rohi` client, persist across a restart and are cleared by a factory reset.

mod common;

use common::*;
use rohi::model::{ActionAccess, FieldValue, Selection, Value};
use rohi::{Device, TcpLink};
use tempfile::TempDir;

fn arg(field_id: u32, value: Value) -> FieldValue {
    FieldValue { field_id, value }
}

fn mask(bits: &[usize]) -> Vec<u8> {
    let mut bytes = vec![0u8; 32];
    for bit in bits {
        bytes[bit / 8] |= 1 << (bit % 8);
    }
    bytes
}

const OWNER: [u8; 32] = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];

async fn set_public(device: &mut Device<TcpLink>, index: u64, bits: &[usize]) {
    device
        .invoke(
            "telemetry",
            "set_public",
            &[arg(1, Value::U64(index)), arg(2, Value::Bytes(mask(bits)))],
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn the_three_telemetry_actions_are_discoverable_with_their_access() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    let actions = device.actions().await.unwrap();
    let telemetry: Vec<_> = actions
        .iter()
        .filter(|action| action.service == "telemetry")
        .map(|action| (action.name.as_str(), action.access))
        .collect();
    assert_eq!(
        telemetry,
        [
            ("set_public", ActionAccess::Mutate),
            ("set_private", ActionAccess::Mutate),
            ("measurements", ActionAccess::Read),
        ]
    );
    // More than the 12 actions of a single page: nothing is cut off.
    assert!(actions.len() > 12, "{}", actions.len());

    // The selection fields name the catalog that explains their bits.
    let schema = device
        .action_schema("telemetry", "set_public")
        .await
        .unwrap();
    assert!(matches!(
        schema.args[1].selection,
        Some(Selection { size_bytes: 32, .. })
    ));
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn the_catalog_lists_what_the_robonomics_format_can_encode() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    let schema = device
        .action_schema("telemetry", "set_public")
        .await
        .unwrap();
    let selection = schema.args[1].selection.unwrap();
    let catalog = device.catalog(&selection).await.unwrap();
    let names: Vec<_> = catalog.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "location",
            "bmx280.temperature",
            "bmx280.humidity",
            "bmx280.pressure",
            "sds011.pm2_5",
            "sds011.pm10",
            "microphone.average",
            "microphone.maximum"
        ]
    );
    // The bits are the stable catalog bits, not positions in this list.
    assert_eq!(catalog[4].bit, 7);
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn profiles_are_edited_persist_and_a_factory_reset_clears_them() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;

    // Both slots exist from the start and disclose nothing.
    let status = fields(&mut device, "telemetry").await;
    for slot in 0..2 {
        assert_eq!(
            status.get(&format!("profile{slot}_public")),
            Some(&Value::Bytes(vec![0u8; 32]))
        );
        assert_eq!(
            status.get(&format!("profile{slot}_private")),
            Some(&Value::Bool(false))
        );
    }
    let created = status.get("profile0_revision").cloned();

    set_public(&mut device, 0, &[1, 7]).await;
    let status = fields(&mut device, "telemetry").await;
    assert_eq!(
        status.get("profile0_public"),
        Some(&Value::Bytes(mask(&[1, 7])))
    );
    assert_ne!(status.get("profile0_revision").cloned(), created);
    // Slot 1 is untouched.
    assert_eq!(
        status.get("profile1_public"),
        Some(&Value::Bytes(vec![0u8; 32]))
    );

    // An unsupported bit (the SCD4x CO2) and an index outside the two slots are refused and
    // change nothing.
    for (index, bits) in [(0u64, &[4usize][..]), (2, &[1][..])] {
        assert!(
            device
                .invoke(
                    "telemetry",
                    "set_public",
                    &[arg(1, Value::U64(index)), arg(2, Value::Bytes(mask(bits)))],
                )
                .await
                .is_err(),
            "index {index} bits {bits:?}"
        );
    }
    let after = fields(&mut device, "telemetry").await;
    assert_eq!(after.get("profile0_public"), status.get("profile0_public"));

    // The one private recipient of slot 1 is replaced, never added to.
    for recipient in [OWNER, [9u8; 32]] {
        let result = device
            .invoke(
                "telemetry",
                "set_private",
                &[
                    arg(1, Value::U64(1)),
                    arg(2, Value::Bytes(recipient.to_vec())),
                    arg(3, Value::Bytes(mask(&[0]))),
                ],
            )
            .await;
        // A recipient that is not a valid key is refused; the valid one is accepted.
        assert_eq!(result.is_ok(), recipient == OWNER);
    }
    let status = fields(&mut device, "telemetry").await;
    assert_eq!(status.get("profile1_private"), Some(&Value::Bool(true)));
    assert_eq!(
        status.get("profile1_priv_selection"),
        Some(&Value::Bytes(mask(&[0])))
    );
    drop(device);
    assert!(sim.stop().success());

    // The policy survives a restart.
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    let restored = fields(&mut device, "telemetry").await;
    assert_eq!(
        restored.get("profile0_public"),
        Some(&Value::Bytes(mask(&[1, 7])))
    );
    assert_eq!(restored.get("profile1_private"), Some(&Value::Bool(true)));

    // `none` removes the private section.
    device
        .invoke(
            "telemetry",
            "set_private",
            &[
                arg(1, Value::U64(1)),
                arg(2, Value::Bytes(OWNER.to_vec())),
                arg(3, Value::Bytes(vec![0u8; 32])),
            ],
        )
        .await
        .unwrap();
    let status = fields(&mut device, "telemetry").await;
    assert_eq!(status.get("profile1_private"), Some(&Value::Bool(false)));

    device.invoke("system", "factory_reset", &[]).await.unwrap();
    let status = fields(&mut device, "telemetry").await;
    assert_eq!(
        status.get("profile0_public"),
        Some(&Value::Bytes(vec![0u8; 32]))
    );
    drop(device);
    sim.stop();
}

fn route_args(index: u64, profile: u64, endpoint: &str) -> Vec<FieldValue> {
    vec![
        arg(1, Value::U64(index)),
        arg(2, Value::U64(profile)),
        arg(3, text(endpoint)),
    ]
}

#[tokio::test(flavor = "current_thread")]
async fn routes_are_edited_with_actions_persist_and_a_factory_reset_clears_them() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;

    let actions = device.actions().await.unwrap();
    let connectivity: Vec<_> = actions
        .iter()
        .filter(|action| action.service == "connectivity")
        .map(|action| (action.name.as_str(), action.access))
        .collect();
    assert_eq!(
        connectivity,
        [
            ("set_endpoint", ActionAccess::Mutate),
            ("set_interval", ActionAccess::Mutate),
            ("set_fallback", ActionAccess::Mutate),
            ("get_payload", ActionAccess::Read),
        ]
    );
    let status = fields(&mut device, "connectivity").await;
    assert_eq!(status.get("active_routes"), Some(&Value::U64(0)));
    for slot in 0..2 {
        assert_eq!(
            status.get(&format!("route{slot}_state")),
            Some(&Value::Enum("empty".into())),
            "no route by default"
        );
    }

    // The longest supported endpoint (64 bytes) is listed back in full.
    let long = format!("http://{}.example.org:8080", "h".repeat(40));
    assert_eq!(long.len(), 64);
    device
        .invoke("connectivity", "set_endpoint", &route_args(0, 1, &long))
        .await
        .unwrap();
    device
        .invoke(
            "connectivity",
            "set_interval",
            &[arg(1, Value::U64(0)), arg(2, Value::U64(120))],
        )
        .await
        .unwrap();
    device
        .invoke(
            "connectivity",
            "set_fallback",
            &[arg(1, Value::U64(0)), arg(2, text("http://backup.example"))],
        )
        .await
        .unwrap();
    let status = fields(&mut device, "connectivity").await;
    assert_eq!(status.get("route0_endpoint"), Some(&text(&long)));
    assert_eq!(status.get("route0_profile"), Some(&Value::U64(1)));
    assert_eq!(status.get("route0_interval_s"), Some(&Value::U64(120)));
    assert_eq!(
        status.get("route0_fallback"),
        Some(&text("http://backup.example"))
    );
    // Slot 1 did not move.
    assert_eq!(
        status.get("route1_state"),
        Some(&Value::Enum("empty".into()))
    );

    // Refused: an invalid interval, an index outside the two slots, a profile outside the
    // two slots, an interval of an empty slot.
    for (action, args) in [
        (
            "set_interval",
            vec![arg(1, Value::U64(0)), arg(2, Value::U64(5))],
        ),
        ("set_endpoint", route_args(2, 0, "http://a.example")),
        ("set_endpoint", route_args(1, 2, "http://a.example")),
        (
            "set_interval",
            vec![arg(1, Value::U64(1)), arg(2, Value::U64(60))],
        ),
    ] {
        assert!(
            device.invoke("connectivity", action, &args).await.is_err(),
            "{action}"
        );
    }
    assert_eq!(
        fields(&mut device, "connectivity")
            .await
            .get("route0_interval_s"),
        Some(&Value::U64(120))
    );
    drop(device);
    assert!(sim.stop().success());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    let status = fields(&mut device, "connectivity").await;
    assert_eq!(
        status.get("route0_endpoint"),
        Some(&text(&long)),
        "persisted"
    );

    // An absent endpoint (the empty string) deletes the route; the slot is reusable and a new
    // route inherits nothing.
    device
        .invoke("connectivity", "set_endpoint", &route_args(0, 0, ""))
        .await
        .unwrap();
    let status = fields(&mut device, "connectivity").await;
    assert_eq!(
        status.get("route0_state"),
        Some(&Value::Enum("empty".into()))
    );
    device
        .invoke(
            "connectivity",
            "set_endpoint",
            &route_args(0, 0, "http://again.example"),
        )
        .await
        .unwrap();
    let status = fields(&mut device, "connectivity").await;
    assert_eq!(status.get("route0_interval_s"), Some(&Value::U64(60)));
    assert!(!status.contains_key("route0_fallback"));

    device.invoke("system", "factory_reset", &[]).await.unwrap();
    let status = fields(&mut device, "connectivity").await;
    assert_eq!(status.get("active_routes"), Some(&Value::U64(0)));
    drop(device);
    sim.stop();
}
