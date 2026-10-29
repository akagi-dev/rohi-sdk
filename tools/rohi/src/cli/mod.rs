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
mod factory_reset;
mod reboot;
mod status;
mod target;
mod terminal;
mod values;

use std::future::Future;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

pub use args::{Cli, ColorChoice, Command, ConfigAction, OutputFormat};
pub use diagnostic::{CliError, Diagnostic, PartialApply};
pub use target::{Match, Target, resolve};
pub use terminal::Io;

use crate::format::Paint;
use crate::{BleLink, ClientTransport, Device, format, scan, scan_until};

/// Everything a command needs besides the device: the terminal, the output
/// format and what is known about the connection.
pub struct Ctx {
    /// Terminal streams.
    pub io: Io,
    /// How results are written.
    pub format: OutputFormat,
    /// Styling of the human output; plain unless the terminal supports color.
    pub paint: Paint,
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
            paint: Paint::plain(),
            uri: None,
            watch_limit: None,
        }
    }

    /// Styles the human output as `choice` says. `auto` colors when stdout is a
    /// terminal, unless `NO_COLOR` (<https://no-color.org>) is set or `TERM` is
    /// `dumb`.
    pub fn color(&mut self, choice: ColorChoice) {
        let wanted = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                self.io.screen()
                    && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
                    && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
            }
        };
        self.paint = if wanted {
            Paint::ansi()
        } else {
            Paint::plain()
        };
    }
}

/// Runs `command` on a connected device.
pub async fn execute<T: ClientTransport>(
    device: &mut Device<T>,
    command: Command,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    if let Some(help) = static_help(&command) {
        return ctx.io.result(&help);
    }
    match command {
        Command::Scan { .. } => Err(CliError::Usage("scan does not talk to a device".into())),
        Command::Status {
            service,
            watch,
            interval,
        } => status::run(device, service, watch, interval, ctx).await,
        Command::Config {
            action,
            section,
            assignments,
            dry_run,
            help,
        } => {
            let request = config::Request {
                section,
                assignments,
                dry_run,
                help,
            };
            config::run(device, action, request, ctx).await
        }
        Command::FactoryReset { yes } => factory_reset::run(device, yes, ctx).await,
        Command::Reboot => reboot::run(device, ctx).await,
        Command::Action {
            service,
            action,
            args,
            help,
        } => {
            let request = action::Request {
                service,
                action,
                args,
                help,
            };
            action::run(device, request, ctx).await
        }
    }
}

/// The ordinary help of `rohi config` and `rohi action`, which do not need a
/// device. Their `--help` is handled here and not by clap, because for a
/// section or an action it describes the object and is generated from the
/// schema the device reports.
pub fn static_help(command: &Command) -> Option<String> {
    let name = match command {
        Command::Config {
            action: None,
            section: None,
            help: true,
            ..
        } => "config",
        Command::Action {
            service: None,
            help: true,
            ..
        } => "action",
        _ => return None,
    };
    let mut cli = <Cli as clap::CommandFactory>::command();
    cli.build();
    let help = cli.find_subcommand_mut(name)?.render_long_help();
    Some(format!("{help}\n"))
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
        || format::devices(ctx.paint, &devices),
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
    if let Some(help) = static_help(&cli.command) {
        return ctx.io.result(&help);
    }

    let target = Target::parse(cli.device.as_deref())?;
    let selector = match &target {
        Target::Ble(selector) => selector.clone(),
        Target::Tcp(_) => return Err(CliError::UnsupportedTransport("tcp".into())),
    };
    let mut device = interruptible(connect_ble(&cli, selector.as_deref(), ctx)).await?;
    let result = interruptible(execute(&mut device, cli.command, ctx)).await;
    // Dropping the peripheral does not disconnect it (BlueZ keeps the link),
    // and the device accepts a single connection: always close it.
    let closed = device.into_transport().disconnect().await;
    result.and(closed.map_err(CliError::from))
}

/// Runs the command line and returns the process exit code.
pub async fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut ctx = Ctx::new(Io::stdio(), cli.output);
    ctx.color(cli.color);
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
