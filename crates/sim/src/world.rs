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

//! The simulated world: what the native backends read and the management
//! interface and the scenario change.
//!
//! Everything that is *simulation* lives here and nowhere else: sensor values
//! and faults, the Wi-Fi access points, IP faults. The device runtime never
//! sees this type; it only sees the backends built on it (sensor drivers,
//! `SimWifi`, `SimIp`), so no simulation-specific path leaks into
//! application logic.

use std::sync::{Mutex, MutexGuard, OnceLock};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use rohi_net::WifiSecurity;

use crate::command::{ApAction, Command, Field, IpFault, Sensor, SensorFault, WifiFault};
use crate::scenario::Scenario;

/// The simulated physical quantities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Values {
    pub temperature: f32,
    pub humidity: f32,
    /// hPa.
    pub pressure: f32,
    pub co2: f32,
    pub pm25: f32,
    pub pm10: f32,
    pub noise_average: f32,
    pub noise_maximum: f32,
}

impl Default for Values {
    fn default() -> Self {
        Self {
            temperature: 22.5,
            humidity: 45.0,
            pressure: 1013.0,
            co2: 600.0,
            pm25: 8.0,
            pm10: 12.0,
            noise_average: 42.0,
            noise_maximum: 55.0,
        }
    }
}

/// One simulated sensor.
#[derive(Clone, Copy, Debug)]
pub struct SensorState {
    pub fault: Option<SensorFault>,
    /// No data before this uptime, in ms: the warm-up of the sensor.
    pub warm_until_ms: u64,
    pub interval_ms: u32,
}

/// One simulated access point.
#[derive(Clone, Debug)]
pub struct AccessPoint {
    pub ssid: String,
    pub rssi: i8,
    pub security: WifiSecurity,
    pub channel: u8,
    /// `None` accepts every non-empty password.
    pub password: Option<String>,
    pub available: bool,
}

/// The simulated Wi-Fi environment.
#[derive(Clone, Debug)]
pub struct WifiWorld {
    pub aps: Vec<AccessPoint>,
    pub network_up: bool,
    /// The station is associated and has an address.
    pub link_up: bool,
    pub fault: Option<WifiFault>,
    pub connect_delay_ms: u32,
    pub flap: Option<(u32, u32)>,
    /// The link is refused until this uptime, in ms (a flap in its down phase).
    pub blocked_until_ms: u64,
    /// The established link must be lost once.
    pub drop_requested: bool,
}

/// The simulated IP environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct IpWorld {
    pub fault: Option<IpFault>,
    pub latency_ms: u32,
    pub drop_next: u32,
}

pub struct State {
    pub values: Values,
    pub sensors: [SensorState; 4],
    pub wifi: WifiWorld,
    pub ip: IpWorld,
    pub seed: Option<u64>,
    pub jitter: f32,
    pub device_name: String,
}

/// The shared world.
pub struct World {
    state: Mutex<State>,
    /// Raised when the world changed in a way an established Wi-Fi link must
    /// re-check.
    pub wifi_changed: Signal<CriticalSectionRawMutex, ()>,
}

static WORLD: OnceLock<World> = OnceLock::new();

/// Creates the world of `scenario`. Call once, before anything uses [`world`].
pub fn init(scenario: &Scenario, seed: Option<u64>, device_name: String) -> &'static World {
    WORLD.get_or_init(|| World::new(scenario, seed, device_name))
}

/// Creates the world for unit tests, once per process.
#[cfg(test)]
pub fn init_for_tests(scenario: &Scenario) -> &'static World {
    init(scenario, Some(1), "test".to_owned())
}

/// The world.
///
/// # Panics
///
/// If [`init`] was not called; the composition root does it first.
pub fn world() -> &'static World {
    WORLD
        .get()
        .expect("the simulated world is initialized at startup")
}

impl World {
    fn new(scenario: &Scenario, seed: Option<u64>, device_name: String) -> Self {
        let interval_ms = scenario.sensors.interval_ms;
        let mut sensors = [SensorState {
            fault: None,
            warm_until_ms: 0,
            interval_ms,
        }; 4];
        for (sensor, warm_up_ms) in &scenario.sensors.warm_up_ms {
            sensors[sensor.index()].warm_until_ms = *warm_up_ms;
        }
        Self {
            state: Mutex::new(State {
                values: scenario.sensors.values,
                sensors,
                wifi: WifiWorld {
                    aps: scenario.wifi.aps.clone(),
                    network_up: true,
                    link_up: false,
                    fault: None,
                    connect_delay_ms: scenario.wifi.connect_delay_ms,
                    flap: None,
                    blocked_until_ms: 0,
                    drop_requested: false,
                },
                ip: IpWorld::default(),
                seed,
                jitter: scenario.sensors.jitter,
                device_name,
            }),
            wifi_changed: Signal::new(),
        }
    }

    /// Locks the state. Never held across an `.await`.
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Applies the world part of `command`; `Ok(true)` if it was one.
    /// Storage commands, `status` and `shutdown` belong to the callers that
    /// own that state.
    pub fn apply(&self, command: &Command) -> bool {
        let mut state = self.lock();
        match command {
            Command::Set(field, value) => {
                let values = &mut state.values;
                match field {
                    Field::Temperature => values.temperature = *value,
                    Field::Humidity => values.humidity = *value,
                    Field::Pressure => values.pressure = *value,
                    Field::Co2 => values.co2 = *value,
                    Field::Pm25 => values.pm25 = *value,
                    Field::Pm10 => values.pm10 = *value,
                    Field::NoiseAverage => values.noise_average = *value,
                    Field::NoiseMaximum => values.noise_maximum = *value,
                }
            }
            Command::Fail(sensor, fault) => state.sensors[sensor.index()].fault = Some(*fault),
            Command::Recover(sensor) => {
                let sensor = &mut state.sensors[sensor.index()];
                sensor.fault = None;
                sensor.warm_until_ms = 0;
            }
            Command::Network { up } => {
                state.wifi.network_up = *up;
                drop(state);
                self.wifi_changed.signal(());
                return true;
            }
            Command::Ap { ssid, action } => {
                for ap in state.wifi.aps.iter_mut().filter(|ap| &ap.ssid == ssid) {
                    match action {
                        ApAction::Up => ap.available = true,
                        ApAction::Down => ap.available = false,
                        ApAction::Rssi(rssi) => ap.rssi = *rssi,
                    }
                }
                drop(state);
                self.wifi_changed.signal(());
                return true;
            }
            Command::WifiFail(fault) => state.wifi.fault = Some(*fault),
            Command::WifiRecover => state.wifi.fault = None,
            Command::WifiDelay { ms } => state.wifi.connect_delay_ms = *ms,
            Command::WifiDrop => {
                state.wifi.drop_requested = true;
                drop(state);
                self.wifi_changed.signal(());
                return true;
            }
            Command::WifiFlap(flap) => {
                state.wifi.flap = *flap;
                drop(state);
                self.wifi_changed.signal(());
                return true;
            }
            Command::IpFail(fault) => state.ip.fault = Some(*fault),
            Command::IpRecover => state.ip = IpWorld::default(),
            Command::IpLatency { ms } => state.ip.latency_ms = *ms,
            Command::IpDropNext => state.ip.drop_next += 1,
            Command::Storage(_) | Command::Status | Command::Measurements | Command::Shutdown => {
                return false;
            }
        }
        true
    }

    /// One line per sensor and a Wi-Fi/IP summary, for `status`.
    pub fn summary(&self) -> String {
        let state = self.lock();
        let mut out = String::new();
        for sensor in Sensor::ALL {
            let sensor_state = &state.sensors[sensor.index()];
            out.push_str(&format!(
                "sensor {} fault={:?} interval={}ms\n",
                sensor.name(),
                sensor_state.fault,
                sensor_state.interval_ms
            ));
        }
        out.push_str(&format!("values {:?}\n", state.values));
        out.push_str(&format!(
            "wifi up={} fault={:?} delay={}ms flap={:?}\n",
            state.wifi.network_up, state.wifi.fault, state.wifi.connect_delay_ms, state.wifi.flap
        ));
        for ap in &state.wifi.aps {
            out.push_str(&format!(
                "ap {} rssi={} security={:?} available={}\n",
                ap.ssid, ap.rssi, ap.security, ap.available
            ));
        }
        out.push_str(&format!("ip {:?}\n", state.ip));
        out
    }
}
