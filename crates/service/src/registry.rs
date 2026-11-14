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

//! Static registries of the status and action capabilities.
//!
//! Rust cannot discover, in a generic tuple, which of its elements also
//! implement an optional trait. The firmware therefore passes one tuple per
//! capability, built from the same service instances:
//!
//! ```
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use rohi_service::{
//!     Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, ActionRegistry,
//!     FieldDescriptor, Service, ServiceHealth, ServiceState, ServiceSummary, Status, StatusError,
//!     StatusRegistry, StatusWriter, Statuses, Value, ValueKind, WriteError,
//! };
//!
//! #[derive(Service)]
//! struct WifiService;
//!
//! impl Status for WifiService {
//!     const VERSION: u16 = 1;
//!     const STATUS_SCHEMA: &'static [FieldDescriptor] =
//!         &[FieldDescriptor::new(1, "connected", ValueKind::Bool)];
//!
//!     async fn summary(&self) -> ServiceSummary {
//!         ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
//!     }
//!
//!     async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
//!         out.bool(1, true)
//!     }
//! }
//!
//! impl Action for WifiService {
//!     const ACTIONS: &'static [ActionDescriptor] = &[ActionDescriptor::new(1, "reconnect", ActionAccess::Read)];
//!
//!     async fn invoke(
//!         &self,
//!         action: u32,
//!         _args: &[(u32, Value)],
//!         _out: &mut impl ActionOutput,
//!     ) -> Result<(), ActionError> {
//!         if action == 1 { Ok(()) } else { Err(ActionError::UnknownAction) }
//!     }
//! }
//!
//! // Status only: no actions.
//! #[derive(Service)]
//! struct SensorsService;
//!
//! impl Status for SensorsService {
//!     const VERSION: u16 = 1;
//!     const STATUS_SCHEMA: &'static [FieldDescriptor] = &[];
//!
//!     async fn summary(&self) -> ServiceSummary {
//!         ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
//!     }
//!
//!     async fn status(&self, _out: &mut impl StatusWriter) -> Result<(), WriteError> {
//!         Ok(())
//!     }
//! }
//!
//! let wifi = WifiService;
//! let sensors = SensorsService;
//!
//! // One tuple per capability, over the same instances.
//! let statuses = Statuses::<NoopRawMutex, _>::new((&wifi, &sensors));
//! let actions = (&wifi,);
//!
//! assert_eq!(statuses.service_count(), 2);
//! assert!(block_on(statuses.status(WifiService::ID, 1_000)).is_ok());
//! assert!(block_on(actions.invoke(WifiService::ID, 1, &[])).is_ok());
//! // `sensors` has no actions; the status registry still knows it.
//! assert!(!actions.contains(SensorsService::ID));
//! assert_eq!(
//!     block_on(actions.invoke(SensorsService::ID, 1, &[])),
//!     Err(ActionError::UnknownService)
//! );
//! assert!(matches!(
//!     block_on(statuses.status(0, 1_000)),
//!     Err(StatusError::UnknownService)
//! ));
//! ```
//!
//! A tuple of `&S` where every `S: Status` is a [`StatusSet`] and, wrapped in
//! [`Statuses`], a [`StatusRegistry`]; a tuple of `&S` where every
//! `S: Action` is an [`ActionRegistry`] by itself. Services are shared by
//! reference, so no service state, and in particular no configuration
//! section, is ever duplicated by a registry. Ids and schemas are checked
//! while compiling: two services with the same id, or an invalid schema, are
//! a build error.
//!
//! A service that is in neither tuple is invisible to clients; one that is
//! only in the status tuple answers action requests with "not supported" (the
//! control plane tells it from an unknown service by asking both registries).
//!
//! Both registry traits are implemented for `()` (a device with no services)
//! and for tuples of up to [`MAX_REGISTRY_ENTRIES`](crate::MAX_REGISTRY_ENTRIES)
//! (12) services; [`ConfigRegistry`](crate::ConfigRegistry) is the same kind of
//! tuple over config sections.
//!
//! # Bookkeeping
//!
//! [`Statuses`] keeps one [`Tracker`] per service. It adds what every service
//! needs without its cooperation: a **status revision** that changes when
//! state, health, readiness or the active issues change (values that vary all
//! the time, such as counters or signal strength, deliberately do not bump
//! it), and the **time in the current state**. Revisions and ages are derived
//! from what the service reports when a client asks, with the device uptime
//! given by the caller.
//!
//! # Ids and names
//!
//! Requests address sections, services and actions by their numeric ids,
//! which must be unique within a registry; names are for discovery. A service
//! id is [`service_id`](crate::service_id) of its name. Two services with the
//! same id are a build error ([`Statuses::new`] and `Device::new` of
//! [`rohi-control`](../../rohi_control/index.html) check it in a `const`
//! block); two config sections with the same id are caught by
//! `Device::new` of [`rohi-control`](../../rohi_control/index.html), which
//! panics in every build, including release.
//!
//! # Schema validation
//!
//! Schemas are validated when the section or service is built, as a
//! compile-time error: section, service, action, field and issue names are
//! not empty and at most [`MAX_NAME_LEN`](crate::MAX_NAME_LEN) (24) bytes
//! (variant names [`MAX_VARIANT_NAME_LEN`](crate::MAX_VARIANT_NAME_LEN), 16),
//! so nothing is ever truncated on the wire. Field ids and names are unique
//! within a schema, action ids and names within a service, issue codes and
//! names within a service (codes from
//! [`FIRST_SERVICE_ISSUE`](crate::FIRST_SERVICE_ISSUE), or the SDK startup
//! code), enum ids are non-zero and unique, and variant values and names are
//! unique within their enum. An enum field must refer to an enum of its
//! table, no other field may. A config section has at most three
//! enums ([`MAX_ENUMS`](crate::MAX_ENUMS)), a service or action at most one
//! ([`MAX_SERVICE_ENUMS`]), constraints of the compact
//! protocol, with at most [`MAX_ENUM_VARIANTS`](crate::MAX_ENUM_VARIANTS)
//! variants. Tables are also bounded by the limits of the crate, see the
//! [crate documentation](crate#limits).
//!
//! # What lives where
//!
//! | Crate | Config section | Service | Actions |
//! |-------|----------------|---------|---------|
//! | [`rohi-net`](../../rohi_net/index.html) | `wifi` (`ssid`, `password` secret, timeouts, backoff), `network` (address mode, static IPv4, host name, DHCP timeout), `ble` (`name`) | `wifi` (link, radio heap), `network` (address, sockets), `ble` (state, name, link security) | `wifi`: `scan`, `reconnect`, `disconnect` |
//! | [`rohi-connectivity`](../../rohi_connectivity/index.html) | `connectivity` | `connectivity` (queue, counters, endpoint) | |
//! | [`rohi-location`](../../rohi_location/index.html) | `location` | `location` | |
//! | [`rohi-time`](../../rohi_time/index.html) / [`rohi-time-ntp`](../../rohi_time_ntp/index.html) | `time` (NTP servers, `rohi-time-ntp`) | `time` (unix time, source, age, failures) | |
//! | [`rohi-hal`](../../rohi_hal/index.html) | | `sensors` (`<sensor>.<measurement>` values) | |
//! | [`rohi-identity`](../../rohi_identity/index.html) | | `identity` (provisioned, public key) | `provision`, `erase` |
//!
//! The firmware (`altruist-urban/src/control.rs`) builds the tuples over its
//! runtime handles and gives them to the dispatcher. The steps to add a
//! service are in the
//! [Add a service](https://rohi.akagi.dev/guides/add-service.html) guide.
//!
//! # Limits
//!
//! The in-memory limits of the services and registries are listed in the
//! [crate documentation](crate#limits); the wire limits are defined once in
//! the build script of `rohi-control-proto`.

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;

use crate::action::{
    Action, ActionDescriptor, ActionError, ActionResult, RecordCollector, StreamChunk, check_args,
};
use crate::isolated::isolated;
use crate::schema::{actions_valid, enums_valid, fields_valid, issues_valid};
use crate::service::{Service, distinct_ids, service_name_is_valid};
use crate::status::{
    Collector, Issue, ServiceInfo, ServiceSchema, ServiceStatus, ServiceStatusSummary, Status,
    StatusError, TrackCell, WriteError, track_observation,
};
use crate::{MAX_FIELDS, MAX_ISSUES, MAX_SERVICE_ENUMS};
use rohi_config::Value;

/// Id and name of a service, as listed by an [`ActionRegistry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceIdentity {
    /// Stable numeric id.
    pub id: u32,
    /// Stable public name.
    pub name: &'static str,
}

impl ServiceIdentity {
    /// The identity of `S`.
    pub const fn of<S: Service>() -> Self {
        Self {
            id: S::ID,
            name: S::NAME,
        }
    }
}

const fn status_schema_valid<S: Status>() -> bool {
    service_name_is_valid(S::NAME)
        && S::STATUS_SCHEMA.len() <= MAX_FIELDS
        && enums_valid(S::ENUMS, MAX_SERVICE_ENUMS)
        && fields_valid(S::STATUS_SCHEMA, S::ENUMS)
        && issues_valid(S::ISSUES)
}

const fn actions_schema_valid<S: Action>() -> bool {
    service_name_is_valid(S::NAME) && actions_valid(S::ACTIONS)
}

/// The bookkeeping the status registry keeps for one service: the status
/// revision and the time since the last state change.
///
/// Revisions and ages are derived from what the service reports, so a service
/// does not have to cooperate. Observation happens when a client asks, with
/// the device uptime given by the caller; poll the compact summary regularly
/// for precise ages. A revision follows the lifecycle state, health,
/// readiness and the active issues; it does not follow the service-specific
/// status fields.
pub struct Tracker<M: RawMutex>(TrackCell<M>);

impl<M: RawMutex> Tracker<M> {
    const fn new() -> Self {
        Self(TrackCell::new(core::cell::Cell::new(None)))
    }
}

fn info_of<S: Status>() -> ServiceInfo {
    ServiceInfo {
        id: S::ID,
        name: S::NAME,
        version: u32::from(S::VERSION),
    }
}

fn schema_of<S: Status>() -> ServiceSchema {
    ServiceSchema {
        info: info_of::<S>(),
        fields: S::STATUS_SCHEMA,
        enums: S::ENUMS,
        issues: S::ISSUES,
    }
}

async fn observe<S: Status, M: RawMutex>(
    service: &S,
    tracker: &Tracker<M>,
    now_ms: u64,
) -> (ServiceStatusSummary, Vec<Issue, MAX_ISSUES>) {
    let summary = service.summary().await;
    let mut issues = Vec::new();
    service.issues(&mut issues).await;
    let track = track_observation(S::NAME, &tracker.0, &summary, &mut issues, now_ms);
    (
        ServiceStatusSummary {
            id: S::ID,
            name: S::NAME,
            version: u32::from(S::VERSION),
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

async fn summary_of<S: Status, M: RawMutex>(
    service: &S,
    tracker: &Tracker<M>,
    now_ms: u64,
) -> ServiceStatusSummary {
    observe(service, tracker, now_ms).await.0
}

async fn status_of<S: Status, M: RawMutex>(
    service: &S,
    tracker: &Tracker<M>,
    now_ms: u64,
) -> ServiceStatus {
    let (summary, issues) = observe(service, tracker, now_ms).await;
    let mut collector = Collector {
        schema: S::STATUS_SCHEMA,
        enums: S::ENUMS,
        fields: Vec::new(),
    };
    // A field the service fails to write is absent; the rest is still useful.
    if let Err(error) = service.status(&mut collector).await {
        log_status_failed(S::NAME, error);
    }
    ServiceStatus {
        summary,
        fields: collector.fields,
        issues,
    }
}

async fn invoke_of<S: Action>(
    service: &S,
    action: u32,
    args: &[(u32, Value)],
) -> Result<ActionResult, ActionError> {
    let descriptor = S::ACTIONS
        .iter()
        .find(|descriptor| descriptor.id == action)
        .ok_or(ActionError::UnknownAction)
        .inspect_err(|error| log_action_failed(S::NAME, action, *error))?;
    log::debug!(
        target: "service",
        "Invoking action (service={} action={})",
        S::NAME,
        descriptor.name
    );
    check_args(descriptor, args).inspect_err(|error| log_action_failed(S::NAME, action, *error))?;
    let mut out = RecordCollector::new(descriptor);
    service
        .invoke(action, args, &mut out)
        .await
        .inspect_err(|error| log_action_failed(S::NAME, action, *error))?;
    match out.stream {
        Some(_) if !out.records.is_empty() => {
            let error = ActionError::Internal;
            log_action_failed(S::NAME, action, error);
            Err(error)
        }
        Some(stream) => Ok(ActionResult::Stream(stream)),
        None => Ok(ActionResult::Records(out.records)),
    }
}

#[cold]
#[inline(never)]
fn log_status_failed(service: &'static str, error: WriteError) {
    log::warn!(
        target: "service",
        "Status fields incomplete: {:?} (service={})",
        error,
        service
    );
}

#[cold]
#[inline(never)]
fn log_action_failed(service: &'static str, action: u32, error: ActionError) {
    log::warn!(
        target: "service",
        "Action failed: {:?} (service={} id={:#010x})",
        error,
        service,
        action
    );
}

#[cold]
#[inline(never)]
fn log_unknown_service(operation: &'static str, service: u32) {
    log::warn!(
        target: "service",
        "Unknown service in {} (id={:#010x})",
        operation,
        service
    );
}

/// A tuple of services that all implement [`Status`], with the tracks the
/// [`Statuses`] registry keeps for them.
///
/// Implemented for tuples of `&S` (up to [`MAX_REGISTRY_ENTRIES`](crate::MAX_REGISTRY_ENTRIES)).
#[allow(async_fn_in_trait)]
pub trait StatusSet<M: RawMutex> {
    /// One [`Tracker`] per service.
    type Tracks;
    /// Whether no two services share an id.
    const IDS_UNIQUE: bool;
    /// Whether every status schema is valid.
    const SCHEMAS_VALID: bool;

    /// Fresh trackers.
    fn new_tracks() -> Self::Tracks;
    /// Number of services.
    fn count(&self) -> usize;
    /// Id, name and version of the service at `index`.
    fn info(&self, index: usize) -> Option<ServiceInfo>;
    /// Status schema of the service with the given id.
    fn schema(&self, service: u32) -> Option<ServiceSchema>;
    /// Compact status of the service at `index`.
    async fn summary(
        &self,
        tracks: &Self::Tracks,
        index: usize,
        now_ms: u64,
    ) -> Option<ServiceStatusSummary>;
    /// Detailed status of the service with the given id.
    ///
    /// # Errors
    ///
    /// [`StatusError::UnknownService`] when no service has this id. A failing
    /// [`Status::status`] is not an error here, see there.
    async fn status(
        &self,
        tracks: &Self::Tracks,
        service: u32,
        now_ms: u64,
    ) -> Result<ServiceStatus, StatusError>;
}

/// All status services compiled into the firmware.
#[allow(async_fn_in_trait)]
pub trait StatusRegistry {
    /// Whether no two services share an id.
    const IDS_UNIQUE: bool;

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

    /// Whether a service with this id reports status.
    fn contains(&self, service: u32) -> bool {
        (0..self.service_count()).any(|index| {
            self.service_info(index)
                .is_some_and(|info| info.id == service)
        })
    }
}

impl StatusRegistry for () {
    const IDS_UNIQUE: bool = true;

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
}

/// The status registry: a tuple of `&S` services and their trackers.
///
/// The tuple holds shared references to the services, so a registry never
/// copies service state. `M` is the raw mutex of the trackers, see
/// [`Tracker`]. The module documentation has a complete example.
pub struct Statuses<M: RawMutex, T: StatusSet<M>> {
    services: T,
    tracks: T::Tracks,
}

impl<M: RawMutex, T: StatusSet<M>> Statuses<M, T> {
    /// A registry over `services`. Two services with the same id or an
    /// invalid status schema fail the build.
    pub fn new(services: T) -> Self {
        const {
            assert!(T::IDS_UNIQUE, "two status services share an id");
            assert!(
                T::SCHEMAS_VALID,
                "invalid status schema: ids and names must be unique and fit"
            );
        };
        Self {
            services,
            tracks: T::new_tracks(),
        }
    }
}

impl<M: RawMutex, T: StatusSet<M>> StatusRegistry for Statuses<M, T> {
    const IDS_UNIQUE: bool = T::IDS_UNIQUE;

    fn service_count(&self) -> usize {
        self.services.count()
    }

    fn service_info(&self, index: usize) -> Option<ServiceInfo> {
        self.services.info(index)
    }

    fn schema(&self, service: u32) -> Option<ServiceSchema> {
        self.services.schema(service)
    }

    async fn summary(&self, index: usize, now_ms: u64) -> Option<ServiceStatusSummary> {
        self.services.summary(&self.tracks, index, now_ms).await
    }

    async fn status(&self, service: u32, now_ms: u64) -> Result<ServiceStatus, StatusError> {
        self.services.status(&self.tracks, service, now_ms).await
    }
}

/// All action services compiled into the firmware.
///
/// Implemented for tuples of `&S` where every `S: Action` (up to
/// [`MAX_REGISTRY_ENTRIES`](crate::MAX_REGISTRY_ENTRIES)) and for `()`.
#[allow(async_fn_in_trait)]
pub trait ActionRegistry {
    /// Whether no two services share an id.
    const IDS_UNIQUE: bool;
    /// Whether every action table is valid.
    const SCHEMAS_VALID: bool;

    /// Number of services.
    fn service_count(&self) -> usize;
    /// Identity of the service at `index`.
    fn service_identity(&self, index: usize) -> Option<ServiceIdentity>;
    /// Actions of the service at `index`.
    fn service_actions(&self, index: usize) -> &'static [ActionDescriptor];
    /// Descriptor of an action of the service with the given id.
    fn action(&self, service: u32, action: u32) -> Result<&'static ActionDescriptor, ActionError>;
    /// Runs an action of the service with the given id. The arguments are
    /// checked against the descriptor first.
    async fn invoke(
        &self,
        service: u32,
        action: u32,
        args: &[(u32, Value)],
    ) -> Result<ActionResult, ActionError>;
    /// Reads a chunk of a file of an open stream of the service with the given
    /// id into `buf`.
    async fn read_stream(
        &self,
        service: u32,
        stream: u32,
        file: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<StreamChunk, ActionError>;
    /// Closes a stream of the service with the given id.
    async fn close_stream(&self, service: u32, stream: u32) -> Result<(), ActionError>;
    /// The control link was lost: every service releases its open streams.
    fn link_lost(&self);

    /// Whether a service with this id has actions.
    fn contains(&self, service: u32) -> bool {
        (0..self.service_count()).any(|index| {
            self.service_identity(index)
                .is_some_and(|identity| identity.id == service)
        })
    }
}

impl ActionRegistry for () {
    const IDS_UNIQUE: bool = true;
    const SCHEMAS_VALID: bool = true;

    fn service_count(&self) -> usize {
        0
    }

    fn service_identity(&self, _: usize) -> Option<ServiceIdentity> {
        None
    }

    fn service_actions(&self, _: usize) -> &'static [ActionDescriptor] {
        &[]
    }

    fn action(&self, _: u32, _: u32) -> Result<&'static ActionDescriptor, ActionError> {
        Err(ActionError::UnknownService)
    }

    async fn invoke(
        &self,
        _: u32,
        _: u32,
        _: &[(u32, Value)],
    ) -> Result<ActionResult, ActionError> {
        Err(ActionError::UnknownService)
    }

    async fn read_stream(
        &self,
        _: u32,
        _: u32,
        _: u32,
        _: u64,
        _: &mut [u8],
    ) -> Result<StreamChunk, ActionError> {
        Err(ActionError::UnknownService)
    }

    async fn close_stream(&self, _: u32, _: u32) -> Result<(), ActionError> {
        Err(ActionError::UnknownService)
    }

    fn link_lost(&self) {}
}

// One tracker per tuple element; the generic `M` of the expanding impl is not
// hygienic.
macro_rules! tracker_type {
    ($name:ident) => {
        Tracker<M>
    };
}

macro_rules! tracker_new {
    ($name:ident) => {
        Tracker::<M>::new()
    };
}

macro_rules! tuple_registries {
    ($count:expr; $($name:ident . $index:tt),+) => {
        impl<'a, M: RawMutex, $($name: Status),+> StatusSet<M> for ($(&'a $name,)+) {
            type Tracks = ($(tracker_type!($name),)+);
            const IDS_UNIQUE: bool = distinct_ids(&[$($name::ID),+]);
            const SCHEMAS_VALID: bool = true $(&& status_schema_valid::<$name>())+;

            fn new_tracks() -> Self::Tracks {
                ($(tracker_new!($name),)+)
            }

            fn count(&self) -> usize {
                $count
            }

            fn info(&self, index: usize) -> Option<ServiceInfo> {
                match index {
                    $($index => Some(info_of::<$name>()),)+
                    _ => None,
                }
            }

            fn schema(&self, service: u32) -> Option<ServiceSchema> {
                $(if $name::ID == service {
                    return Some(schema_of::<$name>());
                })+
                None
            }

            async fn summary(
                &self,
                tracks: &Self::Tracks,
                index: usize,
                now_ms: u64,
            ) -> Option<ServiceStatusSummary> {
                match index {
                    $($index => Some(isolated(summary_of(self.$index, &tracks.$index, now_ms)).await),)+
                    _ => None,
                }
            }

            async fn status(
                &self,
                tracks: &Self::Tracks,
                service: u32,
                now_ms: u64,
            ) -> Result<ServiceStatus, StatusError> {
                $(if $name::ID == service {
                    return Ok(isolated(status_of(self.$index, &tracks.$index, now_ms)).await);
                })+
                log_unknown_service("status", service);
                Err(StatusError::UnknownService)
            }
        }

        impl<'a, $($name: Action),+> ActionRegistry for ($(&'a $name,)+) {
            const IDS_UNIQUE: bool = distinct_ids(&[$($name::ID),+]);
            const SCHEMAS_VALID: bool = true $(&& actions_schema_valid::<$name>())+;

            fn service_count(&self) -> usize {
                $count
            }

            fn service_identity(&self, index: usize) -> Option<ServiceIdentity> {
                match index {
                    $($index => Some(ServiceIdentity::of::<$name>()),)+
                    _ => None,
                }
            }

            fn service_actions(&self, index: usize) -> &'static [ActionDescriptor] {
                match index {
                    $($index => $name::ACTIONS,)+
                    _ => &[],
                }
            }

            fn action(
                &self,
                service: u32,
                action: u32,
            ) -> Result<&'static ActionDescriptor, ActionError> {
                $(if $name::ID == service {
                    return $name::ACTIONS
                        .iter()
                        .find(|descriptor| descriptor.id == action)
                        .ok_or(ActionError::UnknownAction);
                })+
                log_unknown_service("action", service);
                Err(ActionError::UnknownService)
            }

            async fn invoke(
                &self,
                service: u32,
                action: u32,
                args: &[(u32, Value)],
            ) -> Result<ActionResult, ActionError> {
                $(if $name::ID == service {
                    return isolated(invoke_of(self.$index, action, args)).await;
                })+
                log_unknown_service("invoke", service);
                Err(ActionError::UnknownService)
            }

            async fn read_stream(
                &self,
                service: u32,
                stream: u32,
                file: u32,
                offset: u64,
                buf: &mut [u8],
            ) -> Result<StreamChunk, ActionError> {
                $(if $name::ID == service {
                    return isolated(self.$index.read_stream(stream, file, offset, buf)).await;
                })+
                log_unknown_service("read_stream", service);
                Err(ActionError::UnknownService)
            }

            async fn close_stream(&self, service: u32, stream: u32) -> Result<(), ActionError> {
                $(if $name::ID == service {
                    return isolated(self.$index.close_stream(stream)).await;
                })+
                log_unknown_service("close_stream", service);
                Err(ActionError::UnknownService)
            }

            fn link_lost(&self) {
                log::info!(target: "service", "Link lost, notifying services (count={})", $count);
                $(self.$index.link_lost();)+
            }
        }
    };
}

tuple_registries!(1; A.0);
tuple_registries!(2; A.0, B.1);
tuple_registries!(3; A.0, B.1, C.2);
tuple_registries!(4; A.0, B.1, C.2, D.3);
tuple_registries!(5; A.0, B.1, C.2, D.3, E.4);
tuple_registries!(6; A.0, B.1, C.2, D.3, E.4, F.5);
tuple_registries!(7; A.0, B.1, C.2, D.3, E.4, F.5, G.6);
tuple_registries!(8; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7);
tuple_registries!(9; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8);
tuple_registries!(10; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9);
tuple_registries!(11; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9, K.10);
tuple_registries!(12; A.0, B.1, C.2, D.3, E.4, F.5, G.6, H.7, I.8, J.9, K.10, L.11);
