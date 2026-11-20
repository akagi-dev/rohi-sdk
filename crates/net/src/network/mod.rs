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
//! The IPv4 settings of the Wi-Fi link: the `ip_*` fields of the `wireless`
//! section of the management plane.
//!
//! The device has one network: a Wi-Fi station with an IPv4 stack. Its
//! desired state is the [`NetworkConfig`] (address mode, host name, DHCP
//! timing); the Wi-Fi station itself is the [`wifi`](crate::wifi) module.
//! Both are shown to clients as one `wireless` section and one `wireless`
//! service, see [`WirelessConfig`](crate::WirelessConfig) and
//! [`WirelessService`](crate::WirelessService).
//!
//! # The IP fields
//!
//! [`NetworkConfig`] holds the address mode ([`AddressMode::Dhcp`] or
//! [`AddressMode::Static`], field `ip_mode`), `ip_address`,
//! `ip_prefix_len`, `ip_gateway`, `ip_dns`, `ip_hostname` (DHCP option 12)
//! and `ip_dhcp_timeout` (ms). Static mode needs the address, the prefix
//! length and the gateway on the same link. A change reconnects, because the
//! address is obtained while joining the network (see
//! [`WifiHandle::set_network_config`](crate::WifiHandle::set_network_config)).
//! The address of the link is reported as the `wifi_ip` status field.

mod config;
mod managed;

pub use config::{
    AddressMode, DEFAULT_DHCP_TIMEOUT_MS, DHCP_TIMEOUT_RANGE_MS, Hostname, Ipv4, MAX_HOSTNAME_LEN,
    NetworkConfig, StaticIpv4,
};
pub use managed::{ADDRESS_MODE_ENUM, ADDRESS_MODES, Ipv4Codec, address_mode};
