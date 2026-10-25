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
//! Command line definition: the public interface of `rohi`, including its
//! `--help` texts. Help examples are parsed by the tests, so they cannot go
//! stale.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

const TARGET: &str = "Target";
const OUTPUT: &str = "Output";
const CONNECTION: &str = "Connection";

/// How command results are written to stdout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Text for people.
    #[default]
    Human,
    /// JSON with a stable, versioned schema, for scripts.
    Json,
}

#[derive(Parser)]
#[command(
    name = "rohi",
    version,
    about = "ROHI devices control and diagnostics",
    before_help = r#"
╔══════════════════════════════════════════════════════╗
║                                                      ║
║             ██████╗  ██████╗ ██╗  ██╗██╗             ║
║             ██╔══██╗██╔═══██╗██║  ██║██║             ║
║             ██████╔╝██║   ██║███████║██║             ║
║             ██╔══██╗██║   ██║██╔══██║██║             ║
║             ██║  ██║╚██████╔╝██║  ██║██║             ║
║             ╚═╝  ╚═╝ ╚═════╝ ╚═╝  ╚═╝╚═╝             ║
║                                                      ║
║      Robonomics Open Hardware terminal Interface     ║
║                                                      ║
╚══════════════════════════════════════════════════════╝
"#,
    long_about = "Control Robonomics devices from terminal.\n\n\
        CLI discovers nearby ROHI devices, reads their status and sensors,\n\
        manages device configuration, and provides backup/restore workflows.",
    after_help = "Run `rohi <COMMAND> --help` for more information about a command.",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Target device: advertised name, ID, or a unique prefix or suffix of
    /// either; or a URI such as ble://NAME or tcp://HOST:PORT. Without it the
    /// only device found is used, or you choose among several.
    #[arg(
        short,
        long,
        global = true,
        env = "ROHI_DEVICE",
        value_name = "DEVICE",
        help_heading = TARGET
    )]
    pub device: Option<String>,

    /// Output format.
    #[arg(
        short,
        long,
        global = true,
        value_enum,
        default_value_t,
        value_name = "FORMAT",
        help_heading = OUTPUT
    )]
    pub output: OutputFormat,

    /// How long to search for the target device.
    #[arg(
        long,
        global = true,
        default_value_t = 10,
        value_name = "SECONDS",
        help_heading = CONNECTION
    )]
    pub scan_timeout: u64,

    /// How long to wait for a device response.
    #[arg(
        long,
        global = true,
        default_value_t = 10,
        value_name = "SECONDS",
        help_heading = CONNECTION
    )]
    pub timeout: u64,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Discover nearby ROHI devices.
    ///
    /// Lists the devices advertising the ROHI control service with the
    /// platform ID and advertised name that `--device` accepts, and the signal
    /// strength.
    #[command(after_help = "\
Examples:
  rohi scan
  rohi scan --duration 10
  rohi -o json scan")]
    Scan {
        /// How long to scan.
        #[arg(long, default_value_t = 5, value_name = "SECONDS")]
        duration: u64,
    },
    /// Show device information, capabilities, and current status.
    ///
    /// Displays the connection, firmware and board information, supported
    /// capabilities, and available runtime status such as Wi-Fi connectivity,
    /// the wall-clock time and identity. Sections the device does not support
    /// are omitted.
    #[command(after_help = "\
Examples:
  rohi status
  rohi -d A12F status
  rohi -d A12F -o json status")]
    Status,
    /// Read current sensor measurements.
    #[command(after_help = "\
Examples:
  rohi sensors
  rohi -d A12F -o json sensors")]
    Sensors,
    /// Inspect device configuration.
    ///
    /// Shows the stored configuration. Secrets, such as the Wi-Fi password,
    /// are never shown.
    #[command(after_help = "\
Examples:
  rohi config
  rohi config wifi
  rohi config connectivity")]
    Config {
        #[command(subcommand)]
        section: Option<ConfigSection>,
    },
    /// Inspect and configure Wi-Fi.
    ///
    /// Without a command: show the Wi-Fi status.
    #[command(after_help = "\
Examples:
  rohi wifi
  rohi wifi scan
  rohi wifi connect office")]
    Wifi {
        #[command(subcommand)]
        action: Option<WifiAction>,
    },
    /// Configure telemetry delivery to the Robonomics ingress.
    ///
    /// Use `rohi config connectivity` to show the stored configuration.
    #[command(after_help = "\
Examples:
  rohi connectivity set --host ingress.example.org --node-id 42 --enable
  rohi connectivity set --interval 120
  rohi connectivity set --disable")]
    Connectivity {
        #[command(subcommand)]
        action: ConnectivityAction,
    },
    /// Show the wall-clock time of the device and its synchronization state.
    ///
    /// The device has no time until a source, such as NTP, delivered a
    /// plausible sample; it never reports its uptime or a placeholder date
    /// instead. The time may step when a later sample corrects it, and it is
    /// not authenticated.
    #[command(after_help = "\
Examples:
  rohi time
  rohi -d A12F -o json time")]
    Time,
    /// Export or restore device configuration.
    ///
    /// Backups contain persistent device settings that can be restored later
    /// or applied to another compatible ROHI device.
    ///
    /// Secrets that cannot be read back from the device are not included.
    /// They may be requested during restore.
    #[command(after_help = "\
Examples:
  rohi backup export backup.json
  rohi backup export > backup.json
  rohi backup import backup.json --dry-run
  rohi backup import backup.json")]
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
    /// Cryptographic identity of the device: show, provision or erase.
    ///
    /// Without a command: show the identity state and the public key as a
    /// Robonomics address.
    #[command(
        visible_alias = "id",
        after_help = "\
Examples:
  rohi identity
  rohi identity provision --number 42 --seed master.suri
  rohi identity erase --yes"
    )]
    Identity {
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
    #[command(after_help = "\
Examples:
  rohi identity provision --number 42 --seed master.suri
  rohi id provision -n 42 --seed master.suri")]
    Provision(Provision),
    /// Remove the identity; the device must be provisioned again to sign.
    ///
    /// A confirmation prompt is shown unless --yes is passed.
    #[command(after_help = "\
Examples:
  rohi identity erase
  rohi identity erase --yes")]
    Erase {
        /// Skip the confirmation prompt.
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
    #[arg(long, value_name = "FILE")]
    pub seed: PathBuf,
}

#[derive(Subcommand)]
pub enum ConfigSection {
    /// Show the Wi-Fi configuration.
    Wifi,
    /// Show the telemetry (connectivity) configuration.
    Connectivity,
}

#[derive(Subcommand)]
pub enum ConnectivityAction {
    /// Change the telemetry configuration; omitted settings are kept.
    ///
    /// The device stores the configuration and applies it at once, without a
    /// restart. While nothing is stored a host is required; the interval is
    /// 10 to 86400 seconds and the port defaults to 80. Telemetry is signed
    /// with the identity of the device, see `rohi identity`.
    #[command(after_help = "\
Examples:
  rohi connectivity set --host ingress.example.org --node-id 42 --enable
  rohi connectivity set --interval 120
  rohi connectivity set --disable")]
    Set(SetConnectivity),
}

#[derive(Args)]
#[command(group(
    ArgGroup::new("change")
        .required(true)
        .multiple(true)
        .args(["host", "port", "node_id", "interval", "enable", "disable"])
))]
pub struct SetConnectivity {
    /// Ingress host name or IPv4 address.
    #[arg(long, value_name = "HOST")]
    pub host: Option<String>,

    /// Ingress TCP port.
    #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,

    /// Robonomics CPS node identifier.
    #[arg(long, value_name = "ID")]
    pub node_id: Option<u64>,

    /// Seconds between telemetry frames.
    #[arg(long, value_name = "SECONDS")]
    pub interval: Option<u32>,

    /// Send telemetry.
    #[arg(long, conflicts_with = "disable")]
    pub enable: bool,

    /// Stop sending telemetry; the configuration is kept.
    #[arg(long)]
    pub disable: bool,
}

#[derive(Subcommand)]
pub enum WifiAction {
    /// Show the Wi-Fi status (default).
    #[command(after_help = "\
Examples:
  rohi wifi status
  rohi -o json wifi status")]
    Status,
    /// List networks the device sees.
    ///
    /// The scan runs on the device and can take half a minute.
    #[command(after_help = "\
Examples:
  rohi wifi scan")]
    Scan,
    /// Join a network and wait until the device is connected.
    ///
    /// Stores the network on the device, then follows the Wi-Fi status until
    /// the device is connected, reports a failure, or --wait expires. A stored
    /// configuration does not mean the network was reached.
    ///
    /// The password is read from a hidden prompt, from stdin with
    /// --password-stdin, or from the ROHI_WIFI_PASSWORD environment variable;
    /// an empty answer keeps the stored password. Use --open for a network
    /// without a password.
    #[command(after_help = "\
Examples:
  rohi wifi connect office
  rohi wifi connect guests --open
  rohi wifi connect office --password-stdin
  rohi wifi connect office --no-wait")]
    Connect(Connect),
    /// Change the Wi-Fi configuration; omitted settings are kept.
    ///
    /// Only stores the configuration; use `rohi wifi status` to follow the
    /// connection, or `rohi wifi connect` to wait for it.
    #[command(after_help = "\
Examples:
  rohi wifi configure --ssid office
  rohi wifi configure --ssid guests --open")]
    Configure(Configure),
    /// Reconnect now.
    #[command(after_help = "\
Examples:
  rohi wifi reconnect")]
    Reconnect,
    /// Disconnect and stay idle.
    #[command(after_help = "\
Examples:
  rohi wifi disconnect")]
    Disconnect,
    /// Remove the stored Wi-Fi configuration.
    ///
    /// This removes the configured SSID and credentials from the device.
    /// A confirmation prompt is shown unless --yes is passed.
    #[command(after_help = "\
Examples:
  rohi wifi forget
  rohi wifi forget --yes")]
    Forget {
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Args)]
pub struct Connect {
    /// Network name.
    #[arg(value_name = "SSID")]
    pub ssid: String,

    /// The network has no password; remove a stored one.
    #[arg(long, conflicts_with = "password_stdin")]
    pub open: bool,

    /// Read the password from the first line of stdin.
    #[arg(long)]
    pub password_stdin: bool,

    /// Return once the configuration is stored, without waiting for the
    /// connection.
    #[arg(long, conflicts_with = "wait")]
    pub no_wait: bool,

    /// How long to wait for the connection.
    #[arg(long, default_value_t = 30, value_name = "SECONDS")]
    pub wait: u64,
}

#[derive(Args)]
pub struct Configure {
    /// Network name.
    #[arg(long, value_name = "SSID")]
    pub ssid: Option<String>,

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
    pub password: Option<String>,

    /// The network has no password; remove a stored one.
    #[arg(long, conflicts_with = "password")]
    pub open: bool,
}

#[derive(Subcommand)]
pub enum BackupAction {
    /// Export device configuration.
    ///
    /// Writes a versioned JSON backup of the stored configuration to FILE, or
    /// to stdout without FILE. Runtime state and secrets the device does not
    /// reveal, such as the Wi-Fi password, are not included; the backup only
    /// records that a password is required.
    #[command(after_help = "\
Examples:
  rohi backup export backup.json
  rohi backup export > backup.json")]
    Export {
        /// File to write; stdout when omitted.
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
    },
    /// Validate and restore device configuration.
    ///
    /// The whole backup is validated, and compared with the device, before
    /// anything is changed. A backup that is malformed, has an unsupported
    /// schema version, or holds sections the device cannot restore is
    /// rejected without changes. Secrets missing from the backup are asked
    /// for. The restore is not atomic: if a section fails, the ones already
    /// applied are reported.
    #[command(after_help = "\
Examples:
  rohi backup import backup.json --dry-run
  rohi backup import backup.json
  rohi backup import backup.json --yes --wifi-password-stdin")]
    Import(Import),
}

#[derive(Args)]
pub struct Import {
    /// Backup file written by `rohi backup export`.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,

    /// Show the planned changes without modifying the device.
    #[arg(long, conflicts_with = "yes")]
    pub dry_run: bool,

    /// Skip the confirmation prompt.
    #[arg(long, short)]
    pub yes: bool,

    /// Read the Wi-Fi password from the first line of stdin, if the backup
    /// requires one. ROHI_WIFI_PASSWORD works too.
    #[arg(long)]
    pub wifi_password_stdin: bool,
}
