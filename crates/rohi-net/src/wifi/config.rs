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

use core::fmt;

use heapless::Vec;
use minicbor::bytes::ByteSlice;
use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::config_overrides;

use crate::network::NetworkConfig;
use crate::{MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, MIN_WIFI_PASSWORD_LEN, WifiError};

/// CBOR codec of a bounded byte vector: a CBOR byte string.
mod bytes_codec {
    use super::*;

    pub fn encode<C, W: Write, const N: usize>(
        value: &Vec<u8, N>,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.bytes(value)?;
        Ok(())
    }

    pub fn decode<C, const N: usize>(
        d: &mut Decoder<'_>,
        _: &mut C,
    ) -> Result<Vec<u8, N>, decode::Error> {
        let position = d.position();
        Vec::from_slice(d.bytes()?)
            .map_err(|_| decode::Error::message("byte string too long").at(position))
    }

    pub fn cbor_len<C, const N: usize>(value: &Vec<u8, N>, ctx: &mut C) -> usize {
        <&ByteSlice>::from(value.as_slice()).cbor_len(ctx)
    }
}

/// Wi-Fi network name: 0..=[`MAX_SSID_LEN`] arbitrary bytes.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct WifiSsid(
    #[n(0)]
    #[cbor(
        encode_with = "bytes_codec::encode",
        decode_with = "bytes_codec::decode",
        cbor_len = "bytes_codec::cbor_len"
    )]
    Vec<u8, MAX_SSID_LEN>,
);

impl WifiSsid {
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    /// Returns `None` when `value` is longer than [`MAX_SSID_LEN`].
    pub fn from_bytes(value: &[u8]) -> Option<Self> {
        Vec::from_slice(value).ok().map(Self)
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// The SSID as text, if it is valid UTF-8.
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(self.as_bytes()).ok()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for WifiSsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(text) => write!(f, "{text:?}"),
            None => write!(f, "{:02x?}", self.as_bytes()),
        }
    }
}

impl fmt::Display for WifiSsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(text) => f.write_str(text),
            None => write!(f, "{:02x?}", self.as_bytes()),
        }
    }
}

/// Wi-Fi password. Secret: its `Debug` output never reveals the value.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct WifiPassword(
    #[n(0)]
    #[cbor(
        encode_with = "bytes_codec::encode",
        decode_with = "bytes_codec::decode",
        cbor_len = "bytes_codec::cbor_len"
    )]
    Vec<u8, MAX_WIFI_PASSWORD_LEN>,
);

impl WifiPassword {
    /// Returns `None` when `value` is longer than [`MAX_WIFI_PASSWORD_LEN`].
    pub fn from_bytes(value: &[u8]) -> Option<Self> {
        Vec::from_slice(value).ok().map(Self)
    }

    /// The raw secret. Only the platform backend should call this.
    pub fn expose_secret(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for WifiPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WifiPassword(<redacted>)")
    }
}

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

fn within(value: u32, (min, max): (u32, u32)) -> bool {
    (min..=max).contains(&value)
}

/// Effective Wi-Fi configuration (shown to clients as the `wifi` section):
/// the network to join and the timing of joining it.
///
/// `WifiConfig::default()` is the firmware default: no network (an empty
/// SSID, not valid on its own) and the default timing. What is stored are the
/// explicit user overrides of it, [`WifiOverrides`].
///
/// An open network is represented by `password == None`, never by an empty
/// password.
#[derive(Clone, PartialEq, Eq)]
pub struct WifiConfig {
    /// Network name.
    pub ssid: WifiSsid,
    /// Passphrase, `None` for an open network.
    pub password: Option<WifiPassword>,
    /// Bound of a network scan.
    pub scan_timeout_ms: u32,
    /// Bound of associating with the access point.
    pub connect_timeout_ms: u32,
    /// First delay before a reconnect attempt, doubling up to `backoff_max_ms`.
    pub backoff_initial_ms: u32,
    /// Longest delay between reconnect attempts.
    pub backoff_max_ms: u32,
    /// Interval of refreshing the connection details while connected; the
    /// link counts as stable after one such interval.
    pub link_refresh_ms: u32,
}

config_overrides! {
    /// Persisted overrides of the [`WifiConfig`] section (section `WifiConfig`).
    ///
    /// Persistent ABI: a CBOR map keyed by the field ids below, which are only
    /// ever added; a field that is not overridden is absent. The map is pinned
    /// by a golden test.
    pub struct WifiOverrides for WifiConfig, version 1 {
        1 => ssid: WifiSsid,
        2 => password: Option<WifiPassword>,
        3 => scan_timeout_ms: u32,
        4 => connect_timeout_ms: u32,
        5 => backoff_initial_ms: u32,
        6 => backoff_max_ms: u32,
        7 => link_refresh_ms: u32,
    }
}

impl Default for WifiConfig {
    fn default() -> Self {
        Self::new(WifiSsid::empty(), None)
    }
}

impl WifiConfig {
    /// A configuration for `ssid` and `password` with the default timing.
    pub const fn new(ssid: WifiSsid, password: Option<WifiPassword>) -> Self {
        Self {
            ssid,
            password,
            scan_timeout_ms: DEFAULT_SCAN_TIMEOUT_MS,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            backoff_initial_ms: DEFAULT_BACKOFF_INITIAL_MS,
            backoff_max_ms: DEFAULT_BACKOFF_MAX_MS,
            link_refresh_ms: DEFAULT_LINK_REFRESH_MS,
        }
    }

    /// Checks the configuration before it is persisted or applied.
    pub fn validate(&self) -> Result<(), WifiError> {
        let valid = !self.ssid.is_empty()
            && self
                .password
                .as_ref()
                .is_none_or(|password| password.len() >= MIN_WIFI_PASSWORD_LEN)
            && within(self.scan_timeout_ms, SCAN_TIMEOUT_RANGE_MS)
            && within(self.connect_timeout_ms, CONNECT_TIMEOUT_RANGE_MS)
            && within(self.backoff_initial_ms, BACKOFF_INITIAL_RANGE_MS)
            && within(self.backoff_max_ms, BACKOFF_MAX_RANGE_MS)
            && self.backoff_initial_ms <= self.backoff_max_ms
            && within(self.link_refresh_ms, LINK_REFRESH_RANGE_MS);
        if valid {
            Ok(())
        } else {
            Err(WifiError::InvalidConfig)
        }
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

impl fmt::Debug for WifiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WifiConfig")
            .field("ssid", &self.ssid)
            .field("password", &self.password)
            .field("scan_timeout_ms", &self.scan_timeout_ms)
            .field("connect_timeout_ms", &self.connect_timeout_ms)
            .field("backoff_initial_ms", &self.backoff_initial_ms)
            .field("backoff_max_ms", &self.backoff_max_ms)
            .field("link_refresh_ms", &self.link_refresh_ms)
            .finish()
    }
}

/// Everything the radio needs to join a network and get an address: the
/// [`WifiConfig`] and the IPv4 settings of the [`NetworkConfig`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkConfig {
    /// Network, credentials and timing.
    pub wifi: WifiConfig,
    /// How the address is obtained.
    pub network: NetworkConfig,
}

impl LinkConfig {
    /// Checks both parts.
    pub fn validate(&self) -> Result<(), WifiError> {
        self.wifi.validate()?;
        self.network.validate()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rohi_config::{
        ConfigOverrides, ConfigSection, ConfigStore, MemoryStorage, Override, SectionId,
    };

    use super::*;

    #[test]
    fn oversized_values_are_rejected() {
        assert!(WifiSsid::from_bytes(&[0; MAX_SSID_LEN + 1]).is_none());
        assert!(WifiPassword::from_bytes(&[0; MAX_WIFI_PASSWORD_LEN + 1]).is_none());
        // An oversized byte string in a stored payload must not panic.
        let mut payload = [0u8; 40];
        payload[0] = 0x58;
        payload[1] = 33;
        assert!(rohi_config::decode_exact::<WifiSsid>(&payload[..35]).is_err());
        assert!(rohi_config::decode_exact::<WifiSsid>(&payload[..34]).is_err());
    }

    fn ssid(value: &str) -> WifiSsid {
        WifiSsid::from_bytes(value.as_bytes()).unwrap()
    }

    fn password(value: &str) -> WifiPassword {
        WifiPassword::from_bytes(value.as_bytes()).unwrap()
    }

    fn encode(overrides: &WifiOverrides) -> std::vec::Vec<u8> {
        let mut buffer = [0u8; 256];
        let used = rohi_config::encode_to_slice(overrides, &mut buffer).unwrap();
        assert_eq!(used, minicbor::len(overrides));
        buffer[..used].to_vec()
    }

    fn roundtrip(overrides: &WifiOverrides) -> WifiOverrides {
        rohi_config::decode_exact(&encode(overrides)).unwrap()
    }

    #[test]
    fn the_section_id_is_derived_from_the_type_name() {
        assert_eq!(WifiOverrides::ID, SectionId::from_name("WifiConfig"));
        assert_eq!(WifiOverrides::VERSION, 1);
        assert_eq!(WifiOverrides::ID, SectionId(0x3352_46ad_2024_8b26));
    }

    #[test]
    fn nothing_is_stored_for_the_defaults() {
        let overrides = WifiOverrides::default();
        assert!(overrides.is_empty());
        assert_eq!(encode(&overrides), [0xa0]);
        assert_eq!(overrides.effective(), WifiConfig::default());
    }

    #[test]
    fn the_cbor_encoding_is_stable() {
        let overrides = WifiOverrides {
            ssid: Override::Value(ssid("ab")),
            scan_timeout_ms: Override::Value(5_000),
            ..WifiOverrides::default()
        };
        assert_eq!(
            encode(&overrides),
            [
                0xa2, // map(2)
                0x01, 0x42, 0x61, 0x62, // 1: ssid
                0x03, 0x19, 0x13, 0x88, // 3: scan_timeout_ms 5000
            ]
        );
    }

    #[test]
    fn the_password_has_three_states() {
        let mut overrides = WifiOverrides::default();
        // Not overridden: the default, no password.
        assert_eq!(overrides.effective().password, None);
        assert_eq!(encode(&overrides), [0xa0]);
        // An explicit `None` is stored as null.
        overrides.password = Override::Value(None);
        assert_eq!(encode(&overrides), [0xa1, 0x02, 0xf6]);
        assert_eq!(roundtrip(&overrides), overrides);
        // An explicit password.
        overrides.password = Override::Value(Some(password("hunter2hunter2")));
        assert_eq!(roundtrip(&overrides), overrides);
        assert_eq!(
            overrides.effective().password,
            Some(password("hunter2hunter2"))
        );
    }

    #[test]
    fn other_layouts_and_trailing_bytes_are_rejected() {
        assert!(rohi_config::decode_exact::<WifiOverrides>(&[0x81, 0x42, 0x61, 0x62]).is_err());
        let mut bytes = encode(&WifiOverrides::default());
        bytes.push(0x00);
        assert!(rohi_config::decode_exact::<WifiOverrides>(&bytes).is_err());
    }

    #[test]
    fn the_largest_value_fits_a_section() {
        let overrides = WifiOverrides {
            ssid: Override::Value(WifiSsid::from_bytes(&[b'a'; MAX_SSID_LEN]).unwrap()),
            password: Override::Value(Some(
                WifiPassword::from_bytes(&[b'p'; MAX_WIFI_PASSWORD_LEN]).unwrap(),
            )),
            scan_timeout_ms: Override::Value(u32::MAX),
            connect_timeout_ms: Override::Value(u32::MAX),
            backoff_initial_ms: Override::Value(u32::MAX),
            backoff_max_ms: Override::Value(u32::MAX),
            link_refresh_ms: Override::Value(u32::MAX),
        };
        assert!(minicbor::len(&overrides) <= rohi_config::MAX_SECTION_PAYLOAD);
        assert_eq!(roundtrip(&overrides), overrides);
    }

    #[test]
    fn debug_never_prints_the_password() {
        let config = WifiConfig::new(ssid("office"), Some(password("super-secret-pass")));
        let text = std::format!("{config:?}");
        assert!(!text.contains("super-secret-pass"));
        assert!(text.contains("<redacted>"));
        assert!(text.contains("office"));
    }

    #[test]
    fn the_defaults_validate_once_a_network_is_set() {
        assert_eq!(
            WifiConfig::new(WifiSsid::empty(), None).validate(),
            Err(WifiError::InvalidConfig)
        );
        assert_eq!(WifiConfig::new(ssid("a"), None).validate(), Ok(()));
        assert_eq!(
            WifiConfig::new(ssid("a"), Some(password("short"))).validate(),
            Err(WifiError::InvalidConfig)
        );
        assert_eq!(
            WifiConfig::new(ssid("a"), Some(password("longenough"))).validate(),
            Ok(())
        );
    }

    #[test]
    fn the_timing_is_bounded_and_ordered() {
        let base = WifiConfig::new(ssid("a"), None);
        let invalid = |config: WifiConfig| config.validate() == Err(WifiError::InvalidConfig);
        assert!(invalid(WifiConfig {
            backoff_initial_ms: 0,
            ..base.clone()
        }));
        assert!(invalid(WifiConfig {
            backoff_max_ms: 500,
            ..base.clone()
        }));
        assert!(invalid(WifiConfig {
            backoff_max_ms: u32::MAX,
            ..base.clone()
        }));
        assert!(invalid(WifiConfig {
            link_refresh_ms: 0,
            ..base.clone()
        }));
        assert!(invalid(WifiConfig {
            scan_timeout_ms: 0,
            ..base.clone()
        }));
        assert!(invalid(WifiConfig {
            connect_timeout_ms: 1_000,
            ..base.clone()
        }));
        let equal = WifiConfig {
            backoff_initial_ms: 5_000,
            backoff_max_ms: 5_000,
            ..base
        };
        assert_eq!(equal.validate(), Ok(()));
    }

    #[test]
    fn a_partial_update_preserves_the_rest() {
        let config = WifiConfig {
            scan_timeout_ms: 5_000,
            ..WifiConfig::new(ssid("old"), Some(password("hunter2hunter2")))
        };
        let updated = config.clone().with_ssid(ssid("new"));
        assert_eq!(updated.ssid, ssid("new"));
        assert_eq!(updated.password, config.password);
        assert_eq!(updated.scan_timeout_ms, 5_000);

        let cleared = updated.with_password(None);
        assert_eq!(cleared.password, None);
        assert_eq!(cleared.ssid, ssid("new"));
    }

    #[test]
    fn overrides_persist_in_the_config_store_and_can_be_removed() {
        let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
        let overrides = WifiOverrides {
            ssid: Override::Value(ssid("office")),
            password: Override::Value(Some(password("hunter2hunter2"))),
            ..WifiOverrides::default()
        };
        store.set(&overrides).unwrap();
        assert_eq!(store.get::<WifiOverrides>().unwrap(), Some(overrides));
        store.remove::<WifiOverrides>().unwrap();
        assert_eq!(store.get::<WifiOverrides>().unwrap(), None);
    }

    #[test]
    fn a_changed_default_reaches_only_the_fields_without_an_override() {
        let overrides = WifiOverrides {
            ssid: Override::Value(ssid("office")),
            scan_timeout_ms: Override::Value(5_000),
            ..WifiOverrides::default()
        };
        let effective = overrides.effective();
        assert_eq!(effective.ssid, ssid("office"));
        assert_eq!(effective.scan_timeout_ms, 5_000);
        // Not overridden: the field follows `WifiConfig::default()`.
        assert_eq!(effective.connect_timeout_ms, DEFAULT_CONNECT_TIMEOUT_MS);
        assert_eq!(effective.backoff_max_ms, DEFAULT_BACKOFF_MAX_MS);
        assert_eq!(effective.validate(), Ok(()));
    }
}
