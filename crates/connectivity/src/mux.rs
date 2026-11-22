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
//! Static composition of the enabled backends.
//!
//! The firmware picks the transports it supports and nests them in a
//! [`Mux`]; nothing is boxed or allocated and a backend that is not
//! composed costs no code. The same type composes the [`EndpointPolicy`]
//! of the backends for the configuration service, so a route is checked with
//! exactly the schemes the build can deliver.
//!
//! ```
//! use rohi_connectivity::{Disabled, EndpointPolicy, EndpointError, Mux, StructuralPolicy};
//!
//! // Two backends: the first one that supports the scheme owns the endpoint.
//! let policy = Mux(StructuralPolicy, Disabled);
//! assert!(policy.validate("any://thing").is_ok());
//!
//! let none = Mux(Disabled, Disabled);
//! assert_eq!(none.validate("http://a"), Err(EndpointError::UnsupportedScheme));
//! ```

use crate::endpoint::{EndpointError, EndpointPolicy, scheme_of};
use crate::transport::{PacketTransport, TransportAck, TransportError};

/// A backend that is not compiled in: it supports no scheme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Disabled;

impl EndpointPolicy for Disabled {
    fn supports(&self, _scheme: &str) -> bool {
        false
    }
    fn validate(&self, _uri: &str) -> Result<(), EndpointError> {
        Err(EndpointError::UnsupportedScheme)
    }
    fn equivalent(&self, a: &str, b: &str) -> bool {
        a == b
    }
}

impl PacketTransport for Disabled {
    fn handles(&self, _scheme: &str) -> bool {
        false
    }
    async fn deliver(
        &mut self,
        _endpoint_uri: &str,
        _packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        Err(TransportError::Unsupported)
    }
}

/// Two backends dispatched on the **whole scheme** of the endpoint, the first
/// one that supports it first. Nest it for more: `Mux(a, Mux(b, c))`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mux<A, B>(pub A, pub B);

impl<A: EndpointPolicy, B: EndpointPolicy> EndpointPolicy for Mux<A, B> {
    fn supports(&self, scheme: &str) -> bool {
        self.0.supports(scheme) || self.1.supports(scheme)
    }

    fn validate(&self, uri: &str) -> Result<(), EndpointError> {
        let scheme = scheme_of(uri);
        if self.0.supports(scheme) {
            self.0.validate(uri)
        } else if self.1.supports(scheme) {
            self.1.validate(uri)
        } else {
            Err(EndpointError::UnsupportedScheme)
        }
    }

    fn equivalent(&self, a: &str, b: &str) -> bool {
        // Different schemes are different delivery paths, even for the same
        // gateway.
        let scheme = scheme_of(a);
        if scheme != scheme_of(b) {
            false
        } else if self.0.supports(scheme) {
            self.0.equivalent(a, b)
        } else if self.1.supports(scheme) {
            self.1.equivalent(a, b)
        } else {
            a == b
        }
    }
}

impl<A: PacketTransport, B: PacketTransport> PacketTransport for Mux<A, B> {
    fn handles(&self, scheme: &str) -> bool {
        self.0.handles(scheme) || self.1.handles(scheme)
    }

    async fn deliver(
        &mut self,
        endpoint_uri: &str,
        packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        let scheme = scheme_of(endpoint_uri);
        if self.0.handles(scheme) {
            self.0.deliver(endpoint_uri, packet).await
        } else if self.1.handles(scheme) {
            self.1.deliver(endpoint_uri, packet).await
        } else {
            Err(TransportError::Unsupported)
        }
    }
}

#[cfg(test)]
mod tests;
