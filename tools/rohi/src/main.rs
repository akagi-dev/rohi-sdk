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
//! `rohi`: command line client of ROHI devices over BLE.
//!
//! A thin layer over the `rohi` library: argument parsing, the password
//! prompt, and output (human readable or JSON). Protocol logic lives in the
//! library.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use rohi::model::{PasswordChange, WifiUpdate};
use rohi::{BleLink, ClientTransport, Device, Error, format};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "rohi",
    version,
    about = "Control ROHI devices over Bluetooth LE"
)]
struct Cli {
    /// Print JSON instead of human readable text.
    #[arg(long, global = true)]
    json: bool,

    /// Device to use: the identifier printed by `rohi scan`. Defaults to the
    /// first ROHI device found.
    #[arg(long, short, global = true, value_name = "ID")]
    device: Option<String>,

    /// Seconds to look for the device.
    #[arg(long, global = true, default_value_t = 10, value_name = "SECONDS")]
    scan_timeout: u64,

    /// Seconds to wait for a response of the device.
    #[arg(long, global = true, default_value_t = 10, value_name = "SECONDS")]
    timeout: u64,

    /// Bytes per BLE chunk, if the link negotiated more than the default
    /// 20 bytes (ATT MTU minus 3).
    #[arg(long, global = true, value_name = "BYTES")]
    payload: Option<usize>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Find ROHI devices nearby.
    Scan {
        /// Seconds to scan.
        #[arg(long, default_value_t = 5, value_name = "SECONDS")]
        duration: u64,
    },
    /// Show firmware, board and supported features.
    Info,
    /// Show the overall device status.
    Status,
    /// Show current sensor measurements.
    Sensors,
    /// Show the device configuration.
    Config {
        #[command(subcommand)]
        section: Option<ConfigSection>,
    },
    /// Wi-Fi status and control.
    Wifi {
        #[command(subcommand)]
        action: Option<WifiAction>,
    },
}

#[derive(Subcommand)]
enum ConfigSection {
    /// Show the Wi-Fi configuration.
    Wifi,
}

#[derive(Subcommand)]
enum WifiAction {
    /// Show the Wi-Fi status (default).
    Status,
    /// List networks the device sees.
    Scan,
    /// Change the Wi-Fi configuration; omitted settings are kept.
    Configure(Configure),
    /// Reconnect now.
    Reconnect,
    /// Disconnect and stay idle.
    Disconnect,
    /// Remove the stored Wi-Fi configuration.
    Forget {
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Args)]
struct Configure {
    /// Network name.
    #[arg(long)]
    ssid: Option<String>,

    /// Network password.
    ///
    /// Prefer the prompt or the environment variable: a password on the command line stays in
    /// the shell history and is visible to other users in the process list.
    #[arg(
        long,
        env = "ROHI_WIFI_PASSWORD",
        hide_env_values = true,
        value_name = "PASSWORD"
    )]
    password: Option<String>,

    /// The network has no password; remove a stored one.
    #[arg(long, conflicts_with = "password")]
    open: bool,
}

#[derive(Serialize)]
struct Done {
    ok: bool,
}

fn emit<T: Serialize>(json: bool, value: &T, text: impl FnOnce() -> String) {
    if json {
        match serde_json::to_string_pretty(value) {
            Ok(out) => println!("{out}"),
            Err(error) => eprintln!("error: {error}"),
        }
    } else {
        print!("{}", text());
    }
}

async fn connect(cli: &Cli) -> rohi::Result<Device<BleLink>> {
    let mut link =
        BleLink::connect(cli.device.as_deref(), Duration::from_secs(cli.scan_timeout)).await?;
    if let Some(payload) = cli.payload {
        link.set_payload(payload);
    }
    let mut device = Device::new(link);
    device.set_timeout(Duration::from_secs(cli.timeout));
    Ok(device)
}

fn password_prompt(ssid: Option<&str>) -> rohi::Result<PasswordChange> {
    let prompt = match ssid {
        Some(ssid) => format!("Password for {ssid} (empty for an open network): "),
        None => "New password (empty for an open network): ".to_owned(),
    };
    let password = rpassword::prompt_password(prompt)
        .map_err(|error| Error::InvalidInput(format!("cannot read the password: {error}")))?;
    Ok(if password.is_empty() {
        PasswordChange::Clear
    } else {
        PasswordChange::Set(password.into_bytes())
    })
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
}

async fn wifi<T: ClientTransport>(
    device: &mut Device<T>,
    action: WifiAction,
    json: bool,
) -> rohi::Result<()> {
    match action {
        WifiAction::Status => {
            let status = device.wifi_status().await?;
            emit(json, &status, || format::wifi_status(&status));
        }
        WifiAction::Scan => {
            let networks = device.wifi_scan().await?;
            emit(json, &networks, || format::wifi_scan(&networks));
        }
        WifiAction::Configure(configure) => {
            let password = if configure.open {
                PasswordChange::Clear
            } else if let Some(password) = configure.password {
                PasswordChange::Set(password.into_bytes())
            } else {
                password_prompt(configure.ssid.as_deref())?
            };
            let update = WifiUpdate {
                ssid: configure.ssid.map(String::into_bytes),
                password,
            };
            device.update_wifi(&update).await?;
            emit(json, &Done { ok: true }, || {
                "Wi-Fi configuration stored. Follow `rohi wifi status` to see the connection.\n"
                    .to_owned()
            });
        }
        WifiAction::Reconnect => {
            device.wifi_reconnect().await?;
            emit(json, &Done { ok: true }, || "Reconnecting\n".to_owned());
        }
        WifiAction::Disconnect => {
            device.wifi_disconnect().await?;
            emit(json, &Done { ok: true }, || "Disconnected\n".to_owned());
        }
        WifiAction::Forget { yes } => {
            if !yes && !confirm("Remove the stored Wi-Fi configuration?") {
                return Err(Error::InvalidInput("not confirmed".into()));
            }
            device.forget_wifi().await?;
            emit(json, &Done { ok: true }, || {
                "Wi-Fi configuration removed\n".to_owned()
            });
        }
    }
    Ok(())
}

async fn run(cli: Cli) -> rohi::Result<()> {
    let json = cli.json;
    if let Command::Scan { duration } = &cli.command {
        let devices = rohi::scan(Duration::from_secs(*duration)).await?;
        emit(json, &devices, || format::devices(&devices));
        return Ok(());
    }

    let mut device = connect(&cli).await?;
    match cli.command {
        Command::Scan { .. } => unreachable!("handled above"),
        Command::Info => {
            let info = device.info().await?;
            let capabilities = device.capabilities().await?;
            #[derive(Serialize)]
            struct Out<'a> {
                #[serde(flatten)]
                info: &'a rohi::model::DeviceInfo,
                capabilities: Vec<&'static str>,
            }
            let out = Out {
                info: &info,
                capabilities: capabilities.iter().map(|c| c.as_str()).collect(),
            };
            emit(json, &out, || format::info(&info, &capabilities));
        }
        Command::Status => {
            let info = device.info().await?;
            let capabilities = device.capabilities().await?;
            let status = if capabilities.contains(&rohi::model::Capability::WifiStatus) {
                Some(device.wifi_status().await?)
            } else {
                None
            };
            #[derive(Serialize)]
            struct Out<'a> {
                device: &'a rohi::model::DeviceInfo,
                wifi: &'a Option<rohi::model::WifiStatus>,
            }
            emit(
                json,
                &Out {
                    device: &info,
                    wifi: &status,
                },
                || {
                    let mut text = format::info(&info, &capabilities);
                    if let Some(status) = &status {
                        text.push_str(&format::wifi_status(status));
                    }
                    text
                },
            );
        }
        Command::Sensors => {
            let sensors = device.sensors().await?;
            emit(json, &sensors, || format::sensors(&sensors));
        }
        Command::Config { section } => {
            let config = device.config().await?;
            match section {
                None | Some(ConfigSection::Wifi) => {
                    emit(json, &config, || format::config(&config));
                }
            }
        }
        Command::Wifi { action } => {
            wifi(&mut device, action.unwrap_or(WifiAction::Status), json).await?;
        }
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
