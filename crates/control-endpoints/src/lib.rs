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

//! Managed control endpoints: the `control_tcp` and `control_ble` config and
//! status, and the administrator key of signed requests.
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
//! | [`ControlAuthService`] | status and action service `control_auth` | the trusted administrator key: `set_admin_key`, `clear_admin_key` |
//! | [`AdminKeySection`] | private persisted section | the administrator public key, never shown by a config request |
//!
//! The crate knows no platform: the state lives in a
//! [`ControlAccess`](rohi_control::ControlAccess) that the firmware publishes
//! into, and the key is stored through [`AdminKeyStore`]. It is `no_std` and
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
//! **Access mode.** `open` allows every request; `restricted` lets reads
//! through and wants proof for mutations and provisioning: an Ed25519
//! signature of the administrator key on TCP, an encrypted (paired) link on
//! BLE. The mode of one endpoint never authorizes a request on the other. The
//! mode applies at once; the other fields apply at the next start, which the
//! status reports as `restart_required`, so that a change can never cut the
//! endpoint it arrives on before its response is delivered.
//!
//! **Administrator key.** One trusted 32-byte Ed25519 public key. It is
//! enrolled by the `provision`-level action `control_auth.set_admin_key`, which
//! is open on an open endpoint and signed on a restricted one, and it never
//! leaves the device: only a fingerprint is in the status. Without a key a
//! restricted TCP endpoint refuses every privileged request; it never falls
//! back to open.
//!
//! # Guarantees and Limitations
//!
//! * A signature authenticates one request to the device. It gives no
//!   confidentiality and does not authenticate the device to the client.
//! * Enrolling the first key over an open endpoint exposes the device to anyone
//!   who can reach that endpoint until it is enrolled.
//! * A factory reset removes the key and restores the defaults; recovery of a
//!   device whose key is lost is a factory reset over an endpoint that is open
//!   or paired, or physical access.

#[cfg(test)]
extern crate std;

mod auth;
mod config;
mod service;

pub use auth::{
    AdminKeySection, AdminKeyStore, AdminKeyStoreError, ControlAuthService, auth_action,
    auth_field, fingerprint,
};
pub use config::{
    ACCESS_MODES, AccessMode, ControlBleConfig, ControlBleOverrides, ControlTcpConfig,
    ControlTcpOverrides,
};
pub use service::{ControlBleService, ControlTcpService, endpoint_field};
