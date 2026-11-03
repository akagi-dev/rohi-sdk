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

//! Command line of the simulator.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;
use log::LevelFilter;

/// Native Altruist Urban simulator.
///
/// Runs the Altruist firmware stack as a host process: simulated sensors,
/// file-backed configuration flash, a native Wi-Fi/IP platform and the ROHI
/// control protocol over localhost TCP. Connect with `rohi --device
/// tcp://127.0.0.1:9300 ...`.
#[derive(Parser, Debug, Clone)]
#[command(name = "altruist-sim", version)]
pub struct Args {
    /// Device name, reported in logs and diagnostics.
    #[arg(long, default_value = "urban-sim")]
    pub name: String,

    /// Address of the control and management listeners.
    #[arg(long, default_value = "127.0.0.1", value_name = "ADDRESS")]
    pub bind: IpAddr,

    /// Port of the ROHI control transport; 0 picks a free port.
    #[arg(long, default_value_t = 9300, value_name = "PORT")]
    pub control_port: u16,

    /// Port of the simulator management interface (fault injection and
    /// scenario control); 0 picks a free port. Defaults to the control port
    /// plus one, or a free port when the control port is 0.
    #[arg(long, value_name = "PORT")]
    pub manage_port: Option<u16>,

    /// Configuration flash image. It survives restarts.
    #[arg(
        long,
        default_value = ".altruist-sim/device.flash",
        value_name = "FILE",
        conflicts_with = "ephemeral"
    )]
    pub storage: PathBuf,

    /// Keep the configuration flash in memory only.
    #[arg(long)]
    pub ephemeral: bool,

    /// Scenario file (TOML) with the initial state and a timeline of events.
    #[arg(long, value_name = "FILE")]
    pub scenario: Option<PathBuf>,

    /// Seed of every random sequence of the simulation: sensor jitter and the
    /// nonces of the telemetry frames. The same scenario, seed and
    /// configuration produce the same simulated sequence.
    #[arg(long, value_name = "SEED")]
    pub seed: Option<u64>,

    /// Log level: error, warn, info, debug or trace. `RUST_LOG` overrides it.
    #[arg(long, default_value = "info", value_name = "LEVEL")]
    pub log_level: LevelFilter,

    /// Also write the READY line to this file.
    #[arg(long, value_name = "FILE")]
    pub ready_file: Option<PathBuf>,

    /// The ports of the previous run, passed on by a reboot of the device.
    #[arg(long, hide = true, num_args = 2, value_names = ["CONTROL", "MANAGE"])]
    pub reboot_ports: Option<Vec<u16>>,
}

impl Args {
    /// The control and management ports to bind.
    pub fn ports(&self) -> (u16, u16) {
        if let Some([control, manage]) = self.reboot_ports.as_deref() {
            return (*control, *manage);
        }
        let manage = self.manage_port.unwrap_or(match self.control_port {
            0 => 0,
            port => port.wrapping_add(1),
        });
        (self.control_port, manage)
    }
}
