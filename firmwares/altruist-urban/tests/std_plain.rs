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

//! A device built without `sensor-simulator`: no sensor is fabricated and the
//! services report what is true. Runs only when the feature is off.

#![cfg(not(feature = "sensor-simulator"))]

mod common;

use common::*;
use rohi::model::ServiceState;

#[tokio::test(flavor = "current_thread")]
async fn without_the_simulator_no_sensor_is_reported_as_working() {
    let sim = Sim::start(&Launch::new());
    let mut device = sim.device().await;

    let (state, _, ready, _) = summary(&mut device, "sensors").await;
    assert!(!ready, "{state:?}");
    let sensors = fields(&mut device, "sensors").await;
    assert!(
        sensors.values().all(|value| f32_of(Some(value)).is_none()),
        "{sensors:?}"
    );

    // The BLE service tells that no BLE peripheral runs; it does not pretend
    // to advertise over TCP.
    let (state, _, ready, _) = summary(&mut device, "ble").await;
    assert_eq!(state, ServiceState::Disabled);
    assert!(!ready);

    // Commands that need simulated sensors are refused, not silently ignored.
    let reply = sim.manage("set co2 1500");
    assert!(reply.starts_with("error:"), "{reply}");
    assert!(reply.contains("sensor simulation"), "{reply}");
    // The rest of the management interface works.
    sim.manage_ok("network down");
    drop(device);
    assert!(sim.stop().success());
}
