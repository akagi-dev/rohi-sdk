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
///////////////////////////////////////////////////////////////////////////////

use core::fmt;

use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;

use crate::{IdentityError, IdentitySigner, PublicKey, Secret, Signer};

/// Size of the Ed25519 secret key (RFC 8032 seed), in bytes.
pub const SECRET_LEN: usize = 32;

/// The device identity (configuration section `Identity`).
///
/// Holds only the derived Ed25519 secret; the public key is derived from it.
/// It is written exclusively by identity provisioning and removed only by an
/// explicit identity erase, never by ordinary configuration changes.
///
/// # Storage
///
/// The identity lives in the normal configuration store
/// ([`rohi-config`](../rohi_config/index.html)), so it inherits A/B
/// transactional writes, generation tracking, CRC validation and power-loss
/// recovery. There is no separate identity storage or partition.
///
/// * Section type `Identity`, ID `0x94d63573d312fe9b` (FNV-1a of the struct
///   name), version 1.
/// * CBOR: `[bytes(32)]` (`#[cbor(array)]`), pinned by the golden byte test
///   `cbor_encoding_is_stable`. The section ID and this encoding are persistent
///   ABI; see the [persistent ABI reference](https://rohi.akagi.dev/reference/persistent-abi.html).
/// * The identity is **not** a config section of the generic configuration
///   API: `identity` is a service whose secret is a write-only action argument
///   ([`IdentityService`](crate::IdentityService)), so no config request can
///   read, patch or reset it.
/// * Ordinary configuration changes and removing another section never touch
///   it. Only the `erase` action removes it.
/// * A corrupted store is reported as unavailable, never as "not
///   provisioned".
#[derive(Clone, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct Identity {
    #[n(0)]
    secret: Secret<[u8; SECRET_LEN]>,
}

impl Identity {
    /// Identity from a received secret. Every 32-byte value is a valid
    /// Ed25519 secret key. The temporary copy of the secret made here is
    /// zeroized.
    ///
    /// # Errors
    ///
    /// [`IdentityError::InvalidSecret`] unless `secret` is exactly
    /// [`SECRET_LEN`] bytes.
    ///
    /// ```
    /// use rohi_identity::{Identity, IdentityError};
    ///
    /// assert!(Identity::from_secret(&[1; 32]).is_ok());
    /// assert!(matches!(
    ///     Identity::from_secret(&[1; 31]),
    ///     Err(IdentityError::InvalidSecret)
    /// ));
    /// ```
    pub fn from_secret(secret: &[u8]) -> Result<Self, IdentityError> {
        let mut bytes: [u8; SECRET_LEN] = secret
            .try_into()
            .map_err(|_| IdentityError::InvalidSecret)?;
        let identity = Self {
            secret: Secret::new(bytes),
        };
        zeroize::Zeroize::zeroize(&mut bytes);
        Ok(identity)
    }

    /// Identity owning `secret`, already of the right size.
    pub const fn new(secret: Secret<[u8; SECRET_LEN]>) -> Self {
        Self { secret }
    }

    /// The Ed25519 public key, derived from the secret on every call (the
    /// public key is never persisted).
    pub fn public_key(&self) -> PublicKey {
        self.signer().public_key()
    }

    /// A signer backed by this identity.
    pub fn signer(&self) -> IdentitySigner {
        IdentitySigner::new(&self.secret)
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}
