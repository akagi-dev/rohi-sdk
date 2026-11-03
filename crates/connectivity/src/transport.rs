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
//! [`EnvelopeTransport`] operates on the immutable
//! [`EnvelopeFrame`]: a transport cannot change what was
//! signed. [`HttpTransport`](crate::HttpTransport) is the implementation
//! shipped.
//!
//! ```
//! use rohi_connectivity::{EnvelopeFrame, EnvelopeTransport, TransportAck, TransportError};
//!
//! /// Accepts every frame and counts the bytes it was given.
//! struct Sink(usize);
//!
//! impl EnvelopeTransport for Sink {
//!     async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError> {
//!         self.0 += frame.len();
//!         Ok(TransportAck { status: 202 })
//!     }
//! }
//! ```
//!
//! [`HttpTransport`](crate::HttpTransport) is the maintained implementation; a
//! complete host exchange against an in-memory network is in its
//! documentation.
//!
//! # Error classification
//!
//! Centralized in [`TransportError::disposition`] and [`classify_status`]:
//!
//! | Result | Disposition |
//! |--------|-------------|
//! | `202 Accepted` | delivered, frame removed |
//! | network unavailable, DNS, connect, timeout, I/O error, malformed response | [`Retry`](Disposition::Retry): the same frame with backoff |
//! | `408`, `429`, `5xx` | retry with backoff |
//! | `1xx`, `3xx`, `2xx` other than `202` (captive portal, proxy) | retry (not an acceptance) |
//! | `401`, `403` | [`Unauthorized`](Disposition::Unauthorized): frame dropped, pause for the longest backoff (no hot loop) |
//! | `400`, `413`, any other `4xx` | [`Permanent`](Disposition::Permanent): frame dropped |
//!
//! The service applies the disposition in
//! [`ConnectivityService::deliver`](crate::ConnectivityService::deliver) and
//! its backoff is described in [`service`](crate::service).

use core::fmt;

use crate::envelope::EnvelopeFrame;

/// The ingress accepted the frame; it can be removed from the outbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportAck {
    /// Transport specific status (the HTTP status code for HTTP).
    pub status: u16,
}

/// A frame could not be delivered.
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

/// What the service does after a failed delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Keep the same frame, wait (with backoff) and send it again.
    Retry,
    /// The frame can never be accepted: drop it.
    Permanent,
    /// The device is not authorized or not provisioned at the ingress: drop
    /// the frame and pause for the longest backoff, a quick retry cannot
    /// help and must not turn into a hot loop.
    Unauthorized,
}

impl TransportError {
    /// How the failure is handled. The only place that classifies errors.
    ///
    /// ```
    /// use rohi_connectivity::{Disposition, TransportError};
    ///
    /// assert_eq!(TransportError::Timeout.disposition(), Disposition::Retry);
    /// assert_eq!(TransportError::Status(503).disposition(), Disposition::Retry);
    /// // Not an acceptance (a captive portal answers 200): the frame is kept.
    /// assert_eq!(TransportError::Status(200).disposition(), Disposition::Retry);
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
            | Self::BadResponse => Disposition::Retry,
            Self::Status(status) => classify_status_code(status),
        }
    }
}

const fn classify_status_code(status: u16) -> Disposition {
    match status {
        // Request timeout, throttling and server failures are transient.
        408 | 429 | 500..=599 => Disposition::Retry,
        401 | 403 => Disposition::Unauthorized,
        // Malformed (400), too large (413) and every other client error.
        400..=499 => Disposition::Permanent,
        // Informational, redirects and 2xx other than 202 (a captive portal,
        // a proxy) are not an acceptance: keep the frame.
        _ => Disposition::Retry,
    }
}

/// Maps an HTTP status code to the delivery result: only `202 Accepted`
/// commits the frame.
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

/// Delivers immutable envelope frames.
///
/// Implementations send exactly [`EnvelopeFrame::as_bytes`]; they never see
/// decoded protocol structures, so a transport cannot change what was signed.
#[allow(async_fn_in_trait)]
pub trait EnvelopeTransport {
    /// Sends `frame` and waits for the acknowledgement.
    ///
    /// Cancellation: safe at any point; the frame is not modified, so it can
    /// be sent again (the ingress may or may not have received the cancelled
    /// attempt).
    ///
    /// # Errors
    ///
    /// A [`TransportError`], classified by
    /// [`TransportError::disposition`]. Only an acknowledgement
    /// ([`TransportAck`]) lets the service remove the frame.
    async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError>;
}
