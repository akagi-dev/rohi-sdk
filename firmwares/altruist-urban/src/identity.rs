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
//! Local control plane: the Altruist Urban side of `rohi-control`.

//! Device identity runtime.
//!
//! The Ed25519 identity is provisioned from the host through the control
//! protocol and stored in the `Identity` section of the configuration store,
//! next to the other sections and with the same A/B transactional writes. It is
//! never generated here: without a stored identity the device is
//! *not provisioned*, sensors and BLE diagnostics keep working, and only
//! operations that need the identity fail with
//! [`IdentityError::NotProvisioned`].
//!
//! ```text
//! control protocol -> IdentityHandle -> ConfigHandle -> ConfigStore (Identity section)
//!                          |
//!                          +-> STATE -> IdentityHandle::sign (future signed telemetry)
//! ```
//!
//! `STATE` is the runtime view of the stored identity: published by the
//! configuration owner task at boot and in the same step as every commit or
//! removal. Asking whether the device is provisioned, and for its public key,
//! reads only `STATE` and never the persistent store; the store is read again
//! only while `STATE` is unknown because it was unreadable at start.
//!
//! The secret is never logged and never leaves this module except into the
//! configuration store; only the public key is.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use log::info;
use rohi_identity::{Identity, IdentityError, IdentitySigner, PublicKey, Signature, Signer};

use crate::config::{ConfigHandle, ServiceError};
use crate::tasks::RawMutex;

/// What the runtime knows about the stored identity.
#[derive(Clone)]
// One static value that is as large as its signer anyway.
#[allow(clippy::large_enum_variant)]
enum IdentityState {
    /// The stored identity could not be read yet: the storage is unavailable
    /// or the section is unreadable. Not the same as not provisioned.
    Unknown,
    /// The device has no identity.
    NotProvisioned,
    /// The stored identity, ready to sign; its public key is the signer's.
    Provisioned(IdentitySigner),
}

impl IdentityState {
    /// The public key if the state is known: `Some(None)` when not
    /// provisioned.
    fn public_key(&self) -> Option<Option<PublicKey>> {
        match self {
            Self::Unknown => None,
            Self::NotProvisioned => Some(None),
            Self::Provisioned(signer) => Some(Some(signer.public_key())),
        }
    }

    fn of(signer: Option<IdentitySigner>) -> Self {
        match signer {
            Some(signer) => Self::Provisioned(signer),
            None => Self::NotProvisioned,
        }
    }
}

/// The runtime identity state, with the signer.
static STATE: Mutex<RawMutex, RefCell<IdentityState>> =
    Mutex::new(RefCell::new(IdentityState::Unknown));

/// Makes `signer` the signing identity (`None`: not provisioned).
///
/// Synchronous on purpose: the configuration owner task calls it right after
/// it committed or removed the `Identity` section, with no `.await` in
/// between, so a cancelled caller cannot leave the signer out of step with the
/// stored identity (see [`crate::config`]).
pub(crate) fn set_signer(signer: Option<IdentitySigner>) {
    STATE.lock(|state| *state.borrow_mut() = IdentityState::of(signer));
}

/// Cloneable access to the device identity.
#[derive(Clone, Copy)]
pub struct IdentityHandle {
    config: ConfigHandle,
}

/// The handle to the identity behind `config`.
///
/// It reads nothing: the configuration owner task publishes the stored identity
/// at boot, see [`crate::bindings::IdentityBinding`]. A missing identity is the
/// normal state of a fresh device. An unreadable configuration is reported as
/// such and never mistaken for a missing identity: the identity requests then
/// fail with the configuration error.
pub const fn start(config: ConfigHandle) -> IdentityHandle {
    IdentityHandle { config }
}

impl IdentityHandle {
    /// Public key of the stored identity, `None` when not provisioned.
    ///
    /// Answered from the runtime state; the store is only read while that is
    /// unknown, which is how a storage that was unavailable at start recovers.
    pub async fn public_key(&self) -> Result<Option<PublicKey>, ServiceError> {
        match STATE.lock(|state| state.borrow().public_key()) {
            Some(key) => Ok(key),
            None => self.load().await,
        }
    }

    async fn load(&self) -> Result<Option<PublicKey>, ServiceError> {
        let signer = self
            .config
            .get::<Identity>()
            .await?
            .map(|identity| identity.signer());
        Ok(STATE.lock(|state| {
            let mut state = state.borrow_mut();
            // A commit that raced with the read has published a newer state.
            if matches!(*state, IdentityState::Unknown) {
                *state = IdentityState::of(signer);
            }
            state.public_key().flatten()
        }))
    }

    /// Persists `identity`; the configuration owner task makes it the signing
    /// identity in the same step as the commit.
    ///
    /// Cancellation: as `ConfigHandle::set`: unchanged before the request is
    /// queued, logically completed (stored and signing) afterwards.
    ///
    /// Replacing an existing identity is the caller's decision, see the
    /// provisioning rules of the control protocol.
    pub async fn store(&self, identity: &Identity) -> Result<(), ServiceError> {
        self.config.set(identity).await?;
        info!(
            "Identity: provisioned, public key {}",
            identity.public_key()
        );
        Ok(())
    }

    /// Removes the identity; signing stops until the device is provisioned again
    /// (the owner task clears the signer together with the removal).
    ///
    /// Cancellation: as `ConfigHandle::remove`.
    pub async fn erase(&self) -> Result<(), ServiceError> {
        self.config.remove::<Identity>().await?;
        info!("Identity: erased");
        Ok(())
    }

    /// Ed25519 signature over exactly `message`, or
    /// [`IdentityError::NotProvisioned`].
    pub async fn sign(&self, message: &[u8]) -> Result<Signature, IdentityError> {
        match STATE.lock(|state| state.borrow().clone()) {
            IdentityState::Provisioned(signer) => signer.sign(message),
            _ => Err(IdentityError::NotProvisioned),
        }
    }
}

/// The signing identity as seen by the telemetry service: the identity may be
/// provisioned or erased at any time, and only signatures leave this module.
#[derive(Clone, Copy, Default)]
pub struct StaticSigner;

impl rohi_connectivity::FrameSigner for StaticSigner {
    fn sensor_id(&self) -> Result<PublicKey, rohi_connectivity::SignError> {
        STATE.lock(|state| match &*state.borrow() {
            IdentityState::Provisioned(signer) => Ok(signer.public_key()),
            _ => Err(rohi_connectivity::SignError::NotProvisioned),
        })
    }

    fn sign_payload(&self, message: &[u8]) -> Result<Signature, rohi_connectivity::SignError> {
        match STATE.lock(|state| state.borrow().clone()) {
            IdentityState::Provisioned(signer) => {
                Signer::sign(&signer, message).map_err(Into::into)
            }
            _ => Err(rohi_connectivity::SignError::NotProvisioned),
        }
    }
}
