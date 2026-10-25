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
//! Transport abstraction and error classification.

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
    /// be sent again.
    async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError>;
}
