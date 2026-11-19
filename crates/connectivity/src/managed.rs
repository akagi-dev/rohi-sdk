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
//! The managed `connectivity` service: route Actions, packet export and
//! status.
//!
//! The persisted state is one [`ConnectivitySection`](crate::ConnectivitySection), a typed bounded value
//! kept by the firmware's configuration owner: it is not a generic config
//! section, clients change it only through the Actions. Every mutation loads
//! the committed value, applies the change to a candidate, validates it
//! (including the 1 024-byte section budget) and asks the store to commit it
//! atomically; the runtime sees it only after the commit. A refused or failed
//! edit changes nothing.
//!
//! # Actions
//!
//! | Action | Arguments | Access | Result |
//! |--------|-----------|--------|--------|
//! | `route_add` | `route_id`, `profile_id`, `endpoint` | Mutate | |
//! | `route_remove` | `route_id` | Mutate | |
//! | `set_interval` | `route_id`, `interval_s` | Mutate | |
//! | `fallback_add` | `route_id`, `fallback_id`, `endpoint` | Mutate | |
//! | `fallback_remove` | `route_id`, `fallback_id` | Mutate | |
//! | `route_list` | `offset` | Read | `route_id`, `profile_id`, `endpoint`, `interval_s` |
//! | `fallback_list` | `route_id`, `offset` | Read | `fallback_id`, `endpoint`, `priority` |
//! | `packet_get` | `profile_id` | Read, stream | one file `packet.bin` |
//!
//! `route_add` binds the existing profile (see `telemetry.profile_list`) at
//! its current incarnation, uses the default interval of 60 s and no fallback;
//! the id, profile and primary endpoint of a route change only by removing and
//! adding it again. Offsets are zero-based ordinals of the stable order of
//! creation (routes) or priority (fallbacks); an empty page ends the listing.
//! `fallback_list` priorities start at 1.
//!
//! `packet_get` builds a fresh sealed packet for a profile, with no route and
//! no network, retains it for the lifetime of the stream and serves it
//! through the standard stream chunks: reading never builds or signs again.
//! It is an ordinary `Read` action: the packet is public as an *encrypted*
//! signed packet, a reader gets no key and cannot change a profile. A stream
//! is released on `close_stream`, after [`EXPORT_TTL_MS`] (60 s), when the control link is lost and when the
//! policy of its profile changed; the number of open streams is bounded and
//! more requests are answered with `Busy`.
//!
//! # Status
//!
//! Static slots, not a dynamic tree: for each of the `ROUTES` route slots of the build (1 to 3 are supported, see [`Slots`])
//! the fields `route<i>.id`, `.state`, `.queue_depth`, `.delivered`,
//! `.retries` and `.active_path` (`0` none, `1` the primary, `n + 1` the
//! fallback `n`), plus `active_routes` and `total_queue_depth`. Packet
//! contents and keys never appear in status or logs.

#[cfg(feature = "packet-export")]
use core::cell::RefCell;

#[cfg(feature = "packet-export")]
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use heapless::Vec;
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor, EnumVariant,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES, MAX_RECORDS, Service,
    ServiceHealth, ServiceState, ServiceSummary, Status, StatusWriter, Value, ValueKind,
    WriteError,
};
#[cfg(feature = "packet-export")]
use rohi_service::{MAX_STREAM_CHUNK, StreamChunk, StreamFile, StreamInfo};

use crate::config::{ProfileBinding, RouteError, Routes};
#[cfg(feature = "packet-export")]
use crate::pool::{PacketHandle, PacketPool};
use crate::provider::PacketExporter;
use crate::router::RouteState;
use crate::state::ConnectivityState;

pub use rohi_config::MAX_SECTION_PAYLOAD;

/// Ids of the Actions of the `connectivity` service.
pub mod action {
    /// Adds a route.
    pub const ROUTE_ADD: u32 = 1;
    /// Removes a route.
    pub const ROUTE_REMOVE: u32 = 2;
    /// Changes the publication interval of a route.
    pub const SET_INTERVAL: u32 = 3;
    /// Appends a fallback.
    pub const FALLBACK_ADD: u32 = 4;
    /// Removes a fallback.
    pub const FALLBACK_REMOVE: u32 = 5;
    /// Pages the routes.
    pub const ROUTE_LIST: u32 = 6;
    /// Pages the fallbacks of a route.
    pub const FALLBACK_LIST: u32 = 7;
    /// Exports a sealed packet as a stream.
    pub const PACKET_GET: u32 = 8;
}

/// Field ids of the Actions.
pub mod action_field {
    /// Argument: route id (every route Action).
    pub const ROUTE_ID: u32 = 1;
    /// Argument of `route_add`: the profile.
    pub const PROFILE_ID: u32 = 2;
    /// Argument of `route_add`: the primary endpoint.
    pub const ENDPOINT: u32 = 3;
    /// Argument of `set_interval`: seconds.
    pub const INTERVAL_S: u32 = 2;
    /// Argument of the fallback Actions: the fallback id.
    pub const FALLBACK_ID: u32 = 2;
    /// Argument of `fallback_add`: the endpoint.
    pub const FALLBACK_ENDPOINT: u32 = 3;
    /// Argument of `route_list`: the offset.
    pub const OFFSET: u32 = 1;
    /// Argument of `fallback_list`: the offset (the route takes id 1).
    pub const FALLBACK_OFFSET: u32 = 2;
    /// Argument of `packet_get`: the profile.
    pub const PACKET_PROFILE: u32 = 1;
    /// Result of `route_list`: route id.
    pub const LIST_ROUTE: u32 = 1;
    /// Result of `route_list`: profile id.
    pub const LIST_PROFILE: u32 = 2;
    /// Result of `route_list`: primary endpoint.
    pub const LIST_ENDPOINT: u32 = 3;
    /// Result of `route_list`: interval in seconds.
    pub const LIST_INTERVAL: u32 = 4;
    /// Result of `fallback_list`: fallback id.
    pub const FALLBACK_LIST_ID: u32 = 1;
    /// Result of `fallback_list`: endpoint.
    pub const FALLBACK_LIST_ENDPOINT: u32 = 2;
    /// Result of `fallback_list`: priority, starting at 1.
    pub const FALLBACK_LIST_PRIORITY: u32 = 3;
}

/// Field ids of the status of the service.
pub mod status_field {
    /// Routes with a queue or a publication due.
    pub const ACTIVE_ROUTES: u32 = 1;
    /// Packets queued over all routes.
    pub const TOTAL_QUEUE_DEPTH: u32 = 2;

    /// The id of field `n` (0 id, 1 state, 2 queue depth, 3 delivered, 4
    /// retries, 5 active path) of route slot `slot`.
    pub const fn route(slot: usize, n: u32) -> u32 {
        10 * (slot as u32 + 1) + n
    }
}

/// Issue codes of the service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE;

    /// The routes cannot be read from storage; none is active.
    pub const STORAGE_UNAVAILABLE: u32 = FIRST_SERVICE_ISSUE;
    /// A route's profile is missing or was recreated; the route is suspended.
    pub const PROFILE_MISSING: u32 = FIRST_SERVICE_ISSUE + 1;
    /// The last attempt to build a packet failed.
    pub const PACKET_BUILD_FAILED: u32 = FIRST_SERVICE_ISSUE + 2;
    /// Packets are queued but every delivery path of a route is failing and
    /// cooling down.
    pub const DELIVERY_FAILING: u32 = FIRST_SERVICE_ISSUE + 3;
}

const SERVICE_ID: u32 = rohi_service::service_id("connectivity");
const STATE_ENUM: u32 = 1;

/// Failure of the persistence behind the routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The storage is unavailable or the stored section is unreadable.
    Unavailable,
    /// Unexpected failure.
    Internal,
}

impl From<StoreError> for ActionError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Unavailable => Self::Unavailable,
            StoreError::Internal => Self::Internal,
        }
    }
}

/// Persistence of the routes, implemented by the firmware over the
/// configuration owner. `commit` is durable: it persists the whole candidate
/// atomically and publishes it to [`ConnectivityState`], or fails and changes
/// nothing.
#[allow(async_fn_in_trait)]
pub trait ConnectivityStore<const ROUTES: usize, const FALLBACKS: usize> {
    /// The committed routes; none when nothing was stored.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; an unreadable section is an error, not an empty set.
    async fn load(&self) -> Result<Routes<ROUTES, FALLBACKS>, StoreError>;

    /// Atomically persists `candidate` and publishes it.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; nothing changed.
    async fn commit(&self, candidate: &Routes<ROUTES, FALLBACKS>) -> Result<(), StoreError>;
}

#[cfg(feature = "packet-export")]
struct Export {
    id: u32,
    handle: PacketHandle,
    created_ms: u64,
}

/// How long an exported packet is kept when the client neither reads to the
/// end and closes nor loses the link, in milliseconds.
#[cfg(feature = "packet-export")]
pub const EXPORT_TTL_MS: u64 = 60_000;

#[cfg(feature = "packet-export")]
struct Exports<const N: usize, const BYTES: usize> {
    pool: PacketPool<N, BYTES>,
    open: Vec<Export, N>,
    next: u32,
}

/// The `connectivity` service over a [`ConnectivityStore`] and a
/// [`PacketExporter`], with room for `EXPORTS` open packet streams of up to
/// `BYTES` bytes.
pub struct ConnectivityService<
    'a,
    M: RawMutex,
    S,
    X,
    const ROUTES: usize,
    const FALLBACKS: usize,
    const EXPORTS: usize,
    const BYTES: usize,
> {
    store: S,
    exporter: X,
    state: &'a ConnectivityState<M, ROUTES, FALLBACKS>,
    // Serializes read-modify-write: no lost updates between Actions.
    edit: Mutex<M, ()>,
    #[cfg(feature = "packet-export")]
    exports: BlockingMutex<M, RefCell<Exports<EXPORTS, BYTES>>>,
}

impl<M: RawMutex, S, X, const R: usize, const F: usize, const E: usize, const B: usize> Service
    for ConnectivityService<'_, M, S, X, R, F, E, B>
{
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "connectivity";
}

impl<
    'a,
    M: RawMutex,
    S: ConnectivityStore<R, F>,
    X: PacketExporter,
    const R: usize,
    const F: usize,
    const E: usize,
    const B: usize,
> ConnectivityService<'a, M, S, X, R, F, E, B>
{
    /// A service over `store`, building exports with `exporter` and reporting
    /// the runtime state of `state`.
    pub const fn new(store: S, exporter: X, state: &'a ConnectivityState<M, R, F>) -> Self {
        Self {
            store,
            exporter,
            state,
            edit: Mutex::new(()),
            #[cfg(feature = "packet-export")]
            exports: BlockingMutex::new(RefCell::new(Exports {
                pool: PacketPool::new(),
                open: Vec::new(),
                next: 1,
            })),
        }
    }

    async fn edit(
        &self,
        change: impl FnOnce(&mut Routes<R, F>) -> Result<(), RouteError>,
    ) -> Result<(), ActionError> {
        let _guard = self.edit.lock().await;
        let mut candidate = self.store.load().await?;
        change(&mut candidate).map_err(reject)?;
        if minicbor::len(SectionRef(&candidate)) > MAX_SECTION_PAYLOAD {
            return Err(ActionError::Rejected("section_too_large"));
        }
        self.store.commit(&candidate).await?;
        Ok(())
    }

    /// The number of open packet streams.
    #[cfg(feature = "packet-export")]
    pub fn open_streams(&self) -> usize {
        self.exports.lock(|exports| exports.borrow().open.len())
    }

    /// Releases the streams that outlived [`EXPORT_TTL_MS`].
    #[cfg(feature = "packet-export")]
    fn expire(&self) {
        let now = self.exporter.now_ms();
        self.exports.lock(|exports| {
            let mut exports = exports.borrow_mut();
            let mut index = 0;
            while index < exports.open.len() {
                if now.saturating_sub(exports.open[index].created_ms) >= EXPORT_TTL_MS {
                    let export = exports.open.swap_remove(index);
                    exports.pool.release(export.handle);
                } else {
                    index += 1;
                }
            }
        });
    }

    #[cfg(feature = "packet-export")]
    fn release(&self, stream: u32) -> bool {
        self.exports.lock(|exports| {
            let mut exports = exports.borrow_mut();
            match exports.open.iter().position(|e| e.id == stream) {
                Some(position) => {
                    let export = exports.open.swap_remove(position);
                    exports.pool.release(export.handle);
                    true
                }
                None => false,
            }
        })
    }
}

struct SectionRef<'a, const R: usize, const F: usize>(&'a Routes<R, F>);

impl<C, const R: usize, const F: usize> minicbor::Encode<C> for SectionRef<'_, R, F> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        minicbor::Encode::encode(self.0, e, ctx)
    }
}

impl<C, const R: usize, const F: usize> minicbor::CborLen<C> for SectionRef<'_, R, F> {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        minicbor::CborLen::cbor_len(self.0, ctx)
    }
}

fn reject(error: RouteError) -> ActionError {
    log::debug!(target: "connectivity", "Route change rejected (err={:?})", error);
    ActionError::Rejected(match error {
        RouteError::InvalidId => "invalid_id",
        RouteError::DuplicateRoute => "route_exists",
        RouteError::RouteNotFound => "route_not_found",
        RouteError::DuplicateFallback => "fallback_exists",
        RouteError::FallbackNotFound => "fallback_not_found",
        RouteError::DuplicateEndpoint => "duplicate_endpoint",
        RouteError::InvalidEndpoint => "invalid_endpoint",
        RouteError::InvalidInterval => "invalid_interval",
        RouteError::TooManyRoutes => "too_many_routes",
        RouteError::TooManyFallbacks => "too_many_fallbacks",
    })
}

fn arg(args: &[(u32, Value)], id: u32) -> Result<&Value, ActionError> {
    args.iter()
        .find(|(field, _)| *field == id)
        .map(|(_, value)| value)
        .ok_or(ActionError::InvalidArgument)
}

fn text(args: &[(u32, Value)], id: u32) -> Result<&str, ActionError> {
    arg(args, id)?.as_str().ok_or(ActionError::InvalidArgument)
}

fn offset(args: &[(u32, Value)], id: u32) -> Result<usize, ActionError> {
    let offset = arg(args, id)?
        .as_u64()
        .ok_or(ActionError::InvalidArgument)?;
    usize::try_from(offset).map_err(|_| ActionError::InvalidArgument)
}

fn write(result: Result<(), WriteError>) -> Result<(), ActionError> {
    result.map_err(|_| ActionError::Internal)
}

const ROUTE: FieldDescriptor =
    FieldDescriptor::new(action_field::ROUTE_ID, "route_id", ValueKind::String);

const STATES: &[EnumDescriptor] = &[EnumDescriptor::new(
    STATE_ENUM,
    &[
        EnumVariant::new(0, "idle"),
        EnumVariant::new(1, "delivering"),
        EnumVariant::new(2, "backoff"),
        EnumVariant::new(3, "profile_missing"),
    ],
)];

// The status schema is written out for the route count of the build: the six fields of
// every route slot follow the two aggregates, with stable ids and names, as a literal
// slice like the schema of any other service. There is nothing to build or register at
// run time.
macro_rules! status_schema {
    ($($slot:literal),+) => {
        &[
            FieldDescriptor::new(status_field::ACTIVE_ROUTES, "active_routes", ValueKind::U64),
            FieldDescriptor::new(
                status_field::TOTAL_QUEUE_DEPTH,
                "total_queue_depth",
                ValueKind::U64,
            ),
            $(
                FieldDescriptor::new(
                    status_field::route($slot, 0),
                    concat!("route", $slot, ".id"),
                    ValueKind::String,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, 1),
                    concat!("route", $slot, ".state"),
                    ValueKind::Enum,
                )
                .with_enum(STATE_ENUM),
                FieldDescriptor::new(
                    status_field::route($slot, 2),
                    concat!("route", $slot, ".queue_depth"),
                    ValueKind::U64,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, 3),
                    concat!("route", $slot, ".delivered"),
                    ValueKind::U64,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, 4),
                    concat!("route", $slot, ".retries"),
                    ValueKind::U64,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, 5),
                    concat!("route", $slot, ".active_path"),
                    ValueKind::U64,
                ),
            )+
        ]
    };
}

/// The route count of a build, as a type: the status schema of a service with `N` route
/// slots. [`RouteSlots`] is implemented for 1, 2 and 3 slots; any other count does not
/// compile, so no schema exists for a count the build does not use.
pub struct Slots<const N: usize>;

/// The static status schema of [`Slots`] of one size.
pub trait RouteSlots {
    /// The aggregate fields and six fields per route slot.
    const SCHEMA: &'static [FieldDescriptor];
}

impl RouteSlots for Slots<1> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0);
}

impl RouteSlots for Slots<2> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0, 1);
}

impl RouteSlots for Slots<3> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0, 1, 2);
}

fn state_number(state: RouteState) -> u32 {
    match state {
        RouteState::Idle => 0,
        RouteState::Delivering => 1,
        RouteState::Backoff => 2,
        RouteState::ProfileMissing => 3,
    }
}

impl<
    M: RawMutex,
    S: ConnectivityStore<R, F>,
    X: PacketExporter,
    const R: usize,
    const F: usize,
    const E: usize,
    const B: usize,
> Status for ConnectivityService<'_, M, S, X, R, F, E, B>
where
    Slots<R>: RouteSlots,
{
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = <Slots<R> as RouteSlots>::SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::STORAGE_UNAVAILABLE, "storage_unavailable"),
        IssueDescriptor::new(issue::PROFILE_MISSING, "profile_missing"),
        IssueDescriptor::new(issue::PACKET_BUILD_FAILED, "packet_build_failed"),
        IssueDescriptor::new(issue::DELIVERY_FAILING, "delivery_failing"),
    ];

    async fn summary(&self) -> ServiceSummary {
        if self.state.is_unreadable() {
            return ServiceSummary::new(ServiceState::Failed, ServiceHealth::Error, false);
        }
        let routes = self.state.routes();
        if routes.routes.is_empty() {
            // No route is a valid state: nothing is sent.
            return ServiceSummary::new(ServiceState::Stopped, ServiceHealth::Unknown, false);
        }
        // Producing packets and delivering them are different things: a route
        // whose every path is failing is degraded even when packets are built.
        let degraded = self
            .state
            .views()
            .iter()
            .any(|view| matches!(view.state, RouteState::ProfileMissing | RouteState::Backoff))
            || self.state.health().last_build_failure.is_some();
        let health = if degraded {
            ServiceHealth::Degraded
        } else {
            ServiceHealth::Ok
        };
        ServiceSummary::new(ServiceState::Running, health, true)
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let mut push = |code| {
            let _ = out.push(Issue {
                severity: IssueSeverity::Warning,
                code,
                age_ms: None,
            });
        };
        if self.state.is_unreadable() {
            push(issue::STORAGE_UNAVAILABLE);
        }
        if self
            .state
            .views()
            .iter()
            .any(|view| view.state == RouteState::ProfileMissing)
        {
            push(issue::PROFILE_MISSING);
        }
        if self.state.health().last_build_failure.is_some() {
            push(issue::PACKET_BUILD_FAILED);
        }
        if self
            .state
            .views()
            .iter()
            .any(|view| view.state == RouteState::Backoff)
        {
            push(issue::DELIVERY_FAILING);
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let views = self.state.views();
        let active = views.iter().filter(|v| v.queue_depth > 0).count();
        let total: usize = views.iter().map(|v| v.queue_depth).sum();
        out.u64(status_field::ACTIVE_ROUTES, active as u64)?;
        out.u64(status_field::TOTAL_QUEUE_DEPTH, total as u64)?;
        for (slot, view) in views.iter().enumerate().take(R) {
            out.text(status_field::route(slot, 0), view.id.as_str())?;
            out.variant(status_field::route(slot, 1), state_number(view.state))?;
            out.u64(status_field::route(slot, 2), view.queue_depth as u64)?;
            out.u64(
                status_field::route(slot, 3),
                u64::from(view.counters.delivered),
            )?;
            out.u64(
                status_field::route(slot, 4),
                u64::from(view.counters.retries),
            )?;
            out.u64(
                status_field::route(slot, 5),
                view.active_path.map_or(0, |path| path as u64 + 1),
            )?;
        }
        Ok(())
    }
}

impl<
    M: RawMutex,
    S: ConnectivityStore<R, F>,
    X: PacketExporter,
    const R: usize,
    const F: usize,
    const E: usize,
    const B: usize,
> Action for ConnectivityService<'_, M, S, X, R, F, E, B>
{
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::ROUTE_ADD, "route_add", ActionAccess::Mutate).args(&[
            ROUTE,
            FieldDescriptor::new(action_field::PROFILE_ID, "profile_id", ValueKind::String),
            FieldDescriptor::new(action_field::ENDPOINT, "endpoint", ValueKind::String),
        ]),
        ActionDescriptor::new(action::ROUTE_REMOVE, "route_remove", ActionAccess::Mutate)
            .args(&[ROUTE]),
        ActionDescriptor::new(action::SET_INTERVAL, "set_interval", ActionAccess::Mutate).args(&[
            ROUTE,
            FieldDescriptor::new(action_field::INTERVAL_S, "interval_s", ValueKind::U64),
        ]),
        ActionDescriptor::new(action::FALLBACK_ADD, "fallback_add", ActionAccess::Mutate).args(&[
            ROUTE,
            FieldDescriptor::new(action_field::FALLBACK_ID, "fallback_id", ValueKind::String),
            FieldDescriptor::new(
                action_field::FALLBACK_ENDPOINT,
                "endpoint",
                ValueKind::String,
            ),
        ]),
        ActionDescriptor::new(
            action::FALLBACK_REMOVE,
            "fallback_remove",
            ActionAccess::Mutate,
        )
        .args(&[
            ROUTE,
            FieldDescriptor::new(action_field::FALLBACK_ID, "fallback_id", ValueKind::String),
        ]),
        ActionDescriptor::new(action::ROUTE_LIST, "route_list", ActionAccess::Read)
            .args(&[FieldDescriptor::new(
                action_field::OFFSET,
                "offset",
                ValueKind::U64,
            )])
            .results(&[
                FieldDescriptor::new(action_field::LIST_ROUTE, "route_id", ValueKind::String),
                FieldDescriptor::new(action_field::LIST_PROFILE, "profile_id", ValueKind::String),
                FieldDescriptor::new(action_field::LIST_ENDPOINT, "endpoint", ValueKind::String),
                FieldDescriptor::new(action_field::LIST_INTERVAL, "interval_s", ValueKind::U64),
            ]),
        ActionDescriptor::new(action::FALLBACK_LIST, "fallback_list", ActionAccess::Read)
            .args(&[
                ROUTE,
                FieldDescriptor::new(action_field::FALLBACK_OFFSET, "offset", ValueKind::U64),
            ])
            .results(&[
                FieldDescriptor::new(
                    action_field::FALLBACK_LIST_ID,
                    "fallback_id",
                    ValueKind::String,
                ),
                FieldDescriptor::new(
                    action_field::FALLBACK_LIST_ENDPOINT,
                    "endpoint",
                    ValueKind::String,
                ),
                FieldDescriptor::new(
                    action_field::FALLBACK_LIST_PRIORITY,
                    "priority",
                    ValueKind::U64,
                ),
            ]),
        #[cfg(feature = "packet-export")]
        ActionDescriptor::new(action::PACKET_GET, "packet_get", ActionAccess::Read)
            .args(&[FieldDescriptor::new(
                action_field::PACKET_PROFILE,
                "profile_id",
                ValueKind::String,
            )])
            .stream(),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            action::ROUTE_ADD => {
                let id = text(args, action_field::ROUTE_ID)?;
                let profile = text(args, action_field::PROFILE_ID)?;
                let endpoint = text(args, action_field::ENDPOINT)?;
                let stamp = self
                    .exporter
                    .resolve(profile)
                    .ok_or(ActionError::Rejected("profile_not_found"))?;
                let binding = ProfileBinding {
                    id: stamp.id,
                    incarnation: stamp.incarnation,
                };
                self.edit(|routes| routes.add_route(id, binding, endpoint))
                    .await
            }
            action::ROUTE_REMOVE => {
                let id = text(args, action_field::ROUTE_ID)?;
                self.edit(|routes| routes.remove_route(id)).await
            }
            action::SET_INTERVAL => {
                let id = text(args, action_field::ROUTE_ID)?;
                let seconds = arg(args, action_field::INTERVAL_S)?
                    .as_u64()
                    .ok_or(ActionError::InvalidArgument)?;
                let seconds = u32::try_from(seconds)
                    .map_err(|_| ActionError::Rejected("invalid_interval"))?;
                self.edit(|routes| routes.set_interval(id, seconds)).await
            }
            action::FALLBACK_ADD => {
                let route = text(args, action_field::ROUTE_ID)?;
                let fallback = text(args, action_field::FALLBACK_ID)?;
                let endpoint = text(args, action_field::FALLBACK_ENDPOINT)?;
                self.edit(|routes| routes.add_fallback(route, fallback, endpoint))
                    .await
            }
            action::FALLBACK_REMOVE => {
                let route = text(args, action_field::ROUTE_ID)?;
                let fallback = text(args, action_field::FALLBACK_ID)?;
                self.edit(|routes| routes.remove_fallback(route, fallback))
                    .await
            }
            action::ROUTE_LIST => {
                let offset = offset(args, action_field::OFFSET)?;
                let routes = self.store.load().await?;
                for route in routes.routes.iter().skip(offset).take(MAX_RECORDS) {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    write(out.text(action_field::LIST_ROUTE, route.id.as_str()))?;
                    write(out.text(action_field::LIST_PROFILE, route.profile.id.as_str()))?;
                    write(out.text(action_field::LIST_ENDPOINT, route.primary.as_str()))?;
                    write(out.u64(action_field::LIST_INTERVAL, u64::from(route.interval_s)))?;
                }
                Ok(())
            }
            action::FALLBACK_LIST => {
                let id = text(args, action_field::ROUTE_ID)?;
                let offset = offset(args, action_field::FALLBACK_OFFSET)?;
                let routes = self.store.load().await?;
                let route = routes
                    .get(id)
                    .ok_or(ActionError::Rejected("route_not_found"))?;
                for (index, fallback) in route
                    .fallbacks
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .take(MAX_RECORDS)
                {
                    out.next_record().map_err(|_| ActionError::Internal)?;
                    write(out.text(action_field::FALLBACK_LIST_ID, fallback.id.as_str()))?;
                    write(out.text(
                        action_field::FALLBACK_LIST_ENDPOINT,
                        fallback.endpoint.as_str(),
                    ))?;
                    write(out.u64(action_field::FALLBACK_LIST_PRIORITY, index as u64 + 1))?;
                }
                Ok(())
            }
            #[cfg(feature = "packet-export")]
            action::PACKET_GET => {
                let profile = text(args, action_field::PACKET_PROFILE)?;
                // An abandoned stream must not keep its slot past its lifetime.
                self.expire();
                let stamp = self
                    .exporter
                    .resolve(profile)
                    .ok_or(ActionError::Rejected("profile_not_found"))?;
                if self.exports.lock(|exports| {
                    let exports = exports.borrow();
                    exports.open.is_full() || exports.pool.in_use() >= E
                }) {
                    return Err(ActionError::Busy);
                }
                // Built once, outside any lock; then retained until the stream
                // is closed: reading never builds or signs again.
                let created_ms = self.exporter.now_ms();
                let mut buffer = [0u8; B];
                let (built, len) = self
                    .exporter
                    .export(profile, &mut buffer)
                    .await
                    .map_err(|failure| ActionError::Rejected(failure.code()))?;
                if built != stamp {
                    // The policy changed while building: never offer a packet
                    // built under another one.
                    return Err(ActionError::Rejected("profile_changed"));
                }
                let info = self.exports.lock(|exports| {
                    let mut exports = exports.borrow_mut();
                    let (handle, slot) = exports.pool.alloc(built)?;
                    slot[..len].copy_from_slice(&buffer[..len]);
                    if !exports.pool.seal(handle, len) {
                        exports.pool.release(handle);
                        return None;
                    }
                    let id = exports.next;
                    exports.next = exports.next.checked_add(1).unwrap_or(1);
                    if exports
                        .open
                        .push(Export {
                            id,
                            handle,
                            created_ms,
                        })
                        .is_err()
                    {
                        exports.pool.release(handle);
                        return None;
                    }
                    Some(id)
                });
                let id = info.ok_or(ActionError::Busy)?;
                let mut files = Vec::new();
                let mut name = heapless::String::new();
                let _ = name.push_str("packet.bin");
                let _ = files.push(StreamFile {
                    id: 1,
                    name,
                    size_bytes: len as u64,
                });
                if out.stream(StreamInfo { id, files }).is_err() {
                    self.release(id);
                    return Err(ActionError::Internal);
                }
                Ok(())
            }
            _ => Err(ActionError::UnknownAction),
        }
    }

    #[cfg(feature = "packet-export")]
    async fn read_stream(
        &self,
        stream: u32,
        file: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<StreamChunk, ActionError> {
        if file != 1 {
            return Err(ActionError::UnknownStream);
        }
        self.expire();
        let exporter = &self.exporter;
        enum Read {
            Chunk(StreamChunk),
            Stale,
            Missing,
            Beyond,
        }
        let read = self.exports.lock(|exports| {
            let exports = exports.borrow_mut();
            let Some(export) = exports.open.iter().find(|e| e.id == stream) else {
                return Read::Missing;
            };
            let handle = export.handle;
            let Some((stamp, bytes)) = exports.pool.get(handle) else {
                return Read::Missing;
            };
            // A packet built under a policy that is no longer current is not
            // served any more.
            if exporter.resolve(stamp.id.as_str()).as_ref() != Some(stamp) {
                return Read::Stale;
            }
            let Ok(offset) = usize::try_from(offset) else {
                return Read::Beyond;
            };
            if offset > bytes.len() {
                return Read::Beyond;
            }
            let len = (bytes.len() - offset).min(buf.len()).min(MAX_STREAM_CHUNK);
            buf[..len].copy_from_slice(&bytes[offset..offset + len]);
            Read::Chunk(StreamChunk {
                len,
                eof: offset + len == bytes.len(),
            })
        });
        match read {
            Read::Chunk(chunk) => Ok(chunk),
            Read::Missing => Err(ActionError::UnknownStream),
            Read::Beyond => Err(ActionError::InvalidArgument),
            Read::Stale => {
                self.release(stream);
                Err(ActionError::UnknownStream)
            }
        }
    }

    #[cfg(feature = "packet-export")]
    async fn close_stream(&self, stream: u32) -> Result<(), ActionError> {
        if self.release(stream) {
            Ok(())
        } else {
            Err(ActionError::UnknownStream)
        }
    }

    #[cfg(feature = "packet-export")]
    fn link_lost(&self) {
        self.exports.lock(|exports| {
            let mut exports = exports.borrow_mut();
            while let Some(export) = exports.open.pop() {
                exports.pool.release(export.handle);
            }
        });
    }
}

#[cfg(test)]
mod tests;
