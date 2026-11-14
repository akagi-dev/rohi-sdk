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
//! The telemetry service: snapshot, encode, sign, queue, deliver.
//!
//! ```text
//! interval
//!    │
//!    ▼
//! outbox full? ── yes ─► skip (skipped_outbox_full)
//! identity?    ── no  ─► skip (WaitingForIdentity)
//! UTC?         ── no  ─► skip (WaitingForUtc)
//!    │
//!    ▼
//! snapshot ─► encode once ─► nonce ─► sign ─► envelope once ─► outbox
//!                                                                │
//!                         accepted ◄── send exact bytes ◄────────┘
//!                         retryable ─► backoff, same bytes again
//! ```
//!
//! [`ConnectivityService::run`] is one task, separate from sensing: every
//! [`interval`](ServiceConfig::interval_ms) it creates a frame (outbox room?
//! identity? UTC? snapshot, encode, sign), queues it in the bounded in-memory
//! [`Outbox`] (no allocation), and delivers the oldest frame. A snapshot that
//! the owner of the store does not answer within 5 s counts as
//! [`Diagnostics::skipped_no_snapshot`]. The timestamp of a frame is the UTC
//! time of the snapshot: UTC now minus the age of the snapshot.
//!
//! - Retryable failures keep the frame at the front and back off exponentially
//!   (2 s, doubling, capped at 5 min, see [`ServiceConfig::new`]); a success
//!   resets the backoff. There is never a zero-length wait.
//! - A permanent rejection drops the frame. An authorization failure
//!   (HTTP 401/403) drops it too and pauses delivery for the longest backoff
//!   ([`ServiceConfig::retry_max_ms`]), so there is no hot loop. See
//!   [`transport`](crate::transport) for the classification.
//! - When the outbox is full, **no new frame is created** (nothing is
//!   evicted) and [`Diagnostics::skipped_outbox_full`] is incremented.
//! - The service only reads snapshots: sensing and the measurement store do
//!   not depend on it, so no Wi-Fi, DNS, TCP, server, time or authorization
//!   failure can stop or delay them.
//! - The queue is in memory: frames are lost on reboot (a flash spool is out
//!   of scope).
//! - [`ConnectivityShared::status`] publishes the state ([`ConnectivityState`]:
//!   `Idle`, `WaitingForUtc`, `WaitingForIdentity`, `Backoff(error)`,
//!   `Unauthorized(error)`, and `Starting` before the first interval) and the
//!   [`Diagnostics`] counters, readable from any task. The
//!   [`managed`](crate::managed) module turns them into the `connectivity`
//!   service of the status plane.
//!
//! The firmware stops and replaces the service when the `connectivity`
//! configuration changes, see [`config`](crate::config).
//!
//! # Example
//!
//! The service is exercised step by step with [`produce`] and [`deliver`]
//! (what [`run`] does on a timer). A transport that fails twice shows the
//! retry of the same bytes and the backoff:
//!
//! ```
//! use core::cell::{Cell, RefCell};
//!
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use embedded_hal_async::delay::DelayNs;
//! use rohi_connectivity::{
//!     ClockError, ConnectivityService, ConnectivityShared, ConnectivityState, EnvelopeFrame,
//!     EnvelopeTransport, NONCE_LEN, NonceError, NonceSource, ServiceConfig, SnapshotSource,
//!     TransportAck, TransportError, UtcClock,
//! };
//! use rohi_time::Clock;
//! use rohi_sensors::{
//!     Bmx280Update, Celsius, MeasurementStore, MeasurementUpdate, MeasurementsSnapshot,
//! Pascal, RelativeHumidity, SensorEvent,
//! };
//! use rohi_time::MonotonicTimestamp;
//! use rohi_identity::Identity;
//! use rohi_location::NoLocation;
//!
//! struct Source(MeasurementsSnapshot);
//! impl SnapshotSource for Source {
//!     async fn snapshot(&self) -> Option<MeasurementsSnapshot> {
//!         Some(self.0)
//!     }
//! }
//! struct Uptime<'a>(&'a Cell<u64>);
//! impl Clock for Uptime<'_> {
//!     fn now(&self) -> MonotonicTimestamp {
//!         MonotonicTimestamp::from_millis(self.0.get())
//!     }
//! }
//! struct Utc;
//! impl UtcClock for Utc {
//!     fn now_unix_ms(&self) -> Result<u64, ClockError> {
//!         Ok(1_700_000_000_000)
//!     }
//! }
//! struct Counter(u8);
//! impl NonceSource for Counter {
//!     fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
//!         self.0 += 1;
//!         dst.fill(self.0);
//!         Ok(())
//!     }
//! }
//! // The first two attempts fail, the third is accepted.
//! struct Flaky<'a>(&'a RefCell<Vec<Vec<u8>>>);
//! impl EnvelopeTransport for Flaky<'_> {
//!     async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError> {
//!         let mut sent = self.0.borrow_mut();
//!         sent.push(frame.as_bytes().to_vec());
//!         if sent.len() < 3 { Err(TransportError::Timeout) } else { Ok(TransportAck { status: 202 }) }
//!     }
//! }
//! struct NoDelay;
//! impl DelayNs for NoDelay {
//!     async fn delay_ns(&mut self, _ns: u32) {}
//! }
//!
//! let now = Cell::new(10_000);
//! let mut store = MeasurementStore::new();
//! store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(Bmx280Update::bme280(
//!     Celsius(20.0),
//!     RelativeHumidity(50.0),
//!     Pascal(100_000.0),
//!     MonotonicTimestamp::from_millis(9_500),
//! ))));
//! let snapshot = store.snapshot(MonotonicTimestamp::from_millis(9_900));
//!
//! let signer = Identity::from_secret(&[7; 32]).unwrap().signer();
//! let shared = ConnectivityShared::<NoopRawMutex>::new();
//! let sent = RefCell::new(Vec::new());
//! let mut service: ConnectivityService<'_, NoopRawMutex, _, _, _, _, _, _, _, 4> =
//!     ConnectivityService::new(
//!         ServiceConfig::new(1, 60_000),
//!         &shared,
//!         Source(snapshot),
//!         &NoLocation,
//!         Uptime(&now),
//!         &Utc,
//!         &signer,
//!         Counter(0),
//!         Flaky(&sent),
//!         NoDelay,
//!     );
//!
//! block_on(async {
//!     service.produce().await;
//!     service.deliver().await; // timeout: retry after 2 s
//!     assert_eq!(service.state(), ConnectivityState::Backoff(TransportError::Timeout));
//!     assert_eq!(service.retry_at_ms(), 12_000);
//!
//!     now.set(12_000);
//!     service.deliver().await; // timeout again: the delay doubles
//!     assert_eq!(service.retry_at_ms(), 16_000);
//!
//!     now.set(16_000);
//!     service.deliver().await; // accepted: removed, backoff reset
//! });
//! assert_eq!(service.state(), ConnectivityState::Idle);
//! assert_eq!(service.queued(), 0);
//!
//! // Three attempts, one frame: the very same bytes each time.
//! let sent = sent.borrow();
//! assert_eq!(sent.len(), 3);
//! assert!(sent.iter().all(|attempt| *attempt == sent[0]));
//! let diagnostics = shared.status().diagnostics;
//! assert_eq!((diagnostics.frames_created, diagnostics.send_retries), (1, 2));
//! ```
//!
//! [`produce`]: ConnectivityService::produce
//! [`deliver`]: ConnectivityService::deliver
//! [`run`]: ConnectivityService::run

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use rohi_sensors::MeasurementsSnapshot;
use rohi_sensors::runtime::MeasurementHandle;
use rohi_time::{Backoff, Clock, with_timeout};

use crate::clock::{NonceSource, UtcClock};
use crate::encoder::{EncodeError, UrbanEncoder};
use crate::envelope::{BuildError, EnvelopeBuilder, FrameSigner, SignError};
use crate::outbox::Outbox;
use crate::transport::{Disposition, EnvelopeTransport, TransportError};
use rohi_location::LocationSource;

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
    /// Default retry delays: 2 s, doubling up to 5 min. The delays are exact
    /// doublings without jitter, and the longest one is also the pause after
    /// an authorization failure.
    ///
    /// ```
    /// use rohi_connectivity::ServiceConfig;
    ///
    /// let config = ServiceConfig::new(7, 60_000);
    /// assert_eq!((config.node_id, config.interval_ms), (7, 60_000));
    /// assert_eq!((config.retry_initial_ms, config.retry_max_ms), (2_000, 300_000));
    /// ```
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
///
/// It is implemented for [`MeasurementHandle`], the handle the firmware
/// receives from the owner of the measurement store. The service waits at
/// most 5 s for an answer and then skips the frame
/// ([`Diagnostics::skipped_no_snapshot`]), so a stuck store cannot stall it.
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

/// Counters of the service. They only grow (saturating) and restart from zero
/// when the service is replaced (a configuration change).
///
/// The `skipped_*` counters and `build_failures` partition the reasons why a
/// tick created no frame; `frames_created` counts those that did.
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
    /// Frames created without a location because none was known.
    pub frames_without_location: u32,
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
///
/// The service publishes after every step; readers (the status plane, the
/// firmware) only copy the latest [`ConnectivityStatus`] out of a blocking
/// mutex of type `M`, so reading never waits for the network.
///
/// ```
/// use embassy_sync::blocking_mutex::raw::NoopRawMutex;
/// use rohi_connectivity::{ConnectivityShared, ConnectivityState};
///
/// let shared = ConnectivityShared::<NoopRawMutex>::new();
/// let status = shared.status();
/// assert_eq!(status.state, ConnectivityState::Starting);
/// assert_eq!(status.outbox_len, 0);
/// assert_eq!(status.diagnostics.frames_created, 0);
/// ```
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
                    frames_without_location: 0,
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

    /// Resets to the initial status, for a service that was stopped or replaced.
    pub fn reset(&self) {
        self.publish(ConnectivityStatus::default());
    }

    pub(crate) fn publish(&self, status: ConnectivityStatus) {
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
    location: &'a dyn LocationSource,
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
        location: &'a dyn LocationSource,
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
            location,
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
    /// Each turn creates a frame when the interval elapsed, delivers the oldest
    /// frame when its backoff expired, publishes the status and sleeps until
    /// the next of the two (never less than 1 ms). A delivery attempt is
    /// bounded by the transport's own timeout, and frame creation waits for
    /// it, so the interval is a lower bound.
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
    /// Never blocks sensing: it only reads a snapshot. Every reason not to
    /// create a frame is counted in [`Diagnostics`]; nothing is returned
    /// because the next interval simply tries again.
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

        let location = self.location.location();
        let message = match UrbanEncoder::encode_snapshot_at(
            &snapshot,
            self.config.node_id,
            timestamp,
            location.as_ref(),
        ) {
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
                    if !stats.location_included {
                        bump!(d.frames_without_location);
                    }
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
