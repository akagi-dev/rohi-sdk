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
//! End to end: the `rohi` client against the real dispatcher and BLE framing,
//! connected in memory. No hardware, no Bluetooth adapter.

mod common;

use std::time::Duration;

use common::*;
use rohi::model::*;
use rohi::{ClientTransport, Device, Error, Session};
use rohi_config::Override;
use rohi_control::{Controller, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_control_proto::control as pb;
use rohi_net::WifiSsid;

fn text(value: &str) -> Value {
    Value::String(value.to_owned())
}

fn set(id: u32, value: Value) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::Set(value),
    }
}

fn none(id: u32) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::None,
    }
}

fn clear(id: u32) -> Mutation {
    Mutation {
        field_id: id,
        op: MutationOp::Clear,
    }
}

fn entry(values: &ConfigValues, id: u32) -> Option<&EntryState> {
    values
        .entries
        .iter()
        .find(|entry| entry.field_id == id)
        .map(|entry| &entry.state)
}

#[tokio::test(flavor = "current_thread")]
async fn info_and_discovery() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        let info = device.info().await.unwrap();
        assert_eq!(info.firmware, "fake-1.0.0");
        assert_eq!(info.board, "bench-esp32c3");

        let sections: Vec<_> = device
            .config_sections()
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.version))
            .collect();
        assert_eq!(
            sections,
            [
                ("wifi".to_owned(), 1),
                ("connectivity".to_owned(), 1),
                ("gadget".to_owned(), 3),
                ("location".to_owned(), 1)
            ]
        );
        let services: Vec<_> = device
            .services()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(services, ["radio", "identity", "location"]);
        let actions: Vec<_> = device
            .actions()
            .await
            .unwrap()
            .into_iter()
            .map(|a| format!("{}.{}", a.service, a.name))
            .collect();
        assert_eq!(
            actions,
            [
                "radio.scan",
                "radio.reconnect",
                "radio.tune",
                "radio.explode",
                "identity.provision",
                "identity.erase"
            ]
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_schema_describes_the_fields_of_a_section_the_client_has_never_seen() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        let schema = device.config_schema("gadget").await.unwrap();
        assert_eq!(schema.section.version, 3);
        let names: Vec<_> = schema
            .fields
            .iter()
            .map(|f| f.field.name.as_str())
            .collect();
        assert_eq!(names, ["label", "level", "mode", "serial"]);
        let level = &schema.fields[1];
        assert_eq!(level.field.kind, ValueKind::U64);
        assert_eq!(level.field.unit, "%");
        let mode = &schema.fields[2];
        assert!(mode.optional);
        assert_eq!(mode.field.variants, ["fast", "slow"]);
        assert!(!schema.fields[3].writable);

        let wifi = device.config_schema("wifi").await.unwrap();
        let password = wifi
            .fields
            .iter()
            .find(|f| f.field.name == "password")
            .unwrap();
        assert!(password.secret && password.optional);

        let error = device.config_schema("missing").await.unwrap_err();
        assert!(matches!(error, Error::Device(ErrorCode::NotFound)));
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_patch_is_stored_atomically_and_secrets_are_never_returned() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        // Nothing is overridden: the effective values are the defaults.
        let defaults = device.config_get("wifi").await.unwrap();
        assert!(defaults.overridden.is_empty());
        assert_eq!(entry(&defaults, 1), None);
        assert_eq!(entry(&defaults, 2), None);
        assert_eq!(
            entry(&defaults, 3),
            Some(&EntryState::Value(Value::U64(20_000)))
        );

        let applied = device
            .config_patch(
                "wifi",
                &[set(1, text("office")), set(2, text("hunter2hunter2"))],
                Some(0),
                false,
            )
            .await
            .unwrap();
        assert_eq!(applied.revision, 1);
        assert!(!applied.restart_required);

        let values = device.config_get("wifi").await.unwrap();
        assert_eq!(values.overridden, [1, 2]);
        assert_eq!(values.section.revision, 1);
        assert_eq!(entry(&values, 1), Some(&EntryState::Value(text("office"))));
        assert_eq!(entry(&values, 2), Some(&EntryState::Redacted));
        // The timing is not overridden: it is the default.
        assert!(!values.is_overridden(3));
        assert_eq!(
            entry(&values, 3),
            Some(&EntryState::Value(Value::U64(20_000)))
        );

        // Clearing the override of the password restores its default, none.
        device
            .config_patch("wifi", &[clear(2)], None, false)
            .await
            .unwrap();
        let values = device.config_get("wifi").await.unwrap();
        assert_eq!(entry(&values, 2), None);
        assert_eq!(values.overridden, [1]);
    })
    .await;
    let stored = bench.wifi.value.borrow().clone().unwrap();
    assert_eq!(
        stored.ssid,
        Override::Value(Some(WifiSsid::from_bytes(b"office").unwrap()))
    );
    assert_eq!(stored.password, Override::Default);
}

#[tokio::test(flavor = "current_thread")]
async fn none_is_an_explicit_override_that_clear_removes() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        // An open network is an explicit choice: no password, overridden.
        device
            .config_patch("wifi", &[set(1, text("office")), none(2)], None, false)
            .await
            .unwrap();
        let values = device.config_get("wifi").await.unwrap();
        assert_eq!(values.overridden, [1, 2]);
        assert_eq!(entry(&values, 2), None);

        // A required field has no "none"; nothing changes.
        let error = device
            .config_patch("wifi", &[none(3)], None, false)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Device(ErrorCode::InvalidArgument)));

        // A value replaces it, clear follows the default again.
        device
            .config_patch("wifi", &[set(2, text("hunter2hunter2"))], None, false)
            .await
            .unwrap();
        assert_eq!(
            entry(&device.config_get("wifi").await.unwrap(), 2),
            Some(&EntryState::Redacted)
        );
        device
            .config_patch("wifi", &[none(2)], None, false)
            .await
            .unwrap();
        device
            .config_patch("wifi", &[clear(2)], None, false)
            .await
            .unwrap();
        assert_eq!(device.config_get("wifi").await.unwrap().overridden, [1]);
    })
    .await;
    let stored = bench.wifi.value.borrow().clone().unwrap();
    assert_eq!(stored.password, Override::Default);
}

#[tokio::test(flavor = "current_thread")]
async fn an_invalid_mutation_stores_nothing() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        // The second mutation is out of range: the first must not be stored.
        let error = device
            .config_patch(
                "gadget",
                &[set(1, text("lamp")), set(2, Value::U64(1_000))],
                None,
                false,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Device(ErrorCode::InvalidArgument)));
        // A complete result that fails the typed validation is refused too.
        let error = device
            .config_patch("gadget", &[set(3, Value::Enum("fast".into()))], None, false)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Device(ErrorCode::InvalidArgument)));
        // So are a read-only field and an unknown one.
        assert!(
            device
                .config_patch("gadget", &[set(4, text("x"))], None, false)
                .await
                .is_err()
        );
        assert!(
            device
                .config_patch("gadget", &[clear(4)], None, false)
                .await
                .is_err()
        );
        assert!(
            device
                .config_patch("gadget", &[clear(99)], None, false)
                .await
                .is_err()
        );
    })
    .await;
    assert!(bench.gadget.value.borrow().is_none());
    assert_eq!(bench.writes(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn dry_runs_conflicts_restarts_and_reset() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        let dry = device
            .config_patch("gadget", &[set(1, text("lamp"))], None, true)
            .await
            .unwrap();
        assert_eq!(dry.revision, 0);
        assert!(
            device
                .config_get("gadget")
                .await
                .unwrap()
                .overridden
                .is_empty()
        );

        // This section only takes effect after a restart.
        let applied = device
            .config_patch("gadget", &[set(1, text("lamp"))], Some(0), false)
            .await
            .unwrap();
        assert!(applied.restart_required);

        let error = device
            .config_patch("gadget", &[set(2, Value::U64(7))], Some(0), false)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Device(ErrorCode::Conflict)));

        let reset = device.config_reset("gadget", None, false).await.unwrap();
        assert_eq!(reset.revision, 2);
        assert!(
            device
                .config_get("gadget")
                .await
                .unwrap()
                .overridden
                .is_empty()
        );
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn services_report_state_health_readiness_and_issues() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        let all = device.services_status().await.unwrap();
        assert_eq!(all.uptime_ms, 90_000);
        assert_eq!(all.health, DeviceHealth::Ok);
        let radio = &all.services[0];
        assert_eq!(radio.service, "radio");
        assert_eq!(radio.state, ServiceState::Running);
        assert!(radio.ready);
        let revision = radio.revision;

        let schema = device.service_schema("radio").await.unwrap();
        assert_eq!(schema.service.version, 2);
        assert_eq!(schema.fields[1].unit, "dBm");
        let status = device.service_status("radio").await.unwrap();
        assert_eq!(status.fields[0].value, text("office"));
        assert_eq!(status.fields[1].value, Value::I64(-61));
        assert!(status.issues.is_empty());

        assert!(matches!(
            device.service_status("missing").await.unwrap_err(),
            Error::Device(ErrorCode::NotFound)
        ));
        let _ = revision;
    })
    .await;

    bench.radio.degraded.set(true);
    with_device(20, &bench, |mut device| async move {
        let all = device.services_status().await.unwrap();
        assert_eq!(all.health, DeviceHealth::Degraded);
        assert_eq!(all.services[0].health, ServiceHealth::Degraded);
        let status = device.service_status("radio").await.unwrap();
        assert_eq!(status.issues[0].code, "link_lost");
        assert_eq!(status.issues[0].severity, IssueSeverity::Warning);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn actions_take_arguments_and_return_records() {
    let bench = Bench::new();
    with_device(20, &bench, |mut device| async move {
        let schema = device.action_schema("radio", "scan").await.unwrap();
        assert!(!schema.action.privileged);
        assert_eq!(schema.results.len(), 3);
        assert_eq!(schema.results[2].variants, ["open", "wpa2"]);
        assert!(
            device
                .action_schema("radio", "reconnect")
                .await
                .unwrap()
                .action
                .privileged
        );

        let records = device.invoke("radio", "scan", &[]).await.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1][0].value, text("guest"));
        assert_eq!(records[0][2].value, Value::Enum("wpa2".into()));

        device
            .invoke(
                "radio",
                "tune",
                &[FieldValue {
                    field_id: 1,
                    value: Value::U64(11),
                }],
            )
            .await
            .unwrap();

        // Missing arguments, unknown names and refusals are typed.
        assert!(matches!(
            device.invoke("radio", "tune", &[]).await.unwrap_err(),
            Error::Device(ErrorCode::InvalidArgument)
        ));
        assert!(matches!(
            device.invoke("radio", "missing", &[]).await.unwrap_err(),
            Error::Device(ErrorCode::NotFound)
        ));
        // `location` reports status but offers no actions.
        assert!(matches!(
            device.invoke("location", "scan", &[]).await.unwrap_err(),
            Error::Device(ErrorCode::NotSupported)
        ));
        assert!(matches!(
            device.invoke("nobody", "scan", &[]).await.unwrap_err(),
            Error::Device(ErrorCode::NotFound)
        ));
        let error = device.invoke("radio", "explode", &[]).await.unwrap_err();
        assert!(matches!(&error, Error::Rejected(reason) if reason == "not_today"));
    })
    .await;
    assert_eq!(
        bench.radio.calls.borrow().as_slice(),
        ["scan", "tune", "channel=11", "explode"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn works_with_a_large_mtu() {
    let bench = Bench::new();
    with_device(244, &bench, |mut device| async move {
        assert_eq!(device.services_status().await.unwrap().services.len(), 3);
        assert_eq!(device.invoke("radio", "scan", &[]).await.unwrap().len(), 2);
    })
    .await;
}

// ---------------------------------------------------------------------------
// Session behaviour
// ---------------------------------------------------------------------------

struct Silent;

impl ClientTransport for Silent {
    fn max_payload(&self) -> usize {
        20
    }

    async fn send(&mut self, _: &[u8]) -> rohi::Result<()> {
        Ok(())
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        std::future::pending().await
    }
}

#[tokio::test(flavor = "current_thread")]
async fn silent_device_times_out() {
    let mut session = Session::new(Silent);
    session.set_timeout(Duration::from_millis(50));
    let error = session
        .call(pb::Request_::Request::GetInfo(pb::GetInfoRequest {}))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Timeout));
}

/// Answers every request twice: first with a stale response of another
/// message, then with the right one.
struct StaleFirst {
    inner: ClientEnd,
    pending: Vec<Vec<u8>>,
}

impl ClientTransport for StaleFirst {
    fn max_payload(&self) -> usize {
        self.inner.max_payload()
    }

    async fn send(&mut self, chunk: &[u8]) -> rohi::Result<()> {
        self.inner.send(chunk).await
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        if let Some(chunk) = self.pending.pop() {
            return Ok(chunk);
        }
        let mut chunk = self.inner.recv().await?;
        // Pretend this is the end of an older message (id + 1).
        let mut stale = chunk.clone();
        stale[1] = stale[1].wrapping_add(1);
        self.pending.push(chunk.clone());
        chunk.clear();
        Ok(stale)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stale_responses_are_skipped() {
    let bench = Bench::new();
    let (client, device) = link(244);
    let controller = Controller::new(backend(&bench), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    let transport = StaleFirst {
        inner: client,
        pending: Vec::new(),
    };
    tokio::select! {
        () = async { server.run().await } => unreachable!(),
        () = async {
            let mut device = Device::new(transport);
            assert_eq!(device.info().await.unwrap().firmware, "fake-1.0.0");
        } => {}
    }
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// The public Substrate development phrase. Never a production master.
const DEV_PHRASE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";

fn dev_identity(number: u64) -> rohi::DerivedIdentity {
    rohi::derive_identity(DEV_PHRASE, number).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn identity_is_an_ordinary_service_with_actions() {
    let bench = Bench::new();
    let derived = dev_identity(7);
    let secret = derived.secret().expose_secret().to_vec();
    let expected = derived.public_key();
    with_device(20, &bench, |mut device| async move {
        let status = device.service_status("identity").await.unwrap();
        assert_eq!(status.summary.state, ServiceState::Stopped);
        assert_eq!(status.issues[0].code, "not_provisioned");

        let schema = device.action_schema("identity", "provision").await.unwrap();
        assert!(schema.action.privileged);
        let records = device
            .invoke(
                "identity",
                "provision",
                &[FieldValue {
                    field_id: schema.args[0].id,
                    value: Value::Bytes(secret.clone()),
                }],
            )
            .await
            .unwrap();
        assert_eq!(
            records[0][0].value,
            Value::Bytes(expected.to_bytes().to_vec())
        );

        let status = device.service_status("identity").await.unwrap();
        assert_eq!(status.summary.state, ServiceState::Running);
        // The public key is reported, the secret never is.
        assert!(
            status
                .fields
                .iter()
                .all(|f| f.value != Value::Bytes(secret.clone()))
        );

        let error = device
            .invoke(
                "identity",
                "provision",
                &[FieldValue {
                    field_id: schema.args[0].id,
                    value: Value::Bytes(secret.clone()),
                }],
            )
            .await
            .unwrap_err();
        assert!(matches!(&error, Error::Rejected(reason) if reason == "already_provisioned"));

        device.invoke("identity", "erase", &[]).await.unwrap();
    })
    .await;
    assert!(bench.identity.0.borrow().is_none());
}

/// Lets the radio change as the `n`-th request is sent, so that the service
/// moves on between the compact and the detailed status.
struct Changing<'a> {
    inner: ClientEnd,
    radio: &'a Radio,
    sent: usize,
    from: usize,
    every: bool,
}

impl ClientTransport for Changing<'_> {
    fn max_payload(&self) -> usize {
        self.inner.max_payload()
    }

    async fn send(&mut self, chunk: &[u8]) -> rohi::Result<()> {
        self.sent += 1;
        if self.sent == self.from || (self.every && self.sent > self.from) {
            self.radio.degraded.set(!self.radio.degraded.get());
        }
        self.inner.send(chunk).await
    }

    async fn recv(&mut self) -> rohi::Result<Vec<u8>> {
        self.inner.recv().await
    }
}

async fn read_status_while_changing(every: bool) -> rohi::Result<ServiceStatus> {
    let bench = Bench::new();
    let (client, device) = link(244);
    let controller = Controller::new(backend(&bench), OpenPolicy);
    let mut server = ControlBle::new(device, &controller);
    // Requests: services, schema, then the compact status (3rd) and the
    // detailed one (4th), which is the one that changes the radio first.
    let transport = Changing {
        inner: client,
        radio: &bench.radio,
        sent: 0,
        from: 4,
        every,
    };
    tokio::select! {
        () = async { server.run().await } => unreachable!(),
        result = async { Device::new(transport).service_status("radio").await } => result,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_service_changing_between_compact_and_detailed_status_is_read_again() {
    let status = read_status_while_changing(false).await.unwrap();
    // Never the old summary with the new details.
    assert_eq!(status.summary.health, ServiceHealth::Degraded);
    assert_eq!(status.issues[0].code, "link_lost");
}

#[tokio::test(flavor = "current_thread")]
async fn a_service_that_keeps_changing_is_an_error_not_a_mixed_status() {
    let error = read_status_while_changing(true).await.unwrap_err();
    assert!(matches!(error, Error::StatusChanging(service) if service == "radio"));
}
