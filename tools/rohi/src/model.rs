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
//! Plain Rust model of the control protocol.
//!
//! These types are what applications use. They hold no protobuf types, spell
//! out the canonical unit of every measurement and never contain a Wi-Fi
//! password. With the `serde` feature they serialize to JSON.

use core::fmt;
use std::net::Ipv4Addr;

#[cfg(feature = "serde")]
use serde::Serialize;

macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        $name:ident: $proto:path {
            $($(#[$vmeta:meta])* $variant:ident = $pb:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[cfg_attr(feature = "serde", derive(Serialize))]
        #[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
            /// A value this version of the library does not know.
            Unrecognized,
        }

        impl $name {
            pub(crate) fn from_proto(value: $proto) -> Self {
                $(if value == <$proto>::$pb { return Self::$variant; })+
                Self::Unrecognized
            }

            /// Stable lower-case name.
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                    Self::Unrecognized => "unrecognized",
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

use rohi_control_proto::control as pb;

open_enum! {
    /// Typed error of the control protocol.
    ErrorCode: pb::ErrorCode {
        InvalidArgument = InvalidArgument => "invalid argument",
        NotFound = NotFound => "not found",
        NotSupported = NotSupported => "not supported",
        Busy = Busy => "busy",
        Unavailable = Unavailable => "unavailable",
        PermissionDenied = PermissionDenied => "permission denied",
        Internal = Internal => "internal error",
    }
}

open_enum! {
    /// Feature of a device.
    Capability: pb::Capability {
        Sensors = Sensors => "sensors",
        Config = Config => "config",
        WifiScan = WifiScan => "wifi scan",
        WifiStatus = WifiStatus => "wifi status",
        WifiControl = WifiControl => "wifi control",
    }
}

open_enum! {
    /// Physical quantity of a measurement; the canonical unit is fixed per type.
    MeasurementType: pb::MeasurementType {
        Temperature = Temperature => "temperature",
        Humidity = Humidity => "humidity",
        Pressure = Pressure => "pressure",
        Co2 = Co2 => "co2",
        Pm25 = Pm25 => "pm2.5",
        Pm10 = Pm10 => "pm10",
        NoiseAverage = NoiseAverage => "noise average",
        NoiseMaximum = NoiseMaximum => "noise maximum",
        O3 = O3 => "o3",
        No2 = No2 => "no2",
        AqiFast = AqiFast => "aqi fast",
        AqiEpa = AqiEpa => "aqi epa",
        Co = Co => "co",
        Radiation = Radiation => "radiation",
        PulseCount = PulseCount => "pulse count",
    }
}

impl MeasurementType {
    /// Canonical unit symbol.
    pub const fn unit(&self) -> &'static str {
        match self {
            Self::Temperature => "°C",
            Self::Humidity => "%",
            Self::Pressure => "Pa",
            Self::Co2 | Self::Co => "ppm",
            Self::Pm25 | Self::Pm10 => "µg/m³",
            Self::NoiseAverage | Self::NoiseMaximum => "dB",
            Self::O3 | Self::No2 => "ppb",
            Self::AqiFast | Self::AqiEpa => "AQI",
            Self::Radiation => "µSv/h",
            Self::PulseCount => "pulses",
            Self::Unrecognized => "",
        }
    }
}

open_enum! {
    /// State of one measurement.
    MeasurementState: pb::MeasurementState {
        Unavailable = Unavailable => "unavailable",
        WarmingUp = WarmingUp => "warming up",
        Valid = Valid => "valid",
        Stale = Stale => "stale",
        Error = Error => "error",
    }
}

open_enum! {
    /// Runtime state of a sensor.
    SensorState: pb::SensorState {
        NotDetected = NotDetected => "not detected",
        Initializing = Initializing => "initializing",
        WarmingUp = WarmingUp => "warming up",
        Ready = Ready => "ready",
        Degraded = Degraded => "degraded",
        Error = Error => "error",
    }
}

open_enum! {
    /// Last error of a sensor.
    SensorError: pb::SensorError {
        Timeout = Timeout => "timeout",
        Bus = Bus => "bus error",
        Crc = Crc => "crc mismatch",
        InvalidData = InvalidData => "invalid data",
        NotReady = NotReady => "not ready",
        Calibration = Calibration => "calibration",
        Unknown = Unknown => "unknown error",
    }
}

open_enum! {
    /// Authentication mode of a scanned network.
    WifiSecurity: pb::WifiSecurity {
        Open = Open => "open",
        Wep = Wep => "wep",
        Wpa = Wpa => "wpa",
        Wpa2 = Wpa2 => "wpa2",
        WpaWpa2 = WpaWpa2 => "wpa/wpa2",
        Wpa3 = Wpa3 => "wpa3",
        Wpa2Wpa3 = Wpa2Wpa3 => "wpa2/wpa3",
        Enterprise = Enterprise => "enterprise",
    }
}

open_enum! {
    /// Wi-Fi runtime state.
    WifiState: pb::WifiState {
        NotConfigured = NotConfigured => "not configured",
        Idle = Idle => "idle",
        Scanning = Scanning => "scanning",
        Connecting = Connecting => "connecting",
        Connected = Connected => "connected",
        Disconnected = Disconnected => "disconnected",
        Error = Error => "error",
    }
}

open_enum! {
    /// Why the last Wi-Fi attempt failed.
    WifiFailure: pb::WifiFailure {
        NotConfigured = NotConfigured => "not configured",
        Busy = Busy => "busy",
        Timeout = Timeout => "timeout",
        ScanFailed = ScanFailed => "scan failed",
        NetworkNotFound = NetworkNotFound => "network not found",
        AuthenticationFailed = AuthenticationFailed => "authentication failed",
        AssociationFailed = AssociationFailed => "association failed",
        DhcpFailed = DhcpFailed => "dhcp failed",
        Disconnected = Disconnected => "disconnected",
        InvalidConfig = InvalidConfig => "invalid configuration",
        DriverError = DriverError => "driver error",
    }
}

/// Static description of the device.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct DeviceInfo {
    /// Firmware name.
    pub firmware: String,
    /// Firmware version.
    pub firmware_version: String,
    /// Board.
    pub board: String,
    /// Chip.
    pub chip: String,
}

/// One measurement in the canonical unit of its type.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Measurement {
    /// Quantity.
    #[cfg_attr(feature = "serde", serde(rename = "type"))]
    pub kind: MeasurementType,
    /// Canonical unit of [`Measurement::kind`].
    pub unit: &'static str,
    /// Last valid value; absent when never measured.
    pub value: Option<f64>,
    /// State.
    pub state: MeasurementState,
    /// Age of the value when the device answered, in milliseconds.
    pub age_ms: Option<u64>,
}

/// A sensor and its measurements.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Sensor {
    /// Identifies the physical sensor.
    pub name: String,
    /// Runtime state.
    pub state: SensorState,
    /// Last error.
    pub error: Option<SensorError>,
    /// Measurements.
    pub measurements: Vec<Measurement>,
}

/// Snapshot of all sensors.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Sensors {
    /// Device uptime when the snapshot was generated, in milliseconds.
    pub uptime_ms: u64,
    /// Present sensors.
    pub sensors: Vec<Sensor>,
}

/// Wi-Fi network name: arbitrary bytes, usually UTF-8.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Ssid(pub Vec<u8>);

impl Ssid {
    /// Text of the SSID, lossy for non UTF-8 names.
    pub fn to_text(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

impl fmt::Debug for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_text())
    }
}

impl fmt::Display for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

#[cfg(feature = "serde")]
impl Serialize for Ssid {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_text())
    }
}

/// Stored Wi-Fi configuration. The password is never available.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct WifiConfig {
    /// Configured network.
    pub ssid: Ssid,
    /// Whether a password is stored.
    pub password_configured: bool,
}

/// Device configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct Config {
    /// Wi-Fi, `None` when not configured.
    pub wifi: Option<WifiConfig>,
}

/// A network found by a Wi-Fi scan.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct WifiNetwork {
    /// Network name.
    pub ssid: Ssid,
    /// Signal strength in dBm.
    pub rssi_dbm: i32,
    /// Channel.
    pub channel: u32,
    /// Authentication mode.
    pub security: WifiSecurity,
}

/// Details of an established Wi-Fi connection.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct WifiConnection {
    /// Network name.
    pub ssid: Ssid,
    /// Signal strength in dBm.
    pub rssi_dbm: i32,
    /// IPv4 address.
    pub ipv4: Ipv4Addr,
    /// Gateway.
    pub gateway: Option<Ipv4Addr>,
    /// DNS server.
    pub dns: Option<Ipv4Addr>,
}

/// Wi-Fi status of the device.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct WifiStatus {
    /// Runtime state.
    pub state: WifiState,
    /// Error of the last failed attempt.
    pub failure: Option<WifiFailure>,
    /// Connection details while connected.
    pub connection: Option<WifiConnection>,
}

/// Change of the Wi-Fi password in a [`WifiUpdate`].
#[derive(Clone, PartialEq, Eq, Default)]
pub enum PasswordChange {
    /// Keep the stored password.
    #[default]
    Keep,
    /// Replace the password.
    Set(Vec<u8>),
    /// Remove the password (open network).
    Clear,
}

impl fmt::Debug for PasswordChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Keep => "Keep",
            Self::Set(_) => "Set(<redacted>)",
            Self::Clear => "Clear",
        })
    }
}

/// Partial update of the Wi-Fi configuration: omitted fields are preserved.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WifiUpdate {
    /// New SSID; required while nothing is stored.
    pub ssid: Option<Vec<u8>>,
    /// Password change.
    pub password: PasswordChange,
}
