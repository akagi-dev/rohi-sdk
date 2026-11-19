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
//! The `control_psk` service: the pre-shared key of the TCP endpoint.

use heapless::Vec;
use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;
use rohi_control::ControlAccess;
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, FieldDescriptor, Issue,
    IssueDescriptor, IssueSeverity, MAX_ISSUES, Service, ServiceHealth, ServiceState,
    ServiceSummary, Status, StatusWriter, Value, ValueKind, WriteError,
};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// Size of the pre-shared key, in bytes.
pub const PSK_LEN: usize = 32;

/// Size of the key fingerprint reported in the status, in bytes.
const FINGERPRINT_LEN: usize = 8;

/// The persisted pre-shared key of the TCP endpoint (configuration section
/// `TcpPskSection`).
///
/// It lives in the normal configuration store, so it has the A/B transactional writes of
/// every section, and it is written only by `control_psk.set_tcp_psk` and removed by
/// `clear_tcp_psk` and by a factory reset. It is not a managed config section: no config
/// request reads, patches or resets it, and no status or log shows it. The memory it occupies
/// is wiped when the value is dropped. CBOR: `[bytes(32)]`.
#[derive(Clone, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct TcpPskSection {
    #[n(0)]
    #[cbor(with = "minicbor::bytes")]
    key: [u8; PSK_LEN],
}

impl Drop for TcpPskSection {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl TcpPskSection {
    /// The section for `key`.
    pub const fn new(key: [u8; PSK_LEN]) -> Self {
        Self { key }
    }

    /// The stored key; handle it like a secret: copy it only where it is used and wipe the copy.
    pub const fn key(&self) -> &[u8; PSK_LEN] {
        &self.key
    }
}

/// Why the key store failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PskStoreError {
    /// The configuration storage is not usable.
    Unavailable,
    /// Anything else.
    Internal,
}

/// Where the pre-shared key is kept. The firmware implements it over its configuration owner,
/// which publishes the committed key to the TCP endpoint and records in the
/// [`ControlAccess`] that there is one (and that it changed) in the same step as the commit.
#[allow(async_fn_in_trait)]
pub trait PskStore {
    /// Persists `key` as the one pre-shared key.
    async fn store(&self, key: &[u8; PSK_LEN]) -> Result<(), PskStoreError>;

    /// Removes the pre-shared key.
    async fn erase(&self) -> Result<(), PskStoreError>;
}

/// Action ids of the `control_psk` service.
pub mod psk_action {
    /// Sets or replaces the pre-shared key.
    pub const SET_TCP_PSK: u32 = 1;
    /// Removes the pre-shared key.
    pub const CLEAR_TCP_PSK: u32 = 2;
}

/// Field ids of the `control_psk` service.
pub mod psk_field {
    /// Status: a pre-shared key is provisioned.
    pub const ENROLLED: u32 = 1;
    /// Action result: a fingerprint of the key. The status does not show it.
    pub const FINGERPRINT: u32 = 2;
    /// Action argument: the 32-byte key.
    pub const PSK: u32 = 1;
}

/// Issue codes of the `control_psk` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// No pre-shared key: a restricted TCP endpoint refuses every client.
    pub const NOT_ENROLLED: u32 = BASE;
}

/// The first bytes of a SHA-256 of `key` under a fixed domain: what the status shows instead of
/// the key, so that an operator can tell which key a device holds.
///
/// ```
/// use rohi_control_endpoints::fingerprint;
///
/// let first = fingerprint(&[1; 32]);
/// assert_eq!(first.len(), 8);
/// assert_ne!(first, fingerprint(&[2; 32]));
/// ```
pub fn fingerprint(key: &[u8; PSK_LEN]) -> [u8; FINGERPRINT_LEN] {
    let mut hash = Sha256::new();
    hash.update(b"rohi-control-tcp psk fingerprint v1");
    hash.update(key);
    let digest = hash.finalize();
    let mut out = [0u8; FINGERPRINT_LEN];
    out.copy_from_slice(&digest[..FINGERPRINT_LEN]);
    out
}

/// The `control_psk` service: sets, replaces and removes the one pre-shared key of the TCP
/// endpoint, and reports whether there is one.
///
/// `set_tcp_psk` is a `Provision` action with a secret argument and `clear_tcp_psk` a `Mutate`
/// one. `set_tcp_psk` is marked `secure_link`: in every access mode it needs an encrypted (paired)
/// BLE link, or the authenticated and encrypted TCP connection that the current key established. It never runs
/// over an unauthenticated TCP link, not even on an open endpoint and the key is never part of any response or status. Setting
/// or removing the key ends the TCP connections that were established with the old one.
///
/// ```
/// use core::cell::Cell;
///
/// use embassy_futures::block_on;
/// use rohi_control::ControlAccess;
/// use rohi_control_endpoints::{ControlPskService, PSK_LEN, PskStore, PskStoreError};
/// use rohi_service::{ActionRegistry, ServiceState, Status, Value, service_id};
///
/// // The firmware stores the key and publishes it; this one only records that there is one.
/// struct Memory<'a>(&'a ControlAccess, Cell<[u8; PSK_LEN]>);
///
/// impl PskStore for Memory<'_> {
///     async fn store(&self, key: &[u8; PSK_LEN]) -> Result<(), PskStoreError> {
///         self.1.set(*key);
///         self.0.set_tcp_psk(true);
///         Ok(())
///     }
///     async fn erase(&self) -> Result<(), PskStoreError> {
///         self.0.set_tcp_psk(false);
///         Ok(())
///     }
/// }
///
/// let access = ControlAccess::new(|_| false);
/// let service = ControlPskService::new(&access, Memory(&access, Cell::new([0; PSK_LEN])));
/// assert_eq!(block_on(service.summary()).state, ServiceState::Stopped);
///
/// let registry = (&service,);
/// let args = [(1, Value::bytes(&[7; PSK_LEN]).unwrap())];
/// block_on(registry.invoke(service_id("control_psk"), 1, &args)).unwrap();
/// assert_eq!(block_on(service.summary()).state, ServiceState::Running);
/// assert!(access.tcp_psk_enrolled());
/// ```
#[derive(Service)]
pub struct ControlPskService<'a, S> {
    access: &'a ControlAccess,
    store: S,
}

impl<'a, S> ControlPskService<'a, S> {
    /// The service over `access` and `store`.
    pub const fn new(access: &'a ControlAccess, store: S) -> Self {
        Self { access, store }
    }
}

impl<S: PskStore> Status for ControlPskService<'_, S> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[FieldDescriptor::new(
        psk_field::ENROLLED,
        "enrolled",
        ValueKind::Bool,
    )];
    const ISSUES: &'static [IssueDescriptor] =
        &[IssueDescriptor::new(issue::NOT_ENROLLED, "not_enrolled")];

    async fn summary(&self) -> ServiceSummary {
        if self.access.tcp_psk_enrolled() {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
        } else {
            // A fresh device: not an error, a restricted TCP endpoint waits for a key.
            ServiceSummary::new(ServiceState::Stopped, ServiceHealth::Unknown, false)
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if !self.access.tcp_psk_enrolled() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Info,
                code: issue::NOT_ENROLLED,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        out.bool(psk_field::ENROLLED, self.access.tcp_psk_enrolled())
    }
}

impl<S: PskStore> Action for ControlPskService<'_, S> {
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(
            psk_action::SET_TCP_PSK,
            "set_tcp_psk",
            ActionAccess::Provision,
        )
        .args(&[FieldDescriptor::new(
            psk_field::PSK,
            "psk",
            ValueKind::Bytes,
        )])
        .secret()
        .secure_link()
        .results(&[FieldDescriptor::new(
            psk_field::FINGERPRINT,
            "fingerprint",
            ValueKind::Bytes,
        )]),
        ActionDescriptor::new(
            psk_action::CLEAR_TCP_PSK,
            "clear_tcp_psk",
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
            psk_action::SET_TCP_PSK => {
                let mut key = [0u8; PSK_LEN];
                let valid = args
                    .iter()
                    .find(|(id, _)| *id == psk_field::PSK)
                    .and_then(|(_, value)| value.as_bytes())
                    .filter(|bytes| bytes.len() == PSK_LEN)
                    .map(|bytes| key.copy_from_slice(bytes))
                    .is_some()
                    // A key of zeros is not a key anybody generated.
                    && key.iter().any(|byte| *byte != 0);
                if !valid {
                    key.zeroize();
                    log::warn!(target: "control_psk", "Key rejected: not 32 bytes of key material");
                    return Err(ActionError::InvalidArgument);
                }
                let stored = self.store.store(&key).await;
                let print = fingerprint(&key);
                key.zeroize();
                stored.map_err(|error| store_failed("store", error))?;
                log::info!(target: "control_psk", "TCP key set (fp={:02x}{:02x}{:02x}{:02x})", print[0], print[1], print[2], print[3]);
                out.bytes(psk_field::FINGERPRINT, &print)
                    .map_err(|_| ActionError::Internal)
            }
            psk_action::CLEAR_TCP_PSK => {
                self.store
                    .erase()
                    .await
                    .map_err(|error| store_failed("erase", error))?;
                log::info!(target: "control_psk", "TCP key removed");
                Ok(())
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cold]
#[inline(never)]
fn store_failed(operation: &'static str, error: PskStoreError) -> ActionError {
    log::warn!(target: "control_psk", "Store failed (op={} err={:?})", operation, error);
    match error {
        PskStoreError::Unavailable => ActionError::Unavailable,
        PskStoreError::Internal => ActionError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use embassy_futures::block_on;
    use rohi_service::{ActionRegistry, service_id};

    use super::*;

    struct Memory<'a> {
        access: &'a ControlAccess,
        fail: Cell<bool>,
        stored: Cell<Option<[u8; PSK_LEN]>>,
    }

    impl PskStore for &Memory<'_> {
        async fn store(&self, key: &[u8; PSK_LEN]) -> Result<(), PskStoreError> {
            if self.fail.get() {
                return Err(PskStoreError::Unavailable);
            }
            self.stored.set(Some(*key));
            self.access.set_tcp_psk(true);
            Ok(())
        }

        async fn erase(&self) -> Result<(), PskStoreError> {
            self.stored.set(None);
            self.access.set_tcp_psk(false);
            Ok(())
        }
    }

    fn memory(access: &ControlAccess) -> Memory<'_> {
        Memory {
            access,
            fail: Cell::new(false),
            stored: Cell::new(None),
        }
    }

    #[test]
    fn the_actions_declare_their_privileges() {
        let actions = <ControlPskService<&Memory> as Action>::ACTIONS;
        assert_eq!(actions[0].access, ActionAccess::Provision);
        assert!(actions[0].secret_args, "the key is wiped from the request");
        assert!(
            actions[0].secure_link,
            "the key never travels over an unprotected link"
        );
        assert_eq!(actions[1].access, ActionAccess::Mutate);
    }

    #[test]
    fn a_key_is_set_replaced_and_removed_and_each_change_is_a_new_generation() {
        let access = ControlAccess::new(|_| false);
        let memory = memory(&access);
        let service = ControlPskService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control_psk");
        let set =
            |key: [u8; 32]| block_on(registry.invoke(id, 1, &[(1, Value::bytes(&key).unwrap())]));
        set([1; 32]).unwrap();
        assert_eq!(memory.stored.get(), Some([1; 32]));
        let first = access.tcp_psk_epoch();
        set([2; 32]).unwrap();
        assert_eq!(memory.stored.get(), Some([2; 32]));
        assert_ne!(first, access.tcp_psk_epoch());
        block_on(registry.invoke(id, 2, &[])).unwrap();
        assert!(!access.tcp_psk_enrolled());
        assert_eq!(memory.stored.get(), None);
    }

    #[test]
    fn a_bad_key_is_refused_and_leaves_the_state_alone() {
        let access = ControlAccess::new(|_| false);
        let memory = memory(&access);
        let service = ControlPskService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control_psk");
        for bad in [&[1u8; 31][..], &[1u8; 33][..], &[0u8; 32][..], &[][..]] {
            assert_eq!(
                block_on(registry.invoke(id, 1, &[(1, Value::bytes(bad).unwrap())])),
                Err(ActionError::InvalidArgument)
            );
        }
        assert_eq!(
            block_on(registry.invoke(id, 1, &[])),
            Err(ActionError::InvalidArgument)
        );
        assert!(!access.tcp_psk_enrolled());
        memory.fail.set(true);
        assert_eq!(
            block_on(registry.invoke(id, 1, &[(1, Value::bytes(&[1; 32]).unwrap())])),
            Err(ActionError::Unavailable)
        );
    }

    #[test]
    fn the_status_never_shows_the_key_or_its_fingerprint() {
        struct Collect(std::vec::Vec<(u32, Value)>);
        impl StatusWriter for Collect {
            fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
                self.0.push((id, value));
                Ok(())
            }
        }
        let access = ControlAccess::new(|_| false);
        let memory = memory(&access);
        let service = ControlPskService::new(&access, &memory);
        let mut out = Collect(std::vec::Vec::new());
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.0, [(1, Value::Bool(false))]);
        access.set_tcp_psk(true);
        let mut out = Collect(std::vec::Vec::new());
        block_on(service.status(&mut out)).unwrap();
        // Read access to the status is open on a restricted endpoint: only the fact is public.
        assert_eq!(out.0, [(1, Value::Bool(true))]);
    }

    #[test]
    fn the_section_encoding_is_stable() {
        let section = TcpPskSection::new([0xab; 32]);
        let mut buffer = [0u8; 64];
        let used = rohi_config::encode_to_slice(&section, &mut buffer).unwrap();
        // array(1), bytes(32), key.
        assert_eq!(&buffer[..3], [0x81, 0x58, 0x20]);
        assert_eq!(used, 35);
        let back: TcpPskSection = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(back.key(), &[0xab; 32]);
    }
}
