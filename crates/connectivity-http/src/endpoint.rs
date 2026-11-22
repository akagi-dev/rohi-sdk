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
//! The `http://host[:port]` endpoint grammar and the policy of the `http` scheme.

use rohi_connectivity::{EndpointError, EndpointPolicy};

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
    /// use rohi_connectivity_http::Endpoint;
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

/// The policy of the `http` scheme: `http://host[:port]` and nothing else.
///
/// Two endpoints are the same path when the host matches ignoring ASCII case
/// and the port matches, with the default port 80 when none is written.
///
/// ```
/// use rohi_connectivity::{EndpointError, EndpointPolicy};
/// use rohi_connectivity_http::HttpPolicy;
///
/// assert!(HttpPolicy.validate("http://example.org:8080").is_ok());
/// assert_eq!(HttpPolicy.validate("http://example.org/x"), Err(EndpointError::Invalid));
/// assert_eq!(HttpPolicy.validate("https://example.org"), Err(EndpointError::UnsupportedScheme));
/// assert!(HttpPolicy.equivalent("http://example.org", "http://EXAMPLE.ORG:80"));
/// assert!(!HttpPolicy.equivalent("http://example.org:80", "http://example.org:8080"));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HttpPolicy;

impl EndpointPolicy for HttpPolicy {
    fn supports(&self, scheme: &str) -> bool {
        scheme == "http"
    }

    fn validate(&self, uri: &str) -> Result<(), EndpointError> {
        if !self.supports(rohi_connectivity::scheme_of(uri)) {
            return Err(EndpointError::UnsupportedScheme);
        }
        Endpoint::parse(uri).map(drop).ok_or(EndpointError::Invalid)
    }

    fn equivalent(&self, a: &str, b: &str) -> bool {
        match (Endpoint::parse(a), Endpoint::parse(b)) {
            (Some(a), Some(b)) => a.host.eq_ignore_ascii_case(b.host) && a.port == b.port,
            _ => a == b,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_follows_the_grammar() {
        for bad in [
            "http://",
            "http://h:",
            "http://h:0",
            "http://h/x",
            "http://u@h",
            "http://h?q",
        ] {
            assert_eq!(
                HttpPolicy.validate(bad),
                Err(EndpointError::Invalid),
                "{bad}"
            );
        }
        assert_eq!(
            HttpPolicy.validate("meshtastic+uart://!deadbeef"),
            Err(EndpointError::UnsupportedScheme)
        );
    }
}
