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

use crate::action::{
    ActionDescriptor, ActionError, ActionOutput, RecordCollector, Records, check_args,
};
use crate::field::FieldDescriptor;
use crate::value::{Value, ValueKind};
use crate::{MAX_FIELDS, MAX_ISSUES, MAX_VALUE_LEN};

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

/// A currently active problem of a service. History belongs to diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Issue {
    /// How serious it is.
    pub severity: IssueSeverity,
    /// Stable machine-readable identifier, for example `connection_timeout`.
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: &'static str,
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
    /// Stable machine-readable short reason; defaults to the code of the
    /// first active issue.
    pub reason: Option<&'static str>,
}

impl ServiceSummary {
    /// A summary without a reason.
    pub const fn new(state: ServiceState, health: ServiceHealth, ready: bool) -> Self {
        Self {
            state,
            health,
            ready,
            reason: None,
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
    fn f64(&mut self, id: u32, value: f64) -> Result<(), WriteError> {
        self.put(id, Value::F64(value))
    }

    /// Writes text, truncated at a character boundary if it exceeds
    /// [`MAX_VALUE_LEN`].
    fn text(&mut self, id: u32, value: &str) -> Result<(), WriteError> {
        let mut end = value.len().min(MAX_VALUE_LEN);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        let text = Value::text(&value[..end]).ok_or(WriteError::TooLong)?;
        self.put(id, text)
    }

    /// Writes bytes.
    fn bytes(&mut self, id: u32, value: &[u8]) -> Result<(), WriteError> {
        self.put(id, Value::bytes(value).ok_or(WriteError::TooLong)?)
    }

    /// Writes a variant of an enum field.
    fn variant(&mut self, id: u32, value: &str) -> Result<(), WriteError> {
        self.put(id, Value::variant(value).ok_or(WriteError::TooLong)?)
    }
}

pub(crate) struct Collector {
    pub(crate) schema: &'static [FieldDescriptor],
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
        if field.kind == ValueKind::Enum
            && value
                .as_str()
                .is_none_or(|variant| !field.variants.contains(&variant))
        {
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
    /// Stable public service name, for example `connectivity`.
    const NAME: &'static str;
    /// Version of the status schema.
    const VERSION: u16;
    /// Metadata of the service-specific status fields, at most
    /// [`MAX_FIELDS`] entries.
    const STATUS_SCHEMA: &'static [FieldDescriptor];
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
        _action: &str,
        _args: &[(u32, Value)],
        _out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        Err(ActionError::UnknownAction)
    }
}

/// Name and version of a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceInfo {
    /// Stable public name.
    pub name: &'static str,
    /// Version of the status schema.
    pub version: u32,
}

/// Compact status of one service, as listed for the whole device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceStatusSummary {
    /// Service name.
    pub name: &'static str,
    /// Lifecycle state.
    pub state: ServiceState,
    /// Operational quality.
    pub health: ServiceHealth,
    /// Whether the service can currently perform its intended role.
    pub ready: bool,
    /// Incremented whenever the state, health, readiness, reason or the active
    /// issues change.
    pub revision: u32,
    /// Time spent in the current lifecycle state.
    pub state_age_ms: u64,
    /// Stable machine-readable short reason.
    pub reason: Option<&'static str>,
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
    hash: u32,
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

fn observable_hash(summary: &ServiceSummary, issues: &[Issue]) -> u32 {
    let mut hash = fnv(FNV_OFFSET, &[summary.state as u8, summary.health as u8]);
    hash = fnv(hash, &[u8::from(summary.ready)]);
    if let Some(reason) = summary.reason {
        hash = fnv(hash, reason.as_bytes());
    }
    for issue in issues {
        hash = fnv(hash, &[issue.severity as u8]);
        hash = fnv(hash, issue.code.as_bytes());
        hash = fnv(hash, &[0]);
    }
    hash
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
        const { assert!(S::STATUS_SCHEMA.len() <= MAX_FIELDS) };
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
            name: S::NAME,
            version: S::VERSION as u32,
        }
    }

    async fn observe(&self, now_ms: u64) -> (ServiceStatusSummary, Vec<Issue, MAX_ISSUES>) {
        let summary = self.service.summary().await;
        let mut issues = Vec::new();
        self.service.issues(&mut issues).await;
        let reason = summary
            .reason
            .or_else(|| issues.first().map(|issue| issue.code));
        let summary = ServiceSummary { reason, ..summary };
        let hash = observable_hash(&summary, &issues);

        let track = self.track.lock(|cell| {
            let mut track = cell.get().unwrap_or(Track {
                revision: 1,
                hash,
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
                name: S::NAME,
                state: summary.state,
                health: summary.health,
                ready: summary.ready,
                revision: track.revision,
                state_age_ms: now_ms.saturating_sub(track.state_since_ms),
                reason: summary.reason,
                essential: S::ESSENTIAL,
            },
            issues,
        )
    }
}

/// The generic operations of one tracked service.
#[allow(async_fn_in_trait)]
pub trait ServiceOps {
    /// Name and version.
    fn info(&self) -> ServiceInfo;
    /// Status field metadata.
    fn schema(&self) -> &'static [FieldDescriptor];
    /// Compact status.
    async fn summary(&self, now_ms: u64) -> ServiceStatusSummary;
    /// Detailed status.
    async fn status(&self, now_ms: u64) -> ServiceStatus;
    /// Action metadata.
    fn actions(&self) -> &'static [ActionDescriptor];
    /// Runs an action.
    async fn invoke(&self, action: &str, args: &[(u32, Value)]) -> Result<Records, ActionError>;
}

impl<S: ManagedService, M: RawMutex> ServiceOps for Tracked<S, M> {
    fn info(&self) -> ServiceInfo {
        Self::info()
    }

    fn schema(&self) -> &'static [FieldDescriptor] {
        S::STATUS_SCHEMA
    }

    async fn summary(&self, now_ms: u64) -> ServiceStatusSummary {
        self.observe(now_ms).await.0
    }

    async fn status(&self, now_ms: u64) -> ServiceStatus {
        let (summary, issues) = self.observe(now_ms).await;
        let mut collector = Collector {
            schema: S::STATUS_SCHEMA,
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

    async fn invoke(&self, action: &str, args: &[(u32, Value)]) -> Result<Records, ActionError> {
        let descriptor = S::ACTIONS
            .iter()
            .find(|descriptor| descriptor.name == action)
            .ok_or(ActionError::UnknownAction)?;
        check_args(descriptor, args)?;
        let mut out = RecordCollector::new(descriptor.results);
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
    /// Name and version of the service at `index`.
    fn service_info(&self, index: usize) -> Option<ServiceInfo>;
    /// Name, version and status schema of the named service.
    fn schema(&self, service: &str) -> Option<(ServiceInfo, &'static [FieldDescriptor])>;
    /// Compact status of the service at `index`.
    async fn summary(&self, index: usize, now_ms: u64) -> Option<ServiceStatusSummary>;
    /// Detailed status of the named service.
    async fn status(&self, service: &str, now_ms: u64) -> Result<ServiceStatus, StatusError>;
    /// Actions of the service at `index`.
    fn service_actions(&self, index: usize) -> &'static [ActionDescriptor];
    /// Descriptor of an action of the named service.
    fn action(&self, service: &str, action: &str)
    -> Result<&'static ActionDescriptor, ActionError>;
    /// Runs an action of the named service.
    async fn invoke(
        &self,
        service: &str,
        action: &str,
        args: &[(u32, Value)],
    ) -> Result<Records, ActionError>;
}

impl ServiceRegistry for () {
    fn service_count(&self) -> usize {
        0
    }

    fn service_info(&self, _: usize) -> Option<ServiceInfo> {
        None
    }

    fn schema(&self, _: &str) -> Option<(ServiceInfo, &'static [FieldDescriptor])> {
        None
    }

    async fn summary(&self, _: usize, _: u64) -> Option<ServiceStatusSummary> {
        None
    }

    async fn status(&self, _: &str, _: u64) -> Result<ServiceStatus, StatusError> {
        Err(StatusError::UnknownService)
    }

    fn service_actions(&self, _: usize) -> &'static [ActionDescriptor] {
        &[]
    }

    fn action(&self, _: &str, _: &str) -> Result<&'static ActionDescriptor, ActionError> {
        Err(ActionError::UnknownService)
    }

    async fn invoke(&self, _: &str, _: &str, _: &[(u32, Value)]) -> Result<Records, ActionError> {
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

            fn schema(&self, service: &str) -> Option<(ServiceInfo, &'static [FieldDescriptor])> {
                $(if self.$index.info().name == service {
                    return Some((self.$index.info(), self.$index.schema()));
                })+
                None
            }

            async fn summary(&self, index: usize, now_ms: u64) -> Option<ServiceStatusSummary> {
                match index {
                    $($index => Some(self.$index.summary(now_ms).await),)+
                    _ => None,
                }
            }

            async fn status(&self, service: &str, now_ms: u64) -> Result<ServiceStatus, StatusError> {
                $(if self.$index.info().name == service {
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
                service: &str,
                action: &str,
            ) -> Result<&'static ActionDescriptor, ActionError> {
                $(if self.$index.info().name == service {
                    return self
                        .$index
                        .actions()
                        .iter()
                        .find(|descriptor| descriptor.name == action)
                        .ok_or(ActionError::UnknownAction);
                })+
                Err(ActionError::UnknownService)
            }

            async fn invoke(
                &self,
                service: &str,
                action: &str,
                args: &[(u32, Value)],
            ) -> Result<Records, ActionError> {
                $(if self.$index.info().name == service {
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
