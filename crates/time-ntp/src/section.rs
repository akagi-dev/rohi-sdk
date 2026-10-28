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
//! The persistent `time` section: which NTP servers to use.

use core::fmt;

use heapless::{String, Vec};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::{Text, TextValue};
use rohi_service::{Config, FieldError};

use crate::{DEFAULT_SERVERS, MAX_SERVERS};

/// Longest configured server name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Host name or IPv4 address of an NTP server.
///
/// Letters, digits, `.` and `-` only, at most [`MAX_NAME_LEN`] bytes.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct ServerName(
    #[n(0)]
    #[cbor(
        encode_with = "rohi_config::cbor::text::encode",
        decode_with = "rohi_config::cbor::text::decode",
        cbor_len = "rohi_config::cbor::text::cbor_len"
    )]
    String<MAX_NAME_LEN>,
);

impl ServerName {
    /// `None` if `name` is empty, too long or has forbidden characters.
    pub fn new(name: &str) -> Option<Self> {
        let valid = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
        if !valid {
            return None;
        }
        String::try_from(name).ok().map(Self)
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(&self) -> bool {
        Self::new(&self.0).is_some()
    }
}

impl fmt::Debug for ServerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// The NTP servers the device synchronizes its clock with (effective
/// configuration).
///
/// The sources rotate through the servers in order, moving on after a
/// failure; unset slots are skipped. The first server is required.
/// `NtpServersConfig::default()` holds the public servers of
/// [`DEFAULT_SERVERS`]. What is stored are the explicit user overrides of it,
/// [`NtpServersOverrides`]; a slot the user did not override follows the
/// default.
#[derive(Clone, Debug, PartialEq, Eq, Config)]
#[config(
    name = "time",
    overrides = NtpServersOverrides,
    version = 1,
    validate = validate_section
)]
pub struct NtpServersConfig {
    /// First server.
    #[config(id = 1, required, with = Text)]
    pub server1: Option<ServerName>,
    /// Second server.
    #[config(id = 2, with = Text)]
    pub server2: Option<ServerName>,
    /// Third server.
    #[config(id = 3, with = Text)]
    pub server3: Option<ServerName>,
    /// Fourth server.
    #[config(id = 4, with = Text)]
    pub server4: Option<ServerName>,
}

impl Default for NtpServersConfig {
    /// The public servers of [`DEFAULT_SERVERS`].
    fn default() -> Self {
        let mut names = DEFAULT_SERVERS.iter().map(|name| ServerName::new(name));
        Self {
            server1: names.next().flatten(),
            server2: names.next().flatten(),
            server3: names.next().flatten(),
            server4: names.next().flatten(),
        }
    }
}

impl NtpServersConfig {
    /// The configured servers in order of use, without the unset slots.
    pub fn servers(&self) -> Vec<&str, MAX_SERVERS> {
        [&self.server1, &self.server2, &self.server3, &self.server4]
            .into_iter()
            .flatten()
            .map(ServerName::as_str)
            .collect()
    }
}

/// A server name as text.
impl TextValue for ServerName {
    fn from_text(text: &str) -> Option<Self> {
        Self::new(text)
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.as_str())
    }
}

/// The first server is required and every name must be valid.
fn validate_section(config: &NtpServersConfig) -> Result<(), FieldError> {
    let names_valid = [
        &config.server1,
        &config.server2,
        &config.server3,
        &config.server4,
    ]
    .into_iter()
    .flatten()
    .all(ServerName::is_valid);
    if config.server1.is_some() && names_valid {
        Ok(())
    } else {
        Err(FieldError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use rohi_config::{ConfigOverrides, ConfigSection, MAX_SECTION_PAYLOAD, Override, Value};

    use super::*;

    const SERVER1: u32 = 1;
    const SERVER2: u32 = 2;
    const SERVER3: u32 = 3;
    const SERVER4: u32 = 4;

    fn name(text: &str) -> Option<ServerName> {
        ServerName::new(text)
    }

    #[test]
    fn cbor_encoding_is_stable() {
        let overrides = NtpServersOverrides {
            server1: Override::Value(name("a.io")),
            server3: Override::Value(name("b")),
            ..NtpServersOverrides::default()
        };
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
        // map(2): 1: "a.io", 3: "b"; the other slots are not overridden.
        assert_eq!(
            &buffer[..used],
            &[0xa2, 0x01, 0x64, b'a', b'.', b'i', b'o', 0x03, 0x61, b'b']
        );
        let decoded: NtpServersOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, overrides);
        // The second server is not overridden: it follows the default.
        assert_eq!(
            rohi_config::materialize::<NtpServersConfig>(&Default::default(), &decoded)
                .servers()
                .as_slice(),
            ["a.io", DEFAULT_SERVERS[1], "b"]
        );
    }

    #[test]
    fn section_id_and_version() {
        assert_eq!(
            NtpServersOverrides::ID,
            rohi_config::SectionId(0x2feb_7d23_0f6e_d989)
        );
        assert_eq!(<NtpServersOverrides as ConfigSection>::VERSION, 1);
        assert_eq!(
            NtpServersOverrides::ID,
            rohi_config::SectionId::from_name("NtpServersConfig")
        );
    }

    #[test]
    fn every_field_can_be_overridden() {
        let mut ids: std::vec::Vec<_> = NtpServersConfig::FIELDS
            .iter()
            .map(|f| f.field.id)
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, NtpServersOverrides::FIELD_IDS);
    }

    #[test]
    fn the_largest_value_fits_a_section() {
        let long = || Override::Value(name(&"a".repeat(MAX_NAME_LEN)));
        let overrides = NtpServersOverrides {
            server1: long(),
            server2: long(),
            server3: long(),
            server4: long(),
        };
        let mut buffer = [0u8; MAX_SECTION_PAYLOAD];
        let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
        let decoded: NtpServersOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, overrides);
    }

    #[test]
    fn server_names_are_plain_host_names() {
        assert!(name("pool.ntp.org").is_some());
        assert!(name("192.0.2.1").is_some());
        assert!(name("").is_none());
        assert!(name("a b").is_none());
        assert!(name("a:123").is_none());
        assert!(name(&"a".repeat(MAX_NAME_LEN + 1)).is_none());
    }

    #[test]
    fn the_defaults_are_the_public_servers() {
        let defaults = NtpServersConfig::default();
        assert_eq!(defaults.validate(), Ok(()));
        assert_eq!(defaults.servers().as_slice(), DEFAULT_SERVERS);
    }

    #[test]
    fn unset_slots_are_skipped_in_order() {
        let config = NtpServersConfig {
            server1: name("a"),
            server2: None,
            server3: name("c"),
            server4: name("d"),
        };
        assert_eq!(config.servers().as_slice(), ["a", "c", "d"]);
    }

    #[test]
    fn fields_are_read_set_and_cleared() {
        let mut config = NtpServersConfig::default();
        config
            .set_field(SERVER3, Value::text("ntp.example").unwrap())
            .unwrap();
        assert_eq!(
            config.read_field(SERVER3),
            Ok(Some(Value::text("ntp.example").unwrap()))
        );
        let mut overrides = NtpServersOverrides::default();
        overrides.capture(&config, SERVER3);
        assert_eq!(
            rohi_config::materialize::<NtpServersConfig>(&Default::default(), &overrides).server3,
            config.server3
        );
        overrides.clear(SERVER3);
        assert_eq!(
            rohi_config::materialize::<NtpServersConfig>(&Default::default(), &overrides)
                .read_field(SERVER3),
            Ok(None)
        );
        // A cleared first server is the default, not a missing one.
        overrides.capture(&config, SERVER1);
        overrides.clear(SERVER1);
        assert_eq!(
            rohi_config::materialize::<NtpServersConfig>(&Default::default(), &overrides)
                .validate(),
            Ok(())
        );
        assert_eq!(config.read_field(9), Err(FieldError::UnknownField));
        assert_eq!(
            config.set_field(9, Value::text("a").unwrap()),
            Err(FieldError::UnknownField)
        );
        assert_eq!(
            config.set_field(SERVER2, Value::text("bad name").unwrap()),
            Err(FieldError::Invalid)
        );
    }

    #[test]
    fn none_disables_an_optional_server_and_the_first_is_required() {
        let mut config = NtpServersConfig::default();
        assert_eq!(config.set_none(SERVER2), Ok(()));
        assert_eq!(config.server2, None);
        assert_eq!(config.set_none(SERVER4), Ok(()));
        assert_eq!(config.set_none(SERVER1), Err(FieldError::NotOptional));
        assert_eq!(config.set_none(9), Err(FieldError::UnknownField));
        assert!(
            NtpServersConfig::FIELDS
                .iter()
                .all(|f| f.optional == (f.field.id != SERVER1))
        );
    }

    #[test]
    fn an_explicit_none_differs_from_the_default_server() {
        let mut overrides = NtpServersOverrides::default();
        let mut config = NtpServersConfig::default();
        config.set_none(SERVER2).unwrap();
        overrides.capture(&config, SERVER2);
        assert_eq!(overrides.server2, Override::Value(None));
        let mut effective = NtpServersConfig::default();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.server2, None);
        overrides.clear(SERVER2);
        let mut effective = NtpServersConfig::default();
        overrides.apply_to(&mut effective);
        assert_eq!(effective.server2, NtpServersConfig::default().server2);
    }

    #[test]
    fn a_section_without_the_first_server_is_invalid() {
        let config = NtpServersConfig {
            server1: None,
            ..NtpServersConfig::default()
        };
        assert_eq!(config.validate(), Err(FieldError::Invalid));
    }

    #[test]
    fn the_schema_fits_the_limits() {
        let fields = NtpServersConfig::FIELDS;
        assert!(fields.len() <= rohi_service::MAX_CONFIG_FIELDS);
        assert!(fields.len() == MAX_SERVERS);
        assert!(fields.iter().all(|f| f.field.name.len() <= 24));
    }
}
