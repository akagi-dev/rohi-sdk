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
//! Persistent telemetry configuration.

use core::fmt;

use heapless::String;
use minicbor::{CborLen, Decode, Encode};
use rohi_config::{Text, TextValue};
use rohi_service::Config;

use crate::managed::validate_section;

/// Maximum length of the endpoint URI, in bytes: the longest text a config
/// value can hold.
pub const MAX_ENDPOINT_LEN: usize = 64;
/// Shortest allowed telemetry interval, in seconds.
pub const MIN_INTERVAL_S: u32 = 10;
/// Longest allowed telemetry interval, in seconds.
pub const MAX_INTERVAL_S: u32 = 24 * 60 * 60;
/// Default telemetry interval, in seconds.
pub const DEFAULT_INTERVAL_S: u32 = 60;
/// Default HTTP port.
pub const DEFAULT_PORT: u16 = 80;

const SCHEME: &str = "http://";

/// Invalid telemetry configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// The endpoint is not a valid `http://host[:port]` URI.
    InvalidEndpoint,
    /// The interval is outside `MIN_INTERVAL_S..=MAX_INTERVAL_S`.
    InvalidInterval,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidEndpoint => "invalid telemetry endpoint",
            Self::InvalidInterval => "invalid telemetry interval",
        })
    }
}

impl core::error::Error for ConfigError {}

/// Where telemetry is sent, split into its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint<'a> {
    /// Host name or IPv4 address, also the HTTP `Host` header.
    pub host: &'a str,
    /// TCP port.
    pub port: u16,
}

impl<'a> Endpoint<'a> {
    /// Splits `http://host[:port]`; `None` for anything else.
    ///
    /// The scheme is required and the port defaults to [`DEFAULT_PORT`]. The
    /// host is letters, digits, `.` and `-` only (a DNS name or an IPv4
    /// address): it is written into the HTTP `Host` header, so it can never
    /// carry spaces or line breaks. There is no user information, path, query
    /// or fragment: the path of the ingress is fixed by the protocol.
    pub fn parse(uri: &'a str) -> Option<Self> {
        let authority = uri.strip_prefix(SCHEME)?;
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        let host_is_valid = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
        if !host_is_valid {
            return None;
        }
        let port = match port {
            None => DEFAULT_PORT,
            Some(digits) => {
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                match digits.parse::<u16>() {
                    Ok(port) if port != 0 => port,
                    _ => return None,
                }
            }
        };
        Some(Self { host, port })
    }
}

/// The URI of the telemetry ingress: `http://host[:port]`, at most
/// [`MAX_ENDPOINT_LEN`] bytes.
///
/// See [`Endpoint::parse`] for the accepted form. A value that is not valid is
/// refused by [`new`](Self::new); one that was decoded from storage is checked
/// by [`ConnectivityConfig::validate`].
#[derive(Clone, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct EndpointUri(
    #[n(0)]
    #[cbor(
        encode_with = "rohi_config::cbor::text::encode",
        decode_with = "rohi_config::cbor::text::decode",
        cbor_len = "rohi_config::cbor::text::cbor_len"
    )]
    String<MAX_ENDPOINT_LEN>,
);

impl EndpointUri {
    /// `None` if `uri` is not a valid endpoint or too long.
    pub fn new(uri: &str) -> Option<Self> {
        Endpoint::parse(uri)?;
        String::try_from(uri).ok().map(Self)
    }

    /// The URI as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The parts of the endpoint; `None` if the stored text is not valid.
    pub fn endpoint(&self) -> Option<Endpoint<'_>> {
        Endpoint::parse(&self.0)
    }
}

impl TextValue for EndpointUri {
    fn from_text(text: &str) -> Option<Self> {
        Self::new(text)
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.as_str())
    }
}

impl fmt::Debug for EndpointUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Effective telemetry configuration (section `connectivity`).
///
/// Telemetry is sent only while an [`endpoint`](Self::endpoint) is set: without
/// one the service is disabled, and that is a valid state.
/// `ConnectivityConfig::default()` is [`disabled`](Self::disabled). What is
/// stored are the explicit user overrides of it, [`ConnectivityOverrides`].
#[derive(Clone, PartialEq, Eq, Debug, Config)]
#[config(
    name = "connectivity",
    overrides = ConnectivityOverrides,
    version = 1,
    validate = validate_section
)]
pub struct ConnectivityConfig {
    /// Robonomics CPS node identifier (`core.v1.Meta.node_id`).
    #[config(id = 2)]
    pub node_id: u64,
    /// Seconds between telemetry frames.
    #[config(id = 3, unit = S, min = MIN_INTERVAL_S, max = MAX_INTERVAL_S)]
    pub interval_s: u32,
    /// Ingress URI, `http://host[:port]`. `None` (the default) disables
    /// telemetry.
    #[config(id = 4, with = Text)]
    pub endpoint: Option<EndpointUri>,
}

impl ConnectivityConfig {
    /// No endpoint, so telemetry is disabled, with the default interval.
    pub const fn disabled() -> Self {
        Self {
            node_id: 0,
            interval_s: DEFAULT_INTERVAL_S,
            endpoint: None,
        }
    }

    /// Whether telemetry is sent: an endpoint is set.
    pub const fn is_enabled(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Checks the configuration before it is persisted or applied. A disabled
    /// configuration is valid.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(uri) = &self.endpoint
            && uri.endpoint().is_none()
        {
            return Err(ConfigError::InvalidEndpoint);
        }
        if !(MIN_INTERVAL_S..=MAX_INTERVAL_S).contains(&self.interval_s) {
            return Err(ConfigError::InvalidInterval);
        }
        Ok(())
    }

    /// The ingress endpoint, `None` while telemetry is disabled or the stored
    /// URI is not valid.
    pub fn endpoint(&self) -> Option<Endpoint<'_>> {
        self.endpoint.as_ref()?.endpoint()
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

    fn uri(text: &str) -> EndpointUri {
        EndpointUri::new(text).unwrap()
    }

    fn config() -> ConnectivityConfig {
        ConnectivityConfig {
            node_id: 5,
            interval_s: 60,
            endpoint: Some(uri("http://a.io:8080")),
        }
    }

    fn overrides() -> ConnectivityOverrides {
        ConnectivityOverrides {
            node_id: Override::Value(5),
            endpoint: Override::Value(Some(uri("http://a.io:8080"))),
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
                0xa2, // map(2)
                0x02, 0x05, // 2: node_id
                0x04, 0x70, // 4: endpoint, text(16)
                b'h', b't', b't', b'p', b':', b'/', b'/', b'a', b'.', b'i', b'o', b':', b'8', b'0',
                b'8', b'0',
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
        assert_eq!(
            effective,
            ConnectivityConfig {
                interval_s: DEFAULT_INTERVAL_S,
                ..config()
            }
        );
    }

    #[test]
    fn round_trips_and_fits_a_section() {
        let mut buffer = [0u8; MAX_SECTION_PAYLOAD];
        let longest = std::format!("http://{}", "a".repeat(MAX_ENDPOINT_LEN - SCHEME.len()));
        let overrides = ConnectivityOverrides {
            node_id: Override::Value(u64::MAX),
            interval_s: Override::Value(u32::MAX),
            endpoint: Override::Value(Some(uri(&longest))),
        };
        let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
        let decoded: ConnectivityOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, overrides);
    }

    #[test]
    fn no_endpoint_is_a_valid_disabled_configuration() {
        let disabled = ConnectivityConfig::disabled();
        assert!(!disabled.is_enabled());
        assert_eq!(disabled.endpoint(), None);
        assert_eq!(disabled.validate(), Ok(()));
        assert!(config().is_enabled());
    }

    #[test]
    fn validation() {
        assert_eq!(config().validate(), Ok(()));
        let mut c = config();
        c.interval_s = MIN_INTERVAL_S - 1;
        assert_eq!(c.validate(), Err(ConfigError::InvalidInterval));
        c.interval_s = MAX_INTERVAL_S + 1;
        assert_eq!(c.validate(), Err(ConfigError::InvalidInterval));
        // Stored text that is no endpoint, for example a host of an older
        // firmware, is invalid rather than guessed.
        let stored = ConnectivityConfig {
            endpoint: Some(EndpointUri(
                String::try_from("ingress.example.org").unwrap(),
            )),
            ..config()
        };
        assert_eq!(stored.validate(), Err(ConfigError::InvalidEndpoint));
        assert_eq!(stored.endpoint(), None);
    }

    #[test]
    fn the_endpoint_is_an_http_uri_with_an_optional_port() {
        let parsed = |text| Endpoint::parse(text);
        assert_eq!(
            parsed("http://a.io"),
            Some(Endpoint {
                host: "a.io",
                port: 80
            })
        );
        assert_eq!(
            parsed("http://192.168.1.5:8080"),
            Some(Endpoint {
                host: "192.168.1.5",
                port: 8080
            })
        );
        assert_eq!(
            parsed("http://ingress-1.example.org:65535"),
            Some(Endpoint {
                host: "ingress-1.example.org",
                port: 65535
            })
        );
        for refused in [
            "",
            "a.io",
            "a.io:80",
            "https://a.io",
            "HTTP://a.io",
            "http://",
            "http://:80",
            "http://a.io:",
            "http://a.io:0",
            "http://a.io:65536",
            "http://a.io:+80",
            "http://a.io:8x",
            "http://a.io/",
            "http://a.io/v1/telemetry",
            "http://a.io?x=1",
            "http://a.io#frag",
            "http://user@a.io",
            "http://a b",
            "http://a.io:80:81",
            "http://[::1]:80",
        ] {
            assert_eq!(parsed(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn a_uri_cannot_inject_header_bytes_or_overflow() {
        assert!(EndpointUri::new("http://ok.example").is_some());
        assert!(EndpointUri::new("http://a.io\r\nX: 1").is_none());
        assert!(EndpointUri::new("http://a b").is_none());
        let too_long = std::format!("http://{}", "a".repeat(MAX_ENDPOINT_LEN));
        assert!(EndpointUri::new(&too_long).is_none());
    }

    #[test]
    fn the_endpoint_borrows_the_config() {
        let config = config();
        assert_eq!(
            config.endpoint(),
            Some(Endpoint {
                host: "a.io",
                port: 8080
            })
        );
        assert_eq!(config.interval_ms(), 60_000);
    }
}
