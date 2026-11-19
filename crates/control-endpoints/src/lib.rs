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

//! Managed control endpoints: the `control_tcp` and `control_ble` config and
//! status, and the pre-shared key of the TCP endpoint.
//!
//! # Overview
//!
//! The control plane has one worker and any number of transport endpoints, see
//! [`rohi_control`]. This crate makes the endpoints manageable like every other
//! part of a ROHI device, through the normal Config, Status and Action
//! registries and not through a side channel:
//!
//! | Item | Kind | What it is |
//! |------|------|------------|
//! | [`ControlTcpConfig`] | config section `control_tcp` | `enabled`, `access_mode`, `bind_address`, `port` of the TCP endpoint |
//! | [`ControlBleConfig`] | config section `control_ble` | `enabled`, `access_mode` of the BLE endpoint |
//! | [`ControlTcpService`], [`ControlBleService`] | status services `control_tcp`, `control_ble` | lifecycle, access mode, connections, counters, pending changes |
//! | [`ControlPskService`] | status and action service `control_psk` | the pre-shared key of the TCP endpoint: `set_tcp_psk`, `clear_tcp_psk` |
//! | [`TcpPskSection`] | private persisted section | the pre-shared key, never shown by a request, a status or a log |
//!
//! The crate knows no platform: the state lives in a
//! [`ControlAccess`](rohi_control::ControlAccess) that the firmware publishes
//! into, and the key is stored through [`PskStore`]. It is `no_std` and
//! does not allocate.
//!
//! # Quick Example
//!
//! The defaults of the two endpoints: TCP is restricted, BLE is open.
//!
//! ```
//! use rohi_control::TransportAccessMode;
//! use rohi_control_endpoints::{AccessMode, ControlBleConfig, ControlTcpConfig};
//!
//! let tcp = ControlTcpConfig::default();
//! assert_eq!(tcp.access_mode, AccessMode::Restricted);
//! assert_eq!(tcp.port, 9300);
//! assert_eq!(tcp.settings().mode, TransportAccessMode::Restricted);
//! assert_eq!(ControlBleConfig::default().access_mode, AccessMode::Open);
//! ```
//!
//! # Core Concepts
//!
//! **Access mode.** `open` allows every request; `restricted` wants a secure link: on TCP
//! the authenticated and encrypted connection of the handshake with the pre-shared key (the
//! endpoint serves no other client), on BLE an encrypted (paired) link for mutations and
//! provisioning. The mode of one endpoint never authorizes a request on the other. The
//! mode applies at once; the other fields apply at the next start, which the
//! status reports as `restart_required`, so that a change can never cut the
//! endpoint it arrives on before its response is delivered.
//!
//! **Pre-shared key.** One 32-byte secret shared with the clients. It is set by the
//! `provision`-level action `control_psk.set_tcp_psk` over a protected link only (an
//! encrypted BLE link, or the TCP connection the current key established): there is no
//! unauthenticated TCP bootstrapping. It never leaves the device: it is in no response, no status
//! and no log; only the action result shows a fingerprint. Without a key a restricted TCP
//! endpoint serves no client; it never falls back to open or to plaintext. Setting or removing
//! the key ends the TCP connections of the old one.
//!
//! # Guarantees and Limitations
//!
//! * The key is not derived from a password and not rotated automatically: provision a random
//!   32-byte value and replace it through a protected link when needed.
//! * The TCP transport uses the key for authentication and for session keys, see
//!   `rohi-control-tcp`; it has **no forward secrecy**: whoever learns the key later can decrypt
//!   a recorded session.
//! * A factory reset removes the key and restores the defaults; recovery of a device whose key
//!   is lost is a factory reset over a paired BLE link, or physical access.

#[cfg(test)]
extern crate std;

mod config;
mod psk;
mod service;

pub use config::{
    ACCESS_MODES, AccessMode, ControlBleConfig, ControlBleOverrides, ControlTcpConfig,
    ControlTcpOverrides,
};
pub use psk::{
    ControlPskService, PSK_LEN, PskStore, PskStoreError, TcpPskSection, fingerprint, psk_action,
    psk_field,
};
pub use service::{ControlBleService, ControlTcpService, endpoint_field};
