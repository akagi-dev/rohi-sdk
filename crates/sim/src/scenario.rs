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
///////////////////////////////////////////////////////////////////////////////

//! Scenario files: the human-editable description of a simulated device.
//!
//! A scenario is consumed only by the simulation and is separate from the
//! persistent device configuration. It describes the initial sensor values,
//! the Wi-Fi networks and a timeline of [`Command`]s.
//!
//! ```toml
//! [device]
//! name = "urban-sim"
//!
//! [sensors]
//! temperature = 22.5      # °C
//! humidity = 45.0         # %
//! pressure = 1013.0       # hPa
//! co2 = 600               # ppm
//! pm25 = 8                # µg/m³
//! pm10 = 12
//! noise = 42              # dB
//! interval = "1s"         # time between measurements of every sensor
//! jitter = 0.0            # relative noise on the values, seeded by --seed
//! warm_up = { scd4x = "5s" }   # no data before this uptime
//!
//! [freshness]
//! stale_after = "10s"     # measurements older than this are stale
//!
//! [wifi]
//! connect_delay = "0s"
//! networks = ["ROHI-LAB", { ssid = "Office", rssi = -60, security = "wpa2", password = "secret" }]
//!
//! [[events]]
//! at = "30s"              # uptime of the device
//! action = "set"          # any command of the management interface:
//! sensor = "co2"          # the fields are joined in the order
//! value = 1200            # action, sensor, ssid, target, kind, value
//!
//! [[events]]
//! at = "1m"
//! command = "wifi flap 4000 2000"   # or the whole command line
//! ```

use std::fmt;
use std::path::Path;

use rohi_net::WifiSecurity;
use serde::Deserialize;

use crate::command::{Command, ParseError, Sensor};
use crate::world::{AccessPoint, Values};

/// A command that happens at a given uptime.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedCommand {
    pub at_ms: u64,
    pub command: Command,
}

#[derive(Clone, Debug)]
pub struct SensorsScenario {
    pub values: Values,
    pub interval_ms: u32,
    pub jitter: f32,
    pub warm_up_ms: Vec<(Sensor, u64)>,
}

#[derive(Clone, Debug)]
pub struct WifiScenario {
    pub aps: Vec<AccessPoint>,
    pub connect_delay_ms: u32,
}

/// A parsed scenario.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub device_name: Option<String>,
    pub sensors: SensorsScenario,
    pub stale_after_ms: Option<u64>,
    pub wifi: WifiScenario,
    pub events: Vec<TimedCommand>,
}

/// A scenario that cannot be used.
#[derive(Debug)]
pub struct ScenarioError(String);

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScenarioError {}

fn error(message: impl Into<String>) -> ScenarioError {
    ScenarioError(message.into())
}

impl Default for Scenario {
    fn default() -> Self {
        Self::from_file(File::default()).expect("the default scenario is valid")
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    device: DeviceSection,
    #[serde(default)]
    sensors: SensorsSection,
    #[serde(default)]
    freshness: FreshnessSection,
    #[serde(default)]
    wifi: WifiSection,
    #[serde(default)]
    events: Vec<EventSection>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DeviceSection {
    name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SensorsSection {
    temperature: Option<f32>,
    humidity: Option<f32>,
    pressure: Option<f32>,
    co2: Option<f32>,
    pm25: Option<f32>,
    pm10: Option<f32>,
    noise: Option<f32>,
    noise_max: Option<f32>,
    interval: Option<String>,
    jitter: Option<f32>,
    #[serde(default)]
    warm_up: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FreshnessSection {
    stale_after: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct WifiSection {
    connect_delay: Option<String>,
    networks: Option<Vec<NetworkSpec>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NetworkSpec {
    Name(String),
    Detail(NetworkDetail),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkDetail {
    ssid: String,
    rssi: Option<i8>,
    security: Option<String>,
    channel: Option<u8>,
    password: Option<String>,
    available: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventSection {
    at: String,
    /// A whole command line; replaces the fields below.
    command: Option<String>,
    action: Option<String>,
    sensor: Option<String>,
    ssid: Option<String>,
    target: Option<String>,
    kind: Option<String>,
    value: Option<toml::Value>,
}

/// `"500ms"`, `"30s"`, `"2m"` or `"1h"` in milliseconds.
pub fn parse_duration_ms(text: &str) -> Result<u64, ScenarioError> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .ok_or_else(|| error(format!("duration `{text}` needs a unit: ms, s, m or h")))?;
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| error(format!("`{text}` is not a valid duration")))?;
    let factor = match unit {
        "ms" => 1.0,
        "s" => 1_000.0,
        "m" => 60_000.0,
        "h" => 3_600_000.0,
        _ => return Err(error(format!("unknown duration unit in `{text}`"))),
    };
    Ok((number * factor).round() as u64)
}

fn security(text: &str) -> Result<WifiSecurity, ScenarioError> {
    match text.to_ascii_lowercase().as_str() {
        "open" | "none" => Ok(WifiSecurity::Open),
        "wep" => Ok(WifiSecurity::Wep),
        "wpa" => Ok(WifiSecurity::Wpa),
        "wpa2" => Ok(WifiSecurity::Wpa2),
        "wpa-wpa2" => Ok(WifiSecurity::WpaWpa2),
        "wpa3" => Ok(WifiSecurity::Wpa3),
        "wpa2-wpa3" => Ok(WifiSecurity::Wpa2Wpa3),
        other => Err(error(format!("unknown security `{other}`"))),
    }
}

fn default_networks() -> Vec<AccessPoint> {
    let ap = |ssid: &str, rssi, security, channel| AccessPoint {
        ssid: ssid.to_owned(),
        rssi,
        security,
        channel,
        password: None,
        available: true,
    };
    vec![
        ap("ROHI-LAB", -45, WifiSecurity::Wpa2, 6),
        ap("Office", -62, WifiSecurity::Wpa2, 11),
        ap("Guest", -70, WifiSecurity::Open, 1),
    ]
}

fn network(spec: NetworkSpec) -> Result<AccessPoint, ScenarioError> {
    match spec {
        NetworkSpec::Name(ssid) => Ok(default_networks()
            .into_iter()
            .find(|ap| ap.ssid == ssid)
            .unwrap_or(AccessPoint {
                ssid,
                rssi: -55,
                security: WifiSecurity::Wpa2,
                channel: 6,
                password: None,
                available: true,
            })),
        NetworkSpec::Detail(detail) => Ok(AccessPoint {
            rssi: detail.rssi.unwrap_or(-55),
            security: detail
                .security
                .as_deref()
                .map(security)
                .transpose()?
                .unwrap_or(WifiSecurity::Wpa2),
            channel: detail.channel.unwrap_or(6),
            password: detail.password,
            available: detail.available.unwrap_or(true),
            ssid: detail.ssid,
        }),
    }
}

fn value_text(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn event(index: usize, section: EventSection) -> Result<TimedCommand, ScenarioError> {
    let at_ms =
        parse_duration_ms(&section.at).map_err(|e| error(format!("events[{index}].at: {e}")))?;
    let line = match (section.command, section.action) {
        (Some(_), Some(_)) => {
            return Err(error(format!(
                "events[{index}]: use either `command` or `action`, not both"
            )));
        }
        (Some(command), None) => command,
        (None, Some(action)) => {
            let mut words = vec![action];
            words.extend(section.sensor);
            words.extend(section.ssid);
            words.extend(section.target);
            words.extend(section.kind);
            words.extend(section.value.as_ref().map(value_text));
            words.join(" ")
        }
        (None, None) => {
            return Err(error(format!(
                "events[{index}]: needs a `command` or an `action`"
            )));
        }
    };
    let command =
        Command::parse(&line).map_err(|e: ParseError| error(format!("events[{index}]: {e}")))?;
    Ok(TimedCommand { at_ms, command })
}

impl Scenario {
    /// Reads and checks the scenario at `path`.
    ///
    /// # Errors
    ///
    /// An unreadable file, a syntax error or an unknown name.
    pub fn load(path: &Path) -> Result<Self, ScenarioError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| error(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&text).map_err(|e| error(format!("{}: {e}", path.display())))
    }

    /// Parses scenario text.
    ///
    /// # Errors
    ///
    /// A syntax error or an unknown name.
    pub fn parse(text: &str) -> Result<Self, ScenarioError> {
        let file: File = toml::from_str(text).map_err(|e| error(e.to_string()))?;
        Self::from_file(file)
    }

    fn from_file(file: File) -> Result<Self, ScenarioError> {
        let defaults = Values::default();
        let sensors = file.sensors;
        let values = Values {
            temperature: sensors.temperature.unwrap_or(defaults.temperature),
            humidity: sensors.humidity.unwrap_or(defaults.humidity),
            pressure: sensors.pressure.unwrap_or(defaults.pressure),
            co2: sensors.co2.unwrap_or(defaults.co2),
            pm25: sensors.pm25.unwrap_or(defaults.pm25),
            pm10: sensors.pm10.unwrap_or(defaults.pm10),
            noise_average: sensors.noise.unwrap_or(defaults.noise_average),
            noise_maximum: sensors
                .noise_max
                .or(sensors.noise.map(|noise| noise + 13.0))
                .unwrap_or(defaults.noise_maximum),
        };
        let interval_ms = match &sensors.interval {
            Some(text) => u32::try_from(parse_duration_ms(text)?)
                .map_err(|_| error("sensors.interval is too long"))?
                .max(1),
            None => 1_000,
        };
        let mut warm_up_ms = Vec::new();
        for (name, duration) in &sensors.warm_up {
            let sensor = Sensor::ALL
                .into_iter()
                .find(|sensor| sensor.name() == name)
                .ok_or_else(|| error(format!("sensors.warm_up: unknown sensor `{name}`")))?;
            warm_up_ms.push((sensor, parse_duration_ms(duration)?));
        }
        let aps = match file.wifi.networks {
            Some(networks) => networks
                .into_iter()
                .map(network)
                .collect::<Result<Vec<_>, _>>()?,
            None => default_networks(),
        };
        let events = file
            .events
            .into_iter()
            .enumerate()
            .map(|(index, section)| event(index, section))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            device_name: file.device.name,
            sensors: SensorsScenario {
                values,
                interval_ms,
                jitter: sensors.jitter.unwrap_or(0.0).clamp(0.0, 1.0),
                warm_up_ms,
            },
            stale_after_ms: file
                .freshness
                .stale_after
                .as_deref()
                .map(parse_duration_ms)
                .transpose()?,
            wifi: WifiScenario {
                aps,
                connect_delay_ms: match &file.wifi.connect_delay {
                    Some(text) => u32::try_from(parse_duration_ms(text)?)
                        .map_err(|_| error("wifi.connect_delay is too long"))?,
                    None => 0,
                },
            },
            events,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Field, SensorFault};

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms("500ms").unwrap(), 500);
        assert_eq!(parse_duration_ms("30s").unwrap(), 30_000);
        assert_eq!(parse_duration_ms("1.5m").unwrap(), 90_000);
        assert!(parse_duration_ms("30").is_err());
        assert!(parse_duration_ms("3 parsecs").is_err());
    }

    #[test]
    fn the_default_scenario_has_the_documented_device() {
        let scenario = Scenario::default();
        assert_eq!(scenario.sensors.values.co2, 600.0);
        assert_eq!(scenario.sensors.values.pressure, 1013.0);
        let names: Vec<_> = scenario
            .wifi
            .aps
            .iter()
            .map(|ap| ap.ssid.as_str())
            .collect();
        assert_eq!(names, ["ROHI-LAB", "Office", "Guest"]);
    }

    #[test]
    fn events_become_commands() {
        let scenario = Scenario::parse(
            r#"
            [sensors]
            co2 = 700
            warm_up = { scd4x = "5s" }

            [wifi]
            networks = ["ROHI-LAB", { ssid = "Cafe", security = "open", rssi = -80 }]

            [[events]]
            at = "30s"
            action = "set"
            sensor = "co2"
            value = 1200

            [[events]]
            at = "1m"
            action = "fail"
            sensor = "sds011"
            kind = "timeout"
            "#,
        )
        .unwrap();
        assert_eq!(scenario.sensors.values.co2, 700.0);
        assert_eq!(scenario.sensors.warm_up_ms, [(Sensor::Scd4x, 5_000)]);
        assert_eq!(scenario.wifi.aps.len(), 2);
        assert_eq!(scenario.wifi.aps[1].security, WifiSecurity::Open);
        assert_eq!(
            scenario.events,
            [
                TimedCommand {
                    at_ms: 30_000,
                    command: Command::Set(Field::Co2, 1200.0)
                },
                TimedCommand {
                    at_ms: 60_000,
                    command: Command::Fail(Sensor::Sds011, SensorFault::Timeout)
                },
            ]
        );
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(Scenario::parse("[sensors]\nco3 = 1").is_err());
        assert!(Scenario::parse("[[events]]\nat = \"1s\"\naction = \"explode\"").is_err());
        assert!(Scenario::parse("[[events]]\nat = \"soon\"\naction = \"status\"").is_err());
    }
}

#[cfg(test)]
mod checked_in {
    use super::*;

    #[test]
    fn every_checked_in_scenario_is_valid() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let scenario = Scenario::load(&path).unwrap_or_else(|e| panic!("{e}"));
            assert!(!scenario.events.is_empty(), "{}", path.display());
            count += 1;
        }
        assert!(count >= 3);
    }
}
