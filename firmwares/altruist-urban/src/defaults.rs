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

//! The firmware defaults of every managed config section.
//!
//! `effective = firmware defaults + persisted overrides`. These providers are
//! the one authoritative base: the generic config layer (reads, validation,
//! reset), the configuration owner task (runtime publication) and the tasks
//! that read their section at start all take the base from here, never from
//! the portable `Default` of the type directly. A product or build can differ
//! from the portable baseline; today only the connectivity section does.

use rohi_connectivity::ConnectivityConfig;
use rohi_location::LocationConfig;
use rohi_net::{BleSettings, NetworkConfig, WifiConfig};
use rohi_time_ntp::NtpServersConfig;

use crate::{connectivity, control};

/// `wifi`: no network configured.
pub fn wifi() -> WifiConfig {
    WifiConfig::default()
}

/// `network`: DHCP.
pub fn network() -> NetworkConfig {
    NetworkConfig::default()
}

/// `ble`: the name of [`control::BLE_CONFIG`].
pub fn ble() -> BleSettings {
    BleSettings {
        name: control::BLE_CONFIG.name.clone(),
    }
}

/// `connectivity`: the build-time defaults, see [`connectivity::defaults`].
pub fn connectivity() -> ConnectivityConfig {
    connectivity::defaults()
}

/// `location`: no position.
pub fn location() -> LocationConfig {
    LocationConfig::default()
}

/// `time`: the public NTP servers.
pub fn time() -> NtpServersConfig {
    NtpServersConfig::default()
}
