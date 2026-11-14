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
//! Delivery endpoints: `http://host[:port]`.

use core::fmt;

use heapless::String;
use minicbor::{CborLen, Decode, Encode};

/// Maximum length of the endpoint URI, in bytes: the longest text a config
/// value can hold.
pub const MAX_ENDPOINT_LEN: usize = 64;
/// Default HTTP port.
pub const DEFAULT_PORT: u16 = 80;

const SCHEME: &str = "http://";

/// Where packets are sent, split into its parts.
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
    /// ```
    /// use rohi_connectivity::Endpoint;
    ///
    /// let endpoint = Endpoint::parse("http://ingress.example.org:8080").unwrap();
    /// assert_eq!((endpoint.host, endpoint.port), ("ingress.example.org", 8080));
    /// assert_eq!(Endpoint::parse("http://10.0.0.7").unwrap().port, 80);
    /// // No TLS, path, user information or empty port.
    /// assert_eq!(Endpoint::parse("https://ingress.example.org"), None);
    /// assert_eq!(Endpoint::parse("http://ingress.example.org/x"), None);
    /// assert_eq!(Endpoint::parse("http://ingress.example.org:"), None);
    /// ```
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
/// by [`Routes::validate`](crate::Routes::validate).
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

impl fmt::Debug for EndpointUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}
