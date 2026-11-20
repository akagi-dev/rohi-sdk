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
//! The BLE settings: how the peripheral presents itself.

use core::fmt;

use heapless::String;
use minicbor::{CborLen, Decode, Encode};

/// Longest advertised name, in bytes: the whole scan response minus its
/// 2-byte header.
pub const MAX_BLE_NAME_LEN: usize = 29;

/// The name a BLE central sees: 1 to [`MAX_BLE_NAME_LEN`] bytes of UTF-8
/// without control characters.
///
/// ```
/// use rohi_net::{BleName, MAX_BLE_NAME_LEN};
///
/// assert_eq!(BleName::new("ALTRUIST-URBAN-A1B2").unwrap().as_str(), "ALTRUIST-URBAN-A1B2");
/// assert!(BleName::new("").is_none());
/// assert!(BleName::new("two\nlines").is_none());
/// assert!(BleName::new(&"a".repeat(MAX_BLE_NAME_LEN + 1)).is_none());
/// ```
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct BleName(
    #[n(0)]
    #[cbor(
        encode_with = "rohi_config::cbor::text::encode",
        decode_with = "rohi_config::cbor::text::decode",
        cbor_len = "rohi_config::cbor::text::cbor_len"
    )]
    String<MAX_BLE_NAME_LEN>,
);

impl BleName {
    /// `None` if `name` is empty, too long or contains control characters.
    pub fn new(name: &str) -> Option<Self> {
        if name.is_empty() || name.chars().any(char::is_control) {
            return None;
        }
        String::try_from(name).ok().map(Self)
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Effective BLE configuration, the `ble_name` field of the
/// [`WirelessConfig`](crate::WirelessConfig) section.
///
/// The name is read when the peripheral starts: a change takes effect after
/// a restart.
///
/// ```
/// use rohi_net::{BleName, BleSettings};
///
/// // The default keeps the firmware name.
/// assert_eq!(BleSettings::default().name, None);
/// let settings = BleSettings { name: BleName::new("kitchen") };
/// assert_eq!(settings.name.unwrap().as_str(), "kitchen");
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BleSettings {
    /// Advertised and GAP device name. `None` keeps the firmware default,
    /// see [`BleConfig::name_prefix`](crate::BleConfig::name_prefix).
    pub name: Option<BleName>,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn names_are_bounded_printable_text() {
        assert!(BleName::new("ALTRUIST-URBAN-A1B2").is_some());
        assert!(BleName::new("Küche").is_some());
        assert!(BleName::new(&"a".repeat(MAX_BLE_NAME_LEN)).is_some());
        assert!(BleName::new("").is_none());
        assert!(BleName::new(&"a".repeat(MAX_BLE_NAME_LEN + 1)).is_none());
        assert!(BleName::new("a\nb").is_none());
    }
}
