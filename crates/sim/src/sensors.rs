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

//! Simulated sensors: the BMx280, SCD4x, SDS011 and noise drivers over the
//! [`World`](crate::world::World).
//!
//! The drivers implement the same [`SensorDriver`] trait as the real ones and
//! run in the same [`Runner`](rohi_hal::runtime::Runner)/`Reporter`/`EventBus`/store pipeline as the
//! firmware: the simulation only replaces the bus transaction. A value reaches
//! the control plane as
//!
//! ```text
//! SimBmx280 -> MeasurementUpdate -> Runner -> SensorEvent -> EVENTS
//!           -> store task -> MeasurementStore -> MeasurementsSnapshot
//! ```
//!
//! Nothing writes a snapshot or a protobuf message directly. The sensor
//! lifecycle (`NotDetected`, `WarmingUp`, `Degraded`, `Error`, recovery) is the
//! one of the [`Runner`](rohi_hal::runtime::Runner); this module only decides what the "bus" answers.

use core::marker::PhantomData;

use embassy_time::{Instant, Timer};
use rohi_hal::runtime::{Clock, DriverError, RunnerConfig, SensorDriver};
use rohi_hal::{
    Bmx280Update, Celsius, Decibels, MeasurementUpdate, MicrogramsPerCubicMeter,
    MonotonicTimestamp, NoiseUpdate, Pascal, Ppm, RelativeHumidity, Scd4xUpdate, Sds011Update,
    SensorErrorCode, SensorKind,
};

use crate::command::{Sensor, SensorFault};
use crate::world::{Values, world};

/// How long a read that times out takes.
const TIMEOUT_MS: u64 = 500;

/// Recovery timings of the runners: the firmware defaults with shorter
/// waits, so that a failing sensor is observed and recovers within seconds.
pub const RUNNER: RunnerConfig = RunnerConfig {
    reprobe_interval_ms: 1_000,
    backoff_initial_ms: 250,
    backoff_max_ms: 2_000,
    ..RunnerConfig::DEFAULT
};

/// What the simulated bus answers instead of data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimError {
    /// The sensor is not on the bus.
    Absent,
    /// Initialization failed.
    Init,
    /// The transaction failed.
    Read,
    /// The sensor did not answer.
    Timeout,
}

impl DriverError for SimError {
    fn code(&self) -> SensorErrorCode {
        match self {
            Self::Absent | Self::Read | Self::Init => SensorErrorCode::Bus,
            Self::Timeout => SensorErrorCode::Timeout,
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// Deterministic noise: xorshift64*, seeded from `--seed` and the sensor.
struct Jitter(u64);

impl Jitter {
    fn new(seed: Option<u64>, sensor: Sensor) -> Self {
        let seed = seed.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |elapsed| elapsed.as_nanos() as u64)
        });
        let mixed = seed ^ (sensor.index() as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        Self(mixed.max(1))
    }

    /// A value in `[-1, 1]`.
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
        (bits as f32 / (1u64 << 23) as f32) - 1.0
    }

    fn apply(&mut self, value: f32, relative: f32) -> f32 {
        if relative == 0.0 {
            value
        } else {
            value * (1.0 + relative * self.next())
        }
    }
}

/// How one sensor turns the quantities of the world into an observation.
pub trait Observe {
    const SENSOR: Sensor;
    const KIND: SensorKind;
    const WARMS_UP: bool;

    fn observe(
        values: &Values,
        jitter: &mut impl FnMut(f32) -> f32,
        at: MonotonicTimestamp,
    ) -> MeasurementUpdate;
}

pub struct Bmx280;

impl Observe for Bmx280 {
    const SENSOR: Sensor = Sensor::Bmx280;
    const KIND: SensorKind = SensorKind::Bmx280;
    const WARMS_UP: bool = false;

    fn observe(
        values: &Values,
        jitter: &mut impl FnMut(f32) -> f32,
        at: MonotonicTimestamp,
    ) -> MeasurementUpdate {
        MeasurementUpdate::Bmx280(Bmx280Update::bme280(
            Celsius(jitter(values.temperature)),
            RelativeHumidity(jitter(values.humidity)),
            Pascal(jitter(values.pressure) * 100.0),
            at,
        ))
    }
}

pub struct Scd4x;

impl Observe for Scd4x {
    const SENSOR: Sensor = Sensor::Scd4x;
    const KIND: SensorKind = SensorKind::Scd4x;
    const WARMS_UP: bool = true;

    fn observe(
        values: &Values,
        jitter: &mut impl FnMut(f32) -> f32,
        at: MonotonicTimestamp,
    ) -> MeasurementUpdate {
        MeasurementUpdate::Scd4x(Scd4xUpdate {
            co2: Ppm(jitter(values.co2)),
            temperature: Celsius(jitter(values.temperature)),
            humidity: RelativeHumidity(jitter(values.humidity)),
            timestamp: at,
        })
    }
}

pub struct Sds011;

impl Observe for Sds011 {
    const SENSOR: Sensor = Sensor::Sds011;
    const KIND: SensorKind = SensorKind::Sds011;
    const WARMS_UP: bool = true;

    fn observe(
        values: &Values,
        jitter: &mut impl FnMut(f32) -> f32,
        at: MonotonicTimestamp,
    ) -> MeasurementUpdate {
        MeasurementUpdate::Sds011(Sds011Update {
            pm25: MicrogramsPerCubicMeter(jitter(values.pm25)),
            pm10: MicrogramsPerCubicMeter(jitter(values.pm10)),
            timestamp: at,
        })
    }
}

pub struct Noise;

impl Observe for Noise {
    const SENSOR: Sensor = Sensor::Noise;
    const KIND: SensorKind = SensorKind::Microphone;
    const WARMS_UP: bool = false;

    fn observe(
        values: &Values,
        jitter: &mut impl FnMut(f32) -> f32,
        at: MonotonicTimestamp,
    ) -> MeasurementUpdate {
        MeasurementUpdate::Microphone(NoiseUpdate {
            average: Decibels(jitter(values.noise_average)),
            maximum: Decibels(jitter(values.noise_maximum)),
            timestamp: at,
        })
    }
}

/// A simulated driver of the sensor `O`.
pub struct SimDriver<O> {
    jitter: Jitter,
    _sensor: PhantomData<O>,
}

impl<O: Observe> SimDriver<O> {
    pub fn new(seed: Option<u64>) -> Self {
        Self {
            jitter: Jitter::new(seed, O::SENSOR),
            _sensor: PhantomData,
        }
    }
}

impl<O: Observe> SensorDriver for SimDriver<O> {
    const KIND: SensorKind = O::KIND;
    const WARMS_UP: bool = O::WARMS_UP;
    type Error = SimError;

    async fn init(&mut self) -> Result<(), SimError> {
        match world().lock().sensors[O::SENSOR.index()].fault {
            Some(SensorFault::Unavailable) => Err(SimError::Absent),
            Some(SensorFault::Init) => Err(SimError::Init),
            _ => Ok(()),
        }
    }

    async fn sample<C: Clock>(&mut self, clock: &C) -> Result<Option<MeasurementUpdate>, SimError> {
        let (fault, warm_until_ms, values, relative) = {
            let world = world().lock();
            let sensor = &world.sensors[O::SENSOR.index()];
            (
                sensor.fault,
                sensor.warm_until_ms,
                world.values,
                world.jitter,
            )
        };
        match fault {
            Some(SensorFault::Unavailable | SensorFault::Read) => return Err(SimError::Read),
            Some(SensorFault::Timeout) => {
                Timer::after_millis(TIMEOUT_MS).await;
                return Err(SimError::Timeout);
            }
            Some(SensorFault::NoData) => return Ok(None),
            Some(SensorFault::Init) | None => {}
        }
        if Instant::now().as_millis() < warm_until_ms {
            return Ok(None);
        }
        let jitter = &mut self.jitter;
        Ok(Some(O::observe(
            &values,
            &mut |value| jitter.apply(value, relative),
            clock.now(),
        )))
    }

    fn poll_interval_ms(&self) -> u32 {
        world().lock().sensors[O::SENSOR.index()].interval_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_sequence() {
        let sequence = |seed, sensor| {
            let mut jitter = Jitter::new(Some(seed), sensor);
            (0..8).map(|_| jitter.apply(100.0, 0.1)).collect::<Vec<_>>()
        };
        assert_eq!(sequence(42, Sensor::Scd4x), sequence(42, Sensor::Scd4x));
        assert_ne!(sequence(42, Sensor::Scd4x), sequence(43, Sensor::Scd4x));
        assert_ne!(sequence(42, Sensor::Scd4x), sequence(42, Sensor::Noise));
        assert!(
            sequence(42, Sensor::Scd4x)
                .iter()
                .all(|value| (90.0..=110.0).contains(value))
        );
    }

    #[test]
    fn without_jitter_a_value_is_exact() {
        let mut jitter = Jitter::new(Some(7), Sensor::Bmx280);
        assert_eq!(jitter.apply(22.5, 0.0), 22.5);
    }
}
