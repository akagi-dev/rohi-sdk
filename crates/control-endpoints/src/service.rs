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
//! The `control_tcp` and `control_ble` services: the state of the endpoints, and the
//! pre-shared key of the TCP endpoint.

use core::net::Ipv4Addr;

use heapless::{String, Vec};
use rohi_control::{
    ControlAccess, EndpointSettings, EndpointState, EndpointStats, Transport, TransportAccessMode,
};
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor, EnumVariant,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES, Service, ServiceHealth,
    ServiceSummary, Status, StatusWriter, Value, ValueKind, WriteError,
};
use zeroize::Zeroize;

use crate::psk::{PSK_LEN, PskStore, PskStoreError, fingerprint};

/// Field ids of the `control_tcp` and `control_ble` status. Only ever added: the ids 9 to 11, 14
/// and 15 once showed a stored setting next to the running one and are retired, never reused.
/// The address, port and key fields are those of the TCP endpoint.
pub mod endpoint_field {
    /// `disabled`, `running` or `failed`.
    pub const STATE: u32 = 1;
    /// `open` or `restricted`, as enforced now.
    pub const ACCESS_MODE: u32 = 2;
    /// Live connections.
    pub const SESSIONS: u32 = 3;
    /// Requests the access policy allowed.
    pub const ACCEPTED: u32 = 4;
    /// Requests the access policy refused.
    pub const DENIED: u32 = 5;
    /// TCP handshakes that did not authenticate.
    pub const AUTH_FAILURES: u32 = 6;
    /// Framing, reassembly and I/O errors.
    pub const ERRORS: u32 = 7;
    /// `none`, `frame`, `transport` (I/O), `busy`, `auth`, `no_psk` or `other`: the last error.
    pub const LAST_ERROR: u32 = 8;
    /// The address the endpoint listens on.
    pub const ADDRESS: u32 = 12;
    /// The port the endpoint listens on.
    pub const PORT: u32 = 13;
    /// A pre-shared key is provisioned (TCP only).
    pub const PSK_ENROLLED: u32 = 16;
}

/// Action ids of the `control_tcp` service.
pub mod endpoint_action {
    /// Generates a new pre-shared key, stores it and returns it.
    pub const REVEAL_PSK: u32 = 1;
}

/// Result field ids of `control_tcp.reveal_psk`.
pub mod psk_result {
    /// The new 32-byte key.
    pub const PSK: u32 = 1;
    /// A fingerprint of the key, to tell later which key the device holds.
    pub const FINGERPRINT: u32 = 2;
}

/// Issue codes of the endpoint services.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// The endpoint could not start or stopped with an error.
    pub const FAILED: u32 = BASE;
    /// The endpoint is open: privileged requests need no proof.
    pub const OPEN: u32 = BASE + 1;
    /// No pre-shared key: a restricted TCP endpoint refuses every client until `reveal_psk` runs.
    pub const NOT_ENROLLED: u32 = BASE + 2;
}

const STATES: &[EnumDescriptor] = &[EnumDescriptor::new(
    1,
    &[
        EnumVariant::new(0, "disabled"),
        EnumVariant::new(1, "running"),
        EnumVariant::new(2, "failed"),
    ],
)];

const BLE_SCHEMA: &[FieldDescriptor] = &[
    FieldDescriptor::new(endpoint_field::STATE, "state", ValueKind::Enum).with_enum(1),
    FieldDescriptor::new(
        endpoint_field::ACCESS_MODE,
        "access_mode",
        ValueKind::String,
    ),
    FieldDescriptor::new(endpoint_field::SESSIONS, "sessions", ValueKind::U64),
    FieldDescriptor::new(endpoint_field::ACCEPTED, "accepted", ValueKind::U64),
    FieldDescriptor::new(endpoint_field::DENIED, "denied", ValueKind::U64),
    FieldDescriptor::new(
        endpoint_field::AUTH_FAILURES,
        "auth_failures",
        ValueKind::U64,
    ),
    FieldDescriptor::new(endpoint_field::ERRORS, "errors", ValueKind::U64),
    FieldDescriptor::new(endpoint_field::LAST_ERROR, "last_error", ValueKind::String),
];

const TCP_SCHEMA: &[FieldDescriptor] = &[
    BLE_SCHEMA[0],
    BLE_SCHEMA[1],
    BLE_SCHEMA[2],
    BLE_SCHEMA[3],
    BLE_SCHEMA[4],
    BLE_SCHEMA[5],
    BLE_SCHEMA[6],
    BLE_SCHEMA[7],
    FieldDescriptor::new(endpoint_field::ADDRESS, "address", ValueKind::String),
    FieldDescriptor::new(endpoint_field::PORT, "port", ValueKind::U64),
    FieldDescriptor::new(
        endpoint_field::PSK_ENROLLED,
        "psk_enrolled",
        ValueKind::Bool,
    ),
];

const ISSUES: &[IssueDescriptor] = &[
    IssueDescriptor::new(issue::FAILED, "failed"),
    IssueDescriptor::new(issue::OPEN, "open"),
    IssueDescriptor::new(issue::NOT_ENROLLED, "not_enrolled"),
];

fn error_name(kind: u8) -> &'static str {
    match kind {
        0 => "none",
        1 => "frame",
        2 => "transport",
        3 => "busy",
        4 => "auth",
        5 => "no_psk",
        _ => "other",
    }
}

fn summary(stats: &EndpointStats) -> ServiceSummary {
    match stats.state {
        EndpointState::Disabled => ServiceSummary::new(ServiceHealth::Unknown),
        EndpointState::Running => ServiceSummary::new(ServiceHealth::Ok),
        EndpointState::Failed => ServiceSummary::new(ServiceHealth::Error),
    }
}

fn issues(access: &ControlAccess, transport: Transport, out: &mut Vec<Issue, MAX_ISSUES>) {
    let stats = access.stats(transport);
    if stats.state == EndpointState::Failed {
        let _ = out.push(Issue {
            severity: IssueSeverity::Error,
            code: issue::FAILED,
            age_ms: None,
        });
    }
    let running = stats.state == EndpointState::Running;
    if running && access.mode(transport) == TransportAccessMode::Open {
        let _ = out.push(Issue {
            severity: IssueSeverity::Warning,
            code: issue::OPEN,
            age_ms: None,
        });
    }
    if running && transport == Transport::Tcp && !access.tcp_psk_enrolled() {
        let _ = out.push(Issue {
            severity: IssueSeverity::Info,
            code: issue::NOT_ENROLLED,
            age_ms: None,
        });
    }
}

fn address(settings: &EndpointSettings) -> String<15> {
    use core::fmt::Write as _;

    let mut text = String::new();
    let _ = write!(text, "{}", Ipv4Addr::from(settings.address));
    text
}

fn write_status(
    access: &ControlAccess,
    transport: Transport,
    out: &mut impl StatusWriter,
) -> Result<(), WriteError> {
    let stats = access.stats(transport);
    let state = match stats.state {
        EndpointState::Disabled => 0,
        EndpointState::Running => 1,
        EndpointState::Failed => 2,
    };
    out.variant(endpoint_field::STATE, state)?;
    out.text(endpoint_field::ACCESS_MODE, access.mode(transport).as_str())?;
    out.u64(endpoint_field::SESSIONS, stats.sessions.into())?;
    out.u64(endpoint_field::ACCEPTED, stats.accepted.into())?;
    out.u64(endpoint_field::DENIED, stats.denied.into())?;
    out.u64(endpoint_field::AUTH_FAILURES, stats.auth_failures.into())?;
    out.u64(endpoint_field::ERRORS, stats.errors.into())?;
    out.text(endpoint_field::LAST_ERROR, error_name(stats.last_error))?;
    if transport == Transport::Tcp {
        out.text(endpoint_field::ADDRESS, &address(&stats.settings))?;
        out.u64(endpoint_field::PORT, stats.settings.port.into())?;
        out.bool(endpoint_field::PSK_ENROLLED, access.tcp_psk_enrolled())?;
    }
    Ok(())
}

/// A key being handled: wiped when dropped, so that a cancelled action never leaves one behind.
struct Key([u8; PSK_LEN]);

impl Drop for Key {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The `control_tcp` service: the state of the TCP control endpoint and its pre-shared key.
///
/// Status: the state, the mode and the settings in use, the counters and `psk_enrolled`.
/// Action `reveal_psk` ([`Provision`](ActionAccess::Provision),
/// [`secure_link`](ActionDescriptor::secure_link)) **generates** a new random 32-byte key, stores
/// it, ends the TCP connections of the old key and returns the new one in the result. A client
/// never chooses the key and no action reads the current one: every call replaces it. The key is
/// in no status, no config and no log. Because the device generates the key, no secret has to be
/// sent to the device to provision it.
///
/// On an open BLE link the action runs without link encryption: that is the accepted insecure
/// state of this build (see the `open` issue of `control_ble`). Over TCP it needs the
/// authenticated and encrypted connection of the current key.
///
/// The service reads the [`ControlAccess`] the transport and the policy publish into and never
/// owns or calls the controller, so any transport can read it.
///
/// ```
/// use core::cell::Cell;
///
/// use embassy_futures::block_on;
/// use rohi_control::{ControlAccess, EndpointState, Transport};
/// use rohi_control_endpoints::{ControlTcpService, PSK_LEN, PskStore, PskStoreError};
/// use rohi_service::{ActionRegistry, ServiceHealth, Status, service_id};
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
/// }
///
/// fn entropy(dst: &mut [u8]) -> bool {
///     dst.fill(7);
///     true
/// }
///
/// let access = ControlAccess::new(entropy);
/// access.set_state(Transport::Tcp, EndpointState::Running);
/// let service = ControlTcpService::new(&access, Memory(&access, Cell::new([0; PSK_LEN])));
/// assert_eq!(block_on(service.summary()).health, ServiceHealth::Ok);
/// assert!(!access.tcp_psk_enrolled());
///
/// let registry = (&service,);
/// block_on(registry.invoke(service_id("control_tcp"), 1, &[])).unwrap();
/// assert!(access.tcp_psk_enrolled());
/// ```
#[derive(Service)]
#[service(name = "control_tcp")]
pub struct ControlTcpService<'a, S> {
    access: &'a ControlAccess,
    store: S,
}

impl<'a, S> ControlTcpService<'a, S> {
    /// The service over `access` and the key `store`.
    pub const fn new(access: &'a ControlAccess, store: S) -> Self {
        Self { access, store }
    }
}

impl<S: PskStore> Status for ControlTcpService<'_, S> {
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = TCP_SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = ISSUES;

    async fn summary(&self) -> ServiceSummary {
        summary(&self.access.stats(Transport::Tcp))
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        issues(self.access, Transport::Tcp, out);
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        write_status(self.access, Transport::Tcp, out)
    }
}

impl<S: PskStore> Action for ControlTcpService<'_, S> {
    const ACTIONS: &'static [ActionDescriptor] = &[ActionDescriptor::new(
        endpoint_action::REVEAL_PSK,
        "reveal_psk",
        ActionAccess::Provision,
    )
    .secure_link()
    .results(&[
        FieldDescriptor::new(psk_result::PSK, "psk", ValueKind::Bytes),
        FieldDescriptor::new(psk_result::FINGERPRINT, "fingerprint", ValueKind::Bytes),
    ])];

    async fn invoke(
        &self,
        action: u32,
        _args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            endpoint_action::REVEAL_PSK => {
                let mut key = Key([0; PSK_LEN]);
                // A key of zeros is not a key anybody generated; no entropy fails closed.
                if !self.access.fill_random(&mut key.0) || key.0.iter().all(|byte| *byte == 0) {
                    no_entropy();
                    return Err(ActionError::Unavailable);
                }
                self.store.store(&key.0).await.map_err(store_failed)?;
                let print = fingerprint(&key.0);
                log::info!(
                    target: "control_tcp",
                    "Key generated (fp={:02x}{:02x}{:02x}{:02x})",
                    print[0], print[1], print[2], print[3]
                );
                out.bytes(psk_result::PSK, &key.0)
                    .and_then(|()| out.bytes(psk_result::FINGERPRINT, &print))
                    .map_err(|_| ActionError::Internal)
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}

#[cold]
#[inline(never)]
fn no_entropy() {
    log::warn!(target: "control_tcp", "Key not generated: no random source");
}

#[cold]
#[inline(never)]
fn store_failed(error: PskStoreError) -> ActionError {
    log::warn!(target: "control_tcp", "Key not stored (err={:?})", error);
    match error {
        PskStoreError::Unavailable => ActionError::Unavailable,
        PskStoreError::Internal => ActionError::Internal,
    }
}

/// The `control_ble` service: the state of the BLE control endpoint, separate from the `ble`
/// service of the radio. The endpoint is open: no pairing and no link encryption in this build,
/// which the status reports as the `open` issue.
#[derive(Service, Clone, Copy)]
pub struct ControlBleService<'a> {
    access: &'a ControlAccess,
}

impl<'a> ControlBleService<'a> {
    /// The service over `access`.
    pub const fn new(access: &'a ControlAccess) -> Self {
        Self { access }
    }
}

impl Status for ControlBleService<'_> {
    const VERSION: u16 = 2;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = BLE_SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = ISSUES;

    async fn summary(&self) -> ServiceSummary {
        summary(&self.access.stats(Transport::Ble))
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        issues(self.access, Transport::Ble, out);
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        write_status(self.access, Transport::Ble, out)
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use embassy_futures::block_on;
    use rohi_service::{ActionRegistry, MAX_FIELDS, service_id};

    use super::*;

    #[derive(Default)]
    struct Collect(std::vec::Vec<(u32, Value)>);

    impl StatusWriter for Collect {
        fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
            self.0.push((id, value));
            Ok(())
        }
    }

    impl Collect {
        fn get(&self, id: u32) -> Option<&Value> {
            self.0
                .iter()
                .find(|(field, _)| *field == id)
                .map(|(_, v)| v)
        }
    }

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
    }

    fn new_memory(access: &ControlAccess) -> Memory<'_> {
        Memory {
            access,
            fail: Cell::new(false),
            stored: Cell::new(None),
        }
    }

    // Different bytes on every call, like a real source.
    fn counter(dst: &mut [u8]) -> bool {
        static NEXT: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(1);
        dst.fill(NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed));
        true
    }

    #[test]
    fn the_schemas_are_well_formed() {
        for schema in [TCP_SCHEMA, BLE_SCHEMA] {
            assert!(schema.len() <= MAX_FIELDS);
            let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), schema.len());
        }
        // The retired ids are never reused.
        for retired in [9, 10, 11, 14, 15] {
            assert!(TCP_SCHEMA.iter().all(|field| field.id != retired));
        }
        assert!(
            BLE_SCHEMA
                .iter()
                .all(|field| field.id < endpoint_field::ADDRESS)
        );
    }

    #[test]
    fn the_status_reports_only_what_is_in_use() {
        let access = ControlAccess::new(|_| false);
        access.set_settings(Transport::Tcp, EndpointSettings::TCP);
        access.set_state(Transport::Tcp, EndpointState::Running);
        let memory = new_memory(&access);
        let service = ControlTcpService::new(&access, &memory);

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(
            out.get(endpoint_field::ACCESS_MODE),
            Some(&Value::text("restricted").unwrap())
        );
        assert_eq!(out.get(endpoint_field::PORT), Some(&Value::U64(9300)));
        assert_eq!(
            out.get(endpoint_field::ADDRESS),
            Some(&Value::text("127.0.0.1").unwrap())
        );
        assert_eq!(
            out.get(endpoint_field::PSK_ENROLLED),
            Some(&Value::Bool(false))
        );
        // Nothing but the schema fields, and no stored-versus-running comparison.
        assert_eq!(out.0.len(), TCP_SCHEMA.len());
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert_eq!(issues[0].code, issue::NOT_ENROLLED);

        access.set_mode(Transport::Tcp, TransportAccessMode::Open);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert_eq!(issues[0].code, issue::OPEN);
    }

    #[test]
    fn the_ble_endpoint_is_separate_open_and_has_no_address() {
        let access = ControlAccess::new(|_| false);
        access.set_state(Transport::Ble, EndpointState::Running);
        let service = ControlBleService::new(&access);
        assert_eq!(<ControlBleService as Service>::NAME, "control_ble");
        assert_eq!(<ControlTcpService<&Memory> as Service>::NAME, "control_tcp");
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(endpoint_field::ADDRESS).is_none());
        assert!(out.get(endpoint_field::PSK_ENROLLED).is_none());
        assert_eq!(
            out.get(endpoint_field::ACCESS_MODE),
            Some(&Value::text("open").unwrap())
        );
        assert_eq!(block_on(service.summary()).health, ServiceHealth::Ok);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert_eq!(issues[0].code, issue::OPEN);
    }

    #[test]
    fn a_failed_endpoint_is_reported_as_failed() {
        let access = ControlAccess::new(|_| false);
        access.set_state(Transport::Ble, EndpointState::Failed);
        let service = ControlBleService::new(&access);
        assert_eq!(block_on(service.summary()).health, ServiceHealth::Error);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert_eq!(issues[0].code, issue::FAILED);
    }

    #[test]
    fn counters_and_the_last_error_are_reported() {
        let access = ControlAccess::new(|_| false);
        access.session_opened(Transport::Tcp);
        access.transport_error(Transport::Tcp, 1);
        let mut out = Collect::default();
        let memory = new_memory(&access);
        block_on(ControlTcpService::new(&access, &memory).status(&mut out)).unwrap();
        assert_eq!(out.get(endpoint_field::SESSIONS), Some(&Value::U64(1)));
        assert_eq!(out.get(endpoint_field::ERRORS), Some(&Value::U64(1)));
        assert_eq!(
            out.get(endpoint_field::LAST_ERROR),
            Some(&Value::text("frame").unwrap())
        );
    }

    #[test]
    fn reveal_psk_is_a_secure_link_provision_action_and_the_only_one() {
        let actions = <ControlTcpService<&Memory> as Action>::ACTIONS;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "reveal_psk");
        assert_eq!(actions[0].access, ActionAccess::Provision);
        assert!(actions[0].secure_link);
    }

    #[test]
    fn every_reveal_generates_a_new_key_and_a_new_generation() {
        let access = ControlAccess::new(counter);
        let memory = new_memory(&access);
        let service = ControlTcpService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control_tcp");
        let reveal = || block_on(registry.invoke(id, 1, &[]));
        reveal().unwrap();
        let first = memory.stored.get().unwrap();
        let epoch = access.tcp_psk_epoch();
        reveal().unwrap();
        let second = memory.stored.get().unwrap();
        assert_ne!(first, second);
        assert_ne!(epoch, access.tcp_psk_epoch());
        assert!(access.tcp_psk_enrolled());
    }

    #[test]
    fn no_key_is_made_without_entropy_or_a_store_and_nothing_changes() {
        let access = ControlAccess::new(|_| false);
        let memory = new_memory(&access);
        let service = ControlTcpService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control_tcp");
        assert_eq!(
            block_on(registry.invoke(id, 1, &[])),
            Err(ActionError::Unavailable)
        );
        assert!(!access.tcp_psk_enrolled());

        let access = ControlAccess::new(counter);
        let memory = new_memory(&access);
        memory.fail.set(true);
        let service = ControlTcpService::new(&access, &memory);
        assert_eq!(
            block_on((&service,).invoke(id, 1, &[])),
            Err(ActionError::Unavailable)
        );
        assert!(!access.tcp_psk_enrolled());
        assert_eq!(memory.stored.get(), None);
    }

    #[test]
    fn the_status_never_shows_the_key_or_its_fingerprint() {
        let access = ControlAccess::new(counter);
        let memory = new_memory(&access);
        let service = ControlTcpService::new(&access, &memory);
        block_on((&service,).invoke(service_id("control_tcp"), 1, &[])).unwrap();
        let key = memory.stored.get().unwrap();
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(
            out.get(endpoint_field::PSK_ENROLLED),
            Some(&Value::Bool(true))
        );
        for (_, value) in &out.0 {
            assert!(value.as_bytes().is_none_or(|bytes| bytes != key));
        }
    }
}
