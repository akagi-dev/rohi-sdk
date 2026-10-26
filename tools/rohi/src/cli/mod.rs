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
//! clap / terminal / output / configuration files
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

mod action;
mod args;
mod config;
mod diagnostic;
mod dto;
mod identity;
mod status;
mod target;
mod terminal;
mod values;

use std::future::Future;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

pub use args::{
    ActionCommand, Cli, Command, ConfigAction, IdentityAction, OutputFormat, Provision,
};
pub use diagnostic::{CliError, Diagnostic, PartialApply};
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
    /// The URI of the connected device.
    pub uri: Option<String>,
    /// Stops `status --watch` after this many printed documents; for tests.
    pub watch_limit: Option<usize>,
}

impl Ctx {
    /// A context writing `format` to `io`.
    pub fn new(io: Io, format: OutputFormat) -> Self {
        Self {
            io,
            format,
            uri: None,
            watch_limit: None,
        }
    }
}

/// What a command needs from the host before it talks to the device, so that
/// a bad seed file fails before a device is touched.
#[derive(Default)]
pub struct Prepared {
    /// For `identity provision`: the identity derived from the seed file.
    pub identity: Option<DerivedIdentity>,
}

/// Reads the files `command` refers to.
pub fn prepare(command: &Command) -> Result<Prepared, CliError> {
    Ok(match command {
        Command::Identity {
            action: Some(IdentityAction::Provision(provision)),
        } => Prepared {
            identity: Some(identity::load(provision)?),
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
        Command::Status {
            service,
            watch,
            interval,
        } => status::run(device, service, watch, interval, ctx).await,
        Command::Config { action } => config::run(device, action, ctx).await,
        Command::Action { action } => action::run(device, action, ctx).await,
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
