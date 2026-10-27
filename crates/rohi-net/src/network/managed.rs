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
//! The `network` config section in the generic management plane.

use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::str::FromStr;

use heapless::String;
use rohi_config::ConfigSection;
use rohi_service::{
    Bounds, ConfigField, EnumDescriptor, EnumVariant, FieldError, ManagedConfig, Unit, Value,
    ValueKind,
};

use super::config::{AddressMode, DHCP_TIMEOUT_RANGE_MS, Hostname, Ipv4, NetworkConfig};

/// Field ids of the `network` config section. Only ever added.
pub mod field {
    /// `dhcp` or `static`.
    pub const ADDRESS_MODE: u32 = 1;
    /// Static IPv4 address.
    pub const IP: u32 = 2;
    /// Static network prefix length.
    pub const PREFIX_LEN: u32 = 3;
    /// Static default gateway.
    pub const GATEWAY: u32 = 4;
    /// Static DNS server.
    pub const DNS: u32 = 5;
    /// Host name announced to the DHCP server.
    pub const HOSTNAME: u32 = 6;
    /// Bound of obtaining an address from DHCP.
    pub const DHCP_TIMEOUT: u32 = 7;
}

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

fn address_text(address: Ipv4) -> Option<Value> {
    let mut text = String::<15>::new();
    write!(text, "{}", Ipv4Addr::from(address)).ok()?;
    Value::text(&text)
}

fn parse_address(value: &Value) -> Result<Ipv4, FieldError> {
    let text = value.as_str().ok_or(FieldError::WrongKind)?;
    Ipv4Addr::from_str(text)
        .map(Ipv4::from)
        .map_err(|_| FieldError::Invalid)
}

fn number(value: &Value) -> Result<u32, FieldError> {
    match value {
        Value::U64(number) => u32::try_from(*number).map_err(|_| FieldError::OutOfRange),
        _ => Err(FieldError::WrongKind),
    }
}

impl ManagedConfig for NetworkConfig {
    const SECTION_ID: u64 = <Self as ConfigSection>::ID.0;
    const NAME: &'static str = "network";
    const VERSION: u16 = <Self as ConfigSection>::VERSION;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::ADDRESS_MODE, "address_mode", ValueKind::Enum)
            .with_enum(ADDRESS_MODE_ENUM),
        ConfigField::new(field::IP, "ip", ValueKind::String).optional(),
        ConfigField::new(field::PREFIX_LEN, "prefix_len", ValueKind::U64)
            .bounds(Bounds::U64(1, 30))
            .optional(),
        ConfigField::new(field::GATEWAY, "gateway", ValueKind::String).optional(),
        ConfigField::new(field::DNS, "dns", ValueKind::String).optional(),
        ConfigField::new(field::HOSTNAME, "hostname", ValueKind::String).optional(),
        ConfigField::new(field::DHCP_TIMEOUT, "dhcp_timeout", ValueKind::U64)
            .unit(Unit::Ms)
            .bounds(Bounds::U64(
                DHCP_TIMEOUT_RANGE_MS.0 as u64,
                DHCP_TIMEOUT_RANGE_MS.1 as u64,
            )),
    ];
    const ENUMS: &'static [EnumDescriptor] = ADDRESS_MODES;

    fn defaults() -> Self {
        Self::new()
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        let optional_address = |address: Option<Ipv4>| match address {
            None => Ok(None),
            Some(address) => address_text(address).map(Some).ok_or(FieldError::Invalid),
        };
        match id {
            field::ADDRESS_MODE => Ok(Some(Value::Enum(match self.address_mode {
                AddressMode::Dhcp => address_mode::DHCP,
                AddressMode::Static => address_mode::STATIC,
            }))),
            field::IP => optional_address(self.static_ip),
            field::PREFIX_LEN => Ok(self.prefix_len.map(|len| Value::U64(len.into()))),
            field::GATEWAY => optional_address(self.gateway),
            field::DNS => optional_address(self.dns),
            field::HOSTNAME => match &self.hostname {
                None => Ok(None),
                Some(name) => Value::text(name.as_str())
                    .map(Some)
                    .ok_or(FieldError::Invalid),
            },
            field::DHCP_TIMEOUT => Ok(Some(Value::U64(self.dhcp_timeout_ms.into()))),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match id {
            field::ADDRESS_MODE => {
                self.address_mode = match &value {
                    Value::Enum(address_mode::DHCP) => AddressMode::Dhcp,
                    Value::Enum(address_mode::STATIC) => AddressMode::Static,
                    Value::Enum(_) => return Err(FieldError::OutOfRange),
                    _ => return Err(FieldError::WrongKind),
                };
            }
            field::IP => self.static_ip = Some(parse_address(&value)?),
            field::PREFIX_LEN => {
                self.prefix_len =
                    Some(u8::try_from(number(&value)?).map_err(|_| FieldError::OutOfRange)?);
            }
            field::GATEWAY => self.gateway = Some(parse_address(&value)?),
            field::DNS => self.dns = Some(parse_address(&value)?),
            field::HOSTNAME => {
                let text = value.as_str().ok_or(FieldError::WrongKind)?;
                self.hostname = Some(Hostname::new(text).ok_or(FieldError::Invalid)?);
            }
            field::DHCP_TIMEOUT => self.dhcp_timeout_ms = number(&value)?,
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::IP => self.static_ip = None,
            field::PREFIX_LEN => self.prefix_len = None,
            field::GATEWAY => self.gateway = None,
            field::DNS => self.dns = None,
            field::HOSTNAME => self.hostname = None,
            field::ADDRESS_MODE | field::DHCP_TIMEOUT => return Err(FieldError::NotOptional),
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), FieldError> {
        NetworkConfig::validate(self).map_err(|_| FieldError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rohi_service::MAX_CONFIG_FIELDS;

    use super::*;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn the_reported_version_is_the_stored_one() {
        assert_eq!(<NetworkConfig as ManagedConfig>::VERSION, 1);
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
        let config = NetworkConfig::defaults();
        assert_eq!(ManagedConfig::validate(&config), Ok(()));
        let read = |id| config.read_field(id).unwrap();
        assert_eq!(
            read(field::ADDRESS_MODE),
            Some(Value::Enum(address_mode::DHCP))
        );
        assert_eq!(read(field::DHCP_TIMEOUT), Some(Value::U64(20_000)));
        for id in [
            field::IP,
            field::PREFIX_LEN,
            field::GATEWAY,
            field::DNS,
            field::HOSTNAME,
        ] {
            assert_eq!(read(id), None);
        }
    }

    #[test]
    fn a_static_address_is_set_field_by_field_and_validated_as_a_whole() {
        let mut config = NetworkConfig::defaults();
        config
            .set_field(field::ADDRESS_MODE, Value::Enum(address_mode::STATIC))
            .unwrap();
        // Static mode without an address is incomplete.
        assert_eq!(ManagedConfig::validate(&config), Err(FieldError::Invalid));

        config.set_field(field::IP, text("192.168.1.50")).unwrap();
        config.set_field(field::PREFIX_LEN, Value::U64(24)).unwrap();
        config
            .set_field(field::GATEWAY, text("192.168.1.1"))
            .unwrap();
        config.set_field(field::DNS, text("1.1.1.1")).unwrap();
        assert_eq!(ManagedConfig::validate(&config), Ok(()));
        assert_eq!(config.read_field(field::IP), Ok(Some(text("192.168.1.50"))));
        assert_eq!(
            config.read_field(field::PREFIX_LEN),
            Ok(Some(Value::U64(24)))
        );
        assert_eq!(config.read_field(field::DNS), Ok(Some(text("1.1.1.1"))));

        // A gateway outside the network is rejected by the whole section.
        config.set_field(field::GATEWAY, text("10.0.0.1")).unwrap();
        assert_eq!(ManagedConfig::validate(&config), Err(FieldError::Invalid));

        config.unset_field(field::DNS).unwrap();
        assert_eq!(config.read_field(field::DNS), Ok(None));
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = NetworkConfig::defaults();
        assert_eq!(
            config.set_field(field::IP, text("not an address")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(field::IP, text("192.168.1.256")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(field::IP, Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            config.set_field(field::PREFIX_LEN, Value::U64(300)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(field::ADDRESS_MODE, Value::Enum(7)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(field::HOSTNAME, text("bad name")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(field::DHCP_TIMEOUT, Value::U64(u64::MAX)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(99, Value::U64(1)),
            Err(FieldError::UnknownField)
        );
    }

    #[test]
    fn the_required_fields_cannot_be_unset() {
        let mut config = NetworkConfig::defaults();
        assert_eq!(
            config.unset_field(field::ADDRESS_MODE),
            Err(FieldError::NotOptional)
        );
        assert_eq!(
            config.unset_field(field::DHCP_TIMEOUT),
            Err(FieldError::NotOptional)
        );
    }

    #[test]
    fn the_host_name_is_optional() {
        let mut config = NetworkConfig::defaults();
        config
            .set_field(field::HOSTNAME, text("altruist-1"))
            .unwrap();
        assert_eq!(
            config.read_field(field::HOSTNAME),
            Ok(Some(text("altruist-1")))
        );
        config.unset_field(field::HOSTNAME).unwrap();
        assert_eq!(config.hostname, None);
    }

    #[test]
    fn there_is_no_secret_field() {
        assert!(NetworkConfig::FIELDS.iter().all(|field| !field.secret));
    }
}
