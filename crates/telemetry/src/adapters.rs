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
//! Adapters from concrete services to [`TelemetrySource`](crate::TelemetrySource). Each is optional:
//! the core collects from any source and needs neither.

#[cfg(feature = "location")]
pub use location_source::LocationTelemetrySource;
#[cfg(feature = "sensors")]
pub use sensors_source::SensorsTelemetrySource;

#[cfg(feature = "sensors")]
mod sensors_source {
    use embassy_sync::blocking_mutex::raw::RawMutex;
    use rohi_sensors::runtime::MeasurementHandle;
    use rohi_sensors::{Measurement, MeasurementState};

    use crate::catalog::*;
    use crate::observation::{MeasurementId, SourceId};
    use crate::snapshot::SourceWriter;
    use crate::source::{SourceError, TelemetrySource};

    /// The local physical sensors as one source, over the snapshot service of
    /// `rohi-sensors`.
    ///
    /// Only `Valid` measurements are published: `Stale`, `Error`, `WarmingUp`
    /// and `Unavailable` ones are left out, and a last-known value kept while
    /// a sensor fails is never published. Every observation keeps its own
    /// acquisition time and its sensor in its name: nothing is merged.
    pub struct SensorsTelemetrySource<'a, M: RawMutex> {
        handle: MeasurementHandle<'a, M>,
        id: SourceId,
    }

    impl<'a, M: RawMutex> SensorsTelemetrySource<'a, M> {
        /// The local sensors as source [`SourceId::LOCAL`].
        pub const fn new(handle: MeasurementHandle<'a, M>) -> Self {
            Self::with_id(handle, SourceId::LOCAL)
        }

        /// The same with another source id.
        pub const fn with_id(handle: MeasurementHandle<'a, M>, id: SourceId) -> Self {
            Self { handle, id }
        }
    }

    trait AsNumber: Copy {
        fn as_number(self) -> f32;
    }

    macro_rules! as_number {
        ($($ty:ty),+) => {$(
            impl AsNumber for $ty {
                fn as_number(self) -> f32 {
                    self.0 as f32
                }
            }
        )+};
    }

    as_number!(
        rohi_sensors::Celsius,
        rohi_sensors::RelativeHumidity,
        rohi_sensors::Pascal,
        rohi_sensors::Ppm,
        rohi_sensors::Ppb,
        rohi_sensors::Decibels,
        rohi_sensors::MicrogramsPerCubicMeter,
        rohi_sensors::Aqi,
        rohi_sensors::RadiationLevel,
        rohi_sensors::PulseCount
    );

    fn put<T: AsNumber, const N: usize, const SRC: usize>(
        writer: &mut SourceWriter<'_, N, SRC>,
        id: MeasurementId,
        measurement: &Measurement<T>,
    ) {
        if measurement.state != MeasurementState::Valid {
            return;
        }
        if let (Some(value), Some(at)) = (measurement.value, measurement.updated_at) {
            // Refusals are counted by the writer.
            let _ = writer.number(id, value.as_number(), at);
        }
    }

    impl<M: RawMutex> TelemetrySource for SensorsTelemetrySource<'_, M> {
        fn id(&self) -> SourceId {
            self.id
        }

        async fn collect<const N: usize, const SRC: usize>(
            &mut self,
            writer: &mut SourceWriter<'_, N, SRC>,
        ) -> Result<(), SourceError> {
            let snapshot = self
                .handle
                .snapshot()
                .await
                .map_err(|_| SourceError::Unavailable)?;
            let m = &snapshot.measurements;
            if let Some(bmx) = &m.bmx280 {
                put(writer, BMX280_TEMPERATURE, &bmx.temperature);
                if let Some(humidity) = &bmx.humidity {
                    put(writer, BMX280_HUMIDITY, humidity);
                }
                put(writer, BMX280_PRESSURE, &bmx.pressure);
            }
            if let Some(scd) = &m.scd4x {
                put(writer, SCD4X_CO2, &scd.co2);
                put(writer, SCD4X_TEMPERATURE, &scd.temperature);
                put(writer, SCD4X_HUMIDITY, &scd.humidity);
            }
            if let Some(sds) = &m.sds011 {
                put(writer, SDS011_PM2_5, &sds.pm25);
                put(writer, SDS011_PM10, &sds.pm10);
            }
            if let Some(mic) = &m.microphone {
                put(writer, MICROPHONE_AVERAGE, &mic.average);
                put(writer, MICROPHONE_MAXIMUM, &mic.maximum);
            }
            if let Some(zmod) = &m.zmod4510 {
                put(writer, ZMOD4510_O3, &zmod.o3);
                put(writer, ZMOD4510_NO2, &zmod.no2);
                put(writer, ZMOD4510_AQI_FAST, &zmod.fast_aqi);
                put(writer, ZMOD4510_AQI_EPA, &zmod.epa_aqi);
            }
            if let Some(ags) = &m.ags3871 {
                put(writer, AGS3871_CO, &ags.co);
            }
            if let Some(rad) = &m.radsens {
                put(writer, RADSENS_RADIATION, &rad.radiation);
                put(writer, RADSENS_PULSES, &rad.pulses);
            }
            Ok(())
        }
    }
}

#[cfg(feature = "location")]
mod location_source {
    use rohi_location::LocationSource;
    use rohi_time::MonotonicTimestamp;

    use crate::catalog::LOCATION;
    use crate::observation::{GeoPoint, ObservationValue, SourceId};
    use crate::snapshot::SourceWriter;
    use crate::source::{SourceError, TelemetrySource};

    /// The device location as one atomic `location` observation. It is
    /// unavailable while the device does not know where it is.
    ///
    /// The location has no acquisition time of its own: it is stamped with the
    /// capture boundary supplied at construction through `now`.
    pub struct LocationTelemetrySource<L, F> {
        location: L,
        now: F,
    }

    impl<L: LocationSource, F: Fn() -> MonotonicTimestamp> LocationTelemetrySource<L, F> {
        /// A location source stamping observations with `now()`.
        pub const fn new(location: L, now: F) -> Self {
            Self { location, now }
        }
    }

    impl<L: LocationSource, F: Fn() -> MonotonicTimestamp> TelemetrySource
        for LocationTelemetrySource<L, F>
    {
        fn id(&self) -> SourceId {
            SourceId::LOCATION
        }

        async fn collect<const N: usize, const SRC: usize>(
            &mut self,
            writer: &mut SourceWriter<'_, N, SRC>,
        ) -> Result<(), SourceError> {
            let location = self.location.location().ok_or(SourceError::Unavailable)?;
            let point = GeoPoint {
                latitude: location.latitude,
                longitude: location.longitude,
                altitude_m: location.height_m,
            };
            writer
                .write(LOCATION, ObservationValue::Location(point), (self.now)())
                .map_err(|_| SourceError::Failed)
        }
    }
}

#[cfg(all(test, feature = "sensors"))]
mod tests {
    use embassy_futures::block_on;
    use embassy_futures::join::join;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_sensors::runtime::MeasurementService;
    use rohi_sensors::{
        Bmx280Update, Celsius, MeasurementStore, MeasurementUpdate, Pascal, RelativeHumidity,
        Scd4xUpdate, SensorErrorCode, SensorEvent, SensorKind,
    };
    use rohi_time::{Clock, MonotonicTimestamp};

    use super::*;
    use crate::catalog::{BMX280_HUMIDITY, BMX280_TEMPERATURE, SCD4X_CO2};
    use crate::observation::{ObservationValue, SourceId};
    use crate::snapshot::TelemetrySnapshot;
    use crate::source::collect_one;

    /// A budget that is never spent: the owner task answers within the test.
    struct NeverDelay;
    impl embedded_hal_async::delay::DelayNs for NeverDelay {
        async fn delay_ns(&mut self, _: u32) {
            core::future::pending::<()>().await
        }
    }

    struct Clk(u64);
    impl Clock for Clk {
        fn now(&self) -> MonotonicTimestamp {
            MonotonicTimestamp::from_millis(self.0)
        }
    }

    fn collect(store: &MeasurementStore, now: u64) -> TelemetrySnapshot<16, 8> {
        collect_n(store, now)
    }

    fn collect_n<const N: usize, const SRC: usize>(
        store: &MeasurementStore,
        now: u64,
    ) -> TelemetrySnapshot<N, SRC> {
        let service = MeasurementService::<NoopRawMutex>::new();
        let mut source = SensorsTelemetrySource::new(service.handle());
        let mut snapshot = TelemetrySnapshot::new(MonotonicTimestamp::from_millis(now));
        let serve = async {
            service
                .accept()
                .await
                .respond(store.snapshot(Clk(now).now()));
        };
        block_on(join(
            collect_one(&mut source, &mut NeverDelay, 100, &mut snapshot),
            serve,
        ));
        snapshot
    }

    fn every_sensor_active() -> MeasurementStore {
        use rohi_sensors::{
            Ags3871Update, Aqi, Decibels, MicrogramsPerCubicMeter, NoiseUpdate, Ppb, Ppm,
            PulseCount, RadSensUpdate, RadiationLevel, Sds011Update, Zmod4510Update,
        };
        let at = MonotonicTimestamp::from_millis(1_000);
        let mut store = MeasurementStore::new();
        let mut apply = |update| store.apply(SensorEvent::Measurements(update));
        apply(MeasurementUpdate::Bmx280(Bmx280Update::bme280(
            Celsius(21.5),
            RelativeHumidity(45.0),
            Pascal(100_800.0),
            at,
        )));
        apply(MeasurementUpdate::Scd4x(Scd4xUpdate {
            co2: Ppm(640.0),
            temperature: Celsius(22.0),
            humidity: RelativeHumidity(40.0),
            timestamp: at,
        }));
        apply(MeasurementUpdate::Sds011(Sds011Update {
            pm25: MicrogramsPerCubicMeter(6.5),
            pm10: MicrogramsPerCubicMeter(12.0),
            timestamp: at,
        }));
        apply(MeasurementUpdate::Microphone(NoiseUpdate {
            average: Decibels(41.0),
            maximum: Decibels(55.0),
            timestamp: at,
        }));
        apply(MeasurementUpdate::Zmod4510(Zmod4510Update {
            o3: Ppb(30.0),
            no2: Ppb(12.0),
            fast_aqi: Aqi(40),
            epa_aqi: Aqi(35),
            timestamp: at,
        }));
        apply(MeasurementUpdate::Ags3871(Ags3871Update {
            co: Ppm(1.5),
            timestamp: at,
        }));
        apply(MeasurementUpdate::RadSens(RadSensUpdate {
            radiation: RadiationLevel(0.12),
            pulses: PulseCount(7),
            timestamp: at,
        }));
        store
    }

    #[test]
    fn all_sensors_active_fit_a_snapshot_of_the_catalog_size_without_a_location() {
        // 17 sensor measurements and no location source at all: a packet of
        // sensors only must be possible, and a snapshot sized by the catalog
        // never overflows.
        let snapshot =
            collect_n::<{ crate::catalog::CATALOG.len() }, 8>(&every_sensor_active(), 2_000);
        assert_eq!(
            snapshot.observations.len(),
            crate::catalog::CATALOG.len() - 1
        );
        let report = snapshot.diagnostics.of(SourceId::LOCAL).unwrap();
        assert_eq!(
            report.outcome,
            crate::snapshot::SourceOutcome::Collected,
            "{report:?}"
        );
        assert!(
            snapshot
                .get(SourceId::LOCAL, crate::catalog::RADSENS_PULSES)
                .is_some()
        );
    }

    #[test]
    fn a_snapshot_sized_for_the_packaged_measurements_keeps_a_source_that_reports_more() {
        use crate::catalog::*;
        // What a packet format can encode: 7 of the 17 sensor measurements, and the location.
        let packaged = [
            LOCATION,
            BMX280_TEMPERATURE,
            BMX280_HUMIDITY,
            BMX280_PRESSURE,
            SDS011_PM2_5,
            SDS011_PM10,
            MICROPHONE_AVERAGE,
            MICROPHONE_MAXIMUM,
        ];
        let accepted = packaged
            .iter()
            .fold(crate::Selection::NONE, |selection, id| selection.with(*id));
        let store = every_sensor_active();
        let service = MeasurementService::<NoopRawMutex>::new();
        let mut source = SensorsTelemetrySource::new(service.handle());
        let mut snapshot =
            TelemetrySnapshot::<8, 2>::accepting(MonotonicTimestamp::from_millis(2_000), accepted);
        let serve = async {
            service
                .accept()
                .await
                .respond(store.snapshot(Clk(2_000).now()));
        };
        block_on(join(
            collect_one(&mut source, &mut NeverDelay, 100, &mut snapshot),
            serve,
        ));
        let report = snapshot.diagnostics.of(SourceId::LOCAL).unwrap();
        assert_eq!(
            report.outcome,
            crate::snapshot::SourceOutcome::Collected,
            "{report:?}"
        );
        assert_eq!(
            snapshot.observations.len(),
            packaged.len() - 1,
            "every packaged sensor measurement"
        );
        assert!(snapshot.diagnostics.is_clean());
    }

    #[test]
    fn a_snapshot_that_is_too_small_is_reported_as_a_discarded_source() {
        // The failure mode the capacity must prevent: all 17 values do not fit
        // 16 slots, so the whole source is discarded and says why.
        let snapshot = collect_n::<16, 8>(&every_sensor_active(), 2_000);
        assert!(snapshot.observations.is_empty());
        assert_eq!(
            snapshot.diagnostics.of(SourceId::LOCAL).unwrap().outcome,
            crate::snapshot::SourceOutcome::Discarded(
                crate::snapshot::SourceFailure::CapacityExceeded
            )
        );
    }

    #[test]
    fn only_valid_measurements_are_published_with_their_own_times() {
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
            Bmx280Update::bme280(
                Celsius(21.5),
                RelativeHumidity(45.0),
                Pascal(100_800.0),
                MonotonicTimestamp::from_millis(1_000),
            ),
        )));
        let snapshot = collect(&store, 2_000);
        let temperature = snapshot
            .get(SourceId::LOCAL, BMX280_TEMPERATURE)
            .expect("published");
        assert_eq!(temperature.value, ObservationValue::Number(21.5));
        assert_eq!(
            temperature.observed_at,
            MonotonicTimestamp::from_millis(1_000)
        );
        assert!(snapshot.get(SourceId::LOCAL, BMX280_HUMIDITY).is_some());
        // A sensor that never reported publishes nothing.
        assert!(snapshot.get(SourceId::LOCAL, SCD4X_CO2).is_none());
    }

    #[test]
    fn stale_and_failing_values_are_not_published() {
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Scd4x(
            Scd4xUpdate {
                co2: rohi_sensors::Ppm(800.0),
                temperature: Celsius(21.0),
                humidity: RelativeHumidity(40.0),
                timestamp: MonotonicTimestamp::from_millis(0),
            },
        )));
        // Far beyond the freshness policy: stale.
        let stale = collect(&store, 10_000_000);
        assert!(stale.get(SourceId::LOCAL, SCD4X_CO2).is_none());

        // A failing sensor keeps its last value but must not publish it.
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Scd4x(
            Scd4xUpdate {
                co2: rohi_sensors::Ppm(800.0),
                temperature: Celsius(21.0),
                humidity: RelativeHumidity(40.0),
                timestamp: MonotonicTimestamp::from_millis(1_000),
            },
        )));
        store.apply(SensorEvent::Error {
            sensor: SensorKind::Scd4x,
            error: SensorErrorCode::Crc,
            timestamp: MonotonicTimestamp::from_millis(1_500),
        });
        let failing = collect(&store, 2_000);
        assert!(failing.get(SourceId::LOCAL, SCD4X_CO2).is_none());
    }
}
