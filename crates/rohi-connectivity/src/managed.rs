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
//! The telemetry configuration and status in the generic management plane.

use core::fmt::Write as _;

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;
use rohi_location::LocationSource;
use rohi_service::{
    Bounds, ConfigField, FieldDescriptor, FieldError, Issue, IssueDescriptor, IssueSeverity,
    MAX_ISSUES, ManagedConfig, ManagedService, ServiceHealth, ServiceState, ServiceSummary,
    StatusWriter, Unit, Value, ValueKind, WriteError,
};

use crate::config::{MAX_HOST_LEN, MAX_INTERVAL_S, MIN_INTERVAL_S};
use crate::service::{ConnectivityShared, ConnectivityState};
use crate::transport::TransportError;
use crate::{ConnectivityConfig, Host};

/// Field ids of the `connectivity` config section.
pub mod field {
    /// Telemetry is only produced and sent when enabled.
    pub const ENABLED: u32 = 1;
    /// Robonomics CPS node identifier.
    pub const NODE_ID: u32 = 2;
    /// Seconds between telemetry frames.
    pub const INTERVAL_S: u32 = 3;
    /// Ingress host name or IPv4 address.
    pub const HOST: u32 = 4;
    /// Ingress TCP port.
    pub const PORT: u32 = 5;
}

impl ManagedConfig for ConnectivityConfig {
    const SECTION_ID: u64 = <Self as rohi_config::ConfigSection>::ID.0;
    const NAME: &'static str = "connectivity";
    const VERSION: u16 = <Self as rohi_config::ConfigSection>::VERSION;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::ENABLED, "enabled", ValueKind::Bool),
        ConfigField::new(field::NODE_ID, "node_id", ValueKind::U64),
        ConfigField::new(field::INTERVAL_S, "interval_s", ValueKind::U64)
            .unit(Unit::S)
            .bounds(Bounds::U64(MIN_INTERVAL_S as u64, MAX_INTERVAL_S as u64)),
        ConfigField::new(field::HOST, "host", ValueKind::String),
        ConfigField::new(field::PORT, "port", ValueKind::U64).bounds(Bounds::U64(1, 65535)),
    ];

    fn defaults() -> Self {
        Self::disabled()
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        Ok(Some(match id {
            field::ENABLED => Value::Bool(self.enabled),
            field::NODE_ID => Value::U64(self.node_id),
            field::INTERVAL_S => Value::U64(u64::from(self.interval_s)),
            field::HOST => Value::text(self.host.as_str()).ok_or(FieldError::Invalid)?,
            field::PORT => Value::U64(u64::from(self.port)),
            _ => return Err(FieldError::UnknownField),
        }))
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        match id {
            field::ENABLED => self.enabled = value.as_bool().ok_or(FieldError::WrongKind)?,
            field::NODE_ID => self.node_id = value.as_u64().ok_or(FieldError::WrongKind)?,
            field::INTERVAL_S => {
                let seconds = value.as_u64().ok_or(FieldError::WrongKind)?;
                self.interval_s = u32::try_from(seconds).map_err(|_| FieldError::OutOfRange)?;
            }
            field::HOST => {
                let host = value.as_str().ok_or(FieldError::WrongKind)?;
                self.host = Host::new(host).ok_or(FieldError::Invalid)?;
            }
            field::PORT => {
                let port = value.as_u64().ok_or(FieldError::WrongKind)?;
                self.port = u16::try_from(port).map_err(|_| FieldError::OutOfRange)?;
            }
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::ENABLED..=field::PORT => Err(FieldError::NotOptional),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn validate(&self) -> Result<(), FieldError> {
        ConnectivityConfig::validate(self).map_err(|_| FieldError::Invalid)
    }
}

/// Id of the `connectivity` service.
pub const SERVICE_ID: u32 = 6;

/// Issue codes of the `connectivity` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// The network cannot carry traffic.
    pub const NETWORK_UNAVAILABLE: u32 = BASE;
    /// Name resolution failed.
    pub const DNS_FAILED: u32 = BASE + 1;
    /// The connection failed.
    pub const CONNECT_FAILED: u32 = BASE + 2;
    /// The exchange timed out.
    pub const CONNECTION_TIMEOUT: u32 = BASE + 3;
    /// The connection closed unexpectedly.
    pub const IO_ERROR: u32 = BASE + 4;
    /// The response is invalid.
    pub const BAD_RESPONSE: u32 = BASE + 5;
    /// The ingress rejected the frame.
    pub const DELIVERY_REJECTED: u32 = BASE + 6;
    /// The device has no identity yet.
    pub const NO_IDENTITY: u32 = BASE + 7;
    /// UTC time is not synchronized yet.
    pub const NO_UTC_TIME: u32 = BASE + 8;
    /// The ingress does not authorize the device.
    pub const UNAUTHORIZED: u32 = BASE + 9;
    /// No location is set; telemetry is sent without it.
    pub const NO_LOCATION: u32 = BASE + 10;
}

/// Field ids of the `connectivity` service status.
pub mod status_field {
    /// Frames signed and queued.
    pub const FRAMES_CREATED: u32 = 1;
    /// Frames accepted by the ingress.
    pub const FRAMES_DELIVERED: u32 = 2;
    /// Delivery attempts that failed and will be retried.
    pub const SEND_RETRIES: u32 = 3;
    /// Frames dropped after a permanent rejection.
    pub const FRAMES_REJECTED: u32 = 4;
    /// Frames waiting for delivery.
    pub const QUEUE_DEPTH: u32 = 5;
    /// Ingress host of the effective configuration.
    pub const HOST: u32 = 6;
    /// Frames created without a location.
    pub const FRAMES_WITHOUT_LOCATION: u32 = 7;
    /// Ingress port of the effective configuration.
    pub const PORT: u32 = 8;
}

fn warning(code: u32) -> Issue {
    Issue {
        severity: IssueSeverity::Warning,
        code,
        age_ms: None,
    }
}

fn transport_issue(error: TransportError) -> Issue {
    match error {
        TransportError::NetworkUnavailable => warning(issue::NETWORK_UNAVAILABLE),
        TransportError::Dns => warning(issue::DNS_FAILED),
        TransportError::Connect => warning(issue::CONNECT_FAILED),
        TransportError::Timeout => warning(issue::CONNECTION_TIMEOUT),
        TransportError::Io => warning(issue::IO_ERROR),
        TransportError::BadResponse => warning(issue::BAD_RESPONSE),
        TransportError::Status(_) => warning(issue::DELIVERY_REJECTED),
    }
}

/// The telemetry service as a `connectivity` service of the status plane.
///
/// `effective` returns the configuration the service currently runs, or `None`
/// while it is disabled or invalid; a disabled service is not an error. The
/// endpoint of that configuration is part of the status.
///
/// Telemetry without a location still flows but is incomplete: while the
/// device does not know where it is, an otherwise healthy service is
/// `degraded` with the issue `no_location`.
pub struct ConnectivityStatusService<'a, M: RawMutex> {
    shared: &'a ConnectivityShared<M>,
    effective: fn() -> Option<ConnectivityConfig>,
    location: &'a dyn LocationSource,
}

impl<'a, M: RawMutex> ConnectivityStatusService<'a, M> {
    /// Status of the service publishing through `shared`.
    pub const fn new(
        shared: &'a ConnectivityShared<M>,
        effective: fn() -> Option<ConnectivityConfig>,
        location: &'a dyn LocationSource,
    ) -> Self {
        Self {
            shared,
            effective,
            location,
        }
    }

    /// Adds the `no_location` issue while the device does not know where it
    /// is.
    fn no_location(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if self.location.location().is_none() {
            let _ = out.push(warning(issue::NO_LOCATION));
        }
    }
}

impl<M: RawMutex> ManagedService for ConnectivityStatusService<'_, M> {
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "connectivity";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(
            status_field::FRAMES_CREATED,
            "frames_created",
            ValueKind::U64,
        ),
        FieldDescriptor::new(
            status_field::FRAMES_DELIVERED,
            "frames_delivered",
            ValueKind::U64,
        ),
        FieldDescriptor::new(status_field::SEND_RETRIES, "send_retries", ValueKind::U64),
        FieldDescriptor::new(
            status_field::FRAMES_REJECTED,
            "frames_rejected",
            ValueKind::U64,
        ),
        FieldDescriptor::new(status_field::QUEUE_DEPTH, "queue_depth", ValueKind::U64),
        FieldDescriptor::new(status_field::HOST, "host", ValueKind::String),
        FieldDescriptor::new(
            status_field::FRAMES_WITHOUT_LOCATION,
            "frames_without_location",
            ValueKind::U64,
        ),
    ];
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::NETWORK_UNAVAILABLE, "network_unavailable"),
        IssueDescriptor::new(issue::DNS_FAILED, "dns_failed"),
        IssueDescriptor::new(issue::CONNECT_FAILED, "connect_failed"),
        IssueDescriptor::new(issue::CONNECTION_TIMEOUT, "connection_timeout"),
        IssueDescriptor::new(issue::IO_ERROR, "io_error"),
        IssueDescriptor::new(issue::BAD_RESPONSE, "bad_response"),
        IssueDescriptor::new(issue::DELIVERY_REJECTED, "delivery_rejected"),
        IssueDescriptor::new(issue::NO_IDENTITY, "no_identity"),
        IssueDescriptor::new(issue::NO_UTC_TIME, "no_utc_time"),
        IssueDescriptor::new(issue::UNAUTHORIZED, "unauthorized"),
        IssueDescriptor::new(issue::NO_LOCATION, "no_location"),
    ];
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        use ServiceHealth as H;
        use ServiceState as S;
        if (self.effective)().is_none() {
            return ServiceSummary::new(S::Disabled, H::Unknown, false);
        }
        match self.shared.status().state {
            ConnectivityState::Starting => ServiceSummary::new(S::Starting, H::Unknown, false),
            // Frames are sent, only the location is missing.
            ConnectivityState::Idle if self.location.location().is_none() => {
                ServiceSummary::new(S::Running, H::Degraded, true)
            }
            ConnectivityState::Idle => ServiceSummary::new(S::Running, H::Ok, true),
            ConnectivityState::WaitingForIdentity
            | ConnectivityState::WaitingForUtc
            | ConnectivityState::Backoff(_) => ServiceSummary::new(S::Running, H::Degraded, false),
            ConnectivityState::Unauthorized(_) => ServiceSummary::new(S::Running, H::Error, false),
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if (self.effective)().is_none() {
            return;
        }
        let issue = match self.shared.status().state {
            ConnectivityState::Starting => return,
            ConnectivityState::Idle => {
                self.no_location(out);
                return;
            }
            ConnectivityState::WaitingForIdentity => warning(issue::NO_IDENTITY),
            ConnectivityState::WaitingForUtc => warning(issue::NO_UTC_TIME),
            ConnectivityState::Backoff(error) => transport_issue(error),
            ConnectivityState::Unauthorized(_) => Issue {
                severity: IssueSeverity::Error,
                code: issue::UNAUTHORIZED,
                age_ms: None,
            },
        };
        let _ = out.push(issue);
        self.no_location(out);
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let status = self.shared.status();
        let counters = status.diagnostics;
        out.u64(
            status_field::FRAMES_CREATED,
            u64::from(counters.frames_created),
        )?;
        out.u64(
            status_field::FRAMES_DELIVERED,
            u64::from(counters.frames_delivered),
        )?;
        out.u64(status_field::SEND_RETRIES, u64::from(counters.send_retries))?;
        out.u64(
            status_field::FRAMES_REJECTED,
            u64::from(counters.frames_rejected),
        )?;
        out.u64(status_field::QUEUE_DEPTH, status.outbox_len as u64)?;
        out.u64(
            status_field::FRAMES_WITHOUT_LOCATION,
            u64::from(counters.frames_without_location),
        )?;
        if let Some(config) = (self.effective)() {
            let endpoint = config.endpoint();
            // Separate fields: host and port together can exceed the value limit.
            let mut host = heapless::String::<MAX_HOST_LEN>::new();
            let _ = write!(host, "{}", endpoint.host);
            out.text(status_field::HOST, &host)?;
            out.u64(status_field::PORT, u64::from(endpoint.port))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_location::NoLocation;

    use super::*;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn a_patch_on_the_defaults_needs_a_host() {
        let mut config = ConnectivityConfig::defaults();
        assert_eq!(ManagedConfig::validate(&config), Err(FieldError::Invalid));
        config
            .set_field(field::HOST, text("ingress.example.org"))
            .unwrap();
        config.set_field(field::ENABLED, Value::Bool(true)).unwrap();
        config.set_field(field::PORT, Value::U64(443)).unwrap();
        config.set_field(field::NODE_ID, Value::U64(42)).unwrap();
        config
            .set_field(field::INTERVAL_S, Value::U64(120))
            .unwrap();
        assert_eq!(ManagedConfig::validate(&config), Ok(()));
        assert_eq!(
            config.read_field(field::HOST),
            Ok(Some(text("ingress.example.org")))
        );
        assert_eq!(config.read_field(field::PORT), Ok(Some(Value::U64(443))));
        assert_eq!(config.interval_s, 120);
    }

    #[test]
    fn typed_conversions_reject_what_does_not_fit() {
        let mut config = ConnectivityConfig::defaults();
        assert_eq!(
            config.set_field(field::PORT, Value::U64(70_000)),
            Err(FieldError::OutOfRange)
        );
        assert_eq!(
            config.set_field(field::HOST, text("bad host!")),
            Err(FieldError::Invalid)
        );
        assert_eq!(
            config.set_field(field::ENABLED, Value::U64(1)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            config.unset_field(field::HOST),
            Err(FieldError::NotOptional)
        );
        assert_eq!(config.unset_field(99), Err(FieldError::UnknownField));
    }

    #[test]
    fn the_schema_bounds_match_the_typed_validation() {
        let field = |id| {
            ConnectivityConfig::FIELDS
                .iter()
                .find(|field| field.field.id == id)
                .unwrap()
        };
        assert_eq!(
            field(field::INTERVAL_S).bounds,
            Some(Bounds::U64(MIN_INTERVAL_S.into(), MAX_INTERVAL_S.into()))
        );
        assert_eq!(field(field::INTERVAL_S).field.unit, Unit::S);
    }

    fn enabled() -> Option<ConnectivityConfig> {
        let mut config = ConnectivityConfig::defaults();
        config.host = Host::new("ingress.example.org").unwrap();
        config.port = 443;
        config.enabled = true;
        Some(config)
    }

    fn disabled() -> Option<ConnectivityConfig> {
        None
    }

    struct Located;

    impl LocationSource for Located {
        fn location(&self) -> Option<rohi_location::Location> {
            Some(rohi_location::Location {
                latitude: 52.52,
                longitude: 13.405,
                height_m: None,
                source: rohi_location::LocationSourceKind::Configured,
            })
        }
    }

    #[test]
    fn without_a_location_a_working_service_is_degraded_with_a_reason() {
        use crate::service::{ConnectivityState, ConnectivityStatus};

        let shared = ConnectivityShared::<NoopRawMutex>::new();
        shared.publish(ConnectivityStatus {
            state: ConnectivityState::Idle,
            ..Default::default()
        });

        let unlocated = ConnectivityStatusService::new(&shared, enabled, &NoLocation);
        let summary = block_on(unlocated.summary());
        assert_eq!(summary.state, ServiceState::Running);
        assert_eq!(summary.health, ServiceHealth::Degraded);
        // Frames still go out: only the location is missing.
        assert!(summary.ready);
        let mut issues = Vec::new();
        block_on(unlocated.issues(&mut issues));
        assert_eq!(issues[0].code, issue::NO_LOCATION);
        assert_eq!(issues[0].severity, IssueSeverity::Warning);

        let located = ConnectivityStatusService::new(&shared, enabled, &Located);
        let summary = block_on(located.summary());
        assert_eq!(summary.health, ServiceHealth::Ok);
        let mut issues = Vec::new();
        block_on(located.issues(&mut issues));
        assert!(issues.is_empty());

        // Disabled telemetry is not degraded by a missing location.
        let off = ConnectivityStatusService::new(&shared, disabled, &NoLocation);
        assert_eq!(block_on(off.summary()).state, ServiceState::Disabled);
        let mut issues = Vec::new();
        block_on(off.issues(&mut issues));
        assert!(issues.is_empty());
    }

    #[test]
    fn a_backing_off_service_lists_its_own_issue_first_then_the_missing_location() {
        use crate::service::{ConnectivityState, ConnectivityStatus};
        use crate::transport::TransportError;

        let shared = ConnectivityShared::<NoopRawMutex>::new();
        shared.publish(ConnectivityStatus {
            state: ConnectivityState::Backoff(TransportError::Timeout),
            ..Default::default()
        });
        let service = ConnectivityStatusService::new(&shared, enabled, &NoLocation);
        let mut issues = Vec::new();
        block_on(service.issues(&mut issues));
        let codes: std::vec::Vec<_> = issues.iter().map(|issue| issue.code).collect();
        assert_eq!(codes, [issue::CONNECTION_TIMEOUT, issue::NO_LOCATION]);
    }

    #[test]
    fn the_service_state_follows_the_published_status() {
        let shared = ConnectivityShared::<NoopRawMutex>::new();
        let service = ConnectivityStatusService::new(&shared, enabled, &NoLocation);
        let summary = block_on(service.summary());
        assert_eq!(summary.state, ServiceState::Starting);

        // The endpoint of the running configuration is reported as host and port.
        struct Sink(std::vec::Vec<(u32, Value)>);
        impl StatusWriter for Sink {
            fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
                self.0.push((id, value));
                Ok(())
            }
        }
        let mut sink = Sink(std::vec::Vec::new());
        block_on(service.status(&mut sink)).unwrap();
        assert!(sink.0.contains(&(
            status_field::HOST,
            Value::text("ingress.example.org").unwrap()
        )));
        assert!(sink.0.contains(&(status_field::PORT, Value::U64(443))));

        let off = ConnectivityStatusService::new(&shared, disabled, &NoLocation);
        let mut sink = Sink(std::vec::Vec::new());
        block_on(off.status(&mut sink)).unwrap();
        // No effective configuration, no endpoint.
        assert!(
            sink.0
                .iter()
                .all(|(id, _)| *id != status_field::HOST && *id != status_field::PORT)
        );
        let summary = block_on(off.summary());
        assert_eq!(summary.state, ServiceState::Disabled);
        assert!(!summary.ready);
        let mut issues = Vec::new();
        block_on(off.issues(&mut issues));
        assert!(issues.is_empty());
    }
}
