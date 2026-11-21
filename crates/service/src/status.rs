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
//! A service implements [`Status`] on top of its **live** runtime state; there
//! is no stored status struct to duplicate. The [`StatusRegistry`](crate::StatusRegistry)
//! adds the bookkeeping every service needs (the status revision) without the
//! service cooperating, see [`Tracker`](crate::Tracker).
//!
//! ```
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use rohi_service::{
//!     FieldDescriptor, Service, ServiceHealth, ServiceSummary, Status, StatusRegistry,
//!     StatusWriter, Statuses, Unit, ValueKind, WriteError,
//! };
//!
//! #[derive(Service)]
//! struct WifiService {
//!     rssi: i64,
//! }
//!
//! impl Status for WifiService {
//!     const VERSION: u16 = 1;
//!     const STATUS_SCHEMA: &'static [FieldDescriptor] =
//!         &[FieldDescriptor::new(1, "rssi", ValueKind::I64).unit(Unit::Dbm)];
//!
//!     async fn summary(&self) -> ServiceSummary {
//!         ServiceSummary::new(ServiceHealth::Ok)
//!     }
//!
//!     async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
//!         out.i64(1, self.rssi)
//!     }
//! }
//!
//! let wifi = WifiService { rssi: -61 };
//! let statuses = Statuses::<NoopRawMutex, _>::new((&wifi,));
//! let status = block_on(statuses.status(WifiService::ID, 1_000)).unwrap();
//! assert_eq!(status.summary.health, ServiceHealth::Ok);
//! assert_eq!(status.fields.len(), 1);
//! ```
//!
//! The service reports a [`ServiceSummary`] ([`Status::summary`]), its active
//! [`Issue`]s ([`Status::issues`]) and the service-specific fields
//! ([`Status::status`], written through a [`StatusWriter`] that checks them
//! against [`Status::STATUS_SCHEMA`]; a string or bytes value is at most
//! [`MAX_FIELD_VALUE_LEN`] bytes and never cut). Field ids are stable and only
//! ever added.
//!
//! # Health
//!
//! [`ServiceHealth`] is the one common operational indicator of a service:
//! `unknown`, `ok`, `degraded` or `error`. There is no separate lifecycle
//! state or readiness flag; a service that needs to tell more (a link state,
//! a sensor state) does so in its own status fields.
//!
//! Examples: a connecting Wi-Fi is `unknown`; a telemetry service that cannot
//! reach its backend is `degraded`; an optional GPS turned off is `unknown`,
//! which is **not an error**.
//!
//! # Issues
//!
//! [`Issue`]s are the currently active problems, each with a stable numeric
//! `code`, an [`IssueSeverity`] (`info`, `warning`, `error`, `critical`) and an
//! age. The names of the codes are listed once in [`Status::ISSUES`] and sent
//! with the schema, never with every status; there is no message text. Codes
//! below [`FIRST_SERVICE_ISSUE`] (256) are reserved for the SDK
//! ([`Startup`] reports [`ISSUE_STARTUP_FAILED`]); services start at
//! `FIRST_SERVICE_ISSUE`. History does not belong here: it is diagnostics.
//!
//! # Revision
//!
//! The registry derives a **status revision** that changes when the health or
//! the active issues change (values that vary all the time, such as counters
//! or signal strength, deliberately do not bump it). Revisions and ages are
//! derived from what the service reports when a client asks; an issue that
//! carries no age is aged by the time since the set of active issues last
//! changed.
//!
//! # Device health
//!
//! **Device health** is decided by the firmware, never by the client.
//! `ControlBackend::device_health` of
//! [`rohi-control`](../../rohi_control/index.html) defaults to
//! [`default_device_health`]: an `error` service is an error for the device
//! when it is [`ESSENTIAL`](Status::ESSENTIAL) (the default) and a degradation
//! otherwise; a `degraded` service degrades the device; `unknown` and `ok`
//! services do not count. Firmware with other redundancy rules overrides it.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;

use crate::action::enum_ok;
use crate::action::{Action, ActionDescriptor, ActionError, ActionOutput, StreamChunk};
use crate::field::{EnumDescriptor, FieldDescriptor};
use crate::service::Service;
use crate::{MAX_FIELD_VALUE_LEN, MAX_FIELDS, MAX_ISSUES};
use rohi_config::Value;

// Operational quality of a service: the one common status indicator.
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
    /// Operational quality.
    pub health: ServiceHealth,
}

impl ServiceSummary {
    /// A summary of the given health.
    pub const fn new(health: ServiceHealth) -> Self {
        Self { health }
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

/// A service wrote a field that the schema does not accept. Only `debug`: it can
/// repeat on every poll.
#[cold]
#[inline(never)]
pub(crate) fn log_rejected_field(area: &'static str, id: u32, error: WriteError) {
    log::debug!(
        target: "service",
        "Rejected {} field: {:?} (id={})",
        area,
        error,
        id
    );
}

impl StatusWriter for Collector {
    fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
        self.store(id, value)
            .inspect_err(|error| log_rejected_field("status", id, *error))
    }
}

impl Collector {
    fn store(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
        let field = self
            .schema
            .iter()
            .find(|field| field.id == id)
            .ok_or(WriteError::UnknownField)?;
        if !field.admits(&value) {
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
/// Field ids are stable and only ever added. Shared bookkeeping (revision,
/// device health) is done by the status registry, not by the
/// service; the service only reports what it knows.
///
/// A service implements `Status` only if it has runtime state to show, and
/// independently of [`Action`].
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
    /// [`MAX_ISSUE_DESCRIPTORS`](crate::MAX_ISSUE_DESCRIPTORS).
    const ISSUES: &'static [IssueDescriptor] = &[];
    /// Whether the device is impaired when this service is not working.
    /// Optional services degrade the device health at most.
    const ESSENTIAL: bool = true;

    /// Health.
    async fn summary(&self) -> ServiceSummary;

    /// Appends the currently active issues.
    async fn issues(&self, _out: &mut Vec<Issue, MAX_ISSUES>) {}

    /// Writes the service-specific fields that currently have a value.
    ///
    /// # Errors
    ///
    /// The [`WriteError`] of the first rejected write: an id outside
    /// [`STATUS_SCHEMA`](Self::STATUS_SCHEMA), a value of the wrong kind or an
    /// enum variant that is not listed, a string or bytes value above
    /// [`MAX_FIELD_VALUE_LEN`], or more than [`MAX_FIELDS`] fields. The
    /// registry ignores the error and reports the fields written until then,
    /// so a service that stops at the first error (`?`) omits the rest.
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
            return ServiceSummary::new(ServiceHealth::Error);
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

    // What a started service holds is read and released through the wrapper too.
    async fn read_records(
        &self,
        action: u32,
        snapshot: u32,
        from: u32,
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        Action::read_records(&self.service, action, snapshot, from, out).await
    }

    async fn close_records(&self, snapshot: u32) -> Result<(), ActionError> {
        Action::close_records(&self.service, snapshot).await
    }

    async fn read_stream(
        &self,
        stream: u32,
        file: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<StreamChunk, ActionError> {
        Action::read_stream(&self.service, stream, file, offset, buf).await
    }

    async fn close_stream(&self, stream: u32) -> Result<(), ActionError> {
        Action::close_stream(&self.service, stream).await
    }

    fn link_lost(&self) {
        Action::link_lost(&self.service);
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
    /// Operational quality.
    pub health: ServiceHealth,
    /// Incremented whenever the health or the active issues change.
    pub revision: u32,
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
    /// No service with this id.
    UnknownService,
}

#[derive(Clone, Copy)]
pub(crate) struct Track {
    pub(crate) revision: u32,
    // What the revision follows: health and issues.
    hash: u32,
    // Only the active issues: their age restarts when the set changes, not
    // when something else about the service does.
    issues_hash: u32,
    health: ServiceHealth,
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
    let hash = fnv(FNV_OFFSET, &[summary.health as u8]);
    fnv(hash, &issues_hash.to_le_bytes())
}

pub(crate) type TrackCell<M> = Mutex<M, Cell<Option<Track>>>;

/// Updates the track of a service with what it reported just now and fills
/// in the age of the issues that have none. `hash` and the revision follow
/// the health and the active issues only, never the
/// service-specific status fields.
pub(crate) fn track_observation<M: RawMutex>(
    name: &'static str,
    cell: &TrackCell<M>,
    summary: &ServiceSummary,
    issues: &mut Vec<Issue, MAX_ISSUES>,
    now_ms: u64,
) -> Track {
    let issues_hash = issues_hash(issues);
    let hash = observable_hash(summary, issues_hash);
    let (track, previous) = cell.lock(|cell| {
        let previous = cell.get();
        let mut track = previous.unwrap_or(Track {
            revision: 1,
            hash,
            issues_hash,
            health: summary.health,
            issues_since_ms: now_ms,
        });
        if track.hash != hash {
            track.hash = hash;
            track.health = summary.health;
            track.revision = track.revision.wrapping_add(1);
        }
        if track.issues_hash != issues_hash {
            track.issues_hash = issues_hash;
            track.issues_since_ms = now_ms;
        }
        cell.set(Some(track));
        (track, previous)
    });
    log_observation(name, summary, issues.len(), previous, &track);
    for issue in issues.iter_mut() {
        if issue.age_ms.is_none() {
            issue.age_ms = Some(now_ms.saturating_sub(track.issues_since_ms));
        }
    }
    track
}

fn log_observation(
    name: &'static str,
    summary: &ServiceSummary,
    issues: usize,
    previous: Option<Track>,
    track: &Track,
) {
    if previous.is_some_and(|previous| previous.revision == track.revision) {
        return;
    }
    log_status_change(
        name,
        summary,
        issues,
        previous.map(|previous| previous.health),
        track,
    );
}

#[cold]
#[inline(never)]
fn log_status_change(
    name: &'static str,
    summary: &ServiceSummary,
    issues: usize,
    previous: Option<ServiceHealth>,
    track: &Track,
) {
    if summary.health == ServiceHealth::Error && previous != Some(ServiceHealth::Error) {
        log::warn!(
            target: "service",
            "Service failed (service={} issues={} revision={})",
            name,
            issues,
            track.revision
        );
        return;
    }
    match previous {
        Some(previous) => log::debug!(
            target: "service",
            "Health changed: {:?} -> {:?} (service={} issues={} revision={})",
            previous,
            summary.health,
            name,
            issues,
            track.revision
        ),
        None => log::debug!(
            target: "service",
            "Service first seen: {:?} (service={} issues={})",
            summary.health,
            name,
            issues
        ),
    }
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
/// A service with `Error` health is an error when the device depends on it
/// and a degradation otherwise; a degraded service degrades the device.
/// `Unknown` and `Ok` services do not count. Firmware with other redundancy rules (for example a
/// cellular modem that is irrelevant while Ethernet works) supplies its own
/// policy.
pub fn default_device_health(services: &[ServiceStatusSummary]) -> DeviceHealth {
    services
        .iter()
        .map(|service| match service.health {
            ServiceHealth::Error if service.essential => DeviceHealth::Error,
            ServiceHealth::Error | ServiceHealth::Degraded => DeviceHealth::Degraded,
            ServiceHealth::Unknown | ServiceHealth::Ok => DeviceHealth::Ok,
        })
        .max()
        .unwrap_or(DeviceHealth::Ok)
}
