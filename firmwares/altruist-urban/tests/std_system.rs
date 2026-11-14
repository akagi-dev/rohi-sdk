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

//! The `system` service on the host: reboot and factory reset are ordinary
//! actions of a real simulator process, driven through the real `rohi` client.

mod common;

use std::time::Duration;

use common::*;
use rohi::model::{ActionAccess, ActionOutcome, FieldValue, Stream, Value};
use rohi::{Device, TcpLink};
use rohi_sdk::config::{ConfigStore, SectionId};
use rohi_sdk::logging::read_arg;
use rohi_sdk::sim::storage::FileFlash;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(20);

/// Runs `system.factory_reset` and returns `restart_required`.
async fn factory_reset(device: &mut Device<TcpLink>) -> bool {
    let records = device.invoke("system", "factory_reset", &[]).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].len(), 1);
    match &records[0][0].value {
        Value::Bool(required) => *required,
        other => panic!("restart_required is {other:?}"),
    }
}

async fn provision_identity(device: &mut Device<TcpLink>) {
    let secret = FieldValue {
        field_id: 1,
        value: Value::Bytes(vec![7; 32]),
    };
    device
        .invoke("identity", "provision", &[secret])
        .await
        .unwrap();
}

async fn provisioned(device: &mut Device<TcpLink>) -> bool {
    matches!(
        fields(device, "identity").await.get("provisioned"),
        Some(Value::Bool(true))
    )
}

/// Every retained record: one `logging.read` returns about one `READ_BYTES` of
/// text, so it asks again after the last sequence.
async fn log_text(device: &mut Device<TcpLink>) -> String {
    let arg = |field_id, value| FieldValue {
        field_id,
        value: Value::U64(value),
    };
    let mut text = String::new();
    let mut after = 0;
    loop {
        let outcome = device
            .invoke_outcome(
                "logging",
                "read",
                &[arg(read_arg::AFTER, after), arg(read_arg::LIMIT, 500)],
            )
            .await
            .unwrap();
        let ActionOutcome::Stream(stream) = outcome else {
            panic!("logging.read returned records");
        };
        let data = read_file(device, &stream).await;
        device.close_stream(&stream).await.unwrap();
        let page = String::from_utf8(data).unwrap();
        let Some(last) = page
            .lines()
            .filter_map(|line| line.split(' ').next()?.parse::<u64>().ok())
            .last()
        else {
            return text;
        };
        after = last;
        text.push_str(&page);
    }
}

async fn read_file(device: &mut Device<TcpLink>, stream: &Stream) -> Vec<u8> {
    let file = stream.files[0].id;
    let mut data = Vec::new();
    loop {
        let chunk = device
            .read_stream(stream, file, data.len() as u64)
            .await
            .unwrap();
        data.extend_from_slice(&chunk.data);
        if chunk.eof {
            return data;
        }
    }
}

/// The sections stored in the configuration flash file of a stopped device.
fn stored_sections(storage: &std::path::Path) -> usize {
    let store = ConfigStore::open(FileFlash::open(storage).unwrap()).unwrap();
    store.section_count()
}

#[tokio::test(flavor = "current_thread")]
async fn system_is_an_action_only_service_with_explicit_access() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;

    // No status: the service is not in the list of services.
    let services = device.services().await.unwrap();
    assert!(services.iter().all(|service| service.name != "system"));

    let actions = device.actions().await.unwrap();
    let system: Vec<_> = actions
        .iter()
        .filter(|action| action.service == "system")
        .map(|action| (action.name.as_str(), action.access, action.secret_args))
        .collect();
    assert_eq!(
        system,
        [
            ("reboot", ActionAccess::Mutate, false),
            ("factory_reset", ActionAccess::Provision, false),
        ]
    );
    let reboot = device.action_schema("system", "reboot").await.unwrap();
    assert!(reboot.args.is_empty() && reboot.results.is_empty());
    let reset = device
        .action_schema("system", "factory_reset")
        .await
        .unwrap();
    assert!(reset.args.is_empty());
    assert_eq!(reset.results.len(), 1);
    assert_eq!(reset.results[0].name, "restart_required");
    drop(device);
    assert!(sim.stop().success());
}

#[tokio::test(flavor = "current_thread")]
async fn factory_reset_formats_the_configuration_and_keeps_the_log_and_the_process() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");

    // A section this firmware does not know, as an older one could have left.
    {
        let mut store = ConfigStore::format(FileFlash::open(&storage).unwrap())
            .map_err(|failure| failure.error)
            .unwrap();
        store
            .set_raw(SectionId::from_name("obsolete"), 3, &[1, 2, 3, 4])
            .unwrap();
        assert_eq!(store.section_count(), 1);
    }

    let mut sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    device
        .config_patch("wifi", &[set(wifi::SSID, text("home-net"))], false)
        .await
        .unwrap();
    provision_identity(&mut device).await;
    assert!(provisioned(&mut device).await);

    // Whether a restart is needed is reported, and nothing restarts by itself.
    factory_reset(&mut device).await;

    // The runtime is back to the defaults, the identity is gone, and nothing
    // rebooted: the same process still answers.
    assert_eq!(config_text(&mut device, "wifi", wifi::SSID).await, None);
    assert!(!provisioned(&mut device).await);
    assert!(sim.wait_exit(Duration::from_millis(500)).is_none());
    device.info().await.unwrap();
    // The persistent log was not touched: the records from before the reset
    // are still there, and so is the record of the reset itself.
    let log = eventually("the log to show the reset", WAIT, async || {
        let log = log_text(&mut device).await;
        log.contains("factory reset").then_some(log)
    })
    .await;
    assert!(log.contains("Starting urban-std"), "{log}");
    drop(device);
    assert!(sim.stop().success());

    // The whole region was formatted: not even the unknown section is left.
    assert_eq!(stored_sections(&storage), 0);

    // A restart finds the factory state.
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(config_text(&mut device, "wifi", wifi::SSID).await, None);
    assert!(!provisioned(&mut device).await);
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn factory_reset_repairs_a_corrupted_configuration_storage() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");

    // The only committed slot loses its checksum: the store cannot open, but
    // the region can still be formatted.
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    device
        .config_patch("wifi", &[set(wifi::SSID, text("lost"))], false)
        .await
        .unwrap();
    drop(device);
    sim.manage_ok("storage corrupt crc");
    assert!(sim.stop().success());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert!(
        device
            .config_patch("wifi", &[set(wifi::SSID, text("refused"))], false)
            .await
            .is_err(),
        "the corrupted store is unusable"
    );

    factory_reset(&mut device).await;
    device
        .config_patch("wifi", &[set(wifi::SSID, text("repaired"))], false)
        .await
        .unwrap();
    drop(device);
    assert!(sim.stop().success());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(
        config_text(&mut device, "wifi", wifi::SSID)
            .await
            .as_deref(),
        Some("repaired")
    );
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn reboot_answers_first_and_restarts_the_process_in_place() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    device
        .config_patch("wifi", &[set(wifi::SSID, text("kept"))], false)
        .await
        .unwrap();

    let asked = std::time::Instant::now();

    // The Action succeeds, and the link still works right after: the reset
    // is deferred, not synchronous.
    let records = device.invoke("system", "reboot", &[]).await.unwrap();
    assert!(records.is_empty());
    device.info().await.unwrap();
    drop(device);

    // The process restarts on the same ports and keeps its configuration:
    // reboot is not a factory reset.
    let mut device = eventually("the device to come back", WAIT, async || {
        let link = TcpLink::connect(&sim.control.to_string(), Duration::from_secs(1))
            .await
            .ok()?;
        let mut device = Device::new(link);
        device.set_timeout(Duration::from_secs(5));
        let uptime = device.services_status().await.ok()?.uptime_ms;
        // A restarted process counts its uptime from zero, and the deferred
        // reboot leaves at least a second for the response.
        (uptime + 1_000 < asked.elapsed().as_millis() as u64).then_some(device)
    })
    .await;
    assert_eq!(
        config_text(&mut device, "wifi", wifi::SSID)
            .await
            .as_deref(),
        Some("kept")
    );
    drop(device);
    assert!(sim.stop().success());
}
