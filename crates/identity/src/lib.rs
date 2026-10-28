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
#![no_std]

//! Cryptographic identity of a Robonomics Open Hardware device.
//!
//! Every provisioned device has exactly one Ed25519 identity: a 32-byte
//! secret (the RFC 8032 secret key, also called seed) stored in the
//! [`Identity`] configuration section. The public key is derived from it and
//! never persisted.
//!
//! ```text
//! host: master SURI //<number> -> derived secret -> control protocol
//! device: Identity (ConfigStore) -> IdentitySigner -> sign(exact bytes)
//! ```
//!
//! The device never generates its own identity, and never learns the master
//! seed or the provisioning number. This crate only provides the key, the
//! [`Signer`] abstraction and Ed25519 signatures; message formats, domain
//! separation and replay protection belong to the protocol that is signed. It
//! has no transport (BLE, Wi-Fi, ...) dependency.
//!
//! ```
//! use rohi_identity::{Identity, Signer};
//!
//! let identity = Identity::from_secret(&[7; 32]).unwrap();
//! let signer = identity.signer();
//! let signature = signer.sign(b"payload").unwrap();
//! assert!(identity.public_key().verify(b"payload", &signature).is_ok());
//! ```

#[cfg(any(test, feature = "std"))]
extern crate std;

mod error;
mod identity;
mod keys;
mod secret;
mod service;
mod signer;

#[cfg(test)]
mod tests;

pub use error::IdentityError;
pub use identity::{Identity, SECRET_LEN};
pub use keys::{PUBLIC_KEY_LEN, PublicKey, SIGNATURE_LEN, Signature};
pub use secret::Secret;
pub use service::{
    IdentityService, IdentityStore, StoreError as IdentityStoreError, action_field, status_field,
};
#[cfg(any(test, feature = "mock"))]
pub use signer::MockSigner;
pub use signer::{IdentitySigner, Signer};
