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
//! The IPv4 settings: how the device gets its IPv4 address.

use core::fmt;
use core::net::Ipv4Addr;

use crate::WifiError;
use heapless::String;
use log::debug;
use minicbor::{CborLen, Decode, Encode};

/// Longest DHCP host name, in bytes (the limit of the IP stack).
pub const MAX_HOSTNAME_LEN: usize = 32;

/// Default bound of obtaining an address from DHCP.
pub const DEFAULT_DHCP_TIMEOUT_MS: u32 = 20_000;

/// Inclusive bounds of the DHCP timeout, in milliseconds.
pub const DHCP_TIMEOUT_RANGE_MS: (u32, u32) = (1_000, 120_000);

fn within(value: u32, (min, max): (u32, u32)) -> bool {
    (min..=max).contains(&value)
}

/// An IPv4 address stored as its big-endian integer (the CBOR form of the
/// `ip`, `gateway` and `dns` properties). Convert from and to
/// [`core::net::Ipv4Addr`].
///
/// ```
/// use core::net::Ipv4Addr;
/// use rohi_net::Ipv4;
///
/// let address = Ipv4::from(Ipv4Addr::new(192, 168, 1, 50));
/// assert_eq!(Ipv4Addr::from(address), Ipv4Addr::new(192, 168, 1, 50));
/// assert_eq!(format!("{address:?}"), "192.168.1.50");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct Ipv4(#[n(0)] u32);

impl From<Ipv4Addr> for Ipv4 {
    fn from(address: Ipv4Addr) -> Self {
        Self(u32::from(address))
    }
}

impl From<Ipv4> for Ipv4Addr {
    fn from(address: Ipv4) -> Self {
        Self::from(address.0)
    }
}

impl fmt::Debug for Ipv4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&Ipv4Addr::from(*self), f)
    }
}

/// How the device gets its IPv4 address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Encode, Decode, CborLen)]
#[cbor(index_only)]
pub enum AddressMode {
    /// From the DHCP server of the network.
    #[default]
    #[n(0)]
    Dhcp,
    /// From the section: [`NetworkConfig::static_ip`], `prefix_len` and
    /// `gateway` are required.
    #[n(1)]
    Static,
}

/// The host name the device announces to the DHCP server.
///
/// One label of letters, digits and `-`, not starting or ending with `-`, at
/// most [`MAX_HOSTNAME_LEN`] bytes.
///
/// ```
/// use rohi_net::Hostname;
///
/// assert_eq!(Hostname::new("altruist-urban").unwrap().as_str(), "altruist-urban");
/// assert!(Hostname::new("").is_none());
/// assert!(Hostname::new("-lead").is_none());
/// assert!(Hostname::new("has space").is_none());
/// assert!(Hostname::new("a.b").is_none()); // one label only
/// ```
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct Hostname(
    #[n(0)]
    #[cbor(
        encode_with = "rohi_config::cbor::text::encode",
        decode_with = "rohi_config::cbor::text::decode",
        cbor_len = "rohi_config::cbor::text::cbor_len"
    )]
    String<MAX_HOSTNAME_LEN>,
);

impl Hostname {
    /// `None` if `name` is not a valid host name.
    pub fn new(name: &str) -> Option<Self> {
        let valid = !name.is_empty()
            && !name.starts_with('-')
            && !name.ends_with('-')
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        if !valid {
            return None;
        }
        String::try_from(name).ok().map(Self)
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Hostname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Effective network configuration: how the device gets its IPv4 address.
/// Clients see and change it as the `ip_*` fields of the
/// [`WirelessConfig`](crate::WirelessConfig) section.
///
/// `NetworkConfig::default()` is DHCP with the default timeout.
///
/// The static address fields are used only in [`AddressMode::Static`] and
/// kept otherwise, so switching the mode back and forth does not lose them.
///
/// In static mode `ip`, `prefix_len` (1 to 30) and `gateway` are required, the
/// gateway must be on the same network as the address and must differ from it;
/// the DHCP timeout must lie in [`DHCP_TIMEOUT_RANGE_MS`].
///
/// ```
/// use core::net::Ipv4Addr;
/// use rohi_net::{AddressMode, NetworkConfig, WifiError};
///
/// // The default is DHCP.
/// let mut config = NetworkConfig::default();
/// assert_eq!(config.address_mode, AddressMode::Dhcp);
/// assert!(config.validate().is_ok());
/// assert!(config.static_settings().is_none());
///
/// // Static mode without a complete address is invalid.
/// config.address_mode = AddressMode::Static;
/// assert_eq!(config.validate(), Err(WifiError::InvalidConfig));
///
/// config.static_ip = Some(Ipv4Addr::new(192, 168, 1, 50).into());
/// config.prefix_len = Some(24);
/// config.gateway = Some(Ipv4Addr::new(192, 168, 1, 1).into());
/// assert!(config.validate().is_ok());
/// let settings = config.static_settings().unwrap();
/// assert_eq!(settings.gateway, Ipv4Addr::new(192, 168, 1, 1));
/// assert_eq!(settings.dns, None);
///
/// // A gateway outside the network of the address is rejected.
/// config.gateway = Some(Ipv4Addr::new(10, 0, 0, 1).into());
/// assert!(config.validate().is_err());
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NetworkConfig {
    /// How the IPv4 address is obtained.
    pub address_mode: AddressMode,
    /// Static IPv4 address.
    pub static_ip: Option<Ipv4>,
    /// Length of the network prefix of the static address, 1 to 30.
    pub prefix_len: Option<u8>,
    /// Static default gateway.
    pub gateway: Option<Ipv4>,
    /// Static DNS server.
    pub dns: Option<Ipv4>,
    /// Host name announced to the DHCP server.
    pub hostname: Option<Hostname>,
    /// Bound of obtaining an address from DHCP.
    pub dhcp_timeout_ms: u32,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkConfig {
    /// DHCP with the default timeout.
    pub const fn new() -> Self {
        Self {
            address_mode: AddressMode::Dhcp,
            static_ip: None,
            prefix_len: None,
            gateway: None,
            dns: None,
            hostname: None,
            dhcp_timeout_ms: DEFAULT_DHCP_TIMEOUT_MS,
        }
    }

    /// The static address settings, `None` in [`AddressMode::Dhcp`] and when
    /// they are incomplete.
    pub fn static_settings(&self) -> Option<StaticIpv4> {
        if self.address_mode != AddressMode::Static {
            return None;
        }
        Some(StaticIpv4 {
            address: self.static_ip?.into(),
            prefix_len: self.prefix_len?,
            gateway: self.gateway?.into(),
            dns: self.dns.map(Into::into),
        })
    }

    /// Checks the configuration before it is persisted or applied.
    ///
    /// # Errors
    ///
    /// [`WifiError::InvalidConfig`] when static mode lacks `ip`, `prefix_len`
    /// or `gateway`, the gateway is not on the link of the address or equals
    /// it, `prefix_len` is outside 1 to 30, or the DHCP timeout is outside
    /// [`DHCP_TIMEOUT_RANGE_MS`].
    pub fn validate(&self) -> Result<(), WifiError> {
        let valid = (self.address_mode == AddressMode::Dhcp || self.static_is_complete())
            && self.prefix_len.is_none_or(|len| (1..=30).contains(&len))
            && within(self.dhcp_timeout_ms, DHCP_TIMEOUT_RANGE_MS);
        if valid {
            Ok(())
        } else {
            self.log_invalid();
            Err(WifiError::InvalidConfig)
        }
    }

    #[cold]
    #[inline(never)]
    fn log_invalid(&self) {
        if self.address_mode == AddressMode::Static && !self.static_is_complete() {
            debug!(target: "network", "Invalid config: static mode needs ip, prefix and gateway");
        }
        if let Some(len) = self.prefix_len
            && !(1..=30).contains(&len)
        {
            debug!(target: "network", "Invalid config: prefix length out of range (prefix_len={})", len);
        }
        if !within(self.dhcp_timeout_ms, DHCP_TIMEOUT_RANGE_MS) {
            debug!(
                target: "network",
                "Invalid config: DHCP timeout out of range (timeout_ms={} range_ms={:?})",
                self.dhcp_timeout_ms, DHCP_TIMEOUT_RANGE_MS
            );
        }
    }

    fn static_is_complete(&self) -> bool {
        let (Some(address), Some(prefix_len), Some(gateway)) =
            (self.static_ip, self.prefix_len, self.gateway)
        else {
            return false;
        };
        let (address, gateway) = (
            u32::from(Ipv4Addr::from(address)),
            u32::from(Ipv4Addr::from(gateway)),
        );
        let mask = u32::MAX
            .checked_shl(32 - u32::from(prefix_len))
            .unwrap_or(0);
        // The gateway must be on the link of the address and a different host.
        (1..=30).contains(&prefix_len) && address & mask == gateway & mask && address != gateway
    }
}

/// Complete static IPv4 settings, see [`NetworkConfig::static_settings`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaticIpv4 {
    /// Address of the device.
    pub address: Ipv4Addr,
    /// Length of the network prefix, 1 to 30.
    pub prefix_len: u8,
    /// Default gateway.
    pub gateway: Ipv4Addr,
    /// DNS server.
    pub dns: Option<Ipv4Addr>,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn address(a: u8, b: u8, c: u8, d: u8) -> Option<Ipv4> {
        Some(Ipv4Addr::new(a, b, c, d).into())
    }

    fn static_config() -> NetworkConfig {
        NetworkConfig {
            address_mode: AddressMode::Static,
            static_ip: address(192, 168, 1, 50),
            prefix_len: Some(24),
            gateway: address(192, 168, 1, 1),
            dns: address(1, 1, 1, 1),
            ..NetworkConfig::new()
        }
    }

    #[test]
    fn the_defaults_are_valid() {
        assert_eq!(NetworkConfig::new().validate(), Ok(()));
        assert_eq!(NetworkConfig::new().static_settings(), None);
    }

    #[test]
    fn static_mode_needs_a_consistent_address() {
        assert_eq!(static_config().validate(), Ok(()));
        assert_eq!(
            static_config().static_settings(),
            Some(StaticIpv4 {
                address: Ipv4Addr::new(192, 168, 1, 50),
                prefix_len: 24,
                gateway: Ipv4Addr::new(192, 168, 1, 1),
                dns: Some(Ipv4Addr::new(1, 1, 1, 1)),
            })
        );

        for broken in [
            NetworkConfig {
                static_ip: None,
                ..static_config()
            },
            NetworkConfig {
                prefix_len: None,
                ..static_config()
            },
            NetworkConfig {
                gateway: None,
                ..static_config()
            },
            // 0, 31 and 32 leave no room for a gateway and a host.
            NetworkConfig {
                prefix_len: Some(0),
                ..static_config()
            },
            NetworkConfig {
                prefix_len: Some(31),
                ..static_config()
            },
            NetworkConfig {
                gateway: address(10, 0, 0, 1),
                ..static_config()
            },
            NetworkConfig {
                gateway: address(192, 168, 1, 50),
                ..static_config()
            },
        ] {
            assert_eq!(
                broken.validate(),
                Err(WifiError::InvalidConfig),
                "{broken:?}"
            );
        }

        // The DNS server is optional; DHCP ignores and keeps the static fields.
        assert_eq!(
            NetworkConfig {
                dns: None,
                ..static_config()
            }
            .validate(),
            Ok(())
        );
        let dhcp = NetworkConfig {
            address_mode: AddressMode::Dhcp,
            ..static_config()
        };
        assert_eq!(dhcp.validate(), Ok(()));
        assert_eq!(dhcp.static_settings(), None);
        assert!(dhcp.static_ip.is_some());
    }

    #[test]
    fn the_dhcp_timeout_is_bounded() {
        let invalid = |dhcp_timeout_ms| {
            NetworkConfig {
                dhcp_timeout_ms,
                ..NetworkConfig::new()
            }
            .validate()
                == Err(WifiError::InvalidConfig)
        };
        assert!(invalid(0));
        assert!(invalid(DHCP_TIMEOUT_RANGE_MS.1 + 1));
        assert!(!invalid(DHCP_TIMEOUT_RANGE_MS.0));
        assert!(!invalid(DHCP_TIMEOUT_RANGE_MS.1));
    }

    #[test]
    fn hostnames_are_single_labels() {
        assert!(Hostname::new("altruist-1").is_some());
        assert!(Hostname::new(&"a".repeat(MAX_HOSTNAME_LEN)).is_some());
        assert!(Hostname::new("").is_none());
        assert!(Hostname::new("-a").is_none());
        assert!(Hostname::new("a-").is_none());
        assert!(Hostname::new("a.b").is_none());
        assert!(Hostname::new("a b").is_none());
        assert!(Hostname::new(&"a".repeat(MAX_HOSTNAME_LEN + 1)).is_none());
    }
}
