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
//! Delivery endpoints: an opaque, bounded URI and the policy that gives it meaning.
//!
//! The core never parses a host, a port or a node ID. [`EndpointUri`] only
//! checks the *shape* (`scheme://rest`, bounded, printable ASCII) so that it can
//! be stored, compared and logged safely. What the URI means is decided by the
//! transport crates through an [`EndpointPolicy`], which the firmware composes
//! (see [`Mux`](crate::Mux)) and which the configuration service consults
//! **before** a change is stored. No policy does network, radio or serial I/O.
//!
//! ```
//! use rohi_connectivity::{EndpointPolicy, EndpointUri, StructuralPolicy};
//!
//! let uri = EndpointUri::new("http://ingress.example.org:8080").unwrap();
//! assert_eq!(uri.scheme(), "http");
//! // No scheme, whitespace or control characters.
//! assert!(EndpointUri::new("ingress.example.org").is_none());
//! assert!(EndpointUri::new("http://a b").is_none());
//! // The structural policy accepts any well-formed URI and compares bytes.
//! assert!(StructuralPolicy.validate(uri.as_str()).is_ok());
//! ```

use core::fmt;

use heapless::String;
use minicbor::{CborLen, Decode, Encode};

/// Maximum length of the endpoint URI, in bytes: the longest text a config
/// value can hold.
pub const MAX_ENDPOINT_LEN: usize = 64;

/// Why an endpoint URI was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    /// Not a valid URI of its scheme.
    Invalid,
    /// This build has no backend for the scheme.
    UnsupportedScheme,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid endpoint"),
            Self::UnsupportedScheme => f.write_str("unsupported endpoint scheme"),
        }
    }
}

impl core::error::Error for EndpointError {}

/// The meaning of endpoint URIs in one build: which schemes it supports, what
/// is valid and which spellings denote the same delivery path.
///
/// Synchronous and free of I/O: it checks the text only. Whether the Wi-Fi,
/// the radio or the gateway is reachable is an operational failure of the
/// delivery attempt, never a reason to refuse a configuration.
pub trait EndpointPolicy {
    /// Whether this policy owns `scheme` (the text before `://`, as written).
    fn supports(&self, scheme: &str) -> bool;

    /// Checks that `uri` is a valid, supported endpoint.
    ///
    /// # Errors
    ///
    /// [`EndpointError::UnsupportedScheme`] when the scheme has no backend,
    /// [`EndpointError::Invalid`] when the rest is not valid for it.
    fn validate(&self, uri: &str) -> Result<(), EndpointError>;

    /// Whether `a` and `b` (both valid) are the same delivery path, for
    /// duplicate detection. The text is never rewritten to compare.
    fn equivalent(&self, a: &str, b: &str) -> bool;
}

impl<P: EndpointPolicy + ?Sized> EndpointPolicy for &P {
    fn supports(&self, scheme: &str) -> bool {
        (**self).supports(scheme)
    }
    fn validate(&self, uri: &str) -> Result<(), EndpointError> {
        (**self).validate(uri)
    }
    fn equivalent(&self, a: &str, b: &str) -> bool {
        (**self).equivalent(a, b)
    }
}

/// A policy that accepts every well-formed URI and compares them byte for byte.
///
/// For tests and for builds that only check the shape; a firmware that
/// delivers anything composes the policies of its transports instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StructuralPolicy;

impl EndpointPolicy for StructuralPolicy {
    fn supports(&self, _scheme: &str) -> bool {
        true
    }
    fn validate(&self, uri: &str) -> Result<(), EndpointError> {
        if is_well_formed(uri) {
            Ok(())
        } else {
            Err(EndpointError::Invalid)
        }
    }
    fn equivalent(&self, a: &str, b: &str) -> bool {
        a == b
    }
}

/// The text before `://`, or the empty string when there is none.
pub fn scheme_of(uri: &str) -> &str {
    uri.split_once("://").map_or("", |(scheme, _)| scheme)
}

// `scheme://rest`: an ASCII scheme (`[A-Za-z][A-Za-z0-9+.-]*`), a nonempty rest
// and nothing but printable ASCII, so no space, control or non-ASCII character.
fn is_well_formed(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return false;
    };
    let mut scheme_bytes = scheme.bytes();
    let scheme_ok = scheme_bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && scheme_bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'));
    scheme_ok
        && !rest.is_empty()
        && uri.len() <= MAX_ENDPOINT_LEN
        && uri.bytes().all(|b| b.is_ascii_graphic())
}

/// The URI of a delivery endpoint, at most [`MAX_ENDPOINT_LEN`] bytes.
///
/// Opaque: only the shape is checked (see the [module](self) page); the value
/// is stored as the CBOR text it always was, so a stored HTTP route reads
/// unchanged. A value that is malformed is refused by [`new`](Self::new); one
/// that was decoded from storage is checked by
/// [`Routes::validate`](crate::Routes::validate).
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
    /// `None` if `uri` is not `scheme://rest` of printable ASCII or too long.
    pub fn new(uri: &str) -> Option<Self> {
        if !is_well_formed(uri) {
            return None;
        }
        String::try_from(uri).ok().map(Self)
    }

    /// The URI as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The scheme: the text before `://`.
    pub fn scheme(&self) -> &str {
        scheme_of(&self.0)
    }

    /// Whether the stored text has the shape of an endpoint; `false` only for
    /// a value decoded from damaged storage.
    pub fn is_well_formed(&self) -> bool {
        is_well_formed(&self.0)
    }
}

impl fmt::Debug for EndpointUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl rohi_config::TextValue for EndpointUri {
    fn from_text(text: &str) -> Option<Self> {
        let uri = Self::new(text);
        if uri.is_none() {
            log_invalid_endpoint(text.len());
        }
        uri
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.as_str())
    }
}

// Only the length: a rejected value may carry credentials.
#[cold]
#[inline(never)]
fn log_invalid_endpoint(len: usize) {
    log::warn!(
        target: "connectivity",
        "Endpoint rejected, expected scheme://... (len_b={})",
        len
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_is_checked_and_bounded() {
        for ok in ["http://a", "meshtastic+uart://!deadbeef", "x1.y-z+w://q"] {
            assert!(EndpointUri::new(ok).is_some(), "{ok}");
        }
        for bad in [
            "",
            "http://",
            "://a",
            "1http://a",
            "http:/a",
            "http://a b",
            "http://a\n",
            "http://é",
            "ht tp://a",
        ] {
            assert!(EndpointUri::new(bad).is_none(), "{bad:?}");
        }
        let at_limit = std::format!("http://{}", "a".repeat(MAX_ENDPOINT_LEN - 7));
        assert!(EndpointUri::new(&at_limit).is_some());
        assert!(EndpointUri::new(&std::format!("{at_limit}a")).is_none());
    }

    #[test]
    fn scheme_is_the_text_before_the_separator() {
        assert_eq!(scheme_of("meshtastic+ble://!1"), "meshtastic+ble");
        assert_eq!(scheme_of("nothing"), "");
    }

    #[test]
    fn cbor_is_a_plain_text_string() {
        let uri = EndpointUri::new("http://ingress.example").unwrap();
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(&uri, &mut buffer).unwrap();
        // A definite-length text string: major type 3.
        assert_eq!(buffer[0] >> 5, 3);
        assert_eq!(&buffer[1..used], b"http://ingress.example");
    }
}
