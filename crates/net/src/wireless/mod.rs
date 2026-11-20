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
//! The `wireless` section and service of the management plane: Wi-Fi, IP and
//! BLE as one public interface.
//!
//! The device has one radio side with three separately implemented parts: the
//! Wi-Fi station ([`wifi`](crate::wifi)), its IPv4 stack
//! ([`network`](crate::network)) and the BLE peripheral ([`ble`](crate::ble)).
//! Clients do not see the split: they get one config section,
//! [`WirelessConfig`], and one service, [`WirelessService`], both named
//! `wireless`.
//!
//! | Plane | What |
//! |-------|------|
//! | Config `wireless` | 15 fields: `wifi_*` (7), `ip_*` (7) and `ble_name`; see [`WirelessConfig`] |
//! | Status | `wifi_state`, `wifi_ip`, `wifi_rssi`, `wifi_reconnects`, `heap_free` |
//! | Actions | `wifi_scan` (read), `wifi_reconnect` and `wifi_disconnect` (mutate) |
//! | Health | the worst of the Wi-Fi link and the BLE peripheral, with typed [`issue`]s |
//!
//! Wi-Fi and IP changes apply live (the firmware publishes
//! [`WirelessConfig::wifi`] and [`WirelessConfig::network`] to the
//! [`WifiManager`](crate::WifiManager) right after a commit); the BLE name is
//! read once, when the peripheral starts, so it applies after a restart.

mod config;
mod service;

pub use config::{WirelessConfig, WirelessOverrides};
pub use service::{
    HeapStats, WirelessProbe, WirelessService, action, issue, scan_field, security, status_field,
    wifi_state,
};
