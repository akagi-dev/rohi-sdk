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
use rohi_connectivity::{ConnectivityConfig, Host};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply, UpdateConfigRequest_,
    WifiConfigUpdate_,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};
use rohi_identity::{Identity, PublicKey};
use rohi_net::{WifiConfig, WifiPassword, WifiSsid};
use zeroize::Zeroize;

use crate::mapping;
use crate::{
    AuthorizationPolicy, Capabilities, ControlServices, LinkSecurity, Privilege, ServiceError,
};

/// Dispatches control requests to [`ControlServices`].
pub struct Controller<S, P> {
    services: S,
    policy: P,
}

impl<S: ControlServices, P: AuthorizationPolicy> Controller<S, P> {
    /// A dispatcher over `services`, gated by `policy`.
    pub const fn new(services: S, policy: P) -> Self {
        Self { services, policy }
    }

    /// The wrapped services.
    pub fn services(&self) -> &S {
        &self.services
    }

    /// Handles one encoded request received on a link with the given
    /// security and writes exactly one encoded response to `response`.
    ///
    /// Returns the response length. Every failure, including malformed
    /// requests and an undersized `response` buffer, is answered with a typed
    /// error response; the transport is never expected to disconnect. `0` is
    /// returned only if not even an error fits into `response`.
    pub async fn handle(&self, link: &LinkSecurity, request: &[u8], response: &mut [u8]) -> usize {
        let (request_id, reply) = self.dispatch(link, request).await;
        let message = pb::Response {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Some(reply),
        };
        if let Ok(len) = encode(&message, response) {
            return len;
        }
        let fallback = pb::Response {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Some(error(pb::ErrorCode::Internal)),
        };
        encode(&fallback, response).unwrap_or(0)
    }

    async fn dispatch(&self, link: &LinkSecurity, bytes: &[u8]) -> (u32, Reply) {
        if bytes.len() > limits::MAX_REQUEST_LEN {
            return (0, error(pb::ErrorCode::InvalidArgument));
        }
        let Ok(request) = decode::<pb::Request>(bytes) else {
            return (0, error(pb::ErrorCode::InvalidArgument));
        };
        let id = request.request_id;
        if request.protocol_version != PROTOCOL_VERSION {
            return (id, error(pb::ErrorCode::NotSupported));
        }
        let Some(call) = request.request else {
            return (id, error(pb::ErrorCode::NotSupported));
        };

        let (needs, privilege) = requirements(&call);
        if !self.services.capabilities().contains(needs) {
            return (id, error(pb::ErrorCode::NotSupported));
        }
        if !self.policy.allows(privilege, link) {
            return (id, error(pb::ErrorCode::PermissionDenied));
        }
        (id, self.execute(call).await)
    }

    async fn execute(&self, call: Call) -> Reply {
        let services = &self.services;
        match call {
            Call::GetInfo(_) => Reply::GetInfo(mapping::device_info(&services.device_info())),
            Call::GetCapabilities(_) => Reply::GetCapabilities(pb::GetCapabilitiesResponse {
                capabilities: services.capabilities().to_proto(),
            }),
            Call::GetSensors(_) => match services.measurements().await {
                Ok(snapshot) => Reply::GetSensors(mapping::sensors(&snapshot)),
                Err(e) => service_error(e),
            },
            Call::GetConfig(_) => match self.get_config().await {
                Ok(config) => Reply::GetConfig(config),
                Err(e) => service_error(e),
            },
            Call::UpdateConfig(update) => match self.update_config(update).await {
                Ok(()) => Reply::UpdateConfig(pb::UpdateConfigResponse {}),
                Err(e) => service_error(e),
            },
            Call::ScanWifi(_) => match services.wifi_scan().await {
                Ok(results) => Reply::ScanWifi(mapping::wifi_scan(&results)),
                Err(e) => service_error(e),
            },
            Call::GetWifiStatus(_) => Reply::GetWifiStatus(mapping::wifi_status(
                &services.wifi_status(),
                services.wifi_network_info().as_ref(),
            )),
            Call::ReconnectWifi(_) => match services.wifi_reconnect() {
                Ok(()) => Reply::ReconnectWifi(pb::ReconnectWifiResponse {}),
                Err(e) => service_error(e),
            },
            Call::DisconnectWifi(_) => match services.wifi_disconnect() {
                Ok(()) => Reply::DisconnectWifi(pb::DisconnectWifiResponse {}),
                Err(e) => service_error(e),
            },
            Call::GetTimeStatus(_) => {
                Reply::GetTimeStatus(mapping::time_status(&services.time_status()))
            }
            Call::GetIdentity(_) => match services.identity().await {
                Ok(key) => Reply::GetIdentity(mapping::identity(key)),
                Err(e) => service_error(e),
            },
            Call::ProvisionIdentity(request) => match self.provision_identity(request).await {
                Ok(key) => Reply::ProvisionIdentity(pb::ProvisionIdentityResponse {
                    public_key: mapping::public_key(&key),
                }),
                Err(e) => service_error(e),
            },
            Call::EraseIdentity(_) => match services.erase_identity().await {
                Ok(()) => Reply::EraseIdentity(pb::EraseIdentityResponse {}),
                Err(e) => service_error(e),
            },
        }
    }

    /// Installs the received secret as the device identity.
    ///
    /// Never replaces an existing identity: that needs an explicit erase
    /// first. The stored identity is read back and its public key compared
    /// before success is reported.
    async fn provision_identity(
        &self,
        mut request: pb::ProvisionIdentityRequest,
    ) -> Result<PublicKey, ServiceError> {
        let identity = Identity::from_secret(&request.ed25519_secret);
        request.ed25519_secret.as_mut_slice().zeroize();
        let identity = identity.map_err(|_| ServiceError::InvalidArgument)?;
        if self.services.identity().await?.is_some() {
            return Err(ServiceError::IdentityAlreadyProvisioned);
        }
        self.services.store_identity(&identity).await?;
        let expected = identity.public_key();
        match self.services.identity().await? {
            Some(stored) if stored == expected => Ok(expected),
            _ => Err(ServiceError::Internal),
        }
    }

    async fn get_config(&self) -> Result<pb::GetConfigResponse, ServiceError> {
        let mut out = pb::GetConfigResponse::default();
        if let Some(config) = self.services.wifi_config().await? {
            out.set_wifi(mapping::wifi_config(&config));
        }
        if self
            .services
            .capabilities()
            .contains(Capabilities::CONNECTIVITY)
        {
            match self.services.connectivity_config().await {
                Ok(Some(config)) => {
                    out.set_connectivity(mapping::connectivity_config(&config));
                }
                // A device that lists the capability but cannot read the
                // section has nothing to report.
                Ok(None) | Err(ServiceError::NotSupported) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Validate, then persist and publish as one service call per section.
    /// Nothing is stored when validation of any section fails, and success
    /// does not mean the network was reached.
    async fn update_config(&self, update: pb::UpdateConfigRequest) -> Result<(), ServiceError> {
        let capabilities = self.services.capabilities();
        if update.wifi_update.is_some() && !capabilities.contains(Capabilities::WIFI_CONTROL) {
            // Without a Wi-Fi runtime nothing would ever take the update.
            return Err(ServiceError::NotSupported);
        }
        if update.connectivity_update.is_some()
            && !capabilities.contains(Capabilities::CONNECTIVITY)
        {
            return Err(ServiceError::NotSupported);
        }
        // `None`: the section is not touched; `Some(None)`: it is removed.
        let wifi = match update.wifi_update {
            None => None,
            Some(UpdateConfigRequest_::WifiUpdate::ClearWifi(_)) => Some(None),
            Some(UpdateConfigRequest_::WifiUpdate::UpdateWifi(update)) => {
                let current = self.services.wifi_config().await?;
                Some(Some(merge_wifi(current, &update)?))
            }
        };
        let connectivity = match update.connectivity_update {
            None => None,
            Some(UpdateConfigRequest_::ConnectivityUpdate::ClearConnectivity(_)) => Some(None),
            Some(UpdateConfigRequest_::ConnectivityUpdate::UpdateConnectivity(update)) => {
                let current = self.services.connectivity_config().await?;
                Some(Some(merge_connectivity(current, &update)?))
            }
        };
        if let Some(config) = wifi {
            self.services.set_wifi_config(config.as_ref()).await?;
        }
        if let Some(config) = connectivity {
            self.services
                .set_connectivity_config(config.as_ref())
                .await?;
        }
        Ok(())
    }
}

/// Applies a partial update to the stored telemetry configuration: omitted
/// fields are preserved. Without a stored configuration the update starts
/// from a disabled one, so a host is required.
fn merge_connectivity(
    current: Option<ConnectivityConfig>,
    update: &pb::ConnectivityConfigUpdate,
) -> Result<ConnectivityConfig, ServiceError> {
    let mut config = current.unwrap_or_else(ConnectivityConfig::disabled);
    if let Some(enabled) = update.enabled() {
        config.enabled = *enabled;
    }
    if let Some(node_id) = update.node_id() {
        config.node_id = *node_id;
    }
    if let Some(interval_s) = update.interval_s() {
        config.interval_s = *interval_s;
    }
    if let Some(host) = update.host() {
        config.host = Host::new(host).ok_or(ServiceError::InvalidArgument)?;
    }
    if let Some(port) = update.port() {
        config.port = u16::try_from(*port).map_err(|_| ServiceError::InvalidArgument)?;
    }
    config
        .validate()
        .map_err(|_| ServiceError::InvalidArgument)?;
    Ok(config)
}

/// Applies a partial update to the stored configuration: omitted fields are
/// preserved, provided fields replace, explicit clear removes.
fn merge_wifi(
    current: Option<WifiConfig>,
    update: &pb::WifiConfigUpdate,
) -> Result<WifiConfig, ServiceError> {
    let ssid = match update.ssid() {
        Some(bytes) => WifiSsid::from_bytes(bytes).ok_or(ServiceError::InvalidArgument)?,
        None => current
            .as_ref()
            .ok_or(ServiceError::InvalidArgument)?
            .ssid
            .clone(),
    };
    let password = match &update.password_update {
        Some(WifiConfigUpdate_::PasswordUpdate::Password(bytes)) => {
            Some(WifiPassword::from_bytes(bytes).ok_or(ServiceError::InvalidArgument)?)
        }
        Some(WifiConfigUpdate_::PasswordUpdate::ClearPassword(_)) => None,
        None => current.and_then(|config| config.password),
    };
    let config = WifiConfig::new(ssid, password);
    config
        .validate()
        .map_err(|_| ServiceError::InvalidArgument)?;
    Ok(config)
}

fn requirements(call: &Call) -> (Capabilities, Privilege) {
    match call {
        Call::GetInfo(_) | Call::GetCapabilities(_) => (Capabilities::NONE, Privilege::Read),
        Call::GetSensors(_) => (Capabilities::SENSORS, Privilege::Read),
        Call::GetConfig(_) => (Capabilities::CONFIG, Privilege::Read),
        Call::UpdateConfig(_) => (Capabilities::CONFIG, Privilege::Mutate),
        Call::ScanWifi(_) => (Capabilities::WIFI_SCAN, Privilege::Read),
        Call::GetWifiStatus(_) => (Capabilities::WIFI_STATUS, Privilege::Read),
        Call::ReconnectWifi(_) | Call::DisconnectWifi(_) => {
            (Capabilities::WIFI_CONTROL, Privilege::Mutate)
        }
        Call::GetIdentity(_) => (Capabilities::IDENTITY, Privilege::Read),
        Call::GetTimeStatus(_) => (Capabilities::TIME, Privilege::Read),
        Call::ProvisionIdentity(_) | Call::EraseIdentity(_) => {
            (Capabilities::IDENTITY, Privilege::Provision)
        }
    }
}

fn error(code: pb::ErrorCode) -> Reply {
    Reply::Error(pb::Error { code })
}

fn service_error(error: ServiceError) -> Reply {
    self::error(match error {
        ServiceError::InvalidArgument => pb::ErrorCode::InvalidArgument,
        ServiceError::NotFound => pb::ErrorCode::NotFound,
        ServiceError::NotSupported => pb::ErrorCode::NotSupported,
        ServiceError::Busy => pb::ErrorCode::Busy,
        ServiceError::Unavailable => pb::ErrorCode::Unavailable,
        ServiceError::Internal => pb::ErrorCode::Internal,
        ServiceError::IdentityNotProvisioned => pb::ErrorCode::IdentityNotProvisioned,
        ServiceError::IdentityAlreadyProvisioned => pb::ErrorCode::IdentityAlreadyProvisioned,
    })
}
