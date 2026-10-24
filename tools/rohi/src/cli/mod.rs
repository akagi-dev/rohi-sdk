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
//! The `rohi` command line.
//!
//! ```text
//! clap / terminal / output / backup files
//!                   ↓
//!                CLI app
//!                   ↓
//!               Device<T>
//! ```
//!
//! `args` defines the interface; [`execute`] runs one command on a
//! connected [`Device`] over any [`ClientTransport`]; [`run`] resolves the
//! target, connects and cleans up; [`main`] is the process entry point.
//! Protocol logic lives in [`Device`] and below; nothing here frames or
//! decodes messages.

mod args;
mod backup;
mod diagnostic;
mod dto;
mod identity;
mod status;
mod target;
mod terminal;
mod time;
mod wifi;

use std::future::Future;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

pub use args::{
    BackupAction, Cli, Command, ConfigSection, Configure, Connect, IdentityAction, Import,
    OutputFormat, Provision, WifiAction,
};
pub use backup::{Backup, BackupError};
pub use diagnostic::{CliError, Diagnostic, PartialRestore};
pub use target::{Match, Target, resolve};
pub use terminal::Io;

use crate::{BleLink, ClientTransport, DerivedIdentity, Device, format, scan, scan_until};

/// Everything a command needs besides the device: the terminal, the output
/// format and what is known about the connection.
pub struct Ctx {
    /// Terminal streams.
    pub io: Io,
    /// How results are written.
    pub format: OutputFormat,
    /// The URI of the connected device, for `status`.
    pub uri: Option<String>,
    /// A Wi-Fi password given through `ROHI_WIFI_PASSWORD`.
    pub wifi_password: Option<String>,
    /// Pause between Wi-Fi status polls of `wifi connect`.
    pub poll_interval: Duration,
}

impl Ctx {
    /// A context writing `format` to `io`.
    pub fn new(io: Io, format: OutputFormat) -> Self {
        Self {
            io,
            format,
            uri: None,
            wifi_password: None,
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// What a command needs from the host before it talks to the device, so that
/// a bad seed or backup file fails before a device is touched.
#[derive(Default)]
pub struct Prepared {
    /// For `identity provision`: the identity derived from the seed file.
    pub identity: Option<DerivedIdentity>,
    /// For `backup import`: the parsed and validated backup.
    pub backup: Option<Backup>,
}

/// Reads the files `command` refers to.
pub fn prepare(command: &Command) -> Result<Prepared, CliError> {
    Ok(match command {
        Command::Identity {
            action: Some(IdentityAction::Provision(provision)),
        } => Prepared {
            identity: Some(identity::load(provision)?),
            backup: None,
        },
        Command::Backup {
            action: BackupAction::Import(import),
        } => Prepared {
            identity: None,
            backup: Some(Backup::load(&import.file)?),
        },
        _ => Prepared::default(),
    })
}

/// Runs `command` on a connected device.
pub async fn execute<T: ClientTransport>(
    device: &mut Device<T>,
    command: Command,
    prepared: Prepared,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match command {
        Command::Scan { .. } => Err(CliError::Usage("scan does not talk to a device".into())),
        Command::Status => status::run(device, ctx).await,
        Command::Sensors => {
            let sensors = device.sensors().await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::Sensors::new(&sensors)),
                || format::sensors(&sensors),
            )
        }
        Command::Time => time::run(device, ctx).await,
        Command::Config { section } => match section {
            None | Some(ConfigSection::Wifi) => wifi::config(device, ctx).await,
        },
        Command::Wifi { action } => {
            wifi::run(device, action.unwrap_or(WifiAction::Status), ctx).await
        }
        Command::Backup { action } => match action {
            BackupAction::Export { file } => backup::export(device, file, ctx).await,
            BackupAction::Import(import) => {
                let backup = prepared.backup.ok_or_else(|| {
                    CliError::Usage("the backup was not read before connecting".into())
                })?;
                backup::import(device, import, backup, ctx).await
            }
        },
        Command::Identity { action } => identity::run(device, action, prepared, ctx).await,
    }
}

async fn interruptible<T>(work: impl Future<Output = Result<T, CliError>>) -> Result<T, CliError> {
    tokio::select! {
        result = work => result,
        _ = tokio::signal::ctrl_c() => Err(CliError::Interrupted),
    }
}

async fn discover(duration: u64, ctx: &mut Ctx) -> Result<(), CliError> {
    let devices = scan(Duration::from_secs(duration)).await?;
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::Scan::new(&devices)),
        || format::devices(&devices),
    )
}

/// Finds the device the user means and connects to it over BLE.
async fn connect_ble(
    cli: &Cli,
    selector: Option<&str>,
    ctx: &mut Ctx,
) -> Result<Device<BleLink>, CliError> {
    let wait = Duration::from_secs(cli.scan_timeout);
    let found = scan_until(wait, |devices, elapsed| {
        target::settled(selector, devices, elapsed)
    })
    .await?;
    let chosen = &found[target::choose(selector, &found, &mut ctx.io).await?];
    ctx.uri = Some(dto::ble_uri(chosen));

    let link = BleLink::connect(Some(&chosen.id), wait).await?;
    let mut device = Device::new(link);
    device.set_timeout(Duration::from_secs(cli.timeout));
    Ok(device)
}

/// Runs the command line `cli`: resolves the target, connects, executes and
/// always closes the connection.
pub async fn run(cli: Cli, ctx: &mut Ctx) -> Result<(), CliError> {
    if let Command::Scan { duration } = &cli.command {
        return interruptible(discover(*duration, ctx)).await;
    }

    let target = Target::parse(cli.device.as_deref())?;
    let selector = match &target {
        Target::Ble(selector) => selector.clone(),
        Target::Tcp(_) => return Err(CliError::UnsupportedTransport("tcp".into())),
    };
    // A bad seed or backup must fail before a device is touched.
    let prepared = prepare(&cli.command)?;
    let mut device = interruptible(connect_ble(&cli, selector.as_deref(), ctx)).await?;
    let result = interruptible(execute(&mut device, cli.command, prepared, ctx)).await;
    // Dropping the peripheral does not disconnect it (BlueZ keeps the link),
    // and the device accepts a single connection: always close it.
    let closed = device.into_transport().disconnect().await;
    result.and(closed.map_err(CliError::from))
}

/// Runs the command line and returns the process exit code.
pub async fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut ctx = Ctx::new(Io::stdio(), cli.output);
    ctx.wifi_password = std::env::var("ROHI_WIFI_PASSWORD")
        .ok()
        .filter(|password| !password.is_empty());
    match run(cli, &mut ctx).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !error.is_broken_pipe() {
                ctx.io.note(&format!("{}\n", error.diagnostic()));
            }
            error.exit_code()
        }
    }
}
