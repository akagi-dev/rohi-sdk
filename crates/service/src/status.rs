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
//! Read-only runtime status of services behind a generic interface.
//!
//! A service implements [`Status`] on top of its live runtime state; no
//! duplicated status struct is needed. The [`StatusRegistry`](crate::StatusRegistry)
//! adds the bookkeeping every service needs (status revision, time in the
//! current state) without the service cooperating.
//!
//! State, health and readiness are independent: a running service can be
//! degraded and not ready, and a disabled service is not an error.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;

use crate::action::enum_ok;
use crate::action::{Action, ActionDescriptor, ActionError, ActionOutput};
use crate::field::{EnumDescriptor, FieldDescriptor};
use crate::service::Service;
use crate::{MAX_FIELD_VALUE_LEN, MAX_FIELDS, MAX_ISSUES};
use rohi_config::Value;

/// Lifecycle state of a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    /// Turned off by configuration; not an error.
    Disabled,
    /// Not running.
    Stopped,
    /// Starting up.
    Starting,
    /// Running.
    Running,
    /// Shutting down.
    Stopping,
    /// Crashed and not recovering by itself.
    Failed,
}

/// Operational quality of a service, independent of its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceHealth {
    /// Not known, for example while starting or when disabled.
    Unknown,
    /// Fully operational.
    Ok,
    /// Working with reduced capability.
    Degraded,
    /// Not working.
    Error,
}

/// Health of the whole device, decided by the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceHealth {
    /// Everything essential works.
    Ok,
    /// Something essential is impaired.
    Degraded,
    /// Something essential does not work.
    Error,
}

/// Severity of an active issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueSeverity {
    /// Informational.
    Info,
    /// Needs attention but works.
    Warning,
    /// A failure.
    Error,
    /// A failure that endangers the device role.
    Critical,
}

/// Codes below this value are reserved for the SDK; services use codes from
/// here on.
pub const FIRST_SERVICE_ISSUE: u32 = 256;

/// Issue code of a service whose runtime failed to start, see [`Startup`].
pub const ISSUE_STARTUP_FAILED: u32 = 1;

/// The name of an issue, listed once in the schema of its service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IssueDescriptor {
    /// Stable numeric code.
    pub code: u32,
    /// Stable machine-readable name, for example `connection_timeout`.
    pub name: &'static str,
}

impl IssueDescriptor {
    /// An issue description.
    pub const fn new(code: u32, name: &'static str) -> Self {
        Self { code, name }
    }
}

/// A currently active problem of a service. History belongs to diagnostics.
/// What the issue means is told by the [`IssueDescriptor`] of the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Issue {
    /// How serious it is.
    pub severity: IssueSeverity,
    /// Stable numeric code, described in [`Status::ISSUES`].
    pub code: u32,
    /// How long the issue has been active, if the service knows. Otherwise
    /// the registry reports the time since the set of active issues last
    /// changed.
    pub age_ms: Option<u64>,
}

/// What a service reports about itself in one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceSummary {
    /// Lifecycle state.
    pub state: ServiceState,
    /// Operational quality.
    pub health: ServiceHealth,
    /// Whether the service can currently perform its intended role.
    pub ready: bool,
}

impl ServiceSummary {
    /// A summary of the given state, health and readiness.
    pub const fn new(state: ServiceState, health: ServiceHealth, ready: bool) -> Self {
        Self {
            state,
            health,
            ready,
        }
    }
}

/// Why a status field could not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// The schema has no field with this id.
    UnknownField,
    /// The value does not match the kind of the field.
    WrongKind,
    /// The bytes do not fit into a value.
    TooLong,
    /// More fields than [`MAX_FIELDS`].
    Full,
}

/// Receives the field values of a service status, checked against the
/// service schema.
pub trait StatusWriter {
    /// Writes a value.
    fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError>;

    /// Writes a boolean.
    fn bool(&mut self, id: u32, value: bool) -> Result<(), WriteError> {
        self.put(id, Value::Bool(value))
    }

    /// Writes a signed integer.
    fn i64(&mut self, id: u32, value: i64) -> Result<(), WriteError> {
        self.put(id, Value::I64(value))
    }

    /// Writes an unsigned integer.
    fn u64(&mut self, id: u32, value: u64) -> Result<(), WriteError> {
        self.put(id, Value::U64(value))
    }

    /// Writes a float.
    fn f32(&mut self, id: u32, value: f32) -> Result<(), WriteError> {
        self.put(id, Value::F32(value))
    }

    /// Writes text, at most [`MAX_FIELD_VALUE_LEN`] bytes: a longer value is
    /// [`WriteError::TooLong`], never cut. Shortening for display is up to the
    /// client.
    fn text(&mut self, id: u32, value: &str) -> Result<(), WriteError> {
        if value.len() > MAX_FIELD_VALUE_LEN {
            return Err(WriteError::TooLong);
        }
        self.put(id, Value::text(value).ok_or(WriteError::TooLong)?)
    }

    /// Writes bytes, at most [`MAX_FIELD_VALUE_LEN`].
    fn bytes(&mut self, id: u32, value: &[u8]) -> Result<(), WriteError> {
        if value.len() > MAX_FIELD_VALUE_LEN {
            return Err(WriteError::TooLong);
        }
        self.put(id, Value::bytes(value).ok_or(WriteError::TooLong)?)
    }

    /// Writes a variant of an enum field.
    fn variant(&mut self, id: u32, value: u32) -> Result<(), WriteError> {
        self.put(id, Value::Enum(value))
    }
}

pub(crate) struct Collector {
    pub(crate) schema: &'static [FieldDescriptor],
    pub(crate) enums: &'static [EnumDescriptor],
    pub(crate) fields: Vec<(u32, Value), MAX_FIELDS>,
}

impl StatusWriter for Collector {
    fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
        let field = self
            .schema
            .iter()
            .find(|field| field.id == id)
            .ok_or(WriteError::UnknownField)?;
        if field.kind != value.kind() {
            return Err(WriteError::WrongKind);
        }
        if !enum_ok(self.enums, field, &value) {
            return Err(WriteError::WrongKind);
        }
        self.fields.push((id, value)).map_err(|_| WriteError::Full)
    }
}

/// The status capability of a [`Service`]: a read-only snapshot of its live
/// runtime state.
///
/// The status is written field by field; there is no stored status struct.
/// Field ids are stable and only ever added. Shared bookkeeping (state age,
/// revision, device health) is done by the status registry, not by the
/// service; the service only reports what it knows.
///
/// A service implements `Status` only if it has runtime state to show, and
/// independently of [`Action`](crate::Action).
#[allow(async_fn_in_trait)]
pub trait Status: Service {
    /// Version of the status schema.
    const VERSION: u16;
    /// Metadata of the service-specific status fields, at most
    /// [`MAX_FIELDS`] entries.
    const STATUS_SCHEMA: &'static [FieldDescriptor];
    /// The enums the status fields refer to, see [`EnumDescriptor`].
    const ENUMS: &'static [EnumDescriptor] = &[];
    /// The names of the issues the service reports, at most
    /// [`MAX_ISSUE_DESCRIPTORS`].
    const ISSUES: &'static [IssueDescriptor] = &[];
    /// Whether the device is impaired when this service is not working.
    /// Optional services degrade the device health at most.
    const ESSENTIAL: bool = true;

    /// Lifecycle state, health and readiness.
    async fn summary(&self) -> ServiceSummary;

    /// Appends the currently active issues.
    async fn issues(&self, _out: &mut Vec<Issue, MAX_ISSUES>) {}

    /// Writes the service-specific fields that currently have a value.
    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError>;
}

/// A service whose runtime may have failed to start.
///
/// The runtime handles of a service that never started stay untouched and
/// would read as a disabled service, which the default health policy ignores.
/// A failed start is reported as a failed service with the issue
/// [`ISSUE_STARTUP_FAILED`] instead; everything else is forwarded.
pub struct Startup<S> {
    service: S,
    failed: bool,
}

impl<S> Startup<S> {
    /// Wraps `service`; `failed` tells that its runtime did not start.
    pub const fn new(service: S, failed: bool) -> Self {
        Self { service, failed }
    }
}

impl<S: Service> Service for Startup<S> {
    const ID: u32 = S::ID;
    const NAME: &'static str = S::NAME;
}

impl<S: Status> Status for Startup<S> {
    const VERSION: u16 = S::VERSION;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = S::STATUS_SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = S::ENUMS;
    const ISSUES: &'static [IssueDescriptor] = S::ISSUES;
    const ESSENTIAL: bool = S::ESSENTIAL;

    async fn summary(&self) -> ServiceSummary {
        if self.failed {
            return ServiceSummary::new(ServiceState::Failed, ServiceHealth::Error, false);
        }
        Status::summary(&self.service).await
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if self.failed {
            let _ = out.push(Issue {
                severity: IssueSeverity::Error,
                code: ISSUE_STARTUP_FAILED,
                age_ms: None,
            });
        } else {
            Status::issues(&self.service, out).await;
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        if self.failed {
            return Ok(());
        }
        Status::status(&self.service, out).await
    }
}

impl<S: Action> Action for Startup<S> {
    const ACTIONS: &'static [ActionDescriptor] = S::ACTIONS;

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        if self.failed {
            return Err(ActionError::Unavailable);
        }
        Action::invoke(&self.service, action, args, out).await
    }
}

/// Id, name and version of a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceInfo {
    /// Stable numeric id.
    pub id: u32,
    /// Stable public name.
    pub name: &'static str,
    /// Version of the status schema.
    pub version: u32,
}

/// Compact status of one service, as listed for the whole device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceStatusSummary {
    /// Service id.
    pub id: u32,
    /// Service name.
    pub name: &'static str,
    /// Version of the status schema.
    pub version: u32,
    /// Lifecycle state.
    pub state: ServiceState,
    /// Operational quality.
    pub health: ServiceHealth,
    /// Whether the service can currently perform its intended role.
    pub ready: bool,
    /// Incremented whenever the state, health, readiness or the active
    /// issues change.
    pub revision: u32,
    /// Time spent in the current lifecycle state.
    pub state_age_ms: u64,
    /// Whether the device depends on the service, see
    /// [`Status::ESSENTIAL`].
    pub essential: bool,
}

/// Detailed status of one service.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceStatus {
    /// Compact status.
    pub summary: ServiceStatusSummary,
    /// Service-specific values, by field id.
    pub fields: Vec<(u32, Value), MAX_FIELDS>,
    /// Active issues, with their age filled in.
    pub issues: Vec<Issue, MAX_ISSUES>,
}

/// Failure of a status query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusError {
    /// No service with this name.
    UnknownService,
}

#[derive(Clone, Copy)]
pub(crate) struct Track {
    pub(crate) revision: u32,
    // What the revision follows: state, health, readiness and issues.
    hash: u32,
    // Only the active issues: their age restarts when the set changes, not
    // when something else about the service does.
    issues_hash: u32,
    state: ServiceState,
    pub(crate) state_since_ms: u64,
    issues_since_ms: u64,
}

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

fn fnv(mut hash: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        hash = (hash ^ u32::from(*byte)).wrapping_mul(FNV_PRIME);
    }
    hash
}

fn issues_hash(issues: &[Issue]) -> u32 {
    let mut hash = FNV_OFFSET;
    for issue in issues {
        hash = fnv(hash, &[issue.severity as u8]);
        hash = fnv(hash, &issue.code.to_le_bytes());
    }
    hash
}

fn observable_hash(summary: &ServiceSummary, issues_hash: u32) -> u32 {
    let mut hash = fnv(FNV_OFFSET, &[summary.state as u8, summary.health as u8]);
    hash = fnv(hash, &[u8::from(summary.ready)]);
    fnv(hash, &issues_hash.to_le_bytes())
}

pub(crate) type TrackCell<M> = Mutex<M, Cell<Option<Track>>>;

/// Updates the track of a service with what it reported just now and fills
/// in the age of the issues that have none. `hash` and the revision follow
/// state, health, readiness and the active issues only, never the
/// service-specific status fields.
pub(crate) fn track_observation<M: RawMutex>(
    cell: &TrackCell<M>,
    summary: &ServiceSummary,
    issues: &mut Vec<Issue, MAX_ISSUES>,
    now_ms: u64,
) -> Track {
    let issues_hash = issues_hash(issues);
    let hash = observable_hash(summary, issues_hash);
    let track = cell.lock(|cell| {
        let mut track = cell.get().unwrap_or(Track {
            revision: 1,
            hash,
            issues_hash,
            state: summary.state,
            state_since_ms: now_ms,
            issues_since_ms: now_ms,
        });
        if track.state != summary.state {
            track.state = summary.state;
            track.state_since_ms = now_ms;
        }
        if track.hash != hash {
            track.hash = hash;
            track.revision = track.revision.wrapping_add(1);
        }
        if track.issues_hash != issues_hash {
            track.issues_hash = issues_hash;
            track.issues_since_ms = now_ms;
        }
        cell.set(Some(track));
        track
    });
    for issue in issues.iter_mut() {
        if issue.age_ms.is_none() {
            issue.age_ms = Some(now_ms.saturating_sub(track.issues_since_ms));
        }
    }
    track
}

/// The status schema of a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceSchema {
    /// Id, name and version.
    pub info: ServiceInfo,
    /// Status field metadata.
    pub fields: &'static [FieldDescriptor],
    /// The enums the fields refer to.
    pub enums: &'static [EnumDescriptor],
    /// The names of the issues the service reports.
    pub issues: &'static [IssueDescriptor],
}

/// The default device health policy.
///
/// Disabled services do not count. A failed service or one with `Error`
/// health is an error when the device depends on it and a degradation
/// otherwise; a degraded service, or a running one that is not ready,
/// degrades the device. Firmware with other redundancy rules (for example a
/// cellular modem that is irrelevant while Ethernet works) supplies its own
/// policy.
pub fn default_device_health(services: &[ServiceStatusSummary]) -> DeviceHealth {
    services
        .iter()
        .map(|service| {
            if service.state == ServiceState::Disabled {
                return DeviceHealth::Ok;
            }
            if service.state == ServiceState::Failed || service.health == ServiceHealth::Error {
                return if service.essential {
                    DeviceHealth::Error
                } else {
                    DeviceHealth::Degraded
                };
            }
            if service.health == ServiceHealth::Degraded
                || (service.state == ServiceState::Running && !service.ready)
            {
                return DeviceHealth::Degraded;
            }
            DeviceHealth::Ok
        })
        .max()
        .unwrap_or(DeviceHealth::Ok)
}
