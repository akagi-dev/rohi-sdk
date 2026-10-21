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
//! The sensor is operated in a duty cycle to extend the life of its laser and
//! fan. One measurement cycle is an explicit state machine:
//!
//! ```text
//! Sleeping -> Wake -> Warmup -> Sampling -> Aggregate -> Sleeping
//! ```
//!
//! * `Sleeping` - fan and laser are off; the runner waits
//!   [`Config::sleep_ms`] before the next cycle.
//! * `Wake` - the work command is sent.
//! * `Warmup` - frames are read and discarded for [`Config::warmup_ms`] so the
//!   UART FIFO never overflows and the airflow stabilizes.
//! * `Sampling` - up to [`Config::frames_per_cycle`] valid frames are
//!   collected, bounded by [`Config::sampling_timeout_ms`].
//! * `Aggregate` - malformed frames are already rejected by the parser; with at
//!   least [`MIN_FRAMES_FOR_TRIMMING`] samples the lowest and highest value of
//!   each channel are dropped, the rest is averaged. Fewer than
//!   [`MIN_FRAMES_FOR_RESULT`] valid frames make the cycle fail.
//!
//! # Defaults
//!
//! 30 s warm-up, 10 frames (about 10 s at the sensor's 1 frame/s rate),
//! 90 s sleep; a cycle takes about 2 minutes. Every single read is bounded by
//! [`READ_TIMEOUT_MS`].
//!
//! # Wire format
//!
//! Data frame, 10 bytes: `AA C0 PM25_L PM25_H PM10_L PM10_H ID_L ID_H CHK AB`,
//! values in 0.1 µg/m³, `CHK` is the sum of bytes 2..=7 modulo 256. Commands
//! are 19 bytes: `AA B4 CMD DATA1..DATA12 FF FF CHK AB` where `CHK` is the sum
//! of bytes 2..=16 modulo 256.

use core::fmt::Debug;

use embedded_hal_async::delay::DelayNs;
use embedded_io_async::{Read, Write};
use rohi_sensor_core::{
    MeasurementUpdate, MicrogramsPerCubicMeter, Sds011Update, SensorErrorCode, SensorKind,
};

use crate::runtime::{Clock, DriverError, SensorDriver, with_timeout};

/// Bound of a single UART read.
pub const READ_TIMEOUT_MS: u32 = 3_000;
/// Minimum valid frames needed to produce a result.
pub const MIN_FRAMES_FOR_RESULT: usize = 3;
/// Minimum valid frames to drop the lowest and highest value of each channel.
pub const MIN_FRAMES_FOR_TRIMMING: usize = 5;
/// Highest value of the sensor range, in 0.1 µg/m³ (999.9 µg/m³).
pub const MAX_RAW_VALUE: u16 = 9_999;
/// Reads attempted while probing, so a probe takes at most
/// `INIT_READ_ATTEMPTS * READ_TIMEOUT_MS`.
pub const INIT_READ_ATTEMPTS: u32 = 3;
/// Maximum number of UART errors tolerated within one cycle.
pub const MAX_UART_ERRORS: u32 = 5;
/// Frames collected per cycle, upper bound of the aggregator buffer.
pub const MAX_FRAMES: usize = 32;

const FRAME_LEN: usize = 10;
const COMMAND_LEN: usize = 19;
const HEAD: u8 = 0xAA;
const TAIL: u8 = 0xAB;
const DATA_ID: u8 = 0xC0;
const REPLY_ID: u8 = 0xC5;

/// Timing and sampling parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Time the sensor spends awake before samples are used.
    pub warmup_ms: u32,
    /// Valid frames to collect per cycle, at most [`MAX_FRAMES`].
    pub frames_per_cycle: usize,
    /// Upper bound of the sampling phase.
    pub sampling_timeout_ms: u32,
    /// Sleep time between two cycles.
    pub sleep_ms: u32,
}

impl Config {
    /// Default parameters, see the module documentation.
    pub const DEFAULT: Self = Self {
        warmup_ms: 30_000,
        frames_per_cycle: 10,
        sampling_timeout_ms: 20_000,
        sleep_ms: 90_000,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Measurement cycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Fan and laser are off.
    Sleeping,
    /// The work command is being sent.
    Wake,
    /// Frames are discarded while the sensor stabilizes.
    Warmup,
    /// Valid frames are being collected.
    Sampling,
    /// The cycle result is being calculated.
    Aggregate,
}

/// A frame received from the sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// Measurement, in 0.1 µg/m³.
    Data {
        /// PM2.5.
        pm25: u16,
        /// PM10.
        pm10: u16,
    },
    /// Reply to a command.
    Reply {
        /// Command the reply belongs to.
        command: u8,
    },
}

/// Streaming frame parser without allocation. Resynchronizes on the next
/// `AA` header after corrupted bytes.
#[derive(Debug, Clone, Copy)]
pub struct FrameParser {
    buf: [u8; FRAME_LEN],
    len: usize,
    rejected: u32,
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameParser {
    /// Create an empty parser.
    pub const fn new() -> Self {
        Self {
            buf: [0; FRAME_LEN],
            len: 0,
            rejected: 0,
        }
    }

    /// Number of complete 10-byte candidates dropped because of a bad
    /// checksum or tail.
    pub fn rejected(&self) -> u32 {
        self.rejected
    }

    /// Feed one byte; returns a frame when one is completed.
    pub fn push(&mut self, byte: u8) -> Option<Frame> {
        if self.len == 0 {
            if byte == HEAD {
                self.buf[0] = byte;
                self.len = 1;
            }
            return None;
        }

        self.buf[self.len] = byte;
        self.len += 1;

        if self.len == 2 && byte != DATA_ID && byte != REPLY_ID {
            self.resync();
            return None;
        }
        if self.len < FRAME_LEN {
            return None;
        }

        let b = self.buf;
        let checksum = b[2..8].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        if b[9] != TAIL || b[8] != checksum {
            self.rejected = self.rejected.saturating_add(1);
            self.resync();
            return None;
        }
        self.len = 0;
        Some(if b[1] == DATA_ID {
            Frame::Data {
                pm25: u16::from_le_bytes([b[2], b[3]]),
                pm10: u16::from_le_bytes([b[4], b[5]]),
            }
        } else {
            Frame::Reply { command: b[2] }
        })
    }

    /// Drop the first byte and replay the rest, which may contain the start of
    /// the next frame.
    fn resync(&mut self) {
        let buffered = self.buf;
        let n = self.len;
        self.len = 0;
        // Fewer than FRAME_LEN bytes are replayed, so no frame can complete.
        for &byte in &buffered[1..n] {
            self.push(byte);
        }
    }
}

/// Collects valid frames of one cycle and calculates the cycle result.
#[derive(Debug, Clone, Copy)]
pub struct Aggregator {
    pm25: [u16; MAX_FRAMES],
    pm10: [u16; MAX_FRAMES],
    count: usize,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Aggregator {
    /// Create an empty aggregator.
    pub const fn new() -> Self {
        Self {
            pm25: [0; MAX_FRAMES],
            pm10: [0; MAX_FRAMES],
            count: 0,
        }
    }

    /// Number of accepted frames.
    pub fn len(&self) -> usize {
        self.count
    }

    /// No frame accepted yet.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Accept a sample; values outside of the sensor range and samples beyond
    /// the capacity are rejected. Returns whether it was accepted.
    pub fn push(&mut self, pm25: u16, pm10: u16) -> bool {
        if pm25 > MAX_RAW_VALUE || pm10 > MAX_RAW_VALUE || self.count >= MAX_FRAMES {
            return false;
        }
        self.pm25[self.count] = pm25;
        self.pm10[self.count] = pm10;
        self.count += 1;
        true
    }

    fn channel_average(values: &[u16], trim: bool) -> f32 {
        let sum: u32 = values.iter().map(|&v| v as u32).sum();
        let (sum, n) = if trim {
            let min = *values.iter().min().unwrap_or(&0) as u32;
            let max = *values.iter().max().unwrap_or(&0) as u32;
            (sum - min - max, values.len() as u32 - 2)
        } else {
            (sum, values.len() as u32)
        };
        sum as f32 / n as f32 / 10.0
    }

    /// Cycle result `(pm25, pm10)` in µg/m³, `None` with too few samples.
    pub fn result(&self) -> Option<(f32, f32)> {
        if self.count < MIN_FRAMES_FOR_RESULT {
            return None;
        }
        let trim = self.count >= MIN_FRAMES_FOR_TRIMMING;
        Some((
            Self::channel_average(&self.pm25[..self.count], trim),
            Self::channel_average(&self.pm10[..self.count], trim),
        ))
    }
}

/// Build a command packet.
pub fn command(cmd: u8, data1: u8, data2: u8) -> [u8; COMMAND_LEN] {
    let mut packet = [0u8; COMMAND_LEN];
    packet[0] = HEAD;
    packet[1] = 0xB4;
    packet[2] = cmd;
    packet[3] = data1;
    packet[4] = data2;
    packet[15] = 0xFF;
    packet[16] = 0xFF;
    packet[17] = packet[2..17].iter().fold(0u8, |a, &x| a.wrapping_add(x));
    packet[18] = TAIL;
    packet
}

const CMD_REPORTING_MODE: u8 = 2;
const CMD_SLEEP_WORK: u8 = 6;

/// SDS011 driver error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// UART failure.
    Uart(E),
    /// The sensor does not send anything.
    NoResponse,
    /// Only malformed frames were received.
    InvalidFrames,
    /// Too few valid frames were received to build a result.
    TooFewSamples,
}

impl<E: Debug> DriverError for Error<E> {
    fn code(&self) -> SensorErrorCode {
        match self {
            Error::Uart(_) => SensorErrorCode::Bus,
            Error::NoResponse => SensorErrorCode::Timeout,
            Error::InvalidFrames => SensorErrorCode::Crc,
            Error::TooFewSamples => SensorErrorCode::InvalidData,
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Error::NoResponse)
    }
}

/// SDS011 driver over any async UART.
pub struct Sds011<U, D> {
    uart: U,
    delay: D,
    config: Config,
    parser: FrameParser,
    phase: Phase,
}

impl<U: Read + Write, D: DelayNs> Sds011<U, D> {
    /// Create a driver; call [`SensorDriver::init`] next.
    pub fn new(uart: U, delay: D, config: Config) -> Self {
        Self {
            uart,
            delay,
            config,
            parser: FrameParser::new(),
            phase: Phase::Sleeping,
        }
    }

    /// Current cycle phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    async fn send(&mut self, packet: [u8; COMMAND_LEN]) -> Result<(), Error<U::Error>> {
        self.uart.write_all(&packet).await.map_err(Error::Uart)?;
        self.uart.flush().await.map_err(Error::Uart)
    }

    async fn set_awake(&mut self, awake: bool) -> Result<(), Error<U::Error>> {
        self.send(command(CMD_SLEEP_WORK, 1, awake as u8)).await
    }

    /// Read one chunk and feed it to the parser. `Ok(None)`: read timed out.
    async fn next_frames(
        &mut self,
        on_frame: &mut impl FnMut(Frame),
    ) -> Result<bool, Error<U::Error>> {
        let mut chunk = [0u8; 32];
        let n = match with_timeout(&mut self.delay, READ_TIMEOUT_MS, self.uart.read(&mut chunk))
            .await
        {
            Err(_) => return Ok(false),
            Ok(Err(e)) => return Err(Error::Uart(e)),
            Ok(Ok(n)) => n,
        };
        for &byte in &chunk[..n] {
            if let Some(frame) = self.parser.push(byte) {
                on_frame(frame);
            }
        }
        Ok(true)
    }

    /// Wake the sensor, switch it to active reporting, require at least one
    /// valid frame and put it back to sleep.
    pub async fn initialize(&mut self) -> Result<(), Error<U::Error>> {
        self.phase = Phase::Wake;
        self.parser = FrameParser::new();
        self.send(command(CMD_REPORTING_MODE, 1, 0)).await?;
        self.set_awake(true).await?;

        let mut seen = false;
        for _ in 0..INIT_READ_ATTEMPTS {
            self.next_frames(&mut |_| seen = true).await?;
            if seen {
                break;
            }
        }
        let result = self.set_awake(false).await;
        self.phase = Phase::Sleeping;
        if !seen {
            return Err(Error::NoResponse);
        }
        result
    }

    /// Run one complete duty cycle and return `(pm25, pm10)` in µg/m³.
    pub async fn measure_cycle<C: Clock>(
        &mut self,
        clock: &C,
    ) -> Result<(f32, f32), Error<U::Error>> {
        self.phase = Phase::Wake;
        self.parser = FrameParser::new();
        if let Err(e) = self.set_awake(true).await {
            self.phase = Phase::Sleeping;
            return Err(e);
        }
        let result = self.collect(clock).await;
        // Always try to stop the fan, whatever happened.
        let slept = self.set_awake(false).await;
        self.phase = Phase::Sleeping;
        let value = result?;
        slept?;
        Ok(value)
    }

    async fn collect<C: Clock>(&mut self, clock: &C) -> Result<(f32, f32), Error<U::Error>> {
        let mut uart_errors = 0;

        self.phase = Phase::Warmup;
        let warmup_end = clock.now().millis + self.config.warmup_ms as u64;
        while clock.now().millis < warmup_end {
            match self.next_frames(&mut |_| ()).await {
                Err(Error::Uart(e)) => {
                    uart_errors += 1;
                    if uart_errors >= MAX_UART_ERRORS {
                        return Err(Error::Uart(e));
                    }
                }
                Err(e) => return Err(e),
                Ok(_) => {}
            }
        }

        self.phase = Phase::Sampling;
        let wanted = self.config.frames_per_cycle.min(MAX_FRAMES);
        let sampling_end = clock.now().millis + self.config.sampling_timeout_ms as u64;
        let mut aggregator = Aggregator::new();
        let rejected_before = self.parser.rejected();
        let mut received_anything = false;
        while aggregator.len() < wanted && clock.now().millis < sampling_end {
            let mut frames = [None; 4];
            let mut frame_count = 0;
            let progressed = match self
                .next_frames(&mut |f| {
                    if frame_count < frames.len() {
                        frames[frame_count] = Some(f);
                        frame_count += 1;
                    }
                })
                .await
            {
                Err(Error::Uart(e)) => {
                    uart_errors += 1;
                    if uart_errors >= MAX_UART_ERRORS {
                        return Err(Error::Uart(e));
                    }
                    continue;
                }
                Err(e) => return Err(e),
                Ok(progressed) => progressed,
            };
            received_anything |= progressed;
            for frame in frames.into_iter().flatten() {
                if let Frame::Data { pm25, pm10 } = frame
                    && aggregator.len() < wanted
                {
                    aggregator.push(pm25, pm10);
                }
            }
        }

        self.phase = Phase::Aggregate;
        match aggregator.result() {
            Some(result) => Ok(result),
            None if !received_anything => Err(Error::NoResponse),
            None if aggregator.is_empty() && self.parser.rejected() > rejected_before => {
                Err(Error::InvalidFrames)
            }
            None => Err(Error::TooFewSamples),
        }
    }
}

impl<U: Read + Write, D: DelayNs> SensorDriver for Sds011<U, D> {
    const KIND: SensorKind = SensorKind::Sds011;
    const WARMS_UP: bool = true;
    type Error = Error<U::Error>;

    async fn init(&mut self) -> Result<(), Self::Error> {
        self.initialize().await
    }

    async fn sample<C: Clock>(
        &mut self,
        clock: &C,
    ) -> Result<Option<MeasurementUpdate>, Self::Error> {
        let (pm25, pm10) = self.measure_cycle(clock).await?;
        Ok(Some(MeasurementUpdate::Sds011(Sds011Update {
            pm25: MicrogramsPerCubicMeter(pm25),
            pm10: MicrogramsPerCubicMeter(pm10),
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
    use crate::testutil::{MockUart, NoDelay, StepClock, block_on};
    use std::vec::Vec;

    fn frame(pm25: u16, pm10: u16) -> [u8; 10] {
        let mut f = [HEAD, DATA_ID, 0, 0, 0, 0, 0xA1, 0x60, 0, TAIL];
        f[2..4].copy_from_slice(&pm25.to_le_bytes());
        f[4..6].copy_from_slice(&pm10.to_le_bytes());
        f[8] = f[2..8].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        f
    }

    fn parse(bytes: &[u8]) -> (Vec<Frame>, FrameParser) {
        let mut parser = FrameParser::new();
        let frames = bytes.iter().filter_map(|&b| parser.push(b)).collect();
        (frames, parser)
    }

    const TEST_CONFIG: Config = Config {
        warmup_ms: 3_000,
        frames_per_cycle: 10,
        sampling_timeout_ms: 30_000,
        sleep_ms: 1_000,
    };

    #[test]
    fn parses_clean_frame() {
        let (frames, parser) = parse(&frame(123, 456));
        assert_eq!(
            frames,
            [Frame::Data {
                pm25: 123,
                pm10: 456
            }]
        );
        assert_eq!(parser.rejected(), 0);
    }

    #[test]
    fn resynchronizes_after_garbage_including_header_bytes() {
        let mut stream = std::vec![0x01, HEAD, HEAD, 0x77, HEAD];
        stream.extend_from_slice(&frame(10, 20));
        let (frames, _) = parse(&stream);
        assert_eq!(frames, [Frame::Data { pm25: 10, pm10: 20 }]);
    }

    #[test]
    fn rejects_bad_checksum_and_bad_tail() {
        let mut bad_sum = frame(10, 20);
        bad_sum[8] ^= 0xFF;
        let mut bad_tail = frame(10, 20);
        bad_tail[9] = 0x00;
        let mut stream = Vec::new();
        stream.extend_from_slice(&bad_sum);
        stream.extend_from_slice(&bad_tail);
        stream.extend_from_slice(&frame(30, 40));
        let (frames, parser) = parse(&stream);
        assert_eq!(frames, [Frame::Data { pm25: 30, pm10: 40 }]);
        assert_eq!(parser.rejected(), 2);
    }

    #[test]
    fn recovers_when_next_frame_starts_inside_truncated_frame() {
        let mut stream = std::vec![HEAD, DATA_ID, 1, 2, 3, 4];
        stream.extend_from_slice(&frame(55, 66));
        let (frames, _) = parse(&stream);
        assert_eq!(frames, [Frame::Data { pm25: 55, pm10: 66 }]);
    }

    #[test]
    fn reply_frames_are_recognized() {
        let mut reply = [HEAD, REPLY_ID, 7, 0x15, 0x01, 0x02, 0xA1, 0x60, 0, TAIL];
        reply[8] = reply[2..8].iter().fold(0u8, |a, &x| a.wrapping_add(x));
        let (frames, _) = parse(&reply);
        assert_eq!(frames, [Frame::Reply { command: 7 }]);
    }

    #[test]
    fn command_matches_protocol_examples() {
        assert_eq!(
            command(CMD_SLEEP_WORK, 1, 0),
            [
                0xAA, 0xB4, 0x06, 0x01, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0x05, 0xAB
            ]
        );
        assert_eq!(command(CMD_SLEEP_WORK, 1, 1)[17], 0x06);
    }

    #[test]
    fn aggregator_trims_extremes_when_enough_samples() {
        let mut a = Aggregator::new();
        for v in [100, 110, 120, 130, 140, 150, 160, 170, 180, 900] {
            assert!(a.push(v, v * 2));
        }
        let (pm25, pm10) = a.result().unwrap();
        assert!((pm25 - 14.5).abs() < 1e-4, "{pm25}");
        assert!((pm10 - 29.0).abs() < 1e-4);
    }

    #[test]
    fn aggregator_does_not_trim_small_sets_and_needs_minimum() {
        let mut a = Aggregator::new();
        a.push(100, 100);
        a.push(200, 200);
        assert_eq!(a.result(), None);
        a.push(300, 300);
        assert_eq!(a.result(), Some((20.0, 20.0)));
    }

    #[test]
    fn aggregator_rejects_out_of_range() {
        let mut a = Aggregator::new();
        assert!(!a.push(MAX_RAW_VALUE + 1, 0));
        assert!(!a.push(0, MAX_RAW_VALUE + 1));
        assert!(a.is_empty());
    }

    fn cycle_stream() -> Vec<u8> {
        let values = [100u16, 110, 120, 130, 140, 150, 160, 170, 180, 900];
        let mut stream = std::vec![0x00, 0x13];
        for (i, v) in values.iter().enumerate() {
            if i == 4 {
                // A corrupted frame in the middle of the stream.
                let mut bad = frame(1, 1);
                bad[8] ^= 0x55;
                stream.extend_from_slice(&bad);
            }
            stream.extend_from_slice(&frame(*v, v * 2));
        }
        stream
    }

    #[test]
    fn full_cycle_wakes_samples_aggregates_and_sleeps() {
        let mut uart = MockUart::default();
        // Warm-up traffic that must be ignored.
        uart.push_bytes(&frame(9_000, 9_000), 10);
        uart.push_bytes(&[0x00, 0x01], 2);
        uart.push_bytes(&cycle_stream(), 7);
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        let clock = StepClock::new(1_000);

        let Some(MeasurementUpdate::Sds011(u)) = block_on(sensor.sample(&clock)).unwrap() else {
            panic!("expected SDS011 update")
        };
        assert!((u.pm25.0 - 14.5).abs() < 1e-4, "{}", u.pm25.0);
        assert!((u.pm10.0 - 29.0).abs() < 1e-4);
        assert_eq!(sensor.phase(), Phase::Sleeping);

        let written = &sensor.uart.written;
        assert_eq!(
            written.first().unwrap().as_slice(),
            command(CMD_SLEEP_WORK, 1, 1)
        );
        assert_eq!(
            written.last().unwrap().as_slice(),
            command(CMD_SLEEP_WORK, 1, 0)
        );
    }

    #[test]
    fn silent_sensor_times_out_and_is_put_to_sleep() {
        let uart = MockUart::default();
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        let clock = StepClock::new(1_000);
        let err = block_on(sensor.measure_cycle(&clock)).unwrap_err();
        assert_eq!(err, Error::NoResponse);
        assert_eq!(
            sensor.uart.written.last().unwrap().as_slice(),
            command(CMD_SLEEP_WORK, 1, 0)
        );
    }

    #[test]
    fn only_corrupted_frames_fail_with_crc_error() {
        let mut uart = MockUart::default();
        uart.push_bytes(&[0x00; 2], 2);
        uart.push_bytes(&[0x00; 2], 2);
        for _ in 0..5 {
            let mut bad = frame(100, 100);
            bad[8] ^= 0x55;
            uart.push_bytes(&bad, 10);
        }
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        let clock = StepClock::new(1_000);
        let err = block_on(sensor.measure_cycle(&clock)).unwrap_err();
        assert_eq!(err, Error::InvalidFrames);
        assert_eq!(err.code(), SensorErrorCode::Crc);
    }

    #[test]
    fn repeated_uart_errors_abort_the_cycle() {
        let mut uart = MockUart::default();
        for _ in 0..MAX_UART_ERRORS {
            uart.chunks.push_back(Err(crate::testutil::MockUartError));
        }
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        let clock = StepClock::new(1_000);
        let err = block_on(sensor.measure_cycle(&clock)).unwrap_err();
        assert!(matches!(err, Error::Uart(_)));
    }

    #[test]
    fn init_requires_a_valid_frame() {
        let mut uart = MockUart::default();
        uart.push_bytes(&frame(10, 10), 10);
        let mut sensor = Sds011::new(uart, NoDelay, TEST_CONFIG);
        assert_eq!(block_on(sensor.initialize()), Ok(()));
        assert_eq!(sensor.phase(), Phase::Sleeping);

        let mut sensor = Sds011::new(MockUart::default(), NoDelay, TEST_CONFIG);
        let err = block_on(sensor.initialize()).unwrap_err();
        assert_eq!(err, Error::NoResponse);
        assert!(err.is_absent());
    }
}
