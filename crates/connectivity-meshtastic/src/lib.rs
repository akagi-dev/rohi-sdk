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
//! Delivery of sealed telemetry packets over Meshtastic, for [`rohi-connectivity`](rohi_connectivity).
//!
//! A device without a Wi-Fi uplink hands its sealed `SignedEnvelope` to a local Meshtastic radio,
//! which sends it as PKI-encrypted unicast to a gateway node running the `robins` edge ingress.
//! The radio and the session with it belong to [`rohi-meshtastic`](rohi_meshtastic); this crate
//! owns what is specific to *delivering telemetry*:
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`uri`] | `meshtastic+uart://!xxxxxxxx` / `meshtastic+ble://!xxxxxxxx`, [`MeshtasticPolicy`] |
//! | [`frame`] | Transport v1 framing: `SINGLE` and `FRAGMENT`, 219-byte MTU |
//! | [`transport`] | [`MeshtasticTransport`], the [`PacketTransport`](rohi_connectivity::PacketTransport) over any [`MeshtasticRadio`](rohi_meshtastic::MeshtasticRadio) |
//!
//! # Delivery semantics
//!
//! A delivery succeeds when **every** fragment received a matching successful routing ACK
//! ([`AckKind::Relayed`](rohi_connectivity::AckKind::Relayed)). That confirms the mesh delivered
//! the fragments; it does **not** prove that the gateway reassembled, validated or published the
//! envelope. Delivery is at-least-once, so the receiver deduplicates immutable envelopes.
//!
//! The local radio does PKI and the short-timescale retransmissions: the transport never retries
//! on its own while the radio firmware is still trying.
//!
//! # Not here
//!
//! Gateway discovery, key provisioning and rotation, application-level receipts and the BLE
//! central backend are out of scope. The UART or BLE resources are chosen by the firmware and
//! injected; they are not part of the endpoint URI or of the stored configuration.

#[cfg(test)]
extern crate std;

pub mod frame;
pub mod transport;
pub mod uri;

pub use transport::{
    DEFAULT_ACK_TIMEOUT_MS, MAX_ATTEMPT_MS, MeshtasticTransport, PORT_PRIVATE_APP,
};
pub use uri::{Endpoint, Link, MeshtasticPolicy, NodeId, SCHEME_BLE, SCHEME_UART};
