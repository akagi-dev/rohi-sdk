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

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use rohi_sdk::hal::SensorKind;
use rohi_sdk::hal::runtime::{Clock, EVENT_QUEUE_CAPACITY, EventBus, Reporter, SensorStates};

/// The `MeasurementStore` owner.
pub mod store;

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
