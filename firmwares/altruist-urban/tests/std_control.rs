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
//! modes of the TCP endpoint, the pre-shared key and the secure channel.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use rohi::model::{FieldValue, Value};
use rohi::{Device, Error, TcpKey, TcpLink, write_tcp_key};
use tempfile::TempDir;

const ACCESS_MODE: u32 = 2;
const SSID: u32 = wifi::SSID;

fn key(seed: u8) -> TcpKey {
    TcpKey::from_bytes([seed; 32]).unwrap()
}

fn key_file(dir: &Path, name: &str, key: &TcpKey) -> std::path::PathBuf {
    let path = dir.join(name);
    write_tcp_key(&path, key.as_bytes()).unwrap();
    path
}

async fn set_psk(device: &mut Device<TcpLink>, key: &TcpKey) -> Result<(), Error> {
    device
        .invoke(
            "control_psk",
            "set_tcp_psk",
            &[FieldValue {
                field_id: 1,
                value: Value::Bytes(key.as_bytes().to_vec()),
            }],
        )
        .await
        .map(|_| ())
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
    // Open for the run: a plaintext client can look at what the device offers.
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    let sections = device.config_sections().await.unwrap();
    for name in ["control_tcp", "control_ble"] {
        assert!(
            sections.iter().any(|section| section.name == name),
            "{name}"
        );
    }
    let tcp = fields(&mut device, "control_tcp").await;
    assert_eq!(tcp.get("state"), Some(&Value::Enum("running".into())));
    assert_eq!(tcp.get("access_mode"), Some(&text("open")));
    let ble = fields(&mut device, "control_ble").await;
    assert_eq!(ble.get("access_mode"), Some(&text("open")));
    // There is no key and the status never shows one.
    let psk = fields(&mut device, "control_psk").await;
    assert_eq!(psk.get("enrolled"), Some(&Value::Bool(false)));
    assert!(!psk.contains_key("fingerprint"));
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_restricted_endpoint_without_a_key_serves_nobody() {
    let sim = Sim::start(&Launch::new().restricted());
    // A plaintext client gets no answer in the clear.
    let mut plain = sim.device().await;
    assert!(plain.info().await.is_err(), "{}", sim.log());
    drop(plain);
    // A client with a key cannot authenticate to a device that has none.
    let result =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &key(1)).await;
    let link = match result {
        Ok(link) => link,
        Err(_) => {
            sim.stop();
            return;
        }
    };
    let mut device = Device::new(link);
    device.set_timeout(Duration::from_secs(5));
    assert!(device.info().await.is_err());
    drop(device);
    let log = sim.log();
    assert!(log.contains("No pre-shared key"), "{log}");
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_client_with_the_key_is_served_and_everyone_else_is_not() {
    let dir = TempDir::new().unwrap();
    let psk = key(7);
    let file = key_file(dir.path(), "tcp.key", &psk);
    let sim = Sim::start(&Launch::new().restricted().psk_file(&file));

    // Authenticated: reads, changes and what they read back.
    let mut device = sim.secure_device(&psk).await;
    device.info().await.unwrap();
    patch_ssid(&mut device, "first").await.unwrap();
    assert_eq!(
        config_text(&mut device, "wifi", SSID).await.as_deref(),
        Some("first")
    );
    let tcp = fields(&mut device, "control_tcp").await;
    assert_eq!(tcp.get("access_mode"), Some(&text("restricted")));
    drop(device);

    // Another key: the device proves nothing to it, and it never gets a request through.
    let stranger =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &key(8)).await;
    assert!(
        matches!(stranger, Err(Error::Unauthorized(_))),
        "a device is not trusted that cannot prove the key"
    );

    // No key at all: plaintext records are not served.
    let mut plain = sim.device().await;
    assert!(plain.info().await.is_err());
    drop(plain);

    // The refusals are visible in the counters and the authenticated client is unaffected.
    let mut device = sim.secure_device(&psk).await;
    let tcp = fields(&mut device, "control_tcp").await;
    assert!(
        matches!(tcp.get("errors"), Some(Value::U64(n)) if *n >= 1),
        "{tcp:?}"
    );
    assert_eq!(
        config_text(&mut device, "wifi", SSID).await.as_deref(),
        Some("first")
    );
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn the_key_is_never_provisioned_over_an_unauthenticated_link() {
    // An open endpoint takes every other request in the clear, but not the key.
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    let refused = set_psk(&mut device, &key(5)).await;
    assert_eq!(reason(refused), "encryption_required", "{}", sim.log());
    let psk = fields(&mut device, "control_psk").await;
    assert_eq!(psk.get("enrolled"), Some(&Value::Bool(false)));
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_key_is_rotated_over_the_secure_channel_and_ends_the_old_connections() {
    let dir = TempDir::new().unwrap();
    let old = key(1);
    let new = key(2);
    let file = key_file(dir.path(), "old.key", &old);
    let sim = Sim::start(&Launch::new().restricted().psk_file(&file));

    let mut device = sim.secure_device(&old).await;
    set_psk(&mut device, &new).await.unwrap();
    // The device ended the connection of the old key after the answer.
    assert!(device.info().await.is_err());
    drop(device);

    // The old key is refused from now on; the new one is served.
    let refused =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &old).await;
    assert!(matches!(refused, Err(Error::Unauthorized(_))));
    let mut device = sim.secure_device(&new).await;
    let psk = fields(&mut device, "control_psk").await;
    assert_eq!(psk.get("enrolled"), Some(&Value::Bool(true)));
    patch_ssid(&mut device, "rotated").await.unwrap();
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_stored_key_survives_a_restart_and_a_factory_reset_removes_it() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let psk = key(3);
    let bootstrap = key_file(dir.path(), "bootstrap.key", &key(9));

    // The first run authenticates with a key file, stores another and restricts the endpoint.
    let sim = Sim::start(
        &Launch::new()
            .storage(&storage)
            .restricted()
            .psk_file(&bootstrap),
    );
    let mut device = sim.secure_device(&key(9)).await;
    set_psk(&mut device, &psk).await.unwrap();
    drop(device);
    let mut device = sim.secure_device(&psk).await;
    device
        .config_patch(
            "control_tcp",
            &[set(ACCESS_MODE, Value::Enum("restricted".into()))],
            false,
        )
        .await
        .unwrap();
    drop(device);
    assert!(sim.stop().success());

    // Without a key file, the stored key applies: the device starts restricted and trusts it.
    let sim = Sim::start(&Launch::new().storage(&storage).restricted());
    let mut device = sim.secure_device(&psk).await;
    let state = fields(&mut device, "control_psk").await;
    assert_eq!(state.get("enrolled"), Some(&Value::Bool(true)));
    patch_ssid(&mut device, "kept").await.unwrap();

    // A factory reset removes the key; the connection of the old key ends with it.
    device.invoke("system", "factory_reset", &[]).await.unwrap();
    drop(device);
    let refused =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &psk).await;
    let log = sim.log();
    match refused {
        Err(_) => {}
        Ok(link) => {
            let mut device = Device::new(link);
            device.set_timeout(Duration::from_secs(5));
            assert!(device.info().await.is_err(), "{log}");
        }
    }
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_client_that_fails_the_proof_is_counted_and_never_processed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = TempDir::new().unwrap();
    let psk = key(7);
    let file = key_file(dir.path(), "tcp.key", &psk);
    let sim = Sim::start(&Launch::new().restricted().psk_file(&file));

    // A client that sends a well-formed hello and a final message that proves nothing, followed by
    // a request in the clear: the device ends the connection and answers nothing.
    let mut stream = tokio::net::TcpStream::connect(sim.control).await.unwrap();
    let mut hello = [0u8; 37];
    hello[..4].copy_from_slice(b"ROHI");
    hello[4] = 1;
    hello[5..].fill(0x66);
    stream.write_all(&hello).await.unwrap();
    let mut server_hello = [0u8; 69];
    stream.read_exact(&mut server_hello).await.unwrap();
    stream.write_all(&[0u8; 32]).await.unwrap();
    stream
        .write_all(&[4, 0, 0, 0, 0x08, 1, 0x10, 1])
        .await
        .unwrap();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
    assert!(matches!(read, Ok(Ok(0))), "{read:?}");
    assert!(
        rest.is_empty(),
        "nothing is answered to a client that did not authenticate"
    );

    let mut device = sim.secure_device(&psk).await;
    let tcp = fields(&mut device, "control_tcp").await;
    assert!(
        matches!(tcp.get("auth_failures"), Some(Value::U64(n)) if *n >= 1),
        "{tcp:?}"
    );
    drop(device);
    sim.stop();
}
