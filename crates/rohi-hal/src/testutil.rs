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
//! Host-side test helpers: mock delay, clock and I2C bus.

use core::future::Future;
use std::collections::VecDeque;
use std::vec::Vec;

use embedded_hal::i2c::{ErrorKind, ErrorType, NoAcknowledgeSource, Operation};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;
use rohi_sensor_core::MonotonicTimestamp;

use crate::runtime::Clock;

pub fn block_on<F: Future>(f: F) -> F::Output {
    embassy_futures::block_on(f)
}

/// Delay that completes immediately.
pub struct NoDelay;

impl DelayNs for NoDelay {
    async fn delay_ns(&mut self, _ns: u32) {}
}

/// Clock that always returns the same time.
pub struct FixedClock(pub MonotonicTimestamp);

impl Clock for FixedClock {
    fn now(&self) -> MonotonicTimestamp {
        self.0
    }
}

/// Scripted I2C bus: reads are served from queued responses, writes recorded.
#[derive(Default)]
pub struct MockI2c {
    /// Responses for successive read operations; `Err` fails the transaction.
    pub reads: VecDeque<Result<Vec<u8>, ErrorKind>>,
    /// All written byte sequences.
    pub writes: Vec<Vec<u8>>,
}

impl MockI2c {
    pub fn push_read(&mut self, bytes: &[u8]) {
        self.reads.push_back(Ok(bytes.to_vec()));
    }

    pub fn push_error(&mut self, kind: ErrorKind) {
        self.reads.push_back(Err(kind));
    }

    pub fn nack() -> ErrorKind {
        ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
    }
}

impl ErrorType for MockI2c {
    type Error = ErrorKind;
}

impl I2c for MockI2c {
    async fn transaction(
        &mut self,
        _address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        for op in operations {
            match op {
                Operation::Write(bytes) => self.writes.push(bytes.to_vec()),
                Operation::Read(buf) => {
                    let next = self.reads.pop_front().expect("unexpected I2C read");
                    let data = next?;
                    assert_eq!(data.len(), buf.len(), "mock read length mismatch");
                    buf.copy_from_slice(&data);
                }
            }
        }
        Ok(())
    }
}

/// Clock advancing by `step_ms` every time it is read.
pub struct StepClock {
    now: core::cell::Cell<u64>,
    step_ms: u64,
}

impl StepClock {
    pub fn new(step_ms: u64) -> Self {
        Self {
            now: core::cell::Cell::new(0),
            step_ms,
        }
    }
}

impl Clock for StepClock {
    fn now(&self) -> MonotonicTimestamp {
        let t = self.now.get();
        self.now.set(t + self.step_ms);
        MonotonicTimestamp::from_millis(t)
    }
}

/// UART failure used by [`MockUart`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MockUartError;

impl core::fmt::Display for MockUartError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("mock uart error")
    }
}

impl core::error::Error for MockUartError {}

impl embedded_io_async::Error for MockUartError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

/// Scripted UART. Reads are served from queued chunks; when the queue is empty
/// a read never completes (so bounded reads time out).
#[derive(Default)]
pub struct MockUart {
    pub chunks: VecDeque<Result<Vec<u8>, MockUartError>>,
    pub written: Vec<Vec<u8>>,
}

impl MockUart {
    pub fn push_bytes(&mut self, bytes: &[u8], chunk: usize) {
        for part in bytes.chunks(chunk) {
            self.chunks.push_back(Ok(part.to_vec()));
        }
    }
}

impl embedded_io_async::ErrorType for MockUart {
    type Error = MockUartError;
}

impl embedded_io_async::Read for MockUart {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self.chunks.pop_front() {
            Some(Ok(mut data)) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                if data.len() > n {
                    self.chunks.push_front(Ok(data.split_off(n)));
                }
                Ok(n)
            }
            Some(Err(e)) => Err(e),
            None => core::future::pending().await,
        }
    }
}

impl embedded_io_async::Write for MockUart {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.written.push(buf.to_vec());
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
