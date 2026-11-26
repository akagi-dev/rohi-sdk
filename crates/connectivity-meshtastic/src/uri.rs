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
//! Meshtastic endpoints: `meshtastic+uart://!xxxxxxxx` and `meshtastic+ble://!xxxxxxxx`.
//!
//! The scheme names the **local link** the firmware uses to reach its radio;
//! the node ID is the **mesh destination** (the gateway), not the identity of
//! the local radio. Nothing else is part of the URI: no port, path, query,
//! fragment or user information, and in particular no `device` or `radio`
//! parameter. Which UART or BLE peer is used is chosen by the firmware when it
//! builds the transport.

use rohi_connectivity::{EndpointError, EndpointPolicy};

/// The scheme of a UART-connected local radio.
pub const SCHEME_UART: &str = "meshtastic+uart";
/// The scheme of a BLE-connected local radio.
pub const SCHEME_BLE: &str = "meshtastic+ble";

/// How the firmware reaches its local radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// A serial line.
    Uart,
    /// A BLE central connection to the radio's GATT service.
    Ble,
}

impl Link {
    /// The URI scheme of the link.
    pub const fn scheme(self) -> &'static str {
        match self {
            Self::Uart => SCHEME_UART,
            Self::Ble => SCHEME_BLE,
        }
    }
}

pub use rohi_meshtastic::NodeId;

/// The parts of a Meshtastic endpoint.
///
/// ```
/// use rohi_connectivity_meshtastic::{Endpoint, Link, NodeId};
///
/// let endpoint = Endpoint::parse("meshtastic+uart://!DEADbeef").unwrap();
/// assert_eq!((endpoint.link, endpoint.gateway), (Link::Uart, NodeId(0xdead_beef)));
/// // Exactly this form: no query, port, path or other node notation.
/// assert_eq!(Endpoint::parse("meshtastic+uart://!deadbeef?device=0"), None);
/// assert_eq!(Endpoint::parse("meshtastic+ble://!deadbeef:1"), None);
/// assert_eq!(Endpoint::parse("meshtastic+ble://deadbeef"), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    /// The local link.
    pub link: Link,
    /// The mesh destination.
    pub gateway: NodeId,
}

impl Endpoint {
    /// Splits the URI; `None` unless it is exactly `meshtastic+uart://!xxxxxxxx` or
    /// `meshtastic+ble://!xxxxxxxx`.
    pub fn parse(uri: &str) -> Option<Self> {
        let (scheme, rest) = uri.split_once("://")?;
        let link = match scheme {
            SCHEME_UART => Link::Uart,
            SCHEME_BLE => Link::Ble,
            _ => return None,
        };
        let gateway = NodeId::parse(rest)?;
        if gateway == NodeId::BROADCAST {
            return None;
        }
        Some(Self { link, gateway })
    }
}

/// The policy of the Meshtastic schemes a build supports.
///
/// A scheme whose link has no backend in this build is *unsupported*, never
/// silently served by the other link.
///
/// ```
/// use rohi_connectivity::{EndpointError, EndpointPolicy};
/// use rohi_connectivity_meshtastic::MeshtasticPolicy;
///
/// let uart_only = MeshtasticPolicy::UART;
/// assert!(uart_only.validate("meshtastic+uart://!deadbeef").is_ok());
/// assert_eq!(
///     uart_only.validate("meshtastic+ble://!deadbeef"),
///     Err(EndpointError::UnsupportedScheme)
/// );
/// assert_eq!(
///     uart_only.validate("meshtastic+uart://!deadbeef?device=1"),
///     Err(EndpointError::Invalid)
/// );
/// // Hex case does not matter, the link does.
/// assert!(uart_only.equivalent("meshtastic+uart://!DEADBEEF", "meshtastic+uart://!deadbeef"));
/// assert!(!MeshtasticPolicy::BOTH.equivalent(
///     "meshtastic+uart://!deadbeef",
///     "meshtastic+ble://!deadbeef"
/// ));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshtasticPolicy {
    uart: bool,
    ble: bool,
}

impl MeshtasticPolicy {
    /// Only the UART link.
    pub const UART: Self = Self {
        uart: true,
        ble: false,
    };
    /// Only the BLE link.
    pub const BLE: Self = Self {
        uart: false,
        ble: true,
    };
    /// Both links.
    pub const BOTH: Self = Self {
        uart: true,
        ble: true,
    };

    const fn enabled(self, link: Link) -> bool {
        match link {
            Link::Uart => self.uart,
            Link::Ble => self.ble,
        }
    }
}

impl EndpointPolicy for MeshtasticPolicy {
    fn supports(&self, scheme: &str) -> bool {
        (scheme == SCHEME_UART && self.uart) || (scheme == SCHEME_BLE && self.ble)
    }

    fn validate(&self, uri: &str) -> Result<(), EndpointError> {
        let scheme = rohi_connectivity::scheme_of(uri);
        if !self.supports(scheme) {
            return Err(EndpointError::UnsupportedScheme);
        }
        match Endpoint::parse(uri) {
            Some(endpoint) if self.enabled(endpoint.link) => Ok(()),
            _ => Err(EndpointError::Invalid),
        }
    }

    fn equivalent(&self, a: &str, b: &str) -> bool {
        match (Endpoint::parse(a), Endpoint::parse(b)) {
            (Some(a), Some(b)) => a == b,
            _ => a == b,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_forms_are_accepted() {
        let policy = MeshtasticPolicy::BOTH;
        for ok in [
            "meshtastic+uart://!deadbeef",
            "meshtastic+ble://!00000000",
            "meshtastic+uart://!DEADBEEF",
            "meshtastic+ble://!DeadBeef",
        ] {
            assert_eq!(policy.validate(ok), Ok(()), "{ok}");
        }
        for bad in [
            "meshtastic+uart://",
            "meshtastic+uart://!",
            "meshtastic+uart://!deadbee",
            "meshtastic+uart://!deadbeef0",
            "meshtastic+uart://!deadbeeg",
            "meshtastic+uart://deadbeef",
            "meshtastic+uart://!deadbeef?device=0",
            "meshtastic+uart://!deadbeef?radio=a",
            "meshtastic+uart://!deadbeef?x",
            "meshtastic+uart://!deadbeef/path",
            "meshtastic+uart://!deadbeef#f",
            "meshtastic+uart://u@!deadbeef",
            "meshtastic+uart://!deadbeef:80",
            "meshtastic+uart://0xdeadbeef",
        ] {
            assert_eq!(policy.validate(bad), Err(EndpointError::Invalid), "{bad}");
        }
        for other in [
            "meshtastic://!deadbeef",
            "meshtastic+lora://!deadbeef",
            "http://a",
        ] {
            assert_eq!(
                policy.validate(other),
                Err(EndpointError::UnsupportedScheme),
                "{other}"
            );
        }
    }

    #[test]
    fn a_link_without_a_backend_is_unsupported_and_never_replaced() {
        assert_eq!(
            MeshtasticPolicy::UART.validate("meshtastic+ble://!deadbeef"),
            Err(EndpointError::UnsupportedScheme)
        );
        assert_eq!(
            MeshtasticPolicy::BLE.validate("meshtastic+uart://!deadbeef"),
            Err(EndpointError::UnsupportedScheme)
        );
        assert!(!MeshtasticPolicy::UART.supports("meshtastic+ble"));
    }

    #[test]
    fn equivalence_ignores_hex_case_but_not_the_link_or_the_node() {
        let policy = MeshtasticPolicy::BOTH;
        assert!(policy.equivalent("meshtastic+ble://!ABCDEF01", "meshtastic+ble://!abcdef01"));
        assert!(!policy.equivalent("meshtastic+ble://!abcdef01", "meshtastic+uart://!abcdef01"));
        assert!(!policy.equivalent("meshtastic+ble://!abcdef01", "meshtastic+ble://!abcdef02"));
    }
}
