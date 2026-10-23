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
//! The JSON written by `-o json`. These types are owned by the command line
//! and versioned with `schema_version`; they are deliberately not the model
//! types of the library, which may change independently.

use serde::Serialize;

use crate::DiscoveredDevice;
use crate::model::*;

/// Version of every JSON document of the command line.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
pub struct Versioned<T> {
    schema_version: u32,
    #[serde(flatten)]
    body: T,
}

impl<T> Versioned<T> {
    pub fn new(body: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            body,
        }
    }
}

/// Identifier of a capability in JSON: its name with underscores.
pub fn capability_id(capability: &Capability) -> String {
    capability.as_str().replace(' ', "_")
}

#[derive(Serialize)]
pub struct Done {
    pub ok: bool,
}

pub fn done() -> Versioned<Done> {
    Versioned::new(Done { ok: true })
}

#[derive(Serialize)]
pub struct ScanDevice {
    pub id: String,
    pub name: Option<String>,
    pub rssi_dbm: Option<i16>,
    pub uri: String,
}

#[derive(Serialize)]
pub struct Scan {
    pub devices: Vec<ScanDevice>,
}

/// The `ble://` URI of a discovered device: the advertised name, or the ID.
pub fn ble_uri(device: &DiscoveredDevice) -> String {
    format!("ble://{}", device.name.as_deref().unwrap_or(&device.id))
}

impl Scan {
    pub fn new(devices: &[DiscoveredDevice]) -> Self {
        Self {
            devices: devices
                .iter()
                .map(|device| ScanDevice {
                    id: device.id.clone(),
                    name: device.name.clone(),
                    rssi_dbm: device.rssi,
                    uri: ble_uri(device),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct Connection {
    pub uri: String,
}

#[derive(Serialize)]
pub struct Device {
    pub firmware: String,
    pub board: String,
}

#[derive(Serialize)]
pub struct Wifi {
    pub state: &'static str,
    pub failure: Option<&'static str>,
    pub ssid: Option<String>,
    pub signal_dbm: Option<i32>,
    pub address: Option<String>,
    pub gateway: Option<String>,
    pub dns: Option<String>,
}

impl Wifi {
    pub fn new(status: &WifiStatus) -> Self {
        let connection = status.connection.as_ref();
        Self {
            state: status.state.as_str(),
            failure: status.failure.map(|failure| failure.as_str()),
            ssid: connection.map(|c| c.ssid.to_text()),
            signal_dbm: connection.map(|c| c.rssi_dbm),
            address: connection.map(|c| c.ipv4.to_string()),
            gateway: connection.and_then(|c| c.gateway).map(|a| a.to_string()),
            dns: connection.and_then(|c| c.dns).map(|a| a.to_string()),
        }
    }
}

#[derive(Serialize)]
pub struct Identity {
    pub state: &'static str,
    /// The public key as a Robonomics (SS58) address.
    pub address: Option<String>,
    /// The public key, hex.
    pub public_key: Option<String>,
}

impl Identity {
    pub fn new(identity: &IdentityInfo) -> Self {
        Self {
            state: identity.state.as_str(),
            address: identity.public_key.as_ref().map(super::identity::address),
            public_key: identity.public_key.as_ref().map(|key| key.to_string()),
        }
    }
}

#[derive(Serialize)]
pub struct Status {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<Connection>,
    pub device: Device,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wifi: Option<Wifi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
}

#[derive(Serialize)]
pub struct Measurement {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub unit: &'static str,
    pub value: Option<f64>,
    pub state: &'static str,
    pub age_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct Sensor {
    pub name: String,
    pub state: &'static str,
    pub error: Option<&'static str>,
    pub measurements: Vec<Measurement>,
}

#[derive(Serialize)]
pub struct Sensors {
    pub uptime_ms: u64,
    pub sensors: Vec<Sensor>,
}

impl Sensors {
    pub fn new(sensors: &crate::model::Sensors) -> Self {
        Self {
            uptime_ms: sensors.uptime_ms,
            sensors: sensors
                .sensors
                .iter()
                .map(|sensor| Sensor {
                    name: sensor.name.clone(),
                    state: sensor.state.as_str(),
                    error: sensor.error.map(|error| error.as_str()),
                    measurements: sensor
                        .measurements
                        .iter()
                        .map(|m| Measurement {
                            kind: m.kind.as_str(),
                            unit: m.unit,
                            value: m.value,
                            state: m.state.as_str(),
                            age_ms: m.age_ms,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct WifiConfig {
    pub ssid: String,
    pub password_configured: bool,
}

#[derive(Serialize)]
pub struct Config {
    pub wifi: Option<WifiConfig>,
}

impl Config {
    pub fn new(config: &crate::model::Config) -> Self {
        Self {
            wifi: config.wifi.as_ref().map(|wifi| WifiConfig {
                ssid: wifi.ssid.to_text(),
                password_configured: wifi.password_configured,
            }),
        }
    }
}

#[derive(Serialize)]
pub struct Network {
    pub ssid: String,
    pub signal_dbm: i32,
    pub channel: u32,
    pub security: &'static str,
}

#[derive(Serialize)]
pub struct Networks {
    pub networks: Vec<Network>,
}

impl Networks {
    pub fn new(networks: &[WifiNetwork]) -> Self {
        Self {
            networks: networks
                .iter()
                .map(|network| Network {
                    ssid: network.ssid.to_text(),
                    signal_dbm: network.rssi_dbm,
                    channel: network.channel,
                    security: network.security.as_str(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct Provisioned {
    pub ok: bool,
    pub address: String,
    pub public_key: String,
}

#[derive(Serialize)]
pub struct Connected {
    pub ok: bool,
    /// The configuration is stored on the device.
    pub stored: bool,
    /// The device reported being connected.
    pub connected: bool,
    pub wifi: Option<Wifi>,
}

#[derive(Serialize)]
pub struct Written {
    pub ok: bool,
    pub file: String,
}
