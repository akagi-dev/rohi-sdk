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

//! Resource limits of the network layer. Every application-facing buffer and
//! queue in this crate is bounded by one of these constants.
//!
//! | Limit | Value | Where it applies |
//! |-------|-------|------------------|
//! | [`MAX_SSID_LEN`] | 32 bytes | [`WifiSsid`](crate::WifiSsid) |
//! | [`MAX_WIFI_PASSWORD_LEN`] / [`MIN_WIFI_PASSWORD_LEN`] | 64 / 8 bytes | [`WifiPassword`](crate::WifiPassword) |
//! | [`MAX_WIFI_SCAN_RESULTS`] | 16 networks | [`WifiScanResults`](crate::WifiScanResults) of one scan |
//! | [`WIFI_COMMAND_QUEUE_DEPTH`] | 4 commands | scan, reconnect, disconnect queue of the [`WifiManager`](crate::WifiManager) |
//! | [`STATUS_RECEIVERS`](crate::STATUS_RECEIVERS) | 3 tasks | waiters for Wi-Fi status changes |
//! | [`MAX_HOSTNAME_LEN`](crate::MAX_HOSTNAME_LEN) | 32 bytes | DHCP host name |
//! | [`MAX_BLE_NAME_LEN`](crate::MAX_BLE_NAME_LEN) | 29 bytes | advertised BLE name |
//! | [`MAX_BLE_PAYLOAD`](crate::MAX_BLE_PAYLOAD) | 244 bytes | largest BLE chunk |
//!
//! The reconnect timing defaults and ranges are on [`WifiConfig`](crate::WifiConfig)
//! (backoff 1 s doubling up to 60 s, link refresh 10 s, scan timeout 20 s,
//! connect timeout 30 s), the DHCP timeout on [`NetworkConfig`](crate::NetworkConfig)
//! (20 s). The number of TCP connections and UDP sockets and their buffer sizes
//! are not constants of this crate: the product sizes them in the platform
//! backend (see [`IpUsage`](crate::IpUsage) and
//! `NetResources` of [`rohi-net-esp`](../../rohi_net_esp/index.html)). A scan lists
//! at most six networks to a client of the `wifi` service. The numbers of the
//! whole SDK are collected at
//! <https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html>.
//!
//! # Example
//!
//! The limits are enforced where the values are built:
//!
//! ```
//! use rohi_net::{
//!     MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, MIN_WIFI_PASSWORD_LEN, WifiConfig, WifiPassword,
//!     WifiSsid,
//! };
//!
//! assert!(WifiSsid::from_bytes(&[b'a'; MAX_SSID_LEN]).is_some());
//! assert!(WifiSsid::from_bytes(&[b'a'; MAX_SSID_LEN + 1]).is_none());
//! assert!(WifiPassword::from_bytes(&[b'a'; MAX_WIFI_PASSWORD_LEN + 1]).is_none());
//!
//! // The minimum is checked when a configuration is validated.
//! let short = WifiPassword::from_bytes(&[b'a'; MIN_WIFI_PASSWORD_LEN - 1]).unwrap();
//! let ssid = WifiSsid::from_bytes(b"office").unwrap();
//! assert!(WifiConfig::new(ssid, Some(short)).validate().is_err());
//! ```

/// Maximum SSID length in bytes (IEEE 802.11).
pub const MAX_SSID_LEN: usize = 32;
/// Maximum Wi-Fi password length in bytes (64 hex digit WPA PSK).
pub const MAX_WIFI_PASSWORD_LEN: usize = 64;
/// Minimum length of a WPA/WPA2/WPA3 passphrase.
pub const MIN_WIFI_PASSWORD_LEN: usize = 8;
/// Maximum number of networks returned by one scan.
pub const MAX_WIFI_SCAN_RESULTS: usize = 16;
/// Depth of the [`WifiManager`](crate::WifiManager) command queue (transient
/// commands only: scan, reconnect, disconnect).
pub const WIFI_COMMAND_QUEUE_DEPTH: usize = 4;
