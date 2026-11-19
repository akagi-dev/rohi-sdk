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
//! Transport abstraction and error classification.
//!
//! A [`PacketTransport`] sends the exact bytes of a sealed packet to one
//! endpoint and reports an acknowledgement or a classified error. It never
//! sees decoded content, so it cannot change what was signed.
//! [`HttpTransport`](crate::HttpTransport) is the implementation shipped.
//!
//! ```
//! use rohi_connectivity::{Endpoint, PacketTransport, TransportAck, TransportError};
//!
//! /// Accepts every packet and counts the bytes it was given.
//! struct Sink(usize);
//!
//! impl PacketTransport for Sink {
//!     async fn deliver(
//!         &mut self,
//!         _endpoint: Endpoint<'_>,
//!         packet: &[u8],
//!     ) -> Result<TransportAck, TransportError> {
//!         self.0 += packet.len();
//!         Ok(TransportAck { status: 202 })
//!     }
//! }
//! ```
//!
//! # Error classification
//!
//! Centralized in [`TransportError::disposition`] and [`classify_status`]:
//!
//! | Result | Disposition |
//! |--------|-------------|
//! | `202 Accepted` | delivered: the logical route is complete for this packet |
//! | network unavailable, DNS, connect, timeout, I/O error, malformed response | [`Transient`](Disposition::Transient): try the next path, retry later with backoff |
//! | `408`, `429`, `5xx` | transient |
//! | `1xx`, `3xx`, `2xx` other than `202` (captive portal, proxy) | transient (not an acknowledgement) |
//! | `401`, `403` | [`Unauthorized`](Disposition::Unauthorized): try the next path, and the failing path pauses for the long authorization backoff |
//! | `400`, `413`, any other `4xx` | [`Permanent`](Disposition::Permanent): the packet is dropped and is **not** offered to other endpoints |

use core::fmt;

use crate::endpoint::Endpoint;

/// The ingress accepted the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportAck {
    /// Transport specific status (the HTTP status code for HTTP).
    pub status: u16,
}

/// A packet could not be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// The network cannot carry traffic right now.
    NetworkUnavailable,
    /// Name resolution failed.
    Dns,
    /// The connection could not be established.
    Connect,
    /// The exchange took too long.
    Timeout,
    /// The connection failed or closed before a response was received.
    Io,
    /// The response is not a valid HTTP response.
    BadResponse,
    /// The ingress answered with a status that is not an acceptance.
    Status(u16),
}

/// What the publisher does after a failed delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Keep the same packet: try the next path, wait with backoff and send
    /// it again.
    Transient,
    /// The packet can never be accepted: drop it and do not offer it to
    /// another endpoint.
    Permanent,
    /// The device is not authorized at this endpoint: keep the packet and try
    /// the next path, but the failing path pauses for the longest backoff, a
    /// quick retry cannot help and must not turn into a hot loop.
    Unauthorized,
}

impl TransportError {
    /// How the failure is handled. The only place that classifies errors.
    ///
    /// ```
    /// use rohi_connectivity::{Disposition, TransportError};
    ///
    /// assert_eq!(TransportError::Timeout.disposition(), Disposition::Transient);
    /// assert_eq!(TransportError::Status(503).disposition(), Disposition::Transient);
    /// // Not an acceptance (a captive portal answers 200): the packet is kept.
    /// assert_eq!(TransportError::Status(200).disposition(), Disposition::Transient);
    /// assert_eq!(TransportError::Status(401).disposition(), Disposition::Unauthorized);
    /// assert_eq!(TransportError::Status(413).disposition(), Disposition::Permanent);
    /// ```
    pub const fn disposition(self) -> Disposition {
        match self {
            Self::NetworkUnavailable
            | Self::Dns
            | Self::Connect
            | Self::Timeout
            | Self::Io
            | Self::BadResponse => Disposition::Transient,
            Self::Status(status) => classify_status_code(status),
        }
    }
}

const fn classify_status_code(status: u16) -> Disposition {
    match status {
        // Request timeout, throttling and server failures are transient.
        408 | 429 | 500..=599 => Disposition::Transient,
        401 | 403 => Disposition::Unauthorized,
        // Malformed (400), too large (413) and every other client error.
        400..=499 => Disposition::Permanent,
        // Informational, redirects and 2xx other than 202 (a captive portal,
        // a proxy) are not an acceptance: keep the packet.
        _ => Disposition::Transient,
    }
}

/// Maps an HTTP status code to the delivery result: only `202 Accepted`
/// commits the packet.
///
/// # Errors
///
/// [`TransportError::Status`] for every status other than `202`; its
/// [`disposition`](TransportError::disposition) says what to do with it.
///
/// ```
/// use rohi_connectivity::{TransportAck, TransportError, classify_status};
///
/// assert_eq!(classify_status(202), Ok(TransportAck { status: 202 }));
/// assert_eq!(classify_status(200), Err(TransportError::Status(200)));
/// ```
pub const fn classify_status(status: u16) -> Result<TransportAck, TransportError> {
    if status == 202 {
        Ok(TransportAck { status })
    } else {
        Err(TransportError::Status(status))
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetworkUnavailable => f.write_str("network unavailable"),
            Self::Dns => f.write_str("DNS lookup failed"),
            Self::Connect => f.write_str("connection failed"),
            Self::Timeout => f.write_str("timeout"),
            Self::Io => f.write_str("connection error"),
            Self::BadResponse => f.write_str("malformed response"),
            Self::Status(status) => write!(f, "rejected with status {status}"),
        }
    }
}

impl core::error::Error for TransportError {}

/// Delivers sealed packets to an endpoint.
///
/// Implementations send exactly the bytes they are given.
#[allow(async_fn_in_trait)]
pub trait PacketTransport {
    /// Sends `packet` to `endpoint` and waits for the acknowledgement.
    ///
    /// Cancellation: safe at any point; the packet is not modified, so it can
    /// be sent again (the ingress may or may not have received the cancelled
    /// attempt, delivery is at-least-once).
    ///
    /// # Errors
    ///
    /// A [`TransportError`], classified by
    /// [`TransportError::disposition`]. Only an acknowledgement
    /// ([`TransportAck`]) completes the packet.
    async fn deliver(
        &mut self,
        endpoint: Endpoint<'_>,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError>;
}
