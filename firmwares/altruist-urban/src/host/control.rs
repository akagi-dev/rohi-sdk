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

//! The control plane of the Altruist Urban on the host.
//!
//! One [`Controller`] over the portable [`UrbanDevice`] serves every transport
//! the build enables. The transports are independent of each other: TCP
//! (`std-tcp`) and BLE (`std-ble`) carry the same Protobuf messages to the same
//! controller, with their own framing.
//!
//! ```text
//! rohi CLI -> TCP records (rohi-control-tcp) -> ControlTcp --+
//!                                                            +-> Controller -> UrbanDevice
//! mobile   -> GATT (BlueZ, rohi-control-ble) -> ControlBle --+
//! ```
//!
//! Neither transport has a link security policy yet: [`OpenPolicy`] allows
//! everything, including identity provisioning and erase. TCP is bound to the
//! loopback address unless the developer asks otherwise, see
//! [`Args::unsafe_allow_remote`](super::args::Args::unsafe_allow_remote).

use rohi_sdk::control::{Controller, DeviceInfo, OpenPolicy};
use rohi_sdk::host::NativeIp;
use rohi_sdk::net::{BleName, BleProbe, BleState, DEFAULT_BLE_PAYLOAD, NetworkHandle};
use rohi_sdk::sim::ip::SimIp;
use static_cell::StaticCell;

use super::logging;
use crate::control::{Platform, ServicesSlot, UrbanDevice};
use crate::{config, identity, reboot};

/// Firmware name and version.
const FIRMWARE: &str = concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"));

/// Board of the host platform.
const BOARD: &str = "urban-std";

/// The IP backend of the host platform: the sockets of the host behind the
/// faults of the simulated world.
pub type HostIp = SimIp<NativeIp>;

/// The view of the BLE peripheral for the `ble` service.
#[cfg(feature = "std-ble")]
pub type HostBle = rohi_sdk::host::ble::BluezProbe;

/// The view of the BLE peripheral for the `ble` service.
#[cfg(not(feature = "std-ble"))]
pub type HostBle = NoBle;

/// The `ble` service of a build without a BLE peripheral: it is disabled, not
/// pretending to advertise.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoBle;

impl BleProbe for NoBle {
    fn state(&self) -> BleState {
        BleState::Disabled
    }

    fn max_payload(&self) -> usize {
        DEFAULT_BLE_PAYLOAD
    }

    fn name(&self) -> Option<BleName> {
        None
    }
}

/// The control backend of the Altruist Urban on the host.
pub type HostDevice = UrbanDevice<HostIp, HostBle, logging::Service>;

/// The controller every transport serves.
pub type HostController = Controller<HostDevice, OpenPolicy>;

static SERVICES: ServicesSlot<HostIp, HostBle, logging::Service> = StaticCell::new();
static CONTROLLER: StaticCell<HostController> = StaticCell::new();

/// The controller over the runtime handles of the application, built once.
///
/// `network` is the IP data plane; `ble` the view of the BLE peripheral for
/// the `ble` service.
pub fn controller(
    network: Option<NetworkHandle<'static, HostIp>>,
    ble: HostBle,
) -> &'static HostController {
    let device = crate::control::device(
        Platform {
            info: DeviceInfo {
                firmware: FIRMWARE,
                board: BOARD,
            },
            services: &SERVICES,
            network,
            heap: None,
            ble,
            logging: logging::runtime().service(),
            reboot: reboot::request,
        },
        config::handle(),
        identity::start(config::handle()),
    );
    CONTROLLER.init(Controller::new(device, OpenPolicy))
}

/// Serves control requests received over TCP.
#[cfg(feature = "std-tcp")]
#[embassy_executor::task]
pub async fn tcp_task(acceptor: rohi_sdk::host::TcpAcceptor, controller: &'static HostController) {
    let mut control = rohi_sdk::tcp::ControlTcp::new(acceptor, controller);
    let error = control.run().await;
    log::error!("control: the TCP listener failed: {error}");
}

/// Serves control requests received over the BLE peripheral.
#[cfg(feature = "std-ble")]
#[embassy_executor::task]
pub async fn ble_task(
    transport: rohi_sdk::host::ble::BluezBle,
    controller: &'static HostController,
) {
    let mut control = rohi_sdk::ble::ControlBle::new(transport, controller);
    control.run().await
}
