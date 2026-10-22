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

use postcard::experimental::max_size::MaxSize;
use rohi_config::ConfigSection;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, MIN_WIFI_PASSWORD_LEN, WifiError};

/// Fixed-capacity byte string, serialized as a postcard byte sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Bounded<const N: usize> {
    len: u8,
    bytes: [u8; N],
}

impl<const N: usize> Bounded<N> {
    const fn empty() -> Self {
        Self {
            len: 0,
            bytes: [0; N],
        }
    }

    fn from_bytes(value: &[u8]) -> Option<Self> {
        if value.len() > N {
            return None;
        }
        let mut out = Self::empty();
        out.bytes[..value.len()].copy_from_slice(value);
        out.len = value.len() as u8;
        Some(out)
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl<const N: usize> Serialize for Bounded<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.as_bytes())
    }
}

impl<'de, const N: usize> Deserialize<'de> for Bounded<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BytesVisitor<const N: usize>;

        impl<const N: usize> Visitor<'_> for BytesVisitor<N> {
            type Value = Bounded<N>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "at most {N} bytes")
            }

            fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                Bounded::from_bytes(value).ok_or_else(|| E::invalid_length(value.len(), &self))
            }
        }

        deserializer.deserialize_bytes(BytesVisitor::<N>)
    }
}

impl<const N: usize> MaxSize for Bounded<N> {
    // One length byte, valid for N <= 127.
    const POSTCARD_MAX_SIZE: usize = 1 + N;
}

/// Wi-Fi network name: 0..=[`MAX_SSID_LEN`] arbitrary bytes.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
pub struct WifiSsid(Bounded<MAX_SSID_LEN>);

impl WifiSsid {
    pub const fn empty() -> Self {
        Self(Bounded::empty())
    }

    /// Returns `None` when `value` is longer than [`MAX_SSID_LEN`].
    pub fn from_bytes(value: &[u8]) -> Option<Self> {
        Bounded::from_bytes(value).map(Self)
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// The SSID as text, if it is valid UTF-8.
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(self.as_bytes()).ok()
    }

    pub fn is_empty(&self) -> bool {
        self.0.len == 0
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
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
pub struct WifiPassword(Bounded<MAX_WIFI_PASSWORD_LEN>);

impl WifiPassword {
    /// Returns `None` when `value` is longer than [`MAX_WIFI_PASSWORD_LEN`].
    pub fn from_bytes(value: &[u8]) -> Option<Self> {
        Bounded::from_bytes(value).map(Self)
    }

    /// The raw secret. Only the platform backend should call this.
    pub fn expose_secret(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn len(&self) -> usize {
        usize::from(self.0.len)
    }

    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }
}

impl fmt::Debug for WifiPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WifiPassword(<redacted>)")
    }
}

/// Persistent Wi-Fi station configuration (section `wifi`).
///
/// An open network is represented by `password == None`, never by an empty
/// password.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize, ConfigSection)]
#[config(name = "wifi", version = 1)]
pub struct WifiConfig {
    pub ssid: WifiSsid,
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

    fn roundtrip(config: WifiConfig) -> WifiConfig {
        let mut buffer = [0u8; WifiConfig::POSTCARD_MAX_SIZE];
        let used = postcard::to_slice(&config, &mut buffer).unwrap().len();
        assert!(used <= WifiConfig::POSTCARD_MAX_SIZE);
        postcard::from_bytes(&buffer[..used]).unwrap()
    }

    #[test]
    fn section_name_and_version() {
        assert_eq!(<WifiConfig as rohi_config::ConfigSection>::NAME, "wifi");
        assert_eq!(<WifiConfig as rohi_config::ConfigSection>::VERSION, 1);
    }

    #[test]
    fn roundtrip_with_password() {
        let config = WifiConfig::new(ssid("office"), Some(password("hunter2hunter2")));
        assert_eq!(roundtrip(config), config);
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
        let mut buffer = [0u8; WifiConfig::POSTCARD_MAX_SIZE];
        let used = postcard::to_slice(&config, &mut buffer).unwrap().len();
        assert_eq!(used, WifiConfig::POSTCARD_MAX_SIZE);
        assert_eq!(roundtrip(config), config);
    }

    #[test]
    fn oversized_values_are_rejected() {
        assert!(WifiSsid::from_bytes(&[0; MAX_SSID_LEN + 1]).is_none());
        assert!(WifiPassword::from_bytes(&[0; MAX_WIFI_PASSWORD_LEN + 1]).is_none());
        // Oversized length prefix in a stored payload must not panic.
        let mut payload = [0u8; 40];
        payload[0] = 33;
        assert!(postcard::from_bytes::<WifiSsid>(&payload).is_err());
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
        let updated = config.with_ssid(ssid("new"));
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
