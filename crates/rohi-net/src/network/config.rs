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
//! The persistent `network` section: how the device gets its IPv4 address.

use core::fmt;
use core::net::Ipv4Addr;

use heapless::String;
use minicbor::{CborLen, Decode, Encode};
use rohi_config::config_overrides;

use crate::WifiError;

/// Longest DHCP host name, in bytes (the limit of the IP stack).
pub const MAX_HOSTNAME_LEN: usize = 32;

/// Default bound of obtaining an address from DHCP.
pub const DEFAULT_DHCP_TIMEOUT_MS: u32 = 20_000;

/// Inclusive bounds of the DHCP timeout, in milliseconds.
pub const DHCP_TIMEOUT_RANGE_MS: (u32, u32) = (1_000, 120_000);

fn within(value: u32, (min, max): (u32, u32)) -> bool {
    (min..=max).contains(&value)
}

/// An IPv4 address stored as its big-endian integer.
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
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct Hostname(
    #[n(0)]
    #[cbor(
        encode_with = "crate::text_codec::encode",
        decode_with = "crate::text_codec::decode",
        cbor_len = "crate::text_codec::cbor_len"
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

/// Effective network configuration (shown to clients as the `network`
/// section): how the device gets its IPv4 address.
///
/// `NetworkConfig::default()` is DHCP with the default timeout. What is
/// stored are the explicit user overrides of it, [`NetworkOverrides`].
///
/// The static address fields are used only in [`AddressMode::Static`] and
/// kept otherwise, so switching the mode back and forth does not lose them.
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

config_overrides! {
    /// Persisted overrides of the [`NetworkConfig`] section (section
    /// `NetworkConfig`).
    ///
    /// Persistent ABI: a CBOR map keyed by the field ids below, which are only
    /// ever added; a field that is not overridden is absent. The map is pinned
    /// by a golden test.
    pub struct NetworkOverrides for NetworkConfig, version 1 {
        1 => address_mode: AddressMode,
        2 => static_ip: Option<Ipv4>,
        3 => prefix_len: Option<u8>,
        4 => gateway: Option<Ipv4>,
        5 => dns: Option<Ipv4>,
        6 => hostname: Option<Hostname>,
        7 => dhcp_timeout_ms: u32,
    }
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
    pub fn validate(&self) -> Result<(), WifiError> {
        let valid = (self.address_mode == AddressMode::Dhcp || self.static_is_complete())
            && self.prefix_len.is_none_or(|len| (1..=30).contains(&len))
            && within(self.dhcp_timeout_ms, DHCP_TIMEOUT_RANGE_MS);
        if valid {
            Ok(())
        } else {
            Err(WifiError::InvalidConfig)
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

    use rohi_config::{
        ConfigOverrides, ConfigSection, ConfigStore, MemoryStorage, Override, SectionId,
    };

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

    const BUFFER: usize = 256;

    fn encode(overrides: &NetworkOverrides) -> std::vec::Vec<u8> {
        let mut buffer = [0u8; BUFFER];
        let used = rohi_config::encode_to_slice(overrides, &mut buffer).unwrap();
        assert_eq!(used, minicbor::len(overrides));
        buffer[..used].to_vec()
    }

    fn roundtrip(overrides: &NetworkOverrides) -> NetworkOverrides {
        rohi_config::decode_exact(&encode(overrides)).unwrap()
    }

    /// The overrides that make `config` the effective configuration.
    fn overrides_of(config: &NetworkConfig) -> NetworkOverrides {
        NetworkOverrides {
            address_mode: Override::Value(config.address_mode),
            static_ip: Override::Value(config.static_ip),
            prefix_len: Override::Value(config.prefix_len),
            gateway: Override::Value(config.gateway),
            dns: Override::Value(config.dns),
            hostname: Override::Value(config.hostname.clone()),
            dhcp_timeout_ms: Override::Value(config.dhcp_timeout_ms),
        }
    }

    #[test]
    fn section_id_is_derived_from_the_type_name() {
        assert_eq!(NetworkOverrides::ID, SectionId::from_name("NetworkConfig"));
        assert_eq!(NetworkOverrides::VERSION, 1);
        assert_eq!(NetworkOverrides::ID, SectionId(0x48f8_9649_eb74_bec7));
    }

    #[test]
    fn cbor_encoding_is_stable() {
        // Nothing overridden is an empty map.
        assert_eq!(encode(&NetworkOverrides::default()), [0xa0]);
        let overrides = NetworkOverrides {
            address_mode: Override::Value(AddressMode::Static),
            static_ip: Override::Value(address(192, 168, 1, 50)),
            prefix_len: Override::Value(Some(24)),
            hostname: Override::Value(Hostname::new("urban")),
            ..NetworkOverrides::default()
        };
        // Static mode, 192.168.1.50 as a big-endian integer, /24.
        assert_eq!(
            encode(&overrides),
            [
                0xa4, // map(4)
                0x01, 0x01, // 1: address_mode static
                0x02, 0x1a, 0xc0, 0xa8, 0x01, 0x32, // 2: static_ip
                0x03, 0x18, 0x18, // 3: prefix_len 24
                0x06, 0x65, b'u', b'r', b'b', b'a', b'n', // 6: hostname
            ]
        );
    }

    #[test]
    fn other_layouts_and_trailing_bytes_are_rejected() {
        // An array instead of a map.
        assert!(rohi_config::decode_exact::<NetworkOverrides>(&[0x81, 0x00]).is_err());
        let mut bytes = encode(&NetworkOverrides::default());
        bytes.push(0x00);
        assert!(rohi_config::decode_exact::<NetworkOverrides>(&bytes).is_err());
    }

    #[test]
    fn a_record_of_another_version_is_not_decoded() {
        let bytes = encode(&NetworkOverrides::default());
        assert!(NetworkOverrides::decode_versioned(1, &bytes).is_ok());
        assert!(NetworkOverrides::decode_versioned(2, &bytes).is_err());
    }

    #[test]
    fn roundtrips() {
        assert_eq!(
            roundtrip(&NetworkOverrides::default()),
            NetworkOverrides::default()
        );
        let mut full = static_config();
        full.hostname = Hostname::new("altruist-1");
        let overrides = overrides_of(&full);
        assert_eq!(roundtrip(&overrides), overrides);
        assert_eq!(overrides.over_default(), full);
    }

    #[test]
    fn nothing_is_stored_for_the_defaults() {
        assert!(NetworkOverrides::default().is_empty());
        assert_eq!(
            NetworkOverrides::default().over_default(),
            NetworkConfig::default()
        );
    }

    #[test]
    fn the_largest_value_fits_a_section() {
        let mut config = static_config();
        config.hostname = Hostname::new(&"h".repeat(MAX_HOSTNAME_LEN));
        config.dhcp_timeout_ms = u32::MAX;
        let overrides = overrides_of(&config);
        assert!(minicbor::len(&overrides) <= rohi_config::MAX_SECTION_PAYLOAD);
        assert_eq!(roundtrip(&overrides), overrides);
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

    #[test]
    fn persists_in_config_store_and_can_be_removed() {
        let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
        let overrides = overrides_of(&static_config());
        store.set(&overrides).unwrap();
        assert_eq!(store.get::<NetworkOverrides>().unwrap(), Some(overrides));
        store.remove::<NetworkOverrides>().unwrap();
        assert_eq!(store.get::<NetworkOverrides>().unwrap(), None);
    }
}
