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

//! The telemetry uplink on the ESP: the portable service of
//! [`crate::connectivity`] with the hardware RNG and the ESP IP stack.

use embassy_executor::Spawner;
use esp_hal::rng::Rng;
use rohi_sdk::connectivity::{NONCE_LEN, NonceError, NonceSource};
use rohi_sdk::net::NetworkHandle;
use rohi_sdk::sensors::runtime::MeasurementHandle;

use super::resources::Ip;
use crate::connectivity;
use crate::tasks::RawMutex;

/// Nonces from the hardware RNG.
///
/// The RNG is a true random source while the radio runs, which is whenever
/// telemetry can be delivered; nonces only have to be unique for replay
/// protection, which holds in either mode.
#[derive(Clone, Copy)]
pub struct EspNonce;

impl NonceSource for EspNonce {
    fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
        Rng::new().read(dst);
        Ok(())
    }
}

/// Starts the telemetry task, see [`connectivity::run`]. It reads snapshots
/// only: it can neither delay nor stop the sensors, whatever the uplink does.
pub fn start(
    spawner: Spawner,
    network: NetworkHandle<'static, Ip>,
    measurements: MeasurementHandle<'static, RawMutex>,
) {
    crate::tasks::spawn(
        spawner,
        "connectivity_task",
        connectivity_task(network, measurements),
    );
}

/// Runs the telemetry service while it is enabled.
#[embassy_executor::task]
pub async fn connectivity_task(
    network: NetworkHandle<'static, Ip>,
    measurements: MeasurementHandle<'static, RawMutex>,
) {
    connectivity::run(network, measurements, EspNonce).await
}
