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
//! Local control plane: the Altruist Urban side of `rohi-control`.
//!
//! ```text
//! BLE radio -> rohi-net-esp (EspBle) -> rohi-control-ble -> rohi-control
//!                                                              |
//!                                             UrbanDevice (this module)
//!                          config sections: wifi, connectivity, location, time
//!                          status: wifi, network, ble, connectivity, location, time,
//!                                  sensors, identity
//!                          actions: wifi, identity
//! ```
//!
//! The registries only talk to runtime handles; they never own a store, a flash
//! region or the radio. Adding a config section means adding its binding to
//! `firmware_configs!` in [`crate::config`], which generates the section tuple
//! and its construction. Adding a service means adding its instance to
//! `UrbanServices` and to the tuple of every capability it has, the status
//! tuple if it implements `Status` and the action tuple if it implements
//! `Action`: the Embassy control task needs the concrete type, so the types and
//! the values of the tuples stay explicit. The protocol and the `rohi` tool
//! need no change.

use embassy_executor::Spawner;
use embassy_time::Delay;
use log::{error, info};
use rohi_connectivity::ConnectivityStatusService;
use rohi_control::{Controller, Device, DeviceInfo, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_hal::SensorsService;
use rohi_identity::{Identity, IdentityService, IdentityStore, IdentityStoreError, PublicKey};
use rohi_location::LocationService;
use rohi_net::{BleService, NetworkHandle, NetworkService, WifiService};
use rohi_net_esp::EspWireless;
use rohi_service::{ActionRegistry, Startup, StatusSet, Statuses};
use static_cell::StaticCell;

use crate::bindings::ble_settings;
use crate::config::{
    ConfigHandle, ConfigSections, ServiceError as ConfigError, StorageConfig, WifiSection,
    config_sections, maintenance, wifi_section,
};
use crate::identity::IdentityHandle;
use crate::resources::{BleResources, BleRunner, BleStatus, BleTransport, Ip};
use crate::tasks::RawMutex;
use crate::tasks::store;
use crate::{connectivity, location, net, reboot, time};

/// Firmware name and version.
const FIRMWARE: &str = concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"));

/// Board and chip.
#[cfg(feature = "esp32c3")]
const BOARD: &str = "urban-esp32c3";
#[cfg(feature = "esp32c6")]
const BOARD: &str = "urban-esp32c6";

/// The identity section behind the configuration owner task.
pub struct StoredIdentity(IdentityHandle);

impl IdentityStore for StoredIdentity {
    async fn public_key(&self) -> Result<Option<PublicKey>, IdentityStoreError> {
        self.0.public_key().await.map_err(identity_error)
    }

    async fn store(&self, identity: &Identity) -> Result<(), IdentityStoreError> {
        self.0.store(identity).await.map_err(identity_error)
    }

    async fn erase(&self) -> Result<(), IdentityStoreError> {
        self.0.erase().await.map_err(identity_error)
    }
}

fn identity_error(error: ConfigError) -> IdentityStoreError {
    match error {
        ConfigError::Unavailable(_) => IdentityStoreError::Unavailable,
        _ => IdentityStoreError::Internal,
    }
}

/// The config sections of the device plus the factory reset of the storage
/// behind them.
pub type UrbanConfig = StorageConfig<ConfigSections>;

/// The config sections, shared by the control plane and the services that
/// change their own section.
static SECTIONS: StaticCell<ConfigSections> = StaticCell::new();

/// The services of the device, by capability.
type UrbanWifi = Startup<WifiService<'static, RawMutex, Delay, net::Probe, &'static WifiSection>>;
type UrbanNetwork = Startup<NetworkService<'static, RawMutex, net::Probe>>;
type UrbanBle = BleService<BleStatus>;
type UrbanConnectivity = Startup<ConnectivityStatusService<'static, RawMutex>>;
type UrbanLocation = LocationService<'static, RawMutex>;
type UrbanSensors = SensorsService<'static, RawMutex>;
type UrbanIdentity = IdentityService<StoredIdentity>;

/// The instances of the services of the device.
///
/// Each service is created once and referenced by the registries of the
/// capabilities it has, so that no registry owns or copies a service.
struct UrbanServices {
    wifi: UrbanWifi,
    network: UrbanNetwork,
    ble: UrbanBle,
    connectivity: UrbanConnectivity,
    location: UrbanLocation,
    sensors: UrbanSensors,
    identity: UrbanIdentity,
}

/// The services that report status. The `time` service is the time runtime
/// itself.
type UrbanStatusServices = (
    &'static UrbanWifi,
    &'static UrbanNetwork,
    &'static UrbanBle,
    &'static UrbanConnectivity,
    &'static UrbanLocation,
    &'static time::Time,
    &'static UrbanSensors,
    &'static UrbanIdentity,
);

/// The status registry of the device.
pub type UrbanStatuses = Statuses<RawMutex, UrbanStatusServices>;

/// The services that offer actions.
pub type UrbanActions = (&'static UrbanWifi, &'static UrbanIdentity);

// Two services with one id would route requests to the wrong service; this is
// a build error already in `cargo check`.
const _: () = {
    assert!(<UrbanStatusServices as StatusSet<RawMutex>>::IDS_UNIQUE);
    assert!(<UrbanActions as ActionRegistry>::IDS_UNIQUE);
};

/// The service instances, shared by the status and the action registry.
static SERVICES: StaticCell<UrbanServices> = StaticCell::new();

/// The control backend of the Altruist Urban.
pub type UrbanDevice = Device<UrbanConfig, UrbanStatuses, UrbanActions>;

fn uptime_ms() -> u64 {
    time::service().monotonic_now().millis
}

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
) -> UrbanDevice {
    let network_failed = network.is_none();
    // The `wifi` service and the control plane share the one section: one
    // transaction lock and one revision for every change.
    let sections: &'static ConfigSections = SECTIONS.init(config_sections(config));
    let services: &'static UrbanServices = SERVICES.init(UrbanServices {
        wifi: Startup::new(
            WifiService::new(
                net::handle(),
                Delay,
                net::Probe::new(network),
                wifi_section(sections),
            ),
            network_failed,
        ),
        network: Startup::new(
            NetworkService::new(net::handle(), net::Probe::new(network)),
            network_failed,
        ),
        ble: BleService(ble),
        connectivity: Startup::new(
            ConnectivityStatusService::new(
                connectivity::shared(),
                connectivity::effective_config,
                location::shared(),
            ),
            network_failed,
        ),
        location: LocationService(location::shared()),
        sensors: SensorsService(store::handle()),
        identity: IdentityService(StoredIdentity(identity)),
    });
    Device::new(
        DeviceInfo {
            firmware: FIRMWARE,
            board: BOARD,
        },
        uptime_ms,
        StorageConfig::new(sections, maintenance()),
        Statuses::new((
            &services.wifi,
            &services.network,
            &services.ble,
            &services.connectivity,
            &services.location,
            time::service(),
            &services.sensors,
            &services.identity,
        )),
        (&services.wifi, &services.identity),
    )
    .with_reboot(reboot::request)
}

/// GATT layout of the ROHI Control Service.
pub const GATT: rohi_net::BleGattSpec = rohi_net::BleGattSpec {
    service: rohi_control_ble::uuid::SERVICE_UUID,
    rx: rohi_control_ble::uuid::RX_CHARACTERISTIC_UUID,
    tx: rohi_control_ble::uuid::TX_CHARACTERISTIC_UUID,
};

/// BLE defaults of the Altruist Urban: the name is `ALTRUIST-URBAN-` and the
/// last two bytes of the MAC until the `ble` section sets one.
pub const BLE_CONFIG: rohi_net::BleConfig = rohi_net::BleConfig {
    name_prefix: "ALTRUIST-URBAN",
    name: None,
    gatt: GATT,
};

/// [`BLE_CONFIG`] with the name of the `ble` section as the configuration
/// owner published it at boot: the defaults, the firmware overrides and the
/// stored overrides. The name is read once, when the peripheral starts.
pub fn ble_config() -> rohi_net::BleConfig {
    BLE_CONFIG.with_settings(&ble_settings())
}

/// Static storage of the BLE peripheral, sized by [`crate::resources`].
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
            spawner.spawn(ble_task(runner).unwrap());
            spawner.spawn(control_task(transport, device).unwrap());
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
pub async fn control_task(transport: BleTransport, device: UrbanDevice) {
    let controller = Controller::new(device, OpenPolicy);
    let mut control = ControlBle::new(transport, &controller);
    control.run().await
}
