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
//!   it towards the desired [`NetworkConfig`]; other tasks use [`WifiHandle`]
//!   (status, scan, reconnect, disconnect).
//! - [IP data plane](ip): [`NetworkHandle`] offers DNS and TCP through the
//!   standard `embedded-nal-async` and `embedded-io-async` traits, and bounded
//!   UDP sockets ([`UdpSocket`]).
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
pub mod network;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod text_codec;
pub mod wifi;

pub use ble::{
    BleConfig, BleConnectionInfo, BleError, BleGattSpec, BleName, BleOverrides, BleProbe,
    BleService, BleSettings, BleState, BleTransport, DEFAULT_BLE_PAYLOAD, MAX_BLE_NAME_LEN,
    MAX_BLE_PAYLOAD, STATES as BLE_STATES, config_field as ble_config_field, issue as ble_issue,
    state as ble_state, status_field as ble_status_field,
};
pub use ip::{IpBackend, IpUsage, NetError, NetworkHandle, UdpSocket};
pub use limits::{
    MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, MAX_WIFI_SCAN_RESULTS, MIN_WIFI_PASSWORD_LEN,
    WIFI_COMMAND_QUEUE_DEPTH,
};
pub use network::{
    ADDRESS_MODE_ENUM, ADDRESS_MODES, AddressMode, DEFAULT_DHCP_TIMEOUT_MS, DHCP_TIMEOUT_RANGE_MS,
    Hostname, Ipv4, MAX_HOSTNAME_LEN, NetworkConfig, NetworkOverrides, NetworkProbe,
    NetworkService, StaticIpv4, address_mode, config_field as network_config_field,
    issue as network_issue, status_field as network_status_field,
};
pub use wifi::{
    BACKOFF_INITIAL_RANGE_MS, BACKOFF_MAX_RANGE_MS, Backoff, CONNECT_TIMEOUT_RANGE_MS,
    DEFAULT_BACKOFF_INITIAL_MS, DEFAULT_BACKOFF_MAX_MS, DEFAULT_CONNECT_TIMEOUT_MS,
    DEFAULT_LINK_REFRESH_MS, DEFAULT_SCAN_TIMEOUT_MS, HeapStats, LINK_REFRESH_RANGE_MS, LinkConfig,
    LinkMetrics, ManagerConfig, NetworkInfo, SCAN_TIMEOUT_RANGE_MS, STATUS_RECEIVERS,
    ScanCollector, WifiBackend, WifiConfig, WifiError, WifiHandle, WifiManager, WifiNetwork,
    WifiOverrides, WifiPassword, WifiProbe, WifiScanResults, WifiSecurity, WifiService, WifiShared,
    WifiSsid, WifiState, WifiStatus, action as wifi_action, config_field as wifi_config_field,
    connect_field as wifi_connect_field, issue as wifi_issue, scan_field as wifi_scan_field,
    security as wifi_security, status_field as wifi_status_field,
};
