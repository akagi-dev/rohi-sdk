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

//! Static registries of the status and action capabilities.
//!
//! Rust cannot discover, in a generic tuple, which of its elements also
//! implement an optional trait. The firmware therefore passes one tuple per
//! capability, built from the same service instances:
//!
//! ```text
//! let wifi = Startup::new(WifiService::new(..), network_failed);   // Status + Action
//! let sensors = SensorsService::new(..);                           // Status only
//!
//! statuses: Statuses::new((&wifi, &sensors))   -> StatusRegistry
//! actions:  (&wifi,)                           -> ActionRegistry
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

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;

use crate::action::{Action, ActionDescriptor, ActionError, RecordCollector, Records, check_args};
use crate::schema::{actions_valid, enums_valid, fields_valid, issues_valid};
use crate::service::{Service, distinct_ids, service_name_is_valid};
use crate::status::{
    Collector, Issue, ServiceInfo, ServiceSchema, ServiceStatus, ServiceStatusSummary, Status,
    StatusError, TrackCell, track_observation,
};
use crate::{MAX_FIELDS, MAX_ISSUES};
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
        && enums_valid(S::ENUMS)
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
    let track = track_observation(&tracker.0, &summary, &mut issues, now_ms);
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
    let _ = service.status(&mut collector).await;
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
) -> Result<Records, ActionError> {
    let descriptor = S::ACTIONS
        .iter()
        .find(|descriptor| descriptor.id == action)
        .ok_or(ActionError::UnknownAction)?;
    check_args(descriptor, args)?;
    let mut out = RecordCollector::new(descriptor);
    service.invoke(action, args, &mut out).await?;
    Ok(out.records)
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
/// ```ignore
/// let statuses = Statuses::<RawMutex, _>::new((&wifi, &network, &sensors));
/// ```
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
    ) -> Result<Records, ActionError>;

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

    async fn invoke(&self, _: u32, _: u32, _: &[(u32, Value)]) -> Result<Records, ActionError> {
        Err(ActionError::UnknownService)
    }
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
                    $($index => Some(summary_of(self.$index, &tracks.$index, now_ms).await),)+
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
                    return Ok(status_of(self.$index, &tracks.$index, now_ms).await);
                })+
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
                Err(ActionError::UnknownService)
            }

            async fn invoke(
                &self,
                service: u32,
                action: u32,
                args: &[(u32, Value)],
            ) -> Result<Records, ActionError> {
                $(if $name::ID == service {
                    return invoke_of(self.$index, action, args).await;
                })+
                Err(ActionError::UnknownService)
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
