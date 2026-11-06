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

//! Simulator commands.
//!
//! One small text language serves the management interface (one command per
//! line) and the events of a scenario file. It is the only control surface of
//! the simulation itself and is deliberately not part of the ROHI control
//! protocol: ordinary device operations go through `rohi-control`.
//!
//! ```text
//! set co2 1200                  fail scd4x [unavailable|init|read|timeout|nodata]
//! recover scd4x                 network down | network up
//! ap ROHI-LAB down              ap ROHI-LAB up | ap ROHI-LAB rssi -70
//! wifi fail association|authentication|dhcp
//! wifi recover                  wifi delay 2000
//! wifi drop                     wifi flap 5000 3000 | wifi flap off
//! ip fail offline|dns|connect|reset      ip recover
//! ip latency 250                ip drop-next
//! storage power-loss program|erase 64    storage corrupt crc|commit
//! status                        measurements
//! shutdown
//! ```

use std::fmt;

/// A simulated sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensor {
    Bmx280,
    Scd4x,
    Sds011,
    Noise,
}

impl Sensor {
    pub const ALL: [Self; 4] = [Self::Bmx280, Self::Scd4x, Self::Sds011, Self::Noise];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bmx280 => "bmx280",
            Self::Scd4x => "scd4x",
            Self::Sds011 => "sds011",
            Self::Noise => "noise",
        }
    }

    fn parse(text: &str) -> Result<Self, ParseError> {
        match text {
            "bmx280" | "bme280" | "bmp280" => Ok(Self::Bmx280),
            "scd4x" | "scd40" | "scd41" => Ok(Self::Scd4x),
            "sds011" => Ok(Self::Sds011),
            "noise" | "microphone" => Ok(Self::Noise),
            _ => Err(ParseError::new(format!(
                "unknown sensor `{text}`, expected bmx280, scd4x, sds011 or noise"
            ))),
        }
    }
}

/// A value that can be set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Temperature,
    Humidity,
    /// In hPa.
    Pressure,
    Co2,
    Pm25,
    Pm10,
    NoiseAverage,
    NoiseMaximum,
}

impl Field {
    fn parse(text: &str) -> Result<Self, ParseError> {
        match text {
            "temperature" | "temp" => Ok(Self::Temperature),
            "humidity" => Ok(Self::Humidity),
            "pressure" => Ok(Self::Pressure),
            "co2" => Ok(Self::Co2),
            "pm25" | "pm2.5" => Ok(Self::Pm25),
            "pm10" => Ok(Self::Pm10),
            "noise" | "noise-average" => Ok(Self::NoiseAverage),
            "noise-max" | "noise-maximum" => Ok(Self::NoiseMaximum),
            _ => Err(ParseError::new(format!(
                "unknown value `{text}`, expected temperature, humidity, pressure, co2, pm25, pm10, noise or noise-max"
            ))),
        }
    }
}

/// How a sensor fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorFault {
    /// The sensor does not answer on the bus: not detected at start, bus
    /// errors while running.
    Unavailable,
    /// Initialization fails.
    Init,
    /// Every read fails.
    Read,
    /// Every read times out.
    Timeout,
    /// The sensor answers but has no data: warm-up, or a stale measurement.
    NoData,
}

impl SensorFault {
    fn parse(text: &str) -> Result<Self, ParseError> {
        match text {
            "unavailable" => Ok(Self::Unavailable),
            "init" => Ok(Self::Init),
            "read" => Ok(Self::Read),
            "timeout" => Ok(Self::Timeout),
            "nodata" | "warmup" | "stale" => Ok(Self::NoData),
            _ => Err(ParseError::new(format!(
                "unknown sensor fault `{text}`, expected unavailable, init, read, timeout or nodata"
            ))),
        }
    }
}

/// A failure of the connection attempt of the Wi-Fi backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiFault {
    Association,
    Authentication,
    Dhcp,
}

/// A failure of the IP data plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpFault {
    /// Nothing is reachable: DNS and connections fail.
    Offline,
    /// Name resolution fails.
    Dns,
    /// Connecting fails.
    Connect,
    /// Established connections are reset on the next read or write.
    Reset,
}

/// A storage failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageFault {
    /// Power is lost after `bytes` more bytes were programmed.
    PowerLossProgram { bytes: usize },
    /// Power is lost after `bytes` more bytes were erased.
    PowerLossErase { bytes: usize },
    /// The CRC of the active slot no longer matches.
    CorruptCrc,
    /// The commit marker of the active slot is destroyed.
    CorruptCommit,
}

/// One simulation command.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Set(Field, f32),
    Fail(Sensor, SensorFault),
    Recover(Sensor),
    Network {
        up: bool,
    },
    Ap {
        ssid: String,
        action: ApAction,
    },
    WifiFail(WifiFault),
    WifiRecover,
    WifiDelay {
        ms: u32,
    },
    WifiDrop,
    WifiFlap(Option<(u32, u32)>),
    IpFail(IpFault),
    IpRecover,
    IpLatency {
        ms: u32,
    },
    IpDropNext,
    Storage(StorageFault),
    Status,
    /// The snapshot of the measurement store: sensor states and the
    /// freshness of every measurement.
    Measurements,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ApAction {
    Up,
    Down,
    Rssi(i8),
}

/// A command that does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(String);

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn number<T: std::str::FromStr>(text: Option<&&str>, what: &str) -> Result<T, ParseError> {
    let text = text.ok_or_else(|| ParseError::new(format!("missing {what}")))?;
    text.parse()
        .map_err(|_| ParseError::new(format!("`{text}` is not a valid {what}")))
}

impl Command {
    /// Parses one command line.
    ///
    /// # Errors
    ///
    /// A [`ParseError`] that names what is wrong.
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let words: Vec<&str> = line.split_whitespace().collect();
        let Some((verb, rest)) = words.split_first() else {
            return Err(ParseError::new("empty command"));
        };
        let first = rest.first().copied();
        match *verb {
            "set" => {
                let field =
                    Field::parse(first.ok_or_else(|| ParseError::new("missing value name"))?)?;
                Ok(Self::Set(field, number(rest.get(1), "number")?))
            }
            "fail" => {
                let sensor =
                    Sensor::parse(first.ok_or_else(|| ParseError::new("missing sensor"))?)?;
                let fault = match rest.get(1) {
                    Some(kind) => SensorFault::parse(kind)?,
                    None => SensorFault::Read,
                };
                Ok(Self::Fail(sensor, fault))
            }
            "recover" => Ok(Self::Recover(Sensor::parse(
                first.ok_or_else(|| ParseError::new("missing sensor"))?,
            )?)),
            "network" => match first {
                Some("up") => Ok(Self::Network { up: true }),
                Some("down") => Ok(Self::Network { up: false }),
                _ => Err(ParseError::new("expected `network up` or `network down`")),
            },
            "ap" => {
                let ssid = first
                    .ok_or_else(|| ParseError::new("missing SSID"))?
                    .to_owned();
                let action = match rest.get(1).copied() {
                    Some("up") => ApAction::Up,
                    Some("down") => ApAction::Down,
                    Some("rssi") => ApAction::Rssi(number(rest.get(2), "RSSI in dBm")?),
                    _ => return Err(ParseError::new("expected `up`, `down` or `rssi <dBm>`")),
                };
                Ok(Self::Ap { ssid, action })
            }
            "wifi" => match first {
                Some("fail") => Ok(Self::WifiFail(match rest.get(1).copied() {
                    Some("association") => WifiFault::Association,
                    Some("authentication") => WifiFault::Authentication,
                    Some("dhcp") => WifiFault::Dhcp,
                    _ => {
                        return Err(ParseError::new(
                            "expected `wifi fail association|authentication|dhcp`",
                        ));
                    }
                })),
                Some("recover") => Ok(Self::WifiRecover),
                Some("delay") => Ok(Self::WifiDelay {
                    ms: number(rest.get(1), "delay in ms")?,
                }),
                Some("drop") => Ok(Self::WifiDrop),
                Some("flap") => match rest.get(1).copied() {
                    Some("off") => Ok(Self::WifiFlap(None)),
                    _ => Ok(Self::WifiFlap(Some((
                        number(rest.get(1), "up time in ms")?,
                        number(rest.get(2), "down time in ms")?,
                    )))),
                },
                _ => Err(ParseError::new(
                    "expected `wifi fail|recover|delay|drop|flap`",
                )),
            },
            "ip" => match first {
                Some("fail") => Ok(Self::IpFail(match rest.get(1).copied() {
                    Some("offline") => IpFault::Offline,
                    Some("dns") => IpFault::Dns,
                    Some("connect") => IpFault::Connect,
                    Some("reset") => IpFault::Reset,
                    _ => {
                        return Err(ParseError::new(
                            "expected `ip fail offline|dns|connect|reset`",
                        ));
                    }
                })),
                Some("recover") => Ok(Self::IpRecover),
                Some("latency") => Ok(Self::IpLatency {
                    ms: number(rest.get(1), "latency in ms")?,
                }),
                Some("drop-next") => Ok(Self::IpDropNext),
                _ => Err(ParseError::new(
                    "expected `ip fail|recover|latency|drop-next`",
                )),
            },
            "storage" => match first {
                Some("power-loss") => {
                    let bytes = number(rest.get(2), "byte count")?;
                    match rest.get(1).copied() {
                        Some("program") => {
                            Ok(Self::Storage(StorageFault::PowerLossProgram { bytes }))
                        }
                        Some("erase") => Ok(Self::Storage(StorageFault::PowerLossErase { bytes })),
                        _ => Err(ParseError::new(
                            "expected `storage power-loss program|erase <bytes>`",
                        )),
                    }
                }
                Some("corrupt") => match rest.get(1).copied() {
                    Some("crc") => Ok(Self::Storage(StorageFault::CorruptCrc)),
                    Some("commit") => Ok(Self::Storage(StorageFault::CorruptCommit)),
                    _ => Err(ParseError::new("expected `storage corrupt crc|commit`")),
                },
                _ => Err(ParseError::new("expected `storage power-loss|corrupt`")),
            },
            "status" => Ok(Self::Status),
            "measurements" => Ok(Self::Measurements),
            "shutdown" => Ok(Self::Shutdown),
            other => Err(ParseError::new(format!("unknown command `{other}`"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_commands() {
        assert_eq!(
            Command::parse("set co2 1200"),
            Ok(Command::Set(Field::Co2, 1200.0))
        );
        assert_eq!(
            Command::parse("fail scd4x"),
            Ok(Command::Fail(Sensor::Scd4x, SensorFault::Read))
        );
        assert_eq!(
            Command::parse("fail sds011 timeout"),
            Ok(Command::Fail(Sensor::Sds011, SensorFault::Timeout))
        );
        assert_eq!(
            Command::parse("ap ROHI-LAB rssi -70"),
            Ok(Command::Ap {
                ssid: "ROHI-LAB".into(),
                action: ApAction::Rssi(-70)
            })
        );
        assert_eq!(
            Command::parse("wifi flap 5000 3000"),
            Ok(Command::WifiFlap(Some((5000, 3000))))
        );
        assert_eq!(
            Command::parse("storage power-loss program 64"),
            Ok(Command::Storage(StorageFault::PowerLossProgram {
                bytes: 64
            }))
        );
        assert_eq!(
            Command::parse("network down"),
            Ok(Command::Network { up: false })
        );
    }

    #[test]
    fn rejects_bad_input_with_a_reason() {
        for line in [
            "",
            "set co2",
            "set pressure high",
            "fail toaster",
            "wifi",
            "storage",
            "frobnicate",
        ] {
            let error = Command::parse(line).unwrap_err();
            assert!(!error.to_string().is_empty(), "{line}");
        }
    }
}
