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
//! The local radio as the transport sees it.
//!
//! [`MeshtasticRadio`] is the whole contract between the portable transport
//! state machine and a concrete link (UART in [`uart`](crate::uart), BLE in a
//! firmware that has it): submit one packet, receive routing results. The
//! radio does the Meshtastic protocol of its link (`ToRadio`/`FromRadio`,
//! handshake, reconnect); the transport never sees serial bytes or GATT.

/// `Routing.Error` of the Meshtastic protocol, the codes this crate looks at.
pub mod routing {
    /// Delivered.
    pub const NONE: u8 = 0;
    /// The packet is too large for the radio to carry.
    pub const TOO_LARGE: u8 = 7;
    /// The packet is not authorized.
    pub const NOT_AUTHORIZED: u8 = 33;
    /// PKI decryption or encryption failed.
    pub const PKI_FAILED: u8 = 34;
    /// The public key of the destination is not known.
    pub const PKI_UNKNOWN_PUBKEY: u8 = 35;
    /// The key of the destination could not be used to send.
    pub const PKI_SEND_FAIL_PUBLIC_KEY: u8 = 39;
}

/// One packet for the mesh.
///
/// The transport always asks for a **PKI-encrypted unicast with a routing
/// acknowledgement**; a radio that cannot do that must fail the send
/// ([`RadioError::Io`] or a routing error), never fall back to channel
/// encryption or a broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outgoing<'a> {
    /// The destination node number (the gateway). Never a broadcast address.
    pub destination: u32,
    /// The mesh packet ID, chosen by the transport and unique among its
    /// outstanding packets: the routing acknowledgement refers to it.
    pub id: u32,
    /// The Meshtastic `PortNum` of the application payload.
    pub port: u32,
    /// The `Data.payload`: one transport frame.
    pub payload: &'a [u8],
    /// Whether the radio must request a routing acknowledgement.
    pub want_ack: bool,
    /// Whether the radio must use PKI encryption.
    pub pki: bool,
}

/// The routing outcome of a packet the radio sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingResult {
    /// The [`Outgoing::id`] it answers.
    pub request_id: u32,
    /// `Routing.Error`; [`routing::NONE`] is a successful acknowledgement.
    pub error: u8,
}

/// The local radio is not usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioError {
    /// The link is not up or the radio has not finished its handshake.
    NotConnected,
    /// The link went down while the packet was in flight.
    Disconnected,
    /// The link failed.
    Io,
}

/// A local Meshtastic radio.
#[allow(async_fn_in_trait)]
pub trait MeshtasticRadio {
    /// Reserves `count` consecutive, nonzero packet IDs for the packets of one attempt. IDs
    /// are never handed out twice, even when the attempt is dropped, so a late acknowledgement
    /// of a dropped attempt cannot be taken for one of a later attempt.
    fn reserve_ids(&mut self, count: usize) -> u32;

    /// Hands `packet` to the radio. Returns once the local link has written and flushed it; the
    /// mesh outcome arrives through [`next_result`](Self::next_result).
    ///
    /// Dropping this future after the packet is queued does not withdraw it; the backend may
    /// still write it.
    ///
    /// # Errors
    ///
    /// [`RadioError::NotConnected`] if the local radio is unavailable, [`RadioError::Disconnected`]
    /// if the link ends before the write completes, or [`RadioError::Io`] if the message cannot
    /// be encoded or written.
    async fn send(&mut self, packet: &Outgoing<'_>) -> Result<(), RadioError>;

    /// Waits for the next routing result of any packet this radio sent
    /// (acknowledgements of other traffic are not reported). Cancel-safe.
    ///
    /// # Errors
    ///
    /// [`RadioError`] when the link is lost while waiting.
    async fn next_result(&mut self) -> Result<RoutingResult, RadioError>;
}
