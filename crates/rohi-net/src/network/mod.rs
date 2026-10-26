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

//! The `network` section and service of the management plane.
//!
//! The device has one network: a Wi-Fi station with an IPv4 stack. Its
//! desired state is the persistent [`NetworkConfig`] (network, address mode,
//! host name, timing); its observed state is the [`NetworkService`]: the link
//! and the address, the radio heap and the socket usage, and the actions
//! `scan`, `reconnect` and `disconnect`.

mod config;
mod managed;
mod service;

pub use config::{
    AddressMode, BACKOFF_INITIAL_RANGE_MS, BACKOFF_MAX_RANGE_MS, CONNECT_TIMEOUT_RANGE_MS,
    DEFAULT_BACKOFF_INITIAL_MS, DEFAULT_BACKOFF_MAX_MS, DEFAULT_CONNECT_TIMEOUT_MS,
    DEFAULT_DHCP_TIMEOUT_MS, DEFAULT_LINK_REFRESH_MS, DEFAULT_SCAN_TIMEOUT_MS,
    DHCP_TIMEOUT_RANGE_MS, Hostname, Ipv4, LINK_REFRESH_RANGE_MS, MAX_HOSTNAME_LEN, NetworkConfig,
    SCAN_TIMEOUT_RANGE_MS, StaticIpv4,
};
pub use managed::{ADDRESS_MODES, field as config_field};
pub use service::{HeapStats, NetworkProbe, NetworkService, scan_field, status_field};
