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

//! The firmware overrides of every managed config section: the product policy.
//!
//! `effective = C::default() + firmware overrides + user overrides`. These
//! providers are the one source of the firmware layer: the generic config layer
//! (reads, validation, reset), the configuration owner task (runtime
//! publication) and the tasks that read their section at start all take it from
//! here. The overrides are compiled into the firmware and never persisted, so a
//! new firmware changes every field the user did not override. Only the
//! connectivity section has a product policy today.

use rohi_connectivity::ConnectivityOverrides;
use rohi_location::LocationOverrides;
use rohi_net::{BleOverrides, NetworkOverrides, WifiOverrides};
use rohi_time_ntp::NtpServersOverrides;

use crate::connectivity;

/// `wifi`: no policy, no network configured.
pub fn wifi() -> WifiOverrides {
    WifiOverrides::default()
}

/// `network`: no policy, DHCP.
pub fn network() -> NetworkOverrides {
    NetworkOverrides::default()
}

/// `ble`: no policy, the name of [`crate::control::BLE_CONFIG`].
pub fn ble() -> BleOverrides {
    BleOverrides::default()
}

/// `connectivity`: the build-time policy, see [`connectivity::firmware_overrides`].
pub fn connectivity() -> ConnectivityOverrides {
    connectivity::firmware_overrides()
}

/// `location`: no policy, no position.
pub fn location() -> LocationOverrides {
    LocationOverrides::default()
}

/// `time`: no policy, the public NTP servers.
pub fn time() -> NtpServersOverrides {
    NtpServersOverrides::default()
}
