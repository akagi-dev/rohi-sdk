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

//! Command line of `altruist-urban`.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::Parser;
use log::LevelFilter;

/// Altruist Urban as a host process.
///
/// Runs the Altruist Urban application on the host: file-backed configuration
/// flash, the Wi-Fi/IP platform of the host and the ROHI control protocol over
/// TCP and/or Bluetooth LE, depending on the build features. Connect with
/// `rohi --device tcp://127.0.0.1:9300 ...`.
#[derive(Parser, Debug, Clone)]
#[command(name = "altruist-urban", version)]
pub struct Args {
    /// Device name, reported in logs and diagnostics. Defaults to the name of
    /// the scenario, or `urban-std`.
    #[arg(long)]
    pub name: Option<String>,

    /// Address of the control and management listeners. The control listener
    /// follows the stored `control_tcp.bind_address` (loopback by default) unless
    /// this is given; the management listener is on loopback unless this is
    /// given.
    #[arg(long, value_name = "ADDRESS")]
    pub bind: Option<IpAddr>,

    /// Allow `--bind` to be a non-loopback address. The control transport and
    /// the management interface have no authentication or encryption: anyone who
    /// can reach them can reconfigure, provision and erase the device.
    #[arg(long)]
    pub unsafe_allow_remote: bool,

    /// Port of the ROHI control transport over TCP; 0 picks a free port. The
    /// stored `control_tcp.port` (9300 by default) applies unless this is given.
    #[cfg(feature = "std-tcp")]
    #[arg(long, value_name = "PORT")]
    pub control_port: Option<u16>,

    /// Start with the control endpoint over TCP open for this run, whatever the
    /// stored `control_tcp.access_mode` says: privileged requests then need no
    /// signature. For development and tests; without it the endpoint is
    /// restricted by default and wants a signature of the administrator key.
    #[cfg(feature = "std-tcp")]
    #[arg(long)]
    pub control_open: bool,

    /// Port of the development-management interface (fault injection and
    /// scenario control); 0 picks a free port. Defaults to the control port
    /// plus one, or a free port when the control port is 0.
    #[arg(long, value_name = "PORT")]
    pub manage_port: Option<u16>,

    /// The Bluetooth adapter the BLE peripheral uses, for example `hci0`.
    /// Defaults to the default adapter of BlueZ.
    #[cfg(feature = "std-ble")]
    #[arg(long, value_name = "ADAPTER")]
    pub ble_adapter: Option<String>,

    /// Configuration flash image. It survives restarts, and so does the
    /// persistent log, which is kept next to it in `<name>.log.flash`.
    #[arg(
        long,
        default_value = ".altruist-urban/device.flash",
        value_name = "FILE",
        conflicts_with = "ephemeral"
    )]
    pub storage: PathBuf,

    /// Keep the configuration flash and the persistent log in memory only.
    #[arg(long)]
    pub ephemeral: bool,

    /// Scenario file (TOML) with the initial state and a timeline of events.
    #[arg(long, value_name = "FILE")]
    pub scenario: Option<PathBuf>,

    /// Seed of every random sequence of the simulation: sensor jitter and the
    /// nonces of the encrypted telemetry sections and envelopes. The same scenario, seed and
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
    /// The control port of the TCP transport; `stored` is the one of the
    /// `control_tcp` section.
    #[cfg(feature = "std-tcp")]
    fn control(&self, stored: u16) -> u16 {
        self.control_port.unwrap_or(stored)
    }

    #[cfg(not(feature = "std-tcp"))]
    fn control(&self, _stored: u16) -> u16 {
        0
    }

    /// The address of the management listener, and of the control listener when
    /// `stored` (the `control_tcp.bind_address`) is not asked for.
    pub fn management_address(&self) -> IpAddr {
        self.bind.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    /// The address of the control listener.
    pub fn control_address(&self, stored: Ipv4Addr) -> IpAddr {
        self.bind.unwrap_or(IpAddr::V4(stored))
    }

    /// The control and management ports to bind; `stored` is the port of the
    /// `control_tcp` section.
    pub fn ports(&self, stored: u16) -> (u16, u16) {
        if let Some([control, manage]) = self.reboot_ports.as_deref() {
            return (*control, *manage);
        }
        let control = self.control(stored);
        let manage = self.manage_port.unwrap_or(match control {
            0 => 0,
            port => port.wrapping_add(1),
        });
        (control, manage)
    }

    /// The file of the persistent log flash: `device.flash` is paired with
    /// `device.log.flash`.
    pub fn log_storage(&self) -> PathBuf {
        self.storage.with_extension("log.flash")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_defaults_to_the_ignored_device_directory() {
        let args = Args::try_parse_from(["altruist-urban"]).unwrap();
        assert_eq!(args.storage, PathBuf::from(".altruist-urban/device.flash"));
        assert_eq!(
            args.log_storage(),
            PathBuf::from(".altruist-urban/device.log.flash")
        );
    }
}
