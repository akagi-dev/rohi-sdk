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
//! The `control_auth` service: the trusted administrator key.

use heapless::Vec;
use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;
use rohi_control::ControlAccess;
use rohi_identity::{IdentityError, PUBLIC_KEY_LEN, PublicKey, SIGNATURE_LEN, Signature};
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, FieldDescriptor, Issue,
    IssueDescriptor, IssueSeverity, MAX_ISSUES, Service, ServiceHealth, ServiceState,
    ServiceSummary, Status, StatusWriter, Value, ValueKind, WriteError,
};
use sha2::{Digest, Sha256};

/// Size of the key fingerprint reported in the status, in bytes.
const FINGERPRINT_LEN: usize = 8;

/// The persisted administrator public key (configuration section
/// `AdminKeySection`).
///
/// It lives in the normal configuration store, so it has the A/B transactional
/// writes of every section, and it is written only by
/// `control_auth.set_admin_key` and removed by `clear_admin_key` and by a
/// factory reset. It is not a managed config section: no config request reads,
/// patches or resets it. CBOR: `[bytes(32)]`.
#[derive(Clone, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct AdminKeySection {
    #[n(0)]
    #[cbor(with = "minicbor::bytes")]
    key: [u8; PUBLIC_KEY_LEN],
}

impl AdminKeySection {
    /// The section for `key`.
    pub const fn new(key: PublicKey) -> Self {
        Self {
            key: key.to_bytes(),
        }
    }

    /// The stored key.
    pub const fn key(&self) -> PublicKey {
        PublicKey::from_bytes(self.key)
    }
}

/// Why the key store failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminKeyStoreError {
    /// The configuration storage is not usable.
    Unavailable,
    /// Anything else.
    Internal,
}

/// Where the administrator key is kept. The firmware implements it over its
/// configuration owner, which publishes the committed key into the
/// [`ControlAccess`] in the same step as the commit.
#[allow(async_fn_in_trait)]
pub trait AdminKeyStore {
    /// Persists `key` as the one trusted key.
    async fn store(&self, key: PublicKey) -> Result<(), AdminKeyStoreError>;

    /// Removes the trusted key.
    async fn erase(&self) -> Result<(), AdminKeyStoreError>;
}

/// Action ids of the `control_auth` service.
pub mod auth_action {
    /// Enrolls or replaces the administrator key.
    pub const SET_ADMIN_KEY: u32 = 1;
    /// Removes the administrator key.
    pub const CLEAR_ADMIN_KEY: u32 = 2;
}

/// Field ids of the `control_auth` service.
pub mod auth_field {
    /// Status: an administrator key is enrolled.
    pub const ENROLLED: u32 = 1;
    /// Status and action result: a fingerprint of the key.
    pub const FINGERPRINT: u32 = 2;
    /// Action argument: the 32-byte Ed25519 public key.
    pub const PUBLIC_KEY: u32 = 1;
}

/// Issue codes of the `control_auth` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// No administrator key: a restricted TCP endpoint refuses privileged
    /// requests.
    pub const NOT_ENROLLED: u32 = BASE;
}

/// The first bytes of the SHA-256 of `key`: what the status shows instead of
/// the key.
///
/// ```
/// use rohi_control_endpoints::fingerprint;
/// use rohi_identity::PublicKey;
///
/// let first = fingerprint(&PublicKey::from_bytes([1; 32]));
/// assert_eq!(first.len(), 8);
/// assert_ne!(first, fingerprint(&PublicKey::from_bytes([2; 32])));
/// ```
pub fn fingerprint(key: &PublicKey) -> [u8; FINGERPRINT_LEN] {
    let digest = Sha256::digest(key.as_bytes());
    let mut out = [0u8; FINGERPRINT_LEN];
    out.copy_from_slice(&digest[..FINGERPRINT_LEN]);
    out
}

/// The `control_auth` service: enrolls, replaces and removes the one trusted
/// administrator key of signed requests, and reports whether there is one.
///
/// `set_admin_key` is a `Provision` action and `clear_admin_key` a `Mutate`
/// one, so each needs what its endpoint demands for them: nothing on an open
/// endpoint, a signature of the current key on a restricted TCP endpoint. A
/// key that is not a point of the curve is refused. Replacing or removing the
/// key voids every outstanding challenge.
///
/// ```
/// use core::cell::Cell;
///
/// use embassy_futures::block_on;
/// use rohi_control::ControlAccess;
/// use rohi_control_endpoints::{AdminKeyStore, AdminKeyStoreError, ControlAuthService};
/// use rohi_identity::{Identity, PublicKey};
/// use rohi_service::{ActionRegistry, ServiceState, Status, Value, service_id};
///
/// // The firmware stores the key and publishes it; this one only publishes.
/// struct Memory<'a>(&'a ControlAccess);
///
/// impl AdminKeyStore for Memory<'_> {
///     async fn store(&self, key: PublicKey) -> Result<(), AdminKeyStoreError> {
///         self.0.set_admin_key(Some(key.to_bytes()));
///         Ok(())
///     }
///     async fn erase(&self) -> Result<(), AdminKeyStoreError> {
///         self.0.set_admin_key(None);
///         Ok(())
///     }
/// }
///
/// let access = ControlAccess::new(|_| false);
/// let service = ControlAuthService::new(&access, Memory(&access));
/// assert_eq!(block_on(service.summary()).state, ServiceState::Stopped);
///
/// let admin = Identity::from_secret(&[7; 32]).unwrap().public_key();
/// let registry = (&service,);
/// let args = [(1, Value::bytes(admin.as_bytes()).unwrap())];
/// block_on(registry.invoke(service_id("control_auth"), 1, &args)).unwrap();
/// assert_eq!(block_on(service.summary()).state, ServiceState::Running);
/// assert_eq!(access.admin_key().unwrap().key, admin.to_bytes());
/// ```
#[derive(Service)]
pub struct ControlAuthService<'a, S> {
    access: &'a ControlAccess,
    store: S,
}

impl<'a, S> ControlAuthService<'a, S> {
    /// The service over `access` and `store`.
    pub const fn new(access: &'a ControlAccess, store: S) -> Self {
        Self { access, store }
    }
}

impl<S: AdminKeyStore> Status for ControlAuthService<'_, S> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(auth_field::ENROLLED, "enrolled", ValueKind::Bool),
        FieldDescriptor::new(auth_field::FINGERPRINT, "fingerprint", ValueKind::Bytes),
    ];
    const ISSUES: &'static [IssueDescriptor] =
        &[IssueDescriptor::new(issue::NOT_ENROLLED, "not_enrolled")];

    async fn summary(&self) -> ServiceSummary {
        if self.access.admin_key().is_some() {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
        } else {
            // A fresh device: not an error, signed requests wait for a key.
            ServiceSummary::new(ServiceState::Stopped, ServiceHealth::Unknown, false)
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if self.access.admin_key().is_none() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Info,
                code: issue::NOT_ENROLLED,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let key = self.access.admin_key();
        out.bool(auth_field::ENROLLED, key.is_some())?;
        if let Some(key) = key {
            out.bytes(
                auth_field::FINGERPRINT,
                &fingerprint(&PublicKey::from_bytes(key.key)),
            )?;
        }
        Ok(())
    }
}

impl<S: AdminKeyStore> Action for ControlAuthService<'_, S> {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(
            auth_action::SET_ADMIN_KEY,
            "set_admin_key",
            ActionAccess::Provision,
        )
        .args(&[FieldDescriptor::new(
            auth_field::PUBLIC_KEY,
            "public_key",
            ValueKind::Bytes,
        )])
        .results(&[FieldDescriptor::new(
            auth_field::FINGERPRINT,
            "fingerprint",
            ValueKind::Bytes,
        )]),
        ActionDescriptor::new(
            auth_action::CLEAR_ADMIN_KEY,
            "clear_admin_key",
            ActionAccess::Mutate,
        ),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            auth_action::SET_ADMIN_KEY => {
                let key = args
                    .iter()
                    .find(|(id, _)| *id == auth_field::PUBLIC_KEY)
                    .and_then(|(_, value)| value.as_bytes())
                    .and_then(PublicKey::from_slice)
                    .filter(is_curve_point)
                    .ok_or_else(|| {
                        log::warn!(target: "control_auth", "Key rejected: not an Ed25519 public key");
                        ActionError::InvalidArgument
                    })?;
                self.store
                    .store(key)
                    .await
                    .map_err(|error| store_failed("store", error))?;
                let print = fingerprint(&key);
                log::info!(target: "control_auth", "Administrator key set (fp={:02x}{:02x}{:02x}{:02x})", print[0], print[1], print[2], print[3]);
                out.bytes(auth_field::FINGERPRINT, &print)
                    .map_err(|_| ActionError::Internal)
            }
            auth_action::CLEAR_ADMIN_KEY => {
                self.store
                    .erase()
                    .await
                    .map_err(|error| store_failed("erase", error))?;
                log::info!(target: "control_auth", "Administrator key removed");
                Ok(())
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

fn is_curve_point(key: &PublicKey) -> bool {
    // A zero signature never verifies; only a key that is no curve point is
    // reported as an invalid key.
    let Some(signature) = Signature::from_slice(&[0u8; SIGNATURE_LEN]) else {
        return false;
    };
    key.verify(&[], &signature) != Err(IdentityError::InvalidPublicKey)
}

#[cold]
#[inline(never)]
fn store_failed(operation: &'static str, error: AdminKeyStoreError) -> ActionError {
    log::warn!(target: "control_auth", "Store failed (op={} err={:?})", operation, error);
    match error {
        AdminKeyStoreError::Unavailable => ActionError::Unavailable,
        AdminKeyStoreError::Internal => ActionError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use embassy_futures::block_on;
    use rohi_identity::Identity;
    use rohi_service::{ActionRegistry, service_id};

    use super::*;

    struct Memory<'a> {
        access: &'a ControlAccess,
        fail: Cell<bool>,
    }

    impl AdminKeyStore for &Memory<'_> {
        async fn store(&self, key: PublicKey) -> Result<(), AdminKeyStoreError> {
            if self.fail.get() {
                return Err(AdminKeyStoreError::Unavailable);
            }
            self.access.set_admin_key(Some(key.to_bytes()));
            Ok(())
        }

        async fn erase(&self) -> Result<(), AdminKeyStoreError> {
            self.access.set_admin_key(None);
            Ok(())
        }
    }

    fn key(seed: u8) -> PublicKey {
        Identity::from_secret(&[seed; 32]).unwrap().public_key()
    }

    #[test]
    fn the_actions_declare_their_privileges() {
        let actions = <ControlAuthService<&Memory> as Action>::ACTIONS;
        assert_eq!(actions[0].access, ActionAccess::Provision);
        assert_eq!(actions[1].access, ActionAccess::Mutate);
        assert!(!actions[0].secret_args);
    }

    #[test]
    fn a_key_is_enrolled_replaced_and_removed() {
        let access = ControlAccess::new(|_| false);
        let memory = Memory {
            access: &access,
            fail: Cell::new(false),
        };
        let service = ControlAuthService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control_auth");
        let set = |key: PublicKey| {
            block_on(registry.invoke(id, 1, &[(1, Value::bytes(key.as_bytes()).unwrap())]))
        };
        set(key(1)).unwrap();
        let first = access.admin_key().unwrap();
        // Replacing the key voids the challenges of the old one.
        set(key(2)).unwrap();
        let second = access.admin_key().unwrap();
        assert_eq!(second.key, key(2).to_bytes());
        assert_ne!(first.epoch, second.epoch);
        block_on(registry.invoke(id, 2, &[])).unwrap();
        assert!(access.admin_key().is_none());
    }

    #[test]
    fn a_bad_key_is_refused_and_leaves_the_state_alone() {
        let access = ControlAccess::new(|_| false);
        let memory = Memory {
            access: &access,
            fail: Cell::new(false),
        };
        let service = ControlAuthService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control_auth");
        let mut not_a_point = [0u8; 32];
        not_a_point[0] = 2;
        for bad in [&[1u8; 31][..], &[1u8; 33][..], &not_a_point[..]] {
            assert_eq!(
                block_on(registry.invoke(id, 1, &[(1, Value::bytes(bad).unwrap())])),
                Err(ActionError::InvalidArgument)
            );
        }
        assert_eq!(
            block_on(registry.invoke(id, 1, &[])),
            Err(ActionError::InvalidArgument)
        );
        assert!(access.admin_key().is_none());
        memory.fail.set(true);
        assert_eq!(
            block_on(registry.invoke(id, 1, &[(1, Value::bytes(key(1).as_bytes()).unwrap())])),
            Err(ActionError::Unavailable)
        );
    }

    #[test]
    fn the_status_shows_a_fingerprint_and_never_the_key() {
        struct Collect(std::vec::Vec<(u32, Value)>);
        impl StatusWriter for Collect {
            fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
                self.0.push((id, value));
                Ok(())
            }
        }
        let access = ControlAccess::new(|_| false);
        let memory = Memory {
            access: &access,
            fail: Cell::new(false),
        };
        let service = ControlAuthService::new(&access, &memory);
        let mut out = Collect(std::vec::Vec::new());
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.0, [(1, Value::Bool(false))]);
        access.set_admin_key(Some(key(1).to_bytes()));
        let mut out = Collect(std::vec::Vec::new());
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.0.len(), 2);
        assert_eq!(out.0[1].1, Value::bytes(&fingerprint(&key(1))).unwrap());
    }

    #[test]
    fn the_section_encoding_is_stable() {
        let section = AdminKeySection::new(PublicKey::from_bytes([0xab; 32]));
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(&section, &mut buffer).unwrap();
        // array(1), bytes(32), key.
        assert_eq!(&buffer[..3], [0x81, 0x58, 0x20]);
        assert_eq!(used, 35);
        let back: AdminKeySection = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(back.key(), PublicKey::from_bytes([0xab; 32]));
    }
}
