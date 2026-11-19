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

//! The `telemetry` service on the host: the packet profiles are edited with
//! ordinary Actions of a real simulator process through the real `rohi`
//! client, persist across a restart and are removed by a factory reset.

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

async fn profiles(device: &mut Device<TcpLink>) -> Vec<(String, Vec<u8>, u64)> {
    let records = device
        .invoke("telemetry", "profile_list", &[arg(1, Value::U64(0))])
        .await
        .unwrap();
    records
        .iter()
        .map(|record| {
            let get = |id| record.iter().find(|f| f.field_id == id).unwrap();
            let Value::String(id) = &get(1).value else {
                panic!("profile id")
            };
            let Value::Bytes(public) = &get(3).value else {
                panic!("public selection")
            };
            let Value::U64(revision) = get(4).value else {
                panic!("revision")
            };
            (id.clone(), public.clone(), revision)
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn the_eight_actions_are_discoverable_with_their_access() {
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
            ("profile_add", ActionAccess::Mutate),
            ("profile_remove", ActionAccess::Mutate),
            ("set_public", ActionAccess::Mutate),
            ("add_private", ActionAccess::Mutate),
            ("remove_private", ActionAccess::Mutate),
            ("profile_list", ActionAccess::Read),
            ("profile_recipients", ActionAccess::Read),
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
async fn profiles_are_edited_persist_and_a_factory_reset_removes_them() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert!(profiles(&mut device).await.is_empty());

    device
        .invoke(
            "telemetry",
            "profile_add",
            &[
                arg(1, text("community")),
                arg(2, Value::Enum("robonomics".into())),
            ],
        )
        .await
        .unwrap();
    // Creating a profile discloses nothing.
    let created = profiles(&mut device).await;
    assert_eq!(created[0].1, vec![0u8; 32]);

    device
        .invoke(
            "telemetry",
            "set_public",
            &[
                arg(1, text("community")),
                arg(2, Value::Bytes(mask(&[1, 7]))),
            ],
        )
        .await
        .unwrap();
    let edited = profiles(&mut device).await;
    assert_eq!(edited[0].1, mask(&[1, 7]));
    assert!(edited[0].2 > created[0].2, "an edit has a new revision");

    // An unsupported bit (the SCD4x CO2) is refused and changes nothing.
    assert!(
        device
            .invoke(
                "telemetry",
                "set_public",
                &[arg(1, text("community")), arg(2, Value::Bytes(mask(&[4])))],
            )
            .await
            .is_err()
    );
    assert_eq!(profiles(&mut device).await, edited);

    // A private section for a valid key, listed back completely.
    let owner = vec![
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07,
        0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07,
        0x51, 0x1a,
    ];
    device
        .invoke(
            "telemetry",
            "add_private",
            &[
                arg(1, text("community")),
                arg(2, Value::Bytes(owner.clone())),
                arg(3, Value::Bytes(mask(&[0]))),
            ],
        )
        .await
        .unwrap();
    let recipients = device
        .invoke(
            "telemetry",
            "profile_recipients",
            &[arg(1, text("community")), arg(2, Value::U64(0))],
        )
        .await
        .unwrap();
    assert_eq!(recipients.len(), 1);
    assert_eq!(recipients[0][0].value, Value::Bytes(owner));
    assert_eq!(recipients[0][1].value, Value::Bytes(mask(&[0])));
    drop(device);
    assert!(sim.stop().success());

    // The policy survives a restart.
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    let restored = profiles(&mut device).await;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].1, mask(&[1, 7]));

    device.invoke("system", "factory_reset", &[]).await.unwrap();
    assert!(profiles(&mut device).await.is_empty());
    let status = fields(&mut device, "telemetry").await;
    assert_eq!(status.get("profiles"), Some(&Value::U64(0)));
    drop(device);
    sim.stop();
}

async fn routes(device: &mut Device<TcpLink>) -> Vec<(String, String, String, u64)> {
    let records = device
        .invoke("connectivity", "route_list", &[arg(1, Value::U64(0))])
        .await
        .unwrap();
    records
        .iter()
        .map(|record| {
            let text = |id| match &record.iter().find(|f| f.field_id == id).unwrap().value {
                Value::String(text) => text.clone(),
                other => panic!("text, not {other:?}"),
            };
            let Value::U64(interval) = record.iter().find(|f| f.field_id == 4).unwrap().value
            else {
                panic!("interval")
            };
            (text(1), text(2), text(3), interval)
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn routes_are_edited_with_actions_persist_and_a_factory_reset_removes_them() {
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
            ("route_add", ActionAccess::Mutate),
            ("route_remove", ActionAccess::Mutate),
            ("set_interval", ActionAccess::Mutate),
            ("fallback_add", ActionAccess::Mutate),
            ("fallback_remove", ActionAccess::Mutate),
            ("route_list", ActionAccess::Read),
            ("fallback_list", ActionAccess::Read),
            ("packet_get", ActionAccess::Read),
        ]
    );
    assert!(routes(&mut device).await.is_empty(), "no route by default");

    // A route needs an existing telemetry profile.
    let add = |route: &str, profile: &str, endpoint: &str| {
        vec![
            arg(1, text(route)),
            arg(2, text(profile)),
            arg(3, text(endpoint)),
        ]
    };
    assert!(
        device
            .invoke(
                "connectivity",
                "route_add",
                &add("r", "env", "http://a.example")
            )
            .await
            .is_err()
    );
    device
        .invoke(
            "telemetry",
            "profile_add",
            &[
                arg(1, text("env")),
                arg(2, Value::Enum("robonomics".into())),
            ],
        )
        .await
        .unwrap();
    // The longest supported endpoint (64 bytes) is listed back in full.
    let long = format!("http://{}.example.org:8080", "h".repeat(40));
    assert_eq!(long.len(), 64);
    device
        .invoke("connectivity", "route_add", &add("community", "env", &long))
        .await
        .unwrap();
    device
        .invoke(
            "connectivity",
            "set_interval",
            &[arg(1, text("community")), arg(2, Value::U64(120))],
        )
        .await
        .unwrap();
    device
        .invoke(
            "connectivity",
            "fallback_add",
            &[
                arg(1, text("community")),
                arg(2, text("backup")),
                arg(3, text("http://backup.example")),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        routes(&mut device).await,
        [("community".to_owned(), "env".to_owned(), long.clone(), 120)]
    );
    let fallbacks = device
        .invoke(
            "connectivity",
            "fallback_list",
            &[arg(1, text("community")), arg(2, Value::U64(0))],
        )
        .await
        .unwrap();
    assert_eq!(fallbacks.len(), 1);
    assert_eq!(fallbacks[0][2].value, Value::U64(1));

    // An invalid interval is refused; the route is unchanged.
    assert!(
        device
            .invoke(
                "connectivity",
                "set_interval",
                &[arg(1, text("community")), arg(2, Value::U64(5))],
            )
            .await
            .is_err()
    );
    // The service reports its route in the static status slots.
    let status = fields(&mut device, "connectivity").await;
    assert_eq!(
        status.get("route0.id"),
        Some(&Value::String("community".into()))
    );
    drop(device);
    assert!(sim.stop().success());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(routes(&mut device).await.len(), 1, "persisted");
    device.invoke("system", "factory_reset", &[]).await.unwrap();
    assert!(routes(&mut device).await.is_empty());
    drop(device);
    sim.stop();
}
