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
//! Persistent telemetry configuration.

use core::fmt;

use heapless::Vec;
use minicbor::bytes::ByteSlice;
use minicbor::decode::{self, Decoder};
use minicbor::encode::{self, Encoder, Write};
use minicbor::{CborLen, Decode, Encode};
use rohi_service::Config;

use crate::managed::{HostCodec, validate_section};

/// Maximum length of the configured host name, in bytes.
pub const MAX_HOST_LEN: usize = 64;
/// Shortest allowed telemetry interval, in seconds.
pub const MIN_INTERVAL_S: u32 = 10;
/// Longest allowed telemetry interval, in seconds.
pub const MAX_INTERVAL_S: u32 = 24 * 60 * 60;
/// Default telemetry interval, in seconds.
pub const DEFAULT_INTERVAL_S: u32 = 60;
/// Default HTTP port.
pub const DEFAULT_PORT: u16 = 80;

mod bytes_codec {
    use super::*;

    pub fn encode<C, W: Write, const N: usize>(
        value: &Vec<u8, N>,
        e: &mut Encoder<W>,
        _: &mut C,
    ) -> Result<(), encode::Error<W::Error>> {
        e.bytes(value)?;
        Ok(())
    }

    pub fn decode<C, const N: usize>(
        d: &mut Decoder<'_>,
        _: &mut C,
    ) -> Result<Vec<u8, N>, decode::Error> {
        let position = d.position();
        Vec::from_slice(d.bytes()?)
            .map_err(|_| decode::Error::message("byte string too long").at(position))
    }

    pub fn cbor_len<C, const N: usize>(value: &Vec<u8, N>, ctx: &mut C) -> usize {
        <&ByteSlice>::from(value.as_slice()).cbor_len(ctx)
    }
}

/// Invalid telemetry configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// The host is empty, too long or not a plain host name or IP address.
    InvalidHost,
    /// The port is zero.
    InvalidPort,
    /// The interval is outside `MIN_INTERVAL_S..=MAX_INTERVAL_S`.
    InvalidInterval,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidHost => "invalid telemetry host",
            Self::InvalidPort => "invalid telemetry port",
            Self::InvalidInterval => "invalid telemetry interval",
        })
    }
}

impl core::error::Error for ConfigError {}

/// Host name of the telemetry ingress.
///
/// Letters, digits, `.` and `-` only (a DNS name or an IPv4 address): the value is written into the
/// HTTP `Host` header, so it can never carry spaces or line breaks.
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct Host(
    #[n(0)]
    #[cbor(
        encode_with = "bytes_codec::encode",
        decode_with = "bytes_codec::decode",
        cbor_len = "bytes_codec::cbor_len"
    )]
    Vec<u8, MAX_HOST_LEN>,
);

impl Host {
    /// The empty (invalid) host.
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    /// `None` if `host` is empty, too long or has forbidden characters.
    pub fn new(host: &str) -> Option<Self> {
        let valid = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
        if !valid {
            return None;
        }
        Vec::from_slice(host.as_bytes()).ok().map(Self)
    }

    /// The host as text.
    pub fn as_str(&self) -> &str {
        // Validated ASCII on construction and on `ConnectivityConfig::validate`.
        core::str::from_utf8(&self.0).unwrap_or("")
    }

    fn is_valid(&self) -> bool {
        core::str::from_utf8(&self.0)
            .ok()
            .is_some_and(|host| Self::new(host).is_some())
    }
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Where telemetry is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint<'a> {
    /// Host name or IPv4 address, also the HTTP `Host` header.
    pub host: &'a str,
    /// TCP port.
    pub port: u16,
}

/// Effective telemetry configuration (section `connectivity`).
///
/// `ConnectivityConfig::default()` is [`disabled`](Self::disabled): no host
/// and not valid on its own. What is stored are the explicit user overrides
/// of it, [`ConnectivityOverrides`].
#[derive(Clone, PartialEq, Eq, Debug, Config)]
#[config(
    name = "connectivity",
    overrides = ConnectivityOverrides,
    version = 1,
    validate = validate_section
)]
pub struct ConnectivityConfig {
    /// Telemetry is only produced and sent when enabled.
    #[config(id = 1)]
    pub enabled: bool,
    /// Robonomics CPS node identifier (`core.v1.Meta.node_id`).
    #[config(id = 2)]
    pub node_id: u64,
    /// Seconds between telemetry frames.
    #[config(id = 3, unit = S, min = MIN_INTERVAL_S, max = MAX_INTERVAL_S)]
    pub interval_s: u32,
    /// Ingress host.
    #[config(id = 4, with = HostCodec)]
    pub host: Host,
    /// Ingress TCP port.
    #[config(id = 5, min = 1, max = 65535)]
    pub port: u16,
}

impl ConnectivityConfig {
    /// A disabled configuration with default interval and port, and no host.
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            node_id: 0,
            interval_s: DEFAULT_INTERVAL_S,
            host: Host::empty(),
            port: DEFAULT_PORT,
        }
    }

    /// Checks the configuration before it is persisted or applied.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.host.is_valid() {
            return Err(ConfigError::InvalidHost);
        }
        if self.port == 0 {
            return Err(ConfigError::InvalidPort);
        }
        if !(MIN_INTERVAL_S..=MAX_INTERVAL_S).contains(&self.interval_s) {
            return Err(ConfigError::InvalidInterval);
        }
        Ok(())
    }

    /// The ingress endpoint.
    pub fn endpoint(&self) -> Endpoint<'_> {
        Endpoint {
            host: self.host.as_str(),
            port: self.port,
        }
    }

    /// Telemetry interval in milliseconds.
    pub const fn interval_ms(&self) -> u64 {
        self.interval_s as u64 * 1_000
    }
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

#[cfg(test)]
mod tests {
    use rohi_config::{ConfigSection, MAX_SECTION_PAYLOAD, Override};

    use super::*;

    extern crate std;

    fn config() -> ConnectivityConfig {
        ConnectivityConfig {
            enabled: true,
            node_id: 5,
            interval_s: 60,
            host: Host::new("a.io").unwrap(),
            port: 8080,
        }
    }

    fn overrides() -> ConnectivityOverrides {
        ConnectivityOverrides {
            enabled: Override::Value(true),
            node_id: Override::Value(5),
            host: Override::Value(Host::new("a.io").unwrap()),
            port: Override::Value(8080),
            ..ConnectivityOverrides::default()
        }
    }

    #[test]
    fn cbor_encoding_is_stable() {
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(&overrides(), &mut buffer).unwrap();
        assert_eq!(
            &buffer[..used],
            [
                0xa4, // map(4)
                0x01, 0xf5, // 1: enabled
                0x02, 0x05, // 2: node_id
                0x04, 0x44, b'a', b'.', b'i', b'o', // 4: host
                0x05, 0x19, 0x1f, 0x90, // 5: port
            ]
        );
        assert_eq!(ConnectivityOverrides::VERSION, 1);
        assert_eq!(
            ConnectivityOverrides::ID,
            rohi_config::SectionId::from_name("ConnectivityConfig")
        );
        let used =
            rohi_config::encode_to_slice(&ConnectivityOverrides::default(), &mut buffer).unwrap();
        assert_eq!(&buffer[..used], [0xa0]);
    }

    #[test]
    fn overrides_apply_to_the_defaults() {
        let effective =
            rohi_config::materialize::<ConnectivityConfig>(&Default::default(), &overrides());
        assert_eq!(effective, config_of_overrides());
        // The interval is not overridden: it is the default.
        assert_eq!(effective.interval_s, DEFAULT_INTERVAL_S);
    }

    fn config_of_overrides() -> ConnectivityConfig {
        ConnectivityConfig {
            enabled: true,
            node_id: 5,
            interval_s: DEFAULT_INTERVAL_S,
            host: Host::new("a.io").unwrap(),
            port: 8080,
        }
    }

    #[test]
    fn round_trips_and_fits_a_section() {
        let mut buffer = [0u8; MAX_SECTION_PAYLOAD];
        let overrides = ConnectivityOverrides {
            enabled: Override::Value(true),
            node_id: Override::Value(u64::MAX),
            interval_s: Override::Value(u32::MAX),
            host: Override::Value(Host::new(&"a".repeat(MAX_HOST_LEN)).unwrap()),
            port: Override::Value(u16::MAX),
        };
        let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
        let decoded: ConnectivityOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, overrides);
    }

    #[test]
    fn validation() {
        assert_eq!(config().validate(), Ok(()));
        assert_eq!(
            ConnectivityConfig::disabled().validate(),
            Err(ConfigError::InvalidHost)
        );
        let mut c = config();
        c.port = 0;
        assert_eq!(c.validate(), Err(ConfigError::InvalidPort));
        let mut c = config();
        c.interval_s = MIN_INTERVAL_S - 1;
        assert_eq!(c.validate(), Err(ConfigError::InvalidInterval));
        c.interval_s = MAX_INTERVAL_S + 1;
        assert_eq!(c.validate(), Err(ConfigError::InvalidInterval));
    }

    #[test]
    fn host_cannot_inject_header_bytes() {
        assert!(Host::new("ok.example").is_some());
        assert!(Host::new("ok.example:80").is_none());
        assert!(Host::new("").is_none());
        assert!(Host::new("a b").is_none());
        assert!(Host::new("a\r\nX: 1").is_none());
        assert!(Host::new(&"a".repeat(MAX_HOST_LEN + 1)).is_none());
    }

    #[test]
    fn endpoint_borrows_the_config() {
        let config = config();
        assert_eq!(
            config.endpoint(),
            Endpoint {
                host: "a.io",
                port: 8080
            }
        );
        assert_eq!(config.interval_ms(), 60_000);
    }
}
