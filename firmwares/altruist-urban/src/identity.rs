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

use embassy_sync::mutex::Mutex;
use log::{error, info, warn};
use rohi_identity::{Identity, IdentityError, IdentitySigner, PublicKey, Signature, Signer};

use crate::config::{ConfigHandle, ServiceError};
use crate::tasks::RawMutex;

/// Signer of the stored identity; `None` while not provisioned.
static SIGNER: Mutex<RawMutex, Option<IdentitySigner>> = Mutex::new(None);

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
            *SIGNER.lock().await = Some(identity.signer());
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

    /// Persists `identity` and makes it the signing identity.
    ///
    /// Replacing an existing identity is the caller's decision, see the
    /// provisioning rules of the control protocol.
    pub async fn store(&self, identity: &Identity) -> Result<(), ServiceError> {
        self.config.set(identity).await?;
        *SIGNER.lock().await = Some(identity.signer());
        info!(
            "Identity: provisioned, public key {}",
            identity.public_key()
        );
        Ok(())
    }

    /// Removes the identity; signing stops until the device is provisioned again.
    pub async fn erase(&self) -> Result<(), ServiceError> {
        self.config.remove::<Identity>().await?;
        *SIGNER.lock().await = None;
        info!("Identity: erased");
        Ok(())
    }

    /// Ed25519 signature over exactly `message`, or
    /// [`IdentityError::NotProvisioned`].
    pub async fn sign(&self, message: &[u8]) -> Result<Signature, IdentityError> {
        match SIGNER.lock().await.as_ref() {
            Some(signer) => signer.sign(message),
            None => Err(IdentityError::NotProvisioned),
        }
    }
}
