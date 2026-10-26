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
    FieldDescriptor, Issue, IssueSeverity, MAX_ISSUES, ManagedService, ServiceHealth, ServiceState,
    ServiceSummary, StatusWriter, ValueKind, WriteError,
};

use crate::{SyncFailure, TimeService, TimeSourceKind, TimeSyncState};

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
}

fn failure_issue(failure: SyncFailure) -> Issue {
    let (code, message) = match failure {
        SyncFailure::NetworkUnavailable => ("network_unavailable", "The network is unavailable"),
        SyncFailure::Resolve => ("resolve_failed", "The time server name was not resolved"),
        SyncFailure::Timeout => ("timeout", "The time source did not answer"),
        SyncFailure::InvalidResponse => ("invalid_response", "The answer was invalid"),
        SyncFailure::Rejected => ("rejected", "The time source refused to serve"),
        SyncFailure::Implausible => ("implausible", "The sample was not plausible"),
        SyncFailure::Source => ("source_failure", "The time source failed"),
    };
    Issue {
        severity: IssueSeverity::Warning,
        code,
        message,
        age_ms: None,
    }
}

/// The time service as a `time` service of the status plane.
///
/// Wall-clock time is optional: the device has none until a source delivered
/// a plausible sample, and it is never invented.
pub struct TimeStatusService<'a, C, M: RawMutex>(pub &'a TimeService<C, M>);

impl<C: Clock, M: RawMutex> ManagedService for TimeStatusService<'_, C, M> {
    const NAME: &'static str = "time";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::UNIX_TIME_MS, "unix_time", ValueKind::U64)
            .describe("Wall-clock time since the Unix epoch")
            .unit("ms"),
        FieldDescriptor::new(status_field::SOURCE, "source", ValueKind::Enum)
            .describe("Source of the current time")
            .variants(&["ntp", "rtc", "external"]),
        FieldDescriptor::new(status_field::SYNC_AGE_MS, "sync_age", ValueKind::U64)
            .describe("Time since the current time was accepted")
            .unit("ms"),
        FieldDescriptor::new(status_field::UNCERTAINTY_MS, "uncertainty", ValueKind::U64)
            .describe("Uncertainty when the time was accepted")
            .unit("ms"),
        FieldDescriptor::new(
            status_field::CONSECUTIVE_FAILURES,
            "consecutive_failures",
            ValueKind::U64,
        )
        .describe("Failed attempts since the last accepted sample"),
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
                    TimeSourceKind::Ntp => "ntp",
                    TimeSourceKind::Rtc => "rtc",
                    TimeSourceKind::External => "external",
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
        )
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
        let status = block_on(registry.status("time", 0)).unwrap();
        assert_eq!(status.summary.state, rohi_service::ServiceState::Starting);
        assert!(!status.summary.ready);
        assert!(status.issues.is_empty());
        // No time, no source: only the failure counter.
        assert_eq!(status.fields.len(), 1);
        assert_eq!(
            status.fields[0],
            (status_field::CONSECUTIVE_FAILURES, Value::U64(0))
        );
    }

    #[test]
    fn the_schema_lists_the_sources() {
        type S = TimeStatusService<'static, ManualClock, NoopRawMutex>;
        assert!(
            <S as ManagedService>::STATUS_SCHEMA[1]
                .variants
                .contains(&"rtc")
        );
        const { assert!(!<S as ManagedService>::ESSENTIAL) };
    }
}
