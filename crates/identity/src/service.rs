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
//! The device identity in the generic service pipeline.
//!
//! The device holds at most one Ed25519 identity. It is provisioned from the
//! host, which derives the secret; the device never generates one. The secret
//! is a write-only action argument: no status field or result ever contains
//! it.

use heapless::Vec;
use rohi_service::{
    Action, ActionDescriptor, ActionError, ActionOutput, FIRST_SERVICE_ISSUE, FieldDescriptor,
    Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES, Service, ServiceHealth, ServiceState,
    ServiceSummary, Status, StatusWriter, Value, ValueKind, WriteError,
};

use crate::{Identity, PublicKey, SECRET_LEN};

/// Failure of the persistence behind the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The storage is currently unavailable.
    Unavailable,
    /// Unexpected failure.
    Internal,
}

impl From<StoreError> for ActionError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Unavailable => Self::Unavailable,
            StoreError::Internal => Self::Internal,
        }
    }
}

/// Persistence of the identity, implemented by the firmware over its
/// configuration store. Only the identity section may be touched.
#[allow(async_fn_in_trait)]
pub trait IdentityStore {
    /// Public key of the stored identity, `None` when not provisioned. The
    /// secret is never returned.
    async fn public_key(&self) -> Result<Option<PublicKey>, StoreError>;

    /// Persists `identity`.
    async fn store(&self, identity: &Identity) -> Result<(), StoreError>;

    /// Removes the identity; a no-op when absent.
    async fn erase(&self) -> Result<(), StoreError>;
}

/// Ids of the actions of the `identity` service.
pub mod action {
    /// Installs the device identity.
    pub const PROVISION: u32 = 1;
    /// Removes the device identity.
    pub const ERASE: u32 = 2;
}

/// Issue codes of the `identity` service.
pub mod issue {
    /// No identity is provisioned.
    pub const NOT_PROVISIONED: u32 = super::FIRST_SERVICE_ISSUE;
    /// The identity storage is unavailable.
    pub const STORAGE_UNAVAILABLE: u32 = super::FIRST_SERVICE_ISSUE + 1;
}

/// Field ids of the `identity` service status.
pub mod status_field {
    /// Whether an identity is provisioned.
    pub const PROVISIONED: u32 = 1;
    /// 32-byte Ed25519 public key; absent when not provisioned.
    pub const PUBLIC_KEY: u32 = 2;
}

/// Field ids of the `provision` action.
pub mod action_field {
    /// The derived 32-byte Ed25519 secret key (RFC 8032 seed), as argument.
    pub const SECRET: u32 = 1;
    /// The 32-byte Ed25519 public key of the stored identity, as result.
    pub const PUBLIC_KEY: u32 = 1;
}

/// The identity as an `identity` service with the `provision` and `erase`
/// actions.
#[derive(Service)]
pub struct IdentityService<S>(pub S);

impl<S: IdentityStore> Status for IdentityService<S> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::PROVISIONED, "provisioned", ValueKind::Bool),
        FieldDescriptor::new(status_field::PUBLIC_KEY, "public_key", ValueKind::Bytes),
    ];
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::NOT_PROVISIONED, "not_provisioned"),
        IssueDescriptor::new(issue::STORAGE_UNAVAILABLE, "storage_unavailable"),
    ];

    async fn summary(&self) -> ServiceSummary {
        match self.0.public_key().await {
            Ok(Some(_)) => ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true),
            // A fresh device: not an error, telemetry waits for provisioning.
            Ok(None) => ServiceSummary::new(ServiceState::Stopped, ServiceHealth::Unknown, false),
            Err(_) => ServiceSummary::new(ServiceState::Failed, ServiceHealth::Error, false),
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let issue = match self.0.public_key().await {
            Ok(Some(_)) => return,
            Ok(None) => Issue {
                severity: IssueSeverity::Info,
                code: issue::NOT_PROVISIONED,
                age_ms: None,
            },
            Err(_) => Issue {
                severity: IssueSeverity::Error,
                code: issue::STORAGE_UNAVAILABLE,
                age_ms: None,
            },
        };
        let _ = out.push(issue);
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let key = self.0.public_key().await.ok().flatten();
        out.bool(status_field::PROVISIONED, key.is_some())?;
        if let Some(key) = key {
            out.bytes(status_field::PUBLIC_KEY, key.as_bytes())?;
        }
        Ok(())
    }
}

impl<S: IdentityStore> Action for IdentityService<S> {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::PROVISION, "provision")
            .args(&[FieldDescriptor::new(
                action_field::SECRET,
                "secret",
                ValueKind::Bytes,
            )])
            .results(&[FieldDescriptor::new(
                action_field::PUBLIC_KEY,
                "public_key",
                ValueKind::Bytes,
            )])
            .secret(),
        ActionDescriptor::new(action::ERASE, "erase").privileged(),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            action::PROVISION => {
                let secret = args
                    .iter()
                    .find(|(id, _)| *id == action_field::SECRET)
                    .and_then(|(_, value)| value.as_bytes())
                    .filter(|secret| secret.len() == SECRET_LEN)
                    .ok_or(ActionError::InvalidArgument)?;
                let identity =
                    Identity::from_secret(secret).map_err(|_| ActionError::InvalidArgument)?;
                // Never replaces an existing identity: that needs an explicit
                // erase first.
                if self.0.public_key().await?.is_some() {
                    return Err(ActionError::Rejected("already_provisioned"));
                }
                self.0.store(&identity).await?;
                // The stored identity is read back before success is reported.
                let expected = identity.public_key();
                match self.0.public_key().await? {
                    Some(stored) if stored == expected => out
                        .bytes(action_field::PUBLIC_KEY, expected.as_bytes())
                        .map_err(|_| ActionError::Internal),
                    _ => Err(ActionError::Internal),
                }
            }
            action::ERASE => Ok(self.0.erase().await?),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::{ActionRegistry, StatusRegistry, Statuses};

    use super::*;

    const SERVICE_ID: u32 = rohi_service::service_id("identity");

    #[derive(Default)]
    struct Memory(RefCell<Option<PublicKey>>);

    impl IdentityStore for &Memory {
        async fn public_key(&self) -> Result<Option<PublicKey>, StoreError> {
            Ok(*self.0.borrow())
        }

        async fn store(&self, identity: &Identity) -> Result<(), StoreError> {
            *self.0.borrow_mut() = Some(identity.public_key());
            Ok(())
        }

        async fn erase(&self) -> Result<(), StoreError> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
    }

    fn secret(bytes: &[u8]) -> [(u32, Value); 1] {
        [(action_field::SECRET, Value::bytes(bytes).unwrap())]
    }

    #[test]
    fn provision_stores_the_identity_and_returns_only_the_public_key() {
        let memory = Memory::default();
        let service = IdentityService(&memory);
        let registry = (&service,);
        let records =
            block_on(registry.invoke(SERVICE_ID, action::PROVISION, &secret(&[7; SECRET_LEN])))
                .unwrap();
        let expected = Identity::from_secret(&[7; SECRET_LEN])
            .unwrap()
            .public_key();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0][0].1.as_bytes(),
            Some(expected.as_bytes().as_slice())
        );
        assert_eq!(*memory.0.borrow(), Some(expected));
    }

    #[test]
    fn an_existing_identity_is_never_replaced() {
        let memory = Memory::default();
        let service = IdentityService(&memory);
        let registry = (&service,);
        block_on(registry.invoke(SERVICE_ID, action::PROVISION, &secret(&[7; SECRET_LEN])))
            .unwrap();
        assert_eq!(
            block_on(registry.invoke(SERVICE_ID, action::PROVISION, &secret(&[8; SECRET_LEN]))),
            Err(ActionError::Rejected("already_provisioned"))
        );
        block_on(registry.invoke(SERVICE_ID, action::ERASE, &[])).unwrap();
        assert!(memory.0.borrow().is_none());
        block_on(registry.invoke(SERVICE_ID, action::PROVISION, &secret(&[8; SECRET_LEN])))
            .unwrap();
    }

    #[test]
    fn a_secret_of_the_wrong_size_is_invalid() {
        let memory = Memory::default();
        let service = IdentityService(&memory);
        let registry = (&service,);
        assert_eq!(
            block_on(registry.invoke(SERVICE_ID, action::PROVISION, &secret(&[7; 5]))),
            Err(ActionError::InvalidArgument)
        );
        assert!(memory.0.borrow().is_none());
    }

    #[test]
    fn status_never_contains_a_secret_and_the_actions_are_protected() {
        let memory = Memory::default();
        let service = IdentityService(&memory);
        let statuses = Statuses::<NoopRawMutex, _>::new((&service,));
        let status = block_on(statuses.status(SERVICE_ID, 0)).unwrap();
        assert_eq!(status.summary.state, ServiceState::Stopped);
        assert_eq!(status.issues[0].code, issue::NOT_PROVISIONED);
        assert_eq!(status.fields.len(), 1);

        let actions = (&service,).service_actions(0);
        assert!(actions[0].secret_args && actions[0].privileged);
        assert!(actions[1].privileged && !actions[1].secret_args);
    }

    #[test]
    fn the_service_identity_is_part_of_the_wire_abi() {
        assert_eq!(<IdentityService<&Memory> as Service>::NAME, "identity");
        assert_eq!(SERVICE_ID, <IdentityService<&Memory> as Service>::ID);
    }
}
