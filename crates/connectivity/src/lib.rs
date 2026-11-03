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
#![no_std]

//! Signed telemetry for Robonomics Open Hardware: immutable measurement
//! snapshots become signed
//! [`airalab/connectivity-protocol`](https://github.com/airalab/connectivity-protocol)
//! frames that a pluggable transport delivers.
//!
//! # Overview
//!
//! Use this crate to send the measurements of a device to a telemetry ingress
//! (for example [`airalab/robins`](https://github.com/airalab/robins)). It
//! owns the whole uplink path and nothing else:
//!
//! * mapping a [`rohi_hal::MeasurementsSnapshot`] to a `core.v1.Message`
//!   ([`UrbanEncoder`]);
//! * signing it with the device identity ([`EnvelopeBuilder`], [`FrameSigner`])
//!   into an immutable [`EnvelopeFrame`];
//! * a bounded in-memory [`Outbox`] and the [`ConnectivityService`] task that
//!   creates, queues and delivers frames with retry and backoff;
//! * [`HttpTransport`]: `POST /v1/telemetry` over plain TCP on top of
//!   [`rohi_net::NetworkHandle`];
//! * the persistent `connectivity` configuration section
//!   ([`ConnectivityConfig`]) and its status in the management plane
//!   ([`ConnectivityStatusService`]).
//!
//! It does not sense (that is `rohi-hal`), does not keep the identity (that is
//! [`rohi_identity`]), does not synchronize the clock ([`rohi_time`]) and does
//! not know where the device is ([`rohi_location`]); it consumes those through
//! small traits. The wire format is pinned to a revision of the protocol whose
//! schema is vendored in
//! [`rohi_connectivity_proto`](../rohi_connectivity_proto/index.html), see its
//! documentation for the pinned revision and the messages.
//!
//! # Quick Example
//!
//! One telemetry cycle on the host, with everything the firmware provides
//! replaced by a few lines: a snapshot of a BME280, an identity, a fixed UTC
//! clock, a nonce counter and a transport that records the bytes it is asked
//! to send. The signature of the delivered frame is then checked the way an
//! ingress does:
//!
//! ```
//! use core::cell::RefCell;
//!
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use embedded_hal_async::delay::DelayNs;
//! use rohi_connectivity::{
//!     ClockError, ConnectivityService, ConnectivityShared, ConnectivityState, EnvelopeFrame,
//!     EnvelopeTransport, NONCE_LEN, NonceError, NonceSource, ServiceConfig, SnapshotSource,
//!     TransportAck, TransportError, UtcClock,
//! };
//! use rohi_connectivity_proto::core_v1::Message;
//! use rohi_connectivity_proto::crypto_v1::SignedEnvelope;
//! use rohi_connectivity_proto::{decode, limits, signing_preimage};
//! use rohi_hal::runtime::Clock;
//! use rohi_hal::{
//!     Bmx280Update, Celsius, MeasurementStore, MeasurementUpdate, MeasurementsSnapshot,
//!     MonotonicTimestamp, Pascal, RelativeHumidity, SensorEvent,
//! };
//! use rohi_identity::{Identity, PublicKey, Signature};
//! use rohi_location::NoLocation;
//!
//! // The firmware wires real implementations of these (see the firmware in
//! // `firmwares/altruist-urban`, `src/connectivity.rs`).
//! struct Source(MeasurementsSnapshot);
//! impl SnapshotSource for Source {
//!     async fn snapshot(&self) -> Option<MeasurementsSnapshot> {
//!         Some(self.0)
//!     }
//! }
//! struct Uptime; // monotonic time: 10 s since boot
//! impl Clock for Uptime {
//!     fn now(&self) -> MonotonicTimestamp {
//!         MonotonicTimestamp::from_millis(10_000)
//!     }
//! }
//! struct Utc; // Unix UTC, as a synchronized `rohi_time::TimeService` reports it
//! impl UtcClock for Utc {
//!     fn now_unix_ms(&self) -> Result<u64, ClockError> {
//!         Ok(1_700_000_000_000)
//!     }
//! }
//! struct Counter(u8); // a real source must be unpredictable: the hardware RNG
//! impl NonceSource for Counter {
//!     fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
//!         self.0 += 1;
//!         dst.fill(self.0);
//!         Ok(())
//!     }
//! }
//! struct Recorder<'a>(&'a RefCell<Vec<Vec<u8>>>);
//! impl EnvelopeTransport for Recorder<'_> {
//!     async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError> {
//!         self.0.borrow_mut().push(frame.as_bytes().to_vec());
//!         Ok(TransportAck { status: 202 })
//!     }
//! }
//! struct NoDelay;
//! impl DelayNs for NoDelay {
//!     async fn delay_ns(&mut self, _ns: u32) {}
//! }
//!
//! // A snapshot taken 1 s before "now" (monotonic 9 s).
//! let mut store = MeasurementStore::new();
//! store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
//!     Bmx280Update::bme280(
//!         Celsius(23.47),
//!         RelativeHumidity(53.42),
//!         Pascal(101_325.0),
//!         MonotonicTimestamp::from_millis(8_500),
//!     ),
//! )));
//! let snapshot = store.snapshot(MonotonicTimestamp::from_millis(9_000));
//!
//! let identity = Identity::from_secret(&[7; 32]).unwrap();
//! let signer = identity.signer(); // every `rohi_identity::Signer` is a `FrameSigner`
//! let shared = ConnectivityShared::<NoopRawMutex>::new();
//! let sent = RefCell::new(Vec::new());
//! // The last parameter is the outbox depth in frames.
//! let mut service: ConnectivityService<'_, NoopRawMutex, _, _, _, _, _, _, _, 4> =
//!     ConnectivityService::new(
//!         ServiceConfig::new(7, 60_000), // node id 7, one frame per minute
//!         &shared,
//!         Source(snapshot),
//!         &NoLocation,
//!         Uptime,
//!         &Utc,
//!         &signer,
//!         Counter(0),
//!         Recorder(&sent),
//!         NoDelay,
//!     );
//!
//! // `run()` does this forever, once per interval.
//! block_on(async {
//!     service.produce().await; // snapshot -> message -> signed frame -> outbox
//!     service.deliver().await; // send the oldest frame, drop it once accepted
//! });
//! let status = shared.status();
//! assert_eq!(status.state, ConnectivityState::Idle);
//! assert_eq!(status.diagnostics.frames_delivered, 1);
//!
//! // What the ingress does with the bytes it received.
//! let frames = sent.borrow();
//! let envelope: SignedEnvelope = decode(&frames[0]).unwrap();
//! let mut preimage = [0u8; limits::ED25519_KEY_LEN + limits::NONCE_LEN + limits::MAX_MESSAGE_LEN];
//! let len = signing_preimage(&envelope.sensor_id, &envelope.nonce, &envelope.message, &mut preimage)
//!     .unwrap();
//! let key = PublicKey::from_slice(&envelope.sensor_id).unwrap();
//! let signature = Signature::from_slice(&envelope.signature).unwrap();
//! assert_eq!(key.verify(&preimage[..len], &signature), Ok(()));
//! assert_eq!(key, identity.public_key());
//!
//! // The timestamp is UTC now minus the age of the snapshot (1 s).
//! let message: Message = decode(&envelope.message).unwrap();
//! assert_eq!(message.metadata().unwrap().timestamp, 1_699_999_999_000);
//! ```
//!
//! # Core Concepts
//!
//! * **Snapshot** ([`SnapshotSource`]): an immutable copy of all current
//!   measurements. The service only reads snapshots, so sensing never depends
//!   on the uplink.
//! * **Message** ([`UrbanEncoder`], [`EncodedMessage`]): the serialized
//!   `core.v1.Message`, produced exactly once. Only `Valid` measurements are
//!   emitted; a sensor with no representation in the pinned schema is omitted
//!   and counted, never relabeled (see [`encoder`]).
//! * **Frame** ([`EnvelopeFrame`]): the serialized `crypto.v1.SignedEnvelope`.
//!   The signature covers `sensor_id ‖ nonce ‖ message`; there is no envelope
//!   timestamp, the telemetry time is `core.v1.Meta.timestamp`, Unix UTC
//!   milliseconds. A frame never changes: a retry sends the same bytes (nonce,
//!   timestamp and signature included) and nothing is re-encoded from decoded
//!   fields (see [`envelope`]).
//! * **Boundaries** ([`UtcClock`], [`NonceSource`], [`FrameSigner`],
//!   [`EnvelopeTransport`]): everything the service needs from the outside.
//!   Private key bytes never reach this crate, only public keys and
//!   signatures do. Monotonic time is never used as UTC; without UTC, identity
//!   or entropy no frame is created (see [`clock`]).
//! * **Outbox** ([`Outbox`]): a fixed-depth in-memory FIFO. When it is full no
//!   new frame is created and nothing already queued is evicted.
//!
//! # Architecture & Integration
//!
//! ```text
//! sensor tasks ─► MeasurementStore ─snapshot─► UrbanEncoder ─► core.v1.Message  (encoded once)
//!                                                                  │
//!      LocationSource ─ GPS entry ─────────────────────────────────┤
//!         UtcClock ─ Meta.timestamp ───────────────────────────────┤
//!      NonceSource ─ 16-byte nonce ────────────────────────────────┤
//!      FrameSigner ─ sign(sensor_id ‖ nonce ‖ message) ────────────┤
//!                                                                  ▼
//!                                            EnvelopeBuilder ─► EnvelopeFrame     (encoded once, immutable)
//!                                                                  │
//!                                                Outbox (bounded) ◄┘
//!                                                  │
//!                                       EnvelopeTransport ─► HttpTransport ─► NetworkHandle
//! ```
//!
//! | Neighbour | How this crate uses it |
//! |-----------|------------------------|
//! | `rohi-hal` | [`SnapshotSource`] is implemented for `rohi_hal::runtime::MeasurementHandle`; the service reads the monotonic [`Clock`](rohi_hal::runtime::Clock) and uses [`Backoff`](rohi_hal::runtime::Backoff). |
//! | [`rohi_identity`] | Every [`Signer`](rohi_identity::Signer) is a [`FrameSigner`]. The firmware's runtime signer follows the identity, which can be provisioned or erased while the device runs. |
//! | [`rohi_time`] | [`TimeService`](rohi_time::TimeService) implements [`UtcClock`]; it is the UTC source of the firmware. |
//! | [`rohi_location`] | A [`LocationSource`](rohi_location::LocationSource) adds the `GPS` entry, see [`encoder`]. |
//! | [`rohi_net`] | [`HttpTransport`] runs over a [`NetworkHandle`](rohi_net::NetworkHandle). |
//! | [`rohi_config`], [`rohi_service`] | [`ConnectivityConfig`] is a configuration section; [`ConnectivityStatusService`] is a status-plane service. |
//! | [`rohi_connectivity_proto`](../rohi_connectivity_proto/index.html) | The protobuf messages, the encoder and the signing preimage. |
//!
//! The reference firmware is `firmwares/altruist-urban` (`src/connectivity.rs`):
//! one embassy task runs [`ConnectivityService::run`] and is stopped and
//! restarted with a new service whenever the `connectivity` configuration is
//! committed (see [`config`]).
//!
//! # API Navigation
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`encoder`] | snapshot → `core.v1.Message`: the measurement mapping table, unit conversions, omission policy, the location entry |
//! | [`clock`] | [`UtcClock`] (UTC of the timestamp) and [`NonceSource`] (entropy), [`NONCE_LEN`] |
//! | [`envelope`] | [`FrameSigner`] (identity boundary), [`EnvelopeBuilder`], the immutable [`EnvelopeFrame`], the signed wire contract |
//! | [`outbox`] | the bounded in-memory [`Outbox`] |
//! | [`transport`] | the [`EnvelopeTransport`] trait and the delivery error classification |
//! | [`http`] | [`HttpTransport`]: `POST /v1/telemetry` over plain TCP |
//! | [`service`] | [`ConnectivityService`]: the task that produces, queues and delivers frames; [`ConnectivityShared`] status and [`Diagnostics`] counters |
//! | [`config`] | the persistent `connectivity` section: [`ConnectivityConfig`], [`Endpoint`], firmware overrides |
//! | [`managed`] | the `connectivity` service of the status plane: state/health table, status fields and issue codes |
//!
//! # Guarantees & Limitations
//!
//! **Behaviour**
//!
//! * The message is serialized once, signed as those exact bytes and wrapped
//!   in an envelope that is serialized once. Retries are byte-identical.
//! * Failures of the uplink (Wi-Fi, DNS, TCP, server, UTC, authorization)
//!   cannot stop or delay sensing, because the service only reads snapshots.
//!   They show up as [`ConnectivityState`], [`Diagnostics`] counters and
//!   issues of the status plane.
//! * Delivery is at-least-once per queued frame while the device runs: a frame
//!   is removed after `202 Accepted` or a permanent rejection, so an
//!   acknowledgement lost on the way causes a resend of the same frame and the
//!   ingress must tolerate duplicates (the nonce is the same).
//! * The outbox is **in memory**: frames are lost on reboot or when the
//!   service is replaced; there is no flash spool. When it is full new frames
//!   are skipped ([`Diagnostics::skipped_outbox_full`]), the oldest are kept.
//! * Delivery and frame creation share one task: while a delivery attempt
//!   waits (bounded by [`http::DEFAULT_TIMEOUT_MS`], 20 s, in
//!   [`HttpTransport`]) the next frame is created afterwards, so the interval
//!   is a lower bound of the frame period.
//! * Cancelling [`ConnectivityService::run`] (dropping the future) is safe at
//!   any point and loses the queued frames; a send cancelled mid-way leaves the
//!   frame intact (see [`EnvelopeTransport::send`]).
//! * The transport is plain HTTP, without TLS: the signature authenticates the
//!   telemetry, but nothing encrypts it and the endpoint is not authenticated.
//!   Private (encrypted) sections are not produced.
//!
//! **Errors**: [`encoder::EncodeError`], [`BuildError`], [`SignError`],
//! [`TransportError`] (classified by [`TransportError::disposition`]),
//! [`ConfigError`], [`ClockError`] and [`NonceError`]; none carries secret
//! data.
//!
//! **Resource bounds** (allocation free; see also the
//! [Resource limits](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html)
//! reference):
//!
//! | Constant | Value |
//! |----------|-------|
//! | [`MAX_SENSORS_PER_MESSAGE`](rohi_connectivity_proto::limits::MAX_SENSORS_PER_MESSAGE) | 12 |
//! | [`MAX_MESSAGE_LEN`](rohi_connectivity_proto::limits::MAX_MESSAGE_LEN) | 400 B (worst case of the schema: 399 B) |
//! | [`MAX_ENVELOPE_LEN`](rohi_connectivity_proto::limits::MAX_ENVELOPE_LEN) | 544 B |
//! | [`NONCE_LEN`] | 16 B |
//! | [`DEFAULT_OUTBOX_DEPTH`](service::DEFAULT_OUTBOX_DEPTH) | 4 frames |
//! | `CONNECTIVITY_OUTBOX_DEPTH` (Altruist Urban firmware) | 4 frames |
//! | [`MAX_ENDPOINT_LEN`] | 64 B |
//!
//! A full Altruist Urban frame (7 measurements) is about 200 B. Private
//! (encrypted) sections and envelope batches are not generated: decoding
//! skips them as unknown fields.
//!
//! **Features and platforms**: the crate is `no_std`, has no Cargo features
//! and no platform dependency: everything runs on the host. Its dev-tests use
//! `rohi-identity`'s `mock` feature and `rohi-net`'s `testing` feature.
//!
//! **Validation level** (see the
//! [support matrix](https://rohi.akagi.dev/support/matrix.html)): host-tested
//! (`cargo test -p rohi-connectivity`); the Altruist Urban firmware builds in
//! CI. Delivery over real Wi-Fi to a real ingress is *not hardware-verified*.
//!
//! ## Testing and interoperability
//!
//! - Unit tests cover valid-only emission, per-field mapping, omitted and
//!   unsupported sensors, timestamp placement, the signing preimage, nonce
//!   use, byte-identical retries, status classification, backoff,
//!   outbox-full behaviour and the HTTP exchange against the in-memory IP
//!   backend (`rohi_net::testing`).
//! - `src/tests/golden.rs` holds interoperability vectors (message, preimage,
//!   Ed25519 signature, envelope) generated with the `robins` `edge` CLI
//!   (`a6c1d9c`, whose protocol submodule is exactly the pinned revision).
//!   Note: the CLI help says pressure is in hPa, but it takes pascal and
//!   encodes `101325` as `deci_pascal = 1013250`, as the schema documents.
//!   Any change of field numbering, value scaling, signing order or envelope
//!   layout fails these tests.
//! - `tests/robins_e2e.rs` is an opt-in end-to-end test: a snapshot goes
//!   through the service and [`HttpTransport`] into a real `edge gateway`,
//!   which must answer `202`:
//!
//!   ```text
//!   ROHI_ROBINS_EDGE=/path/to/robins/target/debug/edge \
//!       cargo test -p rohi-connectivity --test robins_e2e -- --ignored
//!   ```
//!
//! Compatibility assumptions: the ingress accepts `POST /v1/telemetry` with
//! `application/protobuf` and answers `202`; the sensor id must be accepted by
//! the ingress authorization (robins `auth.mode = "none"` or whitelisting the
//! device public key).

pub mod clock;
pub mod config;
pub mod encoder;
pub mod envelope;
pub mod http;
pub mod managed;
pub mod outbox;
pub mod service;
pub mod transport;

pub use clock::{ClockError, NONCE_LEN, NonceError, NonceSource, UtcClock};
pub use config::{
    ConfigError, ConnectivityConfig, ConnectivityOverrides, Endpoint, EndpointUri, MAX_ENDPOINT_LEN,
};
pub use encoder::{EncodeError, EncodeStats, EncodedMessage, UrbanEncoder};
pub use envelope::{BuildError, EnvelopeBuilder, EnvelopeFrame, FrameSigner, SignError};
pub use http::HttpTransport;
pub use managed::{ConnectivityStatusService, issue, status_field};
pub use outbox::{Outbox, OutboxFull};
pub use service::{
    ConnectivityService, ConnectivityShared, ConnectivityState, ConnectivityStatus, Diagnostics,
    ServiceConfig, SnapshotSource,
};
pub use transport::{
    Disposition, EnvelopeTransport, TransportAck, TransportError, classify_status,
};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
