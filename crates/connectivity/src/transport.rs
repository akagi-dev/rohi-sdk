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
//! endpoint, named by its opaque URI, and reports an acknowledgement or a
//! classified error. It never sees decoded content, so it cannot change what
//! was signed. The implementations live in their own crates
//! (`rohi-connectivity-http`, `rohi-connectivity-meshtastic`) and are composed
//! statically by the firmware with [`Mux`](crate::Mux).
//!
//! ```
//! use rohi_connectivity::{AckKind, PacketTransport, TransportAck, TransportError};
//!
//! /// Accepts every packet and counts the bytes it was given.
//! struct Sink(usize);
//!
//! impl PacketTransport for Sink {
//!     fn handles(&self, scheme: &str) -> bool {
//!         scheme == "sink"
//!     }
//!
//!     async fn deliver(
//!         &mut self,
//!         _endpoint_uri: &str,
//!         packet: &[u8],
//!     ) -> Result<TransportAck, TransportError> {
//!         self.0 += packet.len();
//!         Ok(TransportAck { kind: AckKind::Accepted, detail: 0 })
//!     }
//! }
//! ```
//!
//! # What an acknowledgement means
//!
//! A route is complete for a packet when **any** path acknowledges it, but the
//! paths do not promise the same thing, so [`TransportAck::kind`] says which:
//!
//! | [`AckKind`] | Meaning |
//! |-------------|---------|
//! | [`Accepted`](AckKind::Accepted) | the ingress itself accepted the packet (HTTP `202`) |
//! | [`Relayed`](AckKind::Relayed) | the network delivered it to the next hop and said so (Meshtastic routing ACK of every fragment); it does **not** prove that the gateway reassembled, validated or published it |
//!
//! `delivered` counts both. Delivery is at-least-once either way.
//!
//! # Error classification
//!
//! A [`TransportError`] carries its own [`Disposition`], decided by the
//! transport that knows its protocol:
//!
//! | Disposition | What the publisher does |
//! |-------------|-------------------------|
//! | [`Transient`](Disposition::Transient) | try the next path now, retry this one later with backoff |
//! | [`Unauthorized`](Disposition::Unauthorized) | try the next path, pause this one for the long authorization backoff |
//! | [`Permanent`](Disposition::Permanent) | the packet can never be accepted: drop it, do **not** offer it to other paths |
//! | [`Incompatible`](Disposition::Incompatible) | this path cannot carry **this** packet (too large for the link, unsupported scheme): try the other path; when every path is incompatible the packet is dropped and counted. Never affects later packets and never backs the path off |

use core::fmt;

/// What the acknowledgement of a delivery promises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckKind {
    /// The ingress accepted the packet.
    Accepted,
    /// The network confirmed delivery to the next hop, without an
    /// application-level receipt.
    Relayed,
}

/// A packet was acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportAck {
    /// What the acknowledgement promises.
    pub kind: AckKind,
    /// A transport specific number for diagnostics: the HTTP status code, the
    /// number of acknowledged fragments.
    pub detail: u16,
}

/// A packet could not be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// The network cannot carry traffic right now.
    NetworkUnavailable,
    /// The local link to the radio or modem is not ready.
    LinkUnavailable,
    /// Name resolution failed.
    Dns,
    /// The connection could not be established.
    Connect,
    /// The exchange took too long.
    Timeout,
    /// The connection or link failed or closed before an answer was received.
    Io,
    /// The answer is not valid for the protocol.
    BadResponse,
    /// The far side or the network refused the packet: a transport specific
    /// `code` (an HTTP status, a routing error) and what to do about it.
    Rejected {
        /// The transport specific code, for diagnostics.
        code: u16,
        /// How the failure is handled.
        disposition: Disposition,
    },
    /// This path cannot carry this packet, for example it exceeds the size
    /// limit of the link.
    Incompatible,
    /// No backend of this build handles the scheme of the endpoint.
    Unsupported,
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
    /// This path cannot carry this packet: try the other path. The path is
    /// not penalized and later packets use it normally; a packet that no path
    /// can carry is dropped and counted.
    Incompatible,
}

impl TransportError {
    /// How the failure is handled. The only place that classifies errors.
    ///
    /// ```
    /// use rohi_connectivity::{Disposition, TransportError};
    ///
    /// assert_eq!(TransportError::Timeout.disposition(), Disposition::Transient);
    /// assert_eq!(TransportError::Incompatible.disposition(), Disposition::Incompatible);
    /// let refused = TransportError::Rejected { code: 413, disposition: Disposition::Permanent };
    /// assert_eq!(refused.disposition(), Disposition::Permanent);
    /// ```
    pub const fn disposition(self) -> Disposition {
        match self {
            Self::NetworkUnavailable
            | Self::LinkUnavailable
            | Self::Dns
            | Self::Connect
            | Self::Timeout
            | Self::Io
            | Self::BadResponse => Disposition::Transient,
            Self::Rejected { disposition, .. } => disposition,
            Self::Incompatible | Self::Unsupported => Disposition::Incompatible,
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetworkUnavailable => f.write_str("network unavailable"),
            Self::LinkUnavailable => f.write_str("link unavailable"),
            Self::Dns => f.write_str("DNS lookup failed"),
            Self::Connect => f.write_str("connection failed"),
            Self::Timeout => f.write_str("timeout"),
            Self::Io => f.write_str("connection error"),
            Self::BadResponse => f.write_str("malformed response"),
            Self::Rejected { code, .. } => write!(f, "rejected with code {code}"),
            Self::Incompatible => f.write_str("packet not carried by this path"),
            Self::Unsupported => f.write_str("unsupported endpoint scheme"),
        }
    }
}

impl core::error::Error for TransportError {}

/// Delivers sealed packets to an endpoint.
///
/// Implementations send exactly the bytes they are given and parse the
/// endpoint themselves, before any I/O.
#[allow(async_fn_in_trait)]
pub trait PacketTransport {
    /// Whether this transport delivers to endpoints of `scheme` (the text
    /// before `://`); a [`Mux`](crate::Mux) dispatches on it.
    fn handles(&self, scheme: &str) -> bool;

    /// Sends `packet` to the endpoint `endpoint_uri` and waits for the
    /// acknowledgement.
    ///
    /// Cancellation: safe at any point; the packet is not modified, so it can
    /// be sent again (the receiver may or may not have received the cancelled
    /// attempt, delivery is at-least-once).
    ///
    /// # Errors
    ///
    /// A [`TransportError`], classified by
    /// [`TransportError::disposition`]. Only an acknowledgement
    /// ([`TransportAck`]) completes the packet.
    async fn deliver(
        &mut self,
        endpoint_uri: &str,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError>;
}
