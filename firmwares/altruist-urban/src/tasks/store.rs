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
//! The single owner of the `MeasurementStore`.

use embassy_futures::select::{Either3, select3};
use embassy_time::{Duration, Ticker};
use log::info;
use rohi_hal::runtime::{Clock, MeasurementHandle, MeasurementService};
use rohi_hal::{MeasurementStore, MeasurementsSnapshot};
use rohi_hal_esp::EmbassyClock;

use super::{EVENTS, RawMutex};

/// Snapshot requests to the store task, see [`handle`].
static SNAPSHOTS: MeasurementService<RawMutex> = MeasurementService::new();

/// Handle for on-demand snapshots; the store itself is never shared.
pub fn handle() -> MeasurementHandle<'static, RawMutex> {
    SNAPSHOTS.handle()
}

/// Interval between logged snapshots.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(30);

fn log_snapshot(snapshot: &MeasurementsSnapshot) {
    info!("--- snapshot at {} ms ---", snapshot.generated_at.millis);
    for status in snapshot.sensor_statuses.iter() {
        if status.state == rohi_hal::SensorState::NotDetected {
            continue;
        }
        info!(
            "{:?}: {:?}, errors {}, last error {:?}",
            status.kind, status.state, status.error_count, status.last_error
        );
    }
    let m = &snapshot.measurements;
    if let Some(bmx) = &m.bmx280 {
        info!(
            "bmx280: T {:?}, P {:?}, RH {:?}",
            bmx.temperature.value,
            bmx.pressure.value,
            bmx.humidity.map(|h| h.value)
        );
    }
    if let Some(scd) = &m.scd4x {
        info!(
            "scd4x: CO2 {:?}, T {:?}, RH {:?}",
            scd.co2.value, scd.temperature.value, scd.humidity.value
        );
    }
    if let Some(sds) = &m.sds011 {
        info!(
            "sds011: PM2.5 {:?}, PM10 {:?}",
            sds.pm25.value, sds.pm10.value
        );
    }
    if let Some(mic) = &m.microphone {
        info!(
            "noise: avg {:?}, max {:?}",
            mic.average.value, mic.maximum.value
        );
    }
    info!("dropped events: {}", EVENTS.dropped());
}

/// Apply sensor events to the store, answer snapshot requests and periodically
/// log a snapshot. Every snapshot is generated with the time of its request.
#[embassy_executor::task]
pub async fn store_task() {
    let mut store = MeasurementStore::new();
    let clock = EmbassyClock;
    let mut ticker = Ticker::every(SNAPSHOT_INTERVAL);

    loop {
        match select3(EVENTS.receive(), SNAPSHOTS.accept(), ticker.next()).await {
            Either3::First(event) => store.apply(event),
            Either3::Second(request) => request.respond(store.snapshot(clock.now())),
            Either3::Third(()) => log_snapshot(&store.snapshot(clock.now())),
        }
    }
}
