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

use core::net::Ipv4Addr;

use crate::WifiSsid;

/// Snapshot of the current network connection. Never contains credentials.
///
/// ```
/// use core::net::Ipv4Addr;
/// use rohi_net::{NetworkInfo, WifiSsid};
///
/// let info = NetworkInfo {
///     ssid: WifiSsid::from_bytes(b"office").unwrap(),
///     rssi_dbm: -61,
///     channel: Some(11),
///     mac: None,
///     ipv4: Ipv4Addr::new(10, 0, 0, 42),
///     gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
///     dns: None,
/// };
/// assert_eq!(info.ssid.as_str(), Some("office"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkInfo {
    /// Name of the connected network.
    pub ssid: WifiSsid,
    /// Signal strength in dBm; the ESP backend reports `i8::MIN` when the
    /// driver cannot tell.
    pub rssi_dbm: i8,
    /// Channel of the access point, if the driver reports it.
    pub channel: Option<u8>,
    /// Hardware address of the station interface.
    pub mac: Option<[u8; 6]>,
    /// IPv4 address, from DHCP or the static configuration.
    pub ipv4: Ipv4Addr,
    /// Default gateway, if the configuration has one.
    pub gateway: Option<Ipv4Addr>,
    /// First DNS server, if the configuration has one.
    pub dns: Option<Ipv4Addr>,
}
