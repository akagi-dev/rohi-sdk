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
//! The `network` section and service of the management plane.
//!
//! The device has one network: a Wi-Fi station with an IPv4 stack. Its
//! desired state is the persistent [`NetworkConfig`] (address mode, host
//! name, DHCP timing); the Wi-Fi station itself is the [`wifi`](crate::wifi)
//! section and service. Its observed state is the [`NetworkService`]: the
//! address and the socket usage.
//!
//! # The `network` section
//!
//! [`NetworkConfig`] holds `address_mode` ([`AddressMode::Dhcp`] or
//! [`AddressMode::Static`]), `ip`, `prefix_len`, `gateway`, `dns`, `hostname`
//! (DHCP option 12) and `dhcp_timeout` (ms). Static mode needs `ip`,
//! `prefix_len` and `gateway` on the same link. A change of the section
//! reconnects, because the address is obtained while joining the network (see
//! [`WifiHandle::set_network_config`](crate::WifiHandle::set_network_config)).
//!
//! # The `network` service
//!
//! Status fields: `ip`, `gateway`, `dns`, `tcp_in_use`, `tcp_capacity`,
//! `udp_in_use` and `udp_capacity`; the socket numbers come from a
//! [`NetworkProbe`] the firmware passes to [`NetworkService`], so this crate
//! stays free of platform types. The service has no actions: the link is
//! operated through the `wifi` service. It shares the link state with `wifi`:
//! `network` is `running` when the link is up and reports `dhcp_failed` as its
//! issue ([`issue`]); every other link error is an issue of `wifi`.

mod config;
mod managed;
mod service;

pub use config::{
    AddressMode, DEFAULT_DHCP_TIMEOUT_MS, DHCP_TIMEOUT_RANGE_MS, Hostname, Ipv4, MAX_HOSTNAME_LEN,
    NetworkConfig, NetworkOverrides, StaticIpv4,
};
pub use managed::{ADDRESS_MODE_ENUM, ADDRESS_MODES, Ipv4Codec, address_mode};
pub use service::{NetworkProbe, NetworkService, issue, status_field};
