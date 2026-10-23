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
//! Desktop client library of ROHI devices.

//! Host side of identity provisioning.
//!
//! The master secret URI (SURI) never leaves the host. A device gets only the
//! Ed25519 secret derived from it with the hard junction `//<number>`:
//!
//! ```text
//! master SURI -> hard derivation //<number> -> derived Ed25519 secret -> device
//! ```
//!
//! The derivation is the Substrate one (`sp-core`), the same that
//! `Pair::from_string("<suri>//<number>")` performs, and no namespace is added
//! to the path. It is part of the provisioning ABI: the same master and number
//! must give the same identity on every machine and in every future version.
//! The number must be unique within one master hierarchy; the device never
//! learns it.

use core::fmt;

use rohi_identity::{Identity, PublicKey, SECRET_LEN, Secret};

/// An identity derived on the host, ready to be installed on a device.
///
/// Holds the secret, so it is zeroized on drop and its `Debug` shows only the
/// public key.
pub struct DerivedIdentity {
    secret: Secret<[u8; SECRET_LEN]>,
    public_key: PublicKey,
}

impl DerivedIdentity {
    /// Identity of a secret; the public key is computed from it.
    pub fn from_secret(secret: [u8; SECRET_LEN]) -> Self {
        let identity = Identity::new(Secret::new(secret));
        Self {
            public_key: identity.public_key(),
            secret: Secret::new(secret),
        }
    }

    /// The derived secret; only to be sent to the device being provisioned.
    pub fn secret(&self) -> &Secret<[u8; SECRET_LEN]> {
        &self.secret
    }

    /// The public key the device is expected to report.
    pub fn public_key(&self) -> PublicKey {
        self.public_key
    }
}

impl fmt::Debug for DerivedIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DerivedIdentity")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

/// Derives the identity `//<number>` of the master `suri`.
///
/// `suri` is a Substrate secret URI: a mnemonic phrase or `0x` hex seed, with
/// an optional path and password. The public key comes from `sp-core`
/// independently of the device-side implementation.
#[cfg(feature = "provision")]
pub fn derive_identity(suri: &str, number: u64) -> crate::Result<DerivedIdentity> {
    use sp_core::crypto::{DeriveJunction, Pair as _};
    use sp_core::ed25519::Pair;
    use zeroize::Zeroize;

    let invalid = || crate::Error::InvalidInput("the seed is not a valid secret URI".into());
    let (master, _) = Pair::from_string_with_seed(suri, None).map_err(|_| invalid())?;
    let (derived, _) = master
        .derive(core::iter::once(DeriveJunction::hard(number)), None)
        .map_err(|_| invalid())?;
    let mut seed = derived.seed();
    let public_key = PublicKey::from_bytes(derived.public().0);
    let identity = DerivedIdentity {
        secret: Secret::new(seed),
        public_key,
    };
    seed.zeroize();
    Ok(identity)
}

#[cfg(test)]
mod tests;
