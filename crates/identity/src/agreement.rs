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
//! Key agreement with the device identity.

use curve25519_dalek::edwards::CompressedEdwardsY;
use sha2::{Digest, Sha512};
use zeroize::Zeroize;

use crate::{IdentityError, IdentitySigner, PublicKey};

/// An X25519 shared secret, zeroized on drop and never printed.
pub struct SharedSecret([u8; 32]);

impl SharedSecret {
    /// The secret bytes. Feed them to a KDF; never store or log them.
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl core::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SharedSecret(..)")
    }
}

impl Drop for SharedSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Diffie-Hellman with the key of the identity, without exposing the key.
///
/// The Ed25519 identity is used for X25519 the way the Robonomics CPS
/// (`libcps`) does it: the private scalar is `SHA-512(seed)[..32]` clamped,
/// the peer's Ed25519 public key is mapped to its Montgomery form.
pub trait KeyAgreement {
    /// The raw X25519 shared secret with `peer`.
    ///
    /// # Errors
    ///
    /// [`IdentityError::InvalidPublicKey`] when `peer` is not a valid curve
    /// point or yields a degenerate (all-zero) secret.
    fn agree(&self, peer: &PublicKey) -> Result<SharedSecret, IdentityError>;
}

impl KeyAgreement for IdentitySigner {
    fn agree(&self, peer: &PublicKey) -> Result<SharedSecret, IdentityError> {
        let mut hash = Sha512::digest(self.seed());
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&hash[..32]);
        hash[..].zeroize();

        let point = CompressedEdwardsY(peer.to_bytes())
            .decompress()
            .ok_or(IdentityError::InvalidPublicKey)?;
        // `mul_clamped` applies the X25519 clamping to `scalar`.
        let shared = point.to_montgomery().mul_clamped(scalar).to_bytes();
        scalar.zeroize();
        if shared == [0; 32] {
            return Err(IdentityError::InvalidPublicKey);
        }
        Ok(SharedSecret(shared))
    }
}

#[cfg(any(test, feature = "mock"))]
impl KeyAgreement for crate::MockSigner {
    fn agree(&self, peer: &PublicKey) -> Result<SharedSecret, IdentityError> {
        self.identity_signer().agree(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Identity, Signer};

    fn signer(byte: u8) -> IdentitySigner {
        IdentitySigner::new(&crate::Secret::new([byte; 32]))
    }

    #[test]
    fn both_sides_derive_the_same_secret() {
        let (alice, bob) = (signer(1), signer(2));
        let a = alice.agree(&bob.public_key()).unwrap();
        let b = bob.agree(&alice.public_key()).unwrap();
        assert_eq!(a.expose(), b.expose());
        let other = alice.agree(&signer(3).public_key()).unwrap();
        assert_ne!(a.expose(), other.expose());
    }

    #[test]
    fn it_matches_an_independent_x25519() {
        use sha2::{Digest, Sha512};
        let (alice, bob) = (signer(5), signer(6));
        // Independent derivation: clamp SHA-512(seed), map the Ed25519 key to
        // Montgomery, X25519.
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&Sha512::digest([5u8; 32])[..32]);
        scalar[0] &= 248;
        scalar[31] &= 127;
        scalar[31] |= 64;
        let theirs = curve25519_dalek::edwards::CompressedEdwardsY(bob.public_key().to_bytes())
            .decompress()
            .unwrap()
            .to_montgomery();
        let expected = x25519_dalek::x25519(scalar, theirs.to_bytes());
        assert_eq!(alice.agree(&bob.public_key()).unwrap().expose(), &expected);
    }

    #[test]
    fn invalid_or_degenerate_peers_fail_closed() {
        let alice = signer(1);
        // y = 2 is not on the curve for any x.
        let mut bad = [0u8; 32];
        bad[0] = 2;
        assert_eq!(
            alice.agree(&PublicKey::from_bytes(bad)).unwrap_err(),
            IdentityError::InvalidPublicKey
        );
        // The identity point has small order: the shared secret is zero.
        let mut identity_point = [0u8; 32];
        identity_point[0] = 1;
        assert_eq!(
            alice
                .agree(&PublicKey::from_bytes(identity_point))
                .unwrap_err(),
            IdentityError::InvalidPublicKey
        );
        let _ = Identity::from_secret(&[1; 32]);
    }
}
