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
//! The `control` service: the state of the control endpoints, and the pre-shared key of the TCP
//! endpoint.

use heapless::Vec;
use rohi_control::{Availability, ControlAccess, EndpointState, Transport, TransportAccessMode};
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor, EnumVariant,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES, Service, ServiceHealth,
    ServiceSummary, Status, StatusWriter, Value, ValueKind, WriteError,
};
use zeroize::Zeroize;

use crate::psk::{PSK_LEN, PskStore, PskStoreError, fingerprint};

/// Field ids of the `control` status. Only ever added, never reused.
pub mod control_field {
    /// Active BLE control connections.
    pub const BLE_SESSIONS: u32 = 1;
    /// Active TCP control connections.
    pub const TCP_SESSIONS: u32 = 2;
    /// `disabled`, `running` or `failed`: the lifecycle of the TCP endpoint.
    pub const TCP_STATE: u32 = 3;
    /// Requests the control worker handled since boot.
    pub const REQUESTS: u32 = 4;
    /// Control and transport failures since boot.
    pub const ERRORS: u32 = 5;
}

/// Action ids of the `control` service.
pub mod control_action {
    /// Generates a new pre-shared key, stores it and returns it.
    pub const TCP_GENERATE_PSK: u32 = 1;
}

/// Result field ids of `control.tcp_generate_psk`.
pub mod psk_result {
    /// The new 32-byte key.
    pub const PSK: u32 = 1;
    /// A fingerprint of the key, to tell later which key the device holds.
    pub const FINGERPRINT: u32 = 2;
}

/// Issue codes of the `control` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// An endpoint that the build has could not start or stopped with an error; the logs of its
    /// transport (`control.tcp`, `control.ble`) say why.
    pub const ENDPOINT_FAILED: u32 = BASE;
    /// A control path that needs no proof for privileged requests is available: the BLE endpoint,
    /// or the TCP endpoint in `open` mode.
    pub const UNSECURED: u32 = BASE + 1;
    /// No pre-shared key: a TCP client cannot authenticate until `tcp_generate_psk` runs.
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

const SCHEMA: &[FieldDescriptor] = &[
    FieldDescriptor::new(control_field::BLE_SESSIONS, "ble_sessions", ValueKind::U64),
    FieldDescriptor::new(control_field::TCP_SESSIONS, "tcp_sessions", ValueKind::U64),
    FieldDescriptor::new(control_field::TCP_STATE, "tcp_state", ValueKind::Enum).with_enum(1),
    FieldDescriptor::new(control_field::REQUESTS, "requests", ValueKind::U64),
    FieldDescriptor::new(control_field::ERRORS, "errors", ValueKind::U64),
];

const ISSUES: &[IssueDescriptor] = &[
    IssueDescriptor::new(issue::ENDPOINT_FAILED, "endpoint_failed"),
    IssueDescriptor::new(issue::UNSECURED, "unsecured"),
    IssueDescriptor::new(issue::NOT_ENROLLED, "not_enrolled"),
];

/// A key being handled: wiped when dropped, so that a cancelled action never leaves one behind.
struct Key([u8; PSK_LEN]);

impl Drop for Key {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The `control` service: the one public face of the control endpoints, BLE and TCP together, and
/// the pre-shared key of the TCP endpoint.
///
/// Status: the standard envelope plus exactly five fields: `ble_sessions`, `tcp_sessions`,
/// `tcp_state` (`disabled`, `running`, `failed`), `requests` and `errors`. Settings are config,
/// not status, so the access mode, the address, the port and whether a key exists are not shown.
/// `requests` counts the requests the control worker decoded, including those the access policy
/// refused; `errors` counts failures of the transports (framing, I/O, a handshake that did not
/// authenticate), every event once, and not an ordinary refusal.
///
/// Health follows whether the endpoints that the build has work, and nothing else: all of them
/// running is `ok`, some failed and some running `degraded`, expected but none running `error`. An
/// endpoint that the build does not have is no failure, and a platform announces its endpoints
/// with [`ControlAccess::expect`]. Current conditions are issues: `endpoint_failed`, `unsecured`
/// (a control path with no proof for privileged requests: BLE today, TCP in `open` mode) and
/// `not_enrolled`.
///
/// Action `tcp_generate_psk` ([`Provision`](ActionAccess::Provision),
/// [`secure_link`](ActionDescriptor::secure_link)) **generates** a new random 32-byte key, stores
/// it, ends the TCP connections of the old key and returns the new one in the result. A client
/// never chooses the key and no action reads the current one: every call replaces it. The key is
/// in no status, no config and no log. Because the device generates the key, no secret has to be
/// sent to the device to provision it.
///
/// On an open BLE link the action runs without link encryption: that is the accepted insecure
/// state of this build (see the `unsecured` issue). Over TCP it needs the authenticated and
/// encrypted connection of the current key, in every access mode.
///
/// The service reads the [`ControlAccess`] the transports and the policy publish into and never
/// owns or calls the controller, so any transport can feed it.
///
/// ```
/// use core::cell::Cell;
///
/// use embassy_futures::block_on;
/// use rohi_control::{ControlAccess, EndpointState, Transport};
/// use rohi_control_endpoints::{ControlService, PSK_LEN, PskStore, PskStoreError};
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
/// access.expect(Transport::Tcp);
/// access.set_state(Transport::Tcp, EndpointState::Running);
/// let service = ControlService::new(&access, Memory(&access, Cell::new([0; PSK_LEN])));
/// assert_eq!(block_on(service.summary()).health, ServiceHealth::Ok);
/// assert!(!access.tcp_psk_enrolled());
///
/// let registry = (&service,);
/// block_on(registry.invoke(service_id("control"), 1, &[])).unwrap();
/// assert!(access.tcp_psk_enrolled());
/// ```
#[derive(Service)]
#[service(name = "control")]
pub struct ControlService<'a, S> {
    access: &'a ControlAccess,
    store: S,
}

impl<'a, S> ControlService<'a, S> {
    /// The service over `access` and the key `store`.
    pub const fn new(access: &'a ControlAccess, store: S) -> Self {
        Self { access, store }
    }
}

impl<S: PskStore> Status for ControlService<'_, S> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = ISSUES;

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(match self.access.availability() {
            Availability::None | Availability::Starting => ServiceHealth::Unknown,
            Availability::All => ServiceHealth::Ok,
            Availability::Some => ServiceHealth::Degraded,
            Availability::Failed => ServiceHealth::Error,
        })
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let tcp = self.access.stats(Transport::Tcp);
        let ble = self.access.stats(Transport::Ble);
        let failed = |stats: &rohi_control::EndpointStats| {
            stats.expected && stats.state == EndpointState::Failed
        };
        if failed(&tcp) || failed(&ble) {
            let _ = out.push(Issue {
                severity: IssueSeverity::Error,
                code: issue::ENDPOINT_FAILED,
                age_ms: None,
            });
        }
        let tcp_running = tcp.state == EndpointState::Running;
        let ble_running = ble.state == EndpointState::Running;
        if ble_running || (tcp_running && tcp.settings.mode == TransportAccessMode::Open) {
            let _ = out.push(Issue {
                severity: IssueSeverity::Warning,
                code: issue::UNSECURED,
                age_ms: None,
            });
        }
        if tcp_running && !self.access.tcp_psk_enrolled() {
            let _ = out.push(Issue {
                severity: IssueSeverity::Info,
                code: issue::NOT_ENROLLED,
                age_ms: None,
            });
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let tcp = self.access.stats(Transport::Tcp);
        let ble = self.access.stats(Transport::Ble);
        out.u64(control_field::BLE_SESSIONS, ble.sessions.into())?;
        out.u64(control_field::TCP_SESSIONS, tcp.sessions.into())?;
        out.variant(
            control_field::TCP_STATE,
            match tcp.state {
                EndpointState::Disabled => 0,
                EndpointState::Running => 1,
                EndpointState::Failed => 2,
            },
        )?;
        out.u64(control_field::REQUESTS, self.access.requests().into())?;
        out.u64(control_field::ERRORS, self.access.errors().into())
    }
}

impl<S: PskStore> Action for ControlService<'_, S> {
    const ACTIONS: &'static [ActionDescriptor] = &[ActionDescriptor::new(
        control_action::TCP_GENERATE_PSK,
        "tcp_generate_psk",
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
            control_action::TCP_GENERATE_PSK => {
                let mut key = Key([0; PSK_LEN]);
                // A key of zeros is not a key anybody generated; no entropy fails closed.
                if !self.access.fill_random(&mut key.0) || key.0.iter().all(|byte| *byte == 0) {
                    no_entropy();
                    return Err(ActionError::Unavailable);
                }
                self.store.store(&key.0).await.map_err(store_failed)?;
                let print = fingerprint(&key.0);
                log::info!(
                    target: "control",
                    "TCP key generated (fp={:02x}{:02x}{:02x}{:02x})",
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
    log::warn!(target: "control", "TCP key not generated: no random source");
}

#[cold]
#[inline(never)]
fn store_failed(error: PskStoreError) -> ActionError {
    log::warn!(target: "control", "TCP key not stored (err={:?})", error);
    match error {
        PskStoreError::Unavailable => ActionError::Unavailable,
        PskStoreError::Internal => ActionError::Internal,
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

    fn health(access: &ControlAccess) -> ServiceHealth {
        let memory = new_memory(access);
        block_on(ControlService::new(access, &memory).summary()).health
    }

    fn issues(access: &ControlAccess) -> std::vec::Vec<u32> {
        let memory = new_memory(access);
        let mut out = Vec::new();
        block_on(ControlService::new(access, &memory).issues(&mut out));
        out.iter().map(|issue| issue.code).collect()
    }

    #[test]
    fn the_schema_is_exactly_five_fields_and_fits_the_limits() {
        assert_eq!(SCHEMA.len(), 5);
        assert!(SCHEMA.len() <= MAX_FIELDS);
        let names: std::vec::Vec<_> = SCHEMA.iter().map(|field| field.name).collect();
        assert_eq!(
            names,
            [
                "ble_sessions",
                "tcp_sessions",
                "tcp_state",
                "requests",
                "errors"
            ]
        );
        let mut ids: std::vec::Vec<_> = SCHEMA.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SCHEMA.len());
        assert_eq!(ISSUES.len(), 3);
        assert!(ISSUES.len() <= MAX_ISSUES);
        assert_eq!(<ControlService<&Memory> as Service>::NAME, "control");
    }

    #[test]
    fn the_status_has_those_five_fields_and_no_setting() {
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Tcp);
        access.set_state(Transport::Tcp, EndpointState::Running);
        access.session_opened(Transport::Tcp);
        access.session_opened(Transport::Ble);
        access.session_opened(Transport::Ble);
        let memory = new_memory(&access);
        let service = ControlService::new(&access, &memory);
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.0.len(), 5);
        assert_eq!(out.get(control_field::BLE_SESSIONS), Some(&Value::U64(2)));
        assert_eq!(out.get(control_field::TCP_SESSIONS), Some(&Value::U64(1)));
        assert_eq!(out.get(control_field::TCP_STATE), Some(&Value::Enum(1)));
        assert_eq!(out.get(control_field::REQUESTS), Some(&Value::U64(0)));
        assert_eq!(out.get(control_field::ERRORS), Some(&Value::U64(0)));
    }

    #[test]
    fn errors_are_one_count_over_the_transports() {
        let access = ControlAccess::new(|_| false);
        access.transport_error(Transport::Tcp, 4);
        access.transport_error(Transport::Ble, 1);
        let memory = new_memory(&access);
        let mut out = Collect::default();
        block_on(ControlService::new(&access, &memory).status(&mut out)).unwrap();
        assert_eq!(out.get(control_field::ERRORS), Some(&Value::U64(2)));
    }

    #[test]
    fn tcp_state_does_not_depend_on_the_key_or_the_mode() {
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Tcp);
        access.set_state(Transport::Tcp, EndpointState::Running);
        let memory = new_memory(&access);
        let service = ControlService::new(&access, &memory);
        for mode in [
            TransportAccessMode::Open,
            TransportAccessMode::Protected,
            TransportAccessMode::Private,
        ] {
            for enrolled in [false, true] {
                access.set_mode(Transport::Tcp, mode);
                access.set_tcp_psk(enrolled);
                let mut out = Collect::default();
                block_on(service.status(&mut out)).unwrap();
                assert_eq!(out.get(control_field::TCP_STATE), Some(&Value::Enum(1)));
                assert_eq!(health(&access), ServiceHealth::Ok, "{mode:?} {enrolled}");
            }
        }
    }

    #[test]
    fn a_build_without_tcp_reports_it_disabled() {
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Ble);
        access.set_state(Transport::Ble, EndpointState::Running);
        let memory = new_memory(&access);
        let mut out = Collect::default();
        block_on(ControlService::new(&access, &memory).status(&mut out)).unwrap();
        assert_eq!(out.get(control_field::TCP_STATE), Some(&Value::Enum(0)));
        // The absent endpoint is no failure.
        assert_eq!(health(&access), ServiceHealth::Ok);
    }

    #[test]
    fn the_health_follows_the_endpoints_the_build_has() {
        use EndpointState::{Failed, Running};

        // Nothing announced, or still starting: unknown.
        let access = ControlAccess::new(|_| false);
        assert_eq!(health(&access), ServiceHealth::Unknown);
        access.expect(Transport::Tcp);
        assert_eq!(health(&access), ServiceHealth::Unknown);

        // TCP only.
        access.set_state(Transport::Tcp, Running);
        assert_eq!(health(&access), ServiceHealth::Ok);
        access.set_state(Transport::Tcp, Failed);
        assert_eq!(health(&access), ServiceHealth::Error);

        // BLE only.
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Ble);
        access.set_state(Transport::Ble, Running);
        assert_eq!(health(&access), ServiceHealth::Ok);
        access.set_state(Transport::Ble, Failed);
        assert_eq!(health(&access), ServiceHealth::Error);

        // Both.
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Tcp);
        access.expect(Transport::Ble);
        access.set_state(Transport::Tcp, Running);
        access.set_state(Transport::Ble, Running);
        assert_eq!(health(&access), ServiceHealth::Ok);
        access.set_state(Transport::Tcp, Failed);
        assert_eq!(health(&access), ServiceHealth::Degraded);
        access.set_state(Transport::Ble, Failed);
        assert_eq!(health(&access), ServiceHealth::Error);

        // Sessions, counters and past errors do not move it.
        access.set_state(Transport::Tcp, Running);
        access.set_state(Transport::Ble, Running);
        access.session_opened(Transport::Tcp);
        access.transport_error(Transport::Tcp, 4);
        assert_eq!(health(&access), ServiceHealth::Ok);
    }

    #[test]
    fn the_issues_name_current_conditions_and_fit_the_limit() {
        // TCP only, protected, no key.
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Tcp);
        access.set_state(Transport::Tcp, EndpointState::Running);
        assert_eq!(issues(&access), [issue::NOT_ENROLLED]);
        access.set_tcp_psk(true);
        assert!(issues(&access).is_empty());
        access.set_mode(Transport::Tcp, TransportAccessMode::Open);
        assert_eq!(issues(&access), [issue::UNSECURED]);

        // BLE only: open by design, reported as a warning, not as a failure.
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Ble);
        access.set_state(Transport::Ble, EndpointState::Running);
        assert_eq!(issues(&access), [issue::UNSECURED]);
        assert_eq!(health(&access), ServiceHealth::Ok);

        // Failed start: the generic issue, whatever the transport.
        access.set_state(Transport::Ble, EndpointState::Failed);
        assert_eq!(issues(&access), [issue::ENDPOINT_FAILED]);

        // The worst combination stays inside the limit: one failed, the other open and no key.
        let access = ControlAccess::new(|_| false);
        access.expect(Transport::Tcp);
        access.expect(Transport::Ble);
        access.set_mode(Transport::Tcp, TransportAccessMode::Open);
        access.set_state(Transport::Tcp, EndpointState::Running);
        access.set_state(Transport::Ble, EndpointState::Failed);
        let all = issues(&access);
        assert_eq!(
            all,
            [
                issue::ENDPOINT_FAILED,
                issue::UNSECURED,
                issue::NOT_ENROLLED
            ]
        );
        assert!(all.len() <= MAX_ISSUES);
    }

    #[test]
    fn tcp_generate_psk_is_a_secure_link_provision_action_and_the_only_one() {
        let actions = <ControlService<&Memory> as Action>::ACTIONS;
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, 1);
        assert_eq!(actions[0].name, "tcp_generate_psk");
        assert_eq!(actions[0].access, ActionAccess::Provision);
        assert!(actions[0].secure_link);
        assert!(actions[0].args.is_empty());
        assert_eq!(actions[0].results.len(), 2);
    }

    #[test]
    fn every_call_generates_a_new_key_and_a_new_generation() {
        let access = ControlAccess::new(counter);
        let memory = new_memory(&access);
        let service = ControlService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control");
        let generate = || block_on(registry.invoke(id, 1, &[]));
        generate().unwrap();
        let first = memory.stored.get().unwrap();
        let epoch = access.tcp_psk_epoch();
        generate().unwrap();
        let second = memory.stored.get().unwrap();
        assert_ne!(first, second);
        assert_ne!(epoch, access.tcp_psk_epoch());
        assert!(access.tcp_psk_enrolled());
    }

    #[test]
    fn no_key_is_made_without_entropy_or_a_store_and_nothing_changes() {
        let access = ControlAccess::new(|_| false);
        let memory = new_memory(&access);
        let service = ControlService::new(&access, &memory);
        let registry = (&service,);
        let id = service_id("control");
        assert_eq!(
            block_on(registry.invoke(id, 1, &[])),
            Err(ActionError::Unavailable)
        );
        assert!(!access.tcp_psk_enrolled());

        let access = ControlAccess::new(counter);
        let memory = new_memory(&access);
        memory.fail.set(true);
        let service = ControlService::new(&access, &memory);
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
        let service = ControlService::new(&access, &memory);
        block_on((&service,).invoke(service_id("control"), 1, &[])).unwrap();
        let key = memory.stored.get().unwrap();
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert_eq!(out.0.len(), 5);
        for (_, value) in &out.0 {
            assert!(value.as_bytes().is_none_or(|bytes| bytes != key));
        }
    }
}
