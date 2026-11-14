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

use core::fmt;

use ed25519_zebra::VerificationKey;

use crate::IdentityError;

/// Size of an Ed25519 public key, in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;

/// Size of an Ed25519 signature, in bytes.
pub const SIGNATURE_LEN: usize = 64;

fn hex(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    f.write_str("0x")?;
    for byte in bytes {
        write!(f, "{byte:02x}")?;
    }
    Ok(())
}

/// Ed25519 public key: the canonical public identity of a device.
///
/// Displayed as `0x` followed by 64 lowercase hex digits. Public keys are not
/// secret.
///
/// ```
/// use rohi_identity::{PUBLIC_KEY_LEN, PublicKey};
///
/// let key = PublicKey::from_bytes([0xab; PUBLIC_KEY_LEN]);
/// assert_eq!(key.to_string(), format!("0x{}", "ab".repeat(32)));
/// assert_eq!(PublicKey::from_slice(&[0; 5]), None); // wrong length
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    /// Wraps raw key bytes. They are not validated, [`verify`](Self::verify)
    /// rejects keys that are not curve points.
    pub const fn from_bytes(bytes: [u8; PUBLIC_KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// `None` unless `bytes` is exactly [`PUBLIC_KEY_LEN`] long.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }

    /// The raw key.
    pub const fn to_bytes(self) -> [u8; PUBLIC_KEY_LEN] {
        self.0
    }

    /// The raw key.
    pub const fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.0
    }

    /// Checks `signature` over exactly `message`, with the `ed25519-zebra`
    /// (ZIP 215) rules.
    ///
    /// # Errors
    ///
    /// [`IdentityError::InvalidPublicKey`] if the key bytes are not a curve
    /// point, [`IdentityError::InvalidSignature`] if the signature does not
    /// verify.
    ///
    /// ```
    /// use rohi_identity::{Identity, IdentityError, Signer};
    ///
    /// let identity = Identity::from_secret(&[1; 32]).unwrap();
    /// let signature = identity.signer().sign(b"data").unwrap();
    /// let key = identity.public_key();
    /// assert_eq!(key.verify(b"data", &signature), Ok(()));
    /// assert_eq!(key.verify(b"tampered", &signature), Err(IdentityError::InvalidSignature));
    /// ```
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<(), IdentityError> {
        let key = VerificationKey::try_from(self.0).map_err(|_| {
            log::warn!(target: "identity", "Invalid public key (key={})", self);
            IdentityError::InvalidPublicKey
        })?;
        key.verify(&ed25519_zebra::Signature::from_bytes(&signature.0), message)
            .map_err(|_| {
                log::warn!(target: "identity", "Signature verification failed (key={})", self);
                IdentityError::InvalidSignature
            })
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        hex(f, &self.0)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublicKey(")?;
        hex(f, &self.0)?;
        f.write_str(")")
    }
}

/// Ed25519 signature: 64 raw bytes, produced by a [`Signer`](crate::Signer) and
/// checked with [`PublicKey::verify`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature([u8; SIGNATURE_LEN]);

impl Signature {
    /// Wraps raw signature bytes.
    pub const fn from_bytes(bytes: [u8; SIGNATURE_LEN]) -> Self {
        Self(bytes)
    }

    /// `None` unless `bytes` is exactly [`SIGNATURE_LEN`] long.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }

    /// The raw signature.
    pub const fn to_bytes(self) -> [u8; SIGNATURE_LEN] {
        self.0
    }

    /// The raw signature.
    pub const fn as_bytes(&self) -> &[u8; SIGNATURE_LEN] {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Signature(")?;
        hex(f, &self.0)?;
        f.write_str(")")
    }
}
