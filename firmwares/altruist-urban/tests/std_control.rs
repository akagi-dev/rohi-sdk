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
//! The managed control endpoints of the std platform end to end: the access
//! modes of the TCP endpoint, the administrator key, and signed requests.

mod common;

use common::*;
use rohi::model::{FieldValue, Value};
use rohi::{AdminKey, Device, Error, TcpLink};
use tempfile::TempDir;

const ACCESS_MODE: u32 = 2;
const SSID: u32 = wifi::SSID;

fn key(seed: u8) -> AdminKey {
    AdminKey::from_seed(&[seed; 32]).unwrap()
}

async fn enroll(device: &mut Device<TcpLink>, key: &AdminKey) {
    device
        .invoke(
            "control_auth",
            "set_admin_key",
            &[FieldValue {
                field_id: 1,
                value: Value::Bytes(key.public_key().to_bytes().to_vec()),
            }],
        )
        .await
        .unwrap();
}

async fn restrict(device: &mut Device<TcpLink>) {
    device
        .config_patch(
            "control_tcp",
            &[set(ACCESS_MODE, Value::Enum("restricted".into()))],
            false,
        )
        .await
        .unwrap();
}

async fn patch_ssid(device: &mut Device<TcpLink>, ssid: &str) -> Result<(), Error> {
    device
        .config_patch("wifi", &[set(SSID, text(ssid))], false)
        .await
        .map(|_| ())
}

fn reason(result: Result<(), Error>) -> String {
    match result {
        Err(Error::Unauthorized(reason)) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn the_endpoints_are_discoverable_with_their_defaults() {
    let sim = Sim::start(&Launch::new().restricted());
    let mut device = sim.device().await;
    let sections = device.config_sections().await.unwrap();
    for name in ["control_tcp", "control_ble"] {
        assert!(
            sections.iter().any(|section| section.name == name),
            "{name}"
        );
    }
    let tcp = fields(&mut device, "control_tcp").await;
    assert_eq!(tcp.get("access_mode"), Some(&text("restricted")));
    assert_eq!(tcp.get("state"), Some(&Value::Enum("running".into())));
    assert_eq!(tcp.get("sessions"), Some(&Value::U64(1)));
    let ble = fields(&mut device, "control_ble").await;
    assert_eq!(ble.get("access_mode"), Some(&text("open")));
    let auth = fields(&mut device, "control_auth").await;
    assert_eq!(auth.get("enrolled"), Some(&Value::Bool(false)));
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_restricted_endpoint_without_a_key_refuses_every_change_but_serves_reads() {
    let sim = Sim::start(&Launch::new().restricted());
    let mut device = sim.device().await;
    device.info().await.unwrap();
    assert_eq!(
        reason(patch_ssid(&mut device, "nope").await),
        "no_admin_key"
    );
    // A client with a key cannot enroll it either: nothing is trusted yet.
    device.set_admin_key(&key(1));
    assert_eq!(
        reason(patch_ssid(&mut device, "nope").await),
        "no_admin_key"
    );
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_signed_request_is_accepted_and_others_are_refused() {
    // Open for the run, so that the key can be enrolled the way a BLE
    // central would.
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    let admin = key(1);
    enroll(&mut device, &admin).await;
    restrict(&mut device).await;
    let auth = fields(&mut device, "control_auth").await;
    assert_eq!(auth.get("enrolled"), Some(&Value::Bool(true)));

    // Reads stay unsigned.
    device.info().await.unwrap();
    // A change without a key is refused before it runs.
    assert_eq!(
        reason(patch_ssid(&mut device, "unsigned").await),
        "auth_required"
    );
    // Another key is refused, too.
    device.set_admin_key(&key(2));
    assert_eq!(
        reason(patch_ssid(&mut device, "stranger").await),
        "bad_signature"
    );
    drop(device);

    // A fresh connection signs with the right key, repeatedly.
    let mut device = sim.device().await;
    device.set_admin_key(&admin);
    patch_ssid(&mut device, "first").await.unwrap();
    patch_ssid(&mut device, "second").await.unwrap();
    assert_eq!(
        config_text(&mut device, "wifi", SSID).await.as_deref(),
        Some("second")
    );
    let tcp = fields(&mut device, "control_tcp").await;
    assert!(matches!(tcp.get("denied"), Some(Value::U64(n)) if *n >= 2));
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn the_key_and_the_mode_survive_a_restart_and_a_reset_removes_them() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let admin = key(3);

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    enroll(&mut device, &admin).await;
    restrict(&mut device).await;
    drop(device);
    assert!(sim.stop().success());

    // The device starts restricted, as stored, and still trusts the key.
    let sim = Sim::start(&Launch::new().storage(&storage).restricted());
    let mut device = sim.device().await;
    assert_eq!(
        reason(patch_ssid(&mut device, "unsigned").await),
        "auth_required"
    );
    device.set_admin_key(&admin);
    patch_ssid(&mut device, "kept").await.unwrap();

    // The key can be replaced only with a signature of the current one;
    // afterwards the old key no longer signs.
    let successor = key(4);
    enroll(&mut device, &successor).await;
    drop(device);
    let mut device = sim.device().await;
    device.set_admin_key(&admin);
    assert_eq!(
        reason(patch_ssid(&mut device, "old").await),
        "bad_signature"
    );
    drop(device);
    let mut device = sim.device().await;
    device.set_admin_key(&successor);
    // A factory reset restores the defaults: no key, restricted.
    device.invoke("system", "factory_reset", &[]).await.unwrap();
    let auth = fields(&mut device, "control_auth").await;
    assert_eq!(auth.get("enrolled"), Some(&Value::Bool(false)));
    drop(device);
    let mut device = sim.device().await;
    device.set_admin_key(&successor);
    assert_eq!(
        reason(patch_ssid(&mut device, "after").await),
        "no_admin_key"
    );
    drop(device);
    sim.stop();
}
