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

use ed25519_zebra::{SigningKey, VerificationKeyBytes};
use zeroize::Zeroize;

use crate::{Identity, IdentityError, PublicKey, SECRET_LEN, Secret, Signature};

/// Transport-independent Ed25519 signing.
///
/// Signs exactly the given bytes: callers serialize their message once and
/// must not decode, modify or re-encode the payload after signing. There is
/// deliberately no access to the private key.
pub trait Signer {
    /// Signing failure.
    type Error;

    /// The public key matching the signatures.
    fn public_key(&self) -> PublicKey;

    /// Ed25519 signature over exactly `message`.
    fn sign(&self, message: &[u8]) -> Result<Signature, Self::Error>;
}

/// [`Signer`] backed by a provisioned [`Identity`].
///
/// The key is zeroized on drop and never printed.
pub struct IdentitySigner {
    key: SigningKey,
    public: PublicKey,
}

impl IdentitySigner {
    pub(crate) fn new(secret: &Secret<[u8; SECRET_LEN]>) -> Self {
        let key = SigningKey::from(*secret.expose_secret());
        let public = PublicKey::from_bytes(VerificationKeyBytes::from(&key).into());
        Self { key, public }
    }

    /// Signer of a stored identity: [`IdentityError::NotProvisioned`] when
    /// there is none. A missing identity is never replaced by a generated one.
    pub fn provisioned(identity: Option<&Identity>) -> Result<Self, IdentityError> {
        identity
            .map(Identity::signer)
            .ok_or(IdentityError::NotProvisioned)
    }
}

impl Signer for IdentitySigner {
    /// Signing with a loaded key never fails.
    type Error = IdentityError;

    fn public_key(&self) -> PublicKey {
        self.public
    }

    fn sign(&self, message: &[u8]) -> Result<Signature, IdentityError> {
        Ok(Signature::from_bytes(self.key.sign(message).to_bytes()))
    }
}

impl Drop for IdentitySigner {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl fmt::Debug for IdentitySigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentitySigner")
            .field("public_key", &self.public)
            .finish_non_exhaustive()
    }
}

/// Test [`Signer`]: a real Ed25519 key from a fixed, public test secret, or a
/// signer that always fails with a given error (for example
/// [`IdentityError::NotProvisioned`]). Never use it outside tests.
#[cfg(any(test, feature = "mock"))]
#[derive(Debug)]
pub struct MockSigner {
    signer: IdentitySigner,
    failure: Option<IdentityError>,
}

#[cfg(any(test, feature = "mock"))]
impl MockSigner {
    /// Signs with the key of `secret`.
    pub fn new(secret: [u8; SECRET_LEN]) -> Self {
        Self {
            signer: IdentitySigner::new(&Secret::new(secret)),
            failure: None,
        }
    }

    /// Fails every [`sign`](Signer::sign) with `error`.
    pub fn failing(error: IdentityError) -> Self {
        Self {
            failure: Some(error),
            ..Self::default()
        }
    }
}

#[cfg(any(test, feature = "mock"))]
impl Default for MockSigner {
    /// Key of the secret `[0x42; 32]`.
    fn default() -> Self {
        Self::new([0x42; SECRET_LEN])
    }
}

#[cfg(any(test, feature = "mock"))]
impl Signer for MockSigner {
    type Error = IdentityError;

    fn public_key(&self) -> PublicKey {
        self.signer.public_key()
    }

    fn sign(&self, message: &[u8]) -> Result<Signature, IdentityError> {
        match self.failure {
            Some(error) => Err(error),
            None => self.signer.sign(message),
        }
    }
}
