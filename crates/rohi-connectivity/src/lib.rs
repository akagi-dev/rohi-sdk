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
#![no_std]

//! Telemetry for Robonomics Open Hardware: immutable measurement snapshots
//! become signed `airalab/connectivity-protocol` frames that a pluggable
//! transport delivers.
//!
//! ```text
//! MeasurementStore ─snapshot─► UrbanEncoder ─► core.v1.Message  (encoded once)
//!                                                   │
//!             UtcClock ── Meta.timestamp ───────────┤
//!             NonceSource ── nonce ─────────────────┤
//!             FrameSigner ── sign(sensor_id‖nonce‖message)
//!                                                   ▼
//!                                  EnvelopeBuilder ─► EnvelopeFrame (encoded once)
//!                                                   │
//!                                       Outbox ◄────┘ bounded, in memory
//!                                         │
//!                                  EnvelopeTransport ─► HttpTransport ─► NetworkHandle
//! ```
//!
//! # Guarantees
//!
//! - The message is serialized once, signed as those exact bytes, wrapped in a
//!   `SignedEnvelope` that is serialized once. A retry sends the same
//!   [`EnvelopeFrame`] bytes: nonce, timestamp and signature never change.
//! - Only `Valid` measurements are sent; a sensor without a representation in
//!   the pinned schema is omitted, never relabeled (see [`encoder`]).
//! - `Meta.timestamp` is Unix UTC milliseconds from a [`UtcClock`]. Monotonic
//!   time is never used as UTC; without UTC no frame is created.
//! - Private key bytes never reach this crate: it only sees a
//!   [`FrameSigner`].
//! - Everything is bounded and allocation free. No uplink failure can stop or
//!   delay sensing.

pub mod clock;
pub mod config;
pub mod encoder;
pub mod envelope;
pub mod http;
mod managed;
pub mod outbox;
pub mod service;
pub mod transport;

pub use clock::{ClockError, NONCE_LEN, NonceError, NonceSource, UtcClock};
pub use config::{ConfigError, ConnectivityConfig, ConnectivityOverrides, Endpoint, Host};
pub use encoder::{EncodeError, EncodeStats, EncodedMessage, UrbanEncoder};
pub use envelope::{BuildError, EnvelopeBuilder, EnvelopeFrame, FrameSigner, SignError};
pub use http::HttpTransport;
pub use managed::{ConnectivityStatusService, field as config_field, issue, status_field};
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
