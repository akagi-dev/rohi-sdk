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

//! The BLE peripheral of the host platform without a Bluetooth adapter being
//! required: the device must start and serve TCP whether BlueZ is there or not,
//! and the `ble` service must tell the truth about it. Talking to a real central
//! is a separate, hardware-bound smoke test, see the std guide.

mod common;

use std::time::Duration;

use common::*;
use rohi::model::ServiceState;

#[tokio::test(flavor = "current_thread")]
async fn the_ble_service_reports_the_real_state_of_the_peripheral() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;

    // The supervisor decides within moments: BlueZ is there and the device
    // advertises, or it is not and the state is an error that is retried.
    let state = eventually(
        "the BLE peripheral to settle",
        Duration::from_secs(30),
        async || {
            let (state, _, _, _) = summary(&mut device, "ble").await;
            (state != ServiceState::Disabled).then_some(state)
        },
    )
    .await;
    assert!(
        matches!(state, ServiceState::Running | ServiceState::Failed),
        "{state:?}"
    );

    // Whatever BLE does, the other transport and the services keep working.
    assert!(device.info().await.is_ok());
    drop(device);
    assert!(sim.stop().success());
}
