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
//! Per-sensor event reporter.

use crate::{MeasurementUpdate, SensorErrorCode, SensorEvent, SensorKind, SensorState};
use embassy_sync::blocking_mutex::raw::RawMutex;

use super::bus::EventBus;
use super::clock::Clock;
use super::states::SensorStates;

/// Publishes events of one sensor into the [`EventBus`].
///
/// State changes are published only when the state actually changes. A
/// successful observation implies [`SensorState::Ready`] and is published as a
/// single event, so the value and the state cannot be observed out of step.
pub struct Reporter<'a, M: RawMutex, C: Clock, const N: usize> {
    kind: SensorKind,
    bus: &'a EventBus<M, N>,
    clock: C,
    shared: Option<&'a SensorStates<M>>,
    state: SensorState,
}

impl<'a, M: RawMutex, C: Clock, const N: usize> Reporter<'a, M, C, N> {
    /// Create a reporter; the sensor is initially `NotDetected`.
    pub fn new(kind: SensorKind, bus: &'a EventBus<M, N>, clock: C) -> Self {
        Self {
            kind,
            bus,
            clock,
            shared: None,
            state: SensorState::NotDetected,
        }
    }

    /// Mirror state changes into a shared [`SensorStates`] view.
    pub fn with_shared_states(mut self, shared: &'a SensorStates<M>) -> Self {
        self.shared = Some(shared);
        self
    }

    /// Clock used for event timestamps.
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// State last reported.
    pub fn state(&self) -> SensorState {
        self.state
    }

    /// Report a state, publishing an event only if it changed.
    pub fn set_state(&mut self, state: SensorState) {
        if self.state == state {
            return;
        }
        self.state = state;
        if let Some(shared) = self.shared {
            shared.set(self.kind, state);
        }
        self.bus.publish(SensorEvent::StateChanged {
            sensor: self.kind,
            state,
            timestamp: self.clock.now(),
        });
    }

    /// Report a successful observation. The sensor becomes `Ready` without a
    /// separate `StateChanged` event: the consumer derives it from the
    /// measurement.
    pub fn measurement(&mut self, update: MeasurementUpdate) {
        self.state = SensorState::Ready;
        if let Some(shared) = self.shared {
            shared.set(self.kind, SensorState::Ready);
        }
        self.bus.publish(SensorEvent::Measurements(update));
    }

    /// Report a failed operation.
    pub fn error(&mut self, error: SensorErrorCode) {
        self.bus.publish(SensorEvent::Error {
            sensor: self.kind,
            error,
            timestamp: self.clock.now(),
        });
    }
}
