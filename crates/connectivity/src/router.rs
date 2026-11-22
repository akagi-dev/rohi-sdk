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
//! [`Router`] keeps, for every route **slot** that holds a route, its own
//! publication deadline, one bounded FIFO outbox of [`PacketHandle`]s shared by
//! the primary and the fallback, and the health of both delivery paths. It
//! owns the [`PacketPool`]. The asynchronous driver ([`crate::publisher`])
//! feeds it time, built packets and delivery results; everything that decides
//! lives here and is tested without a network.
//!
//! The runtime of slot `i` is the `i`-th entry of a fixed array: a route is
//! addressed by its index everywhere (`due`, `admit`, [`Delivery::route`],
//! [`RouteView`]) and deleting the route of one slot never moves another.
//!
//! # Rules
//!
//! * Every route publishes on its own `interval_s`. The first publication of
//!   a new route is due immediately, then `interval_s` after each attempt;
//!   restarting a failed transport never resets the cadence. An interval edit
//!   moves the next deadline to `now + interval` without publishing an extra
//!   packet and without clearing the queue.
//! * The outbox of a route is shared by its paths: a packet is one delivery
//!   obligation. A full outbox skips the new packet (counted); older packets
//!   are never evicted.
//! * The primary is preferred whenever it is eligible; after a failure the
//!   fallback is tried at once, without waiting for the primary's cooldown.
//!   Every path has its own cooldown.
//! * Only `202` completes a packet, from any path. A permanent rejection
//!   drops the packet and does not offer it to the other path. Retries send
//!   the same bytes.
//! * A route that is deleted or whose [`generation`](crate::Route::generation)
//!   changed (a new endpoint or profile) loses its whole runtime: queue,
//!   backoff and counters. A change of the fallback or of the policy of the
//!   profile drops the queued packets of that route only; other routes are
//!   never touched. A route whose profile slot cannot be resolved is
//!   suspended.

use heapless::{Deque, Vec};
use rohi_telemetry::ProfileStamp;
use rohi_time::{Backoff, MonotonicTimestamp};

use crate::config::Routes;
use crate::pool::{PacketHandle, PacketPool};
use crate::transport::{Disposition, TransportAck, TransportError};

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
    /// Packets acknowledged by a path ([`AckKind`](crate::AckKind) says what that promises).
    pub delivered: u32,
    /// Failed delivery attempts.
    pub retries: u32,
    /// Packets delivered by the fallback after the primary failed.
    pub failovers: u32,
    /// Packets not queued because the outbox was full.
    pub skipped_full: u32,
    /// Packets not built because the pool was exhausted.
    pub pool_exhausted: u32,
    /// Packets dropped by a permanent rejection.
    pub rejected: u32,
    /// Packets dropped because no path of the route can carry them.
    pub incompatible: u32,
    /// Queued packets dropped because the policy or the fallback changed.
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
    /// The route's telemetry profile slot is not available: suspended.
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
    /// The route slot.
    pub route: usize,
    /// The [generation](crate::Route::generation) of the route the attempt
    /// belongs to: a result for another generation of the slot is ignored.
    pub generation: u32,
    /// 0 is the primary, 1 the fallback.
    pub path: usize,
    /// The packet at the head of the outbox.
    pub packet: PacketHandle,
}

/// The paths of a route: the primary and the fallback.
const PATHS: usize = 2;

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
    // Paths this route has: the primary, and the fallback when it has one.
    path_count: usize,
    // Bit `p`: path `p` cannot carry the packet at the head of the outbox.
    incompatible: u8,
    generation: u32,
    destinations: u32,
    interval_s: u32,
    next_publish_at: MonotonicTimestamp,
    outbox: Deque<PacketHandle, DEPTH>,
    paths: [Path; PATHS],
    counters: RouteCounters,
    profile_ok: bool,
    failed_before_delivery: bool,
}

impl<const DEPTH: usize> Runtime<DEPTH> {
    // A path that can carry the head packet: it is not cooling down and has not
    // declared the packet incompatible. The primary is preferred.
    fn usable_path(&self, now: MonotonicTimestamp) -> Option<usize> {
        (0..self.path_count)
            .find(|p| self.paths[*p].blocked_until <= now && self.incompatible & (1 << p) == 0)
    }

    // The paths that could still carry the head packet.
    fn candidates(&self) -> impl Iterator<Item = &Path> {
        self.paths[..self.path_count]
            .iter()
            .enumerate()
            .filter(|(p, _)| self.incompatible & (1 << p) == 0)
            .map(|(_, path)| path)
    }
}

/// A read-only view of the runtime of one route, for status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteView {
    /// The [generation](crate::Route::generation) of the route it describes: a
    /// view of an earlier route of the slot is not this route's.
    pub generation: u32,
    /// What the route is doing.
    pub state: RouteState,
    /// Packets queued.
    pub queue_depth: usize,
    /// Counters.
    pub counters: RouteCounters,
    /// The path the next attempt uses (0 primary, 1 fallback), `None` when
    /// none can.
    pub active_path: Option<usize>,
}

/// The state of `ROUTES` route slots, over a pool of `SLOTS` packets of `BYTES`
/// bytes with outboxes of `DEPTH` packets. The capacities are the storage:
/// there is no unused room for a third route.
pub struct Router<const ROUTES: usize, const DEPTH: usize, const SLOTS: usize, const BYTES: usize> {
    pool: PacketPool<SLOTS, BYTES>,
    routes: [Option<Runtime<DEPTH>>; ROUTES],
    // Where the next delivery search starts: it moves past a route after every
    // attempt, so a busy route cannot starve the other.
    cursor: usize,
}

impl<const R: usize, const DEPTH: usize, const SLOTS: usize, const BYTES: usize> Default
    for Router<R, DEPTH, SLOTS, BYTES>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<const R: usize, const DEPTH: usize, const SLOTS: usize, const BYTES: usize>
    Router<R, DEPTH, SLOTS, BYTES>
{
    /// No route.
    pub const fn new() -> Self {
        Self {
            pool: PacketPool::new(),
            routes: [const { None }; R],
            cursor: 0,
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
        runtime.incompatible = 0;
        while let Some(handle) = runtime.outbox.pop_front() {
            pool.release(handle);
            dropped += 1;
        }
        dropped
    }

    fn runtime(&self, index: usize) -> Option<&Runtime<DEPTH>> {
        self.routes.get(index)?.as_ref()
    }

    fn runtime_mut(&mut self, index: usize) -> Option<&mut Runtime<DEPTH>> {
        self.routes.get_mut(index)?.as_mut()
    }

    /// Brings the runtime in line with the effective `config`: the runtime of
    /// a deleted route, or of a slot whose route has another generation, is
    /// dropped with everything it holds (queue, backoff, counters) and the
    /// slot starts afresh; an interval edit moves the deadline, a fallback
    /// edit or an unavailable profile flushes the queue of that route only.
    ///
    /// `profile_exists` says whether a telemetry profile slot can be resolved.
    pub fn sync(
        &mut self,
        config: &Routes<R>,
        profile_exists: impl Fn(u8) -> bool,
        now: MonotonicTimestamp,
    ) {
        for (index, slot) in self.routes.iter_mut().enumerate() {
            let Some(route) = config.get(index) else {
                if let Some(runtime) = slot {
                    Self::drop_queue(&mut self.pool, runtime);
                }
                *slot = None;
                continue;
            };
            if let Some(runtime) = slot
                && runtime.generation != route.generation
            {
                Self::drop_queue(&mut self.pool, runtime);
                *slot = None;
            }
            let ok = profile_exists(route.profile_index);
            match slot {
                Some(runtime) => {
                    if runtime.destinations != route.destinations {
                        runtime.counters.invalidated += Self::drop_queue(&mut self.pool, runtime);
                        runtime.paths = [Path::new(); PATHS];
                        runtime.destinations = route.destinations;
                        runtime.path_count = path_count(route.fallback.is_some());
                    }
                    if runtime.interval_s != route.interval_s {
                        runtime.interval_s = route.interval_s;
                        runtime.next_publish_at = after(now, route.interval_s);
                    }
                    if !ok {
                        runtime.counters.invalidated += Self::drop_queue(&mut self.pool, runtime);
                    }
                    runtime.profile_ok = ok;
                }
                None => {
                    *slot = Some(Runtime {
                        path_count: path_count(route.fallback.is_some()),
                        incompatible: 0,
                        generation: route.generation,
                        destinations: route.destinations,
                        interval_s: route.interval_s,
                        // A new route publishes as soon as it is set.
                        next_publish_at: now,
                        outbox: Deque::new(),
                        paths: [Path::new(); PATHS],
                        counters: RouteCounters::default(),
                        profile_ok: ok,
                        failed_before_delivery: false,
                    });
                }
            }
        }
    }

    /// The route slots due for a publication at `now`.
    pub fn due(&self, now: MonotonicTimestamp) -> Vec<usize, R> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, runtime)| {
                runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.profile_ok && runtime.next_publish_at <= now)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Queues `packet` for route `index` (taking a reference) and schedules the
    /// next publication of the route.
    pub fn admit(&mut self, index: usize, packet: PacketHandle, now: MonotonicTimestamp) -> Admit {
        let Some(runtime) = self.routes.get_mut(index).and_then(Option::as_mut) else {
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
        if let Some(runtime) = self.runtime_mut(index) {
            runtime.next_publish_at = after(now, runtime.interval_s);
            if reason == SkipReason::PoolExhausted {
                runtime.counters.pool_exhausted += 1;
            }
        }
    }

    /// The next delivery attempt: the head packet of a route with the
    /// highest-priority path that is not cooling down.
    pub fn next_delivery(&self, config: &Routes<R>, now: MonotonicTimestamp) -> Option<Delivery> {
        for offset in 0..R {
            let index = (self.cursor + offset) % R;
            let Some(runtime) = self.runtime(index) else {
                continue;
            };
            let Some(packet) = runtime.outbox.front() else {
                continue;
            };
            let Some(route) = config.get(index) else {
                continue;
            };
            if route.generation != runtime.generation {
                continue;
            }
            if let Some(path) = runtime.usable_path(now) {
                return Some(Delivery {
                    route: index,
                    generation: runtime.generation,
                    path,
                    packet: *packet,
                });
            }
        }
        None
    }

    /// Applies the result of the attempt `delivery`. A result for a route that
    /// was deleted or replaced while the attempt ran is ignored.
    pub fn complete(
        &mut self,
        delivery: Delivery,
        result: Result<TransportAck, TransportError>,
        now: MonotonicTimestamp,
    ) {
        // The attempt is over, whatever its result: the next search starts with
        // the following route.
        self.cursor = delivery.route + 1;
        let Some(runtime) = self.routes.get_mut(delivery.route).and_then(Option::as_mut) else {
            return;
        };
        // The slot may hold another route, or its queue may have been flushed,
        // while the attempt was in flight.
        if runtime.generation != delivery.generation
            || runtime.outbox.front() != Some(&delivery.packet)
        {
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
                runtime.incompatible = 0;
                self.pool.release(delivery.packet);
            }
            Err(error) => match error.disposition() {
                Disposition::Permanent => {
                    runtime.counters.rejected += 1;
                    runtime.failed_before_delivery = false;
                    runtime.outbox.pop_front();
                    runtime.incompatible = 0;
                    self.pool.release(delivery.packet);
                }
                Disposition::Incompatible => {
                    runtime.incompatible |= 1 << delivery.path;
                    let all = (1u8 << runtime.path_count) - 1;
                    if runtime.incompatible & all == all {
                        // No path can carry it: the packet ends here, once, and later
                        // packets start with every path open.
                        runtime.counters.incompatible += 1;
                        runtime.failed_before_delivery = false;
                        runtime.incompatible = 0;
                        runtime.outbox.pop_front();
                        self.pool.release(delivery.packet);
                    }
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
        for runtime in self.routes.iter_mut().flatten() {
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
            if kept.len() != runtime.outbox.len() {
                runtime.incompatible = 0;
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
    pub fn next_wakeup(&self, _config: &Routes<R>) -> Option<MonotonicTimestamp> {
        let mut earliest: Option<MonotonicTimestamp> = None;
        let mut consider = |at: MonotonicTimestamp| {
            earliest = Some(earliest.map_or(at, |current| current.min(at)));
        };
        for runtime in self.routes.iter().flatten() {
            if runtime.profile_ok {
                consider(runtime.next_publish_at);
            }
            if !runtime.outbox.is_empty()
                && let Some(at) = runtime.candidates().map(|p| p.blocked_until).min()
            {
                consider(at);
            }
        }
        earliest
    }

    /// The runtime state of every slot: `None` for a slot without a route.
    pub fn views(&self, _config: &Routes<R>, now: MonotonicTimestamp) -> [Option<RouteView>; R] {
        core::array::from_fn(|index| {
            let runtime = self.runtime(index)?;
            let active = runtime.usable_path(now);
            let state = if !runtime.profile_ok {
                RouteState::ProfileMissing
            } else if runtime.outbox.is_empty() {
                RouteState::Idle
            } else if active.is_some() {
                RouteState::Delivering
            } else {
                RouteState::Backoff
            };
            Some(RouteView {
                generation: runtime.generation,
                state,
                queue_depth: runtime.outbox.len(),
                counters: runtime.counters,
                active_path: active,
            })
        })
    }

    /// Total packets queued over all routes.
    pub fn queued(&self) -> usize {
        self.routes
            .iter()
            .flatten()
            .map(|runtime| runtime.outbox.len())
            .sum()
    }
}

// The primary, and the fallback when the route has one.
fn path_count(has_fallback: bool) -> usize {
    1 + usize::from(has_fallback)
}

fn after(now: MonotonicTimestamp, interval_s: u32) -> MonotonicTimestamp {
    after_ms(now, u64::from(interval_s) * 1_000)
}

fn after_ms(now: MonotonicTimestamp, ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(now.millis.saturating_add(ms))
}

#[cfg(test)]
mod tests;
