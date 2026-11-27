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
use embassy_futures::block_on;

use super::*;
use crate::testing::{ACCEPTED, TestPolicy};

/// A transport of one scheme that records where it was called.
struct Named(&'static str, &'static str);

impl PacketTransport for Named {
    fn handles(&self, scheme: &str) -> bool {
        scheme == self.0
    }

    async fn deliver(
        &mut self,
        _uri: &str,
        _packet: &[u8],
    ) -> Result<TransportAck, TransportError> {
        Ok(TransportAck {
            kind: ACCEPTED.kind,
            detail: self.1.len() as u16,
        })
    }
}

#[test]
fn the_transport_is_chosen_by_the_whole_scheme() {
    let mut mux = Mux(Named("a+x", "x"), Named("a+yy", "yy"));
    assert_eq!(block_on(mux.deliver("a+x://1", b"")).unwrap().detail, 1);
    assert_eq!(block_on(mux.deliver("a+yy://1", b"")).unwrap().detail, 2);
    // A prefix of a scheme is another scheme.
    assert_eq!(
        block_on(mux.deliver("a://1", b"")),
        Err(TransportError::Unsupported)
    );
    assert_eq!(
        block_on(mux.deliver("nothing", b"")),
        Err(TransportError::Unsupported)
    );
}

#[test]
fn the_policy_dispatches_and_refuses_unknown_schemes() {
    let policy = Mux(TestPolicy, Disabled);
    assert!(policy.validate("http://a.example").is_ok());
    assert_eq!(policy.validate("http://"), Err(EndpointError::Invalid));
    assert_eq!(
        policy.validate("ftp://a"),
        Err(EndpointError::UnsupportedScheme)
    );
    assert!(policy.equivalent("http://A.example", "http://a.example:80"));
    // Different schemes never name the same path.
    assert!(!policy.equivalent("http://a", "ftp://a"));
    assert!(!Mux(Disabled, Disabled).supports("http"));
}
