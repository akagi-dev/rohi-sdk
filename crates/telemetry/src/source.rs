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
//! Sources of observations and the bounded, best-effort capture over them.

use embedded_hal_async::delay::DelayNs;
use rohi_time::{Clock, with_timeout};

use crate::observation::SourceId;
use crate::snapshot::{SourceFailure, SourceWriter, TelemetrySnapshot};

/// Why a source could not collect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    /// Nothing to report now, for example no valid location. Not a fault.
    Unavailable,
    /// The source failed.
    Failed,
}

impl From<SourceError> for SourceFailure {
    fn from(error: SourceError) -> Self {
        match error {
            SourceError::Unavailable => Self::Unavailable,
            SourceError::Failed => Self::Failed,
        }
    }
}

/// A producer of observations: the local sensors, a location, a software
/// source of a test. Core telemetry knows no concrete source.
///
/// `collect` must be cancel-safe: the capture bounds it in time and drops the
/// future when the budget is spent, and the writer then discards what the
/// source had written.
///
/// A source writes through the [`SourceWriter`], which is bound to
/// [`id`](Self::id): a source cannot attribute data to another source.
/// Values must be acquired by the source, never relabelled from another
/// source; the observations of a remote peer are not local ones.
#[allow(async_fn_in_trait)]
pub trait TelemetrySource {
    /// The identity of this source (instance).
    fn id(&self) -> SourceId;

    /// Writes the current observations.
    ///
    /// # Errors
    ///
    /// [`SourceError`]; the observations written so far are then discarded.
    async fn collect<const N: usize>(
        &mut self,
        writer: &mut SourceWriter<'_, N>,
    ) -> Result<(), SourceError>;
}

/// A static, compile-time bounded set of sources: `()` or a tuple of up to
/// eight `&mut` sources... of up to eight owned sources.
#[allow(async_fn_in_trait)]
pub trait SourceSet {
    /// Collects every source in order into `snapshot`, each within
    /// `timeout_ms`.
    async fn collect_all<Dl: DelayNs, const N: usize>(
        &mut self,
        delay: &mut Dl,
        timeout_ms: u32,
        snapshot: &mut TelemetrySnapshot<N>,
    );
}

/// Collects one source into `snapshot` within `timeout_ms`, atomically.
pub async fn collect_one<S: TelemetrySource, D: DelayNs, const N: usize>(
    source: &mut S,
    delay: &mut D,
    timeout_ms: u32,
    snapshot: &mut TelemetrySnapshot<N>,
) {
    let id = source.id();
    let mark = snapshot.observations.len();
    let outcome = {
        let mut writer = snapshot.writer(id);
        let result = with_timeout(delay, timeout_ms, source.collect(&mut writer)).await;
        match result {
            Ok(Ok(())) => (writer, Ok(())),
            Ok(Err(error)) => (writer, Err(SourceFailure::from(error))),
            Err(_) => (writer, Err(SourceFailure::TimedOut)),
        }
    };
    let (writer, result) = outcome;
    writer.finish(result);
    debug_assert!(snapshot.observations.len() >= mark);
}

impl SourceSet for () {
    async fn collect_all<D: DelayNs, const N: usize>(
        &mut self,
        _: &mut D,
        _: u32,
        _: &mut TelemetrySnapshot<N>,
    ) {
    }
}

macro_rules! source_set {
    ($($name:ident),+) => {
        impl<$($name: TelemetrySource),+> SourceSet for ($($name,)+) {
            #[allow(non_snake_case)]
            async fn collect_all<Dl: DelayNs, const N: usize>(
                &mut self,
                delay: &mut Dl,
                timeout_ms: u32,
                snapshot: &mut TelemetrySnapshot<N>,
            ) {
                let ($($name,)+) = self;
                $(collect_one($name, delay, timeout_ms, snapshot).await;)+
            }
        }
    };
}

source_set!(A);
source_set!(A, B);
source_set!(A, B, C);
source_set!(A, B, C, D);
source_set!(A, B, C, D, E);
source_set!(A, B, C, D, E, F);

/// Captures a snapshot of `sources`.
///
/// One consistent, bounded collection: the capture boundary is read from
/// `clock` once, every source is bounded by `source_timeout_ms`, a failing or
/// slow source is reported and skipped, and the others still contribute. The
/// capture never reads UTC and never blocks other tasks beyond the bound.
pub async fn capture<S: SourceSet, C: Clock, D: DelayNs, const N: usize>(
    sources: &mut S,
    clock: &C,
    delay: &mut D,
    source_timeout_ms: u32,
) -> TelemetrySnapshot<N> {
    let mut snapshot = TelemetrySnapshot::new(clock.now());
    sources
        .collect_all(delay, source_timeout_ms, &mut snapshot)
        .await;
    snapshot
}

/// A source over fixed values, for tests and for software-defined data; it
/// needs no hardware.
#[derive(Debug, Clone)]
pub struct StaticSource<'a> {
    id: SourceId,
    values: &'a [(crate::MeasurementId, f32, rohi_time::MonotonicTimestamp)],
}

impl<'a> StaticSource<'a> {
    /// A source `id` that reports `values` every time.
    pub const fn new(
        id: SourceId,
        values: &'a [(crate::MeasurementId, f32, rohi_time::MonotonicTimestamp)],
    ) -> Self {
        Self { id, values }
    }
}

impl TelemetrySource for StaticSource<'_> {
    fn id(&self) -> SourceId {
        self.id
    }

    async fn collect<const N: usize>(
        &mut self,
        writer: &mut SourceWriter<'_, N>,
    ) -> Result<(), SourceError> {
        for (measurement, value, at) in self.values {
            // Refusals are counted by the writer.
            let _ = writer.number(*measurement, *value, *at);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use embassy_futures::block_on;
    use rohi_time::MonotonicTimestamp;

    use super::*;
    use crate::catalog::{BMX280_TEMPERATURE, SCD4X_CO2, SDS011_PM10};
    use crate::snapshot::SourceOutcome;

    struct Clk(Cell<u64>);
    impl Clock for Clk {
        fn now(&self) -> MonotonicTimestamp {
            MonotonicTimestamp::from_millis(self.0.get())
        }
    }

    struct NoDelay;
    impl DelayNs for NoDelay {
        async fn delay_ns(&mut self, _: u32) {}
    }

    struct Failing;
    impl TelemetrySource for Failing {
        fn id(&self) -> SourceId {
            SourceId(5)
        }
        async fn collect<const N: usize>(
            &mut self,
            writer: &mut SourceWriter<'_, N>,
        ) -> Result<(), SourceError> {
            writer
                .number(SDS011_PM10, 1.0, MonotonicTimestamp::from_millis(1))
                .unwrap();
            Err(SourceError::Failed)
        }
    }

    struct Hanging;
    impl TelemetrySource for Hanging {
        fn id(&self) -> SourceId {
            SourceId(6)
        }
        async fn collect<const N: usize>(
            &mut self,
            writer: &mut SourceWriter<'_, N>,
        ) -> Result<(), SourceError> {
            writer
                .number(SCD4X_CO2, 400.0, MonotonicTimestamp::from_millis(1))
                .unwrap();
            core::future::pending().await
        }
    }

    fn t(ms: u64) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(ms)
    }

    #[test]
    fn a_software_source_works_without_hardware() {
        let values = [(BMX280_TEMPERATURE, 21.5, t(100))];
        let mut sources = (StaticSource::new(SourceId(1), &values),);
        let snapshot: TelemetrySnapshot<4> = block_on(capture(
            &mut sources,
            &Clk(Cell::new(150)),
            &mut NoDelay,
            100,
        ));
        assert_eq!(snapshot.captured_at, t(150));
        assert_eq!(snapshot.observations.len(), 1);
        // The observation keeps its own time, not the capture boundary.
        assert_eq!(snapshot.observations[0].observed_at, t(100));
        assert!(snapshot.diagnostics.is_clean());
    }

    #[test]
    fn a_failing_or_hanging_source_does_not_stop_the_others() {
        let values = [(BMX280_TEMPERATURE, 21.5, t(100))];
        let mut sources = (Failing, Hanging, StaticSource::new(SourceId(1), &values));
        let snapshot: TelemetrySnapshot<8> =
            block_on(capture(&mut sources, &Clk(Cell::new(0)), &mut NoDelay, 100));
        assert_eq!(snapshot.observations.len(), 1);
        assert_eq!(snapshot.observations[0].source, SourceId(1));
        assert_eq!(
            snapshot.diagnostics.of(SourceId(5)).unwrap().outcome,
            SourceOutcome::Discarded(SourceFailure::Failed)
        );
        assert_eq!(
            snapshot.diagnostics.of(SourceId(6)).unwrap().outcome,
            SourceOutcome::Discarded(SourceFailure::TimedOut)
        );
        assert_eq!(
            snapshot.diagnostics.of(SourceId(1)).unwrap().outcome,
            SourceOutcome::Collected
        );
    }

    #[test]
    fn one_capture_serves_several_readers() {
        let values = [(BMX280_TEMPERATURE, 1.0, t(1))];
        let mut sources = (StaticSource::new(SourceId(1), &values),);
        let snapshot: TelemetrySnapshot<4> =
            block_on(capture(&mut sources, &Clk(Cell::new(0)), &mut NoDelay, 100));
        let copy = snapshot.clone();
        assert_eq!(snapshot, copy);
    }
}
