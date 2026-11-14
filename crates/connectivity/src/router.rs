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
//! The state machine of publication and delivery, without I/O.
//!
//! [`Router`] keeps, for every configured route, its own publication
//! deadline, one bounded FIFO outbox of [`PacketHandle`]s shared by the
//! primary and its fallbacks, and the health of every delivery path. It owns
//! the [`PacketPool`]. The asynchronous driver ([`crate::publisher`]) feeds
//! it time, built packets and delivery results; everything that decides
//! lives here and is tested without a network.
//!
//! # Rules
//!
//! * Every route publishes on its own `interval_s`. The first publication of
//!   a new route is due immediately, then `interval_s` after each attempt;
//!   restarting a failed transport never resets the cadence. An interval edit
//!   moves the next deadline to `now + interval` without publishing an extra
//!   packet and without clearing the queue.
//! * The outbox of a route is shared by all its paths: a packet is one
//!   delivery obligation. A full outbox skips the new packet (counted); older
//!   packets are never evicted.
//! * The primary is preferred whenever it is eligible; after a failure the
//!   next eligible fallback is tried at once, without waiting for the
//!   primary's cooldown. Every path has its own cooldown.
//! * Only `202` completes a packet, from any path. A permanent rejection
//!   drops the packet and does not offer it to other paths. Retries send the
//!   same bytes.
//! * A change of the destination set of a route or of the policy of its
//!   profile drops the queued packets of that route; other routes are never
//!   touched. A route whose profile cannot be resolved to the bound
//!   incarnation is suspended.

use heapless::{Deque, Vec};
use rohi_telemetry::ProfileStamp;
use rohi_time::{Backoff, MonotonicTimestamp};

use crate::config::{MAX_FALLBACKS, MAX_ROUTES, RouteId, RouteSet};
use crate::pool::{PacketHandle, PacketPool};
use crate::transport::{Disposition, TransportAck, TransportError};

/// Number of delivery paths of a route: the primary and its fallbacks.
pub const MAX_PATHS: usize = 1 + MAX_FALLBACKS;
/// First cooldown of a path after a transient failure, in milliseconds.
pub const BACKOFF_INITIAL_MS: u32 = 5_000;
/// Longest cooldown after repeated transient failures, in milliseconds.
pub const BACKOFF_MAX_MS: u32 = 5 * 60_000;
/// Cooldown of a path after `401`/`403`, in milliseconds: a quick retry cannot
/// help and must not become a hot loop.
pub const AUTHORIZATION_BACKOFF_MS: u64 = 10 * 60_000;

/// Counters of one route. They never contain packet content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteCounters {
    /// Packets queued for delivery.
    pub published: u32,
    /// Packets acknowledged with `202`.
    pub delivered: u32,
    /// Failed delivery attempts.
    pub retries: u32,
    /// Packets delivered by a fallback after an earlier path failed.
    pub failovers: u32,
    /// Packets not queued because the outbox was full.
    pub skipped_full: u32,
    /// Packets not built because the pool was exhausted.
    pub pool_exhausted: u32,
    /// Packets dropped by a permanent rejection.
    pub rejected: u32,
    /// Queued packets dropped because the policy or destinations changed.
    pub invalidated: u32,
}

/// What a route is doing, for status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteState {
    /// Waiting for the next publication.
    Idle,
    /// Packets are queued and a path can send them.
    Delivering,
    /// Packets are queued and every path is cooling down.
    Backoff,
    /// The bound profile no longer exists or was recreated: suspended.
    ProfileMissing,
}

/// Why a packet was not produced for a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The telemetry side could not build a packet.
    BuildFailed,
    /// The pool has no free slot.
    PoolExhausted,
}

/// The result of admitting a packet to a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Queued.
    Queued,
    /// The outbox is full; the packet was skipped.
    OutboxFull,
}

/// A delivery attempt to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    /// Index of the route in the router.
    pub route: usize,
    /// 0 is the primary, `n` the fallback `n - 1`.
    pub path: usize,
    /// The packet at the head of the outbox.
    pub packet: PacketHandle,
}

#[derive(Clone, Copy)]
struct Path {
    blocked_until: MonotonicTimestamp,
    backoff: Backoff,
    failed_since_success: bool,
}

impl Path {
    const fn new() -> Self {
        Self {
            blocked_until: MonotonicTimestamp::from_millis(0),
            backoff: Backoff::new(BACKOFF_INITIAL_MS, BACKOFF_MAX_MS),
            failed_since_success: false,
        }
    }
}

struct Runtime<const DEPTH: usize> {
    id: RouteId,
    generation: u32,
    destinations: u32,
    interval_s: u32,
    next_publish_at: MonotonicTimestamp,
    outbox: Deque<PacketHandle, DEPTH>,
    paths: [Path; MAX_PATHS],
    counters: RouteCounters,
    profile_ok: bool,
    failed_before_delivery: bool,
}

/// A read-only view of one route, for status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteView {
    /// The route id.
    pub id: RouteId,
    /// What the route is doing.
    pub state: RouteState,
    /// Packets queued.
    pub queue_depth: usize,
    /// Counters.
    pub counters: RouteCounters,
    /// The path the next attempt uses (0 primary), `None` when none can.
    pub active_path: Option<usize>,
}

/// The state of all routes over a pool of `SLOTS` packets of `BYTES` bytes
/// with outboxes of `DEPTH` packets.
pub struct Router<const DEPTH: usize, const SLOTS: usize, const BYTES: usize> {
    pool: PacketPool<SLOTS, BYTES>,
    routes: Vec<Runtime<DEPTH>, MAX_ROUTES>,
}

impl<const DEPTH: usize, const SLOTS: usize, const BYTES: usize> Default
    for Router<DEPTH, SLOTS, BYTES>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const DEPTH: usize, const SLOTS: usize, const BYTES: usize> Router<DEPTH, SLOTS, BYTES> {
    /// No route.
    pub const fn new() -> Self {
        Self {
            pool: PacketPool::new(),
            routes: Vec::new(),
        }
    }

    /// The pool, to build packets in.
    pub fn pool_mut(&mut self) -> &mut PacketPool<SLOTS, BYTES> {
        &mut self.pool
    }

    /// The pool.
    pub fn pool(&self) -> &PacketPool<SLOTS, BYTES> {
        &self.pool
    }

    fn drop_queue(pool: &mut PacketPool<SLOTS, BYTES>, runtime: &mut Runtime<DEPTH>) -> u32 {
        let mut dropped = 0;
        while let Some(handle) = runtime.outbox.pop_front() {
            pool.release(handle);
            dropped += 1;
        }
        dropped
    }

    /// Brings the runtime in line with the effective `config`: removed routes
    /// are dropped with everything they hold, new ones start, an interval
    /// edit moves the deadline, a destination edit or an unresolvable profile
    /// flushes the queue of that route only.
    ///
    /// `resolve` gives the current incarnation of a profile.
    pub fn sync(
        &mut self,
        config: &RouteSet,
        resolve: impl Fn(&str) -> Option<u32>,
        now: MonotonicTimestamp,
    ) {
        let mut index = 0;
        while index < self.routes.len() {
            let runtime = &mut self.routes[index];
            let keep = config
                .get(runtime.id.as_str())
                .is_some_and(|route| route.generation == runtime.generation);
            if keep {
                index += 1;
            } else {
                Self::drop_queue(&mut self.pool, runtime);
                self.routes.swap_remove(index);
            }
        }
        for route in &config.routes {
            let ok = resolve(route.profile.id.as_str()) == Some(route.profile.incarnation);
            if let Some(runtime) = self
                .routes
                .iter_mut()
                .find(|runtime| runtime.id == route.id)
            {
                if runtime.destinations != route.destinations {
                    runtime.counters.invalidated += Self::drop_queue(&mut self.pool, runtime);
                    runtime.paths = [Path::new(); MAX_PATHS];
                    runtime.destinations = route.destinations;
                }
                if runtime.interval_s != route.interval_s {
                    runtime.interval_s = route.interval_s;
                    runtime.next_publish_at = after(now, route.interval_s);
                }
                if !ok {
                    runtime.counters.invalidated += Self::drop_queue(&mut self.pool, runtime);
                }
                runtime.profile_ok = ok;
            } else {
                let _ = self.routes.push(Runtime {
                    id: route.id.clone(),
                    generation: route.generation,
                    destinations: route.destinations,
                    interval_s: route.interval_s,
                    // A new route publishes as soon as it is enabled.
                    next_publish_at: now,
                    outbox: Deque::new(),
                    paths: [Path::new(); MAX_PATHS],
                    counters: RouteCounters::default(),
                    profile_ok: ok,
                    failed_before_delivery: false,
                });
            }
        }
    }

    /// The routes due for a publication at `now`.
    pub fn due(&self, now: MonotonicTimestamp) -> Vec<usize, MAX_ROUTES> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, runtime)| runtime.profile_ok && runtime.next_publish_at <= now)
            .map(|(index, _)| index)
            .collect()
    }

    /// The id of route `index`.
    pub fn id(&self, index: usize) -> Option<&RouteId> {
        self.routes.get(index).map(|runtime| &runtime.id)
    }

    /// Queues `packet` for route `index` (taking a reference) and schedules the
    /// next publication of the route.
    pub fn admit(&mut self, index: usize, packet: PacketHandle, now: MonotonicTimestamp) -> Admit {
        let Some(runtime) = self.routes.get_mut(index) else {
            return Admit::OutboxFull;
        };
        runtime.next_publish_at = after(now, runtime.interval_s);
        if runtime.outbox.is_full() {
            runtime.counters.skipped_full += 1;
            return Admit::OutboxFull;
        }
        if !self.pool.retain(packet) {
            return Admit::OutboxFull;
        }
        let _ = runtime.outbox.push_back(packet);
        runtime.counters.published += 1;
        Admit::Queued
    }

    /// No packet was produced for route `index`; its next publication is
    /// scheduled as usual and the reason counted.
    pub fn skip(&mut self, index: usize, reason: SkipReason, now: MonotonicTimestamp) {
        if let Some(runtime) = self.routes.get_mut(index) {
            runtime.next_publish_at = after(now, runtime.interval_s);
            if reason == SkipReason::PoolExhausted {
                runtime.counters.pool_exhausted += 1;
            }
        }
    }

    /// The next delivery attempt: the head packet of a route with the
    /// highest-priority path that is not cooling down. `endpoints` is the
    /// number of paths the route has (1 + its fallbacks).
    pub fn next_delivery(&self, config: &RouteSet, now: MonotonicTimestamp) -> Option<Delivery> {
        for (index, runtime) in self.routes.iter().enumerate() {
            let Some(packet) = runtime.outbox.front() else {
                continue;
            };
            let Some(route) = config.get(runtime.id.as_str()) else {
                continue;
            };
            let paths = 1 + route.fallbacks.len();
            if let Some(path) =
                (0..paths.min(MAX_PATHS)).find(|p| runtime.paths[*p].blocked_until <= now)
            {
                return Some(Delivery {
                    route: index,
                    path,
                    packet: *packet,
                });
            }
        }
        None
    }

    /// Applies the result of the attempt `delivery`.
    pub fn complete(
        &mut self,
        delivery: Delivery,
        result: Result<TransportAck, TransportError>,
        now: MonotonicTimestamp,
    ) {
        let Some(runtime) = self.routes.get_mut(delivery.route) else {
            return;
        };
        // The queue may have been flushed while the attempt was in flight.
        if runtime.outbox.front() != Some(&delivery.packet) {
            return;
        }
        let path = &mut runtime.paths[delivery.path];
        match result {
            Ok(_) => {
                path.backoff.reset();
                path.blocked_until = MonotonicTimestamp::from_millis(0);
                path.failed_since_success = false;
                if delivery.path > 0 || runtime.failed_before_delivery {
                    runtime.counters.failovers += u32::from(delivery.path > 0);
                }
                runtime.failed_before_delivery = false;
                runtime.counters.delivered += 1;
                runtime.outbox.pop_front();
                self.pool.release(delivery.packet);
            }
            Err(error) => match error.disposition() {
                Disposition::Permanent => {
                    runtime.counters.rejected += 1;
                    runtime.failed_before_delivery = false;
                    runtime.outbox.pop_front();
                    self.pool.release(delivery.packet);
                }
                Disposition::Unauthorized => {
                    runtime.counters.retries += 1;
                    runtime.failed_before_delivery = true;
                    path.failed_since_success = true;
                    path.blocked_until = after_ms(now, AUTHORIZATION_BACKOFF_MS);
                }
                Disposition::Transient => {
                    runtime.counters.retries += 1;
                    runtime.failed_before_delivery = true;
                    path.failed_since_success = true;
                    path.blocked_until = after_ms(now, u64::from(path.backoff.next_delay_ms()));
                }
            },
        }
    }

    /// Drops every queued packet whose policy is no longer current. Other
    /// packets and routes are untouched.
    pub fn invalidate(&mut self, is_current: impl Fn(&ProfileStamp) -> bool) {
        for runtime in &mut self.routes {
            let mut kept: Deque<PacketHandle, DEPTH> = Deque::new();
            while let Some(handle) = runtime.outbox.pop_front() {
                let current = self
                    .pool
                    .get(handle)
                    .is_some_and(|(stamp, _)| is_current(stamp));
                if current {
                    let _ = kept.push_back(handle);
                } else {
                    self.pool.release(handle);
                    runtime.counters.invalidated += 1;
                }
            }
            runtime.outbox = kept;
        }
    }

    /// Whether the head packet of the attempt is still current: checked
    /// immediately before a transport attempt starts.
    pub fn head_is_current(
        &self,
        delivery: &Delivery,
        is_current: impl Fn(&ProfileStamp) -> bool,
    ) -> bool {
        self.pool
            .get(delivery.packet)
            .is_some_and(|(stamp, _)| is_current(stamp))
    }

    /// The earliest moment something becomes due: a publication, or a cooling
    /// path of a route with queued packets. `None` when nothing is scheduled.
    pub fn next_wakeup(&self, config: &RouteSet) -> Option<MonotonicTimestamp> {
        let mut earliest: Option<MonotonicTimestamp> = None;
        let mut consider = |at: MonotonicTimestamp| {
            earliest = Some(earliest.map_or(at, |current| current.min(at)));
        };
        for runtime in &self.routes {
            if runtime.profile_ok {
                consider(runtime.next_publish_at);
            }
            if !runtime.outbox.is_empty() {
                let paths = config
                    .get(runtime.id.as_str())
                    .map_or(1, |route| 1 + route.fallbacks.len())
                    .min(MAX_PATHS);
                if let Some(at) = runtime.paths[..paths].iter().map(|p| p.blocked_until).min() {
                    consider(at);
                }
            }
        }
        earliest
    }

    /// The state of every route, in configuration order of creation.
    pub fn views(&self, config: &RouteSet, now: MonotonicTimestamp) -> Vec<RouteView, MAX_ROUTES> {
        self.routes
            .iter()
            .map(|runtime| {
                let paths = config
                    .get(runtime.id.as_str())
                    .map_or(1, |route| 1 + route.fallbacks.len())
                    .min(MAX_PATHS);
                let active = (0..paths).find(|p| runtime.paths[*p].blocked_until <= now);
                let state = if !runtime.profile_ok {
                    RouteState::ProfileMissing
                } else if runtime.outbox.is_empty() {
                    RouteState::Idle
                } else if active.is_some() {
                    RouteState::Delivering
                } else {
                    RouteState::Backoff
                };
                RouteView {
                    id: runtime.id.clone(),
                    state,
                    queue_depth: runtime.outbox.len(),
                    counters: runtime.counters,
                    active_path: active,
                }
            })
            .collect()
    }

    /// Total packets queued over all routes.
    pub fn queued(&self) -> usize {
        self.routes.iter().map(|runtime| runtime.outbox.len()).sum()
    }
}

fn after(now: MonotonicTimestamp, interval_s: u32) -> MonotonicTimestamp {
    after_ms(now, u64::from(interval_s) * 1_000)
}

fn after_ms(now: MonotonicTimestamp, ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(now.millis.saturating_add(ms))
}

#[cfg(test)]
mod tests;
