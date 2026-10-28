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

//! The `wifi` config section in the generic management plane.

use rohi_config::ConfigSection;
use rohi_service::{Bounds, ConfigField, FieldError, ManagedConfig, Unit, Value, ValueKind};

use super::config::{
    BACKOFF_INITIAL_RANGE_MS, BACKOFF_MAX_RANGE_MS, CONNECT_TIMEOUT_RANGE_MS,
    LINK_REFRESH_RANGE_MS, SCAN_TIMEOUT_RANGE_MS, WifiConfig, WifiOverrides,
};
use crate::{WifiPassword, WifiSsid};

/// Field ids of the `wifi` config section. Only ever added.
pub mod field {
    /// Network name; text only.
    pub const SSID: u32 = 1;
    /// Write-only passphrase; none (the default) for an open network.
    pub const PASSWORD: u32 = 2;
    /// Bound of a scan.
    pub const SCAN_TIMEOUT: u32 = 3;
    /// Bound of associating with the access point.
    pub const CONNECT_TIMEOUT: u32 = 4;
    /// First reconnect delay.
    pub const BACKOFF_INITIAL: u32 = 5;
    /// Longest reconnect delay.
    pub const BACKOFF_MAX: u32 = 6;
    /// Refresh interval of the connection details.
    pub const LINK_REFRESH: u32 = 7;
}

const fn timing(id: u32, name: &'static str, (min, max): (u32, u32)) -> ConfigField {
    ConfigField::new(id, name, ValueKind::U64)
        .unit(Unit::Ms)
        .bounds(Bounds::U64(min as u64, max as u64))
}

fn number(value: &Value) -> Result<u32, FieldError> {
    match value {
        Value::U64(number) => u32::try_from(*number).map_err(|_| FieldError::OutOfRange),
        _ => Err(FieldError::WrongKind),
    }
}

impl ManagedConfig for WifiConfig {
    const SECTION_ID: u64 = <WifiOverrides as ConfigSection>::ID.0;
    const NAME: &'static str = "wifi";
    const VERSION: u16 = <WifiOverrides as ConfigSection>::VERSION;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::SSID, "ssid", ValueKind::String),
        ConfigField::new(field::PASSWORD, "password", ValueKind::String)
            .secret()
            .optional(),
        timing(field::SCAN_TIMEOUT, "scan_timeout", SCAN_TIMEOUT_RANGE_MS),
        timing(
            field::CONNECT_TIMEOUT,
            "connect_timeout",
            CONNECT_TIMEOUT_RANGE_MS,
        ),
        timing(
            field::BACKOFF_INITIAL,
            "backoff_initial",
            BACKOFF_INITIAL_RANGE_MS,
        ),
        timing(field::BACKOFF_MAX, "backoff_max", BACKOFF_MAX_RANGE_MS),
        timing(field::LINK_REFRESH, "link_refresh", LINK_REFRESH_RANGE_MS),
    ];

    type Overrides = WifiOverrides;

    /// An SSID or password that is not UTF-8 cannot be expressed as text and
    /// is reported as invalid; it is not shown, but it keeps working.
    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        let text = |value: &str| Value::text(value).map(Some).ok_or(FieldError::Invalid);
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
            field::SCAN_TIMEOUT => Ok(Some(Value::U64(self.scan_timeout_ms.into()))),
            field::CONNECT_TIMEOUT => Ok(Some(Value::U64(self.connect_timeout_ms.into()))),
            field::BACKOFF_INITIAL => Ok(Some(Value::U64(self.backoff_initial_ms.into()))),
            field::BACKOFF_MAX => Ok(Some(Value::U64(self.backoff_max_ms.into()))),
            field::LINK_REFRESH => Ok(Some(Value::U64(self.link_refresh_ms.into()))),
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
            field::SCAN_TIMEOUT => self.scan_timeout_ms = number(&value)?,
            field::CONNECT_TIMEOUT => self.connect_timeout_ms = number(&value)?,
            field::BACKOFF_INITIAL => self.backoff_initial_ms = number(&value)?,
            field::BACKOFF_MAX => self.backoff_max_ms = number(&value)?,
            field::LINK_REFRESH => self.link_refresh_ms = number(&value)?,
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn set_none(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            // An open network.
            field::PASSWORD => {
                self.password = None;
                Ok(())
            }
            field::SSID
            | field::SCAN_TIMEOUT
            | field::CONNECT_TIMEOUT
            | field::BACKOFF_INITIAL
            | field::BACKOFF_MAX
            | field::LINK_REFRESH => Err(FieldError::NotOptional),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn field_is_set(&self, id: u32) -> Result<bool, FieldError> {
        match id {
            // The password is only asked whether it exists, never read.
            field::PASSWORD => Ok(self.password.is_some()),
            _ => Ok(self.read_field(id)?.is_some()),
        }
    }

    /// An unconfigured section (no SSID) is valid: the radio stays idle.
    fn validate(&self) -> Result<(), FieldError> {
        self.validate_settings().map_err(|_| FieldError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rohi_config::{ConfigOverrides, Override};
    use rohi_service::MAX_CONFIG_FIELDS;

    use super::*;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn the_reported_version_is_the_stored_one() {
        assert_eq!(<WifiConfig as ManagedConfig>::VERSION, 1);
    }

    #[test]
    fn every_field_can_be_overridden() {
        let mut ids: std::vec::Vec<_> = WifiConfig::FIELDS.iter().map(|f| f.field.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, WifiOverrides::FIELD_IDS);
    }

    #[test]
    fn the_schema_is_well_formed() {
        assert!(WifiConfig::FIELDS.len() <= MAX_CONFIG_FIELDS);
        let mut ids: std::vec::Vec<_> = WifiConfig::FIELDS.iter().map(|f| f.field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), WifiConfig::FIELDS.len());
        for field in WifiConfig::FIELDS {
            assert!(field.field.name.len() <= 24, "{}", field.field.name);
        }
    }

    #[test]
    fn fields_round_trip_and_the_password_is_optional() {
        let mut config = WifiConfig::default();
        // No network is a valid, unconfigured section.
        assert_eq!(ManagedConfig::validate(&config), Ok(()));
        assert!(!config.is_configured());
        assert!(WifiConfig::validate(&config).is_err());
        config.set_field(field::SSID, text("office")).unwrap();
        assert!(config.is_configured());
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
    }

    #[test]
    fn clearing_the_password_override_restores_the_open_network_default() {
        let mut overrides = WifiOverrides::default();
        let mut config = overrides.over_default();
        config
            .set_field(field::PASSWORD, text("hunter2hunter2"))
            .unwrap();
        assert!(overrides.capture(&config, field::PASSWORD));
        assert_eq!(overrides.over_default().password, config.password);
        assert!(overrides.clear(field::PASSWORD));
        assert_eq!(overrides.over_default().password, None);
        assert!(overrides.is_empty());
    }

    #[test]
    fn none_is_only_for_the_password() {
        let mut config = WifiConfig::default();
        config
            .set_field(field::PASSWORD, text("hunter2hunter2"))
            .unwrap();
        assert_eq!(config.set_none(field::PASSWORD), Ok(()));
        assert_eq!(config.password, None);
        for id in [
            field::SSID,
            field::SCAN_TIMEOUT,
            field::CONNECT_TIMEOUT,
            field::BACKOFF_INITIAL,
            field::BACKOFF_MAX,
            field::LINK_REFRESH,
        ] {
            assert_eq!(config.set_none(id), Err(FieldError::NotOptional));
        }
        assert_eq!(config.set_none(99), Err(FieldError::UnknownField));
    }

    #[test]
    fn an_explicit_open_network_differs_from_the_default_password() {
        let firmware = WifiConfig {
            password: Some(WifiPassword::from_bytes(b"factory-password").unwrap()),
            ..WifiConfig::default()
        };
        let mut overrides = WifiOverrides::default();
        let mut config = firmware.clone();
        config.set_none(field::PASSWORD).unwrap();
        assert!(overrides.capture(&config, field::PASSWORD));
        assert_eq!(overrides.password, Override::Value(None));
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.password, None);
        // Clearing follows the firmware default again.
        assert!(overrides.clear(field::PASSWORD));
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.password, firmware.password);
    }

    #[test]
    fn the_defaults_are_the_former_constants() {
        let config = WifiConfig::default();
        let read = |id| config.read_field(id).unwrap();
        assert_eq!(read(field::SCAN_TIMEOUT), Some(Value::U64(20_000)));
        assert_eq!(read(field::CONNECT_TIMEOUT), Some(Value::U64(30_000)));
        assert_eq!(read(field::BACKOFF_INITIAL), Some(Value::U64(1_000)));
        assert_eq!(read(field::BACKOFF_MAX), Some(Value::U64(60_000)));
        assert_eq!(read(field::LINK_REFRESH), Some(Value::U64(10_000)));
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = WifiConfig::default();
        assert_eq!(
            config.set_field(field::SSID, Value::U64(1)),
            Err(FieldError::WrongKind)
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
        let mut config = WifiConfig::default();
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
    }

    #[test]
    fn the_complete_section_is_validated() {
        let mut config = WifiConfig::default();
        config.set_field(field::SSID, text("office")).unwrap();
        config.set_field(field::PASSWORD, text("short")).unwrap();
        assert_eq!(ManagedConfig::validate(&config), Err(FieldError::Invalid));
    }

    #[test]
    fn the_password_is_the_only_secret_field() {
        let secret: std::vec::Vec<_> = WifiConfig::FIELDS
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.field.name)
            .collect();
        assert_eq!(secret, ["password"]);
    }
}
