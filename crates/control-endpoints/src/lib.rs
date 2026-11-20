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

//! Managed control endpoints: the `control` config and service, with the pre-shared key of the
//! TCP endpoint.
//!
//! # Overview
//!
//! The control plane has one worker and any number of transport endpoints, see
//! [`rohi_control`]. This crate makes the endpoints manageable like every other
//! part of a ROHI device, through the normal Config, Status and Action
//! registries and not through a side channel, and under one public name, `control`: the
//! transports (BLE, TCP) stay internal.
//!
//! | Item | Kind | What it is |
//! |------|------|------------|
//! | [`ControlConfig`] | config section `control` | `tcp_access_mode`, `tcp_bind_address`, `tcp_port` |
//! | [`ControlService`] | status and action service `control` | five status fields (`ble_sessions`, `tcp_sessions`, `tcp_state`, `requests`, `errors`), aggregated health and issues; the action `tcp_generate_psk` |
//! | [`TcpPskSection`] | private persisted section | the pre-shared key, never shown by a request, a status or a log |
//!
//! The crate knows no platform: the state lives in a
//! [`ControlAccess`](rohi_control::ControlAccess) that the firmware publishes
//! into, and the key is stored through [`PskStore`]. It is `no_std` and
//! does not allocate.
//!
//! # Quick Example
//!
//! The defaults of the TCP endpoint: protected, on loopback.
//!
//! ```
//! use rohi_control::TransportAccessMode;
//! use rohi_control_endpoints::{AccessMode, ControlConfig};
//!
//! let control = ControlConfig::default();
//! assert_eq!(control.tcp_access_mode, AccessMode::Protected);
//! assert_eq!(control.tcp_port, 9300);
//! assert_eq!(control.settings().mode, TransportAccessMode::Protected);
//! ```
//!
//! # Core Concepts
//!
//! **No enable flag.** A service that is part of the firmware always starts, with the defaults
//! and the stored settings on top; an absent section is the defaults, not a disabled service. A
//! failure to start is reported as `failed` in the status and as the `endpoint_failed` issue. The
//! settings of the TCP endpoint apply together at the next start (a reboot); a stored change is
//! never applied partly. Only a platform that has a TCP endpoint registers the `control` section.
//!
//! **TCP access mode.** What a client may do without the pre-shared key; a client that completed
//! the handshake with the key may do everything in every mode.
//!
//! | `tcp_access_mode` | read | mutate | provision |
//! |-------------------|------|--------|-----------|
//! | `open` | yes | yes | yes |
//! | `protected` (default) | yes | no | no |
//! | `private` | no | no | no |
//!
//! The mode is authorization; it is separate from link confidentiality. An action marked
//! `secure_link` (`tcp_generate_psk`) additionally needs the authenticated and encrypted
//! connection of the handshake, so `open` never lets a key travel in plaintext TCP. The state of
//! the endpoint does not depend on the key: a running endpoint without a key stays `running`.
//!
//! **BLE is open.** In this build the BLE endpoint has no pairing, no link encryption and no
//! mode: every request is allowed, a BLE link is never reported as encrypted or authenticated,
//! and the `control` status raises the `unsecured` issue while it runs. This is an accepted
//! insecure state of the current phase, not a configuration: anyone in radio range can change the
//! configuration, provision the identity, reset the device and run `tcp_generate_psk`.
//!
//! **Pre-shared key.** One 32-byte secret shared with the clients. The device always generates
//! it: the action `control.tcp_generate_psk` makes a new random key on every call, stores it,
//! ends the TCP connections of the old key and returns the new key in its result. No client
//! chooses a key and no action reads the current one; it is in no status, config or log.
//! Without a key a `private` TCP endpoint serves nobody and a `protected` one only reads; a
//! client that asks for the handshake is refused, never served in plaintext instead. Only a
//! factory reset erases the key.
//!
//! # Guarantees and Limitations
//!
//! * The key is not derived from a password and not rotated automatically.
//! * `tcp_generate_psk` returns the key to whoever may run it: over TCP the client of the current
//!   key, and over open BLE any central in range. The key is wiped from the memory this crate
//!   holds when the action ends or is cancelled; copies in transport buffers are not tracked.
//! * The TCP transport uses the key for authentication and for session keys, see
//!   `rohi-control-tcp`; it has **no forward secrecy**: whoever learns the key later can decrypt
//!   a recorded session.
//! * A factory reset removes the key and restores the defaults.

#[cfg(test)]
extern crate std;

mod config;
mod psk;
mod service;

pub use config::{ACCESS_MODES, AccessMode, ControlConfig, ControlOverrides};
pub use psk::{PSK_LEN, PskStore, PskStoreError, TcpPskSection, fingerprint};
pub use service::{ControlService, control_action, control_field, issue, psk_result};
