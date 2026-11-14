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

//! Local control plane: the Altruist Urban side of `rohi-control`.
//!
//! ```text
//! control link (BLE on the ESP; BLE and/or TCP on `std`) -> rohi-control
//!                                                              |
//!                                             UrbanDevice (this module)
//!                          config sections: wifi, network, ble, connectivity, location, time, logging
//!                          status: wifi, network, ble, connectivity, location, time,
//!                                  sensors, identity, logging
//!                          actions: wifi, identity, logging (read, download, clear),
//!                                   system (reboot, factory_reset)
//! ```
//!
//! The registries only talk to runtime handles; they never own a store, a flash
//! region or the radio. Adding a config section means adding its binding to
//! `firmware_configs!` in [`crate::config`], which generates the section tuple
//! and its construction. Adding a service means adding its instance to
//! [`UrbanServices`] and to the tuple of every capability it has, the status
//! tuple if it implements `Status` and the action tuple if it implements
//! `Action`: the Embassy control task needs the concrete type, so the types and
//! the values of the tuples stay explicit. The protocol and the `rohi` tool
//! need no change.
//!
//! The composition is generic over the IP backend `I`, the BLE probe `B` and the
//! `logging` service `L` of the platform; the link, its task, the log storage and
//! the static storage of the services are the platform's.

use embassy_time::Delay;
use rohi_sdk::connectivity::ConnectivityStatusService;
use rohi_sdk::control::{Device, DeviceInfo};
use rohi_sdk::identity::{Identity, IdentityService, IdentityStore, IdentityStoreError, PublicKey};
use rohi_sdk::location::LocationService;
use rohi_sdk::net::{BleProbe, BleService, IpBackend, NetworkHandle, NetworkService, WifiService};
use rohi_sdk::sensors::SensorsService;
use rohi_sdk::service::{
    Action, ActionRegistry, ConfigError as RegistryError, ConfigRegistry, FactoryReset, Startup,
    Status, StatusSet, Statuses,
};
use rohi_sdk::system::{SystemError, SystemPlatform, SystemService};
use static_cell::StaticCell;

use crate::config::{
    ConfigHandle, ConfigSections, ServiceError as ConfigError, StorageConfig, config_sections,
    maintenance,
};
use crate::identity::IdentityHandle;
use crate::tasks::RawMutex;
use crate::tasks::store;
use crate::{connectivity, location, net, telemetry, time};

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

/// The config sections, owned by the control plane.
static SECTIONS: StaticCell<ConfigSections> = StaticCell::new();

/// The services of the device, by capability.
type UrbanWifi<I> = Startup<WifiService<'static, RawMutex, Delay, net::Probe<I>>>;
type UrbanNetwork<I> = Startup<NetworkService<'static, RawMutex, net::Probe<I>>>;
type UrbanBle<B> = BleService<B>;
type UrbanConnectivity = Startup<ConnectivityStatusService<'static, RawMutex>>;
type UrbanLocation = LocationService<'static, RawMutex>;
type UrbanSensors = SensorsService<'static, RawMutex>;
type UrbanIdentity = IdentityService<StoredIdentity>;
type UrbanSystem = SystemService<UrbanSystemPlatform>;
type UrbanTelemetry = telemetry::UrbanTelemetry;

/// What the `system` service needs from the Altruist Urban: the deferred reboot
/// of the platform and the storage-aware factory reset of the configuration.
///
/// The factory reset is that of [`UrbanConfig`]: it formats the whole
/// configuration region, so the identity, the overrides and the sections this
/// firmware does not know go away, while the log partition and the rest of the
/// flash stay. It never reboots; `system.reboot` does.
pub struct UrbanSystemPlatform {
    config: UrbanConfig,
    reboot: fn(),
}

impl SystemPlatform for UrbanSystemPlatform {
    fn schedule_reboot(&self) -> Result<(), SystemError> {
        (self.reboot)();
        Ok(())
    }

    async fn factory_reset(&self) -> Result<FactoryReset, SystemError> {
        self.config
            .factory_reset()
            .await
            .map_err(|error| match error {
                RegistryError::Unavailable => SystemError::Unavailable,
                _ => SystemError::Internal,
            })
    }
}

/// The instances of the services of the device.
///
/// Each service is created once and referenced by the registries of the
/// capabilities it has, so that no registry owns or copies a service.
pub struct UrbanServices<I: 'static, B, L> {
    wifi: UrbanWifi<I>,
    network: UrbanNetwork<I>,
    ble: UrbanBle<B>,
    connectivity: UrbanConnectivity,
    location: UrbanLocation,
    sensors: UrbanSensors,
    identity: UrbanIdentity,
    telemetry: UrbanTelemetry,
    logging: L,
    system: UrbanSystem,
}

/// The services that report status. The `time` service is the time runtime
/// itself.
pub type UrbanStatusServices<I, B, L> = (
    &'static UrbanWifi<I>,
    &'static UrbanNetwork<I>,
    &'static UrbanBle<B>,
    &'static UrbanConnectivity,
    &'static UrbanLocation,
    &'static time::Time,
    &'static UrbanSensors,
    &'static UrbanIdentity,
    &'static UrbanTelemetry,
    &'static L,
);

/// The status registry of the device.
pub type UrbanStatuses<I, B, L> = Statuses<RawMutex, UrbanStatusServices<I, B, L>>;

/// The services that offer actions. `system` has no status.
pub type UrbanActions<I, L> = (
    &'static UrbanWifi<I>,
    &'static UrbanIdentity,
    &'static UrbanTelemetry,
    &'static L,
    &'static UrbanSystem,
);

/// The control backend of the Altruist Urban over the IP backend `I` and the
/// BLE probe `B` of the platform.
pub type UrbanDevice<I, B, L> = Device<UrbanConfig, UrbanStatuses<I, B, L>, UrbanActions<I, L>>;

/// The static slot of the service instances of the platform.
pub type ServicesSlot<I, B, L> = StaticCell<UrbanServices<I, B, L>>;

fn uptime_ms() -> u64 {
    time::service().monotonic_now().millis
}

/// What the platform brings to the device of [`device`].
pub struct Platform<I: 'static, B: 'static, L: 'static> {
    /// Firmware name and board, reported by `GetInfo`.
    pub info: DeviceInfo,
    /// The slot of the service instances, a `static` of the platform.
    pub services: &'static ServicesSlot<I, B, L>,
    /// The IP data plane; `None` tells that the network runtime did not
    /// start: its services then report a failed start instead of looking
    /// disabled.
    pub network: Option<NetworkHandle<'static, I>>,
    /// The probe of the wireless heap, if the platform has one.
    pub heap: Option<fn() -> rohi_sdk::net::HeapStats>,
    /// The view of the control link (BLE: the peripheral) for the `ble`
    /// service.
    pub ble: B,
    /// The `logging` service of the platform: its runtime owns the log storage.
    pub logging: L,
    /// Schedules the reset of the device for `system.reboot`, see
    /// [`crate::reboot`].
    pub reboot: fn(),
}

/// The device over the runtime handles of the firmware.
pub fn device<I, B, L>(
    platform: Platform<I, B, L>,
    config: ConfigHandle,
    identity: IdentityHandle,
) -> UrbanDevice<I, B, L>
where
    I: IpBackend + 'static,
    B: BleProbe + 'static,
    L: Status + Action + 'static,
{
    // Two services with one id would route requests to the wrong service; this
    // is a build error already in `cargo check`.
    const {
        assert!(<UrbanStatusServices<I, B, L> as StatusSet<RawMutex>>::IDS_UNIQUE);
        assert!(<UrbanActions<I, L> as ActionRegistry>::IDS_UNIQUE);
    }
    let Platform {
        info,
        services,
        network,
        heap,
        ble,
        logging,
        reboot: reboot_fn,
    } = platform;
    let network_failed = network.is_none();
    let sections: &'static ConfigSections = SECTIONS.init(config_sections(config));
    let storage_config = StorageConfig::new(sections, maintenance());
    let services: &'static UrbanServices<I, B, L> = services.init(UrbanServices {
        wifi: Startup::new(
            WifiService::new(net::handle(), Delay, net::Probe::new(network, heap)),
            network_failed,
        ),
        network: Startup::new(
            NetworkService::new(net::handle(), net::Probe::new(network, heap)),
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
        telemetry: telemetry::service(config),
        logging,
        system: SystemService(UrbanSystemPlatform {
            config: storage_config,
            reboot: reboot_fn,
        }),
    });
    Device::new(
        info,
        uptime_ms,
        storage_config,
        Statuses::new((
            &services.wifi,
            &services.network,
            &services.ble,
            &services.connectivity,
            &services.location,
            time::service(),
            &services.sensors,
            &services.identity,
            &services.telemetry,
            &services.logging,
        )),
        (
            &services.wifi,
            &services.identity,
            &services.telemetry,
            &services.logging,
            &services.system,
        ),
    )
}

/// GATT layout of the ROHI Control Service, the same on every platform.
pub const GATT: rohi_sdk::net::BleGattSpec = rohi_sdk::net::BleGattSpec {
    service: rohi_sdk::ble::uuid::SERVICE_UUID,
    rx: rohi_sdk::ble::uuid::RX_CHARACTERISTIC_UUID,
    tx: rohi_sdk::ble::uuid::TX_CHARACTERISTIC_UUID,
};

/// BLE defaults of the Altruist Urban: the name is `ALTRUIST-URBAN-` and the
/// last two bytes of the MAC until the `ble` section sets one.
pub const BLE_CONFIG: rohi_sdk::net::BleConfig = rohi_sdk::net::BleConfig {
    name_prefix: "ALTRUIST-URBAN",
    name: None,
    gatt: GATT,
};

/// [`BLE_CONFIG`] with the name of the `ble` section as the configuration
/// owner published it at boot: the defaults, the firmware overrides and the
/// stored overrides. The name is read once, when the peripheral starts.
pub fn ble_config() -> rohi_sdk::net::BleConfig {
    BLE_CONFIG.with_settings(&crate::bindings::ble_settings())
}
