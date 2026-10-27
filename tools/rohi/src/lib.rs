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
//! Desktop client library of ROHI devices.
//!
//! Layers, from the bottom:
//!
//! - [`BleLink`] / [`scan`]: BLE central built on `btleplug`, finds devices
//!   exposing the ROHI Control Service and moves chunks;
//! - [`Session`]: frames requests with [`rohi_control_ble::frame`], matches
//!   responses by request id, enforces timeouts;
//! - [`Device`]: generic async API (`config_*`, `services*`, `invoke`)
//!   returning the plain types of [`model`], never protobuf; it knows no
//!   module by name;
//! - [`format`](mod@format): human readable rendering of the model.
//!
//! The `rohi` command line tool is a thin layer over [`Device`]. Another
//! application can use the same library, or implement [`ClientTransport`] for
//! a different link and keep everything above it.

mod ble;
#[cfg(feature = "cli")]
pub mod cli;
mod client;
mod convert;
mod error;
pub mod format;
pub mod model;
mod provision;
mod session;

pub use ble::{BleLink, DiscoveredDevice, scan, scan_until};
pub use client::Device;
pub use error::{Error, Result};
pub use provision::DerivedIdentity;
#[cfg(feature = "provision")]
pub use provision::derive_identity;
#[cfg(feature = "provision")]
pub use provision::ss58;
pub use rohi_identity::PublicKey;
pub use session::{ClientTransport, DEFAULT_TIMEOUT, Session};
