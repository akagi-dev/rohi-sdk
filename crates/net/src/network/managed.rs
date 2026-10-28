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
//! The `network` config section in the generic management plane: the field
//! codecs of the domain types, the enum of the address mode and the validation
//! of the section. The fields, their ids and the persisted overrides are
//! declared once, on [`NetworkConfig`](super::NetworkConfig).

use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::str::FromStr;

use heapless::String;
use rohi_config::{Codec, TextValue, VariantValue};
use rohi_service::{EnumDescriptor, EnumVariant, FieldError, Value, ValueKind};

use super::config::{AddressMode, Hostname, Ipv4, NetworkConfig};

/// Values of the `address_mode` field.
pub mod address_mode {
    /// The address comes from a DHCP server.
    pub const DHCP: u32 = 0;
    /// The address is configured.
    pub const STATIC: u32 = 1;
}

/// Id of the enum of the `address_mode` field.
pub const ADDRESS_MODE_ENUM: u32 = 1;

/// The enum of the `address_mode` config field.
pub const ADDRESS_MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
    ADDRESS_MODE_ENUM,
    &[
        EnumVariant::new(address_mode::DHCP, "dhcp"),
        EnumVariant::new(address_mode::STATIC, "static"),
    ],
)];

/// The address mode as the enum [`ADDRESS_MODES`].
impl VariantValue for AddressMode {
    fn variant(&self) -> u32 {
        match self {
            Self::Dhcp => address_mode::DHCP,
            Self::Static => address_mode::STATIC,
        }
    }

    fn from_variant(variant: u32) -> Option<Self> {
        match variant {
            address_mode::DHCP => Some(Self::Dhcp),
            address_mode::STATIC => Some(Self::Static),
            _ => None,
        }
    }
}

/// An IPv4 address as dotted text.
pub struct Ipv4Codec;

impl Codec<Ipv4> for Ipv4Codec {
    const KIND: ValueKind = ValueKind::String;

    fn encode(address: &Ipv4) -> Result<Option<Value>, FieldError> {
        let mut text = String::<15>::new();
        write!(text, "{}", Ipv4Addr::from(*address)).map_err(|_| FieldError::Invalid)?;
        Value::text(&text).map(Some).ok_or(FieldError::Invalid)
    }

    fn decode(value: &Value) -> Result<Ipv4, FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        Ipv4Addr::from_str(text)
            .map(Ipv4::from)
            .map_err(|_| FieldError::Invalid)
    }
}

/// A DHCP host name as text.
impl TextValue for Hostname {
    fn from_text(text: &str) -> Option<Self> {
        Self::new(text)
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.as_str())
    }
}

/// The static address needs all its parts, and the section is valid as a
/// whole.
pub(super) fn validate_section(config: &NetworkConfig) -> Result<(), FieldError> {
    NetworkConfig::validate(config).map_err(|_| FieldError::Invalid)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rohi_config::ConfigOverrides;
    use rohi_service::{Config, MAX_CONFIG_FIELDS};

    use super::*;
    use crate::NetworkOverrides;
    use rohi_config::ConfigSection;

    const ADDRESS_MODE: u32 = 1;
    const IP: u32 = 2;
    const PREFIX_LEN: u32 = 3;
    const GATEWAY: u32 = 4;
    const DNS: u32 = 5;
    const HOSTNAME: u32 = 6;
    const DHCP_TIMEOUT: u32 = 7;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn the_reported_version_is_the_stored_one() {
        assert_eq!(<NetworkOverrides as ConfigSection>::VERSION, 1);
    }

    #[test]
    fn every_field_can_be_overridden() {
        let mut ids: std::vec::Vec<_> = NetworkConfig::FIELDS.iter().map(|f| f.field.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, NetworkOverrides::FIELD_IDS);
    }

    #[test]
    fn the_schema_is_well_formed() {
        assert!(NetworkConfig::FIELDS.len() <= MAX_CONFIG_FIELDS);
        let mut ids: std::vec::Vec<_> = NetworkConfig::FIELDS.iter().map(|f| f.field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), NetworkConfig::FIELDS.len());
        for field in NetworkConfig::FIELDS {
            assert!(field.field.name.len() <= 24, "{}", field.field.name);
        }
    }

    #[test]
    fn the_defaults_are_dhcp_without_a_static_address() {
        let config = NetworkConfig::default();
        assert_eq!(Config::validate(&config), Ok(()));
        let read = |id| config.read_field(id).unwrap();
        assert_eq!(read(ADDRESS_MODE), Some(Value::Enum(address_mode::DHCP)));
        assert_eq!(read(DHCP_TIMEOUT), Some(Value::U64(20_000)));
        for id in [IP, PREFIX_LEN, GATEWAY, DNS, HOSTNAME] {
            assert_eq!(read(id), None);
        }
    }

    #[test]
    fn a_static_address_is_set_field_by_field_and_validated_as_a_whole() {
        let mut config = NetworkConfig::default();
        config
            .set_field(ADDRESS_MODE, Value::Enum(address_mode::STATIC))
            .unwrap();
        // Static mode without an address is incomplete.
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));

        config.set_field(IP, text("192.168.1.50")).unwrap();
        config.set_field(PREFIX_LEN, Value::U64(24)).unwrap();
        config.set_field(GATEWAY, text("192.168.1.1")).unwrap();
        config.set_field(DNS, text("1.1.1.1")).unwrap();
        assert_eq!(Config::validate(&config), Ok(()));
        assert_eq!(config.read_field(IP), Ok(Some(text("192.168.1.50"))));
        assert_eq!(config.read_field(PREFIX_LEN), Ok(Some(Value::U64(24))));
        assert_eq!(config.read_field(DNS), Ok(Some(text("1.1.1.1"))));

        // A gateway outside the network is rejected by the whole section.
        config.set_field(GATEWAY, text("10.0.0.1")).unwrap();
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));

        // Clearing the override of the DNS server restores the default: none.
        let mut overrides = NetworkOverrides::default();
        assert!(overrides.capture(&config, DNS));
        assert_eq!(
            rohi_config::materialize::<NetworkConfig>(&Default::default(), &overrides).dns,
            config.dns
        );
        assert!(overrides.clear(DNS));
        assert_eq!(
            rohi_config::materialize::<NetworkConfig>(&Default::default(), &overrides)
                .read_field(DNS),
            Ok(None)
        );
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = NetworkConfig::default();
        assert_eq!(
            config.set_field(IP, text("not an address")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(IP, text("192.168.1.256")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(IP, Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            config.set_field(PREFIX_LEN, Value::U64(300)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(ADDRESS_MODE, Value::Enum(7)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(HOSTNAME, text("bad name")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(DHCP_TIMEOUT, Value::U64(u64::MAX)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(99, Value::U64(1)),
            Err(FieldError::UnknownField)
        );
    }

    #[test]
    fn clearing_an_override_restores_the_default_of_a_required_field() {
        let mut overrides = NetworkOverrides::default();
        let mut config = rohi_config::materialize::<NetworkConfig>(&Default::default(), &overrides);
        config.set_field(DHCP_TIMEOUT, Value::U64(5_000)).unwrap();
        overrides.capture(&config, DHCP_TIMEOUT);
        assert_eq!(
            rohi_config::materialize::<NetworkConfig>(&Default::default(), &overrides)
                .dhcp_timeout_ms,
            5_000
        );
        overrides.clear(DHCP_TIMEOUT);
        assert_eq!(
            rohi_config::materialize::<NetworkConfig>(&Default::default(), &overrides)
                .dhcp_timeout_ms,
            20_000
        );
    }

    #[test]
    fn the_host_name_is_optional() {
        let mut config = NetworkConfig::default();
        config.set_field(HOSTNAME, text("altruist-1")).unwrap();
        assert_eq!(config.read_field(HOSTNAME), Ok(Some(text("altruist-1"))));
        let mut overrides = NetworkOverrides::default();
        overrides.capture(&config, HOSTNAME);
        overrides.clear(HOSTNAME);
        assert_eq!(
            rohi_config::materialize::<NetworkConfig>(&Default::default(), &overrides).hostname,
            None
        );
    }

    #[test]
    fn there_is_no_secret_field() {
        assert!(NetworkConfig::FIELDS.iter().all(|field| !field.secret));
    }
}
