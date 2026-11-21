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
//! The publisher: the task that turns routes into deliveries.
//!
//! [`Publisher::run`] loops: reconcile the [`Router`] with the effective
//! route slots, publish what is due (one capture, one sealed packet per distinct
//! profile stamp, shared by the due routes), make at most one delivery
//! attempt, report the status and sleep until the next deadline or change.
//! All decisions are in [`Router`]; this file only does the waiting and the
//! I/O. No lock is held across a capture, a build or a transport attempt.
//!
//! A delivery attempt is raced against route and policy changes: when either
//! changes during an attempt, the attempt is dropped (the transport contract
//! makes that safe) and stale queued packets are invalidated before any new
//! send starts.

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::Vec;
use rohi_telemetry::ProfileStamp;
use rohi_time::{Clock, MonotonicTimestamp};

use crate::config::Routes;
use crate::pool::PacketHandle;
use crate::provider::{BuildFailure, PacketProvider};
use crate::router::{Admit, Delivery, Router, SkipReason};
use crate::state::{ConnectivityState, Health};
use crate::transport::{Disposition, PacketTransport, TransportAck, TransportError};

/// Drives the routes of a device.
pub struct Publisher<
    'a,
    M: RawMutex,
    P: PacketProvider,
    T: PacketTransport,
    D: DelayNs,
    C: Clock,
    const ROUTES: usize,
    const DEPTH: usize,
    const SLOTS: usize,
    const BYTES: usize,
> {
    state: &'a ConnectivityState<M, ROUTES>,
    provider: P,
    transport: T,
    delay: D,
    clock: C,
    router: Router<ROUTES, DEPTH, SLOTS, BYTES>,
    health: Health,
}

impl<
    'a,
    M: RawMutex,
    P: PacketProvider,
    T: PacketTransport,
    D: DelayNs,
    C: Clock,
    const R: usize,
    const DEPTH: usize,
    const SLOTS: usize,
    const BYTES: usize,
> Publisher<'a, M, P, T, D, C, R, DEPTH, SLOTS, BYTES>
{
    /// A publisher over the effective routes in `state`.
    pub const fn new(
        state: &'a ConnectivityState<M, R>,
        provider: P,
        transport: T,
        delay: D,
        clock: C,
    ) -> Self {
        Self {
            state,
            provider,
            transport,
            delay,
            clock,
            router: Router::new(),
            health: Health {
                last_build_failure: None,
                captures: 0,
                built: 0,
            },
        }
    }

    /// The router, for inspection.
    pub fn router(&self) -> &Router<R, DEPTH, SLOTS, BYTES> {
        &self.router
    }

    /// Runs forever.
    pub async fn run(&mut self) -> ! {
        log::info!(target: "connectivity", "Publisher started");
        loop {
            self.step().await;
            self.sleep().await;
        }
    }

    /// One pass: reconcile, publish what is due, make at most one delivery
    /// attempt and report. Not meant to be cancelled in the middle.
    pub async fn step(&mut self) {
        let now = self.clock.now();
        let config = self.state.routes();
        self.reconcile(&config, now);

        let due = self.router.due(now);
        if !due.is_empty() {
            self.publish(&config, &due, now).await;
        }
        let config = self.state.routes();
        self.reconcile(&config, self.clock.now());
        self.deliver(&config).await;

        let now = self.clock.now();
        let config = self.state.routes();
        self.reconcile(&config, now);
        self.state
            .report(self.router.views(&config, now), self.health.clone());
    }

    fn reconcile(&mut self, config: &Routes<R>, now: MonotonicTimestamp) {
        let provider = &self.provider;
        self.router
            .sync(config, |index| provider.resolve(index).is_some(), now);
        self.router.invalidate(|stamp| provider.is_current(stamp));
    }

    async fn publish(&mut self, config: &Routes<R>, due: &[usize], now: MonotonicTimestamp) {
        // The publication context is fixed before anything is awaited: each due
        // route slot with the stamp of the profile slot it publishes. A profile that
        // changes during the capture or the build never joins this execution;
        // the build refuses a stamp that is no longer current and the change
        // takes effect in the next one.
        let mut context: Vec<(usize, ProfileStamp), R> = Vec::new();
        for index in due {
            let resolved = config
                .get(*index)
                .and_then(|route| self.provider.resolve(route.profile_index));
            match resolved {
                Some(stamp) => {
                    let _ = context.push((*index, stamp));
                }
                None => {
                    log_skip(SkipReason::BuildFailed, *index);
                    self.router.skip(*index, SkipReason::BuildFailed, now)
                }
            }
        }
        if context.is_empty() {
            return;
        }
        let snapshot = match self.provider.capture().await {
            Ok(snapshot) => snapshot,
            Err(failure) => {
                if !self.route_snapshot_is_current(config) {
                    return;
                }
                self.health.last_build_failure = Some(failure.code());
                log_failure("capture failed", failure, self.health.captures);
                for (index, _) in &context {
                    self.router.skip(*index, SkipReason::BuildFailed, now);
                }
                return;
            }
        };
        self.health.captures = self.health.captures.saturating_add(1);
        if !self.route_snapshot_is_current(config) {
            return;
        }

        // One packet per distinct stamp: (stamp, the built packet or the failure).
        let mut built: Vec<(ProfileStamp, Result<PacketHandle, SkipReason>), R> = Vec::new();
        for (index, stamp) in &context {
            if !self.route_snapshot_is_current(config) {
                break;
            }
            let existing = built.iter().position(|(known, _)| known == stamp);
            let outcome = match existing {
                Some(position) => built[position].1,
                None => {
                    let outcome = self.build(&snapshot, stamp).await;
                    let _ = built.push((*stamp, outcome));
                    if !self.route_snapshot_is_current(config) {
                        break;
                    }
                    outcome
                }
            };
            match outcome {
                Ok(handle) => {
                    if self.router.admit(*index, handle, now) == Admit::OutboxFull {
                        // Counted by the router; the older packets stay.
                        log_skip_outbox_full(*index);
                    }
                }
                Err(reason) => {
                    log_skip(reason, *index);
                    self.router.skip(*index, reason, now)
                }
            }
        }
        // The references of the builder: the routes hold their own.
        for (_, outcome) in &built {
            if let Ok(handle) = outcome {
                self.router.pool_mut().release(*handle);
            }
        }
    }

    fn route_snapshot_is_current(&self, config: &Routes<R>) -> bool {
        self.state.routes() == *config
    }

    async fn build(
        &mut self,
        snapshot: &P::Snapshot,
        stamp: &ProfileStamp,
    ) -> Result<PacketHandle, SkipReason> {
        let Some((handle, buffer)) = self.router.pool_mut().alloc(*stamp) else {
            return Err(SkipReason::PoolExhausted);
        };
        match self.provider.build(snapshot, stamp, buffer).await {
            Ok(len) if self.router.pool_mut().seal(handle, len) => {
                self.health.built = self.health.built.saturating_add(1);
                self.health.last_build_failure = None;
                Ok(handle)
            }
            Ok(_) => {
                self.router.pool_mut().release(handle);
                self.health.last_build_failure = Some(BuildFailure::TooLarge.code());
                log_failure(
                    "packet too large",
                    BuildFailure::TooLarge,
                    self.health.built,
                );
                Err(SkipReason::BuildFailed)
            }
            Err(failure) => {
                self.router.pool_mut().release(handle);
                self.health.last_build_failure = Some(failure.code());
                log_failure("build failed", failure, self.health.built);
                Err(SkipReason::BuildFailed)
            }
        }
    }

    async fn deliver(&mut self, config: &Routes<R>) {
        if !self.route_snapshot_is_current(config) {
            return;
        }
        let now = self.clock.now();
        let Some(delivery) = self.router.next_delivery(config, now) else {
            return;
        };
        let provider = &self.provider;
        if !self
            .router
            .head_is_current(&delivery, |stamp| provider.is_current(stamp))
        {
            self.router.invalidate(|stamp| provider.is_current(stamp));
            return;
        }
        let Some(route) = config.get(delivery.route) else {
            return;
        };
        let uri = if delivery.path == 0 {
            Some(&route.endpoint)
        } else {
            route.fallback.as_ref()
        };
        let Some(endpoint) = uri.and_then(|uri| uri.endpoint()) else {
            return;
        };
        let Some((_, bytes)) = self.router.pool().get(delivery.packet) else {
            return;
        };

        let attempt = self.transport.deliver(endpoint, bytes);
        match select3(attempt, provider.changed(), self.state.changed()).await {
            Either3::First(result) => {
                let now = self.clock.now();
                // A change of the routes first drops what it touches (a changed or
                // replaced route loses its queue); `complete` then ignores a result
                // for such a route and records one for a route that was not touched.
                if !self.route_snapshot_is_current(config) {
                    let current = self.state.routes();
                    self.reconcile(&current, now);
                }
                log_delivery(&delivery, &result);
                self.router.complete(delivery, result, now);
            }
            Either3::Second(()) => {
                // The policy changed during the attempt: it is dropped and
                // the queues are checked before anything is sent again.
                log::debug!(target: "connectivity", "Delivery dropped: the policy changed");
                self.router.invalidate(|stamp| provider.is_current(stamp));
            }
            Either3::Third(_) => {
                log::debug!(target: "connectivity", "Delivery dropped: the routes changed");
                let current = self.state.routes();
                self.reconcile(&current, self.clock.now());
            }
        }
    }

    async fn sleep(&mut self) {
        let now = self.clock.now();
        let config = self.state.routes();
        let wait_ms = match self.router.next_wakeup(&config) {
            Some(at) => at.millis.saturating_sub(now.millis),
            None => u64::MAX,
        };
        if wait_ms == 0 {
            return;
        }
        let ms = u32::try_from(wait_ms).unwrap_or(u32::MAX);
        match select3(
            self.delay.delay_ms(ms),
            self.state.changed(),
            self.provider.changed(),
        )
        .await
        {
            Either3::First(()) | Either3::Second(_) | Either3::Third(()) => {}
        }
    }
}

// The helpers below keep logging temporaries out of the frames of the async
// `publish` and `deliver`.

#[cold]
#[inline(never)]
fn log_skip(reason: SkipReason, route: usize) {
    log::debug!(target: "connectivity", "Publication skipped (route={} reason={:?})", route, reason);
}

#[cold]
#[inline(never)]
fn log_skip_outbox_full(route: usize) {
    log::debug!(target: "connectivity", "Packet skipped: outbox full (route={})", route);
}

/// Warns on the 1st, 2nd, 4th, ... occurrence and stays at debug level in
/// between, so a persistent fault does not fill the flash log.
#[cold]
#[inline(never)]
fn log_failure(what: &'static str, failure: BuildFailure, count: u32) {
    if count.wrapping_add(1).is_power_of_two() {
        log::warn!(target: "connectivity", "Publication failed: {} (err={:?})", what, failure);
    } else {
        log::debug!(target: "connectivity", "Publication failed: {} (err={:?})", what, failure);
    }
}

#[cold]
#[inline(never)]
fn log_delivery(delivery: &Delivery, result: &Result<TransportAck, TransportError>) {
    match result {
        Ok(_) => log::debug!(
            target: "connectivity",
            "Packet delivered (route={} path={})",
            delivery.route,
            delivery.path
        ),
        Err(error) => match error.disposition() {
            Disposition::Transient => log::debug!(
                target: "connectivity",
                "Send failed (route={} path={} err={:?})",
                delivery.route,
                delivery.path,
                error
            ),
            Disposition::Permanent => log::warn!(
                target: "connectivity",
                "Packet dropped, rejected (route={} path={} err={:?})",
                delivery.route,
                delivery.path,
                error
            ),
            Disposition::Unauthorized => log::warn!(
                target: "connectivity",
                "Delivery unauthorized (route={} path={} err={:?})",
                delivery.route,
                delivery.path,
                error
            ),
        },
    }
}

#[cfg(test)]
mod tests;
