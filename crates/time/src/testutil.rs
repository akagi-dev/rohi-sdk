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

//! Test helpers: a manually advanced clock, blocking executor and delay.

use core::cell::Cell;
use std::rc::Rc;

use embedded_hal_async::delay::DelayNs;
use rohi_hal::MonotonicTimestamp;
use rohi_hal::runtime::Clock;

/// Monotonic clock moved by the test; clones share the time.
#[derive(Clone, Default)]
pub struct ManualClock(Rc<Cell<u64>>);

impl ManualClock {
    pub fn advance_ms(&self, ms: u64) {
        self.0.set(self.0.get() + ms);
    }

    pub fn advance_secs(&self, secs: u64) {
        self.advance_ms(secs * 1_000);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(self.0.get())
    }
}

/// Delay that completes at once, advances the clock by the waited time and
/// records every wait in milliseconds.
#[derive(Clone)]
pub struct ClockDelay {
    clock: ManualClock,
    waits: Rc<core::cell::RefCell<std::vec::Vec<u32>>>,
}

impl ClockDelay {
    pub fn new(clock: &ManualClock) -> Self {
        Self {
            clock: clock.clone(),
            waits: Rc::default(),
        }
    }

    pub fn waits(&self) -> std::vec::Vec<u32> {
        self.waits.borrow().clone()
    }
}

impl DelayNs for ClockDelay {
    async fn delay_ns(&mut self, _ns: u32) {}

    async fn delay_ms(&mut self, ms: u32) {
        self.waits.borrow_mut().push(ms);
        self.clock.advance_ms(u64::from(ms));
    }
}

/// Scripted [`TimeSource`](crate::TimeSource).
pub struct FakeSource {
    pub kind: crate::TimeSourceKind,
    pub available: Rc<Cell<bool>>,
    pub script: std::collections::VecDeque<Result<crate::TimeSample, crate::SyncFailure>>,
    pub samples_taken: usize,
}

impl FakeSource {
    pub fn new(kind: crate::TimeSourceKind) -> Self {
        Self {
            kind,
            available: Rc::new(Cell::new(true)),
            script: Default::default(),
            samples_taken: 0,
        }
    }
}

impl crate::TimeSource for FakeSource {
    type Error = crate::SyncFailure;

    fn kind(&self) -> crate::TimeSourceKind {
        self.kind
    }

    fn is_available(&self) -> bool {
        self.available.get()
    }

    async fn sample(&mut self) -> Result<crate::TimeSample, crate::SyncFailure> {
        self.samples_taken += 1;
        self.script
            .pop_front()
            .unwrap_or(Err(crate::SyncFailure::Source))
    }
}
