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
//! One [`Controller`] over the portable [`UrbanDevice`] runs in one
//! `control_task` and serves every transport the build enables. TCP
//! (`std-tcp`) and BLE (`std-ble`) are thin frontends: they frame their bytes
//! into the slots of one [`ControlHub`] and never call the controller.
//!
//! ```text
//! rohi CLI -> TCP records (rohi-control-tcp) -> ControlTcp --+
//!                                                            +-> ControlHub -> control_task
//! mobile   -> GATT (BlueZ, rohi-control-ble) -> ControlBle --+   (one Controller, UrbanDevice)
//! ```
//!
//! Each transport has its own access mode ([`crate::control::access`]): TCP is
//! restricted by default and wants a signature of the administrator key on
//! every privileged request, BLE is open. TCP is bound to the loopback address
//! unless the developer asks otherwise, see
//! [`Args::unsafe_allow_remote`](super::args::Args::unsafe_allow_remote).

use rohi_sdk::control::{AccessPolicy, ControlHub, Controller, DeviceInfo};
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

/// The controller every transport serves, through the [`HUB`].
pub type HostController = Controller<HostDevice, AccessPolicy<'static>>;

/// The request slots of the control worker: one per live connection of the two
/// transports.
pub static HUB: ControlHub<'static, 2> = ControlHub::new();

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
            entropy: super::app::host_entropy,
            boot_report: || None,
        },
        config::handle(),
        identity::start(config::handle()),
    );
    HUB.bind(crate::control::access());
    CONTROLLER.init(Controller::new(
        device,
        AccessPolicy::new(crate::control::access()),
    ))
}

/// Records the control listener, which the launch options may have moved away
/// from the stored `control_tcp` settings; `None` when it is switched off.
#[cfg(feature = "std-tcp")]
pub fn record_tcp(listening: Option<std::net::SocketAddr>) {
    use rohi_sdk::control::{EndpointState, Transport};

    let access = crate::control::access();
    match listening {
        Some(address) => {
            let octets = match address.ip() {
                std::net::IpAddr::V4(address) => address.octets(),
                std::net::IpAddr::V6(_) => [0; 4],
            };
            access.set_running(Transport::Tcp, true, octets, address.port());
            access.set_state(Transport::Tcp, EndpointState::Running);
        }
        None => {
            access.set_running(Transport::Tcp, false, [0; 4], 0);
            access.set_state(Transport::Tcp, EndpointState::Disabled);
        }
    }
}

/// The control worker: the one task that runs the controller.
#[embassy_executor::task]
pub async fn control_task(controller: &'static HostController) {
    HUB.serve(controller).await
}

/// Serves control requests received over TCP.
#[cfg(feature = "std-tcp")]
#[embassy_executor::task]
pub async fn tcp_task(acceptor: rohi_sdk::host::TcpAcceptor) {
    let mut control = rohi_sdk::tcp::ControlTcp::new(acceptor, &HUB);
    let error = control.run().await;
    log::error!("control: the TCP listener failed: {error}");
}

/// Serves control requests received over the BLE peripheral.
#[cfg(feature = "std-ble")]
#[embassy_executor::task]
pub async fn ble_task(transport: rohi_sdk::host::ble::BluezBle) {
    let mut control = rohi_sdk::ble::ControlBle::new(transport, &HUB);
    control.run().await
}
