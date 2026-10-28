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
///////////////////////////////////////////////////////////////////////////////
//! Host-side test helpers: mock delay, clock and I2C bus.

use core::future::Future;
use std::collections::VecDeque;
use std::vec::Vec;

use crate::MonotonicTimestamp;
use embedded_hal::i2c::{ErrorKind, ErrorType, NoAcknowledgeSource, Operation};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;

use crate::runtime::Clock;

pub fn block_on<F: Future>(f: F) -> F::Output {
    embassy_futures::block_on(f)
}

/// Delay that completes immediately.
#[derive(Clone, Copy)]
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
    /// Address of every transaction.
    pub addresses: Vec<u8>,
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
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.addresses.push(address);
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

/// Delay that completes immediately and records every requested wait in
/// milliseconds; clones share the record.
#[derive(Clone, Default)]
pub struct RecordingDelay {
    waits: std::rc::Rc<core::cell::RefCell<Vec<u32>>>,
}

impl RecordingDelay {
    /// Waits requested so far.
    pub fn waits(&self) -> Vec<u32> {
        self.waits.borrow().clone()
    }
}

impl DelayNs for RecordingDelay {
    async fn delay_ns(&mut self, _ns: u32) {}

    async fn delay_ms(&mut self, ms: u32) {
        self.waits.borrow_mut().push(ms);
    }
}
