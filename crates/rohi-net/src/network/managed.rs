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
use rohi_service::{ConfigField, FieldError, ManagedConfig, Value, ValueKind};

use super::config::{
    AddressMode, BACKOFF_INITIAL_RANGE_MS, BACKOFF_MAX_RANGE_MS, CONNECT_TIMEOUT_RANGE_MS,
    DHCP_TIMEOUT_RANGE_MS, Hostname, Ipv4, LINK_REFRESH_RANGE_MS, NetworkConfig,
    SCAN_TIMEOUT_RANGE_MS,
};
use crate::{WifiPassword, WifiSsid};

/// Field ids of the `network` config section. Only ever added.
pub mod field {
    /// Network name; text only.
    pub const SSID: u32 = 1;
    /// Write-only passphrase; unset for an open network.
    pub const PASSWORD: u32 = 2;
    /// `dhcp` or `static`.
    pub const ADDRESS_MODE: u32 = 3;
    /// Static IPv4 address.
    pub const IP: u32 = 4;
    /// Static network prefix length.
    pub const PREFIX_LEN: u32 = 5;
    /// Static default gateway.
    pub const GATEWAY: u32 = 6;
    /// Static DNS server.
    pub const DNS: u32 = 7;
    /// Host name announced to the DHCP server.
    pub const HOSTNAME: u32 = 8;
    /// First reconnect delay.
    pub const BACKOFF_INITIAL: u32 = 9;
    /// Longest reconnect delay.
    pub const BACKOFF_MAX: u32 = 10;
    /// Refresh interval of the connection details.
    pub const LINK_REFRESH: u32 = 11;
    /// Bound of a scan.
    pub const SCAN_TIMEOUT: u32 = 12;
    /// Bound of associating with the access point.
    pub const CONNECT_TIMEOUT: u32 = 13;
    /// Bound of obtaining an address from DHCP.
    pub const DHCP_TIMEOUT: u32 = 14;
}

/// Variants of the `address_mode` field.
pub const ADDRESS_MODES: &[&str] = &["dhcp", "static"];

const fn timing(id: u32, name: &'static str, (min, max): (u32, u32)) -> ConfigField {
    ConfigField::new(id, name, ValueKind::U64)
        .unit("ms")
        .range(min as f64, max as f64)
}

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
    const NAME: &'static str = "network";
    const VERSION: u16 = 2;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::SSID, "ssid", ValueKind::String)
            .describe("Network name, 1 to 32 bytes"),
        ConfigField::new(field::PASSWORD, "password", ValueKind::String)
            .describe("Passphrase, 8 to 64 bytes; unset if open")
            .secret()
            .optional(),
        ConfigField::new(field::ADDRESS_MODE, "address_mode", ValueKind::Enum)
            .describe("How the IPv4 address is obtained")
            .variants(ADDRESS_MODES),
        ConfigField::new(field::IP, "ip", ValueKind::String)
            .describe("Static IPv4 address")
            .optional(),
        ConfigField::new(field::PREFIX_LEN, "prefix_len", ValueKind::U64)
            .describe("Static network prefix length")
            .range(1.0, 30.0)
            .optional(),
        ConfigField::new(field::GATEWAY, "gateway", ValueKind::String)
            .describe("Static default gateway")
            .optional(),
        ConfigField::new(field::DNS, "dns", ValueKind::String)
            .describe("Static DNS server")
            .optional(),
        ConfigField::new(field::HOSTNAME, "hostname", ValueKind::String)
            .describe("Host name announced over DHCP")
            .optional(),
        timing(
            field::BACKOFF_INITIAL,
            "backoff_initial",
            BACKOFF_INITIAL_RANGE_MS,
        )
        .describe("First delay before a reconnect"),
        timing(field::BACKOFF_MAX, "backoff_max", BACKOFF_MAX_RANGE_MS)
            .describe("Longest delay between reconnects"),
        timing(field::LINK_REFRESH, "link_refresh", LINK_REFRESH_RANGE_MS)
            .describe("Link refresh and stability interval"),
        timing(field::SCAN_TIMEOUT, "scan_timeout", SCAN_TIMEOUT_RANGE_MS)
            .describe("Bound of a network scan"),
        timing(
            field::CONNECT_TIMEOUT,
            "connect_timeout",
            CONNECT_TIMEOUT_RANGE_MS,
        )
        .describe("Bound of joining the network"),
        timing(field::DHCP_TIMEOUT, "dhcp_timeout", DHCP_TIMEOUT_RANGE_MS)
            .describe("Bound of obtaining a DHCP lease"),
    ];

    fn defaults() -> Self {
        Self::new(WifiSsid::empty(), None)
    }

    /// An SSID or password that is not UTF-8 cannot be expressed as text and
    /// is reported as invalid; it is not shown, but it keeps working.
    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        let text = |value: &str| Value::text(value).map(Some).ok_or(FieldError::Invalid);
        let optional_address = |address: Option<Ipv4>| match address {
            None => Ok(None),
            Some(address) => address_text(address).map(Some).ok_or(FieldError::Invalid),
        };
        match id {
            field::SSID => self
                .ssid
                .as_str()
                .and_then(Value::text)
                .map(Some)
                .ok_or(FieldError::Invalid),
            field::PASSWORD => match &self.password {
                None => Ok(None),
                Some(password) => core::str::from_utf8(password.expose_secret())
                    .map_err(|_| FieldError::Invalid)
                    .and_then(text),
            },
            field::ADDRESS_MODE => {
                let name = match self.address_mode {
                    AddressMode::Dhcp => "dhcp",
                    AddressMode::Static => "static",
                };
                Ok(Value::variant(name))
            }
            field::IP => optional_address(self.static_ip),
            field::PREFIX_LEN => Ok(self.prefix_len.map(|len| Value::U64(len.into()))),
            field::GATEWAY => optional_address(self.gateway),
            field::DNS => optional_address(self.dns),
            field::HOSTNAME => match &self.hostname {
                None => Ok(None),
                Some(name) => text(name.as_str()),
            },
            field::BACKOFF_INITIAL => Ok(Some(Value::U64(self.backoff_initial_ms.into()))),
            field::BACKOFF_MAX => Ok(Some(Value::U64(self.backoff_max_ms.into()))),
            field::LINK_REFRESH => Ok(Some(Value::U64(self.link_refresh_ms.into()))),
            field::SCAN_TIMEOUT => Ok(Some(Value::U64(self.scan_timeout_ms.into()))),
            field::CONNECT_TIMEOUT => Ok(Some(Value::U64(self.connect_timeout_ms.into()))),
            field::DHCP_TIMEOUT => Ok(Some(Value::U64(self.dhcp_timeout_ms.into()))),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match id {
            field::SSID => {
                let text = value.as_str().ok_or(FieldError::WrongKind)?;
                self.ssid = WifiSsid::from_bytes(text.as_bytes()).ok_or(FieldError::Invalid)?;
            }
            field::PASSWORD => {
                let text = value.as_str().ok_or(FieldError::WrongKind)?;
                self.password =
                    Some(WifiPassword::from_bytes(text.as_bytes()).ok_or(FieldError::Invalid)?);
            }
            field::ADDRESS_MODE => {
                self.address_mode = match &value {
                    Value::Enum(variant) if variant == "dhcp" => AddressMode::Dhcp,
                    Value::Enum(variant) if variant == "static" => AddressMode::Static,
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
            field::BACKOFF_INITIAL => self.backoff_initial_ms = number(&value)?,
            field::BACKOFF_MAX => self.backoff_max_ms = number(&value)?,
            field::LINK_REFRESH => self.link_refresh_ms = number(&value)?,
            field::SCAN_TIMEOUT => self.scan_timeout_ms = number(&value)?,
            field::CONNECT_TIMEOUT => self.connect_timeout_ms = number(&value)?,
            field::DHCP_TIMEOUT => self.dhcp_timeout_ms = number(&value)?,
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::PASSWORD => self.password = None,
            field::IP => self.static_ip = None,
            field::PREFIX_LEN => self.prefix_len = None,
            field::GATEWAY => self.gateway = None,
            field::DNS => self.dns = None,
            field::HOSTNAME => self.hostname = None,
            field::SSID
            | field::ADDRESS_MODE
            | field::BACKOFF_INITIAL
            | field::BACKOFF_MAX
            | field::LINK_REFRESH
            | field::SCAN_TIMEOUT
            | field::CONNECT_TIMEOUT
            | field::DHCP_TIMEOUT => return Err(FieldError::NotOptional),
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
    fn the_schema_is_well_formed() {
        assert!(NetworkConfig::FIELDS.len() <= MAX_CONFIG_FIELDS);
        let mut ids: std::vec::Vec<_> = NetworkConfig::FIELDS.iter().map(|f| f.field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), NetworkConfig::FIELDS.len());
        for field in NetworkConfig::FIELDS {
            assert!(field.field.name.len() <= 24, "{}", field.field.name);
            assert!(field.field.description.len() <= 40, "{}", field.field.name);
        }
    }

    #[test]
    fn fields_round_trip_and_the_password_is_optional() {
        let mut config = NetworkConfig::defaults();
        assert!(ManagedConfig::validate(&config).is_err());
        config.set_field(field::SSID, text("office")).unwrap();
        assert_eq!(ManagedConfig::validate(&config), Ok(()));
        assert_eq!(config.read_field(field::PASSWORD), Ok(None));

        config
            .set_field(field::PASSWORD, text("hunter2hunter2"))
            .unwrap();
        assert_eq!(
            config.read_field(field::PASSWORD),
            Ok(Some(text("hunter2hunter2")))
        );
        assert_eq!(config.read_field(field::SSID), Ok(Some(text("office"))));
        config.unset_field(field::PASSWORD).unwrap();
        assert_eq!(config.password, None);
        assert_eq!(
            config.unset_field(field::SSID),
            Err(FieldError::NotOptional)
        );
    }

    #[test]
    fn the_defaults_are_the_former_constants() {
        let config = NetworkConfig::defaults();
        let read = |id| config.read_field(id).unwrap();
        assert_eq!(read(field::ADDRESS_MODE), Value::variant("dhcp"));
        assert_eq!(read(field::BACKOFF_INITIAL), Some(Value::U64(1_000)));
        assert_eq!(read(field::BACKOFF_MAX), Some(Value::U64(60_000)));
        assert_eq!(read(field::LINK_REFRESH), Some(Value::U64(10_000)));
        assert_eq!(read(field::SCAN_TIMEOUT), Some(Value::U64(20_000)));
        assert_eq!(read(field::CONNECT_TIMEOUT), Some(Value::U64(30_000)));
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
        config.set_field(field::SSID, text("office")).unwrap();
        config
            .set_field(field::ADDRESS_MODE, Value::variant("static").unwrap())
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
            config.set_field(field::ADDRESS_MODE, Value::variant("auto").unwrap()),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(field::HOSTNAME, text("bad name")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(field::BACKOFF_MAX, Value::U64(u64::MAX)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(field::SSID, text(&"a".repeat(33))),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(99, Value::U64(1)),
            Err(FieldError::UnknownField)
        );
    }

    #[test]
    fn the_timing_belongs_to_the_whole_section() {
        let mut config = NetworkConfig::defaults();
        config.set_field(field::SSID, text("office")).unwrap();
        config
            .set_field(field::BACKOFF_MAX, Value::U64(500))
            .unwrap();
        // Lower than the initial delay of 1 s.
        assert_eq!(ManagedConfig::validate(&config), Err(FieldError::Invalid));
        config
            .set_field(field::BACKOFF_INITIAL, Value::U64(200))
            .unwrap();
        assert_eq!(ManagedConfig::validate(&config), Ok(()));
        assert_eq!(
            config.unset_field(field::BACKOFF_MAX),
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
    fn the_complete_section_is_validated() {
        let mut config = NetworkConfig::defaults();
        config.set_field(field::SSID, text("office")).unwrap();
        config.set_field(field::PASSWORD, text("short")).unwrap();
        assert_eq!(ManagedConfig::validate(&config), Err(FieldError::Invalid));
    }

    #[test]
    fn the_password_is_the_only_secret_field() {
        let secret: std::vec::Vec<_> = NetworkConfig::FIELDS
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.field.name)
            .collect();
        assert_eq!(secret, ["password"]);
    }
}
