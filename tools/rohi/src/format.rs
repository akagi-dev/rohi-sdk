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
//! Human readable rendering of the model.

use core::fmt::{self, Write};

use crate::model::*;

fn value(measurement: &Measurement) -> String {
    match measurement.value {
        Some(value) => {
            let precision = match measurement.kind {
                MeasurementType::Pressure
                | MeasurementType::Co2
                | MeasurementType::AqiFast
                | MeasurementType::AqiEpa
                | MeasurementType::PulseCount => 0,
                _ => 1,
            };
            format!("{value:.precision$} {}", measurement.unit)
                .trim_end()
                .to_owned()
        }
        None => "-".to_owned(),
    }
}

fn age(age_ms: Option<u64>) -> String {
    match age_ms {
        None => String::new(),
        Some(ms) if ms < 1_000 => format!("{ms} ms ago"),
        Some(ms) => format!("{} s ago", ms / 1_000),
    }
}

/// Device information.
pub fn info(info: &DeviceInfo, capabilities: &[Capability]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Firmware: {}", info.firmware);
    let _ = writeln!(out, "Board:    {}", info.board);
    let names: Vec<_> = capabilities.iter().map(Capability::as_str).collect();
    let _ = writeln!(out, "Features: {}", names.join(", "));
    out
}

/// Sensors with their measurements.
pub fn sensors(sensors: &Sensors) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Uptime: {} s", sensors.uptime_ms / 1_000);
    if sensors.sensors.is_empty() {
        let _ = writeln!(out, "No sensors detected");
    }
    for sensor in &sensors.sensors {
        let _ = write!(out, "{} ({}", sensor.name, sensor.state);
        if let Some(error) = sensor.error {
            let _ = write!(out, ", last error: {error}");
        }
        let _ = writeln!(out, ")");
        for measurement in &sensor.measurements {
            let _ = writeln!(
                out,
                "  {:<14} {:>14}  {:<12} {}",
                measurement.kind.as_str(),
                value(measurement),
                measurement.state.as_str(),
                age(measurement.age_ms)
            );
        }
    }
    out
}

/// Stored configuration.
pub fn config(config: &Config) -> String {
    match &config.wifi {
        None => "Wi-Fi: not configured\n".to_owned(),
        Some(wifi) => format!(
            "Wi-Fi: {} (password {})\n",
            wifi.ssid,
            if wifi.password_configured {
                "set"
            } else {
                "not set"
            }
        ),
    }
}

/// Identity state.
pub fn identity(identity: &IdentityInfo) -> String {
    match identity.public_key {
        Some(key) => format!("State:      {}\nPublic key: {key}\n", identity.state),
        None => format!("State: {}\n", identity.state),
    }
}

/// Wi-Fi status.
pub fn wifi_status(status: &WifiStatus) -> String {
    let mut out = format!("Wi-Fi: {}", status.state);
    if let Some(failure) = status.failure {
        let _ = write!(out, " ({failure})");
    }
    out.push('\n');
    if let Some(connection) = &status.connection {
        let _ = writeln!(
            out,
            "  network: {} ({} dBm)",
            connection.ssid, connection.rssi_dbm
        );
        let _ = writeln!(out, "  ip:      {}", connection.ipv4);
        if let Some(gateway) = connection.gateway {
            let _ = writeln!(out, "  gateway: {gateway}");
        }
        if let Some(dns) = connection.dns {
            let _ = writeln!(out, "  dns:     {dns}");
        }
    }
    out
}

/// Scan results.
pub fn wifi_scan(networks: &[WifiNetwork]) -> String {
    if networks.is_empty() {
        return "No networks found\n".to_owned();
    }
    let mut out = String::new();
    let _ = writeln!(out, "{:<34} {:>8} {:>3}  security", "SSID", "signal", "ch");
    for network in networks {
        let _ = writeln!(
            out,
            "{:<34} {:>5} dBm {:>3}  {}",
            network.ssid.to_text(),
            network.rssi_dbm,
            network.channel,
            network.security
        );
    }
    out
}

/// Devices found by a scan.
pub fn devices(devices: &[crate::DiscoveredDevice]) -> String {
    if devices.is_empty() {
        return "No ROHI devices found\n".to_owned();
    }
    let mut out = String::new();
    for device in devices {
        let _ = writeln!(
            out,
            "{}  {}{}",
            device.id,
            device.name.as_deref().unwrap_or("(no name)"),
            device
                .rssi
                .map(|rssi| format!("  {rssi} dBm"))
                .unwrap_or_default()
        );
    }
    out
}

impl fmt::Display for Measurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, value(self))
    }
}
