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

//! Platform-independent networking for Robonomics Open Hardware.
//!
//! Two planes, both expressed only in ROHI and standard embedded Rust types:
//!
//! - [Wi-Fi control plane](wifi): [`WifiManager`] owns the radio and reconciles
//!   it towards the desired [`WifiConfig`]; other tasks use [`WifiHandle`]
//!   (status, scan, reconnect, disconnect).
//! - [IP data plane](ip): [`NetworkHandle`] offers DNS and TCP through the
//!   standard `embedded-nal-async` and `embedded-io-async` traits.
//!
//! The BLE peripheral transport ([`BleTransport`]) lives here too. Platform
//! details (radio drivers, IP stack, radio heap) live in platform crates such
//! as `rohi-net-esp` behind [`WifiBackend`], [`IpBackend`] and
//! [`BleTransport`] and never appear in a public signature. Nothing here
//! allocates.

#[cfg(test)]
extern crate std;

mod ble;
pub mod ip;
mod limits;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod wifi;

pub use ble::{
    BleConfig, BleConnectionInfo, BleError, BleGattSpec, BleState, BleTransport,
    DEFAULT_BLE_PAYLOAD, MAX_BLE_PAYLOAD,
};
pub use ip::{IpBackend, NetError, NetworkHandle};
pub use limits::{
    MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, MAX_WIFI_SCAN_RESULTS, MIN_WIFI_PASSWORD_LEN,
    WIFI_COMMAND_QUEUE_DEPTH,
};
pub use wifi::{
    Backoff, ManagerConfig, NetworkInfo, STATUS_RECEIVERS, ScanCollector, WifiBackend, WifiConfig,
    WifiError, WifiHandle, WifiManager, WifiNetwork, WifiPassword, WifiScanResults, WifiSecurity,
    WifiShared, WifiSsid, WifiState, WifiStatus,
};
