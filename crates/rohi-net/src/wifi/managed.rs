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
//! The `wifi` config section in the generic management plane: the field codecs
//! of the domain types and the validation of the section. The fields, their
//! ids and the persisted overrides are declared once, on [`WifiConfig`].

use rohi_service::{Codec, FieldError, Value, ValueKind};

use super::config::WifiConfig;
use crate::{WifiPassword, WifiSsid};

/// The SSID as text.
///
/// An SSID that is not UTF-8 cannot be expressed as text and is reported as
/// invalid; it is not shown, but it keeps working. An empty name is not a
/// network: `none` disables Wi-Fi instead.
pub struct SsidCodec;

impl Codec<WifiSsid> for SsidCodec {
    const KIND: ValueKind = ValueKind::String;

    fn encode(ssid: &WifiSsid) -> Result<Option<Value>, FieldError> {
        ssid.as_str()
            .and_then(Value::text)
            .map(Some)
            .ok_or(FieldError::Invalid)
    }

    fn decode(value: &Value) -> Result<WifiSsid, FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        WifiSsid::from_bytes(text.as_bytes())
            .filter(|ssid| !ssid.is_empty())
            .ok_or(FieldError::Invalid)
    }
}

/// The passphrase as text. It is write-only: it is never converted to a value.
pub struct PasswordCodec;

impl Codec<WifiPassword> for PasswordCodec {
    const KIND: ValueKind = ValueKind::String;

    fn encode(_: &WifiPassword) -> Result<Option<Value>, FieldError> {
        Err(FieldError::Secret)
    }

    fn decode(value: &Value) -> Result<WifiPassword, FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        WifiPassword::from_bytes(text.as_bytes()).ok_or(FieldError::Invalid)
    }
}

/// A section without an SSID is valid: Wi-Fi is disabled, the radio stays
/// idle. The credentials and the timing are checked as a whole.
pub(super) fn validate_section(config: &WifiConfig) -> Result<(), FieldError> {
    config.validate_settings().map_err(|_| FieldError::Invalid)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rohi_config::{ConfigOverrides, ConfigSection, Override};
    use rohi_service::{Config, MAX_CONFIG_FIELDS};

    use super::*;
    use crate::WifiOverrides;

    const SSID: u32 = 1;
    const PASSWORD: u32 = 2;
    const SCAN_TIMEOUT: u32 = 3;
    const CONNECT_TIMEOUT: u32 = 4;
    const BACKOFF_INITIAL: u32 = 5;
    const BACKOFF_MAX: u32 = 6;
    const LINK_REFRESH: u32 = 7;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn the_reported_version_is_the_stored_one() {
        assert_eq!(<WifiOverrides as ConfigSection>::VERSION, 1);
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
        assert_eq!(Config::validate(&config), Ok(()));
        assert!(!config.is_configured());
        assert!(WifiConfig::validate(&config).is_err());
        config.set_field(SSID, text("office")).unwrap();
        assert!(config.is_configured());
        assert_eq!(Config::validate(&config), Ok(()));
        assert_eq!(config.field_is_set(PASSWORD), Ok(false));

        config.set_field(PASSWORD, text("hunter2hunter2")).unwrap();
        assert_eq!(config.field_is_set(PASSWORD), Ok(true));
        assert_eq!(config.password, WifiPassword::from_bytes(b"hunter2hunter2"));
        assert_eq!(config.read_field(SSID), Ok(Some(text("office"))));
    }

    #[test]
    fn the_password_is_never_converted_to_a_value() {
        let mut config = WifiConfig::default();
        config.set_field(PASSWORD, text("hunter2hunter2")).unwrap();
        // Neither a read nor the presence check yields it.
        assert_eq!(config.read_field(PASSWORD), Err(FieldError::Secret));
        assert_eq!(config.field_is_set(PASSWORD), Ok(true));
        assert_eq!(
            PasswordCodec::encode(&config.password.clone().unwrap()),
            Err(FieldError::Secret)
        );
        assert!(
            WifiConfig::FIELDS
                .iter()
                .find(|field| field.field.id == PASSWORD)
                .unwrap()
                .secret
        );
    }

    #[test]
    fn no_debug_output_shows_the_password() {
        let mut config = WifiConfig::default();
        config.set_field(PASSWORD, text("hunter2hunter2")).unwrap();
        let mut overrides = WifiOverrides::default();
        assert!(overrides.capture(&config, PASSWORD));
        for shown in [
            std::format!("{config:?}"),
            std::format!("{config:#?}"),
            std::format!("{overrides:?}"),
            std::format!("{overrides:#?}"),
        ] {
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(shown.contains("redacted"), "{shown}");
        }
    }

    #[test]
    fn clearing_the_password_override_restores_the_open_network_default() {
        let mut overrides = WifiOverrides::default();
        let mut config = rohi_config::materialize::<WifiConfig>(&Default::default(), &overrides);
        config.set_field(PASSWORD, text("hunter2hunter2")).unwrap();
        assert!(overrides.capture(&config, PASSWORD));
        assert_eq!(
            rohi_config::materialize::<WifiConfig>(&Default::default(), &overrides).password,
            config.password
        );
        assert!(overrides.clear(PASSWORD));
        assert_eq!(
            rohi_config::materialize::<WifiConfig>(&Default::default(), &overrides).password,
            None
        );
        assert!(overrides.is_empty());
    }

    #[test]
    fn none_is_only_for_the_ssid_and_the_password() {
        let mut config = WifiConfig::default();
        config.set_field(PASSWORD, text("hunter2hunter2")).unwrap();
        assert_eq!(config.set_none(PASSWORD), Ok(()));
        assert_eq!(config.password, None);
        for id in [
            SCAN_TIMEOUT,
            CONNECT_TIMEOUT,
            BACKOFF_INITIAL,
            BACKOFF_MAX,
            LINK_REFRESH,
        ] {
            assert_eq!(config.set_none(id), Err(FieldError::NotOptional));
        }
        assert_eq!(config.set_none(99), Err(FieldError::UnknownField));
    }

    #[test]
    fn no_ssid_disables_wifi_and_an_ssid_enables_it() {
        let mut config = WifiConfig::default();
        assert_eq!(config.ssid, None);
        assert_eq!(config.read_field(SSID), Ok(None));
        assert!(!config.is_configured());
        assert_eq!(config.clone().configured(), None);
        config.set_field(SSID, text("office")).unwrap();
        assert!(config.is_configured());
        assert!(config.clone().configured().is_some());
        // An empty name is not a network: `none` disables Wi-Fi instead.
        assert_eq!(config.set_field(SSID, text("")), Err(FieldError::Invalid));
        assert_eq!(config.set_none(SSID), Ok(()));
        assert_eq!(config.read_field(SSID), Ok(None));
        assert_eq!(Config::validate(&config), Ok(()));
    }

    #[test]
    fn an_explicit_no_ssid_overrides_a_firmware_default_network() {
        let firmware = WifiConfig::new(WifiSsid::from_bytes(b"factory").unwrap(), None);
        let mut overrides = WifiOverrides::default();
        let mut config = firmware.clone();
        config.set_none(SSID).unwrap();
        overrides.capture(&config, SSID);
        assert_eq!(overrides.ssid, Override::Value(None));
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert!(!effective.is_configured());
        overrides.clear(SSID);
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.ssid, firmware.ssid);
    }

    #[test]
    fn an_explicit_open_network_differs_from_the_default_password() {
        let firmware = WifiConfig {
            password: Some(WifiPassword::from_bytes(b"factory-password").unwrap()),
            ..WifiConfig::default()
        };
        let mut overrides = WifiOverrides::default();
        let mut config = firmware.clone();
        config.set_none(PASSWORD).unwrap();
        assert!(overrides.capture(&config, PASSWORD));
        assert_eq!(overrides.password, Override::Value(None));
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.password, None);
        // Clearing follows the firmware default again.
        assert!(overrides.clear(PASSWORD));
        let mut effective = firmware.clone();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.password, firmware.password);
    }

    #[test]
    fn the_defaults_are_the_former_constants() {
        let config = WifiConfig::default();
        let read = |id| config.read_field(id).unwrap();
        assert_eq!(read(SCAN_TIMEOUT), Some(Value::U64(20_000)));
        assert_eq!(read(CONNECT_TIMEOUT), Some(Value::U64(30_000)));
        assert_eq!(read(BACKOFF_INITIAL), Some(Value::U64(1_000)));
        assert_eq!(read(BACKOFF_MAX), Some(Value::U64(60_000)));
        assert_eq!(read(LINK_REFRESH), Some(Value::U64(10_000)));
    }

    #[test]
    fn bad_values_are_refused() {
        let mut config = WifiConfig::default();
        assert_eq!(
            config.set_field(SSID, Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            config.set_field(BACKOFF_MAX, Value::U64(u64::MAX)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(SSID, text(&"a".repeat(33))),
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
        config.set_field(SSID, text("office")).unwrap();
        config.set_field(BACKOFF_MAX, Value::U64(500)).unwrap();
        // Lower than the initial delay of 1 s.
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));
        config.set_field(BACKOFF_INITIAL, Value::U64(200)).unwrap();
        assert_eq!(Config::validate(&config), Ok(()));
    }

    #[test]
    fn the_complete_section_is_validated() {
        let mut config = WifiConfig::default();
        config.set_field(SSID, text("office")).unwrap();
        config.set_field(PASSWORD, text("short")).unwrap();
        assert_eq!(Config::validate(&config), Err(FieldError::Invalid));
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
