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
//! desired state is the persistent [`NetworkConfig`] (address mode, host
//! name, DHCP timing); the Wi-Fi station itself is the [`wifi`](crate::wifi)
//! section and service. Its observed state is the [`NetworkService`]: the
//! address and the socket usage.

mod config;
mod managed;
mod service;

pub use config::{
    AddressMode, DEFAULT_DHCP_TIMEOUT_MS, DHCP_TIMEOUT_RANGE_MS, Hostname, Ipv4, MAX_HOSTNAME_LEN,
    NetworkConfig, StaticIpv4,
};
pub use managed::{ADDRESS_MODE_ENUM, ADDRESS_MODES, address_mode, field as config_field};
pub use service::{NetworkProbe, NetworkService, SERVICE_ID, issue, status_field};
