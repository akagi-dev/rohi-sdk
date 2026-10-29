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
//! The telemetry configuration and status in the generic management plane.

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;
use rohi_location::LocationSource;
use rohi_service::{
    FieldDescriptor, FieldError, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES, Service,
    ServiceHealth, ServiceState, ServiceSummary, Status, StatusWriter, ValueKind, WriteError,
};

use crate::ConnectivityConfig;
use crate::service::{ConnectivityShared, ConnectivityState};
use crate::transport::TransportError;

/// A section without an endpoint is valid: telemetry is disabled. The fields, their
/// ids and the persisted overrides of the `connectivity` section are declared
/// once, on [`ConnectivityConfig`].
pub(crate) fn validate_section(config: &ConnectivityConfig) -> Result<(), FieldError> {
    ConnectivityConfig::validate(config).map_err(|_| FieldError::Invalid)
}

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
    /// Frames created without a location.
    pub const FRAMES_WITHOUT_LOCATION: u32 = 6;
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
/// configuration itself is not repeated in the status.
///
/// Telemetry without a location still flows but is incomplete: while the
/// device does not know where it is, an otherwise healthy service is
/// `degraded` with the issue `no_location`.
#[derive(Service)]
#[service(name = "connectivity")]
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

impl<M: RawMutex> Status for ConnectivityStatusService<'_, M> {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_config::ConfigOverrides;
    use rohi_location::NoLocation;
    use rohi_service::{Bounds, Config, Unit, Value};

    use super::*;
    use crate::ConnectivityOverrides;
    use crate::EndpointUri;
    use crate::config::{MAX_INTERVAL_S, MIN_INTERVAL_S};

    const NODE_ID: u32 = 2;
    const INTERVAL_S: u32 = 3;
    const ENDPOINT: u32 = 4;

    fn text(value: &str) -> Value {
        Value::text(value).unwrap()
    }

    #[test]
    fn an_endpoint_enables_telemetry_and_none_disables_it() {
        let mut config = ConnectivityConfig::default();
        // Disabled is a valid state, without any endpoint.
        assert!(!config.is_enabled());
        assert_eq!(Config::validate(&config), Ok(()));
        assert_eq!(config.read_field(ENDPOINT), Ok(None));
        assert_eq!(config.field_is_set(ENDPOINT), Ok(false));

        config
            .set_field(ENDPOINT, text("http://ingress.example.org:8080"))
            .unwrap();
        config.set_field(NODE_ID, Value::U64(42)).unwrap();
        config.set_field(INTERVAL_S, Value::U64(120)).unwrap();
        assert!(config.is_enabled());
        assert_eq!(Config::validate(&config), Ok(()));
        assert_eq!(
            config.read_field(ENDPOINT),
            Ok(Some(text("http://ingress.example.org:8080")))
        );
        assert_eq!(config.interval_s, 120);

        // `none` disables telemetry again.
        assert_eq!(config.set_none(ENDPOINT), Ok(()));
        assert!(!config.is_enabled());
        assert_eq!(config.read_field(ENDPOINT), Ok(None));
    }

    #[test]
    fn typed_conversions_reject_what_does_not_fit() {
        let mut config = ConnectivityConfig::default();
        for refused in [
            "ingress.example.org",
            "https://ingress.example.org",
            "http://ingress.example.org/v1",
            "http://bad host!",
            "http://a.io:70000",
        ] {
            assert_eq!(
                config.set_field(ENDPOINT, text(refused)),
                Err(FieldError::Invalid),
                "{refused}"
            );
        }
        assert_eq!(
            config.set_field(ENDPOINT, Value::U64(80)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(
            config.set_field(INTERVAL_S, Value::Bool(true)),
            Err(FieldError::WrongKind)
        );
        assert_eq!(config.set_none(NODE_ID), Err(FieldError::NotOptional));
        assert_eq!(
            config.set_field(99, Value::U64(1)),
            Err(FieldError::UnknownField)
        );
        // The removed fields are unknown: there is no `enabled` and no port.
        for retired in [1, 5] {
            assert_eq!(
                config.set_field(retired, Value::U64(1)),
                Err(FieldError::UnknownField)
            );
        }
    }

    #[test]
    fn every_field_can_be_overridden_and_cleared() {
        let mut ids: std::vec::Vec<_> = ConnectivityConfig::FIELDS
            .iter()
            .map(|f| f.field.id)
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ConnectivityOverrides::FIELD_IDS);

        let mut overrides = ConnectivityOverrides::default();
        let mut config =
            rohi_config::materialize::<ConnectivityConfig>(&Default::default(), &overrides);
        config.set_field(INTERVAL_S, Value::U64(120)).unwrap();
        overrides.capture(&config, INTERVAL_S);
        assert_eq!(
            rohi_config::materialize::<ConnectivityConfig>(&Default::default(), &overrides)
                .interval_s,
            120
        );
        overrides.clear(INTERVAL_S);
        assert_eq!(
            rohi_config::materialize::<ConnectivityConfig>(&Default::default(), &overrides)
                .interval_s,
            ConnectivityConfig::default().interval_s
        );
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
            field(INTERVAL_S).bounds,
            Some(Bounds::U64(MIN_INTERVAL_S.into(), MAX_INTERVAL_S.into()))
        );
        assert_eq!(field(INTERVAL_S).field.unit, Unit::S);
    }

    fn enabled() -> Option<ConnectivityConfig> {
        Some(ConnectivityConfig {
            endpoint: EndpointUri::new("http://ingress.example.org:443"),
            ..ConnectivityConfig::default()
        })
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

        // The configuration is not repeated in the status.
        struct Sink(std::vec::Vec<u32>);
        impl StatusWriter for Sink {
            fn put(&mut self, id: u32, _: Value) -> Result<(), WriteError> {
                self.0.push(id);
                Ok(())
            }
        }
        let mut sink = Sink(std::vec::Vec::new());
        block_on(service.status(&mut sink)).unwrap();
        assert_eq!(
            sink.0,
            [
                status_field::FRAMES_CREATED,
                status_field::FRAMES_DELIVERED,
                status_field::SEND_RETRIES,
                status_field::FRAMES_REJECTED,
                status_field::QUEUE_DEPTH,
                status_field::FRAMES_WITHOUT_LOCATION,
            ]
        );

        let off = ConnectivityStatusService::new(&shared, disabled, &NoLocation);
        let summary = block_on(off.summary());
        assert_eq!(summary.state, ServiceState::Disabled);
        assert!(!summary.ready);
        let mut issues = Vec::new();
        block_on(off.issues(&mut issues));
        assert!(issues.is_empty());
    }

    #[test]
    fn the_service_identity_is_part_of_the_wire_abi() {
        assert_eq!(
            <ConnectivityStatusService<'static, NoopRawMutex> as Service>::NAME,
            "connectivity"
        );
    }
}
