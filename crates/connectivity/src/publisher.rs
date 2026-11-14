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
//! routes, publish what is due (one capture, one sealed packet per distinct
//! profile stamp, shared by the due routes), make at most one delivery
//! attempt, report the status and sleep until the next deadline or change.
//! All decisions are in [`Router`]; this file only does the waiting and the
//! I/O. No lock is held across a capture, a build or a transport attempt.
//!
//! A delivery attempt is raced against policy changes: when the telemetry
//! policy changes during an attempt, the attempt is dropped (the transport
//! contract makes that safe) and queued packets of an older policy are
//! invalidated before any new send starts.

use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use heapless::Vec;
use rohi_telemetry::ProfileStamp;
use rohi_time::{Clock, MonotonicTimestamp};

use crate::config::{MAX_ROUTES, RouteSet};
use crate::pool::PacketHandle;
use crate::provider::{BuildFailure, PacketProvider};
use crate::router::{Admit, Router, SkipReason};
use crate::state::{ConnectivityState, Health};
use crate::transport::PacketTransport;

/// Drives the routes of a device.
pub struct Publisher<
    'a,
    M: RawMutex,
    P: PacketProvider,
    T: PacketTransport,
    D: DelayNs,
    C: Clock,
    const DEPTH: usize,
    const SLOTS: usize,
    const BYTES: usize,
> {
    state: &'a ConnectivityState<M>,
    provider: P,
    transport: T,
    delay: D,
    clock: C,
    router: Router<DEPTH, SLOTS, BYTES>,
    health: Health,
}

impl<
    'a,
    M: RawMutex,
    P: PacketProvider,
    T: PacketTransport,
    D: DelayNs,
    C: Clock,
    const DEPTH: usize,
    const SLOTS: usize,
    const BYTES: usize,
> Publisher<'a, M, P, T, D, C, DEPTH, SLOTS, BYTES>
{
    /// A publisher over the effective routes in `state`.
    pub const fn new(
        state: &'a ConnectivityState<M>,
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
    pub fn router(&self) -> &Router<DEPTH, SLOTS, BYTES> {
        &self.router
    }

    /// Runs forever.
    pub async fn run(&mut self) -> ! {
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
        self.deliver(&config).await;

        let now = self.clock.now();
        self.state
            .report(self.router.views(&config, now), self.health.clone());
    }

    fn reconcile(&mut self, config: &RouteSet, now: MonotonicTimestamp) {
        let provider = &self.provider;
        self.router.sync(
            config,
            |id| provider.resolve(id).map(|stamp| stamp.incarnation),
            now,
        );
        self.router.invalidate(|stamp| provider.is_current(stamp));
    }

    async fn publish(&mut self, config: &RouteSet, due: &[usize], now: MonotonicTimestamp) {
        let snapshot = match self.provider.capture().await {
            Ok(snapshot) => snapshot,
            Err(failure) => {
                self.health.last_build_failure = Some(failure.code());
                for index in due {
                    self.router.skip(*index, SkipReason::BuildFailed, now);
                }
                return;
            }
        };
        self.health.captures = self.health.captures.saturating_add(1);

        // One packet per distinct stamp: (stamp, the built packet or the failure).
        let mut built: Vec<(ProfileStamp, Result<PacketHandle, SkipReason>), MAX_ROUTES> =
            Vec::new();
        for index in due {
            let Some(route) = self
                .router
                .id(*index)
                .and_then(|id| config.get(id.as_str()))
            else {
                continue;
            };
            let Some(stamp) = self.provider.resolve(route.profile.id.as_str()) else {
                self.router.skip(*index, SkipReason::BuildFailed, now);
                continue;
            };
            let existing = built.iter().position(|(known, _)| *known == stamp);
            let outcome = match existing {
                Some(position) => built[position].1,
                None => {
                    let outcome = self.build(&snapshot, &stamp).await;
                    let _ = built.push((stamp.clone(), outcome));
                    outcome
                }
            };
            match outcome {
                Ok(handle) => {
                    if self.router.admit(*index, handle, now) == Admit::OutboxFull {
                        // Counted by the router; the older packets stay.
                    }
                }
                Err(reason) => self.router.skip(*index, reason, now),
            }
        }
        // The references of the builder: the routes hold their own.
        for (_, outcome) in &built {
            if let Ok(handle) = outcome {
                self.router.pool_mut().release(*handle);
            }
        }
    }

    async fn build(
        &mut self,
        snapshot: &P::Snapshot,
        stamp: &ProfileStamp,
    ) -> Result<PacketHandle, SkipReason> {
        let Some((handle, buffer)) = self.router.pool_mut().alloc(stamp.clone()) else {
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
                Err(SkipReason::BuildFailed)
            }
            Err(failure) => {
                self.router.pool_mut().release(handle);
                self.health.last_build_failure = Some(failure.code());
                Err(SkipReason::BuildFailed)
            }
        }
    }

    async fn deliver(&mut self, config: &RouteSet) {
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
        let Some(route) = self
            .router
            .id(delivery.route)
            .and_then(|id| config.get(id.as_str()))
        else {
            return;
        };
        let uri = if delivery.path == 0 {
            Some(&route.primary)
        } else {
            route
                .fallbacks
                .get(delivery.path - 1)
                .map(|fallback| &fallback.endpoint)
        };
        let Some(endpoint) = uri.and_then(|uri| uri.endpoint()) else {
            return;
        };
        let Some((_, bytes)) = self.router.pool().get(delivery.packet) else {
            return;
        };

        let attempt = self.transport.deliver(endpoint, bytes);
        match select(attempt, provider.changed()).await {
            Either::First(result) => {
                let now = self.clock.now();
                self.router.complete(delivery, result, now);
            }
            Either::Second(()) => {
                // The policy changed during the attempt: it is dropped and
                // the queues are checked before anything is sent again.
                self.router.invalidate(|stamp| provider.is_current(stamp));
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

#[cfg(test)]
mod tests;
