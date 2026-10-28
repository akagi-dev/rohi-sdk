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

//! Services with every subset of the capabilities, composed with explicit
//! tuples and dispatched by the static registries.

use core::cell::Cell;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{
    Action, ActionDescriptor, ActionError, ActionOutput, ActionRegistry, FieldDescriptor,
    ISSUE_STARTUP_FAILED, Service, ServiceHealth, ServiceState, ServiceSummary, Startup, Status,
    StatusError, StatusRegistry, StatusWriter, Statuses, Value, ValueKind, WriteError,
};

/// Status only.
#[derive(Service)]
struct SensorsService {
    pm25: Cell<u64>,
}

impl Status for SensorsService {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] =
        &[FieldDescriptor::new(1, "pm25", ValueKind::U64)];

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.u64(1, self.pm25.get())
    }
}

/// Actions only.
#[derive(Service)]
struct RebootService;

impl Action for RebootService {
    const ACTIONS: &'static [ActionDescriptor] = &[ActionDescriptor::new(1, "now").privileged()];

    async fn invoke(
        &self,
        action: u32,
        _args: &[(u32, Value)],
        _out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            1 => Err(ActionError::Rejected("not_now")),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

/// Status and actions.
#[derive(Service)]
#[service(name = "wifi")]
struct WirelessService {
    scans: Cell<u64>,
}

impl Status for WirelessService {
    const VERSION: u16 = 3;
    const STATUS_SCHEMA: &'static [FieldDescriptor] =
        &[FieldDescriptor::new(1, "scans", ValueKind::U64)];

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.u64(1, self.scans.get())
    }
}

impl Action for WirelessService {
    const ACTIONS: &'static [ActionDescriptor] = &[ActionDescriptor::new(1, "scan")
        .results(&[FieldDescriptor::new(1, "ssid", ValueKind::String)])];

    async fn invoke(
        &self,
        action: u32,
        _args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        if action != 1 {
            return Err(ActionError::UnknownAction);
        }
        self.scans.set(self.scans.get() + 1);
        out.text(1, "home").map_err(|_| ActionError::Internal)
    }
}

struct Firmware {
    sensors: SensorsService,
    reboot: RebootService,
    wifi: WirelessService,
}

fn firmware() -> Firmware {
    Firmware {
        sensors: SensorsService { pm25: Cell::new(7) },
        reboot: RebootService,
        wifi: WirelessService {
            scans: Cell::new(0),
        },
    }
}

#[test]
fn status_registry_lists_only_status_services() {
    let fw = firmware();
    let statuses = Statuses::<NoopRawMutex, _>::new((&fw.sensors, &fw.wifi));
    assert_eq!(statuses.service_count(), 2);
    assert_eq!(statuses.service_info(0).unwrap().name, "sensors");
    assert_eq!(statuses.service_info(1).unwrap().name, "wifi");
    assert_eq!(statuses.service_info(1).unwrap().id, WirelessService::ID);
    assert_eq!(statuses.service_info(1).unwrap().version, 3);
    assert!(statuses.service_info(2).is_none());
    assert!(statuses.contains(SensorsService::ID));
    assert!(!statuses.contains(RebootService::ID));
}

#[test]
fn status_is_addressed_by_service_id() {
    let fw = firmware();
    let statuses = Statuses::<NoopRawMutex, _>::new((&fw.sensors, &fw.wifi));
    let status = block_on(statuses.status(SensorsService::ID, 10)).unwrap();
    assert_eq!(status.summary.name, "sensors");
    assert_eq!(status.fields.as_slice(), &[(1, Value::U64(7))]);
    assert_eq!(
        block_on(statuses.status(RebootService::ID, 10)),
        Err(StatusError::UnknownService)
    );
    assert!(statuses.schema(WirelessService::ID).is_some());
    assert!(statuses.schema(RebootService::ID).is_none());
}

#[test]
fn actions_are_discovered_per_service() {
    let fw = firmware();
    let actions = (&fw.reboot, &fw.wifi);
    assert_eq!(actions.service_count(), 2);
    assert_eq!(actions.service_identity(0).unwrap().name, "reboot");
    assert_eq!(actions.service_identity(1).unwrap().name, "wifi");
    assert_eq!(actions.service_actions(0)[0].name, "now");
    assert!(actions.contains(WirelessService::ID));
    assert!(!actions.contains(SensorsService::ID));
    assert_eq!(
        actions.action(RebootService::ID, 9),
        Err(ActionError::UnknownAction)
    );
    assert_eq!(
        actions.action(SensorsService::ID, 1),
        Err(ActionError::UnknownService)
    );
    assert!(actions.action(RebootService::ID, 1).unwrap().privileged);
}

#[test]
fn an_action_changes_what_status_reports_without_duplicating_state() {
    let fw = firmware();
    let statuses = Statuses::<NoopRawMutex, _>::new((&fw.sensors, &fw.wifi));
    let actions = (&fw.reboot, &fw.wifi);

    let records = block_on(actions.invoke(WirelessService::ID, 1, &[])).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].as_slice(),
        &[(1, Value::String("home".try_into().unwrap()))]
    );

    let status = block_on(statuses.status(WirelessService::ID, 0)).unwrap();
    assert_eq!(status.fields.as_slice(), &[(1, Value::U64(1))]);
}

#[test]
fn action_errors_are_typed() {
    let fw = firmware();
    let actions = (&fw.reboot, &fw.wifi);
    assert_eq!(
        block_on(actions.invoke(RebootService::ID, 1, &[])),
        Err(ActionError::Rejected("not_now"))
    );
    // The registry checks the arguments before the service runs.
    assert_eq!(
        block_on(actions.invoke(WirelessService::ID, 1, &[(1, Value::U64(1))])),
        Err(ActionError::InvalidArgument)
    );
    assert_eq!(
        block_on(actions.invoke(SensorsService::ID, 1, &[])),
        Err(ActionError::UnknownService)
    );
}

#[test]
fn the_empty_registries_know_nothing() {
    assert_eq!(StatusRegistry::service_count(&()), 0);
    assert_eq!(ActionRegistry::service_count(&()), 0);
    assert_eq!(
        block_on(StatusRegistry::status(&(), 1, 0)),
        Err(StatusError::UnknownService)
    );
    assert_eq!(
        block_on(ActionRegistry::invoke(&(), 1, 1, &[])),
        Err(ActionError::UnknownService)
    );
}

#[test]
fn a_failed_start_reports_failed_and_refuses_actions() {
    let failed = Startup::new(
        WirelessService {
            scans: Cell::new(0),
        },
        true,
    );
    let statuses = Statuses::<NoopRawMutex, _>::new((&failed,));
    let actions = (&failed,);

    let status = block_on(statuses.status(WirelessService::ID, 0)).unwrap();
    assert_eq!(status.summary.state, ServiceState::Failed);
    assert!(!status.summary.ready);
    assert_eq!(status.issues[0].code, ISSUE_STARTUP_FAILED);
    assert!(status.fields.is_empty());
    assert_eq!(
        block_on(actions.invoke(WirelessService::ID, 1, &[])),
        Err(ActionError::Unavailable)
    );
}

#[test]
fn a_started_service_is_forwarded() {
    let started = Startup::new(
        WirelessService {
            scans: Cell::new(0),
        },
        false,
    );
    let statuses = Statuses::<NoopRawMutex, _>::new((&started,));
    assert_eq!(Startup::<WirelessService>::NAME, "wifi");
    let status = block_on(statuses.status(WirelessService::ID, 0)).unwrap();
    assert_eq!(status.summary.state, ServiceState::Running);
}

#[test]
fn the_revision_does_not_follow_service_fields() {
    let fw = firmware();
    let statuses = Statuses::<NoopRawMutex, _>::new((&fw.sensors,));
    let first = block_on(statuses.summary(0, 1)).unwrap();
    fw.sensors.pm25.set(99);
    let second = block_on(statuses.summary(0, 2)).unwrap();
    assert_eq!(first.revision, second.revision);
}

#[test]
fn id_clashes_are_visible_to_the_build() {
    use core::hint::black_box;
    use rohi_service::StatusSet;
    assert!(black_box(
        <(&SensorsService, &WirelessService) as StatusSet<NoopRawMutex>>::IDS_UNIQUE
    ));
    assert!(!black_box(
        <(&WirelessService, &WirelessService) as StatusSet<NoopRawMutex>>::IDS_UNIQUE
    ));
    assert!(black_box(
        <(&RebootService, &WirelessService) as ActionRegistry>::IDS_UNIQUE
    ));
    assert!(!black_box(
        <(&RebootService, &RebootService) as ActionRegistry>::IDS_UNIQUE
    ));
    assert!(black_box(
        <(&RebootService,) as ActionRegistry>::SCHEMAS_VALID
    ));
}
