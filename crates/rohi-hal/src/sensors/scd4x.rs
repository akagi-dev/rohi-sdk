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
//! SCD40 / SCD41 driver.
//!
//! Probing stops any running measurement, reads the serial number (validating
//! its CRC) and the sensor variant, then starts periodic measurement (a new
//! value every 5 s). Each sample waits on the data-ready flag, reads CO2,
//! temperature and humidity and validates every word with CRC-8.
//!
//! The first valid measurement arrives about 5 s after start, so the sensor
//! reports `WarmingUp` until then.

use core::fmt::Debug;

use embedded_hal::i2c::ErrorKind;
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;
use rohi_sensor_core::{
    Celsius, MeasurementUpdate, Ppm, RelativeHumidity, Scd4xUpdate, SensorErrorCode, SensorKind,
};

use crate::runtime::{Clock, DriverError, SensorDriver};

/// Fixed I2C address.
pub const ADDRESS: u8 = 0x62;
/// Periodic measurement interval of the sensor.
pub const MEASUREMENT_INTERVAL_MS: u32 = 5_000;
/// Interval between data-ready polls while waiting for a measurement.
pub const POLL_INTERVAL_MS: u32 = 500;
/// Data-ready polls before reporting a timeout.
pub const MAX_READY_POLLS: u32 = 20;

const CMD_START_PERIODIC: u16 = 0x21B1;
const CMD_READ_MEASUREMENT: u16 = 0xEC05;
const CMD_STOP_PERIODIC: u16 = 0x3F86;
const CMD_DATA_READY: u16 = 0xE4B8;
const CMD_SERIAL_NUMBER: u16 = 0x3682;
const CMD_SENSOR_VARIANT: u16 = 0x202F;
const CMD_REINIT: u16 = 0x3646;

const STOP_PERIODIC_DELAY_MS: u32 = 500;
const REINIT_DELAY_MS: u32 = 30;
const COMMAND_DELAY_MS: u32 = 1;
const SERIAL_DELAY_MS: u32 = 1;

/// CO2 above this value is outside of the sensor range and treated as invalid.
const MAX_VALID_CO2_PPM: u16 = 40_000;

/// Sensor variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// SCD40.
    Scd40,
    /// SCD41.
    Scd41,
}

/// SCD4x driver error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// The device did not acknowledge its address.
    Absent,
    /// Bus failure.
    Bus(E),
    /// A received word failed its CRC check.
    Crc,
    /// The sensor variant is not SCD40 / SCD41.
    UnsupportedVariant(u16),
    /// No measurement became ready in time.
    Timeout,
    /// The sensor returned out-of-range values.
    InvalidData,
}

impl<E: embedded_hal::i2c::Error> From<E> for Error<E> {
    fn from(e: E) -> Self {
        match e.kind() {
            ErrorKind::NoAcknowledge(_) => Error::Absent,
            _ => Error::Bus(e),
        }
    }
}

impl<E: Debug> DriverError for Error<E> {
    fn code(&self) -> SensorErrorCode {
        match self {
            Error::Absent | Error::Timeout => SensorErrorCode::Timeout,
            Error::Bus(_) | Error::UnsupportedVariant(_) => SensorErrorCode::Bus,
            Error::Crc => SensorErrorCode::Crc,
            Error::InvalidData => SensorErrorCode::InvalidData,
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Error::Absent | Error::UnsupportedVariant(_))
    }
}

/// Sensirion CRC-8: polynomial `0x31`, initial value `0xFF`.
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0xFFu8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x31
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Decode `N` CRC-protected words (3 bytes each: 2 data + CRC).
pub fn decode_words<const N: usize>(bytes: &[u8]) -> Result<[u16; N], DecodeError> {
    if bytes.len() != N * 3 {
        return Err(DecodeError::Length);
    }
    let mut words = [0u16; N];
    for (word, chunk) in words.iter_mut().zip(bytes.as_chunks::<3>().0) {
        if crc8(&chunk[..2]) != chunk[2] {
            return Err(DecodeError::Crc);
        }
        *word = u16::from_be_bytes([chunk[0], chunk[1]]);
    }
    Ok(words)
}

/// Word decoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Unexpected number of bytes.
    Length,
    /// CRC mismatch.
    Crc,
}

/// A decoded observation without timestamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// CO2 in ppm.
    pub co2: u16,
    /// Temperature in °C.
    pub temperature: f32,
    /// Relative humidity in percent.
    pub humidity: f32,
}

impl Reading {
    /// Convert raw words `[co2, temperature, humidity]` using the datasheet
    /// formulas `T = -45 + 175 * raw / 65535`, `RH = 100 * raw / 65535`.
    pub fn from_words(words: [u16; 3]) -> Self {
        Self {
            co2: words[0],
            temperature: -45.0 + 175.0 * words[1] as f32 / 65_535.0,
            humidity: 100.0 * words[2] as f32 / 65_535.0,
        }
    }
}

/// SCD40 / SCD41 driver.
pub struct Scd4x<I2C, D> {
    i2c: I2C,
    delay: D,
    variant: Option<Variant>,
}

impl<I2C: I2c, D: DelayNs> Scd4x<I2C, D> {
    /// Create a driver; call [`SensorDriver::init`] (or [`Scd4x::initialize`]) next.
    pub fn new(i2c: I2C, delay: D) -> Self {
        Self {
            i2c,
            delay,
            variant: None,
        }
    }

    /// Detected variant.
    pub fn variant(&self) -> Option<Variant> {
        self.variant
    }

    async fn command(&mut self, cmd: u16) -> Result<(), Error<I2C::Error>> {
        if let Err(e) = self.i2c.write(ADDRESS, &cmd.to_be_bytes()).await {
            log::debug!("[scd4x] command {:#06x} failed: {:?}", cmd, e);
            return Err(e.into());
        }
        log::trace!("[scd4x] command {:#06x} sent", cmd);
        Ok(())
    }

    async fn read_words<const N: usize>(
        &mut self,
        cmd: u16,
        delay_ms: u32,
    ) -> Result<[u16; N], Error<I2C::Error>> {
        self.command(cmd).await?;
        self.delay.delay_ms(delay_ms).await;
        // 3 bytes per word, at most 3 words are ever read.
        let mut buf = [0u8; 9];
        let buf = &mut buf[..N * 3];
        if let Err(e) = self.i2c.read(ADDRESS, buf).await {
            log::debug!(
                "[scd4x] read of {} word(s) after command {:#06x} failed: {:?}",
                N,
                cmd,
                e
            );
            return Err(e.into());
        }
        log::trace!("[scd4x] command {:#06x} response: {:02x?}", cmd, buf);
        decode_words(buf).map_err(|e| {
            log::debug!(
                "[scd4x] command {:#06x} response {:02x?} rejected: {:?}",
                cmd,
                buf,
                e
            );
            Error::Crc
        })
    }

    /// Stop any running measurement, identify the sensor and start periodic
    /// measurement.
    pub async fn initialize(&mut self) -> Result<Variant, Error<I2C::Error>> {
        // Stopping also acts as the presence probe; it is ignored by an idle sensor.
        self.command(CMD_STOP_PERIODIC).await?;
        self.delay.delay_ms(STOP_PERIODIC_DELAY_MS).await;
        self.command(CMD_REINIT).await?;
        self.delay.delay_ms(REINIT_DELAY_MS).await;

        let serial: [u16; 3] = self.read_words(CMD_SERIAL_NUMBER, SERIAL_DELAY_MS).await?;
        log::debug!("[scd4x] serial number: {:04x?}", serial);
        let [variant_word]: [u16; 1] = self
            .read_words(CMD_SENSOR_VARIANT, COMMAND_DELAY_MS)
            .await?;
        let variant = match variant_word >> 12 {
            0x0 => Variant::Scd40,
            0x1 => Variant::Scd41,
            _ => {
                log::debug!("[scd4x] unsupported variant word {:#06x}", variant_word);
                return Err(Error::UnsupportedVariant(variant_word));
            }
        };
        log::debug!(
            "[scd4x] detected {:?} (word {:#06x})",
            variant,
            variant_word
        );

        self.command(CMD_START_PERIODIC).await?;
        self.variant = Some(variant);
        Ok(variant)
    }

    /// Whether a new measurement can be read.
    pub async fn data_ready(&mut self) -> Result<bool, Error<I2C::Error>> {
        let [status]: [u16; 1] = self.read_words(CMD_DATA_READY, COMMAND_DELAY_MS).await?;
        Ok(status & 0x07FF != 0)
    }

    /// Wait for the data-ready flag, then read and decode the measurement.
    pub async fn measure(&mut self) -> Result<Reading, Error<I2C::Error>> {
        let mut polls = 0;
        while !self.data_ready().await? {
            polls += 1;
            if polls >= MAX_READY_POLLS {
                log::debug!("[scd4x] no data ready after {} polls", polls);
                return Err(Error::Timeout);
            }
            self.delay.delay_ms(POLL_INTERVAL_MS).await;
        }
        let words: [u16; 3] = self
            .read_words(CMD_READ_MEASUREMENT, COMMAND_DELAY_MS)
            .await?;
        let reading = Reading::from_words(words);
        // A zero CO2 value is reported when no valid measurement exists yet.
        if reading.co2 == 0 || reading.co2 > MAX_VALID_CO2_PPM {
            log::debug!(
                "[scd4x] invalid reading {:?} from words {:?}",
                reading,
                words
            );
            return Err(Error::InvalidData);
        }
        log::debug!("[scd4x] reading: {:?}", reading);
        Ok(reading)
    }
}

impl<I2C: I2c, D: DelayNs> SensorDriver for Scd4x<I2C, D> {
    const KIND: SensorKind = SensorKind::Scd4x;
    const WARMS_UP: bool = true;
    type Error = Error<I2C::Error>;

    async fn init(&mut self) -> Result<(), Self::Error> {
        self.initialize().await.map(|_| ())
    }

    async fn sample<C: Clock>(
        &mut self,
        clock: &C,
    ) -> Result<Option<MeasurementUpdate>, Self::Error> {
        let reading = self.measure().await?;
        Ok(Some(MeasurementUpdate::Scd4x(Scd4xUpdate {
            co2: Ppm(reading.co2 as f32),
            temperature: Celsius(reading.temperature),
            humidity: RelativeHumidity(reading.humidity),
            timestamp: clock.now(),
        })))
    }

    fn poll_interval_ms(&self) -> u32 {
        MEASUREMENT_INTERVAL_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FixedClock, MockI2c, NoDelay, block_on};
    use rohi_sensor_core::MonotonicTimestamp;

    fn word(w: u16) -> [u8; 3] {
        let b = w.to_be_bytes();
        [b[0], b[1], crc8(&b)]
    }

    fn words(ws: &[u16]) -> std::vec::Vec<u8> {
        ws.iter().flat_map(|&w| word(w)).collect()
    }

    fn init_bus(variant: u16) -> MockI2c {
        let mut bus = MockI2c::default();
        bus.push_read(&words(&[0xF896, 0x31A8, 0x2A7F])); // serial number
        bus.push_read(&words(&[variant]));
        bus
    }

    #[test]
    fn crc_matches_datasheet_example() {
        assert_eq!(crc8(&[0xBE, 0xEF]), 0x92);
    }

    #[test]
    fn decode_rejects_bad_crc_and_length() {
        let mut bytes = words(&[1, 2]);
        assert_eq!(decode_words::<2>(&bytes), Ok([1, 2]));
        bytes[5] ^= 1;
        assert_eq!(decode_words::<2>(&bytes), Err(DecodeError::Crc));
        assert_eq!(decode_words::<2>(&bytes[..4]), Err(DecodeError::Length));
    }

    #[test]
    fn conversion_matches_datasheet_formulas() {
        let r = Reading::from_words([500, 0x6667, 0x8000]);
        assert_eq!(r.co2, 500);
        assert!((r.temperature - 25.0).abs() < 0.01, "{}", r.temperature);
        assert!((r.humidity - 50.0).abs() < 0.01);
    }

    #[test]
    fn initializes_and_distinguishes_variants() {
        let mut s = Scd4x::new(init_bus(0x1440), NoDelay);
        assert_eq!(block_on(s.initialize()), Ok(Variant::Scd41));
        let mut s = Scd4x::new(init_bus(0x0440), NoDelay);
        assert_eq!(block_on(s.initialize()), Ok(Variant::Scd40));
        let mut s = Scd4x::new(init_bus(0x5440), NoDelay);
        assert_eq!(
            block_on(s.initialize()),
            Err(Error::UnsupportedVariant(0x5440))
        );
    }

    #[test]
    fn absent_sensor_nacks() {
        let mut bus = MockI2c::default();
        bus.push_error(MockI2c::nack());
        // The first operation is a write, which the mock never fails; absence
        // therefore shows up on the first read.
        let mut s = Scd4x::new(bus, NoDelay);
        let err = block_on(s.initialize()).unwrap_err();
        assert!(err.is_absent());
    }

    #[test]
    fn waits_for_data_ready_then_reads() {
        let mut bus = MockI2c::default();
        bus.push_read(&words(&[0x0000])); // not ready
        bus.push_read(&words(&[0x8006])); // ready
        bus.push_read(&words(&[612, 0x6667, 0x8000]));
        let mut s = Scd4x::new(bus, NoDelay);
        let clock = FixedClock(MonotonicTimestamp::from_millis(9));
        let Some(MeasurementUpdate::Scd4x(u)) = block_on(s.sample(&clock)).unwrap() else {
            panic!("expected SCD4x update")
        };
        assert_eq!(u.co2, Ppm(612.0));
        assert_eq!(u.timestamp, MonotonicTimestamp::from_millis(9));
    }

    #[test]
    fn crc_failure_then_recovery() {
        let mut bus = MockI2c::default();
        bus.push_read(&words(&[0x8006]));
        let mut corrupted = words(&[612, 0x6667, 0x8000]);
        corrupted[2] ^= 0xFF;
        bus.push_read(&corrupted);
        bus.push_read(&words(&[0x8006]));
        bus.push_read(&words(&[612, 0x6667, 0x8000]));
        let mut s = Scd4x::new(bus, NoDelay);
        let err = block_on(s.measure()).unwrap_err();
        assert_eq!(err, Error::Crc);
        assert_eq!(err.code(), SensorErrorCode::Crc);
        assert!(block_on(s.measure()).is_ok());
    }

    #[test]
    fn zero_co2_is_invalid() {
        let mut bus = MockI2c::default();
        bus.push_read(&words(&[0x8006]));
        bus.push_read(&words(&[0, 0x6667, 0x8000]));
        let mut s = Scd4x::new(bus, NoDelay);
        assert_eq!(block_on(s.measure()), Err(Error::InvalidData));
    }

    #[test]
    fn data_never_ready_times_out() {
        let mut bus = MockI2c::default();
        for _ in 0..MAX_READY_POLLS {
            bus.push_read(&words(&[0x0000]));
        }
        let mut s = Scd4x::new(bus, NoDelay);
        assert_eq!(block_on(s.measure()), Err(Error::Timeout));
    }
}
