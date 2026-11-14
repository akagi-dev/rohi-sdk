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
//! The bounded snapshot of one capture and the writer sources fill it with.

use heapless::Vec;
use rohi_time::MonotonicTimestamp;

use crate::catalog::{self, ValueKind};
use crate::observation::{MeasurementId, Observation, ObservationValue, SourceId};

/// Maximum number of per-source reports a snapshot keeps.
pub const MAX_SOURCE_REPORTS: usize = 8;

/// Why a source did not contribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFailure {
    /// The source cannot provide data now (for example no valid location).
    Unavailable,
    /// The source failed while collecting.
    Failed,
    /// The source did not answer within its time budget.
    TimedOut,
    /// The source emitted the same `(source, measurement)` twice.
    Duplicate,
    /// The snapshot is full.
    CapacityExceeded,
}

/// What became of one source in one capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceOutcome {
    /// The source contributed all its accepted observations.
    Collected,
    /// The contribution was discarded as a whole: a source is atomic.
    Discarded(SourceFailure),
}

/// The report of one source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceReport {
    /// The source.
    pub source: SourceId,
    /// What became of it.
    pub outcome: SourceOutcome,
    /// Observations kept in the snapshot.
    pub accepted: u16,
    /// Observations refused one by one (unknown measurement, wrong kind,
    /// non-finite or out-of-range value).
    pub rejected: u16,
}

/// Per-source results of a capture. Sources are independent: a failing one
/// never prevents the others from contributing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionDiagnostics {
    /// One report per source, in collection order.
    pub sources: Vec<SourceReport, MAX_SOURCE_REPORTS>,
    /// Reports that did not fit.
    pub dropped_reports: u8,
}

impl CollectionDiagnostics {
    /// The report of `source`.
    pub fn of(&self, source: SourceId) -> Option<&SourceReport> {
        self.sources.iter().find(|report| report.source == source)
    }

    /// Whether every source contributed and nothing was refused.
    pub fn is_clean(&self) -> bool {
        self.dropped_reports == 0
            && self
                .sources
                .iter()
                .all(|report| report.outcome == SourceOutcome::Collected && report.rejected == 0)
    }
}

/// An internally consistent, bounded collection of observations.
///
/// `captured_at` is the capture boundary, not a claim that every value was
/// acquired at that instant: each [`Observation`] has its own time.
#[derive(Clone, Debug, PartialEq)]
pub struct TelemetrySnapshot<const N: usize> {
    /// When the capture started.
    pub captured_at: MonotonicTimestamp,
    /// The observations, grouped by source in collection order.
    pub observations: Vec<Observation, N>,
    /// What every source did.
    pub diagnostics: CollectionDiagnostics,
}

impl<const N: usize> TelemetrySnapshot<N> {
    /// An empty snapshot captured at `captured_at`.
    pub const fn new(captured_at: MonotonicTimestamp) -> Self {
        Self {
            captured_at,
            observations: Vec::new(),
            diagnostics: CollectionDiagnostics {
                sources: Vec::new(),
                dropped_reports: 0,
            },
        }
    }

    /// The observation with key `(source, measurement)`.
    pub fn get(&self, source: SourceId, measurement: MeasurementId) -> Option<&Observation> {
        self.observations
            .iter()
            .find(|o| o.source == source && o.measurement == measurement)
    }

    /// A writer for the contribution of `source`.
    pub fn writer(&mut self, source: SourceId) -> SourceWriter<'_, N> {
        let mark = self.observations.len();
        SourceWriter {
            snapshot: self,
            source,
            mark,
            accepted: 0,
            rejected: 0,
            fatal: None,
        }
    }

    pub(crate) fn report(&mut self, report: SourceReport) {
        if self.diagnostics.sources.push(report).is_err() {
            self.diagnostics.dropped_reports = self.diagnostics.dropped_reports.saturating_add(1);
        }
    }

    pub(crate) fn rollback(&mut self, mark: usize) {
        self.observations.truncate(mark);
    }
}

/// Why one observation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// The measurement is not in the catalog.
    UnknownMeasurement,
    /// The value is not of the kind of the measurement.
    WrongKind,
    /// The value is not finite or is out of range.
    InvalidValue,
    /// `(source, measurement)` was already written; the contribution is
    /// discarded as a whole.
    Duplicate,
    /// The snapshot is full; the contribution is discarded as a whole.
    Full,
}

/// What a source writes its observations through. It is bound to one source
/// id, so a source cannot attribute data to another source.
pub struct SourceWriter<'a, const N: usize> {
    snapshot: &'a mut TelemetrySnapshot<N>,
    source: SourceId,
    mark: usize,
    accepted: u16,
    rejected: u16,
    fatal: Option<SourceFailure>,
}

impl<const N: usize> SourceWriter<'_, N> {
    /// Adds an observation.
    ///
    /// # Errors
    ///
    /// A refused observation is counted and not stored. [`WriteError::Duplicate`]
    /// and [`WriteError::Full`] additionally make the whole contribution of
    /// this source be discarded when the capture of the source ends.
    pub fn write(
        &mut self,
        measurement: MeasurementId,
        value: ObservationValue,
        observed_at: MonotonicTimestamp,
    ) -> Result<(), WriteError> {
        let result = self.check_and_push(measurement, value, observed_at);
        match result {
            Ok(()) => self.accepted = self.accepted.saturating_add(1),
            Err(WriteError::Duplicate) => {
                self.fatal.get_or_insert(SourceFailure::Duplicate);
            }
            Err(WriteError::Full) => {
                self.fatal.get_or_insert(SourceFailure::CapacityExceeded);
            }
            Err(_) => self.rejected = self.rejected.saturating_add(1),
        }
        result
    }

    /// A numeric observation.
    ///
    /// # Errors
    ///
    /// See [`write`](Self::write).
    pub fn number(
        &mut self,
        measurement: MeasurementId,
        value: f32,
        observed_at: MonotonicTimestamp,
    ) -> Result<(), WriteError> {
        self.write(measurement, ObservationValue::Number(value), observed_at)
    }

    fn check_and_push(
        &mut self,
        measurement: MeasurementId,
        value: ObservationValue,
        observed_at: MonotonicTimestamp,
    ) -> Result<(), WriteError> {
        let descriptor = catalog::descriptor(measurement).ok_or(WriteError::UnknownMeasurement)?;
        match (descriptor.kind, &value) {
            (ValueKind::Number, ObservationValue::Number(number)) => {
                if !number.is_finite() {
                    return Err(WriteError::InvalidValue);
                }
            }
            (ValueKind::Location, ObservationValue::Location(point)) => {
                if !point.is_valid() {
                    return Err(WriteError::InvalidValue);
                }
            }
            _ => return Err(WriteError::WrongKind),
        }
        if self.snapshot.get(self.source, measurement).is_some() {
            return Err(WriteError::Duplicate);
        }
        self.snapshot
            .observations
            .push(Observation {
                source: self.source,
                measurement,
                value,
                observed_at,
            })
            .map_err(|_| WriteError::Full)
    }

    pub(crate) fn finish(self, result: Result<(), SourceFailure>) {
        let failure = self.fatal.or(result.err());
        let (outcome, accepted) = match failure {
            None => (SourceOutcome::Collected, self.accepted),
            Some(failure) => {
                self.snapshot.rollback(self.mark);
                (SourceOutcome::Discarded(failure), 0)
            }
        };
        let report = SourceReport {
            source: self.source,
            outcome,
            accepted,
            rejected: self.rejected,
        };
        self.snapshot.report(report);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{BMX280_TEMPERATURE, LOCATION, SDS011_PM10};
    use crate::observation::GeoPoint;

    fn at(ms: u64) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(ms)
    }

    #[test]
    fn two_sources_keep_equal_quantities_apart() {
        let mut snapshot = TelemetrySnapshot::<4>::new(at(10));
        for (source, value) in [(SourceId(10), 20.0), (SourceId(11), 21.0)] {
            let mut writer = snapshot.writer(source);
            writer.number(BMX280_TEMPERATURE, value, at(5)).unwrap();
            writer.finish(Ok(()));
        }
        assert_eq!(snapshot.observations.len(), 2);
        assert_eq!(
            snapshot
                .get(SourceId(11), BMX280_TEMPERATURE)
                .unwrap()
                .value,
            ObservationValue::Number(21.0)
        );
        assert!(snapshot.diagnostics.is_clean());
    }

    #[test]
    fn bad_values_are_refused_one_by_one_and_counted() {
        let mut snapshot = TelemetrySnapshot::<4>::new(at(0));
        let mut writer = snapshot.writer(SourceId(1));
        assert_eq!(
            writer.number(BMX280_TEMPERATURE, f32::NAN, at(1)),
            Err(WriteError::InvalidValue)
        );
        assert_eq!(
            writer.number(MeasurementId(99), 1.0, at(1)),
            Err(WriteError::UnknownMeasurement)
        );
        assert_eq!(
            writer.number(LOCATION, 1.0, at(1)),
            Err(WriteError::WrongKind)
        );
        let off_planet = GeoPoint {
            latitude: 91.0,
            longitude: 0.0,
            altitude_m: None,
        };
        assert_eq!(
            writer.write(LOCATION, ObservationValue::Location(off_planet), at(1)),
            Err(WriteError::InvalidValue)
        );
        writer.number(SDS011_PM10, 3.0, at(1)).unwrap();
        writer.finish(Ok(()));
        let report = snapshot.diagnostics.of(SourceId(1)).unwrap();
        assert_eq!((report.accepted, report.rejected), (1, 4));
        assert_eq!(report.outcome, SourceOutcome::Collected);
        assert!(!snapshot.diagnostics.is_clean());
    }

    #[test]
    fn a_duplicate_discards_the_whole_contribution_of_that_source_only() {
        let mut snapshot = TelemetrySnapshot::<8>::new(at(0));
        let mut other = snapshot.writer(SourceId(2));
        other.number(SDS011_PM10, 1.0, at(1)).unwrap();
        other.finish(Ok(()));

        let mut writer = snapshot.writer(SourceId(1));
        writer.number(BMX280_TEMPERATURE, 1.0, at(1)).unwrap();
        assert_eq!(
            writer.number(BMX280_TEMPERATURE, 2.0, at(2)),
            Err(WriteError::Duplicate)
        );
        writer.finish(Ok(()));

        assert_eq!(snapshot.observations.len(), 1);
        assert_eq!(snapshot.observations[0].source, SourceId(2));
        assert_eq!(
            snapshot.diagnostics.of(SourceId(1)).unwrap().outcome,
            SourceOutcome::Discarded(SourceFailure::Duplicate)
        );
    }

    #[test]
    fn overflow_is_observable_and_never_partial() {
        let mut snapshot = TelemetrySnapshot::<2>::new(at(0));
        let mut writer = snapshot.writer(SourceId(1));
        writer.number(BMX280_TEMPERATURE, 1.0, at(1)).unwrap();
        writer.number(SDS011_PM10, 1.0, at(1)).unwrap();
        assert_eq!(
            writer.number(crate::catalog::SCD4X_CO2, 1.0, at(1)),
            Err(WriteError::Full)
        );
        writer.finish(Ok(()));
        assert!(snapshot.observations.is_empty());
        assert_eq!(
            snapshot.diagnostics.of(SourceId(1)).unwrap().outcome,
            SourceOutcome::Discarded(SourceFailure::CapacityExceeded)
        );
    }

    #[test]
    fn a_source_error_discards_its_observations() {
        let mut snapshot = TelemetrySnapshot::<4>::new(at(0));
        let mut writer = snapshot.writer(SourceId(1));
        writer.number(BMX280_TEMPERATURE, 1.0, at(1)).unwrap();
        writer.finish(Err(SourceFailure::Failed));
        assert!(snapshot.observations.is_empty());
    }
}
