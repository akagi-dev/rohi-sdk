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
//! Hardware-independent collection runtime: event queue, reporter, state view
//! and snapshot access.
//!
//! Sensor drivers (in `rohi-hal`, or any other producer) publish
//! [`SensorEvent`](crate::SensorEvent)s through a [`Reporter`] into one
//! [`EventBus`]. A single store task consumes the bus and owns the
//! [`MeasurementStore`](crate::MeasurementStore); other tasks get snapshots
//! from it through a [`MeasurementHandle`].
//!
//! ```text
//! sensor task N: driver + Reporter --SensorEvent--> EventBus
//!                                                     |
//! store task:  loop { select(EventBus::receive, MeasurementService::accept) }
//!                |-- event   -> MeasurementStore::apply(event)
//!                `-- request -> request.respond(store.snapshot(now))
//!
//! any other task: MeasurementHandle::snapshot().await
//! ```
//!
//! The capacities are fixed at compile time: [`EVENT_QUEUE_CAPACITY`] events
//! in the bus (the oldest is dropped when full, see [`bus`]) and one request
//! and one response slot in the snapshot service.
//!
//! | Module | Role |
//! |--------|------|
//! | [`reporter`] | [`Reporter`]: publishes the events of one sensor |
//! | [`bus`] | [`EventBus`]: bounded queue, overflow policy, the three event kinds |
//! | [`service`] | [`MeasurementHandle`]: on-demand snapshots from the store owner task |
//! | [`states`] | [`SensorStates`]: read-only view of every sensor state |
//!
//! # Cancellation
//!
//! [`EventBus::receive`] and the owner side [`MeasurementService::accept`] are
//! cancel-safe; [`MeasurementHandle::snapshot`] can be dropped at any point.

pub mod bus;
pub mod reporter;
pub mod service;
pub mod states;

pub use bus::{EVENT_QUEUE_CAPACITY, EventBus};
pub use reporter::Reporter;
pub use service::{
    MeasurementHandle, MeasurementService, MeasurementServiceError, SnapshotRequest,
};
pub use states::SensorStates;
