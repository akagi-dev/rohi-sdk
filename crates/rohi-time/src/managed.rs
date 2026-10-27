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
//! The time service in the generic service pipeline.

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;
use rohi_hal::runtime::Clock;
use rohi_service::{
    EnumDescriptor, EnumVariant, FieldDescriptor, Issue, IssueDescriptor, IssueSeverity,
    MAX_ISSUES, ManagedService, ServiceHealth, ServiceState, ServiceSummary, StatusWriter, Unit,
    ValueKind, WriteError,
};

use crate::service::MAX_SERVERS;
use crate::{SyncFailure, TimeService, TimeSourceKind, TimeSyncState};

/// Id of the `time` service.
pub const SERVICE_ID: u32 = 5;

/// Issue codes of the `time` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// The network is unavailable.
    pub const NETWORK_UNAVAILABLE: u32 = BASE;
    /// The time server name was not resolved.
    pub const RESOLVE_FAILED: u32 = BASE + 1;
    /// The time source did not answer.
    pub const TIMEOUT: u32 = BASE + 2;
    /// The answer was invalid.
    pub const INVALID_RESPONSE: u32 = BASE + 3;
    /// The time source refused to serve.
    pub const REJECTED: u32 = BASE + 4;
    /// The sample was not plausible.
    pub const IMPLAUSIBLE: u32 = BASE + 5;
    /// The time source failed.
    pub const SOURCE_FAILURE: u32 = BASE + 6;
}

/// Values of the `source` status field.
pub mod source_value {
    /// Network time protocol.
    pub const NTP: u32 = 0;
    /// Battery-backed clock.
    pub const RTC: u32 = 1;
    /// Set by the application.
    pub const EXTERNAL: u32 = 2;
}

/// Field ids of the `time` service status.
pub mod status_field {
    /// Milliseconds since the Unix epoch; absent while unsynchronized.
    pub const UNIX_TIME_MS: u32 = 1;
    /// Source of the current time.
    pub const SOURCE: u32 = 2;
    /// Time since the current time was accepted.
    pub const SYNC_AGE_MS: u32 = 3;
    /// Uncertainty of the current time when it was accepted.
    pub const UNCERTAINTY_MS: u32 = 4;
    /// Failed attempts since the last accepted sample.
    pub const CONSECUTIVE_FAILURES: u32 = 5;
    /// First time server; absent when none is reported.
    pub const SERVER1: u32 = 6;
    /// Second time server.
    pub const SERVER2: u32 = 7;
    /// Third time server.
    pub const SERVER3: u32 = 8;
    /// Fourth time server.
    pub const SERVER4: u32 = 9;
}

fn failure_issue(failure: SyncFailure) -> Issue {
    let code = match failure {
        SyncFailure::NetworkUnavailable => issue::NETWORK_UNAVAILABLE,
        SyncFailure::Resolve => issue::RESOLVE_FAILED,
        SyncFailure::Timeout => issue::TIMEOUT,
        SyncFailure::InvalidResponse => issue::INVALID_RESPONSE,
        SyncFailure::Rejected => issue::REJECTED,
        SyncFailure::Implausible => issue::IMPLAUSIBLE,
        SyncFailure::Source => issue::SOURCE_FAILURE,
    };
    Issue {
        severity: IssueSeverity::Warning,
        code,
        age_ms: None,
    }
}

/// The time service as a `time` service of the status plane.
///
/// Wall-clock time is optional: the device has none until a source delivered
/// a plausible sample, and it is never invented.
pub struct TimeStatusService<'a, C, M: RawMutex>(pub &'a TimeService<C, M>);

impl<C: Clock, M: RawMutex> ManagedService for TimeStatusService<'_, C, M> {
    const ID: u32 = SERVICE_ID;
    const NAME: &'static str = "time";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::UNIX_TIME_MS, "unix_time", ValueKind::U64)
            .unit(Unit::Ms),
        FieldDescriptor::new(status_field::SOURCE, "source", ValueKind::Enum).with_enum(1),
        FieldDescriptor::new(status_field::SYNC_AGE_MS, "sync_age", ValueKind::U64).unit(Unit::Ms),
        FieldDescriptor::new(status_field::UNCERTAINTY_MS, "uncertainty", ValueKind::U64)
            .unit(Unit::Ms),
        FieldDescriptor::new(
            status_field::CONSECUTIVE_FAILURES,
            "consecutive_failures",
            ValueKind::U64,
        ),
        FieldDescriptor::new(status_field::SERVER1, "server1", ValueKind::String),
        FieldDescriptor::new(status_field::SERVER2, "server2", ValueKind::String),
        FieldDescriptor::new(status_field::SERVER3, "server3", ValueKind::String),
        FieldDescriptor::new(status_field::SERVER4, "server4", ValueKind::String),
    ];
    const ENUMS: &'static [EnumDescriptor] = &[EnumDescriptor::new(
        1,
        &[
            EnumVariant::new(source_value::NTP, "ntp"),
            EnumVariant::new(source_value::RTC, "rtc"),
            EnumVariant::new(source_value::EXTERNAL, "external"),
        ],
    )];
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::NETWORK_UNAVAILABLE, "network_unavailable"),
        IssueDescriptor::new(issue::RESOLVE_FAILED, "resolve_failed"),
        IssueDescriptor::new(issue::TIMEOUT, "timeout"),
        IssueDescriptor::new(issue::INVALID_RESPONSE, "invalid_response"),
        IssueDescriptor::new(issue::REJECTED, "rejected"),
        IssueDescriptor::new(issue::IMPLAUSIBLE, "implausible"),
        IssueDescriptor::new(issue::SOURCE_FAILURE, "source_failure"),
    ];
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        use ServiceHealth as H;
        use ServiceState as S;
        let status = self.0.status();
        match status.state {
            TimeSyncState::Synchronized => ServiceSummary::new(S::Running, H::Ok, true),
            // The time is usable but may have drifted.
            TimeSyncState::Stale => ServiceSummary::new(S::Running, H::Degraded, true),
            TimeSyncState::Unsynchronized if status.last_failure.is_some() => {
                ServiceSummary::new(S::Running, H::Degraded, false)
            }
            // Nothing failed yet: still waiting for the first sample.
            TimeSyncState::Unsynchronized => ServiceSummary::new(S::Starting, H::Unknown, false),
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if let Some(failure) = self.0.status().last_failure {
            let _ = out.push(failure_issue(failure));
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let status = self.0.status();
        if let Some(now) = self.0.now() {
            out.u64(status_field::UNIX_TIME_MS, now.as_millis())?;
        }
        if let Some(source) = status.source {
            out.variant(
                status_field::SOURCE,
                match source {
                    TimeSourceKind::Ntp => source_value::NTP,
                    TimeSourceKind::Rtc => source_value::RTC,
                    TimeSourceKind::External => source_value::EXTERNAL,
                },
            )?;
        }
        if let Some(age) = status.last_sync_age {
            out.u64(status_field::SYNC_AGE_MS, age.as_millis() as u64)?;
        }
        if let Some(uncertainty) = status.uncertainty {
            out.u64(status_field::UNCERTAINTY_MS, uncertainty.as_millis() as u64)?;
        }
        out.u64(
            status_field::CONSECUTIVE_FAILURES,
            u64::from(status.consecutive_failures),
        )?;
        const SERVER_FIELDS: [u32; MAX_SERVERS] = [
            status_field::SERVER1,
            status_field::SERVER2,
            status_field::SERVER3,
            status_field::SERVER4,
        ];
        for (id, server) in SERVER_FIELDS.into_iter().zip(self.0.servers()) {
            out.text(id, &server)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::{ManagedService, ServiceRegistry, Tracked, Value};

    use super::*;
    use crate::testutil::ManualClock;
    use crate::{TimeConfig, TimeService};

    #[test]
    fn an_unsynchronized_device_has_no_time_and_is_starting() {
        let service = TimeService::<_, NoopRawMutex>::new(
            ManualClock::default(),
            TimeConfig::new(crate::UnixTime::from_secs(1_700_000_000)),
        );
        let registry = (Tracked::<_, NoopRawMutex>::new(TimeStatusService(&service)),);
        let status = block_on(registry.status(SERVICE_ID, 0)).unwrap();
        assert_eq!(status.summary.state, rohi_service::ServiceState::Starting);
        assert!(!status.summary.ready);
        assert!(status.issues.is_empty());
        // No time, no source, no servers: only the failure counter.
        assert_eq!(status.fields.len(), 1);
        assert_eq!(
            status.fields[0],
            (status_field::CONSECUTIVE_FAILURES, Value::U64(0))
        );
    }

    #[test]
    fn the_status_lists_the_time_servers() {
        let service = TimeService::<_, NoopRawMutex>::new(
            ManualClock::default(),
            TimeConfig::new(crate::UnixTime::from_secs(1_700_000_000)),
        );
        service.set_servers(&["ntp.example.org", "time.example.net"]);
        let registry = (Tracked::<_, NoopRawMutex>::new(TimeStatusService(&service)),);
        let status = block_on(registry.status(SERVICE_ID, 0)).unwrap();
        let text = |id| {
            status
                .fields
                .iter()
                .find(|(field, _)| *field == id)
                .and_then(|(_, value)| value.as_str())
        };
        assert_eq!(text(status_field::SERVER1), Some("ntp.example.org"));
        assert_eq!(text(status_field::SERVER2), Some("time.example.net"));
        assert_eq!(text(status_field::SERVER3), None);

        service.set_servers(&["pool.ntp.org"]);
        let status = block_on(registry.status(SERVICE_ID, 0)).unwrap();
        assert_eq!(status.fields.len(), 2);
    }

    #[test]
    fn the_schema_lists_the_sources() {
        type S = TimeStatusService<'static, ManualClock, NoopRawMutex>;
        assert!(<S as ManagedService>::ENUMS[0].contains(source_value::RTC));
        assert_eq!(<S as ManagedService>::STATUS_SCHEMA[1].enum_id, 1);
        const { assert!(!<S as ManagedService>::ESSENTIAL) };
    }
}
