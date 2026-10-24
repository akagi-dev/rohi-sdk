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
//!                          +-> SIGNER cache -> IdentityHandle::sign (future signed telemetry)
//! ```
//!
//! The secret is never logged and never leaves this module except into the
//! configuration store; only the public key is.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use log::{error, info, warn};
use rohi_identity::{Identity, IdentityError, IdentitySigner, PublicKey, Signature, Signer};

use crate::config::{ConfigHandle, ServiceError};
use crate::tasks::RawMutex;

/// Signer of the stored identity; `None` while not provisioned.
static SIGNER: Mutex<RawMutex, RefCell<Option<IdentitySigner>>> = Mutex::new(RefCell::new(None));

/// Makes `signer` the signing identity (`None`: not provisioned).
///
/// Synchronous on purpose: the configuration owner task calls it right after
/// it committed or removed the `Identity` section, with no `.await` in
/// between, so a cancelled caller cannot leave the signer out of step with the
/// stored identity (see [`crate::config`]).
pub(crate) fn set_signer(signer: Option<IdentitySigner>) {
    SIGNER.lock(|cell| *cell.borrow_mut() = signer);
}

/// Cloneable access to the device identity.
#[derive(Clone, Copy)]
pub struct IdentityHandle {
    config: ConfigHandle,
}

/// Loads the stored identity, if any, and returns the handle.
///
/// A missing identity is the normal state of a fresh device. An unreadable
/// configuration is reported as such and never mistaken for a missing identity:
/// the identity requests then fail with the configuration error.
pub async fn start(config: ConfigHandle) -> IdentityHandle {
    let handle = IdentityHandle { config };
    match config.get::<Identity>().await {
        Ok(Some(identity)) => {
            info!(
                "Identity: provisioned, public key {}",
                identity.public_key()
            );
            set_signer(Some(identity.signer()));
        }
        Ok(None) => info!("Identity: not provisioned"),
        Err(ServiceError::Unavailable(_)) => {
            warn!("Identity: configuration unavailable, identity state unknown")
        }
        Err(_) => error!("Identity: the stored identity section is unreadable"),
    }
    handle
}

impl IdentityHandle {
    /// Public key of the stored identity, `None` when not provisioned.
    pub async fn public_key(&self) -> Result<Option<PublicKey>, ServiceError> {
        Ok(self
            .config
            .get::<Identity>()
            .await?
            .map(|identity| identity.public_key()))
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
        SIGNER.lock(|cell| match cell.borrow().as_ref() {
            Some(signer) => signer.sign(message),
            None => Err(IdentityError::NotProvisioned),
        })
    }
}

/// The signing identity as seen by the telemetry service: the identity may be
/// provisioned or erased at any time, and only signatures leave this module.
#[derive(Clone, Copy, Default)]
pub struct StaticSigner;

impl rohi_connectivity::FrameSigner for StaticSigner {
    fn sensor_id(&self) -> Result<PublicKey, rohi_connectivity::SignError> {
        SIGNER.lock(|cell| match cell.borrow().as_ref() {
            Some(signer) => Ok(signer.public_key()),
            None => Err(rohi_connectivity::SignError::NotProvisioned),
        })
    }

    fn sign_payload(&self, message: &[u8]) -> Result<Signature, rohi_connectivity::SignError> {
        SIGNER.lock(|cell| match cell.borrow().as_ref() {
            Some(signer) => Signer::sign(signer, message).map_err(Into::into),
            None => Err(rohi_connectivity::SignError::NotProvisioned),
        })
    }
}
