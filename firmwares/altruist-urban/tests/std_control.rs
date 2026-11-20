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
//! The managed control endpoints of the std platform end to end: the access
//! modes of the TCP endpoint, the pre-shared key and the secure channel.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use rohi::model::{EntryState, Value};
use rohi::{Device, Error, TcpKey, TcpLink, write_tcp_key};
use tempfile::TempDir;

const TCP_ACCESS_MODE: u32 = 1;
const TCP_PORT: u32 = 3;
const SSID: u32 = wireless::WIFI_SSID;

fn key(seed: u8) -> TcpKey {
    TcpKey::from_bytes([seed; 32]).unwrap()
}

fn key_file(dir: &Path, name: &str, key: &TcpKey) -> std::path::PathBuf {
    let path = dir.join(name);
    write_tcp_key(&path, key.as_bytes()).unwrap();
    path
}

/// Asks the device for a new key; the device generates it.
async fn generate_psk(device: &mut Device<TcpLink>) -> Result<TcpKey, Error> {
    let records = device.invoke("control", "tcp_generate_psk", &[]).await?;
    let bytes = records
        .iter()
        .flatten()
        .find_map(|field| match (&field.field_id, &field.value) {
            (1, Value::Bytes(bytes)) => Some(bytes.clone()),
            _ => None,
        })
        .expect("the result carries the key");
    Ok(TcpKey::from_bytes(bytes.try_into().expect("32 bytes")).unwrap())
}

async fn patch_ssid(device: &mut Device<TcpLink>, ssid: &str) -> Result<(), Error> {
    device
        .config_patch("wireless", &[set(SSID, text(ssid))], false)
        .await
        .map(|_| ())
}

fn reason(result: Result<(), Error>) -> String {
    match result {
        Err(Error::Unauthorized(reason)) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn counter(status: &std::collections::HashMap<String, Value>, name: &str) -> u64 {
    match status.get(name) {
        Some(Value::U64(value)) => *value,
        other => panic!("{name}: {other:?}"),
    }
}

/// The effective value of a field of the `control` config section.
async fn control_setting(device: &mut Device<TcpLink>, field: u32) -> Option<Value> {
    let values = device.config_get("control").await.unwrap();
    values
        .entries
        .into_iter()
        .find(|entry| entry.field_id == field)
        .and_then(|entry| match entry.state {
            EntryState::Value(value) => Some(value),
            _ => None,
        })
}

/// Whether a client without a key is served at all: a plaintext connection that answers `info`.
async fn plain_is_served(sim: &Sim) -> bool {
    let link = TcpLink::connect(&sim.control.to_string(), Duration::from_secs(10)).await;
    let Ok(link) = link else {
        return false;
    };
    let mut device = Device::new(link);
    device.set_timeout(Duration::from_secs(5));
    device.info().await.is_ok()
}

#[tokio::test(flavor = "current_thread")]
async fn one_control_service_and_section_are_discoverable_with_their_defaults() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    let sections = device.config_sections().await.unwrap();
    assert!(sections.iter().any(|section| section.name == "control"));
    assert!(sections.iter().all(|section| section.name != "control_tcp"));
    // No legacy names, no aliases.
    for retired in ["control_tcp", "control_ble"] {
        assert!(device.service_schema(retired).await.is_err(), "{retired}");
    }
    assert!(
        device
            .invoke("control_tcp", "reveal_psk", &[])
            .await
            .is_err()
    );

    // Three settings and their defaults, none of which is a status field.
    let values = device.config_get("control").await.unwrap();
    assert_eq!(values.entries.len(), 3);
    assert_eq!(
        control_setting(&mut device, TCP_ACCESS_MODE).await,
        Some(Value::Enum("protected".into()))
    );
    assert_eq!(
        control_setting(&mut device, 2).await,
        Some(text("127.0.0.1"))
    );
    assert_eq!(
        control_setting(&mut device, TCP_PORT).await,
        Some(Value::U64(9300))
    );

    // Exactly five status fields; nothing else about the endpoint, the key or the mode.
    let status = fields(&mut device, "control").await;
    let mut names: Vec<_> = status.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "ble_sessions",
            "errors",
            "requests",
            "tcp_sessions",
            "tcp_state"
        ]
    );
    assert_eq!(
        status.get("tcp_state"),
        Some(&Value::Enum("running".into()))
    );
    assert_eq!(counter(&status, "tcp_sessions"), 1);
    assert_eq!(counter(&status, "ble_sessions"), 0);
    assert_eq!(counter(&status, "errors"), 0);
    assert!(counter(&status, "requests") >= 1);
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_protected_endpoint_without_a_key_reads_and_refuses_the_rest() {
    let sim = Sim::start(&Launch::new().protected());
    let mut plain = sim.device().await;
    // Reads work and the endpoint is running although there is no key.
    plain.info().await.unwrap();
    let status = fields(&mut plain, "control").await;
    assert_eq!(
        status.get("tcp_state"),
        Some(&Value::Enum("running".into()))
    );
    // Mutations and provisioning are denied, and say there is no key to authenticate with.
    assert_eq!(reason(patch_ssid(&mut plain, "no").await), "no_psk");
    let refused = generate_psk(&mut plain).await.map(|_| ());
    assert_eq!(reason(refused), "no_psk", "{}", sim.log());
    // Denials are not errors, and the requests are counted.
    let status = fields(&mut plain, "control").await;
    assert_eq!(counter(&status, "errors"), 0);
    assert!(counter(&status, "requests") >= 5);
    drop(plain);

    // A client that asks for the handshake cannot authenticate to a device that has no key.
    let result =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &key(1)).await;
    if let Ok(link) = result {
        let mut device = Device::new(link);
        device.set_timeout(Duration::from_secs(5));
        assert!(device.info().await.is_err());
    }
    let log = sim.log();
    assert!(log.contains("No pre-shared key"), "{log}");
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_protected_endpoint_serves_plaintext_reads_and_the_key_serves_everything() {
    let dir = TempDir::new().unwrap();
    let psk = key(7);
    let file = key_file(dir.path(), "tcp.key", &psk);
    let sim = Sim::start(&Launch::new().protected().psk_file(&file));

    // Authenticated: reads, changes and what they read back.
    let mut device = sim.secure_device(&psk).await;
    device.info().await.unwrap();
    patch_ssid(&mut device, "first").await.unwrap();
    assert_eq!(
        config_text(&mut device, "wireless", SSID).await.as_deref(),
        Some("first")
    );
    drop(device);

    // Plaintext on the same endpoint: reads, and the change made over the secure channel shows;
    // a mutation or a key is refused, and says why.
    let mut plain = sim.device().await;
    assert_eq!(
        config_text(&mut plain, "wireless", SSID).await.as_deref(),
        Some("first")
    );
    assert_eq!(
        reason(patch_ssid(&mut plain, "second").await),
        "encryption_required"
    );
    let refused = generate_psk(&mut plain).await.map(|_| ());
    assert_eq!(reason(refused), "encryption_required");
    drop(plain);

    // Another key: the device proves nothing to it, and it never gets a request through.
    let before = {
        let mut device = sim.secure_device(&psk).await;
        counter(&fields(&mut device, "control").await, "errors")
    };
    let stranger =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &key(8)).await;
    assert!(
        matches!(stranger, Err(Error::Unauthorized(_))),
        "a device is not trusted that cannot prove the key"
    );

    // The failed handshake is one error, and the authenticated client is unaffected.
    let mut device = eventually("the failed handshake to be counted", WAIT, async || {
        let mut device = sim.secure_device(&psk).await;
        let errors = counter(&fields(&mut device, "control").await, "errors");
        (errors > before).then_some((device, errors))
    })
    .await;
    assert_eq!(device.1, before + 1);
    assert_eq!(
        config_text(&mut device.0, "wireless", SSID)
            .await
            .as_deref(),
        Some("first")
    );
    drop(device);
    sim.stop();
}

const WAIT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "current_thread")]
async fn a_private_endpoint_serves_only_the_authenticated_and_never_falls_back_to_plaintext() {
    let dir = TempDir::new().unwrap();
    let psk = key(5);
    let file = key_file(dir.path(), "tcp.key", &psk);
    let sim = Sim::start(&Launch::new().private().psk_file(&file));

    // Not even a read in plaintext.
    assert!(!plain_is_served(&sim).await, "{}", sim.log());
    // The endpoint still runs; the key opens it, for reads and changes alike.
    let mut device = sim.secure_device(&psk).await;
    device.info().await.unwrap();
    patch_ssid(&mut device, "private").await.unwrap();
    let status = fields(&mut device, "control").await;
    assert_eq!(
        status.get("tcp_state"),
        Some(&Value::Enum("running".into()))
    );
    // Refusing plaintext is policy, not a failure.
    assert_eq!(counter(&status, "errors"), 0);
    drop(device);
    assert!(!plain_is_served(&sim).await);
    sim.stop();

    // Without a key at all nobody is served, though the listener runs.
    let sim = Sim::start(&Launch::new().private());
    assert!(!plain_is_served(&sim).await);
    let result =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &key(5)).await;
    if let Ok(link) = result {
        let mut device = Device::new(link);
        device.set_timeout(Duration::from_secs(5));
        assert!(device.info().await.is_err());
    }
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn an_open_endpoint_takes_everything_but_never_hands_the_key_to_plaintext() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;
    // Mutations need no key in open mode.
    patch_ssid(&mut device, "open").await.unwrap();
    // The secret is an independent rule: the key does not travel in plaintext.
    let refused = generate_psk(&mut device).await.map(|_| ());
    assert_eq!(reason(refused), "encryption_required", "{}", sim.log());
    // An open endpoint is a warning, not a failure.
    let (health, issues) = summary(&mut device, "control").await;
    assert_eq!(health, rohi::model::ServiceHealth::Ok);
    assert!(issues.contains(&"unsecured".to_owned()), "{issues:?}");
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_key_is_rotated_over_the_secure_channel_and_ends_the_old_connections() {
    let dir = TempDir::new().unwrap();
    let old = key(1);
    let file = key_file(dir.path(), "old.key", &old);
    let sim = Sim::start(&Launch::new().protected().psk_file(&file));

    let mut device = sim.secure_device(&old).await;
    let new = generate_psk(&mut device).await.unwrap();
    assert_ne!(new.as_bytes(), old.as_bytes());
    // The device ended the connection of the old key after the answer.
    assert!(device.info().await.is_err());
    drop(device);

    // The old key is refused from now on; the new one is served.
    let refused =
        TcpLink::connect_secure(&sim.control.to_string(), Duration::from_secs(10), &old).await;
    assert!(matches!(refused, Err(Error::Unauthorized(_))));
    let mut device = sim.secure_device(&new).await;
    patch_ssid(&mut device, "rotated").await.unwrap();
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_stored_key_and_mode_survive_a_restart_and_a_factory_reset_removes_the_key() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let bootstrap = key_file(dir.path(), "bootstrap.key", &key(9));

    // The first run authenticates with a key file, stores another and makes the endpoint private.
    let sim = Sim::start(
        &Launch::new()
            .storage(&storage)
            .protected()
            .psk_file(&bootstrap),
    );
    let mut device = sim.secure_device(&key(9)).await;
    let psk = generate_psk(&mut device).await.unwrap();
    drop(device);
    let mut device = sim.secure_device(&psk).await;
    device
        .config_patch(
            "control",
            &[set(TCP_ACCESS_MODE, Value::Enum("private".into()))],
            false,
        )
        .await
        .unwrap();
    drop(device);
    // It applies at the next start, never now: this run still serves plaintext reads.
    assert!(plain_is_served(&sim).await);
    assert!(sim.stop().success());

    // Without a key file, the stored key applies, and so does the stored mode.
    let sim = Sim::start(&Launch::new().storage(&storage).stored_mode());
    assert!(!plain_is_served(&sim).await);
    let mut device = sim.secure_device(&psk).await;
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
async fn a_client_that_fails_the_proof_is_counted_once_and_never_processed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = TempDir::new().unwrap();
    let psk = key(7);
    let file = key_file(dir.path(), "tcp.key", &psk);
    let sim = Sim::start(&Launch::new().protected().psk_file(&file));

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
    let status = fields(&mut device, "control").await;
    assert_eq!(counter(&status, "errors"), 1, "{status:?}");
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn a_taken_stored_port_fails_the_endpoint_and_nothing_else() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();

    // The port is stored; it applies at the next start, never at once.
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    device
        .config_patch("control", &[set(TCP_PORT, Value::U64(port.into()))], false)
        .await
        .unwrap();
    let status = fields(&mut device, "control").await;
    assert_eq!(
        status.get("tcp_state"),
        Some(&Value::Enum("running".into()))
    );
    drop(device);
    assert!(sim.stop().success());

    // The device starts anyway: the endpoint is reported failed, the rest runs.
    let sim = Sim::start(
        &Launch::new()
            .storage(&storage)
            .stored_port()
            .arg("--manage-port")
            .arg("0"),
    );
    let log = sim.log();
    assert!(log.contains("cannot listen"), "{log}");
    assert!(sim.stop().success());
}
