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
use rohi_config::ConfigSection;

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

/// Persistent Wi-Fi station configuration (section `WifiConfig`).
///
/// An open network is represented by `password == None`, never by an empty
/// password.
///
/// The CBOR representation is persistent ABI and declared explicitly
/// (`#[cbor(array)]`); see `cbor_encoding_is_stable`.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct WifiConfig {
    #[n(0)]
    pub ssid: WifiSsid,
    #[n(1)]
    pub password: Option<WifiPassword>,
}

impl WifiConfig {
    pub const fn new(ssid: WifiSsid, password: Option<WifiPassword>) -> Self {
        Self { ssid, password }
    }

    /// Checks the configuration before it is persisted or applied.
    pub fn validate(&self) -> Result<(), WifiError> {
        if self.ssid.is_empty() {
            return Err(WifiError::InvalidConfig);
        }
        match &self.password {
            None => Ok(()),
            Some(password) if password.len() >= MIN_WIFI_PASSWORD_LEN => Ok(()),
            Some(_) => Err(WifiError::InvalidConfig),
        }
    }

    /// Replaces the SSID and keeps the stored password.
    pub fn with_ssid(mut self, ssid: WifiSsid) -> Self {
        self.ssid = ssid;
        self
    }

    /// Replaces (`Some`) or clears (`None`) the password and keeps the SSID.
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
            .finish()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::format;

    use rohi_config::{ConfigStore, MemoryStorage};

    use super::*;

    fn ssid(value: &str) -> WifiSsid {
        WifiSsid::from_bytes(value.as_bytes()).unwrap()
    }

    fn password(value: &str) -> WifiPassword {
        WifiPassword::from_bytes(value.as_bytes()).unwrap()
    }

    const BUFFER: usize = 128;

    fn roundtrip(config: WifiConfig) -> WifiConfig {
        let mut buffer = [0u8; BUFFER];
        let used = rohi_config::encode_to_slice(&config, &mut buffer).unwrap();
        assert_eq!(used, minicbor::len(config));
        rohi_config::decode_exact(&buffer[..used]).unwrap()
    }

    #[test]
    fn section_id_and_version() {
        use rohi_config::{ConfigSection, SectionId};
        // Struct name, no module path: renaming the type changes persisted data.
        assert_eq!(WifiConfig::ID, SectionId::from_name("WifiConfig"));
        assert_eq!(WifiConfig::ID, SectionId(0x3352_46ad_2024_8b26));
        assert_eq!(WifiConfig::VERSION, 1);
    }

    #[test]
    fn cbor_encoding_is_stable() {
        let mut buffer = [0u8; BUFFER];
        let config = WifiConfig::new(ssid("ab"), None);
        let used = rohi_config::encode_to_slice(&config, &mut buffer).unwrap();
        // A trailing `None` is omitted from the array.
        assert_eq!(&buffer[..used], [0x81, 0x42, 0x61, 0x62]);
        let config = WifiConfig::new(ssid("ab"), Some(password("pw")));
        let used = rohi_config::encode_to_slice(&config, &mut buffer).unwrap();
        assert_eq!(&buffer[..used], [0x82, 0x42, 0x61, 0x62, 0x42, 0x70, 0x77]);
    }

    #[test]
    fn roundtrip_with_password() {
        let config = WifiConfig::new(ssid("office"), Some(password("hunter2hunter2")));
        assert_eq!(roundtrip(config.clone()), config);
    }

    #[test]
    fn roundtrip_open_network() {
        let config = WifiConfig::new(ssid("guest"), None);
        assert_eq!(roundtrip(config).password, None);
    }

    #[test]
    fn boundary_lengths_roundtrip() {
        let long_ssid = WifiSsid::from_bytes(&[b'a'; MAX_SSID_LEN]).unwrap();
        let long_password = WifiPassword::from_bytes(&[b'p'; MAX_WIFI_PASSWORD_LEN]).unwrap();
        let config = WifiConfig::new(long_ssid, Some(long_password));
        let mut buffer = [0u8; BUFFER];
        let used = rohi_config::encode_to_slice(&config, &mut buffer).unwrap();
        // array(2), ssid (2 byte head + 32), password (2 byte head + 64).
        assert_eq!(used, 1 + 2 + MAX_SSID_LEN + 2 + MAX_WIFI_PASSWORD_LEN);
        assert_eq!(roundtrip(config.clone()), config);
    }

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

    #[test]
    fn debug_never_prints_password() {
        let config = WifiConfig::new(ssid("office"), Some(password("super-secret-pass")));
        let text = format!("{config:?} {:?}", config.password);
        assert!(!text.contains("super-secret-pass"));
        assert!(text.contains("<redacted>"));
        assert!(text.contains("office"));
    }

    #[test]
    fn validate_rules() {
        assert_eq!(
            WifiConfig::new(WifiSsid::empty(), None).validate(),
            Err(WifiError::InvalidConfig)
        );
        assert_eq!(
            WifiConfig::new(ssid("a"), Some(password("short"))).validate(),
            Err(WifiError::InvalidConfig)
        );
        assert!(WifiConfig::new(ssid("a"), None).validate().is_ok());
        assert!(
            WifiConfig::new(ssid("a"), Some(password("longenough")))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn partial_update_preserves_password() {
        let config = WifiConfig::new(ssid("old"), Some(password("hunter2hunter2")));
        let updated = config.clone().with_ssid(ssid("new"));
        assert_eq!(updated.ssid, ssid("new"));
        assert_eq!(updated.password, config.password);

        let replaced = updated.with_password(Some(password("another-password")));
        assert_eq!(replaced.password, Some(password("another-password")));

        let cleared = replaced.with_password(None);
        assert_eq!(cleared.password, None);
        assert_eq!(cleared.ssid, ssid("new"));
    }

    #[test]
    fn persists_in_config_store_and_can_be_removed() {
        let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
        let config = WifiConfig::new(ssid("office"), Some(password("hunter2hunter2")));
        store.set(&config).unwrap();
        assert_eq!(store.get::<WifiConfig>().unwrap(), Some(config));
        store.remove::<WifiConfig>().unwrap();
        assert_eq!(store.get::<WifiConfig>().unwrap(), None);
    }
}
