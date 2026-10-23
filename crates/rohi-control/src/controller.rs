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
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply, UpdateConfigRequest_,
    WifiConfigUpdate_,
};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode, limits};
use rohi_net::{WifiConfig, WifiPassword, WifiSsid};

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
            Call::GetConfig(_) => match services.wifi_config().await {
                Ok(config) => {
                    let mut out = pb::GetConfigResponse::default();
                    if let Some(config) = config {
                        out.set_wifi(mapping::wifi_config(&config));
                    }
                    Reply::GetConfig(out)
                }
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
        }
    }

    /// Validate, then persist and publish as one service call. Nothing is
    /// stored when validation fails, and success does not mean the network
    /// was reached.
    async fn update_config(&self, update: pb::UpdateConfigRequest) -> Result<(), ServiceError> {
        if update.wifi_update.is_some()
            && !self
                .services
                .capabilities()
                .contains(Capabilities::WIFI_CONTROL)
        {
            // Without a Wi-Fi runtime nothing would ever take the update.
            return Err(ServiceError::NotSupported);
        }
        match update.wifi_update {
            // Nothing to update.
            None => Ok(()),
            Some(UpdateConfigRequest_::WifiUpdate::ClearWifi(_)) => {
                self.services.set_wifi_config(None).await
            }
            Some(UpdateConfigRequest_::WifiUpdate::UpdateWifi(update)) => {
                let current = self.services.wifi_config().await?;
                let merged = merge_wifi(current, &update)?;
                self.services.set_wifi_config(Some(&merged)).await
            }
        }
    }
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
    })
}
