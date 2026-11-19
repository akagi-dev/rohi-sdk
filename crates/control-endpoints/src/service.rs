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
//! The `control_tcp` and `control_ble` services: the state of the endpoints.

use core::net::Ipv4Addr;

use heapless::{String, Vec};
use rohi_control::{
    ControlAccess, EndpointSettings, EndpointState, EndpointStats, Transport, TransportAccessMode,
};
use rohi_service::{
    EnumDescriptor, EnumVariant, FieldDescriptor, Issue, IssueDescriptor, IssueSeverity,
    MAX_ISSUES, Service, ServiceHealth, ServiceState, ServiceSummary, Status, StatusWriter,
    ValueKind, WriteError,
};

/// Field ids of the `control_tcp` and `control_ble` status. Only ever added;
/// the address and port fields are those of a TCP endpoint.
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
    /// Signed requests that were rejected.
    pub const AUTH_FAILURES: u32 = 6;
    /// Framing, reassembly and I/O errors.
    pub const ERRORS: u32 = 7;
    /// `none`, `frame`, `transport` (I/O) or `busy`: the last error.
    pub const LAST_ERROR: u32 = 8;
    /// A stored setting waits for a restart.
    pub const RESTART_REQUIRED: u32 = 9;
    /// What the running endpoint was started with: switched on.
    pub const ENABLED: u32 = 10;
    /// Stored setting: switched on.
    pub const DESIRED_ENABLED: u32 = 11;
    /// The address the endpoint listens on.
    pub const ADDRESS: u32 = 12;
    /// The port the endpoint listens on.
    pub const PORT: u32 = 13;
    /// Stored setting: address.
    pub const DESIRED_ADDRESS: u32 = 14;
    /// Stored setting: port.
    pub const DESIRED_PORT: u32 = 15;
}

/// Issue codes of the endpoint services.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// The endpoint could not start or stopped with an error.
    pub const FAILED: u32 = BASE;
    /// The endpoint is open: privileged requests need no proof.
    pub const OPEN: u32 = BASE + 1;
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
    FieldDescriptor::new(
        endpoint_field::RESTART_REQUIRED,
        "restart_required",
        ValueKind::Bool,
    ),
    FieldDescriptor::new(endpoint_field::ENABLED, "enabled", ValueKind::Bool),
    FieldDescriptor::new(
        endpoint_field::DESIRED_ENABLED,
        "desired_enabled",
        ValueKind::Bool,
    ),
    FieldDescriptor::new(endpoint_field::ADDRESS, "address", ValueKind::String),
    FieldDescriptor::new(endpoint_field::PORT, "port", ValueKind::U64),
    FieldDescriptor::new(
        endpoint_field::DESIRED_ADDRESS,
        "desired_address",
        ValueKind::String,
    ),
    FieldDescriptor::new(endpoint_field::DESIRED_PORT, "desired_port", ValueKind::U64),
];

// The BLE endpoint has no address or port.
const BLE_SCHEMA: &[FieldDescriptor] = SCHEMA.split_at(11).0;

const ISSUES: &[IssueDescriptor] = &[
    IssueDescriptor::new(issue::FAILED, "failed"),
    IssueDescriptor::new(issue::OPEN, "open"),
];

fn mode_name(mode: TransportAccessMode) -> &'static str {
    mode.as_str()
}

fn error_name(kind: u8) -> &'static str {
    match kind {
        0 => "none",
        1 => "frame",
        2 => "transport",
        3 => "busy",
        _ => "other",
    }
}

fn summary(stats: &EndpointStats) -> ServiceSummary {
    match stats.state {
        EndpointState::Disabled => {
            ServiceSummary::new(ServiceState::Disabled, ServiceHealth::Unknown, false)
        }
        EndpointState::Running => {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
        }
        EndpointState::Failed => {
            ServiceSummary::new(ServiceState::Failed, ServiceHealth::Error, false)
        }
    }
}

fn issues(stats: &EndpointStats, out: &mut Vec<Issue, MAX_ISSUES>) {
    if stats.state == EndpointState::Failed {
        let _ = out.push(Issue {
            severity: IssueSeverity::Error,
            code: issue::FAILED,
            age_ms: None,
        });
    }
    if stats.state == EndpointState::Running && stats.effective.mode == TransportAccessMode::Open {
        let _ = out.push(Issue {
            severity: IssueSeverity::Warning,
            code: issue::OPEN,
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
    stats: &EndpointStats,
    tcp: bool,
    out: &mut impl StatusWriter,
) -> Result<(), WriteError> {
    let state = match stats.state {
        EndpointState::Disabled => 0,
        EndpointState::Running => 1,
        EndpointState::Failed => 2,
    };
    out.variant(endpoint_field::STATE, state)?;
    out.text(endpoint_field::ACCESS_MODE, mode_name(stats.effective.mode))?;
    out.u64(endpoint_field::SESSIONS, stats.sessions.into())?;
    out.u64(endpoint_field::ACCEPTED, stats.accepted.into())?;
    out.u64(endpoint_field::DENIED, stats.denied.into())?;
    out.u64(endpoint_field::AUTH_FAILURES, stats.auth_failures.into())?;
    out.u64(endpoint_field::ERRORS, stats.errors.into())?;
    out.text(endpoint_field::LAST_ERROR, error_name(stats.last_error))?;
    out.bool(endpoint_field::RESTART_REQUIRED, stats.restart_required())?;
    out.bool(endpoint_field::ENABLED, stats.effective.enabled)?;
    out.bool(endpoint_field::DESIRED_ENABLED, stats.desired.enabled)?;
    if tcp {
        out.text(endpoint_field::ADDRESS, &address(&stats.effective))?;
        out.u64(endpoint_field::PORT, stats.effective.port.into())?;
        out.text(endpoint_field::DESIRED_ADDRESS, &address(&stats.desired))?;
        out.u64(endpoint_field::DESIRED_PORT, stats.desired.port.into())?;
    }
    Ok(())
}

/// The `control_tcp` service: the state of the TCP control endpoint.
///
/// It reads the [`ControlAccess`] the transport and the policy publish into
/// and never owns or calls the controller, so any transport can read it.
///
/// ```
/// use embassy_futures::block_on;
/// use rohi_control::{ControlAccess, EndpointState, Transport};
/// use rohi_control_endpoints::ControlTcpService;
/// use rohi_service::{ServiceState, Status};
///
/// let access = ControlAccess::new(|_| false);
/// access.set_state(Transport::Tcp, EndpointState::Running);
/// let service = ControlTcpService::new(&access);
/// assert_eq!(block_on(service.summary()).state, ServiceState::Running);
/// ```
#[derive(Service, Clone, Copy)]
pub struct ControlTcpService<'a> {
    access: &'a ControlAccess,
}

impl<'a> ControlTcpService<'a> {
    /// The service over `access`.
    pub const fn new(access: &'a ControlAccess) -> Self {
        Self { access }
    }
}

impl Status for ControlTcpService<'_> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = ISSUES;

    async fn summary(&self) -> ServiceSummary {
        summary(&self.access.stats(Transport::Tcp))
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        issues(&self.access.stats(Transport::Tcp), out);
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        write_status(&self.access.stats(Transport::Tcp), true, out)
    }
}

/// The `control_ble` service: the state of the BLE control endpoint, separate
/// from the `ble` service of the radio.
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
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = BLE_SCHEMA;
    const ENUMS: &'static [EnumDescriptor] = STATES;
    const ISSUES: &'static [IssueDescriptor] = ISSUES;

    async fn summary(&self) -> ServiceSummary {
        summary(&self.access.stats(Transport::Ble))
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        issues(&self.access.stats(Transport::Ble), out);
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        write_status(&self.access.stats(Transport::Ble), false, out)
    }
}

#[cfg(test)]
mod tests {
    use embassy_futures::block_on;
    use rohi_service::{MAX_FIELDS, Value};

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

    #[test]
    fn the_schemas_are_well_formed() {
        for schema in [SCHEMA, BLE_SCHEMA] {
            assert!(schema.len() <= MAX_FIELDS);
            let mut ids: std::vec::Vec<_> = schema.iter().map(|field| field.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), schema.len());
        }
        assert!(
            BLE_SCHEMA
                .iter()
                .all(|field| field.id < endpoint_field::ADDRESS)
        );
    }

    #[test]
    fn the_tcp_status_reports_the_effective_and_the_pending_settings() {
        let access = ControlAccess::new(|_| false);
        access.set_effective(Transport::Tcp, EndpointSettings::TCP);
        access.set_state(Transport::Tcp, EndpointState::Running);
        let mut desired = EndpointSettings::TCP;
        desired.port = 9400;
        desired.mode = TransportAccessMode::Open;
        access.set_desired(Transport::Tcp, desired);
        let service = ControlTcpService::new(&access);

        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        // The mode applies at once, the port waits for a restart.
        assert_eq!(
            out.get(endpoint_field::ACCESS_MODE),
            Some(&Value::text("open").unwrap())
        );
        assert_eq!(out.get(endpoint_field::PORT), Some(&Value::U64(9300)));
        assert_eq!(
            out.get(endpoint_field::DESIRED_PORT),
            Some(&Value::U64(9400))
        );
        assert_eq!(
            out.get(endpoint_field::RESTART_REQUIRED),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            out.get(endpoint_field::ADDRESS),
            Some(&Value::text("127.0.0.1").unwrap())
        );
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        assert_eq!(issues[0].code, issue::OPEN);
    }

    #[test]
    fn the_ble_endpoint_is_separate_and_has_no_address() {
        let access = ControlAccess::new(|_| false);
        access.set_state(Transport::Ble, EndpointState::Running);
        let service = ControlBleService::new(&access);
        assert_eq!(<ControlBleService as Service>::NAME, "control_ble");
        assert_eq!(<ControlTcpService as Service>::NAME, "control_tcp");
        let mut out = Collect::default();
        block_on(service.status(&mut out)).unwrap();
        assert!(out.get(endpoint_field::ADDRESS).is_none());
        assert_eq!(
            out.get(endpoint_field::ACCESS_MODE),
            Some(&Value::text("open").unwrap())
        );
        assert_eq!(block_on(service.summary()).state, ServiceState::Running);
    }

    #[test]
    fn counters_and_the_last_error_are_reported() {
        let access = ControlAccess::new(|_| false);
        access.session_opened(Transport::Tcp);
        access.transport_error(Transport::Tcp, 1);
        let mut out = Collect::default();
        block_on(ControlTcpService::new(&access).status(&mut out)).unwrap();
        assert_eq!(out.get(endpoint_field::SESSIONS), Some(&Value::U64(1)));
        assert_eq!(out.get(endpoint_field::ERRORS), Some(&Value::U64(1)));
        assert_eq!(
            out.get(endpoint_field::LAST_ERROR),
            Some(&Value::text("frame").unwrap())
        );
    }
}
