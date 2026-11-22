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

//! The managed `connectivity` service: the four route Actions, payload
//! export and status.
//!
//! The persisted state is one [`ConnectivitySection`](crate::ConnectivitySection), a typed bounded value
//! kept by the firmware's configuration owner: it is not a generic config
//! section, clients change it only through the Actions. Every mutation loads
//! the committed value, applies the change to a candidate, validates it
//! (including the 1 024-byte section budget) and asks the store to commit it
//! atomically; the runtime sees it only after the commit. A refused or failed
//! edit changes nothing.
//!
//! # Model
//!
//! Routes are **slots addressed by index** (`0`, `1`, ...; Altruist Urban has
//! two), each empty or holding one route with at most one fallback endpoint.
//! There are no names; deleting the route of one slot never moves another.
//! A route publishes a telemetry profile slot (`profile_index`, see the
//! `telemetry` service).
//!
//! # Actions
//!
//! | Action | Arguments | Access | Result |
//! |--------|-----------|--------|--------|
//! | `set_endpoint` | `index`, `profile_index`, `endpoint` | Mutate | |
//! | `set_interval` | `index`, `interval_s` | Mutate | |
//! | `set_fallback` | `index`, `endpoint` | Mutate | |
//! | `get_payload` | `profile_index` | Read, stream | one file `packet.bin` |
//!
//! **Absent endpoint.** An Action argument is always present on the wire, so
//! "no endpoint" is the **empty string** `""`: `set_endpoint(index, _, "")`
//! deletes the route of the slot and `set_fallback(index, "")` removes its
//! fallback. (An implementation that omits the argument is read the same way.)
//! In a positional or named CLI call this is an empty argument, for example
//! `connectivity set_endpoint 0 0 ""` or `... endpoint=`. Anything else must
//! be a valid endpoint URI of at most 64 bytes whose scheme this build supports
//! (`http://host[:port]`, `meshtastic+uart://!xxxxxxxx`, ...): `invalid_endpoint` for a malformed
//! one and `unsupported_endpoint_scheme` for a scheme with no backend. Both are decided
//! before anything is stored and without any network or radio I/O; the whole candidate
//! set is checked, so a stored set the build cannot deliver (after a firmware without
//! that transport) stays untouched in storage, reports the issue
//! `unsupported_scheme`, sends nothing and is repaired by deleting or replacing
//! the offending route.
//!
//! `set_endpoint` creates the route of an empty slot for the telemetry
//! profile slot `profile_index` (it must exist, else `invalid_profile_index`)
//! with the default interval of 60 s and no fallback. On a slot that holds a
//! route it replaces the profile and the primary endpoint and keeps the
//! interval and the fallback; the replacement is a new generation, so the old
//! queue, backoff and counters are dropped and no packet of the earlier route
//! is ever sent to the new endpoint. The same profile and endpoint again
//! change nothing. Deleting an empty slot succeeds and changes nothing.
//! `set_interval` changes an existing route only (`route_not_found`
//! otherwise). `set_fallback` sets the only fallback, replacing any earlier
//! one; it must differ from the primary (`duplicate_endpoint`). An `index`
//! that is not a slot of the build is refused with `invalid_index`.
//!
//! `get_payload` builds a fresh sealed packet for a profile slot, with no
//! route and no network, retains it for the lifetime of the stream and serves
//! it through the standard stream chunks: reading never builds or signs again.
//! It is an ordinary `Read` action: the packet is public as an *encrypted*
//! signed packet, a reader gets no key and cannot change a profile. A stream
//! is released on `close_stream`, after [`EXPORT_TTL_MS`] (60 s), when the control link is lost and when the
//! policy of its profile changed; the number of open streams is bounded and
//! more requests are answered with `Busy`. It is compiled with the
//! `packet-export` feature; without it the service has the other three Actions.
//!
//! # Status
//!
//! Static slots, not a dynamic tree. Two aggregates, `active_routes` (routes
//! with a queue) and `total_queue_depth`, and for each of the `ROUTES` route
//! slots of the build (1 or 2 are supported, see [`Slots`]) nine fields:
//! `route<i>.state` (`empty`, `idle`, `delivering`, `backoff` or
//! `profile_missing`) and, only for a slot that holds a route,
//! `route<i>.profile` (the profile index), `.endpoint`, `.fallback` (absent
//! without one), `.interval_s`, `.queue_depth`, `.delivered`, `.retries` and
//! `.active_path` (`0` none, `1` the primary, `2` the fallback). That is
//! exactly the limit of 20 status fields for two slots, which is why the
//! failover count and the other counters are not exposed per route. An empty
//! slot reports only `state = empty`; a slot whose route was just replaced
//! reports zero counters until the publisher has looked at it. Packet
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
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES, Service, ServiceHealth,
    ServiceSummary, Status, StatusWriter, Unit, Value, ValueKind, WriteError,
};
#[cfg(feature = "packet-export")]
use rohi_service::{MAX_STREAM_CHUNK, ResultFormat, StreamChunk, StreamFile, StreamInfo};

use crate::config::{RouteError, Routes};
use crate::endpoint::EndpointPolicy;
#[cfg(feature = "packet-export")]
use crate::pool::{PacketHandle, PacketPool};
use crate::provider::PacketExporter;
use crate::router::RouteState;
use crate::state::ConnectivityState;

pub use rohi_config::MAX_SECTION_PAYLOAD;

/// Ids of the Actions of the `connectivity` service.
pub mod action {
    /// Creates, replaces or (with an absent endpoint) deletes the route of a slot.
    pub const SET_ENDPOINT: u32 = 1;
    /// Changes the publication interval of a route.
    pub const SET_INTERVAL: u32 = 2;
    /// Sets or (with an absent endpoint) removes the fallback of a route.
    pub const SET_FALLBACK: u32 = 3;
    /// Exports a sealed packet as a stream.
    pub const GET_PAYLOAD: u32 = 4;
}

/// Field ids of the Actions.
pub mod action_field {
    /// Argument of `set_endpoint`, `set_interval` and `set_fallback`: the route slot.
    pub const INDEX: u32 = 1;
    /// Argument of `set_endpoint`: the telemetry profile slot.
    pub const PROFILE_INDEX: u32 = 2;
    /// Argument of `set_endpoint`: the primary endpoint, `""` for none.
    pub const ENDPOINT: u32 = 3;
    /// Argument of `set_interval`: seconds.
    pub const INTERVAL_S: u32 = 2;
    /// Argument of `set_fallback`: the fallback endpoint, `""` for none.
    pub const FALLBACK_ENDPOINT: u32 = 2;
    /// Argument of `get_payload`: the telemetry profile slot.
    pub const PAYLOAD_PROFILE_INDEX: u32 = 1;
}

/// Field ids of the status of the service.
pub mod status_field {
    /// Routes with a queue.
    pub const ACTIVE_ROUTES: u32 = 1;
    /// Packets queued over all routes.
    pub const TOTAL_QUEUE_DEPTH: u32 = 2;

    /// The telemetry profile index of a route, see [`route`].
    pub const PROFILE: u32 = 0;
    /// The primary endpoint, see [`route`].
    pub const ENDPOINT: u32 = 1;
    /// The fallback endpoint, absent without one, see [`route`].
    pub const FALLBACK: u32 = 2;
    /// The publication interval in seconds, see [`route`].
    pub const INTERVAL_S: u32 = 3;
    /// The state of the slot, see [`route`].
    pub const STATE: u32 = 4;
    /// Packets queued, see [`route`].
    pub const QUEUE_DEPTH: u32 = 5;
    /// Packets delivered, see [`route`].
    pub const DELIVERED: u32 = 6;
    /// Failed delivery attempts, see [`route`].
    pub const RETRIES: u32 = 7;
    /// The path of the next attempt, see [`route`].
    pub const ACTIVE_PATH: u32 = 8;

    /// The id of field `n` ([`PROFILE`], [`ENDPOINT`], [`FALLBACK`], [`INTERVAL_S`], [`STATE`],
    /// [`QUEUE_DEPTH`], [`DELIVERED`], [`RETRIES`] or [`ACTIVE_PATH`]) of route slot `slot`.
    pub const fn route(slot: usize, n: u32) -> u32 {
        10 * (slot as u32 + 1) + n
    }
}

/// Issue codes of the service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE;

    /// The routes cannot be read from storage; none is active.
    pub const STORAGE_UNAVAILABLE: u32 = FIRST_SERVICE_ISSUE;
    /// The telemetry profile of a route is not available; the route is suspended.
    pub const PROFILE_MISSING: u32 = FIRST_SERVICE_ISSUE + 1;
    /// The last attempt to build a packet failed.
    pub const PACKET_BUILD_FAILED: u32 = FIRST_SERVICE_ISSUE + 2;
    /// The stored routes use an endpoint scheme this build cannot deliver; no route is
    /// active until a route is removed or replaced.
    pub const UNSUPPORTED_ENDPOINT: u32 = FIRST_SERVICE_ISSUE + 4;
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
pub trait ConnectivityStore<const ROUTES: usize> {
    /// The committed routes; none when nothing was stored.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; an unreadable section is an error, not an empty set.
    async fn load(&self) -> Result<Routes<ROUTES>, StoreError>;

    /// Atomically persists `candidate` and publishes it.
    ///
    /// # Errors
    ///
    /// [`StoreError`]; nothing changed.
    async fn commit(&self, candidate: &Routes<ROUTES>) -> Result<(), StoreError>;
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
    P,
    const ROUTES: usize,
    const EXPORTS: usize,
    const BYTES: usize,
> {
    store: S,
    exporter: X,
    policy: P,
    state: &'a ConnectivityState<M, ROUTES>,
    // Serializes read-modify-write: no lost updates between Actions.
    edit: Mutex<M, ()>,
    #[cfg(feature = "packet-export")]
    exports: BlockingMutex<M, RefCell<Exports<EXPORTS, BYTES>>>,
}

impl<M: RawMutex, S, X, P, const R: usize, const E: usize, const B: usize> Service
    for ConnectivityService<'_, M, S, X, P, R, E, B>
{
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "connectivity";
}

impl<
    'a,
    M: RawMutex,
    S: ConnectivityStore<R>,
    X: PacketExporter,
    P: EndpointPolicy,
    const R: usize,
    const E: usize,
    const B: usize,
> ConnectivityService<'a, M, S, X, P, R, E, B>
{
    /// A service over `store`, building exports with `exporter` and reporting
    /// the runtime state of `state`.
    pub const fn new(store: S, exporter: X, policy: P, state: &'a ConnectivityState<M, R>) -> Self {
        Self {
            store,
            exporter,
            policy,
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
        change: impl FnOnce(&mut Routes<R>, &P) -> Result<(), RouteError>,
    ) -> Result<(), ActionError> {
        let _guard = self.edit.lock().await;
        let mut candidate = self.store.load().await?;
        change(&mut candidate, &self.policy).map_err(reject)?;
        // The whole candidate, not only the edited route: a set the build cannot
        // deliver is never stored. Deleting the offending route is always a valid edit.
        candidate.validate(&self.policy).map_err(reject)?;
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

struct SectionRef<'a, const R: usize>(&'a Routes<R>);

impl<C, const R: usize> minicbor::Encode<C> for SectionRef<'_, R> {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        minicbor::Encode::encode(self.0, e, ctx)
    }
}

impl<C, const R: usize> minicbor::CborLen<C> for SectionRef<'_, R> {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        minicbor::CborLen::cbor_len(self.0, ctx)
    }
}

fn reject(error: RouteError) -> ActionError {
    log::debug!(target: "connectivity", "Route change rejected (err={:?})", error);
    ActionError::Rejected(match error {
        RouteError::InvalidIndex => "invalid_index",
        RouteError::RouteNotFound => "route_not_found",
        RouteError::DuplicateEndpoint => "duplicate_endpoint",
        RouteError::InvalidEndpoint => "invalid_endpoint",
        RouteError::UnsupportedEndpoint => "unsupported_endpoint_scheme",
        RouteError::InvalidInterval => "invalid_interval",
        RouteError::InconsistentGeneration => "inconsistent_generation",
    })
}

fn arg(args: &[(u32, Value)], id: u32) -> Result<&Value, ActionError> {
    args.iter()
        .find(|(field, _)| *field == id)
        .map(|(_, value)| value)
        .ok_or(ActionError::InvalidArgument)
}

// An index that is no slot of any build is a refused index, not a malformed argument.
fn slot(args: &[(u32, Value)]) -> Result<usize, ActionError> {
    let index = arg(args, action_field::INDEX)?
        .as_u64()
        .ok_or(ActionError::InvalidArgument)?;
    usize::try_from(index).map_err(|_| ActionError::Rejected("invalid_index"))
}

fn profile_index(args: &[(u32, Value)], id: u32) -> Result<u8, ActionError> {
    let index = arg(args, id)?
        .as_u64()
        .ok_or(ActionError::InvalidArgument)?;
    u8::try_from(index).map_err(|_| ActionError::Rejected("invalid_profile_index"))
}

// The wire form of an absent endpoint: the empty string. An omitted argument means the same.
fn optional_endpoint(args: &[(u32, Value)], id: u32) -> Result<Option<&str>, ActionError> {
    match args.iter().find(|(field, _)| *field == id) {
        None => Ok(None),
        Some((_, value)) => match value.as_str().ok_or(ActionError::InvalidArgument)? {
            "" => Ok(None),
            endpoint => Ok(Some(endpoint)),
        },
    }
}

const INDEX: FieldDescriptor = FieldDescriptor::new(action_field::INDEX, "index", ValueKind::U64);

const STATES: &[EnumDescriptor] = &[EnumDescriptor::new(
    STATE_ENUM,
    &[
        EnumVariant::new(0, "empty"),
        EnumVariant::new(1, "idle"),
        EnumVariant::new(2, "delivering"),
        EnumVariant::new(3, "backoff"),
        EnumVariant::new(4, "profile_missing"),
    ],
)];

// The status schema is written out for the route count of the build: the nine fields of
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
                    status_field::route($slot, status_field::STATE),
                    concat!("route", $slot, "_state"),
                    ValueKind::Enum,
                )
                .with_enum(STATE_ENUM),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::PROFILE),
                    concat!("route", $slot, "_profile"),
                    ValueKind::U64,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::ENDPOINT),
                    concat!("route", $slot, "_endpoint"),
                    ValueKind::String,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::FALLBACK),
                    concat!("route", $slot, "_fallback"),
                    ValueKind::String,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::INTERVAL_S),
                    concat!("route", $slot, "_interval_s"),
                    ValueKind::U64,
                )
                .unit(Unit::S),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::QUEUE_DEPTH),
                    concat!("route", $slot, "_queue_depth"),
                    ValueKind::U64,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::DELIVERED),
                    concat!("route", $slot, "_delivered"),
                    ValueKind::U64,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::RETRIES),
                    concat!("route", $slot, "_retries"),
                    ValueKind::U64,
                ),
                FieldDescriptor::new(
                    status_field::route($slot, status_field::ACTIVE_PATH),
                    concat!("route", $slot, "_active_path"),
                    ValueKind::U64,
                ),
            )+
        ]
    };
}

/// The route count of a build, as a type: the status schema of a service with `N` route
/// slots. [`RouteSlots`] is implemented for 1 and 2 slots (two aggregates and nine fields per
/// slot: at most 20, the limit of status fields); any other count does not compile, so no schema
/// exists for a count the build does not use.
pub struct Slots<const N: usize>;

/// The static status schema of [`Slots`] of one size.
pub trait RouteSlots {
    /// The aggregate fields and nine fields per route slot.
    const SCHEMA: &'static [FieldDescriptor];
}

impl RouteSlots for Slots<1> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0);
}

impl RouteSlots for Slots<2> {
    const SCHEMA: &'static [FieldDescriptor] = status_schema!(0, 1);
}

const STATE_EMPTY: u32 = 0;

fn state_number(state: RouteState) -> u32 {
    match state {
        RouteState::Idle => 1,
        RouteState::Delivering => 2,
        RouteState::Backoff => 3,
        RouteState::ProfileMissing => 4,
    }
}

impl<
    M: RawMutex,
    S: ConnectivityStore<R>,
    X: PacketExporter,
    P: EndpointPolicy,
    const R: usize,
    const E: usize,
    const B: usize,
> Status for ConnectivityService<'_, M, S, X, P, R, E, B>
where
    Slots<R>: RouteSlots,
{
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = <Slots<R> as RouteSlots>::SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::STORAGE_UNAVAILABLE, "storage_unavailable"),
        IssueDescriptor::new(issue::PROFILE_MISSING, "profile_missing"),
        IssueDescriptor::new(issue::PACKET_BUILD_FAILED, "packet_build_failed"),
        IssueDescriptor::new(issue::DELIVERY_FAILING, "delivery_failing"),
        IssueDescriptor::new(issue::UNSUPPORTED_ENDPOINT, "unsupported_scheme"),
    ];

    async fn summary(&self) -> ServiceSummary {
        if self.state.is_unreadable() || self.state.is_unsupported() {
            return ServiceSummary::new(ServiceHealth::Error);
        }
        if self.state.routes().count() == 0 {
            // No route is a valid state: nothing is sent.
            return ServiceSummary::new(ServiceHealth::Unknown);
        }
        // Producing packets and delivering them are different things: a route
        // whose every path is failing is degraded even when packets are built.
        let degraded = self
            .state
            .views()
            .iter()
            .flatten()
            .any(|view| matches!(view.state, RouteState::ProfileMissing | RouteState::Backoff))
            || self.state.health().last_build_failure.is_some();
        let health = if degraded {
            ServiceHealth::Degraded
        } else {
            ServiceHealth::Ok
        };
        ServiceSummary::new(health)
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
        if self.state.is_unsupported() {
            push(issue::UNSUPPORTED_ENDPOINT);
        }
        let views = self.state.views();
        if views
            .iter()
            .flatten()
            .any(|view| view.state == RouteState::ProfileMissing)
        {
            push(issue::PROFILE_MISSING);
        }
        if self.state.health().last_build_failure.is_some() {
            push(issue::PACKET_BUILD_FAILED);
        }
        if views
            .iter()
            .flatten()
            .any(|view| view.state == RouteState::Backoff)
        {
            push(issue::DELIVERY_FAILING);
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let routes = self.state.routes();
        let views = self.state.views();
        // The runtime of an earlier route of a slot is not this route's.
        let runtime = |slot: usize| {
            let route = routes.get(slot)?;
            views[slot].filter(|view| view.generation == route.generation)
        };
        let active = (0..R)
            .filter(|slot| runtime(*slot).is_some_and(|view| view.queue_depth > 0))
            .count();
        let total: usize = (0..R)
            .filter_map(runtime)
            .map(|view| view.queue_depth)
            .sum();
        out.u64(status_field::ACTIVE_ROUTES, active as u64)?;
        out.u64(status_field::TOTAL_QUEUE_DEPTH, total as u64)?;
        for slot in 0..R {
            let field = |n| status_field::route(slot, n);
            let Some(route) = routes.get(slot) else {
                out.variant(field(status_field::STATE), STATE_EMPTY)?;
                continue;
            };
            let view = runtime(slot);
            out.variant(
                field(status_field::STATE),
                view.map_or(1, |view| state_number(view.state)),
            )?;
            out.u64(field(status_field::PROFILE), u64::from(route.profile_index))?;
            out.text(field(status_field::ENDPOINT), route.endpoint.as_str())?;
            if let Some(fallback) = &route.fallback {
                out.text(field(status_field::FALLBACK), fallback.as_str())?;
            }
            out.u64(field(status_field::INTERVAL_S), u64::from(route.interval_s))?;
            out.u64(
                field(status_field::QUEUE_DEPTH),
                view.map_or(0, |view| view.queue_depth as u64),
            )?;
            out.u64(
                field(status_field::DELIVERED),
                view.map_or(0, |view| u64::from(view.counters.delivered)),
            )?;
            out.u64(
                field(status_field::RETRIES),
                view.map_or(0, |view| u64::from(view.counters.retries)),
            )?;
            out.u64(
                field(status_field::ACTIVE_PATH),
                view.and_then(|view| view.active_path)
                    .map_or(0, |path| path as u64 + 1),
            )?;
        }
        Ok(())
    }
}

impl<
    M: RawMutex,
    S: ConnectivityStore<R>,
    X: PacketExporter,
    P: EndpointPolicy,
    const R: usize,
    const E: usize,
    const B: usize,
> Action for ConnectivityService<'_, M, S, X, P, R, E, B>
{
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::SET_ENDPOINT, "set_endpoint", ActionAccess::Mutate).args(&[
            INDEX,
            FieldDescriptor::new(action_field::PROFILE_INDEX, "profile_index", ValueKind::U64),
            FieldDescriptor::new(action_field::ENDPOINT, "endpoint", ValueKind::String),
        ]),
        ActionDescriptor::new(action::SET_INTERVAL, "set_interval", ActionAccess::Mutate).args(&[
            INDEX,
            FieldDescriptor::new(action_field::INTERVAL_S, "interval_s", ValueKind::U64)
                .unit(Unit::S),
        ]),
        ActionDescriptor::new(action::SET_FALLBACK, "set_fallback", ActionAccess::Mutate).args(&[
            INDEX,
            FieldDescriptor::new(
                action_field::FALLBACK_ENDPOINT,
                "endpoint",
                ValueKind::String,
            ),
        ]),
        #[cfg(feature = "packet-export")]
        ActionDescriptor::new(action::GET_PAYLOAD, "get_payload", ActionAccess::Read)
            .args(&[FieldDescriptor::new(
                action_field::PAYLOAD_PROFILE_INDEX,
                "profile_index",
                ValueKind::U64,
            )])
            .format(ResultFormat::Stream),
    ];

    #[cfg_attr(not(feature = "packet-export"), allow(unused_variables))]
    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            action::SET_ENDPOINT => {
                let index = slot(args)?;
                let profile = profile_index(args, action_field::PROFILE_INDEX)?;
                let endpoint = optional_endpoint(args, action_field::ENDPOINT)?;
                // A route needs a profile to publish; deleting one does not.
                if endpoint.is_some() && self.exporter.resolve(profile).is_none() {
                    return Err(ActionError::Rejected("invalid_profile_index"));
                }
                self.edit(|routes, policy| routes.set_endpoint(index, profile, endpoint, policy))
                    .await
            }
            action::SET_INTERVAL => {
                let index = slot(args)?;
                let seconds = arg(args, action_field::INTERVAL_S)?
                    .as_u64()
                    .ok_or(ActionError::InvalidArgument)?;
                let seconds = u32::try_from(seconds)
                    .map_err(|_| ActionError::Rejected("invalid_interval"))?;
                self.edit(|routes, _| routes.set_interval(index, seconds))
                    .await
            }
            action::SET_FALLBACK => {
                let index = slot(args)?;
                let endpoint = optional_endpoint(args, action_field::FALLBACK_ENDPOINT)?;
                self.edit(|routes, policy| routes.set_fallback(index, endpoint, policy))
                    .await
            }
            #[cfg(feature = "packet-export")]
            action::GET_PAYLOAD => {
                let profile = profile_index(args, action_field::PAYLOAD_PROFILE_INDEX)?;
                // An abandoned stream must not keep its slot past its lifetime.
                self.expire();
                let stamp = self
                    .exporter
                    .resolve(profile)
                    .ok_or(ActionError::Rejected("invalid_profile_index"))?;
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
            if exporter.resolve(stamp.profile_index).as_ref() != Some(stamp) {
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
