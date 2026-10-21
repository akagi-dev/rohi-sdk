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
//! BMP280 / BME280 driver.
//!
//! Detects the chip variant from its ID register, reads factory calibration and
//! performs compensated forced-mode measurements using the integer algorithms
//! from the Bosch datasheet. BMP280 has no humidity channel.
//!
//! # Settings
//!
//! Temperature oversampling x2, pressure x4, humidity x1, IIR filter off,
//! forced mode (one conversion per [`SensorDriver::sample`] call; the sensor
//! sleeps in between). One conversion takes at most about 19 ms.
//! [`MEASUREMENT_INTERVAL_MS`] is the cadence between conversions.

use core::fmt::Debug;

use embedded_hal::i2c::ErrorKind;
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;
use rohi_sensor_core::{
    Bmx280Update, Celsius, MeasurementUpdate, Pascal, RelativeHumidity, SensorErrorCode, SensorKind,
};

use crate::runtime::{Clock, DriverError, SensorDriver};

/// Default I2C address (SDO low).
pub const ADDRESS_PRIMARY: u8 = 0x76;
/// Alternative I2C address (SDO high).
pub const ADDRESS_SECONDARY: u8 = 0x77;
/// Delay between two measurements.
pub const MEASUREMENT_INTERVAL_MS: u32 = 10_000;

const REG_CALIB_TP: u8 = 0x88;
const REG_CALIB_H1: u8 = 0xA1;
const REG_ID: u8 = 0xD0;
const REG_RESET: u8 = 0xE0;
const REG_CALIB_H2: u8 = 0xE1;
const REG_CTRL_HUM: u8 = 0xF2;
const REG_STATUS: u8 = 0xF3;
const REG_CTRL_MEAS: u8 = 0xF4;
const REG_CONFIG: u8 = 0xF5;
const REG_DATA: u8 = 0xF7;

const RESET_COMMAND: u8 = 0xB6;
const STATUS_MEASURING: u8 = 1 << 3;
const STATUS_IM_UPDATE: u8 = 1;

const OSRS_TEMPERATURE: u8 = 0b010;
const OSRS_PRESSURE: u8 = 0b011;
const OSRS_HUMIDITY: u8 = 0b001;
const MODE_FORCED: u8 = 0b01;

const POLL_STEP_MS: u32 = 5;
const POLL_STEPS: u32 = 12;

/// Skipped measurement marker of the 20 bit temperature / pressure registers.
const ADC_SKIPPED: i32 = 0x80000;

/// Chip variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    /// Temperature and pressure only.
    Bmp280,
    /// Temperature, pressure and humidity.
    Bme280,
}

/// BMx280 driver error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// The device did not acknowledge its address.
    Absent,
    /// Bus failure.
    Bus(E),
    /// The ID register holds an unsupported chip ID.
    UnsupportedChip(u8),
    /// The calibration data is invalid.
    Calibration,
    /// The conversion did not finish in time.
    Timeout,
    /// The sensor returned out-of-range or skipped values.
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
            Error::Bus(_) | Error::UnsupportedChip(_) => SensorErrorCode::Bus,
            Error::Calibration => SensorErrorCode::Calibration,
            Error::InvalidData => SensorErrorCode::InvalidData,
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Error::Absent | Error::UnsupportedChip(_))
    }
}

/// Factory calibration coefficients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Calibration {
    t1: u16,
    t2: i16,
    t3: i16,
    p1: u16,
    p2: i16,
    p3: i16,
    p4: i16,
    p5: i16,
    p6: i16,
    p7: i16,
    p8: i16,
    p9: i16,
    h1: u8,
    h2: i16,
    h3: u8,
    h4: i16,
    h5: i16,
    h6: i8,
}

fn le_u16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn le_i16(b: &[u8], i: usize) -> i16 {
    i16::from_le_bytes([b[i], b[i + 1]])
}

impl Calibration {
    /// Parse the 24 temperature / pressure bytes read from `0x88`.
    pub fn from_temperature_pressure(b: &[u8; 24]) -> Self {
        Self {
            t1: le_u16(b, 0),
            t2: le_i16(b, 2),
            t3: le_i16(b, 4),
            p1: le_u16(b, 6),
            p2: le_i16(b, 8),
            p3: le_i16(b, 10),
            p4: le_i16(b, 12),
            p5: le_i16(b, 14),
            p6: le_i16(b, 16),
            p7: le_i16(b, 18),
            p8: le_i16(b, 20),
            p9: le_i16(b, 22),
            ..Self::default()
        }
    }

    /// Add the BME280 humidity coefficients: `h1` from `0xA1`, `rest` from `0xE1`.
    pub fn with_humidity(mut self, h1: u8, rest: &[u8; 7]) -> Self {
        self.h1 = h1;
        self.h2 = le_i16(rest, 0);
        self.h3 = rest[2];
        self.h4 = ((rest[3] as i8 as i16) << 4) | (rest[4] & 0x0F) as i16;
        self.h5 = ((rest[5] as i8 as i16) << 4) | (rest[4] >> 4) as i16;
        self.h6 = rest[6] as i8;
        self
    }

    fn is_plausible(&self) -> bool {
        self.t1 != 0 && self.t1 != 0xFFFF && self.p1 != 0 && self.p1 != 0xFFFF
    }

    /// Returns `(t_fine, temperature in 0.01 °C)`.
    pub fn compensate_temperature(&self, adc_t: i32) -> (i32, i32) {
        let t1 = self.t1 as i32;
        let var1 = (((adc_t >> 3) - (t1 << 1)) * self.t2 as i32) >> 11;
        let d = (adc_t >> 4) - t1;
        let var2 = (((d * d) >> 12) * self.t3 as i32) >> 14;
        let t_fine = var1 + var2;
        (t_fine, (t_fine * 5 + 128) >> 8)
    }

    /// Pressure in Pascals, `None` if the coefficients make it undefined.
    pub fn compensate_pressure(&self, adc_p: i32, t_fine: i32) -> Option<f32> {
        let mut var1 = t_fine as i64 - 128_000;
        let mut var2 = var1 * var1 * self.p6 as i64;
        var2 += (var1 * self.p5 as i64) << 17;
        var2 += (self.p4 as i64) << 35;
        var1 = ((var1 * var1 * self.p3 as i64) >> 8) + ((var1 * self.p2 as i64) << 12);
        var1 = (((1i64 << 47) + var1) * self.p1 as i64) >> 33;
        if var1 == 0 {
            return None;
        }
        let mut p = 1_048_576 - adc_p as i64;
        p = (((p << 31) - var2) * 3125) / var1;
        var1 = (self.p9 as i64 * (p >> 13) * (p >> 13)) >> 25;
        var2 = (self.p8 as i64 * p) >> 19;
        p = ((p + var1 + var2) >> 8) + ((self.p7 as i64) << 4);
        // Q24.8 Pascals.
        Some(p as f32 / 256.0)
    }

    /// Relative humidity in percent.
    pub fn compensate_humidity(&self, adc_h: i32, t_fine: i32) -> f32 {
        let mut v = t_fine - 76_800;
        v = ((((adc_h << 14) - ((self.h4 as i32) << 20) - (self.h5 as i32 * v)) + 16_384) >> 15)
            * (((((((v * self.h6 as i32) >> 10) * (((v * self.h3 as i32) >> 11) + 32_768))
                >> 10)
                + 2_097_152)
                * self.h2 as i32
                + 8_192)
                >> 14);
        v -= ((((v >> 15) * (v >> 15)) >> 7) * self.h1 as i32) >> 4;
        let v = v.clamp(0, 419_430_400);
        (v >> 12) as f32 / 1024.0
    }
}

/// Raw conversion result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSample {
    adc_p: i32,
    adc_t: i32,
    adc_h: Option<i32>,
}

impl RawSample {
    /// Parse the data registers starting at `0xF7`; humidity needs 8 bytes.
    pub fn parse(data: &[u8], with_humidity: bool) -> Self {
        let adc_p = ((data[0] as i32) << 12) | ((data[1] as i32) << 4) | (data[2] as i32 >> 4);
        let adc_t = ((data[3] as i32) << 12) | ((data[4] as i32) << 4) | (data[5] as i32 >> 4);
        let adc_h = with_humidity.then(|| ((data[6] as i32) << 8) | data[7] as i32);
        Self {
            adc_p,
            adc_t,
            adc_h,
        }
    }
}

/// A compensated observation without timestamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Temperature in °C.
    pub temperature: f32,
    /// Pressure in Pascals.
    pub pressure: f32,
    /// Relative humidity in percent, BME280 only.
    pub humidity: Option<f32>,
}

/// BMP280 / BME280 driver.
pub struct Bmx280<I2C, D> {
    i2c: I2C,
    delay: D,
    address: u8,
    chip: Option<Chip>,
    calibration: Calibration,
}

impl<I2C: I2c, D: DelayNs> Bmx280<I2C, D> {
    /// Create a driver; call [`SensorDriver::init`] (or [`Bmx280::probe`]) next.
    pub fn new(i2c: I2C, delay: D, address: u8) -> Self {
        Self {
            i2c,
            delay,
            address,
            chip: None,
            calibration: Calibration::default(),
        }
    }

    /// Detected chip variant.
    pub fn chip(&self) -> Option<Chip> {
        self.chip
    }

    async fn read(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), Error<I2C::Error>> {
        self.i2c.write_read(self.address, &[reg], buf).await?;
        Ok(())
    }

    async fn write(&mut self, reg: u8, value: u8) -> Result<(), Error<I2C::Error>> {
        self.i2c.write(self.address, &[reg, value]).await?;
        Ok(())
    }

    /// Detect the chip variant without changing its configuration.
    pub async fn probe(&mut self) -> Result<Chip, Error<I2C::Error>> {
        let mut id = [0u8];
        self.read(REG_ID, &mut id).await?;
        match id[0] {
            0x56..=0x58 => Ok(Chip::Bmp280),
            0x60 => Ok(Chip::Bme280),
            other => Err(Error::UnsupportedChip(other)),
        }
    }

    /// Detect, reset and read calibration.
    pub async fn initialize(&mut self) -> Result<Chip, Error<I2C::Error>> {
        let chip = self.probe().await?;
        self.write(REG_RESET, RESET_COMMAND).await?;
        self.delay.delay_ms(5).await;
        self.wait_while(STATUS_IM_UPDATE).await?;

        let mut tp = [0u8; 24];
        self.read(REG_CALIB_TP, &mut tp).await?;
        let mut calibration = Calibration::from_temperature_pressure(&tp);
        if chip == Chip::Bme280 {
            let mut h1 = [0u8];
            let mut rest = [0u8; 7];
            self.read(REG_CALIB_H1, &mut h1).await?;
            self.read(REG_CALIB_H2, &mut rest).await?;
            calibration = calibration.with_humidity(h1[0], &rest);
        }
        if !calibration.is_plausible() {
            return Err(Error::Calibration);
        }

        self.write(REG_CONFIG, 0).await?;
        self.calibration = calibration;
        self.chip = Some(chip);
        Ok(chip)
    }

    async fn wait_while(&mut self, mask: u8) -> Result<(), Error<I2C::Error>> {
        for _ in 0..POLL_STEPS {
            let mut status = [0u8];
            self.read(REG_STATUS, &mut status).await?;
            if status[0] & mask == 0 {
                return Ok(());
            }
            self.delay.delay_ms(POLL_STEP_MS).await;
        }
        Err(Error::Timeout)
    }

    /// Perform one forced-mode conversion and compensate it.
    pub async fn measure(&mut self) -> Result<Reading, Error<I2C::Error>> {
        let chip = self.chip.ok_or(Error::Calibration)?;
        if chip == Chip::Bme280 {
            self.write(REG_CTRL_HUM, OSRS_HUMIDITY).await?;
        }
        let ctrl = (OSRS_TEMPERATURE << 5) | (OSRS_PRESSURE << 2) | MODE_FORCED;
        self.write(REG_CTRL_MEAS, ctrl).await?;
        self.delay.delay_ms(10).await;
        self.wait_while(STATUS_MEASURING).await?;

        let mut data = [0u8; 8];
        let len = if chip == Chip::Bme280 { 8 } else { 6 };
        self.read(REG_DATA, &mut data[..len]).await?;
        let raw = RawSample::parse(&data[..len], chip == Chip::Bme280);
        self.compensate(&raw)
    }

    fn compensate(&self, raw: &RawSample) -> Result<Reading, Error<I2C::Error>> {
        if raw.adc_t == ADC_SKIPPED || raw.adc_p == ADC_SKIPPED {
            return Err(Error::InvalidData);
        }
        let (t_fine, centi) = self.calibration.compensate_temperature(raw.adc_t);
        let temperature = centi as f32 / 100.0;
        let pressure = self
            .calibration
            .compensate_pressure(raw.adc_p, t_fine)
            .ok_or(Error::InvalidData)?;
        // Operating range from the datasheet: -40..85 °C, 300..1100 hPa.
        if !(-40.0..=85.0).contains(&temperature) || !(30_000.0..=110_000.0).contains(&pressure) {
            return Err(Error::InvalidData);
        }
        let humidity = match raw.adc_h {
            // 0x8000 means the humidity conversion was skipped.
            Some(0x8000) => return Err(Error::InvalidData),
            Some(adc_h) => Some(self.calibration.compensate_humidity(adc_h, t_fine)),
            None => None,
        };
        Ok(Reading {
            temperature,
            pressure,
            humidity,
        })
    }
}

impl<I2C: I2c, D: DelayNs> SensorDriver for Bmx280<I2C, D> {
    const KIND: SensorKind = SensorKind::Bmx280;
    const WARMS_UP: bool = false;
    type Error = Error<I2C::Error>;

    async fn init(&mut self) -> Result<(), Self::Error> {
        self.initialize().await.map(|_| ())
    }

    async fn sample<C: Clock>(
        &mut self,
        clock: &C,
    ) -> Result<Option<MeasurementUpdate>, Self::Error> {
        let reading = self.measure().await?;
        let now = clock.now();
        let update = match reading.humidity {
            Some(h) => Bmx280Update::bme280(
                Celsius(reading.temperature),
                RelativeHumidity(h),
                Pascal(reading.pressure),
                now,
            ),
            None => {
                Bmx280Update::bmp280(Celsius(reading.temperature), Pascal(reading.pressure), now)
            }
        };
        Ok(Some(MeasurementUpdate::Bmx280(update)))
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

    // Example calibration and raw values from the Bosch BMP280 datasheet.
    const CALIB: [u8; 24] = {
        let t1 = 27504u16.to_le_bytes();
        let t2 = 26435i16.to_le_bytes();
        let t3 = (-1000i16).to_le_bytes();
        let p1 = 36477u16.to_le_bytes();
        let p2 = (-10685i16).to_le_bytes();
        let p3 = 3024i16.to_le_bytes();
        let p4 = 2855i16.to_le_bytes();
        let p5 = 140i16.to_le_bytes();
        let p6 = (-7i16).to_le_bytes();
        let p7 = 15500i16.to_le_bytes();
        let p8 = (-14600i16).to_le_bytes();
        let p9 = 6000i16.to_le_bytes();
        [
            t1[0], t1[1], t2[0], t2[1], t3[0], t3[1], p1[0], p1[1], p2[0], p2[1], p3[0], p3[1],
            p4[0], p4[1], p5[0], p5[1], p6[0], p6[1], p7[0], p7[1], p8[0], p8[1], p9[0], p9[1],
        ]
    };

    #[test]
    fn datasheet_temperature_and_pressure() {
        let c = Calibration::from_temperature_pressure(&CALIB);
        let (t_fine, centi) = c.compensate_temperature(519_888);
        assert_eq!(centi, 2508);
        let p = c.compensate_pressure(415_148, t_fine).unwrap();
        assert!((p - 100_653.27).abs() < 0.5, "pressure {p}");
    }

    #[test]
    fn humidity_is_clamped_to_valid_range() {
        let c = Calibration::from_temperature_pressure(&CALIB)
            .with_humidity(75, &[0x6E, 0x01, 0x00, 0x13, 0x2B, 0x03, 0x1E]);
        let (t_fine, _) = c.compensate_temperature(519_888);
        for adc_h in [0, 20_000, 40_000, 65_535] {
            let h = c.compensate_humidity(adc_h, t_fine);
            assert!((0.0..=100.0).contains(&h), "humidity {h} for {adc_h}");
        }
        assert!(c.compensate_humidity(50_000, t_fine) > c.compensate_humidity(30_000, t_fine));
    }

    #[test]
    fn humidity_calibration_packing() {
        let c =
            Calibration::default().with_humidity(75, &[0x6E, 0x01, 0x00, 0xFE, 0x1F, 0x03, 0x1E]);
        assert_eq!(c.h2, 366);
        assert_eq!(c.h4, (-2 << 4) | 0xF);
        assert_eq!(c.h5, (3 << 4) | 1);
        assert_eq!(c.h6, 30);
    }

    fn bmp_bus() -> MockI2c {
        let mut bus = MockI2c::default();
        bus.push_read(&[0x58]); // chip id
        bus.push_read(&[0x00]); // im_update clear
        bus.push_read(&CALIB);
        bus
    }

    fn push_measurement(bus: &mut MockI2c) {
        bus.push_read(&[0x00]); // not measuring
        // adc_p = 415148 (0x65A5C), adc_t = 519888 (0x7EED0)
        bus.push_read(&[0x65, 0x5A, 0xC0, 0x7E, 0xED, 0x00]);
    }

    #[test]
    fn detects_bmp280_and_measures() {
        let mut bus = bmp_bus();
        push_measurement(&mut bus);
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        assert_eq!(block_on(sensor.initialize()), Ok(Chip::Bmp280));
        let clock = FixedClock(MonotonicTimestamp::from_millis(42));
        let update = block_on(sensor.sample(&clock)).unwrap().unwrap();
        let MeasurementUpdate::Bmx280(u) = update else {
            panic!("wrong sensor")
        };
        assert!(u.humidity.is_none());
        assert!((u.temperature.0 - 25.08).abs() < 0.01);
        assert!((u.pressure.0 - 100_653.27).abs() < 0.5);
        assert_eq!(u.timestamp, MonotonicTimestamp::from_millis(42));
    }

    #[test]
    fn detects_bme280() {
        let mut bus = MockI2c::default();
        bus.push_read(&[0x60]);
        bus.push_read(&[0x00]);
        bus.push_read(&CALIB);
        bus.push_read(&[75]);
        bus.push_read(&[0x6E, 0x01, 0x00, 0x13, 0x2B, 0x03, 0x1E]);
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        assert_eq!(block_on(sensor.initialize()), Ok(Chip::Bme280));
        assert_eq!(sensor.chip(), Some(Chip::Bme280));
    }

    #[test]
    fn absence_is_distinguished_from_bus_failure() {
        let mut bus = MockI2c::default();
        bus.push_error(MockI2c::nack());
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        let err = block_on(sensor.probe()).unwrap_err();
        assert_eq!(err, Error::Absent);
        assert!(err.is_absent());

        let mut bus = MockI2c::default();
        bus.push_error(ErrorKind::Bus);
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        let err = block_on(sensor.probe()).unwrap_err();
        assert_eq!(err, Error::Bus(ErrorKind::Bus));
        assert!(!err.is_absent());
    }

    #[test]
    fn unsupported_chip_id_is_reported() {
        let mut bus = MockI2c::default();
        bus.push_read(&[0x42]);
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        assert_eq!(block_on(sensor.probe()), Err(Error::UnsupportedChip(0x42)));
    }

    #[test]
    fn transient_bus_error_then_recovery() {
        let mut bus = bmp_bus();
        bus.push_error(ErrorKind::Bus);
        push_measurement(&mut bus);
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        block_on(sensor.initialize()).unwrap();
        assert!(matches!(block_on(sensor.measure()), Err(Error::Bus(_))));
        assert!(block_on(sensor.measure()).is_ok());
    }

    #[test]
    fn skipped_conversion_is_invalid() {
        let mut bus = bmp_bus();
        bus.push_read(&[0x00]);
        bus.push_read(&[0x80, 0x00, 0x00, 0x80, 0x00, 0x00]);
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        block_on(sensor.initialize()).unwrap();
        assert_eq!(block_on(sensor.measure()), Err(Error::InvalidData));
    }

    #[test]
    fn invalid_calibration_is_rejected() {
        let mut bus = MockI2c::default();
        bus.push_read(&[0x58]);
        bus.push_read(&[0x00]);
        bus.push_read(&[0xFF; 24]);
        let mut sensor = Bmx280::new(bus, NoDelay, ADDRESS_PRIMARY);
        assert_eq!(block_on(sensor.initialize()), Err(Error::Calibration));
    }
}
