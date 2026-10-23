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
//! The `rohi` command line: argument parsing, the password prompt, seed
//! loading and output (human readable or JSON). Protocol logic lives in
//! [`Device`]; the binary only calls [`main`].

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::model::{PasswordChange, WifiUpdate};
use crate::{BleLink, ClientTransport, DerivedIdentity, Device, Error, derive_identity, format};

#[derive(Parser)]
#[command(
    name = "rohi",
    version,
    about = "Control ROHI devices over Bluetooth LE"
)]
pub struct Cli {
    /// Print JSON instead of human readable text.
    #[arg(long, global = true)]
    pub json: bool,

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
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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
    /// Cryptographic identity of the device: show, provision or erase.
    #[command(visible_alias = "id")]
    Identity {
        /// Without an action: show the identity state.
        #[command(subcommand)]
        action: Option<IdentityAction>,
    },
}

#[derive(Subcommand)]
pub enum IdentityAction {
    /// Derive the identity of this device from a master seed and install it.
    ///
    /// The master seed stays on this machine; the device receives only the
    /// secret derived with the hard junction `//<NUMBER>`. Use every number
    /// once per master: the device cannot detect duplicates.
    Provision(Provision),
    /// Remove the identity; the device must be provisioned again to sign.
    Erase {
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Args)]
pub struct Provision {
    /// Sensor number used for the hard derivation `//<NUMBER>`.
    #[arg(short, long, value_name = "NUMBER")]
    pub number: u64,

    /// File with the master secret URI (mnemonic phrase or `0x` seed, optional
    /// path); it is read here and never sent to the device.
    #[arg(long, value_name = "PATH")]
    pub seed: PathBuf,
}

#[derive(Subcommand)]
pub enum ConfigSection {
    /// Show the Wi-Fi configuration.
    Wifi,
}

#[derive(Subcommand)]
pub enum WifiAction {
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
pub struct Configure {
    /// Network name.
    #[arg(long)]
    ssid: Option<String>,

    /// Network password. Without it (and without `--open`) the password is
    /// asked for; an empty answer keeps the stored one.
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

async fn connect(cli: &Cli) -> crate::Result<Device<BleLink>> {
    let mut link =
        BleLink::connect(cli.device.as_deref(), Duration::from_secs(cli.scan_timeout)).await?;
    if let Some(payload) = cli.payload {
        link.set_payload(payload);
    }
    let mut device = Device::new(link);
    device.set_timeout(Duration::from_secs(cli.timeout));
    Ok(device)
}

fn password_prompt(ssid: Option<&str>) -> crate::Result<PasswordChange> {
    let prompt = match ssid {
        Some(ssid) => format!("Password for {ssid} (empty to keep the stored one): "),
        None => "New password (empty to keep the stored one): ".to_owned(),
    };
    let password = rpassword::prompt_password(prompt)
        .map_err(|error| Error::InvalidInput(format!("cannot read the password: {error}")))?;
    Ok(if password.is_empty() {
        PasswordChange::Keep
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
) -> crate::Result<()> {
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

pub async fn run(cli: Cli) -> crate::Result<()> {
    let json = cli.json;
    if let Command::Scan { duration } = &cli.command {
        let devices = crate::scan(Duration::from_secs(*duration)).await?;
        emit(json, &devices, || format::devices(&devices));
        return Ok(());
    }

    // A bad seed must fail before a device is touched.
    let identity = prepare(&cli.command)?;
    let mut device = connect(&cli).await?;
    let result = execute(&mut device, cli.command, identity, json).await;
    // Dropping the peripheral does not disconnect it (BlueZ keeps the link),
    // and the device accepts a single connection: always close it.
    let closed = device.into_transport().disconnect().await;
    result.and(closed)
}

/// Everything a command needs from the host before it talks to the device:
/// for `identity provision` the identity derived from the seed file.
pub fn prepare(command: &Command) -> crate::Result<Option<DerivedIdentity>> {
    match command {
        Command::Identity {
            action: Some(IdentityAction::Provision(provision)),
        } => load_identity(provision).map(Some),
        _ => Ok(None),
    }
}

fn load_identity(provision: &Provision) -> crate::Result<DerivedIdentity> {
    let seed = std::fs::read_to_string(&provision.seed).map_err(|error| {
        Error::InvalidInput(format!(
            "cannot read the seed file {}: {error}",
            provision.seed.display()
        ))
    })?;
    let seed = Zeroizing::new(seed);
    derive_identity(seed.trim(), provision.number)
}

async fn identity<T: ClientTransport>(
    device: &mut Device<T>,
    action: Option<IdentityAction>,
    derived: Option<DerivedIdentity>,
    json: bool,
) -> crate::Result<()> {
    match action {
        None => {
            let identity = device.identity().await?;
            emit(json, &identity, || format::identity(&identity));
        }
        Some(IdentityAction::Provision(_)) => {
            let derived = derived.ok_or_else(|| {
                Error::InvalidInput("the identity was not derived before connecting".into())
            })?;
            let public_key = device.provision_identity(&derived).await?;
            #[derive(Serialize)]
            struct Out {
                ok: bool,
                public_key: String,
            }
            let out = Out {
                ok: true,
                public_key: public_key.to_string(),
            };
            emit(json, &out, || {
                format!("Identity provisioned\n\nPublic key: {public_key}\n")
            });
        }
        Some(IdentityAction::Erase { yes }) => {
            if !yes
                && !confirm(
                    "This device will lose its current cryptographic identity.\n\
                     It must be provisioned again before signed telemetry can be produced.\n\n\
                     Continue?",
                )
            {
                return Err(Error::InvalidInput("not confirmed".into()));
            }
            device.erase_identity().await?;
            emit(json, &Done { ok: true }, || "Identity erased\n".to_owned());
        }
    }
    Ok(())
}

/// Runs `command` on a connected device. `identity` comes from [`prepare`].
pub async fn execute<T: ClientTransport>(
    device: &mut Device<T>,
    command: Command,
    identity: Option<DerivedIdentity>,
    json: bool,
) -> crate::Result<()> {
    match command {
        Command::Scan { .. } => unreachable!("handled above"),
        Command::Info => {
            let info = device.info().await?;
            let capabilities = device.capabilities().await?;
            #[derive(Serialize)]
            struct Out<'a> {
                #[serde(flatten)]
                info: &'a crate::model::DeviceInfo,
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
            let status = if capabilities.contains(&crate::model::Capability::WifiStatus) {
                Some(device.wifi_status().await?)
            } else {
                None
            };
            #[derive(Serialize)]
            struct Out<'a> {
                device: &'a crate::model::DeviceInfo,
                wifi: &'a Option<crate::model::WifiStatus>,
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
            wifi(device, action.unwrap_or(WifiAction::Status), json).await?;
        }
        Command::Identity { action } => {
            self::identity(device, action, identity, json).await?;
        }
    }
    Ok(())
}

/// Runs the command line and returns the process exit code.
pub async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
