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
//! The persistent `ble` section: how the peripheral presents itself.

use core::fmt;

use heapless::String;
use minicbor::{CborLen, Decode, Encode};
use rohi_config::Text;
use rohi_service::Config;

/// Longest advertised name, in bytes: the whole scan response minus its
/// 2-byte header.
pub const MAX_BLE_NAME_LEN: usize = 29;

/// The name a BLE central sees: 1 to [`MAX_BLE_NAME_LEN`] bytes of UTF-8
/// without control characters.
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

/// Effective BLE configuration (shown to clients as the `ble` section).
///
/// The name is read when the peripheral starts: a change takes effect after
/// a restart. What is stored are the explicit user overrides of it,
/// [`BleOverrides`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Config)]
#[config(name = "ble", overrides = BleOverrides, version = 1)]
pub struct BleSettings {
    /// Advertised and GAP device name. `None` keeps the firmware default,
    /// see [`BleConfig::name_prefix`](crate::BleConfig::name_prefix).
    #[config(id = 1, with = Text)]
    pub name: Option<BleName>,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rohi_config::{
        ConfigOverrides, ConfigSection, ConfigStore, MemoryStorage, Override, SectionId,
    };

    use super::*;

    fn encode(overrides: &BleOverrides) -> std::vec::Vec<u8> {
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(overrides, &mut buffer).unwrap();
        assert_eq!(used, minicbor::len(overrides));
        buffer[..used].to_vec()
    }

    #[test]
    fn the_section_id_is_derived_from_the_type_name() {
        assert_eq!(BleOverrides::ID, SectionId::from_name("BleSettings"));
        assert_eq!(BleOverrides::VERSION, 1);
        assert_eq!(BleOverrides::ID, SectionId(0xcba3_fd7b_3d5a_d5ef));
    }

    #[test]
    fn the_cbor_encoding_is_stable() {
        // Nothing overridden is an empty map.
        assert_eq!(encode(&BleOverrides::default()), [0xa0]);
        assert_eq!(
            rohi_config::decode_exact::<BleOverrides>(&[0xa0]).unwrap(),
            BleOverrides::default()
        );
        let overrides = BleOverrides {
            name: Override::Value(BleName::new("urban")),
        };
        assert_eq!(
            encode(&overrides),
            [0xa1, 0x01, 0x65, b'u', b'r', b'b', b'a', b'n']
        );
        assert_eq!(
            rohi_config::decode_exact::<BleOverrides>(&encode(&overrides)).unwrap(),
            overrides
        );
        assert_eq!(
            rohi_config::materialize::<BleSettings>(&Default::default(), &overrides),
            BleSettings {
                name: BleName::new("urban")
            }
        );
    }

    #[test]
    fn the_name_has_three_states() {
        let mut overrides = BleOverrides::default();
        assert_eq!(
            rohi_config::materialize::<BleSettings>(&Default::default(), &overrides).name,
            None
        );
        // An explicit `None` is stored as null, distinct from not overridden.
        overrides.name = Override::Value(None);
        assert_eq!(encode(&overrides), [0xa1, 0x01, 0xf6]);
        assert_eq!(
            rohi_config::decode_exact::<BleOverrides>(&[0xa1, 0x01, 0xf6]).unwrap(),
            overrides
        );
        assert!(!overrides.is_empty());
    }

    #[test]
    fn other_layouts_and_trailing_bytes_are_rejected() {
        assert!(rohi_config::decode_exact::<BleOverrides>(&[0x80]).is_err());
        let mut bytes = encode(&BleOverrides::default());
        bytes.push(0x00);
        assert!(rohi_config::decode_exact::<BleOverrides>(&bytes).is_err());
        // A stored name longer than the limit must not panic.
        let mut long = std::vec![0xa1, 0x01, 0x78, 30];
        long.extend([b'a'; 30]);
        assert!(rohi_config::decode_exact::<BleOverrides>(&long).is_err());
    }

    #[test]
    fn names_are_bounded_printable_text() {
        assert!(BleName::new("ALTRUIST-URBAN-A1B2").is_some());
        assert!(BleName::new("Küche").is_some());
        assert!(BleName::new(&"a".repeat(MAX_BLE_NAME_LEN)).is_some());
        assert!(BleName::new("").is_none());
        assert!(BleName::new(&"a".repeat(MAX_BLE_NAME_LEN + 1)).is_none());
        assert!(BleName::new("a\nb").is_none());
    }

    #[test]
    fn it_persists_in_the_config_store_and_can_be_removed() {
        let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
        let overrides = BleOverrides {
            name: Override::Value(BleName::new("urban")),
        };
        store.set(&overrides).unwrap();
        assert_eq!(store.get::<BleOverrides>().unwrap(), Some(overrides));
        store.remove::<BleOverrides>().unwrap();
        assert_eq!(store.get::<BleOverrides>().unwrap(), None);
    }
}
