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
//! Shared view of sensor runtime states.
//!
//! A lightweight read-only hook for runtime policies (for example a future
//! noise acquisition policy that depends on SDS011 activity) so they can react
//! to sensor state without coupling drivers to each other. It does not replace
//! the `MeasurementStore`, which stays the single owner of measurements.

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use rohi_sensor_core::{SensorKind, SensorState};

/// Current [`SensorState`] of every sensor.
pub struct SensorStates<M: RawMutex> {
    states: Mutex<M, Cell<[SensorState; SensorKind::COUNT]>>,
}

impl<M: RawMutex> SensorStates<M> {
    /// All sensors start as not detected.
    pub const fn new() -> Self {
        Self {
            states: Mutex::new(Cell::new([SensorState::NotDetected; SensorKind::COUNT])),
        }
    }

    /// Current state of `sensor`.
    pub fn get(&self, sensor: SensorKind) -> SensorState {
        self.states.lock(|c| c.get()[sensor.index()])
    }

    /// Update the state of `sensor`.
    pub fn set(&self, sensor: SensorKind, state: SensorState) {
        self.states.lock(|c| {
            let mut all = c.get();
            all[sensor.index()] = state;
            c.set(all);
        });
    }
}

impl<M: RawMutex> Default for SensorStates<M> {
    fn default() -> Self {
        Self::new()
    }
}
