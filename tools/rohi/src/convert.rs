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
//! Protobuf <-> model conversion.

use std::net::Ipv4Addr;

use rohi_control_proto::control as pb;

use crate::model::*;

pub(crate) fn info(info: pb::GetInfoResponse) -> DeviceInfo {
    DeviceInfo {
        firmware: info.firmware.as_str().to_owned(),
        board: info.board.as_str().to_owned(),
    }
}

pub(crate) fn capabilities(response: &pb::GetCapabilitiesResponse) -> Vec<Capability> {
    response
        .capabilities
        .iter()
        .map(|capability| Capability::from_proto(*capability))
        .collect()
}

fn measurement(measurement: &pb::Measurement) -> Measurement {
    let kind = MeasurementType::from_proto(measurement.r#type);
    Measurement {
        kind,
        unit: kind.unit(),
        value: measurement.value().copied(),
        state: MeasurementState::from_proto(measurement.state),
        age_ms: measurement.age_ms().copied(),
    }
}

pub(crate) fn sensors(response: &pb::GetSensorsResponse) -> Sensors {
    Sensors {
        uptime_ms: response.uptime_ms,
        sensors: response
            .sensors
            .iter()
            .map(|sensor| Sensor {
                name: sensor.name.as_str().to_owned(),
                state: SensorState::from_proto(sensor.state),
                error: sensor.error().map(|error| SensorError::from_proto(*error)),
                measurements: sensor.measurements.iter().map(measurement).collect(),
            })
            .collect(),
    }
}

pub(crate) fn config(response: &pb::GetConfigResponse) -> Result<Config, crate::Error> {
    let connectivity = response
        .connectivity()
        .map(|config| {
            let port = u16::try_from(config.port)
                .map_err(|_| crate::Error::Protocol("port is not a 16-bit number".into()))?;
            Ok::<_, crate::Error>(ConnectivityConfig {
                enabled: config.enabled,
                node_id: config.node_id,
                interval_s: config.interval_s,
                host: config.host.as_str().to_owned(),
                port,
            })
        })
        .transpose()?;
    Ok(Config {
        wifi: response.wifi().map(|wifi| WifiConfig {
            ssid: Ssid(wifi.ssid.to_vec()),
            password_configured: wifi.password_configured,
        }),
        connectivity,
    })
}

pub(crate) fn identity(response: &pb::GetIdentityResponse) -> Result<IdentityInfo, crate::Error> {
    let Some(info) = response.identity() else {
        return Err(crate::Error::Protocol("identity info is missing".into()));
    };
    let state = IdentityState::from_proto(info.state);
    let public_key = info.public_key().map(|key| public_key(key)).transpose()?;
    Ok(IdentityInfo { state, public_key })
}

pub(crate) fn public_key(bytes: &[u8]) -> Result<PublicKey, crate::Error> {
    PublicKey::from_slice(bytes)
        .ok_or_else(|| crate::Error::Protocol("public key is not 32 bytes".into()))
}

pub(crate) fn scan(response: &pb::ScanWifiResponse) -> Vec<WifiNetwork> {
    response
        .networks
        .iter()
        .map(|network| WifiNetwork {
            ssid: Ssid(network.ssid.to_vec()),
            rssi_dbm: network.rssi_dbm,
            channel: network.channel,
            security: WifiSecurity::from_proto(network.security),
        })
        .collect()
}

pub(crate) fn wifi_status(response: &pb::GetWifiStatusResponse) -> WifiStatus {
    WifiStatus {
        state: WifiState::from_proto(response.state),
        failure: response
            .failure()
            .map(|failure| WifiFailure::from_proto(*failure)),
        connection: response.connection().map(|connection| WifiConnection {
            ssid: Ssid(connection.ssid.to_vec()),
            rssi_dbm: connection.rssi_dbm,
            ipv4: Ipv4Addr::from(connection.ipv4),
            gateway: connection.gateway().map(|ip| Ipv4Addr::from(*ip)),
            dns: connection.dns().map(|ip| Ipv4Addr::from(*ip)),
        }),
    }
}

pub(crate) fn time_status(response: &pb::GetTimeStatusResponse) -> TimeStatus {
    TimeStatus {
        state: TimeSyncState::from_proto(response.state),
        unix_time_ms: response.unix_time_ms().copied(),
        source: response
            .source()
            .map(|source| TimeSource::from_proto(*source)),
        sync_age_ms: response.sync_age_ms().copied(),
        uncertainty_ms: response.uncertainty_ms().copied(),
        consecutive_failures: response.consecutive_failures,
        last_failure: response
            .last_failure()
            .map(|failure| TimeSyncFailure::from_proto(*failure)),
    }
}

/// Builds the wire form of a partial telemetry update.
pub(crate) fn connectivity_update(
    update: &ConnectivityUpdate,
) -> Result<pb::ConnectivityConfigUpdate, crate::Error> {
    let mut out = pb::ConnectivityConfigUpdate::default();
    if let Some(enabled) = update.enabled {
        out.set_enabled(enabled);
    }
    if let Some(node_id) = update.node_id {
        out.set_node_id(node_id);
    }
    if let Some(interval_s) = update.interval_s {
        let allowed =
            rohi_connectivity::config::MIN_INTERVAL_S..=rohi_connectivity::config::MAX_INTERVAL_S;
        if !allowed.contains(&interval_s) {
            return Err(crate::Error::InvalidInput(format!(
                "interval must be {} to {} seconds",
                allowed.start(),
                allowed.end()
            )));
        }
        out.set_interval_s(interval_s);
    }
    if let Some(host) = &update.host {
        if rohi_connectivity::Host::new(host).is_none() {
            return Err(crate::Error::InvalidInput(format!(
                "host must be 1 to {} letters, digits, '.' or '-'",
                rohi_connectivity::config::MAX_HOST_LEN
            )));
        }
        // Length checked above.
        let host = heapless::String::try_from(host.as_str()).map_err(|_| crate::Error::TooLarge)?;
        out.set_host(host);
    }
    if let Some(port) = update.port {
        if port == 0 {
            return Err(crate::Error::InvalidInput("port must not be 0".into()));
        }
        out.set_port(u32::from(port));
    }
    Ok(out)
}

/// Builds the wire form of a partial Wi-Fi update.
pub(crate) fn wifi_update(update: &WifiUpdate) -> Result<pb::WifiConfigUpdate, crate::Error> {
    use pb::WifiConfigUpdate_::PasswordUpdate;

    let mut out = pb::WifiConfigUpdate::default();
    if let Some(ssid) = &update.ssid {
        let ssid = heapless::Vec::from_slice(ssid)
            .map_err(|_| crate::Error::InvalidInput("SSID is longer than 32 bytes".into()))?;
        out.set_ssid(ssid);
    }
    out.password_update = match &update.password {
        PasswordChange::Keep => None,
        PasswordChange::Clear => Some(PasswordUpdate::ClearPassword(pb::Empty {})),
        PasswordChange::Set(password) => {
            let password = heapless::Vec::from_slice(password).map_err(|_| {
                crate::Error::InvalidInput("password is longer than 64 bytes".into())
            })?;
            Some(PasswordUpdate::Password(password))
        }
    };
    Ok(out)
}
