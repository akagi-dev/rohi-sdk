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
//! SDS011 particulate matter sensor driver.
//!
//! The sensor is kept in query mode and asleep between cycles, so the fan and
//! the laser run for the warm-up time only:
//!
//! ```text
//! wake -> query mode -> warm-up -> query -> sleep -> wait Config::sleep_ms
//! ```
//!
//! The command flow follows the community
//! [`sds011-rs`](https://github.com/kenohassler/sds011-rs) driver (see
//! [`protocol`]); this implementation adds what the runtime needs:
//!
//! * every UART read is bounded by [`READ_TIMEOUT_MS`], a silent sensor ends
//!   in [`Error::NoResponse`] instead of waiting forever;
//! * stale bytes (frames of a sensor still in active mode, a truncated reply
//!   of a failed cycle) are discarded before a session, the reader skips
//!   garbage up to the next `AA` header and frames that are not the awaited
//!   reply, and the wake-up is retried up to [`INIT_ATTEMPTS`] times;
//! * if a session fails, the sensor is told to sleep (best effort) so the fan
//!   does not keep running;
//! * values outside of the sensor range are rejected.
//!
//! One cycle reports the single sample taken after [`Config::warmup_ms`].

pub mod protocol;

use core::fmt::{self, Debug, Display};

use embedded_hal_async::delay::DelayNs;
use embedded_io_async::{ErrorKind, ErrorType, Read, ReadExactError, Write};
use rohi_sensor_core::{
    MeasurementUpdate, MicrogramsPerCubicMeter, Sds011Update, SensorErrorCode, SensorKind,
};

use self::protocol::{Command, Frame, HEAD, REPLY_LEN, Reply};
use crate::runtime::{Clock, DriverError, SensorDriver, with_timeout};

/// Bound of a single UART read.
pub const READ_TIMEOUT_MS: u32 = 5_000;
/// Wake-up attempts; a probe of a silent sensor takes at most
/// `INIT_ATTEMPTS * READ_TIMEOUT_MS`.
pub const INIT_ATTEMPTS: u32 = 3;
/// Highest value of the sensor range, in 0.1 µg/m³ (999.9 µg/m³).
pub const MAX_RAW_VALUE: u16 = 9_999;

/// Pause before waking the sensor, it may still be busy after a previous
/// command.
const SETTLE_MS: u32 = 500;
/// A read that stays silent this long means the input buffer is empty.
const DRAIN_TIMEOUT_MS: u32 = 50;
const DRAIN_MAX_READS: u32 = 64;
/// Bytes skipped while looking for a frame header.
const MAX_SKIPPED_BYTES: u32 = 64;
/// Frames ignored while waiting for a reply.
const MAX_SKIPPED_FRAMES: u32 = 3;

/// Timing parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Time the fan spins before the measurement is taken.
    pub warmup_ms: u32,
    /// Sleep time between two cycles.
    pub sleep_ms: u32,
}

impl Config {
    /// Default parameters: 30 s warm-up as recommended by the manufacturer,
    /// 90 s sleep.
    pub const DEFAULT: Self = Self {
        warmup_ms: 30_000,
        sleep_ms: 90_000,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// UART error as seen by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartError<E> {
    /// The UART failed.
    Io(E),
    /// A read did not complete within [`READ_TIMEOUT_MS`].
    TimedOut,
}

impl<E: Debug> Display for UartError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UartError::Io(e) => write!(f, "UART error: {e:?}"),
            UartError::TimedOut => f.write_str("UART read timed out"),
        }
    }
}

impl<E: Debug> core::error::Error for UartError<E> {}

impl<E: Debug> embedded_io_async::Error for UartError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            UartError::Io(_) => ErrorKind::Other,
            UartError::TimedOut => ErrorKind::TimedOut,
        }
    }
}

/// UART with bounded reads.
struct TimedUart<U, D> {
    uart: U,
    delay: D,
}

impl<U: Read, D: DelayNs> TimedUart<U, D> {
    /// Discard everything that is buffered or arrives while the line is busy.
    async fn drain(&mut self) {
        let mut buf = [0u8; 32];
        for _ in 0..DRAIN_MAX_READS {
            match with_timeout(&mut self.delay, DRAIN_TIMEOUT_MS, self.uart.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => {
                    log::debug!("[sds011] discarded {:02x?}", &buf[..n]);
                }
                _ => break,
            }
        }
    }
}

impl<U: ErrorType, D> ErrorType for TimedUart<U, D> {
    type Error = UartError<U::Error>;
}

impl<U: Read, D: DelayNs> Read for TimedUart<U, D> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match with_timeout(&mut self.delay, READ_TIMEOUT_MS, self.uart.read(buf)).await {
            Ok(result) => result.map_err(UartError::Io),
            Err(_) => {
                log::debug!("[sds011] UART read timed out after {} ms", READ_TIMEOUT_MS);
                Err(UartError::TimedOut)
            }
        }
    }
}

impl<U: Write, D> Write for TimedUart<U, D> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.uart.write(buf).await.map_err(UartError::Io)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.uart.flush().await.map_err(UartError::Io)
    }
}

/// SDS011 driver error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// UART failure.
    Uart(E),
    /// The sensor does not answer.
    NoResponse,
    /// The sensor sent only malformed or unexpected frames.
    InvalidFrames,
    /// The sensor reported a value outside of its range.
    OutOfRange,
}

impl<E> From<UartError<E>> for Error<E> {
    fn from(error: UartError<E>) -> Self {
        match error {
            UartError::Io(e) => Error::Uart(e),
            UartError::TimedOut => Error::NoResponse,
        }
    }
}

impl<E> From<ReadExactError<UartError<E>>> for Error<E> {
    fn from(error: ReadExactError<UartError<E>>) -> Self {
        match error {
            ReadExactError::UnexpectedEof => Error::NoResponse,
            ReadExactError::Other(e) => e.into(),
        }
    }
}

impl<E: Debug> DriverError for Error<E> {
    fn code(&self) -> SensorErrorCode {
        match self {
            Error::Uart(_) => SensorErrorCode::Bus,
            Error::NoResponse => SensorErrorCode::Timeout,
            Error::InvalidFrames => SensorErrorCode::Crc,
            Error::OutOfRange => SensorErrorCode::InvalidData,
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Error::NoResponse)
    }
}

/// SDS011 driver over any async UART.
pub struct Sds011<U, D> {
    uart: TimedUart<U, D>,
    delay: D,
    config: Config,
}

impl<U: Read + Write, D: DelayNs + Clone> Sds011<U, D> {
    /// Create a driver; call [`SensorDriver::init`] next.
    pub fn new(uart: U, delay: D, config: Config) -> Self {
        Self {
            uart: TimedUart {
                uart,
                delay: delay.clone(),
            },
            delay,
            config,
        }
    }

    async fn send(&mut self, command: Command) -> Result<(), Error<U::Error>> {
        let packet = protocol::encode(command);
        log::trace!("[sds011] sending {:?}: {:02x?}", command, packet);
        self.uart.write_all(&packet).await?;
        Ok(())
    }

    /// Read the next valid frame, skipping garbage before its header.
    async fn read_frame(&mut self) -> Result<Frame, Error<U::Error>> {
        let mut byte = [0u8; 1];
        let mut skipped = 0;
        loop {
            self.uart.read_exact(&mut byte).await?;
            if byte[0] == HEAD {
                break;
            }
            skipped += 1;
            if skipped > MAX_SKIPPED_BYTES {
                return Err(Error::InvalidFrames);
            }
        }
        let mut buf = [HEAD; REPLY_LEN];
        self.uart.read_exact(&mut buf[1..]).await?;
        log::trace!("[sds011] received {:02x?}", buf);
        protocol::parse(&buf).map_err(|e| {
            log::debug!("[sds011] rejected frame {:02x?}: {:?}", buf, e);
            Error::InvalidFrames
        })
    }

    /// Send `command` and return what `pick` extracts from the reply. Other
    /// frames (data of a sensor in active mode, corrupted frames) are skipped.
    async fn exchange<T>(
        &mut self,
        command: Command,
        pick: impl Fn(&Frame) -> Option<T>,
    ) -> Result<T, Error<U::Error>> {
        self.send(command).await?;
        for _ in 0..=MAX_SKIPPED_FRAMES {
            match self.read_frame().await {
                Ok(frame) => match pick(&frame) {
                    Some(value) => return Ok(value),
                    None => log::debug!("[sds011] unexpected {:?} after {:?}", frame, command),
                },
                Err(Error::InvalidFrames) => {}
                Err(e) => return Err(e),
            }
        }
        Err(Error::InvalidFrames)
    }

    /// Wake the sensor and put it into the query mode.
    async fn connect(&mut self) -> Result<(), Error<U::Error>> {
        self.delay.delay_ms(SETTLE_MS).await;
        self.exchange(Command::Wake, |f| {
            matches!(f.reply, Reply::Sleep { work: true }).then_some(())
        })
        .await?;
        self.exchange(Command::QueryMode, |f| {
            matches!(f.reply, Reply::ReportingMode { query: true }).then_some(())
        })
        .await?;
        let (id, (year, month, day)) = self
            .exchange(Command::Firmware, |f| match f.reply {
                Reply::Firmware { year, month, day } => Some((f.id, (year, month, day))),
                _ => None,
            })
            .await?;
        log::debug!(
            "[sds011] ready: ID {:#06x}, firmware 20{:02}.{:02}.{:02}",
            id,
            year,
            month,
            day
        );
        Ok(())
    }

    async fn query(&mut self) -> Result<(u16, u16), Error<U::Error>> {
        self.exchange(Command::Query, |f| match f.reply {
            Reply::Measurement { pm25, pm10 } => Some((pm25, pm10)),
            _ => None,
        })
        .await
    }

    async fn go_to_sleep(&mut self) -> Result<(), Error<U::Error>> {
        self.exchange(Command::Sleep, |f| {
            matches!(f.reply, Reply::Sleep { work: false }).then_some(())
        })
        .await
    }

    async fn run(&mut self, measure: bool) -> Result<Option<(u16, u16)>, Error<U::Error>> {
        let mut result = Err(Error::NoResponse);
        for attempt in 1..=INIT_ATTEMPTS {
            self.uart.drain().await;
            result = self.connect().await;
            match &result {
                Ok(()) => break,
                Err(Error::Uart(_)) => return result.map(|()| None),
                Err(e) => log::debug!(
                    "[sds011] wake-up attempt {}/{} failed: {:?}",
                    attempt,
                    INIT_ATTEMPTS,
                    e
                ),
            }
        }
        result?;

        if !measure {
            self.go_to_sleep().await?;
            return Ok(None);
        }
        log::debug!("[sds011] warm-up {} ms", self.config.warmup_ms);
        // The first answer after waking up is not trusted.
        self.query().await?;
        self.delay.delay_ms(self.config.warmup_ms).await;
        let sample = self.query().await?;
        self.go_to_sleep().await?;
        Ok(Some(sample))
    }

    /// Run a session; on failure tell the sensor to sleep, the fan may still
    /// be running. No reply is awaited, it is discarded in the next session.
    async fn session(&mut self, measure: bool) -> Result<Option<(u16, u16)>, Error<U::Error>> {
        let result = self.run(measure).await;
        if result.is_err() {
            let _ = self.uart.write_all(&protocol::encode(Command::Sleep)).await;
        }
        result
    }
}

impl<U: Read + Write, D: DelayNs + Clone> SensorDriver for Sds011<U, D> {
    const KIND: SensorKind = SensorKind::Sds011;
    const WARMS_UP: bool = true;
    type Error = Error<U::Error>;

    async fn init(&mut self) -> Result<(), Self::Error> {
        self.session(false).await.map(|_| ())
    }

    async fn sample<C: Clock>(
        &mut self,
        clock: &C,
    ) -> Result<Option<MeasurementUpdate>, Self::Error> {
        let Some((pm25, pm10)) = self.session(true).await? else {
            return Ok(None);
        };
        if pm25 > MAX_RAW_VALUE || pm10 > MAX_RAW_VALUE {
            log::debug!("[sds011] out-of-range sample pm25 {} pm10 {}", pm25, pm10);
            return Err(Error::OutOfRange);
        }
        log::debug!("[sds011] result: pm25 {} pm10 {} (0.1 ug/m3)", pm25, pm10);
        Ok(Some(MeasurementUpdate::Sds011(Sds011Update {
            pm25: MicrogramsPerCubicMeter(f32::from(pm25) / 10.0),
            pm10: MicrogramsPerCubicMeter(f32::from(pm10) / 10.0),
            timestamp: clock.now(),
        })))
    }

    fn poll_interval_ms(&self) -> u32 {
        self.config.sleep_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{NoDelay, StepClock, block_on};
    use std::collections::VecDeque;
    use std::vec::Vec;

    const TEST_CONFIG: Config = Config {
        warmup_ms: 3_000,
        sleep_ms: 1_000,
    };
    const SLEEP_COMMAND: [u8; protocol::COMMAND_LEN] = [
        0xAA, 0xB4, 0x06, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0x05, 0xAB,
    ];

    fn frame(kind: u8, b2: u8, b3: u8, b4: u8, b5: u8) -> [u8; REPLY_LEN] {
        let mut f = [0xAA, kind, b2, b3, b4, b5, 0xA1, 0x60, 0, 0xAB];
        f[8] = f[2..8].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        f
    }

    /// A sensor answering every command like the real one.
    struct FakeSensor {
        input: VecDeque<u8>,
        written: Vec<Vec<u8>>,
        pm: (u16, u16),
        /// Command that is never answered.
        mute: Option<u8>,
        silent: bool,
        /// Send a measurement before every reply, like a sensor in active mode.
        chatty: bool,
    }

    impl FakeSensor {
        fn new(pm: (u16, u16)) -> Self {
            Self {
                input: VecDeque::new(),
                written: Vec::new(),
                pm,
                mute: None,
                silent: false,
                chatty: false,
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeError;

    impl Display for FakeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fake")
        }
    }

    impl core::error::Error for FakeError {}

    impl embedded_io_async::Error for FakeError {
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    impl ErrorType for FakeSensor {
        type Error = FakeError;
    }

    impl Read for FakeSensor {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, FakeError> {
            if self.input.is_empty() {
                return core::future::pending().await;
            }
            let n = self.input.len().min(buf.len());
            for slot in &mut buf[..n] {
                *slot = self.input.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for FakeSensor {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, FakeError> {
            self.written.push(buf.to_vec());
            if self.silent || self.mute == Some(buf[2]) {
                return Ok(buf.len());
            }
            if self.chatty {
                self.input.extend(frame(0xC0, 1, 0, 2, 0));
            }
            let reply = match buf[2] {
                2 | 6 => frame(0xC5, buf[2], buf[3], buf[4], 0),
                7 => frame(0xC5, 7, 24, 5, 17),
                4 => {
                    let (a, b) = self.pm;
                    let (a, b) = (a.to_le_bytes(), b.to_le_bytes());
                    frame(0xC0, a[0], a[1], b[0], b[1])
                }
                other => panic!("unexpected command {other}"),
            };
            self.input.extend(reply);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), FakeError> {
            Ok(())
        }
    }

    fn is_sleep(packet: &[u8]) -> bool {
        packet[2] == 6 && packet[4] == 0
    }

    fn is_wake(packet: &[u8]) -> bool {
        packet[2] == 6 && packet[4] == 1
    }

    fn sample(
        sensor: &mut Sds011<FakeSensor, NoDelay>,
    ) -> Result<MeasurementUpdate, Error<FakeError>> {
        let clock = StepClock::new(1_000);
        block_on(sensor.sample(&clock)).map(|u| u.expect("update"))
    }

    #[test]
    fn sample_wakes_queries_and_sleeps() {
        let mut sensor = Sds011::new(FakeSensor::new((123, 456)), NoDelay, TEST_CONFIG);

        let MeasurementUpdate::Sds011(u) = sample(&mut sensor).unwrap() else {
            panic!("expected SDS011 update")
        };
        assert!((u.pm25.0 - 12.3).abs() < 1e-4, "{}", u.pm25.0);
        assert!((u.pm10.0 - 45.6).abs() < 1e-4);

        let written = &sensor.uart.uart.written;
        assert!(is_wake(written.first().unwrap()));
        assert!(is_sleep(written.last().unwrap()));
        assert_eq!(written.iter().filter(|p| p[2] == 4).count(), 2);
    }

    #[test]
    fn init_leaves_the_sensor_asleep_in_query_mode() {
        let mut sensor = Sds011::new(FakeSensor::new((1, 1)), NoDelay, TEST_CONFIG);
        assert_eq!(block_on(sensor.init()), Ok(()));
        let written = &sensor.uart.uart.written;
        assert!(written.iter().any(|p| p[2] == 2 && p[4] == 1));
        assert!(is_sleep(written.last().unwrap()));
    }

    #[test]
    fn stale_bytes_of_an_active_sensor_are_discarded() {
        let mut uart = FakeSensor::new((10, 20));
        for _ in 0..3 {
            uart.input.extend(frame(0xC0, 1, 0, 2, 0));
        }
        uart.input.extend([0xAA, 0xC0, 0x01]);
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        assert_eq!(block_on(sensor.init()), Ok(()));
    }

    #[test]
    fn data_frames_before_a_reply_are_skipped() {
        let mut uart = FakeSensor::new((50, 60));
        uart.chatty = true;
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        let MeasurementUpdate::Sds011(u) = sample(&mut sensor).unwrap() else {
            panic!("expected SDS011 update")
        };
        assert!((u.pm25.0 - 5.0).abs() < 1e-4);
    }

    #[test]
    fn garbage_before_a_frame_is_skipped() {
        let mut sensor = Sds011::new(FakeSensor::new((50, 60)), NoDelay, TEST_CONFIG);
        let mut stream = std::vec![0x00, 0x13, 0x77];
        stream.extend(frame(0xC5, 6, 1, 1, 0));
        sensor.uart.uart.input.extend(stream);
        assert_eq!(
            block_on(sensor.read_frame()).unwrap().reply,
            Reply::Sleep { work: true }
        );
    }

    #[test]
    fn silent_sensor_is_absent() {
        let mut uart = FakeSensor::new((10, 20));
        uart.silent = true;
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        let err = block_on(sensor.init()).unwrap_err();
        assert_eq!(err, Error::NoResponse);
        assert!(err.is_absent());
        assert_eq!(err.code(), SensorErrorCode::Timeout);

        let written = &sensor.uart.uart.written;
        assert_eq!(written.len(), INIT_ATTEMPTS as usize + 1);
        assert_eq!(written.last().unwrap().as_slice(), SLEEP_COMMAND);
    }

    #[test]
    fn failed_measurement_puts_the_sensor_to_sleep() {
        let mut uart = FakeSensor::new((10, 20));
        uart.mute = Some(4);
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        let err = sample(&mut sensor).unwrap_err();
        assert_eq!(err, Error::NoResponse);
        assert_eq!(
            sensor.uart.uart.written.last().unwrap().as_slice(),
            SLEEP_COMMAND
        );
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        let mut sensor = Sds011::new(
            FakeSensor::new((MAX_RAW_VALUE + 1, 20)),
            NoDelay,
            TEST_CONFIG,
        );
        let err = sample(&mut sensor).unwrap_err();
        assert_eq!(err, Error::OutOfRange);
        assert_eq!(err.code(), SensorErrorCode::InvalidData);
    }
}
