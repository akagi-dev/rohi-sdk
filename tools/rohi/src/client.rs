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
//! High-level typed device API.

use std::time::Duration;

use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply, UpdateConfigRequest_,
};

use crate::convert;
use crate::error::{Error, Result};
use crate::model::*;
use crate::provision::DerivedIdentity;
use crate::session::{ClientTransport, Session};

/// Wi-Fi scans run on the device and take a while.
const SCAN_TIMEOUT: Duration = Duration::from_secs(40);

/// A connected ROHI device.
///
/// Every method checks the capability it needs first, so a device without
/// Wi-Fi answers [`Error::Unsupported`] without a round trip of the request
/// itself. Capabilities are read once, on first use.
pub struct Device<T> {
    session: Session<T>,
    capabilities: Option<Vec<Capability>>,
}

impl<T: ClientTransport> Device<T> {
    /// A device behind `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            session: Session::new(transport),
            capabilities: None,
        }
    }

    /// Changes the response timeout of normal requests.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.session.set_timeout(timeout);
    }

    /// Releases the transport, for example to close the connection.
    pub fn into_transport(self) -> T {
        self.session.into_transport()
    }

    async fn call(&mut self, call: Call) -> Result<Reply> {
        self.session.call(call).await
    }

    async fn require(&mut self, capability: Capability) -> Result<()> {
        let capabilities = self.capabilities().await?;
        if capabilities.contains(&capability) {
            Ok(())
        } else {
            Err(Error::Unsupported(capability))
        }
    }

    /// Device description.
    pub async fn info(&mut self) -> Result<DeviceInfo> {
        match self.call(Call::GetInfo(pb::GetInfoRequest {})).await? {
            Reply::GetInfo(info) => Ok(convert::info(info)),
            other => Err(unexpected(&other)),
        }
    }

    /// Features of the device; cached after the first call.
    pub async fn capabilities(&mut self) -> Result<Vec<Capability>> {
        if let Some(capabilities) = &self.capabilities {
            return Ok(capabilities.clone());
        }
        let reply = self
            .call(Call::GetCapabilities(pb::GetCapabilitiesRequest {}))
            .await?;
        let Reply::GetCapabilities(response) = reply else {
            return Err(unexpected(&reply));
        };
        let capabilities = convert::capabilities(&response);
        self.capabilities = Some(capabilities.clone());
        Ok(capabilities)
    }

    /// Current measurements of all sensors.
    pub async fn sensors(&mut self) -> Result<Sensors> {
        self.require(Capability::Sensors).await?;
        match self
            .call(Call::GetSensors(pb::GetSensorsRequest {}))
            .await?
        {
            Reply::GetSensors(response) => Ok(convert::sensors(&response)),
            other => Err(unexpected(&other)),
        }
    }

    /// Stored configuration. Secrets are never included.
    pub async fn config(&mut self) -> Result<Config> {
        self.require(Capability::Config).await?;
        match self.call(Call::GetConfig(pb::GetConfigRequest {})).await? {
            Reply::GetConfig(response) => Ok(convert::config(&response)),
            other => Err(unexpected(&other)),
        }
    }

    /// Applies a partial Wi-Fi update: omitted fields keep their value.
    ///
    /// Success means the device stored the configuration and handed it to its
    /// Wi-Fi runtime, not that the network was reached; follow
    /// [`Device::wifi_status`] for that.
    pub async fn update_wifi(&mut self, update: &WifiUpdate) -> Result<()> {
        self.require(Capability::Config).await?;
        let update = convert::wifi_update(update)?;
        let request = pb::UpdateConfigRequest {
            wifi_update: Some(UpdateConfigRequest_::WifiUpdate::UpdateWifi(update)),
        };
        self.update_config(request).await
    }

    /// Removes the stored Wi-Fi configuration; the device disconnects.
    pub async fn forget_wifi(&mut self) -> Result<()> {
        self.require(Capability::Config).await?;
        let request = pb::UpdateConfigRequest {
            wifi_update: Some(UpdateConfigRequest_::WifiUpdate::ClearWifi(pb::Empty {})),
        };
        self.update_config(request).await
    }

    async fn update_config(&mut self, request: pb::UpdateConfigRequest) -> Result<()> {
        match self.call(Call::UpdateConfig(request)).await? {
            Reply::UpdateConfig(_) => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Scans for Wi-Fi networks, strongest first.
    pub async fn wifi_scan(&mut self) -> Result<Vec<WifiNetwork>> {
        self.require(Capability::WifiScan).await?;
        let reply = self.session_with_timeout(SCAN_TIMEOUT, Call::ScanWifi(pb::ScanWifiRequest {}));
        match reply.await? {
            Reply::ScanWifi(response) => Ok(convert::scan(&response)),
            other => Err(unexpected(&other)),
        }
    }

    async fn session_with_timeout(&mut self, timeout: Duration, call: Call) -> Result<Reply> {
        let normal = self.session.timeout();
        self.session.set_timeout(timeout.max(normal));
        let reply = self.session.call(call).await;
        self.session.set_timeout(normal);
        reply
    }

    /// Wi-Fi status.
    pub async fn wifi_status(&mut self) -> Result<WifiStatus> {
        self.require(Capability::WifiStatus).await?;
        match self
            .call(Call::GetWifiStatus(pb::GetWifiStatusRequest {}))
            .await?
        {
            Reply::GetWifiStatus(response) => Ok(convert::wifi_status(&response)),
            other => Err(unexpected(&other)),
        }
    }

    /// Wall-clock time and synchronization state of the device.
    ///
    /// `unix_time_ms` is `None` while the device has no time; the device never
    /// reports uptime or a placeholder date instead.
    pub async fn time_status(&mut self) -> Result<TimeStatus> {
        self.require(Capability::Time).await?;
        match self
            .call(Call::GetTimeStatus(pb::GetTimeStatusRequest {}))
            .await?
        {
            Reply::GetTimeStatus(response) => Ok(convert::time_status(&response)),
            other => Err(unexpected(&other)),
        }
    }

    /// Asks the device to reconnect now.
    pub async fn wifi_reconnect(&mut self) -> Result<()> {
        self.require(Capability::WifiControl).await?;
        match self
            .call(Call::ReconnectWifi(pb::ReconnectWifiRequest {}))
            .await?
        {
            Reply::ReconnectWifi(_) => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Identity state and public key of the device. The secret is never
    /// available.
    pub async fn identity(&mut self) -> Result<IdentityInfo> {
        self.require(Capability::Identity).await?;
        match self
            .call(Call::GetIdentity(pb::GetIdentityRequest {}))
            .await?
        {
            Reply::GetIdentity(response) => convert::identity(&response),
            other => Err(unexpected(&other)),
        }
    }

    /// Installs the derived identity on a device that has none.
    ///
    /// Only the derived secret is sent. The device refuses to replace an
    /// existing identity ([`ErrorCode::IdentityAlreadyProvisioned`]), and the
    /// public key it reports must equal the one derived on the host, otherwise
    /// [`Error::PublicKeyMismatch`] is returned.
    pub async fn provision_identity(&mut self, identity: &DerivedIdentity) -> Result<PublicKey> {
        self.require(Capability::Identity).await?;
        let request = pb::ProvisionIdentityRequest {
            ed25519_secret: heapless::Vec::from_slice(identity.secret().expose_secret())
                .map_err(|_| Error::InvalidInput("identity secret is not 32 bytes".into()))?,
        };
        let reply = self.call(Call::ProvisionIdentity(request)).await;
        let reported = match reply? {
            Reply::ProvisionIdentity(response) => convert::public_key(&response.public_key)?,
            other => return Err(unexpected(&other)),
        };
        if reported != identity.public_key() {
            return Err(Error::PublicKeyMismatch {
                expected: identity.public_key(),
                reported,
            });
        }
        Ok(reported)
    }

    /// Removes the identity of the device; the device can then be provisioned
    /// again. Succeeds when there is no identity.
    pub async fn erase_identity(&mut self) -> Result<()> {
        self.require(Capability::Identity).await?;
        match self
            .call(Call::EraseIdentity(pb::EraseIdentityRequest {}))
            .await?
        {
            Reply::EraseIdentity(_) => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Asks the device to disconnect and stay idle.
    pub async fn wifi_disconnect(&mut self) -> Result<()> {
        self.require(Capability::WifiControl).await?;
        match self
            .call(Call::DisconnectWifi(pb::DisconnectWifiRequest {}))
            .await?
        {
            Reply::DisconnectWifi(_) => Ok(()),
            other => Err(unexpected(&other)),
        }
    }
}

fn unexpected(reply: &Reply) -> Error {
    Error::Protocol(format!(
        "response of the wrong type: {}",
        match reply {
            Reply::Error(_) => "error",
            Reply::GetInfo(_) => "get_info",
            Reply::GetCapabilities(_) => "get_capabilities",
            Reply::GetSensors(_) => "get_sensors",
            Reply::GetConfig(_) => "get_config",
            Reply::UpdateConfig(_) => "update_config",
            Reply::ScanWifi(_) => "scan_wifi",
            Reply::GetWifiStatus(_) => "get_wifi_status",
            Reply::ReconnectWifi(_) => "reconnect_wifi",
            Reply::DisconnectWifi(_) => "disconnect_wifi",
            Reply::GetIdentity(_) => "get_identity",
            Reply::ProvisionIdentity(_) => "provision_identity",
            Reply::EraseIdentity(_) => "erase_identity",
            Reply::GetTimeStatus(_) => "get_time_status",
        }
    ))
}
