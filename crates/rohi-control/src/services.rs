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
use rohi_hal::MeasurementsSnapshot;
use rohi_identity::{Identity, PublicKey};
use rohi_net::{NetworkInfo, WifiConfig, WifiError, WifiScanResults, WifiStatus};

use crate::Capabilities;

/// Static description of the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Firmware name and version, for example `altruist-urban-0.1.0`.
    pub firmware: &'static str,
    /// Board and chip, for example `urban-esp32c3`.
    pub board: &'static str,
}

/// Failure of a service call, mapped one-to-one to a protocol error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceError {
    /// A value is invalid.
    InvalidArgument,
    /// The addressed object does not exist.
    NotFound,
    /// The service does not support the operation.
    NotSupported,
    /// The service is busy; the client may retry.
    Busy,
    /// The service is temporarily unavailable.
    Unavailable,
    /// Unexpected failure.
    Internal,
    /// The operation needs a device identity, but none is provisioned.
    IdentityNotProvisioned,
    /// The device already has an identity.
    IdentityAlreadyProvisioned,
}

impl From<WifiError> for ServiceError {
    fn from(error: WifiError) -> Self {
        match error {
            WifiError::NotConfigured => Self::NotFound,
            WifiError::Busy => Self::Busy,
            WifiError::InvalidConfig => Self::InvalidArgument,
            WifiError::Timeout | WifiError::ScanFailed => Self::Unavailable,
            _ => Self::Internal,
        }
    }
}

/// The firmware side of the control plane.
///
/// A firmware adapter implements this on top of its runtime handles
/// (measurements, configuration store, Wi-Fi manager). The dispatcher never
/// sees stores, flash layouts or radio drivers.
#[allow(async_fn_in_trait)]
pub trait ControlServices {
    /// Static device description.
    fn device_info(&self) -> DeviceInfo;

    /// Capabilities this device offers. Requests that need a missing
    /// capability fail with `NOT_SUPPORTED`.
    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL
    }

    /// Fresh measurement snapshot, generated at the time of the call.
    async fn measurements(&self) -> Result<MeasurementsSnapshot, ServiceError>;

    /// Stored Wi-Fi configuration, `None` when not configured.
    async fn wifi_config(&self) -> Result<Option<WifiConfig>, ServiceError>;

    /// Persists the Wi-Fi configuration (`None` removes it) and publishes it
    /// to the Wi-Fi runtime as the desired configuration. Only this section
    /// may be touched. Completes when the configuration is committed, not
    /// when the network is reached.
    ///
    /// Cancellation: before the commit the state is unchanged; once the
    /// commit happened the runtime is guaranteed to learn the new
    /// configuration even if the caller is dropped, because publication is
    /// done by the owner of the store, with no cancellable gap in between.
    async fn set_wifi_config(&self, config: Option<&WifiConfig>) -> Result<(), ServiceError>;

    /// Scans for networks.
    async fn wifi_scan(&self) -> Result<WifiScanResults, ServiceError>;

    /// Current Wi-Fi status.
    fn wifi_status(&self) -> WifiStatus;

    /// Connection details, `Some` only while the network is ready.
    fn wifi_network_info(&self) -> Option<NetworkInfo>;

    /// Reconnects immediately.
    fn wifi_reconnect(&self) -> Result<(), ServiceError>;

    /// Disconnects and stays idle.
    fn wifi_disconnect(&self) -> Result<(), ServiceError>;

    /// Public key of the stored identity, `None` when not provisioned.
    /// The secret is never returned.
    async fn identity(&self) -> Result<Option<PublicKey>, ServiceError>;

    /// Persists `identity`. Only the identity section may be touched, and
    /// only the controller decides whether replacing is allowed.
    async fn store_identity(&self, identity: &Identity) -> Result<(), ServiceError>;

    /// Removes the identity section; a no-op when absent.
    async fn erase_identity(&self) -> Result<(), ServiceError>;
}
