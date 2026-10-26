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

//! The persistent `network` section: the Wi-Fi station and how the device
//! gets its IPv4 address, with the timing of connecting and reconnecting.

use core::fmt;
use core::net::Ipv4Addr;

use heapless::String;
use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;

use crate::{MIN_WIFI_PASSWORD_LEN, WifiError, WifiPassword, WifiSsid};

/// Longest DHCP host name, in bytes (the limit of the IP stack).
pub const MAX_HOSTNAME_LEN: usize = 32;

/// Default first delay before a reconnect attempt.
pub const DEFAULT_BACKOFF_INITIAL_MS: u32 = 1_000;
/// Default longest delay between reconnect attempts.
pub const DEFAULT_BACKOFF_MAX_MS: u32 = 60_000;
/// Default interval of refreshing the connection details while connected.
pub const DEFAULT_LINK_REFRESH_MS: u32 = 10_000;
/// Default bound of a network scan.
pub const DEFAULT_SCAN_TIMEOUT_MS: u32 = 20_000;
/// Default bound of associating with the access point.
pub const DEFAULT_CONNECT_TIMEOUT_MS: u32 = 30_000;
/// Default bound of obtaining an address from DHCP.
pub const DEFAULT_DHCP_TIMEOUT_MS: u32 = 20_000;

/// Inclusive bounds of the timing fields, in milliseconds.
pub const BACKOFF_INITIAL_RANGE_MS: (u32, u32) = (100, 600_000);
/// See [`BACKOFF_INITIAL_RANGE_MS`].
pub const BACKOFF_MAX_RANGE_MS: (u32, u32) = (100, 3_600_000);
/// See [`BACKOFF_INITIAL_RANGE_MS`].
pub const LINK_REFRESH_RANGE_MS: (u32, u32) = (1_000, 600_000);
/// See [`BACKOFF_INITIAL_RANGE_MS`].
pub const SCAN_TIMEOUT_RANGE_MS: (u32, u32) = (1_000, 120_000);
/// See [`BACKOFF_INITIAL_RANGE_MS`].
pub const CONNECT_TIMEOUT_RANGE_MS: (u32, u32) = (5_000, 120_000);
/// See [`BACKOFF_INITIAL_RANGE_MS`].
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

mod text_codec {
    use super::*;

    pub fn encode<C, W: Write, const N: usize>(
        value: &String<N>,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.str(value)?;
        Ok(())
    }

    pub fn decode<C, const N: usize>(
        d: &mut Decoder<'_>,
        _: &mut C,
    ) -> Result<String<N>, decode::Error> {
        let position = d.position();
        String::try_from(d.str()?)
            .map_err(|_| decode::Error::message("host name too long").at(position))
    }

    pub fn cbor_len<C, const N: usize>(value: &String<N>, ctx: &mut C) -> usize {
        <&str as CborLen<C>>::cbor_len(&value.as_str(), ctx)
    }
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
        encode_with = "text_codec::encode",
        decode_with = "text_codec::decode",
        cbor_len = "text_codec::cbor_len"
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

/// Persistent network configuration (section `NetworkConfig`, shown to
/// clients as `network`).
///
/// An open Wi-Fi network is represented by `password == None`, never by an
/// empty password. The static address fields are used only in
/// [`AddressMode::Static`] and kept otherwise, so switching the mode back and
/// forth does not lose them.
///
/// Persistent ABI: the CBOR array is pinned by a golden test; fields are
/// only ever appended.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct NetworkConfig {
    /// Network name.
    #[n(0)]
    pub ssid: WifiSsid,
    /// Passphrase, `None` for an open network.
    #[n(1)]
    pub password: Option<WifiPassword>,
    /// How the IPv4 address is obtained.
    #[n(2)]
    pub address_mode: AddressMode,
    /// Static IPv4 address.
    #[n(3)]
    pub static_ip: Option<Ipv4>,
    /// Length of the network prefix of the static address, 1 to 30.
    #[n(4)]
    pub prefix_len: Option<u8>,
    /// Static default gateway.
    #[n(5)]
    pub gateway: Option<Ipv4>,
    /// Static DNS server.
    #[n(6)]
    pub dns: Option<Ipv4>,
    /// Host name announced to the DHCP server.
    #[n(7)]
    pub hostname: Option<Hostname>,
    /// First delay before a reconnect attempt, doubling up to `backoff_max_ms`.
    #[n(8)]
    pub backoff_initial_ms: u32,
    /// Longest delay between reconnect attempts.
    #[n(9)]
    pub backoff_max_ms: u32,
    /// Interval of refreshing the connection details while connected; the
    /// link counts as stable after one such interval.
    #[n(10)]
    pub link_refresh_ms: u32,
    /// Bound of a network scan.
    #[n(11)]
    pub scan_timeout_ms: u32,
    /// Bound of associating with the access point.
    #[n(12)]
    pub connect_timeout_ms: u32,
    /// Bound of obtaining an address from DHCP.
    #[n(13)]
    pub dhcp_timeout_ms: u32,
}

impl NetworkConfig {
    /// A configuration for `ssid` and `password` with DHCP and the default
    /// timing.
    pub const fn new(ssid: WifiSsid, password: Option<WifiPassword>) -> Self {
        Self {
            ssid,
            password,
            address_mode: AddressMode::Dhcp,
            static_ip: None,
            prefix_len: None,
            gateway: None,
            dns: None,
            hostname: None,
            backoff_initial_ms: DEFAULT_BACKOFF_INITIAL_MS,
            backoff_max_ms: DEFAULT_BACKOFF_MAX_MS,
            link_refresh_ms: DEFAULT_LINK_REFRESH_MS,
            scan_timeout_ms: DEFAULT_SCAN_TIMEOUT_MS,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
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
        let valid = !self.ssid.is_empty()
            && self
                .password
                .as_ref()
                .is_none_or(|password| password.len() >= MIN_WIFI_PASSWORD_LEN)
            && (self.address_mode == AddressMode::Dhcp || self.static_is_complete())
            && self.prefix_len.is_none_or(|len| (1..=30).contains(&len))
            && within(self.backoff_initial_ms, BACKOFF_INITIAL_RANGE_MS)
            && within(self.backoff_max_ms, BACKOFF_MAX_RANGE_MS)
            && self.backoff_initial_ms <= self.backoff_max_ms
            && within(self.link_refresh_ms, LINK_REFRESH_RANGE_MS)
            && within(self.scan_timeout_ms, SCAN_TIMEOUT_RANGE_MS)
            && within(self.connect_timeout_ms, CONNECT_TIMEOUT_RANGE_MS)
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

    /// Replaces the SSID and keeps the rest.
    pub fn with_ssid(mut self, ssid: WifiSsid) -> Self {
        self.ssid = ssid;
        self
    }

    /// Replaces (`Some`) or clears (`None`) the password and keeps the rest.
    pub fn with_password(mut self, password: Option<WifiPassword>) -> Self {
        self.password = password;
        self
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

impl fmt::Debug for NetworkConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkConfig")
            .field("ssid", &self.ssid)
            .field("password", &self.password)
            .field("address_mode", &self.address_mode)
            .field("static_ip", &self.static_ip)
            .field("prefix_len", &self.prefix_len)
            .field("gateway", &self.gateway)
            .field("dns", &self.dns)
            .field("hostname", &self.hostname)
            .field("backoff_initial_ms", &self.backoff_initial_ms)
            .field("backoff_max_ms", &self.backoff_max_ms)
            .field("link_refresh_ms", &self.link_refresh_ms)
            .field("scan_timeout_ms", &self.scan_timeout_ms)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("dhcp_timeout_ms", &self.dhcp_timeout_ms)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;

    use rohi_config::{ConfigStore, MemoryStorage, SectionId};

    use super::*;

    fn ssid(value: &str) -> WifiSsid {
        WifiSsid::from_bytes(value.as_bytes()).unwrap()
    }

    fn password(value: &str) -> WifiPassword {
        WifiPassword::from_bytes(value.as_bytes()).unwrap()
    }

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
            ..NetworkConfig::new(ssid("office"), None)
        }
    }

    const BUFFER: usize = 256;

    fn encode(config: &NetworkConfig) -> std::vec::Vec<u8> {
        let mut buffer = [0u8; BUFFER];
        let used = rohi_config::encode_to_slice(config, &mut buffer).unwrap();
        assert_eq!(used, minicbor::len(config));
        buffer[..used].to_vec()
    }

    fn roundtrip(config: &NetworkConfig) -> NetworkConfig {
        rohi_config::decode_exact(&encode(config)).unwrap()
    }

    #[test]
    fn section_id_is_derived_from_the_type_name() {
        assert_eq!(NetworkConfig::ID, SectionId::from_name("NetworkConfig"));
        assert_eq!(NetworkConfig::VERSION, 1);
        assert_eq!(NetworkConfig::ID, SectionId(0x48f8_9649_eb74_bec7));
    }

    #[test]
    fn cbor_encoding_is_stable() {
        assert_eq!(
            encode(&NetworkConfig::new(ssid("ab"), None)),
            [
                0x8e, // array(14)
                0x42, 0x61, 0x62, // ssid
                0xf6, // password
                0x00, // address_mode: dhcp
                0xf6, 0xf6, 0xf6, 0xf6, 0xf6, // static_ip, prefix_len, gateway, dns, hostname
                0x19, 0x03, 0xe8, // backoff_initial_ms 1000
                0x19, 0xea, 0x60, // backoff_max_ms 60000
                0x19, 0x27, 0x10, // link_refresh_ms 10000
                0x19, 0x4e, 0x20, // scan_timeout_ms 20000
                0x19, 0x75, 0x30, // connect_timeout_ms 30000
                0x19, 0x4e, 0x20, // dhcp_timeout_ms 20000
            ]
        );
        let mut config = static_config();
        config.ssid = ssid("ab");
        config.hostname = Hostname::new("urban");
        let bytes = encode(&config);
        // Static mode, 192.168.1.50 as a big-endian integer, /24.
        assert_eq!(
            &bytes[5..],
            [
                0x01, 0x1a, 0xc0, 0xa8, 0x01, 0x32, 0x18, 0x18, 0x1a, 0xc0, 0xa8, 0x01, 0x01, 0x1a,
                0x01, 0x01, 0x01, 0x01, 0x65, b'u', b'r', b'b', b'a', b'n', 0x19, 0x03, 0xe8, 0x19,
                0xea, 0x60, 0x19, 0x27, 0x10, 0x19, 0x4e, 0x20, 0x19, 0x75, 0x30, 0x19, 0x4e, 0x20,
            ]
        );
    }

    #[test]
    fn other_layouts_and_trailing_bytes_are_rejected() {
        // The two-element layout of the former `WifiConfig`.
        assert!(rohi_config::decode_exact::<NetworkConfig>(&[0x81, 0x42, 0x61, 0x62]).is_err());
        let mut bytes = encode(&NetworkConfig::new(ssid("ab"), None));
        bytes.push(0x00);
        assert!(rohi_config::decode_exact::<NetworkConfig>(&bytes).is_err());
    }

    #[test]
    fn roundtrips() {
        let config = NetworkConfig::new(ssid("office"), Some(password("hunter2hunter2")));
        assert_eq!(roundtrip(&config), config);
        let mut full = static_config();
        full.password = Some(password("hunter2hunter2"));
        full.hostname = Hostname::new("altruist-1");
        assert_eq!(roundtrip(&full), full);
    }

    #[test]
    fn the_largest_value_fits_a_section() {
        let mut config = static_config();
        config.ssid = WifiSsid::from_bytes(&[b'a'; crate::MAX_SSID_LEN]).unwrap();
        config.password =
            Some(WifiPassword::from_bytes(&[b'p'; crate::MAX_WIFI_PASSWORD_LEN]).unwrap());
        config.hostname = Hostname::new(&"h".repeat(MAX_HOSTNAME_LEN));
        config.backoff_max_ms = u32::MAX;
        assert!(minicbor::len(&config) <= rohi_config::MAX_SECTION_PAYLOAD);
        assert_eq!(roundtrip(&config), config);
    }

    #[test]
    fn debug_never_prints_password() {
        let config = NetworkConfig::new(ssid("office"), Some(password("super-secret-pass")));
        let text = format!("{config:?}");
        assert!(!text.contains("super-secret-pass"));
        assert!(text.contains("<redacted>"));
        assert!(text.contains("office"));
    }

    #[test]
    fn defaults_validate_once_a_network_is_set() {
        assert_eq!(
            NetworkConfig::new(WifiSsid::empty(), None).validate(),
            Err(WifiError::InvalidConfig)
        );
        assert_eq!(NetworkConfig::new(ssid("a"), None).validate(), Ok(()));
        assert_eq!(
            NetworkConfig::new(ssid("a"), Some(password("short"))).validate(),
            Err(WifiError::InvalidConfig)
        );
        assert_eq!(
            NetworkConfig::new(ssid("a"), Some(password("longenough"))).validate(),
            Ok(())
        );
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
    fn timing_is_bounded_and_ordered() {
        let base = NetworkConfig::new(ssid("a"), None);
        let invalid = |config: NetworkConfig| config.validate() == Err(WifiError::InvalidConfig);
        assert!(invalid(NetworkConfig {
            backoff_initial_ms: 0,
            ..base.clone()
        }));
        assert!(invalid(NetworkConfig {
            backoff_max_ms: 500,
            ..base.clone()
        }));
        assert!(invalid(NetworkConfig {
            backoff_max_ms: u32::MAX,
            ..base.clone()
        }));
        assert!(invalid(NetworkConfig {
            link_refresh_ms: 0,
            ..base.clone()
        }));
        assert!(invalid(NetworkConfig {
            scan_timeout_ms: 0,
            ..base.clone()
        }));
        assert!(invalid(NetworkConfig {
            connect_timeout_ms: 1_000,
            ..base.clone()
        }));
        assert!(invalid(NetworkConfig {
            dhcp_timeout_ms: 0,
            ..base.clone()
        }));
        let equal = NetworkConfig {
            backoff_initial_ms: 5_000,
            backoff_max_ms: 5_000,
            ..base
        };
        assert_eq!(equal.validate(), Ok(()));
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
    fn partial_update_preserves_the_rest() {
        let config = NetworkConfig {
            password: Some(password("hunter2hunter2")),
            ..static_config()
        };
        let updated = config.clone().with_ssid(ssid("new"));
        assert_eq!(updated.ssid, ssid("new"));
        assert_eq!(updated.password, config.password);
        assert_eq!(updated.static_ip, config.static_ip);

        let cleared = updated.with_password(None);
        assert_eq!(cleared.password, None);
        assert_eq!(cleared.ssid, ssid("new"));
    }

    #[test]
    fn persists_in_config_store_and_can_be_removed() {
        let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
        let config = NetworkConfig {
            password: Some(password("hunter2hunter2")),
            ..static_config()
        };
        store.set(&config).unwrap();
        assert_eq!(store.get::<NetworkConfig>().unwrap(), Some(config));
        store.remove::<NetworkConfig>().unwrap();
        assert_eq!(store.get::<NetworkConfig>().unwrap(), None);
    }
}
