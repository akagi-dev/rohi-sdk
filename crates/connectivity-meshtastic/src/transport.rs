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
//! The Meshtastic [`PacketTransport`]: framing, PKI unicast, per-fragment ACKs.
//!
//! One delivery attempt frames the sealed packet ([`Framer`]), submits every
//! frame to the radio in ascending order as a PKI unicast to the gateway with
//! `want_ack`, and then waits until **every** fragment has its own successful
//! routing acknowledgement. The result is [`AckKind::Relayed`]: the mesh
//! delivered the fragments, which says nothing about reassembly or acceptance
//! by the gateway application.
//!
//! * Acknowledgements are matched by the exact mesh packet ID; unrelated, late
//!   and duplicate ones are ignored and may arrive in any order.
//! * A routing failure of any fragment ends the attempt: the radio firmware
//!   has already retried, so the transport never runs a host-side retry loop
//!   of its own; the router's cooldown and fallback take over.
//! * A payload that is empty or larger than the framing allows is
//!   [`Incompatible`](TransportError::Incompatible) and nothing is sent, so
//!   an HTTP fallback can still carry it.
//! * Dropping the attempt (a route change) is safe: the packet bytes are not
//!   modified and late acknowledgements of its fragments find no taker.

use embedded_hal_async::delay::DelayNs;
use rohi_connectivity::{AckKind, Disposition, PacketTransport, TransportAck, TransportError};
use rohi_time::with_timeout;

use crate::frame::{FrameError, Framer, MTU};
use crate::uri::{Endpoint, Link};
use rohi_meshtastic::{MeshtasticRadio, Outgoing, RadioError, RoutingResult, routing};

/// The `PortNum` of version 1 of the transport: `PRIVATE_APP`.
pub const PORT_PRIVATE_APP: u32 = 256;
/// Default wait for the acknowledgement of one fragment, in milliseconds.
pub const DEFAULT_ACK_TIMEOUT_MS: u32 = 60_000;

/// Delivers sealed packets through a local Meshtastic radio.
pub struct MeshtasticTransport<R, D> {
    link: Link,
    radio: R,
    delay: D,
    port: u32,
    ack_timeout_ms: u32,
}

impl<R: MeshtasticRadio, D: DelayNs> MeshtasticTransport<R, D> {
    /// A transport that serves the endpoints of `link` through `radio`;
    /// `delay` bounds every attempt.
    pub const fn new(link: Link, radio: R, delay: D) -> Self {
        Self {
            link,
            radio,
            delay,
            port: PORT_PRIVATE_APP,
            ack_timeout_ms: DEFAULT_ACK_TIMEOUT_MS,
        }
    }

    /// Uses another application port, for a deployment with a registered `PortNum`.
    pub const fn with_port(mut self, port: u32) -> Self {
        self.port = port;
        self
    }

    /// Replaces the wait for the acknowledgement of one fragment. An attempt
    /// lasts at most this long for each of its fragments.
    pub const fn with_ack_timeout_ms(mut self, ack_timeout_ms: u32) -> Self {
        self.ack_timeout_ms = ack_timeout_ms;
        self
    }

    async fn exchange(
        radio: &mut R,
        first_id: u32,
        endpoint: Endpoint,
        port: u32,
        framer: &Framer<'_>,
    ) -> Result<(), TransportError> {
        let count = framer.count();
        let mut frame = [0u8; MTU];
        for index in 0..count {
            let len = framer.frame(index, &mut frame);
            let id = first_id + index as u32;
            radio
                .send(&Outgoing {
                    destination: endpoint.gateway.0,
                    id,
                    port,
                    payload: &frame[..len],
                    want_ack: true,
                    pki: true,
                })
                .await
                .map_err(map_radio)?;
        }
        let mut pending: u32 = (1 << count) - 1;
        while pending != 0 {
            let RoutingResult { request_id, error } =
                radio.next_result().await.map_err(map_radio)?;
            let index = request_id.wrapping_sub(first_id) as usize;
            if index >= count {
                log::trace!(target: "meshtastic", "Unrelated routing result ignored");
                continue;
            }
            if pending & (1 << index) == 0 {
                log::trace!(target: "meshtastic", "Duplicate routing result ignored (fragment={})", index);
                continue;
            }
            if error != routing::NONE {
                return Err(map_routing(error));
            }
            pending &= !(1 << index);
        }
        Ok(())
    }
}

#[inline(never)]
fn map_radio(error: RadioError) -> TransportError {
    log::debug!(target: "meshtastic", "Radio error (err={:?})", error);
    match error {
        RadioError::NotConnected | RadioError::Disconnected => TransportError::LinkUnavailable,
        RadioError::Io => TransportError::Io,
    }
}

#[inline(never)]
fn map_routing(error: u8) -> TransportError {
    log::debug!(target: "meshtastic", "Routing failure (err={})", error);
    let disposition = match error {
        routing::TOO_LARGE => return TransportError::Incompatible,
        // The gateway key is unknown or refused: a quick retry cannot help.
        routing::NOT_AUTHORIZED
        | routing::PKI_FAILED
        | routing::PKI_UNKNOWN_PUBKEY
        | routing::PKI_SEND_FAIL_PUBLIC_KEY => Disposition::Unauthorized,
        _ => Disposition::Transient,
    };
    TransportError::Rejected {
        code: u16::from(error),
        disposition,
    }
}

impl<R: MeshtasticRadio, D: DelayNs> PacketTransport for MeshtasticTransport<R, D> {
    fn handles(&self, scheme: &str) -> bool {
        scheme == self.link.scheme()
    }

    async fn deliver(
        &mut self,
        endpoint_uri: &str,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        // Everything that can be decided without the radio is decided first.
        let endpoint = Endpoint::parse(endpoint_uri)
            .filter(|endpoint| endpoint.link == self.link)
            .ok_or(TransportError::Unsupported)?;
        let framer = Framer::new(packet).map_err(|error| {
            log::debug!(target: "meshtastic", "Packet cannot be framed (err={:?} size_b={})", error, packet.len());
            match error {
                FrameError::Empty | FrameError::TooLarge => TransportError::Incompatible,
            }
        })?;
        let count = framer.count();
        log::debug!(target: "meshtastic", "Sending packet (size_b={} fragments={})", packet.len(), count);

        let budget = self.ack_timeout_ms.saturating_mul(count as u32);
        let first_id = self.radio.reserve_ids(count);
        let exchange = Self::exchange(&mut self.radio, first_id, endpoint, self.port, &framer);
        let result = with_timeout(&mut self.delay, budget, exchange)
            .await
            .unwrap_or(Err(TransportError::Timeout));
        result?;
        Ok(TransportAck {
            kind: AckKind::Relayed,
            detail: count as u16,
        })
    }
}

#[cfg(test)]
mod tests;
