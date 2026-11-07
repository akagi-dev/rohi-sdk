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

//! BLE control plane of the Altruist Urban on the ESP.
//!
//! ```text
//! BLE radio -> rohi-net-esp (EspBle) -> rohi-control-ble -> rohi-control
//!                                                              |
//!                                        UrbanDevice ([`crate::control`])
//! ```
//!
//! The device and its services are the portable composition of
//! [`crate::control`]; this module adds the BLE peripheral and its tasks.

use embassy_executor::Spawner;
use log::{error, info};
use rohi_sdk::ble::ControlBle;
use rohi_sdk::control::{Controller, DeviceInfo, OpenPolicy};
use rohi_sdk::esp::net::EspWireless;
use rohi_sdk::net::NetworkHandle;
use static_cell::StaticCell;

use super::logging;
use super::resources::{BleResources, BleRunner, BleStatus, BleTransport, Ip};
use crate::config::ConfigHandle;
use crate::control::{Platform, ServicesSlot, UrbanDevice};
use crate::identity::IdentityHandle;
use crate::reboot;

/// Firmware name and version.
const FIRMWARE: &str = concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"));

/// Board and chip.
#[cfg(feature = "esp32c3")]
const BOARD: &str = "urban-esp32c3";
#[cfg(feature = "esp32c6")]
const BOARD: &str = "urban-esp32c6";

/// The control backend of the Altruist Urban on the ESP.
pub type EspDevice = UrbanDevice<Ip, BleStatus, logging::Service>;

/// The service instances, shared by the status and the action registry.
static SERVICES: ServicesSlot<Ip, BleStatus, logging::Service> = StaticCell::new();

/// The device over the runtime handles of the firmware.
///
/// `network` is the IP data plane; `None` tells that the network runtime did
/// not start: its services then report a failed start instead of looking
/// disabled. `ble` is the view of the BLE peripheral taken from its transport.
pub fn device(
    config: ConfigHandle,
    identity: IdentityHandle,
    network: Option<NetworkHandle<'static, Ip>>,
    ble: BleStatus,
) -> EspDevice {
    crate::control::device(
        Platform {
            info: DeviceInfo {
                firmware: FIRMWARE,
                board: BOARD,
            },
            services: &SERVICES,
            network,
            heap: super::net::heap_probe(),
            ble,
            logging: logging::runtime().service(),
            reboot: reboot::request,
        },
        config,
        identity,
    )
}

pub use crate::control::{BLE_CONFIG, GATT, ble_config};

/// Static storage of the BLE peripheral, sized by [`super::resources`].
pub static BLE_RESOURCES: StaticCell<BleResources> = StaticCell::new();

/// Runs the BLE controller and host stack.
#[embassy_executor::task]
pub async fn ble_task(runner: BleRunner) {
    runner.run().await
}

/// Starts the BLE control plane on `wireless`: the peripheral with the name
/// the configuration owner published at boot, and the control task over the
/// device built from `config`, `identity` and `network`. A BLE failure must
/// never stop the sensors: it is logged and the firmware carries on.
pub fn start(
    spawner: Spawner,
    wireless: &mut EspWireless,
    config: ConfigHandle,
    identity: IdentityHandle,
    network: Option<NetworkHandle<'static, Ip>>,
) {
    match wireless.ble(ble_config(), BLE_RESOURCES.init(Default::default())) {
        Ok((transport, runner)) => {
            let device = device(config, identity, network, transport.status());
            crate::tasks::spawn(spawner, "ble_task", ble_task(runner));
            crate::tasks::spawn(spawner, "control_task", control_task(transport, device));
            info!(
                "BLE control started, wireless heap free {} B",
                wireless.heap().free
            );
        }
        Err(error) => error!("BLE unavailable: {}", error),
    }
}

/// Serves control requests received over BLE.
///
/// There is no link security policy yet: the transport reports the link
/// security and the policy hook is in place, but [`OpenPolicy`] allows
/// everything, including identity provisioning and erase. Switch to a
/// stricter policy once BLE pairing is enabled.
#[embassy_executor::task]
pub async fn control_task(transport: BleTransport, device: EspDevice) {
    let controller = Controller::new(device, OpenPolicy);
    let mut control = ControlBle::new(transport, &controller);
    control.run().await
}
