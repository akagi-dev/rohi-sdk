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
//
//! The persistent `time` section: which NTP servers to use.

use core::fmt;

use heapless::{String, Vec};
use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;
use rohi_service::{ConfigField, FieldError, ManagedConfig, Value, ValueKind};

use crate::{DEFAULT_SERVERS, MAX_SERVERS};

/// Longest configured server name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Field ids of the `time` config section.
pub mod field {
    /// First server, always set.
    pub const SERVER1: u32 = 1;
    /// Second server.
    pub const SERVER2: u32 = 2;
    /// Third server.
    pub const SERVER3: u32 = 3;
    /// Fourth server.
    pub const SERVER4: u32 = 4;
}

mod text_codec {
    use super::*;

    pub fn encode<C, W: Write, const N: usize>(
        value: &String<N>,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.str(value)?;
        Ok(())
    }

    pub fn decode<C, const N: usize>(
        d: &mut Decoder<'_>,
        _: &mut C,
    ) -> Result<String<N>, decode::Error> {
        let position = d.position();
        String::try_from(d.str()?)
            .map_err(|_| decode::Error::message("server name too long").at(position))
    }

    pub fn cbor_len<C, const N: usize>(value: &String<N>, ctx: &mut C) -> usize {
        <&str as CborLen<C>>::cbor_len(&value.as_str(), ctx)
    }
}

/// Host name or IPv4 address of an NTP server.
///
/// Letters, digits, `.` and `-` only, at most [`MAX_NAME_LEN`] bytes.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct ServerName(
    #[n(0)]
    #[cbor(
        encode_with = "text_codec::encode",
        decode_with = "text_codec::decode",
        cbor_len = "text_codec::cbor_len"
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

/// The NTP servers the device synchronizes its clock with.
///
/// The sources rotate through the servers in order, moving on after a
/// failure; unset slots are skipped. The first server is required.
///
/// Persistent ABI: the section ID is derived from the struct name
/// `NtpServersConfig`, the encoding is a CBOR array pinned by a golden test.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct NtpServersConfig {
    /// First server.
    #[n(0)]
    pub server1: Option<ServerName>,
    /// Second server.
    #[n(1)]
    pub server2: Option<ServerName>,
    /// Third server.
    #[n(2)]
    pub server3: Option<ServerName>,
    /// Fourth server.
    #[n(3)]
    pub server4: Option<ServerName>,
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

    fn slot(&mut self, id: u32) -> Result<&mut Option<ServerName>, FieldError> {
        match id {
            field::SERVER1 => Ok(&mut self.server1),
            field::SERVER2 => Ok(&mut self.server2),
            field::SERVER3 => Ok(&mut self.server3),
            field::SERVER4 => Ok(&mut self.server4),
            _ => Err(FieldError::UnknownField),
        }
    }
}

impl ManagedConfig for NtpServersConfig {
    const NAME: &'static str = "time";
    const VERSION: u16 = 1;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::SERVER1, "server1", ValueKind::String)
            .describe("NTP server host name or IPv4 address"),
        ConfigField::new(field::SERVER2, "server2", ValueKind::String)
            .describe("Second NTP server, used after a failure")
            .optional(),
        ConfigField::new(field::SERVER3, "server3", ValueKind::String)
            .describe("Third NTP server")
            .optional(),
        ConfigField::new(field::SERVER4, "server4", ValueKind::String)
            .describe("Fourth NTP server")
            .optional(),
    ];

    /// The public servers of [`DEFAULT_SERVERS`].
    fn defaults() -> Self {
        let mut names = DEFAULT_SERVERS.iter().map(|name| ServerName::new(name));
        Self {
            server1: names.next().flatten(),
            server2: names.next().flatten(),
            server3: names.next().flatten(),
            server4: names.next().flatten(),
        }
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        let slot = match id {
            field::SERVER1 => &self.server1,
            field::SERVER2 => &self.server2,
            field::SERVER3 => &self.server3,
            field::SERVER4 => &self.server4,
            _ => return Err(FieldError::UnknownField),
        };
        Ok(slot.as_ref().and_then(|name| Value::text(name.as_str())))
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        let name = value
            .as_str()
            .and_then(ServerName::new)
            .ok_or(FieldError::Invalid)?;
        *self.slot(id)? = Some(name);
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::SERVER1 => Err(FieldError::NotOptional),
            _ => {
                *self.slot(id)? = None;
                Ok(())
            }
        }
    }

    fn validate(&self) -> Result<(), FieldError> {
        let names_valid = [&self.server1, &self.server2, &self.server3, &self.server4]
            .into_iter()
            .flatten()
            .all(ServerName::is_valid);
        if self.server1.is_some() && names_valid {
            Ok(())
        } else {
            Err(FieldError::Invalid)
        }
    }
}

#[cfg(test)]
mod tests {
    use rohi_config::MAX_SECTION_PAYLOAD;

    use super::*;

    fn name(text: &str) -> Option<ServerName> {
        ServerName::new(text)
    }

    #[test]
    fn cbor_encoding_is_stable() {
        let config = NtpServersConfig {
            server1: name("a.io"),
            server2: None,
            server3: name("b"),
            server4: None,
        };
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(&config, &mut buffer).unwrap();
        // array(3): "a.io", null, "b"; the trailing unset slot is omitted.
        assert_eq!(
            &buffer[..used],
            &[0x83, 0x64, b'a', b'.', b'i', b'o', 0xf6, 0x61, b'b']
        );
        let decoded: NtpServersConfig = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, config);
    }

    #[test]
    fn section_id_and_version() {
        assert_eq!(
            NtpServersConfig::ID,
            rohi_config::SectionId(0x2feb_7d23_0f6e_d989)
        );
        assert_eq!(<NtpServersConfig as ConfigSection>::VERSION, 1);
        assert_eq!(
            NtpServersConfig::ID,
            rohi_config::SectionId::from_name("NtpServersConfig")
        );
    }

    #[test]
    fn the_largest_value_fits_a_section() {
        let long = "a".repeat(MAX_NAME_LEN);
        let config = NtpServersConfig {
            server1: name(&long),
            server2: name(&long),
            server3: name(&long),
            server4: name(&long),
        };
        let mut buffer = [0u8; MAX_SECTION_PAYLOAD];
        let used = rohi_config::encode_to_slice(&config, &mut buffer).unwrap();
        let decoded: NtpServersConfig = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, config);
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
        let defaults = NtpServersConfig::defaults();
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
    fn fields_are_read_set_and_unset() {
        let mut config = NtpServersConfig::defaults();
        config
            .set_field(field::SERVER3, Value::text("ntp.example").unwrap())
            .unwrap();
        assert_eq!(
            config.read_field(field::SERVER3),
            Ok(Some(Value::text("ntp.example").unwrap()))
        );
        config.unset_field(field::SERVER3).unwrap();
        assert_eq!(config.read_field(field::SERVER3), Ok(None));
        assert_eq!(
            config.unset_field(field::SERVER1),
            Err(FieldError::NotOptional)
        );
        assert_eq!(config.read_field(9), Err(FieldError::UnknownField));
        assert_eq!(
            config.set_field(9, Value::text("a").unwrap()),
            Err(FieldError::UnknownField)
        );
        assert_eq!(
            config.set_field(field::SERVER2, Value::text("bad name").unwrap()),
            Err(FieldError::Invalid)
        );
    }

    #[test]
    fn a_section_without_the_first_server_is_invalid() {
        let mut config = NtpServersConfig::defaults();
        config.server1 = None;
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
