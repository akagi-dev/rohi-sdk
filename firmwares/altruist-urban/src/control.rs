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
//!                                          UrbanControlServices (this module)
//!                                       MeasurementHandle ConfigHandle WifiHandle
//! ```
//!
//! The adapter only talks to runtime handles; it never owns a store, a flash
//! region or the radio.

use embassy_time::{Duration, with_timeout};
use rohi_control::{
    Capabilities, ControlServices, Controller, DeviceInfo, OpenPolicy, ServiceError,
};
use rohi_control_ble::ControlBle;
use rohi_hal::MeasurementsSnapshot;
use rohi_hal::runtime::MeasurementHandle;
use rohi_net::{NetworkInfo, WifiConfig, WifiHandle, WifiScanResults, WifiStatus};
use rohi_net_esp::{BleRunner, EspBle};

use crate::config::{ConfigHandle, ServiceError as ConfigError};
use crate::tasks::RawMutex;

/// Upper bound of a Wi-Fi scan requested over the control link.
const WIFI_SCAN_TIMEOUT: Duration = Duration::from_secs(20);

/// Firmware name and version.
const FIRMWARE: &str = concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"));

/// Board and chip.
#[cfg(feature = "esp32c3")]
const BOARD: &str = "urban-esp32c3";
#[cfg(feature = "esp32c6")]
const BOARD: &str = "urban-esp32c6";

/// [`ControlServices`] over the runtime handles of the firmware.
pub struct UrbanControlServices {
    measurements: MeasurementHandle<'static, RawMutex>,
    config: ConfigHandle,
    wifi: WifiHandle<'static, RawMutex>,
    capabilities: Capabilities,
}

impl UrbanControlServices {
    /// Adapter over the given handles. `capabilities` lists what the running
    /// firmware actually offers, for example without Wi-Fi when its driver
    /// failed to start.
    pub fn new(
        measurements: MeasurementHandle<'static, RawMutex>,
        config: ConfigHandle,
        wifi: WifiHandle<'static, RawMutex>,
        capabilities: Capabilities,
    ) -> Self {
        Self {
            measurements,
            config,
            wifi,
            capabilities,
        }
    }
}

fn config_error(error: ConfigError) -> ServiceError {
    match error {
        ConfigError::Unavailable(_) => ServiceError::Unavailable,
        _ => ServiceError::Internal,
    }
}

impl ControlServices for UrbanControlServices {
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            firmware: FIRMWARE,
            board: BOARD,
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn measurements(&self) -> Result<MeasurementsSnapshot, ServiceError> {
        self.measurements
            .snapshot()
            .await
            .map_err(|_| ServiceError::Busy)
    }

    async fn wifi_config(&self) -> Result<Option<WifiConfig>, ServiceError> {
        self.config.get::<WifiConfig>().await.map_err(config_error)
    }

    /// The configuration owner task publishes the committed value to the
    /// Wi-Fi manager itself, see [`crate::config`]; cancelling this call can
    /// therefore never leave a stored configuration unknown to the runtime.
    async fn set_wifi_config(&self, config: Option<&WifiConfig>) -> Result<(), ServiceError> {
        match config {
            Some(config) => self.config.set(config).await,
            None => self.config.remove::<WifiConfig>().await,
        }
        .map_err(config_error)
    }

    async fn wifi_scan(&self) -> Result<WifiScanResults, ServiceError> {
        match with_timeout(WIFI_SCAN_TIMEOUT, self.wifi.scan()).await {
            Ok(result) => result.map_err(Into::into),
            Err(_) => Err(ServiceError::Unavailable),
        }
    }

    fn wifi_status(&self) -> WifiStatus {
        self.wifi.status()
    }

    fn wifi_network_info(&self) -> Option<NetworkInfo> {
        self.wifi.network_info()
    }

    fn wifi_reconnect(&self) -> Result<(), ServiceError> {
        self.wifi.reconnect().map_err(Into::into)
    }

    fn wifi_disconnect(&self) -> Result<(), ServiceError> {
        self.wifi.disconnect().map_err(Into::into)
    }
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

/// Runs the BLE controller and host stack.
#[embassy_executor::task]
pub async fn ble_task(runner: BleRunner) {
    runner.run().await
}

/// Serves control requests received over BLE.
///
/// There is no link security policy in this version: the transport reports
/// the link security and the policy hook is in place, [`OpenPolicy`] allows
/// everything until identity provisioning arrives.
#[embassy_executor::task]
pub async fn control_task(transport: EspBle, services: UrbanControlServices) {
    let controller = Controller::new(services, OpenPolicy);
    let mut control = ControlBle::new(transport, &controller);
    control.run().await
}
