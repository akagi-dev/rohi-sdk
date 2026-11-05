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

//! Process lifecycle of the host platform: logging, exit codes, readiness and
//! shutdown.

use std::fmt;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use log::LevelFilter;

/// Why the device did not start. Expected host problems are reported with a
/// message and a status, never with a panic.
#[derive(Debug)]
pub struct StartupError {
    kind: Failure,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The scenario cannot be used.
    Scenario,
    /// The configuration flash cannot be opened.
    Storage,
    /// A listener cannot be bound.
    Bind,
    /// The host platform cannot be initialized.
    Platform,
}

impl Failure {
    /// The process exit status. Command line errors use 2, like `clap`.
    pub const fn code(self) -> u8 {
        match self {
            Self::Scenario => 65,
            Self::Storage => 74,
            Self::Bind => 75,
            Self::Platform => 76,
        }
    }
}

impl StartupError {
    pub fn new(kind: Failure, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> Failure {
        self.kind
    }

    pub fn exit_code(&self) -> ExitCode {
        ExitCode::from(self.kind.code())
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StartupError {}

/// Initializes the logger: readable timestamps on stderr.
pub fn init_logging(level: LevelFilter) {
    let _ = env_logger::Builder::new()
        .filter_level(level)
        .parse_default_env()
        .format_timestamp_millis()
        .try_init();
}

/// The interfaces of a running device, as reported by the READY line.
#[derive(Clone, Debug, Default)]
pub struct Interfaces {
    /// The TCP control listener (`std-tcp`).
    pub control: Option<SocketAddr>,
    /// The development-management listener.
    pub manage: Option<SocketAddr>,
    /// The BLE peripheral (`std-ble`): the adapter it advertises on.
    pub ble: Option<String>,
}

/// The line that tells a test harness or a script that the device is up:
/// `READY` and a `key=value` for every interface that runs.
pub fn ready_line(interfaces: &Interfaces) -> String {
    let mut line = String::from("READY");
    if let Some(control) = interfaces.control {
        line.push_str(&format!(" control={control}"));
    }
    if let Some(manage) = interfaces.manage {
        line.push_str(&format!(" manage={manage}"));
    }
    if let Some(ble) = &interfaces.ble {
        line.push_str(&format!(" ble={ble}"));
    }
    line
}

/// Prints the READY line on stdout (and into `file`).
pub fn announce(line: &str, file: Option<&Path>) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
    if let Some(file) = file
        && let Err(e) = std::fs::write(file, format!("{line}\n"))
    {
        log::warn!("cannot write {}: {e}", file.display());
    }
}

/// Raised by the signal handler and the `shutdown` command.
pub static SHUTDOWN: Signal<CriticalSectionRawMutex, ()> = Signal::new();

static DONE: AtomicBool = AtomicBool::new(false);

/// Makes the executor return after the current poll.
pub fn finish() {
    DONE.store(true, Ordering::Release);
}

/// Whether [`finish`] was called.
pub fn finished() -> bool {
    DONE.load(Ordering::Acquire)
}

/// Turns Ctrl-C and SIGTERM into a clean shutdown.
pub fn install_signal_handler() -> Result<(), StartupError> {
    ctrlc::set_handler(|| SHUTDOWN.signal(())).map_err(|e| {
        StartupError::new(
            Failure::Platform,
            format!("cannot install the signal handler: {e}"),
        )
    })
}
