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
//! A policy for the tests of the core: `http://host[:port]` without a transport.

use crate::endpoint::{EndpointError, EndpointPolicy, scheme_of};

/// Accepts `http://host[:port]` (host letters, digits, `.`, `-`) and treats
/// the host case-insensitively with the default port 80.
#[derive(Clone, Copy, Debug, Default)]
pub struct TestPolicy;

fn parts(uri: &str) -> Option<(&str, u16)> {
    let rest = uri.strip_prefix("http://")?;
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok().filter(|p| *p != 0)?),
        None => (rest, 80),
    };
    let ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
    ok.then_some((host, port))
}

impl EndpointPolicy for TestPolicy {
    fn supports(&self, scheme: &str) -> bool {
        scheme == "http"
    }

    fn validate(&self, uri: &str) -> Result<(), EndpointError> {
        if scheme_of(uri) != "http" {
            return Err(EndpointError::UnsupportedScheme);
        }
        parts(uri).map(drop).ok_or(EndpointError::Invalid)
    }

    fn equivalent(&self, a: &str, b: &str) -> bool {
        match (parts(a), parts(b)) {
            (Some((ha, pa)), Some((hb, pb))) => ha.eq_ignore_ascii_case(hb) && pa == pb,
            _ => a == b,
        }
    }
}

use crate::transport::{AckKind, Disposition, TransportAck, TransportError};

/// The acknowledgement of an ingress.
pub const ACCEPTED: TransportAck = TransportAck {
    kind: AckKind::Accepted,
    detail: 202,
};

/// An HTTP-like status as the delivery result, classified like the HTTP transport does.
pub fn status(code: u16) -> Result<TransportAck, TransportError> {
    let disposition = match code {
        202 => return Ok(ACCEPTED),
        401 | 403 => Disposition::Unauthorized,
        400..=499 if code != 408 && code != 429 => Disposition::Permanent,
        _ => Disposition::Transient,
    };
    Err(TransportError::Rejected { code, disposition })
}
