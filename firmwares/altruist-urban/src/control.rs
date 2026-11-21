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
//!                          config sections: wifi, network, ble, connectivity, location, time, logging,
//!                                           control (std platform only)
//!                          status: wifi, network, ble, connectivity, location, time,
//!                                  sensors, identity, logging, system,
//!                                  control
//!                          actions: wifi, identity, logging (read, download, clear),
//!                                   system (reboot, factory_reset),
//!                                   control (tcp_generate_psk)
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

use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::Delay;
use rohi_sdk::control::{ControlAccess, Device, DeviceInfo};
use rohi_sdk::crash::Report;
use rohi_sdk::endpoints::{ControlService, PSK_LEN, PskStore, PskStoreError, TcpPskSection};
use rohi_sdk::identity::{Identity, IdentityService, IdentityStore, IdentityStoreError, PublicKey};
use rohi_sdk::location::LocationService;
use rohi_sdk::net::{BleProbe, IpBackend, NetworkHandle, WirelessService};
use rohi_sdk::sensors::SensorsService;
use rohi_sdk::service::{
    Action, ActionRegistry, ConfigError as RegistryError, ConfigRegistry, FactoryReset, Startup,
    Status, StatusSet, Statuses,
};
use rohi_sdk::system::{SystemError, SystemPlatform, SystemService};
use rohi_sdk::time::EmbassyClock;
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

/// The access state of the control plane: the mode of the TCP endpoint, whether a TCP
/// pre-shared key is provisioned, and the state and counters of the endpoints. The configuration
/// owner publishes into it, the control worker reads it, and the `control` service reports it.
pub static ACCESS: ControlAccess = ControlAccess::new(access_entropy);

/// The random source of the TCP handshake, set by the platform before the control
/// plane starts. Without one no TCP client can complete the handshake.
type EntropySource = fn(&mut [u8]) -> bool;

static ENTROPY: BlockingMutex<CriticalSectionRawMutex, Cell<Option<EntropySource>>> =
    BlockingMutex::new(Cell::new(None));

fn access_entropy(dst: &mut [u8]) -> bool {
    ENTROPY.lock(Cell::get).is_some_and(|source| source(dst))
}

/// The access state of the control plane, see [`ACCESS`].
pub fn access() -> &'static ControlAccess {
    &ACCESS
}

/// Sets the cryptographic random source of the TCP handshake. It must return
/// `false` rather than weak bytes.
pub fn set_access_entropy(source: fn(&mut [u8]) -> bool) {
    ENTROPY.lock(|cell| cell.set(Some(source)));
}

/// The pre-shared key section behind the configuration owner task.
pub struct StoredPsk(ConfigHandle);

impl PskStore for StoredPsk {
    async fn store(&self, key: &[u8; PSK_LEN]) -> Result<(), PskStoreError> {
        // The section wipes itself when it is dropped, at the end of this call.
        let section = TcpPskSection::new(*key);
        self.0.set(&section).await.map_err(psk_error)
    }
}

fn psk_error(error: ConfigError) -> PskStoreError {
    match error {
        ConfigError::Unavailable(_) => PskStoreError::Unavailable,
        _ => PskStoreError::Internal,
    }
}

// The key the TCP frontend authenticates clients with: a process of the host platform holds it in
// memory, a device does not hold it at all (it has no TCP endpoint; the key stays in the flash).
#[cfg(feature = "std-tcp")]
static TCP_KEY: BlockingMutex<CriticalSectionRawMutex, Cell<Option<[u8; PSK_LEN]>>> =
    BlockingMutex::new(Cell::new(None));

/// Replaces the key of the TCP frontend: the stored one when committed or loaded, a key given on
/// the command line, or none.
#[cfg(feature = "std-tcp")]
pub fn set_tcp_key(key: Option<[u8; PSK_LEN]>) {
    use zeroize::Zeroize as _;

    let mut old = TCP_KEY.lock(|cell| cell.replace(key));
    old.zeroize();
}

/// The [`PskSource`](rohi_sdk::tcp::PskSource) of the TCP endpoint of the host platform.
#[cfg(feature = "std-tcp")]
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpKey;

#[cfg(feature = "std-tcp")]
impl rohi_sdk::tcp::PskSource for TcpKey {
    fn psk(&self) -> Option<[u8; PSK_LEN]> {
        TCP_KEY.lock(Cell::get)
    }
}

/// The config sections of the device plus the factory reset of the storage
/// behind them.
pub type UrbanConfig = StorageConfig<ConfigSections>;

/// The config sections, owned by the control plane.
static SECTIONS: StaticCell<ConfigSections> = StaticCell::new();

/// The services of the device, by capability.
type UrbanWireless<B> =
    Startup<WirelessService<'static, RawMutex, Delay, EmbassyClock, net::Probe, B>>;
type UrbanConnectivity = connectivity::UrbanConnectivity;
type UrbanLocation = LocationService<'static, RawMutex>;
type UrbanSensors = SensorsService<'static, RawMutex>;
type UrbanIdentity = IdentityService<StoredIdentity>;
type UrbanSystem = SystemService<UrbanSystemPlatform>;
type UrbanControl = ControlService<'static, StoredPsk>;
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
    boot_report: fn() -> Option<Report>,
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

    fn boot_report(&self) -> Option<Report> {
        (self.boot_report)()
    }

    fn boot_count(&self) -> Option<u32> {
        crate::config::boot_count()
    }
}

/// The instances of the services of the device.
///
/// Each service is created once and referenced by the registries of the
/// capabilities it has, so that no registry owns or copies a service.
pub struct UrbanServices<B, L> {
    wireless: UrbanWireless<B>,
    connectivity: UrbanConnectivity,
    location: UrbanLocation,
    sensors: UrbanSensors,
    identity: UrbanIdentity,
    telemetry: UrbanTelemetry,
    logging: L,
    system: UrbanSystem,
    control: UrbanControl,
}

/// The services that report status. The `time` service is the time runtime
/// itself.
pub type UrbanStatusServices<B, L> = (
    &'static UrbanWireless<B>,
    &'static UrbanConnectivity,
    &'static UrbanLocation,
    &'static time::Time,
    &'static UrbanSensors,
    &'static UrbanIdentity,
    &'static UrbanTelemetry,
    &'static L,
    &'static UrbanSystem,
    &'static UrbanControl,
);

/// The status registry of the device.
pub type UrbanStatuses<B, L> = Statuses<RawMutex, UrbanStatusServices<B, L>>;

/// The services that offer actions.
pub type UrbanActions<B, L> = (
    &'static UrbanWireless<B>,
    &'static UrbanIdentity,
    &'static UrbanTelemetry,
    &'static UrbanConnectivity,
    &'static L,
    &'static UrbanSystem,
    &'static UrbanControl,
);

/// The control backend of the Altruist Urban over the BLE probe `B` of the
/// platform.
pub type UrbanDevice<B, L> = Device<UrbanConfig, UrbanStatuses<B, L>, UrbanActions<B, L>>;

/// The static slot of the service instances of the platform.
pub type ServicesSlot<B, L> = StaticCell<UrbanServices<B, L>>;

fn uptime_ms() -> u64 {
    time::service().monotonic_now().millis
}

/// What the platform brings to the device of [`device`].
pub struct Platform<I: 'static, B: 'static, L: 'static> {
    /// Firmware name and board, reported by `GetInfo`.
    pub info: DeviceInfo,
    /// The slot of the service instances, a `static` of the platform.
    pub services: &'static ServicesSlot<B, L>,
    /// The IP data plane; `None` tells that the network runtime did not
    /// start: the `wireless` service then reports a failed start instead of
    /// looking disabled.
    pub network: Option<NetworkHandle<'static, I>>,
    /// The probe of the wireless heap, if the platform has one.
    pub heap: Option<fn() -> rohi_sdk::net::HeapStats>,
    /// The view of the control link (BLE: the peripheral) for the `wireless`
    /// service.
    pub ble: B,
    /// The `logging` service of the platform: its runtime owns the log storage.
    pub logging: L,
    /// Schedules the reset of the device for `system.reboot`, see
    /// [`crate::reboot`].
    pub reboot: fn(),
    /// Entropy for the nonces of encrypted telemetry sections.
    pub entropy: connectivity::EntropyFn,
    /// Why the device started and what its last panic was, for the `system`
    /// status; `None` on a platform that keeps no crash record.
    pub boot_report: fn() -> Option<Report>,
}

/// The device over the runtime handles of the firmware.
pub fn device<I, B, L>(
    platform: Platform<I, B, L>,
    config: ConfigHandle,
    identity: IdentityHandle,
) -> UrbanDevice<B, L>
where
    I: IpBackend + 'static,
    B: BleProbe + 'static,
    L: Status + Action + 'static,
{
    // Two services with one id would route requests to the wrong service; this
    // is a build error already in `cargo check`.
    const {
        assert!(<UrbanStatusServices<B, L> as StatusSet<RawMutex>>::IDS_UNIQUE);
        assert!(<UrbanActions<B, L> as ActionRegistry>::IDS_UNIQUE);
    }
    let Platform {
        info,
        services,
        network,
        heap,
        ble,
        logging,
        reboot: reboot_fn,
        entropy,
        boot_report,
    } = platform;
    let network_failed = network.is_none();
    let sections: &'static ConfigSections = SECTIONS.init(config_sections(config));
    let storage_config = StorageConfig::new(sections, maintenance());
    let services: &'static UrbanServices<B, L> = services.init(UrbanServices {
        wireless: Startup::new(
            WirelessService::new(
                net::handle(),
                Delay,
                EmbassyClock,
                net::Probe::new(heap),
                ble,
            ),
            network_failed,
        ),
        connectivity: connectivity::service(
            config,
            connectivity::packets(store::handle(), entropy),
        ),
        location: LocationService(location::shared()),
        sensors: SensorsService(store::handle()),
        identity: IdentityService(StoredIdentity(identity)),
        telemetry: telemetry::service(config),
        logging,
        system: SystemService(UrbanSystemPlatform {
            config: storage_config,
            reboot: reboot_fn,
            boot_report,
        }),
        control: ControlService::new(&ACCESS, StoredPsk(config)),
    });
    Device::new(
        info,
        uptime_ms,
        storage_config,
        Statuses::new((
            &services.wireless,
            &services.connectivity,
            &services.location,
            time::service(),
            &services.sensors,
            &services.identity,
            &services.telemetry,
            &services.logging,
            &services.system,
            &services.control,
        )),
        (
            &services.wireless,
            &services.identity,
            &services.telemetry,
            &services.connectivity,
            &services.logging,
            &services.system,
            &services.control,
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
/// last two bytes of the MAC until the `ble_name` field of the `wireless` section sets one.
pub const BLE_CONFIG: rohi_sdk::net::BleConfig = rohi_sdk::net::BleConfig {
    name_prefix: "ALTRUIST-URBAN",
    name: None,
    pairing: false,
    gatt: GATT,
};

/// [`BLE_CONFIG`] with the `ble_name` of the `wireless` section as the configuration
/// owner published it at boot: the defaults, the firmware overrides and the
/// stored overrides. Read once, when the peripheral starts. BLE control runs without
/// pairing and without link encryption in this build.
pub fn ble_config() -> rohi_sdk::net::BleConfig {
    BLE_CONFIG.with_settings(&crate::bindings::ble_settings())
}
