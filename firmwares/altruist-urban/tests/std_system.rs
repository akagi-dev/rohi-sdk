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
use rohi::model::{ActionAccess, FieldValue, Value};
use rohi::{Device, TcpLink};
use rohi_sdk::config::{ConfigStore, SectionId};
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

/// Every retained record of the archive, with the sequence of the archive.
async fn log_text(device: &mut Device<TcpLink>) -> String {
    archive_text(device).await
}

/// The sections stored in the configuration flash file of a stopped device.
fn stored_sections(storage: &std::path::Path) -> usize {
    let store = ConfigStore::open(FileFlash::open(storage).unwrap()).unwrap();
    store.section_count()
}

#[tokio::test(flavor = "current_thread")]
async fn system_has_explicit_access_and_a_status_without_crash_data_on_the_host() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;

    // The status exists for the reset and crash report of a device with
    // retained memory; the host has none, so it has no fields.
    let services = device.services().await.unwrap();
    assert!(services.iter().any(|service| service.name == "system"));

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
        .config_patch(
            "wireless",
            &[set(wireless::WIFI_SSID, text("home-net"))],
            false,
        )
        .await
        .unwrap();
    provision_identity(&mut device).await;
    assert!(provisioned(&mut device).await);

    // Whether a restart is needed is reported, and nothing restarts by itself.
    factory_reset(&mut device).await;

    // The runtime is back to the defaults, the identity is gone, and nothing
    // rebooted: the same process still answers.
    assert_eq!(
        config_text(&mut device, "wireless", wireless::WIFI_SSID).await,
        None
    );
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

    // The whole region was formatted: not even the unknown section is left. Only
    // the boot counter was written again, so that it only grows.
    assert_eq!(stored_sections(&storage), 1);

    // A restart finds the factory state, and counts one more start.
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(
        fields(&mut device, "system").await.get("boot_count"),
        Some(&Value::U64(2))
    );
    assert_eq!(
        config_text(&mut device, "wireless", wireless::WIFI_SSID).await,
        None
    );
    assert!(!provisioned(&mut device).await);
    drop(device);
    sim.stop();
}

#[tokio::test(flavor = "current_thread")]
async fn factory_reset_repairs_a_corrupted_configuration_storage() {
    let dir = TempDir::new().unwrap();
    let storage = dir.path().join("device.flash");

    // The committed slots lose their checksum: the store cannot open, but the
    // region can still be formatted.
    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    device
        .config_patch("wireless", &[set(wireless::WIFI_SSID, text("lost"))], false)
        .await
        .unwrap();
    drop(device);
    assert!(sim.stop().success());
    // The boot counter gave the store a second generation: both slots lose their
    // checksum, so that no committed slot is valid.
    let mut image = std::fs::read(&storage).unwrap();
    for slot in 0..2 {
        image[slot * 8192 + 16] ^= 0x01;
    }
    std::fs::write(&storage, image).unwrap();

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert!(
        device
            .config_patch(
                "wireless",
                &[set(wireless::WIFI_SSID, text("refused"))],
                false
            )
            .await
            .is_err(),
        "the corrupted store is unusable"
    );

    factory_reset(&mut device).await;
    // The reset restores the stored defaults of the endpoint, which apply at the next start,
    // never partly: this run keeps the endpoint it was started with (open by the launch option).
    device
        .config_patch(
            "wireless",
            &[set(wireless::WIFI_SSID, text("denied"))],
            false,
        )
        .await
        .unwrap();
    let tcp = fields(&mut device, "control_tcp").await;
    assert_eq!(tcp.get("access_mode"), Some(&text("open")));
    drop(device);
    assert!(sim.stop().success());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    device
        .config_patch(
            "wireless",
            &[set(wireless::WIFI_SSID, text("repaired"))],
            false,
        )
        .await
        .unwrap();
    drop(device);
    assert!(sim.stop().success());

    let sim = Sim::start(&Launch::new().storage(&storage));
    let mut device = sim.device().await;
    assert_eq!(
        config_text(&mut device, "wireless", wireless::WIFI_SSID)
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
        .config_patch("wireless", &[set(wireless::WIFI_SSID, text("kept"))], false)
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
        config_text(&mut device, "wireless", wireless::WIFI_SSID)
            .await
            .as_deref(),
        Some("kept")
    );
    drop(device);
    assert!(sim.stop().success());
}
