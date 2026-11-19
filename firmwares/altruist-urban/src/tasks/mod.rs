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
//! The sensor runtime: event bus, sensor states and the store owner.
//!
//! The platform declares the Embassy tasks (one per sensor, one for the store)
//! around these pieces, because a task cannot be generic.

use core::cell::Cell;

use embassy_executor::{SpawnError, SpawnToken, Spawner};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use log::error;
use rohi_sdk::sensors::SensorKind;
use rohi_sdk::sensors::runtime::{EVENT_QUEUE_CAPACITY, EventBus, Reporter, SensorStates};
use rohi_sdk::time::Clock;
use static_cell::StaticCell;

/// The `MeasurementStore` owner.
pub mod store;

/// Starts a task. A task that cannot start (its pool is full) is reported and
/// the firmware goes on without it: spawning never panics.
pub fn spawn<S>(spawner: Spawner, name: &str, task: Result<SpawnToken<S>, SpawnError>) {
    match task {
        Ok(token) => spawner.spawn(token),
        Err(error) => error!("Task {} not started: {:?}", name, error),
    }
}

/// Mutex flavour shared by the runtime primitives.
pub type RawMutex = CriticalSectionRawMutex;

/// Event queue between sensor tasks and the store task, see
/// [`EVENT_QUEUE_CAPACITY`] events; the oldest event is dropped when full.
pub static EVENTS: EventBus<RawMutex, EVENT_QUEUE_CAPACITY> = EventBus::new();

/// Read-only view of sensor states for runtime policies.
pub static STATES: SensorStates<RawMutex> = SensorStates::new();

/// The reporter of the sensor `kind`: publishes its events to [`EVENTS`] and
/// its state to [`STATES`], stamped by `clock`.
pub fn reporter<C: Clock>(
    kind: SensorKind,
    clock: C,
) -> Reporter<'static, RawMutex, C, EVENT_QUEUE_CAPACITY> {
    Reporter::new(kind, &EVENTS, clock).with_shared_states(&STATES)
}

/// A static that is built on first use instead of being initialized at link time.
///
/// A static whose initial value has a bit that is not zero (an `Option` of an enum is `None` by a
/// niche value) lands in `.data`, which is copied from flash and has a tight budget on the chips.
/// This one is all zeros until [`get`](Self::get) builds the value in place, so it stays in
/// `.bss`.
pub struct OnDemand<T: 'static> {
    cell: StaticCell<T>,
    built: BlockingMutex<RawMutex, Cell<Option<&'static T>>>,
}

impl<T> OnDemand<T> {
    /// Nothing built yet.
    pub const fn new() -> Self {
        Self {
            cell: StaticCell::new(),
            built: BlockingMutex::new(Cell::new(None)),
        }
    }

    /// The value, built by `init` on the first call. `init` runs once, in place.
    pub fn get(&'static self, init: impl FnOnce() -> T) -> &'static T {
        self.built.lock(|built| match built.get() {
            Some(value) => value,
            None => {
                let value: &'static T = self.cell.init_with(init);
                built.set(Some(value));
                value
            }
        })
    }
}

impl<T> Default for OnDemand<T> {
    fn default() -> Self {
        Self::new()
    }
}
