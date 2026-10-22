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
//! Domain <-> protobuf mapping. The only place that knows both sides.

use heapless::String;
use rohi_control_proto::control as pb;
use rohi_control_proto::limits;
use rohi_hal::units::{
    Aqi, Celsius, Decibels, MicrogramsPerCubicMeter, Pascal, Ppb, Ppm, PulseCount, RadiationLevel,
    RelativeHumidity,
};
use rohi_hal::{
    Measurement, MeasurementState, MeasurementsSnapshot, MonotonicTimestamp, SensorErrorCode,
    SensorKind, SensorState,
};
use rohi_net::{
    NetworkInfo, WifiConfig, WifiError, WifiNetwork, WifiScanResults, WifiSecurity, WifiState,
    WifiStatus,
};

use crate::DeviceInfo;

// The generic response must be able to carry everything the typed model can
// produce.
const _: () = assert!(SensorKind::COUNT <= limits::MAX_SENSORS);
const _: () = assert!(rohi_net::MAX_WIFI_SCAN_RESULTS <= limits::MAX_WIFI_SCAN_RESULTS);
const _: () = assert!(rohi_net::MAX_SSID_LEN <= limits::MAX_SSID_LEN);
const _: () = assert!(rohi_net::MAX_WIFI_PASSWORD_LEN <= limits::MAX_WIFI_PASSWORD_LEN);

/// A typed unit value in the canonical unit of its measurement type.
trait Canonical: Copy {
    fn canonical(self) -> f64;
}

macro_rules! canonical {
    ($($unit:ty),+) => {$(
        impl Canonical for $unit {
            fn canonical(self) -> f64 {
                f64::from(self.0)
            }
        }
    )+};
}

canonical!(
    Celsius,
    RelativeHumidity,
    Pascal,
    Ppm,
    Ppb,
    Decibels,
    MicrogramsPerCubicMeter,
    Aqi,
    RadiationLevel,
    PulseCount
);

fn text<const N: usize>(value: &str) -> String<N> {
    let mut out = String::new();
    // Names are short constants; a longer value is cut, never panics.
    for c in value.chars() {
        if out.push(c).is_err() {
            break;
        }
    }
    out
}

pub(crate) fn device_info(info: &DeviceInfo) -> pb::GetInfoResponse {
    pb::GetInfoResponse {
        firmware: text(info.firmware),
        board: text(info.board),
    }
}

fn sensor_name(kind: SensorKind) -> &'static str {
    match kind {
        SensorKind::Bmx280 => "bmx280",
        SensorKind::Scd4x => "scd4x",
        SensorKind::Sds011 => "sds011",
        SensorKind::Microphone => "microphone",
        SensorKind::Zmod4510 => "zmod4510",
        SensorKind::Ags3871 => "ags3871",
        SensorKind::RadSens => "radsens",
    }
}

fn measurement_state(state: MeasurementState) -> pb::MeasurementState {
    match state {
        MeasurementState::Unavailable => pb::MeasurementState::Unavailable,
        MeasurementState::WarmingUp => pb::MeasurementState::WarmingUp,
        MeasurementState::Valid => pb::MeasurementState::Valid,
        MeasurementState::Stale => pb::MeasurementState::Stale,
        MeasurementState::Error => pb::MeasurementState::Error,
    }
}

fn sensor_state(state: SensorState) -> pb::SensorState {
    match state {
        SensorState::NotDetected => pb::SensorState::NotDetected,
        SensorState::Initializing => pb::SensorState::Initializing,
        SensorState::WarmingUp => pb::SensorState::WarmingUp,
        SensorState::Ready => pb::SensorState::Ready,
        SensorState::Degraded => pb::SensorState::Degraded,
        SensorState::Error => pb::SensorState::Error,
    }
}

fn sensor_error(code: SensorErrorCode) -> pb::SensorError {
    match code {
        SensorErrorCode::Timeout => pb::SensorError::Timeout,
        SensorErrorCode::Bus => pb::SensorError::Bus,
        SensorErrorCode::Crc => pb::SensorError::Crc,
        SensorErrorCode::InvalidData => pb::SensorError::InvalidData,
        SensorErrorCode::NotReady => pb::SensorError::NotReady,
        SensorErrorCode::Calibration => pb::SensorError::Calibration,
        SensorErrorCode::Unknown => pb::SensorError::Unknown,
    }
}

struct Builder {
    now: MonotonicTimestamp,
    measurements: heapless::Vec<pb::Measurement, { limits::MAX_MEASUREMENTS_PER_SENSOR }>,
}

impl Builder {
    fn add<T: Canonical>(&mut self, kind: pb::MeasurementType, measurement: &Measurement<T>) {
        let mut out = pb::Measurement {
            r#type: kind,
            state: measurement_state(measurement.state),
            ..Default::default()
        };
        if let Some(value) = measurement.value {
            out.set_value(value.canonical());
        }
        if let Some(at) = measurement.updated_at {
            out.set_age_ms(self.now.saturating_millis_since(at));
        }
        // At most four measurements per sensor, see the bound check below.
        let _ = self.measurements.push(out);
    }
}

fn sensor(
    snapshot: &MeasurementsSnapshot,
    kind: SensorKind,
    fill: impl FnOnce(&mut Builder),
) -> pb::Sensor {
    let mut builder = Builder {
        now: snapshot.generated_at,
        measurements: heapless::Vec::new(),
    };
    fill(&mut builder);
    let status = snapshot.sensor_statuses.get(kind);
    let mut out = pb::Sensor {
        name: text(sensor_name(kind)),
        state: sensor_state(status.state),
        measurements: builder.measurements,
        ..Default::default()
    };
    if let Some(code) = status.last_error {
        out.set_error(sensor_error(code));
    }
    out
}

/// Generic sensor list of a typed snapshot. Only present sensors appear.
pub(crate) fn sensors(snapshot: &MeasurementsSnapshot) -> pb::GetSensorsResponse {
    use pb::MeasurementType as T;

    let m = &snapshot.measurements;
    let mut out = pb::GetSensorsResponse {
        uptime_ms: snapshot.generated_at.millis,
        ..Default::default()
    };
    let mut push = |sensor: pb::Sensor| {
        // One entry per `SensorKind`, below the bound checked above.
        let _ = out.sensors.push(sensor);
    };

    if let Some(g) = &m.bmx280 {
        push(sensor(snapshot, SensorKind::Bmx280, |b| {
            b.add(T::Temperature, &g.temperature);
            if let Some(humidity) = &g.humidity {
                b.add(T::Humidity, humidity);
            }
            b.add(T::Pressure, &g.pressure);
        }));
    }
    if let Some(g) = &m.scd4x {
        push(sensor(snapshot, SensorKind::Scd4x, |b| {
            b.add(T::Co2, &g.co2);
            b.add(T::Temperature, &g.temperature);
            b.add(T::Humidity, &g.humidity);
        }));
    }
    if let Some(g) = &m.sds011 {
        push(sensor(snapshot, SensorKind::Sds011, |b| {
            b.add(T::Pm25, &g.pm25);
            b.add(T::Pm10, &g.pm10);
        }));
    }
    if let Some(g) = &m.microphone {
        push(sensor(snapshot, SensorKind::Microphone, |b| {
            b.add(T::NoiseAverage, &g.average);
            b.add(T::NoiseMaximum, &g.maximum);
        }));
    }
    if let Some(g) = &m.zmod4510 {
        push(sensor(snapshot, SensorKind::Zmod4510, |b| {
            b.add(T::O3, &g.o3);
            b.add(T::No2, &g.no2);
            b.add(T::AqiFast, &g.fast_aqi);
            b.add(T::AqiEpa, &g.epa_aqi);
        }));
    }
    if let Some(g) = &m.ags3871 {
        push(sensor(snapshot, SensorKind::Ags3871, |b| {
            b.add(T::Co, &g.co);
        }));
    }
    if let Some(g) = &m.radsens {
        push(sensor(snapshot, SensorKind::RadSens, |b| {
            b.add(T::Radiation, &g.radiation);
            b.add(T::PulseCount, &g.pulses);
        }));
    }
    out
}

pub(crate) fn wifi_config(config: &WifiConfig) -> pb::WifiConfig {
    let mut out = pb::WifiConfig::default();
    // Bounded by the same constant on both sides, see the checks above.
    let _ = out.ssid.extend_from_slice(config.ssid.as_bytes());
    out.password_configured = config.password.is_some();
    out
}

fn wifi_security(security: WifiSecurity) -> pb::WifiSecurity {
    match security {
        WifiSecurity::Open => pb::WifiSecurity::Open,
        WifiSecurity::Wep => pb::WifiSecurity::Wep,
        WifiSecurity::Wpa => pb::WifiSecurity::Wpa,
        WifiSecurity::Wpa2 => pb::WifiSecurity::Wpa2,
        WifiSecurity::WpaWpa2 => pb::WifiSecurity::WpaWpa2,
        WifiSecurity::Wpa3 => pb::WifiSecurity::Wpa3,
        WifiSecurity::Wpa2Wpa3 => pb::WifiSecurity::Wpa2Wpa3,
        WifiSecurity::Enterprise => pb::WifiSecurity::Enterprise,
        WifiSecurity::Unknown => pb::WifiSecurity::Unknown,
    }
}

fn wifi_network(network: &WifiNetwork) -> pb::WifiNetwork {
    let mut out = pb::WifiNetwork {
        rssi_dbm: i32::from(network.rssi_dbm),
        channel: u32::from(network.channel),
        security: wifi_security(network.security),
        ..Default::default()
    };
    let _ = out.ssid.extend_from_slice(network.ssid.as_bytes());
    out
}

pub(crate) fn wifi_scan(results: &WifiScanResults) -> pb::ScanWifiResponse {
    let mut out = pb::ScanWifiResponse::default();
    for network in results.iter() {
        // Same bound on both sides.
        let _ = out.networks.push(wifi_network(network));
    }
    out
}

fn wifi_state(state: WifiState) -> pb::WifiState {
    match state {
        WifiState::NotConfigured => pb::WifiState::NotConfigured,
        WifiState::Idle => pb::WifiState::Idle,
        WifiState::Scanning => pb::WifiState::Scanning,
        WifiState::Connecting => pb::WifiState::Connecting,
        WifiState::Connected => pb::WifiState::Connected,
        WifiState::Disconnected => pb::WifiState::Disconnected,
        WifiState::Error => pb::WifiState::Error,
    }
}

fn wifi_failure(error: WifiError) -> pb::WifiFailure {
    match error {
        WifiError::NotConfigured => pb::WifiFailure::NotConfigured,
        WifiError::Busy => pb::WifiFailure::Busy,
        WifiError::Timeout => pb::WifiFailure::Timeout,
        WifiError::ScanFailed => pb::WifiFailure::ScanFailed,
        WifiError::NetworkNotFound => pb::WifiFailure::NetworkNotFound,
        WifiError::AuthenticationFailed => pb::WifiFailure::AuthenticationFailed,
        WifiError::AssociationFailed => pb::WifiFailure::AssociationFailed,
        WifiError::DhcpFailed => pb::WifiFailure::DhcpFailed,
        WifiError::Disconnected => pb::WifiFailure::Disconnected,
        WifiError::InvalidConfig => pb::WifiFailure::InvalidConfig,
        WifiError::DriverError => pb::WifiFailure::DriverError,
    }
}

fn connection(info: &NetworkInfo) -> pb::WifiConnection {
    let mut out = pb::WifiConnection {
        rssi_dbm: i32::from(info.rssi_dbm),
        ipv4: info.ipv4.to_bits(),
        ..Default::default()
    };
    let _ = out.ssid.extend_from_slice(info.ssid.as_bytes());
    if let Some(gateway) = info.gateway {
        out.set_gateway(gateway.to_bits());
    }
    if let Some(dns) = info.dns {
        out.set_dns(dns.to_bits());
    }
    out
}

pub(crate) fn wifi_status(
    status: &WifiStatus,
    info: Option<&NetworkInfo>,
) -> pb::GetWifiStatusResponse {
    let mut out = pb::GetWifiStatusResponse {
        state: wifi_state(status.state),
        ..Default::default()
    };
    if let Some(error) = status.error {
        out.set_failure(wifi_failure(error));
    }
    if let Some(info) = info {
        out.set_connection(connection(info));
    }
    out
}
