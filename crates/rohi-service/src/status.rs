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
//! Read-only runtime status of services behind a generic interface.
//!
//! A module implements [`ManagedService`] on top of its live runtime state;
//! no duplicated status struct is needed. A [`Tracked`] wrapper adds the
//! bookkeeping every service needs (status revision, time in the current
//! state) and a tuple of tracked services is a [`ServiceRegistry`].
//!
//! State, health and readiness are independent: a running service can be
//! degraded and not ready, and a disabled service is not an error.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;

use crate::action::enum_ok;
use crate::action::{
    ActionDescriptor, ActionError, ActionOutput, RecordCollector, Records, check_args,
};
use crate::field::{EnumDescriptor, FieldDescriptor};
use crate::schema::{actions_valid, enums_valid, fields_valid, issues_valid, name_ok};
use crate::value::Value;
use crate::{MAX_FIELD_VALUE_LEN, MAX_FIELDS, MAX_ISSUES};

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
    /// Stable numeric code, described in [`ManagedService::ISSUES`].
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

/// A runtime service whose status is exposed through the generic interface.
///
/// The status is a snapshot of the live runtime state, written field by field;
/// there is no stored status struct. Field ids are stable and only ever added.
#[allow(async_fn_in_trait)]
pub trait ManagedService {
    /// Stable numeric id, the address of requests; unique within the
    /// registry. Only ever added, never reused.
    const ID: u32;
    /// Stable public service name, for example `connectivity`.
    const NAME: &'static str;
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
    /// Actions the service offers, see [`crate::action`].
    const ACTIONS: &'static [ActionDescriptor] = &[];

    /// Lifecycle state, health and readiness.
    async fn summary(&self) -> ServiceSummary;

    /// Appends the currently active issues.
    async fn issues(&self, _out: &mut Vec<Issue, MAX_ISSUES>) {}

    /// Writes the service-specific fields that currently have a value.
    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError>;

    /// Runs one of [`ACTIONS`](Self::ACTIONS). The registry has already
    /// checked the arguments against the descriptor; results are written to
    /// `out` as records.
    async fn invoke(
        &self,
        _action: u32,
        _args: &[(u32, Value)],
        _out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        Err(ActionError::UnknownAction)
    }
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

impl<S: ManagedService> Startup<S> {
    /// Wraps `service`; `failed` tells that its runtime did not start.
    pub const fn new(service: S, failed: bool) -> Self {
        Self { service, failed }
    }
}

impl<S: ManagedService> ManagedService for Startup<S> {
    const ID: u32 = S::ID;
    const NAME: &'static str = S::NAME;
    const VERSION: u16 = S::VERSION;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = S::STATUS_SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = S::ENUMS;
    const ISSUES: &'static [IssueDescriptor] = S::ISSUES;
    const ESSENTIAL: bool = S::ESSENTIAL;
    const ACTIONS: &'static [ActionDescriptor] = S::ACTIONS;

    async fn summary(&self) -> ServiceSummary {
        if self.failed {
            return ServiceSummary::new(ServiceState::Failed, ServiceHealth::Error, false);
        }
        self.service.summary().await
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if self.failed {
            let _ = out.push(Issue {
                severity: IssueSeverity::Error,
                code: ISSUE_STARTUP_FAILED,
                age_ms: None,
            });
        } else {
            self.service.issues(out).await;
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        if self.failed {
            return Ok(());
        }
        self.service.status(out).await
    }

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        if self.failed {
            return Err(ActionError::Unavailable);
        }
        self.service.invoke(action, args, out).await
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
    /// [`ManagedService::ESSENTIAL`].
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
struct Track {
    revision: u32,
    // What the revision follows: state, health, readiness and issues.
    hash: u32,
    // Only the active issues: their age restarts when the set changes, not
    // when something else about the service does.
    issues_hash: u32,
    state: ServiceState,
    state_since_ms: u64,
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

/// A [`ManagedService`] with the bookkeeping the registry needs: the status
/// revision and the time since the last state change.
///
/// Revisions and ages are derived from what the service reports, so a service
/// does not have to cooperate. Observation happens when a client asks, with
/// the device uptime given by the caller; poll the compact summary regularly
/// for precise ages.
pub struct Tracked<S, M: RawMutex> {
    service: S,
    track: Mutex<M, Cell<Option<Track>>>,
}

impl<S: ManagedService, M: RawMutex> Tracked<S, M> {
    /// Tracks `service`.
    pub const fn new(service: S) -> Self {
        const {
            assert!(name_ok(S::NAME), "service name is empty or too long");
            assert!(
                S::STATUS_SCHEMA.len() <= MAX_FIELDS,
                "too many status fields"
            );
            assert!(enums_valid(S::ENUMS), "invalid service enums");
            assert!(
                fields_valid(S::STATUS_SCHEMA, S::ENUMS),
                "invalid status fields: ids and names must be unique and fit"
            );
            assert!(issues_valid(S::ISSUES), "invalid service issues");
            assert!(actions_valid(S::ACTIONS), "invalid service actions");
        };
        Self {
            service,
            track: Mutex::new(Cell::new(None)),
        }
    }

    /// The tracked service.
    pub const fn service(&self) -> &S {
        &self.service
    }

    fn info() -> ServiceInfo {
        ServiceInfo {
            id: S::ID,
            name: S::NAME,
            version: S::VERSION as u32,
        }
    }

    async fn observe(&self, now_ms: u64) -> (ServiceStatusSummary, Vec<Issue, MAX_ISSUES>) {
        let summary = self.service.summary().await;
        let mut issues = Vec::new();
        self.service.issues(&mut issues).await;
        let issues_hash = issues_hash(&issues);
        let hash = observable_hash(&summary, issues_hash);

        let track = self.track.lock(|cell| {
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

        for issue in &mut issues {
            if issue.age_ms.is_none() {
                issue.age_ms = Some(now_ms.saturating_sub(track.issues_since_ms));
            }
        }
        (
            ServiceStatusSummary {
                id: S::ID,
                name: S::NAME,
                version: S::VERSION as u32,
                state: summary.state,
                health: summary.health,
                ready: summary.ready,
                revision: track.revision,
                state_age_ms: now_ms.saturating_sub(track.state_since_ms),
                essential: S::ESSENTIAL,
            },
            issues,
        )
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

/// The generic operations of one tracked service.
#[allow(async_fn_in_trait)]
pub trait ServiceOps {
    /// Id, name and version.
    fn info(&self) -> ServiceInfo;
    /// Status schema.
    fn schema(&self) -> ServiceSchema;
    /// Compact status.
    async fn summary(&self, now_ms: u64) -> ServiceStatusSummary;
    /// Detailed status.
    async fn status(&self, now_ms: u64) -> ServiceStatus;
    /// Action metadata.
    fn actions(&self) -> &'static [ActionDescriptor];
    /// Runs an action.
    async fn invoke(&self, action: u32, args: &[(u32, Value)]) -> Result<Records, ActionError>;
}

impl<S: ManagedService, M: RawMutex> ServiceOps for Tracked<S, M> {
    fn info(&self) -> ServiceInfo {
        Self::info()
    }

    fn schema(&self) -> ServiceSchema {
        ServiceSchema {
            info: Self::info(),
            fields: S::STATUS_SCHEMA,
            enums: S::ENUMS,
            issues: S::ISSUES,
        }
    }

    async fn summary(&self, now_ms: u64) -> ServiceStatusSummary {
        self.observe(now_ms).await.0
    }

    async fn status(&self, now_ms: u64) -> ServiceStatus {
        let (summary, issues) = self.observe(now_ms).await;
        let mut collector = Collector {
            schema: S::STATUS_SCHEMA,
            enums: S::ENUMS,
            fields: Vec::new(),
        };
        // A field the service fails to write is absent; the rest is still
        // useful.
        let _ = self.service.status(&mut collector).await;
        ServiceStatus {
            summary,
            fields: collector.fields,
            issues,
        }
    }

    fn actions(&self) -> &'static [ActionDescriptor] {
        S::ACTIONS
    }

    async fn invoke(&self, action: u32, args: &[(u32, Value)]) -> Result<Records, ActionError> {
        let descriptor = S::ACTIONS
            .iter()
            .find(|descriptor| descriptor.id == action)
            .ok_or(ActionError::UnknownAction)?;
        check_args(descriptor, args)?;
        let mut out = RecordCollector::new(descriptor);
        self.service.invoke(action, args, &mut out).await?;
        Ok(out.records)
    }
}

/// All services compiled into the firmware.
///
/// Implemented for tuples of [`Tracked`] services (up to
/// [`MAX_REGISTRY_ENTRIES`](crate::MAX_REGISTRY_ENTRIES)) and for `()`;
/// adding a service means adding it to the tuple, nothing else changes.
#[allow(async_fn_in_trait)]
pub trait ServiceRegistry {
    /// Number of services.
    fn service_count(&self) -> usize;
    /// Id, name and version of the service at `index`.
    fn service_info(&self, index: usize) -> Option<ServiceInfo>;
    /// Status schema of the service with the given id.
    fn schema(&self, service: u32) -> Option<ServiceSchema>;
    /// Compact status of the service at `index`.
    async fn summary(&self, index: usize, now_ms: u64) -> Option<ServiceStatusSummary>;
    /// Detailed status of the service with the given id.
    async fn status(&self, service: u32, now_ms: u64) -> Result<ServiceStatus, StatusError>;
    /// Actions of the service at `index`.
    fn service_actions(&self, index: usize) -> &'static [ActionDescriptor];
    /// Descriptor of an action of the service with the given id.
    fn action(&self, service: u32, action: u32) -> Result<&'static ActionDescriptor, ActionError>;
    /// Runs an action of the service with the given id.
    async fn invoke(
        &self,
        service: u32,
        action: u32,
        args: &[(u32, Value)],
    ) -> Result<Records, ActionError>;

    /// Whether no two services share an id. Firmware checks it once at
    /// start, a clash would make requests reach the wrong service.
    fn ids_are_unique(&self) -> bool {
        (0..self.service_count()).all(|index| {
            let id = self.service_info(index).map(|info| info.id);
            (0..index).all(|other| self.service_info(other).map(|info| info.id) != id)
        })
    }
}

impl ServiceRegistry for () {
    fn service_count(&self) -> usize {
        0
    }

    fn service_info(&self, _: usize) -> Option<ServiceInfo> {
        None
    }

    fn schema(&self, _: u32) -> Option<ServiceSchema> {
        None
    }

    async fn summary(&self, _: usize, _: u64) -> Option<ServiceStatusSummary> {
        None
    }

    async fn status(&self, _: u32, _: u64) -> Result<ServiceStatus, StatusError> {
        Err(StatusError::UnknownService)
    }

    fn service_actions(&self, _: usize) -> &'static [ActionDescriptor] {
        &[]
    }

    fn action(&self, _: u32, _: u32) -> Result<&'static ActionDescriptor, ActionError> {
        Err(ActionError::UnknownService)
    }

    async fn invoke(&self, _: u32, _: u32, _: &[(u32, Value)]) -> Result<Records, ActionError> {
        Err(ActionError::UnknownService)
    }
}

macro_rules! tuple_service_registry {
    ($count:expr; $($name:ident . $index:tt),+) => {
        impl<$($name: ServiceOps),+> ServiceRegistry for ($($name,)+) {
            fn service_count(&self) -> usize {
                $count
            }

            fn service_info(&self, index: usize) -> Option<ServiceInfo> {
                match index {
                    $($index => Some(self.$index.info()),)+
                    _ => None,
                }
            }

            fn schema(&self, service: u32) -> Option<ServiceSchema> {
                $(if self.$index.info().id == service {
                    return Some(self.$index.schema());
                })+
                None
            }

            async fn summary(&self, index: usize, now_ms: u64) -> Option<ServiceStatusSummary> {
                match index {
                    $($index => Some(self.$index.summary(now_ms).await),)+
                    _ => None,
                }
            }

            async fn status(&self, service: u32, now_ms: u64) -> Result<ServiceStatus, StatusError> {
                $(if self.$index.info().id == service {
                    return Ok(self.$index.status(now_ms).await);
                })+
                Err(StatusError::UnknownService)
            }

            fn service_actions(&self, index: usize) -> &'static [ActionDescriptor] {
                match index {
                    $($index => self.$index.actions(),)+
                    _ => &[],
                }
            }

            fn action(
                &self,
                service: u32,
                action: u32,
            ) -> Result<&'static ActionDescriptor, ActionError> {
                $(if self.$index.info().id == service {
                    return self
                        .$index
                        .actions()
                        .iter()
                        .find(|descriptor| descriptor.id == action)
                        .ok_or(ActionError::UnknownAction);
                })+
                Err(ActionError::UnknownService)
            }

            async fn invoke(
                &self,
                service: u32,
                action: u32,
                args: &[(u32, Value)],
            ) -> Result<Records, ActionError> {
                $(if self.$index.info().id == service {
                    return self.$index.invoke(action, args).await;
                })+
                Err(ActionError::UnknownService)
            }
        }
    };
}

tuple_service_registry!(1; A.0);
tuple_service_registry!(2; A.0, B.1);
tuple_service_registry!(3; A.0, B.1, C.2);
tuple_service_registry!(4; A.0, B.1, C.2, D.3);
tuple_service_registry!(5; A.0, B.1, C.2, D.3, E.4);
tuple_service_registry!(6; A.0, B.1, C.2, D.3, E.4, F.5);
tuple_service_registry!(7; A.0, B.1, C.2, D.3, E.4, F.5, G.6);
tuple_service_registry!(8; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7);
tuple_service_registry!(9; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8);
tuple_service_registry!(10; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9);
tuple_service_registry!(11; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9, K.10);
tuple_service_registry!(12; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9, K.10, L.11);

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
