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
//! The telemetry service: snapshot, encode, sign, queue, deliver.
//!
//! ```text
//! interval
//!    │
//!    ▼
//! identity? ── no ──► skip (WaitingForIdentity)
//! UTC?      ── no ──► skip (WaitingForUtc)
//!    │
//!    ▼
//! snapshot ─► encode once ─► nonce ─► sign ─► envelope once ─► outbox
//!                                                                │
//!                         accepted ◄── send exact bytes ◄────────┘
//!                         retryable ─► backoff, same bytes again
//! ```
//!
//! The service runs in its own task and only reads snapshots: sensing and the
//! measurement store do not depend on it, so no Wi-Fi, DNS, TCP, server, time
//! or authentication failure can stop or delay them.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use rohi_hal::MeasurementsSnapshot;
use rohi_hal::runtime::{Backoff, Clock, MeasurementHandle, with_timeout};

use crate::clock::{NonceSource, UtcClock};
use crate::encoder::{EncodeError, UrbanEncoder};
use crate::envelope::{BuildError, EnvelopeBuilder, FrameSigner, SignError};
use crate::outbox::Outbox;
use crate::transport::{Disposition, EnvelopeTransport, TransportError};

/// Default depth of the outbox, in frames.
pub const DEFAULT_OUTBOX_DEPTH: usize = 4;
/// Time the owner of the measurement store may take to answer a snapshot
/// request.
const SNAPSHOT_TIMEOUT_MS: u32 = 5_000;

/// Runtime parameters of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceConfig {
    /// `core.v1.Meta.node_id`.
    pub node_id: u64,
    /// Time between two telemetry frames, in milliseconds.
    pub interval_ms: u64,
    /// First delay after a retryable failure, in milliseconds.
    pub retry_initial_ms: u32,
    /// Longest delay between two attempts, in milliseconds. Also the pause
    /// after an authorization failure.
    pub retry_max_ms: u32,
}

impl ServiceConfig {
    /// Default retry delays: 2 s, doubling up to 5 min.
    pub const fn new(node_id: u64, interval_ms: u64) -> Self {
        Self {
            node_id,
            interval_ms,
            retry_initial_ms: 2_000,
            retry_max_ms: 300_000,
        }
    }
}

/// Where the service takes immutable snapshots from.
#[allow(async_fn_in_trait)]
pub trait SnapshotSource {
    /// The current snapshot, `None` if it cannot be obtained.
    async fn snapshot(&self) -> Option<MeasurementsSnapshot>;
}

impl<M: RawMutex> SnapshotSource for MeasurementHandle<'_, M> {
    async fn snapshot(&self) -> Option<MeasurementsSnapshot> {
        MeasurementHandle::snapshot(self).await.ok()
    }
}

/// Counters of the service. They only grow (saturating).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Diagnostics {
    /// Frames signed and queued.
    pub frames_created: u32,
    /// Frames accepted by the ingress.
    pub frames_delivered: u32,
    /// Delivery attempts that failed and will be retried.
    pub send_retries: u32,
    /// Frames dropped after a permanent rejection.
    pub frames_rejected: u32,
    /// Frames not created: the device has no identity.
    pub skipped_no_identity: u32,
    /// Frames not created: UTC time is not available.
    pub skipped_no_utc: u32,
    /// Frames not created: no entropy for the nonce.
    pub skipped_no_entropy: u32,
    /// Frames not created: the outbox is full.
    pub skipped_outbox_full: u32,
    /// Frames not created: no snapshot could be obtained.
    pub skipped_no_snapshot: u32,
    /// Frames not created: the snapshot has no valid supported measurement.
    pub skipped_no_measurements: u32,
    /// Frames not created: signing or encoding failed.
    pub build_failures: u32,
    /// Valid measurements omitted because the schema has no representation.
    pub measurements_unsupported: u32,
    /// Valid measurements omitted because the value cannot be represented.
    pub measurements_out_of_range: u32,
}

macro_rules! bump {
    ($field:expr) => {
        $field = $field.saturating_add(1)
    };
}

/// What the service is doing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityState {
    /// Started, no interval elapsed yet.
    #[default]
    Starting,
    /// Everything works; waiting for the next interval.
    Idle,
    /// The device has no identity yet.
    WaitingForIdentity,
    /// UTC time is not synchronized yet.
    WaitingForUtc,
    /// The last attempt failed with a retryable error; backing off.
    Backoff(TransportError),
    /// The ingress rejected the device (HTTP 401/403): not authorized.
    Unauthorized(TransportError),
}

/// Published status of the service.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConnectivityStatus {
    /// Current state.
    pub state: ConnectivityState,
    /// Counters.
    pub diagnostics: Diagnostics,
    /// Frames waiting for delivery.
    pub outbox_len: usize,
}

/// Status of the service, readable from any task.
pub struct ConnectivityShared<M: RawMutex> {
    status: Mutex<M, Cell<ConnectivityStatus>>,
}

impl<M: RawMutex> ConnectivityShared<M> {
    /// Shared status in the `Starting` state.
    pub const fn new() -> Self {
        Self {
            status: Mutex::new(Cell::new(ConnectivityStatus {
                state: ConnectivityState::Starting,
                diagnostics: Diagnostics {
                    frames_created: 0,
                    frames_delivered: 0,
                    send_retries: 0,
                    frames_rejected: 0,
                    skipped_no_identity: 0,
                    skipped_no_utc: 0,
                    skipped_no_entropy: 0,
                    skipped_outbox_full: 0,
                    skipped_no_snapshot: 0,
                    skipped_no_measurements: 0,
                    build_failures: 0,
                    measurements_unsupported: 0,
                    measurements_out_of_range: 0,
                },
                outbox_len: 0,
            })),
        }
    }

    /// The latest published status.
    pub fn status(&self) -> ConnectivityStatus {
        self.status.lock(Cell::get)
    }

    fn publish(&self, status: ConnectivityStatus) {
        self.status.lock(|cell| cell.set(status));
    }
}

impl<M: RawMutex> Default for ConnectivityShared<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// The telemetry service.
///
/// Borrowed parts are shared with the rest of the firmware; the transport, the
/// nonce source and the delay are owned.
pub struct ConnectivityService<
    'a,
    M,
    S,
    C,
    U,
    G,
    N,
    T,
    D,
    const DEPTH: usize = DEFAULT_OUTBOX_DEPTH,
> where
    M: RawMutex,
{
    config: ServiceConfig,
    shared: &'a ConnectivityShared<M>,
    source: S,
    clock: C,
    utc: &'a U,
    signer: &'a G,
    nonce: N,
    transport: T,
    delay: D,
    outbox: Outbox<DEPTH>,
    backoff: Backoff,
    state: ConnectivityState,
    diagnostics: Diagnostics,
    /// Earliest monotonic time of the next delivery attempt.
    retry_at: u64,
}

impl<'a, M, S, C, U, G, N, T, D, const DEPTH: usize>
    ConnectivityService<'a, M, S, C, U, G, N, T, D, DEPTH>
where
    M: RawMutex,
    S: SnapshotSource,
    C: Clock,
    U: UtcClock,
    G: FrameSigner,
    N: NonceSource,
    T: EnvelopeTransport,
    D: DelayNs,
{
    /// Creates the service. Nothing runs until [`run`](Self::run).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: ServiceConfig,
        shared: &'a ConnectivityShared<M>,
        source: S,
        clock: C,
        utc: &'a U,
        signer: &'a G,
        nonce: N,
        transport: T,
        delay: D,
    ) -> Self {
        Self {
            config,
            shared,
            source,
            clock,
            utc,
            signer,
            nonce,
            transport,
            delay,
            outbox: Outbox::new(),
            backoff: Backoff::new(config.retry_initial_ms, config.retry_max_ms),
            state: ConnectivityState::Starting,
            diagnostics: Diagnostics::default(),
            retry_at: 0,
        }
    }

    /// Runs forever: one frame per interval, delivery with backoff.
    ///
    /// Cancellation: safe at any point; queued frames are lost, which is the
    /// contract of the in-memory outbox.
    pub async fn run(&mut self) -> ! {
        let mut next_produce = self.clock.now().millis;
        loop {
            let now = self.clock.now().millis;
            if now >= next_produce {
                self.produce().await;
                next_produce = now.saturating_add(self.config.interval_ms);
            }
            if now >= self.retry_at {
                self.deliver().await;
            }
            self.publish();

            let now = self.clock.now().millis;
            let mut wake = next_produce;
            if !self.outbox.is_empty() {
                wake = wake.min(self.retry_at);
            }
            let wait = wake.saturating_sub(now).clamp(1, u64::from(u32::MAX));
            self.delay.delay_ms(wait as u32).await;
        }
    }

    /// Creates and queues one frame if everything it needs is available.
    ///
    /// Never blocks sensing: it only reads a snapshot.
    pub async fn produce(&mut self) {
        let d = &mut self.diagnostics;
        if self.outbox.is_full() {
            bump!(d.skipped_outbox_full);
            return;
        }
        if let Err(SignError::NotProvisioned) = self.signer.sensor_id() {
            bump!(d.skipped_no_identity);
            self.state = ConnectivityState::WaitingForIdentity;
            return;
        }
        if self.utc.now_unix_ms().is_err() {
            bump!(d.skipped_no_utc);
            self.state = ConnectivityState::WaitingForUtc;
            return;
        }
        let snapshot = match with_timeout(
            &mut self.delay,
            SNAPSHOT_TIMEOUT_MS,
            self.source.snapshot(),
        )
        .await
        {
            Ok(Some(snapshot)) => snapshot,
            _ => {
                bump!(d.skipped_no_snapshot);
                return;
            }
        };
        // UTC and the monotonic clock are read together: the timestamp is the
        // UTC time of the measurement set, the snapshot being `age` old.
        let age = self
            .clock
            .now()
            .saturating_millis_since(snapshot.generated_at);
        let Some(timestamp) = self
            .utc
            .now_unix_ms()
            .ok()
            .and_then(|utc| utc.checked_sub(age))
        else {
            bump!(d.skipped_no_utc);
            self.state = ConnectivityState::WaitingForUtc;
            return;
        };

        let message = match UrbanEncoder::encode_snapshot(&snapshot, self.config.node_id, timestamp)
        {
            Ok(message) => message,
            Err(EncodeError::NothingToSend) => {
                bump!(d.skipped_no_measurements);
                return;
            }
            Err(EncodeError::TooLarge) => {
                bump!(d.build_failures);
                return;
            }
        };
        let stats = message.stats();
        d.measurements_unsupported = d
            .measurements_unsupported
            .saturating_add(stats.omitted_unsupported);
        d.measurements_out_of_range = d
            .measurements_out_of_range
            .saturating_add(stats.omitted_out_of_range);

        let frame = EnvelopeBuilder::new(self.signer, &mut self.nonce).sign(message.as_bytes());
        match frame {
            Ok(frame) => {
                // Cannot fail: fullness was checked above and nothing else
                // pushes.
                if self.outbox.push(frame).is_ok() {
                    bump!(d.frames_created);
                    if matches!(
                        self.state,
                        ConnectivityState::WaitingForIdentity | ConnectivityState::WaitingForUtc
                    ) {
                        self.state = ConnectivityState::Idle;
                    }
                }
            }
            Err(BuildError::NotProvisioned) => {
                bump!(d.skipped_no_identity);
                self.state = ConnectivityState::WaitingForIdentity;
            }
            Err(BuildError::Nonce) => bump!(d.skipped_no_entropy),
            Err(BuildError::Signing | BuildError::TooLarge) => bump!(d.build_failures),
        }
    }

    /// Sends the oldest frame once. The frame stays queued, byte-identical,
    /// until it is accepted or permanently rejected.
    pub async fn deliver(&mut self) {
        let Some(frame) = self.outbox.front() else {
            return;
        };
        let result = self.transport.send(frame).await;
        let now = self.clock.now().millis;
        let d = &mut self.diagnostics;
        match result {
            Ok(_) => {
                self.outbox.pop();
                self.backoff.reset();
                bump!(d.frames_delivered);
                self.state = ConnectivityState::Idle;
            }
            Err(error) => match error.disposition() {
                Disposition::Retry => {
                    bump!(d.send_retries);
                    self.retry_at = now.saturating_add(u64::from(self.backoff.next_delay_ms()));
                    self.state = ConnectivityState::Backoff(error);
                }
                Disposition::Permanent => {
                    self.outbox.pop();
                    bump!(d.frames_rejected);
                    self.state = ConnectivityState::Idle;
                }
                Disposition::Unauthorized => {
                    self.outbox.pop();
                    bump!(d.frames_rejected);
                    self.retry_at = now.saturating_add(u64::from(self.config.retry_max_ms));
                    self.state = ConnectivityState::Unauthorized(error);
                }
            },
        }
        self.publish();
    }

    /// Counters of this service.
    pub fn diagnostics(&self) -> Diagnostics {
        self.diagnostics
    }

    /// Current state.
    pub fn state(&self) -> ConnectivityState {
        self.state
    }

    /// Number of queued frames.
    pub fn queued(&self) -> usize {
        self.outbox.len()
    }

    /// Monotonic time (ms) before which no delivery is attempted.
    pub fn retry_at_ms(&self) -> u64 {
        self.retry_at
    }

    /// The oldest queued frame.
    pub fn front(&self) -> Option<&crate::EnvelopeFrame> {
        self.outbox.front()
    }

    fn publish(&self) {
        self.shared.publish(ConnectivityStatus {
            state: self.state,
            diagnostics: self.diagnostics,
            outbox_len: self.outbox.len(),
        });
    }
}
