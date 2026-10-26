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
//!                          services: wifi, connectivity, time, sensors, identity
//! ```
//!
//! The registries only talk to runtime handles; they never own a store, a flash
//! region or the radio. Adding a config section or a service means adding it to
//! the tuples below; the protocol and the `rohi` tool need no change.

use core::marker::PhantomData;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Delay;
use rohi_config::ConfigSection;
use rohi_connectivity::{ConnectivityConfig, ConnectivityStatusService};
use rohi_control::{Controller, Device, DeviceInfo, OpenPolicy};
use rohi_control_ble::ControlBle;
use rohi_hal::SensorsService;
use rohi_hal_esp::EmbassyClock;
use rohi_identity::{Identity, IdentityService, IdentityStore, IdentityStoreError, PublicKey};
use rohi_location::{LocationConfig, LocationService};
use rohi_net::wifi::{WifiConfig, WifiService};
use rohi_service::{ManagedSection, SectionStore, StoreError, Tracked};
use rohi_time::TimeStatusService;
use rohi_time_ntp::NtpServersConfig;
use static_cell::StaticCell;

use crate::config::{ConfigHandle, ServiceError as ConfigError};
use crate::identity::IdentityHandle;
use crate::resources::{BleResources, BleRunner, BleTransport};
use crate::tasks::RawMutex;
use crate::tasks::store;
use crate::{connectivity, location, net, time};

/// Upper bound of a Wi-Fi scan requested over the control link.
const WIFI_SCAN_TIMEOUT_MS: u32 = 20_000;

/// Firmware name and version.
const FIRMWARE: &str = concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"));

/// Board and chip.
#[cfg(feature = "esp32c3")]
const BOARD: &str = "urban-esp32c3";
#[cfg(feature = "esp32c6")]
const BOARD: &str = "urban-esp32c6";

fn store_error(error: ConfigError) -> StoreError {
    match error {
        ConfigError::Unavailable(_) => StoreError::Unavailable,
        _ => StoreError::Internal,
    }
}

/// A persistent config section behind the configuration owner task.
///
/// The owner task publishes the committed value to the runtime itself, see
/// [`crate::config`]; cancelling a call can therefore never leave a stored
/// configuration unknown to the runtime.
pub struct Stored<C> {
    config: ConfigHandle,
    _section: PhantomData<fn() -> C>,
}

impl<C> Stored<C> {
    const fn new(config: ConfigHandle) -> Self {
        Self {
            config,
            _section: PhantomData,
        }
    }
}

impl<C: ConfigSection> SectionStore<C> for Stored<C> {
    async fn load(&self) -> Result<Option<C>, StoreError> {
        self.config.get::<C>().await.map_err(store_error)
    }

    async fn store(&self, config: &C) -> Result<(), StoreError> {
        self.config.set(config).await.map_err(store_error)
    }

    async fn remove(&self) -> Result<(), StoreError> {
        self.config.remove::<C>().await.map_err(store_error)
    }
}

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

type Section<C> = ManagedSection<C, Stored<C>, RawMutex>;

/// The config sections of the device.
pub type ConfigSections = (
    Section<WifiConfig>,
    Section<ConnectivityConfig>,
    Section<LocationConfig>,
    Section<NtpServersConfig>,
);

/// The services of the device.
pub type Services = (
    Tracked<WifiService<'static, RawMutex, Delay>, RawMutex>,
    Tracked<ConnectivityStatusService<'static, RawMutex>, RawMutex>,
    Tracked<LocationService<'static, RawMutex>, RawMutex>,
    Tracked<TimeStatusService<'static, EmbassyClock, CriticalSectionRawMutex>, RawMutex>,
    Tracked<SensorsService<'static, RawMutex>, RawMutex>,
    Tracked<IdentityService<StoredIdentity>, RawMutex>,
);

/// The control backend of the Altruist Urban.
pub type UrbanDevice = Device<ConfigSections, Services>;

fn uptime_ms() -> u64 {
    time::service().monotonic_now().millis
}

/// The registries over the runtime handles of the firmware.
pub fn device(config: ConfigHandle, identity: IdentityHandle) -> UrbanDevice {
    Device::new(
        DeviceInfo {
            firmware: FIRMWARE,
            board: BOARD,
        },
        uptime_ms,
        (
            ManagedSection::new(Stored::new(config)),
            ManagedSection::new(Stored::new(config)),
            ManagedSection::new(Stored::new(config)),
            ManagedSection::new(Stored::new(config)),
        ),
        (
            Tracked::new(WifiService::new(net::handle(), Delay, WIFI_SCAN_TIMEOUT_MS)),
            Tracked::new(ConnectivityStatusService::new(
                connectivity::shared(),
                connectivity::effective_config,
                location::shared(),
            )),
            Tracked::new(LocationService(location::shared())),
            Tracked::new(TimeStatusService(time::service())),
            Tracked::new(SensorsService(store::handle())),
            Tracked::new(IdentityService(StoredIdentity(identity))),
        ),
    )
}

/// GATT layout of the ROHI Control Service.
pub const GATT: rohi_net::BleGattSpec = rohi_net::BleGattSpec {
    service: rohi_control_ble::uuid::SERVICE_UUID,
    rx: rohi_control_ble::uuid::RX_CHARACTERISTIC_UUID,
    tx: rohi_control_ble::uuid::TX_CHARACTERISTIC_UUID,
};

/// BLE configuration of the Altruist Urban.
pub const BLE_CONFIG: rohi_net::BleConfig = rohi_net::BleConfig {
    name_prefix: "ALTRUIST-URBAN",
    gatt: GATT,
};

/// Static storage of the BLE peripheral, sized by [`crate::resources`].
pub static BLE_RESOURCES: StaticCell<BleResources> = StaticCell::new();

/// Runs the BLE controller and host stack.
#[embassy_executor::task]
pub async fn ble_task(runner: BleRunner) {
    runner.run().await
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
