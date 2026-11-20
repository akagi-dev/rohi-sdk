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
//! The pre-shared key of the TCP endpoint: its private persisted section and its store.

use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// Size of the pre-shared key, in bytes.
pub const PSK_LEN: usize = 32;

/// Size of the key fingerprint reported in the status, in bytes.
const FINGERPRINT_LEN: usize = 8;

/// The persisted pre-shared key of the TCP endpoint (configuration section
/// `TcpPskSection`).
///
/// It lives in the normal configuration store, so it has the A/B transactional writes of
/// every section, and it is written only by `control_tcp.reveal_psk` and removed by a factory reset.
/// It is not a managed config section: no config
/// request reads, patches or resets it, and no status or log shows it. The memory it occupies
/// is wiped when the value is dropped. CBOR: `[bytes(32)]`.
#[derive(Clone, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct TcpPskSection {
    #[n(0)]
    #[cbor(with = "minicbor::bytes")]
    key: [u8; PSK_LEN],
}

impl Drop for TcpPskSection {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl TcpPskSection {
    /// The section for `key`.
    pub const fn new(key: [u8; PSK_LEN]) -> Self {
        Self { key }
    }

    /// The stored key; handle it like a secret: copy it only where it is used and wipe the copy.
    pub const fn key(&self) -> &[u8; PSK_LEN] {
        &self.key
    }
}

/// Why the key store failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PskStoreError {
    /// The configuration storage is not usable.
    Unavailable,
    /// Anything else.
    Internal,
}

/// Where the pre-shared key is kept. The firmware implements it over its configuration owner,
/// which publishes the committed key to the TCP endpoint and records in the
/// [`ControlAccess`](rohi_control::ControlAccess) that there is one (and that it changed) in the same step as the commit.
#[allow(async_fn_in_trait)]
pub trait PskStore {
    /// Persists `key` as the one pre-shared key.
    async fn store(&self, key: &[u8; PSK_LEN]) -> Result<(), PskStoreError>;
}

/// The first bytes of a SHA-256 of `key` under a fixed domain: what the status shows instead of
/// the key, so that an operator can tell which key a device holds (it is in the result of `reveal_psk`, never in the status).
///
/// ```
/// use rohi_control_endpoints::fingerprint;
///
/// let first = fingerprint(&[1; 32]);
/// assert_eq!(first.len(), 8);
/// assert_ne!(first, fingerprint(&[2; 32]));
/// ```
pub fn fingerprint(key: &[u8; PSK_LEN]) -> [u8; FINGERPRINT_LEN] {
    let mut hash = Sha256::new();
    hash.update(b"rohi-control-tcp psk fingerprint v1");
    hash.update(key);
    let digest = hash.finalize();
    let mut out = [0u8; FINGERPRINT_LEN];
    out.copy_from_slice(&digest[..FINGERPRINT_LEN]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_section_encoding_is_stable() {
        let section = TcpPskSection::new([0xab; 32]);
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(&section, &mut buffer).unwrap();
        // array(1), bytes(32), key.
        assert_eq!(&buffer[..3], [0x81, 0x58, 0x20]);
        assert_eq!(used, 35);
        let back: TcpPskSection = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(back.key(), &[0xab; 32]);
    }
}
