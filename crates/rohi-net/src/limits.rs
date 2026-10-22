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

//! Resource limits of the network layer. Every application-facing buffer and
//! queue in this crate is bounded by one of these constants.

/// Maximum SSID length in bytes (IEEE 802.11).
pub const MAX_SSID_LEN: usize = 32;
/// Maximum Wi-Fi password length in bytes (64 hex digit WPA PSK).
pub const MAX_WIFI_PASSWORD_LEN: usize = 64;
/// Minimum length of a WPA/WPA2/WPA3 passphrase.
pub const MIN_WIFI_PASSWORD_LEN: usize = 8;
/// Maximum number of networks returned by one scan.
pub const MAX_WIFI_SCAN_RESULTS: usize = 16;
/// Depth of the [`WifiManager`](crate::WifiManager) command queue.
pub const WIFI_COMMAND_QUEUE_DEPTH: usize = 4;
