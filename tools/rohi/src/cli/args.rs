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
//!
//! The command tree knows no device module: `config`, `status` and `action`
//! take section, service and action names that the device lists.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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

/// When command results are colored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Color when stdout is a terminal, unless `NO_COLOR` is set or `TERM` is
    /// `dumb`.
    #[default]
    Auto,
    /// Always color.
    Always,
    /// Never color.
    Never,
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
        CLI discovers nearby ROHI devices, shows the status of their services,\n\
        manages their configuration and runs their actions. Sections, services\n\
        and actions are discovered from the device, so new firmware modules\n\
        need no update of this tool.",
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

    /// Color the text output.
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t,
        value_name = "WHEN",
        help_heading = OUTPUT
    )]
    pub color: ColorChoice,

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
    /// Show the runtime status of the device and its services.
    ///
    /// Without a service this is one compact request: the device health set by
    /// the firmware and the state, health and readiness of every service.
    /// With a service name it shows that service in detail: its
    /// service-specific values and the active issues, each with a stable
    /// code. Services are discovered from the device; `rohi status` lists
    /// them.
    ///
    /// Status is what is happening now. What the device is set up to do is
    /// `rohi config`.
    #[command(after_help = "\
Examples:
  rohi status
  rohi status network
  rohi status --watch
  rohi status connectivity --watch --interval 5
  rohi -o json status sensors")]
    Status {
        /// The service to show in detail.
        #[arg(value_name = "SERVICE")]
        service: Option<String>,
        /// Poll the device and print again whenever something changed, until
        /// interrupted. On a terminal the screen is refreshed in place instead.
        #[arg(short, long)]
        watch: bool,
        /// Pause between polls of --watch.
        #[arg(long, default_value_t = 2, value_name = "SECONDS", requires = "watch")]
        interval: u64,
    },
    /// Read and change the persistent configuration.
    ///
    /// The device lists its configuration sections and describes their
    /// fields; values are parsed and checked against that description. Changes
    /// are patches applied atomically per section: if anything is invalid,
    /// nothing is stored. Secrets are write-only and are never shown or
    /// exported.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// List and run the actions of the services.
    ///
    /// Actions are explicit operations a service offers, for example scanning
    /// for networks or reconnecting. The device describes the arguments and
    /// the results of every action.
    Action {
        #[command(subcommand)]
        action: ActionCommand,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// List the configuration sections of the device.
    #[command(after_help = "\
Examples:
  rohi config list
  rohi -o json config list")]
    List,
    /// Describe the fields of a section.
    ///
    /// Shows the type, access, constraints and description of every field.
    #[command(after_help = "\
Examples:
  rohi config describe connectivity")]
    Describe {
        /// The section.
        section: String,
    },
    /// Show the values of a section, or of one field.
    ///
    /// Secrets are shown as `<configured>`.
    #[command(after_help = "\
Examples:
  rohi config get connectivity
  rohi config get connectivity host
  rohi -o json config get network")]
    Get {
        /// The section.
        section: String,
        /// Show only this field.
        field: Option<String>,
    },
    /// Set fields of a section: `FIELD=VALUE` pairs, stored atomically.
    ///
    /// Fields that are not mentioned keep their value; a section that is not
    /// configured yet starts from its defaults, and the complete result is
    /// validated by the device. Secret fields are never given on the command
    /// line: use `FIELD=-` to read the value from stdin, or to be asked for
    /// it on a terminal. Bytes are given as hex.
    #[command(after_help = "\
Examples:
  rohi config set connectivity host=ingress.example.org port=443 enabled=true
  rohi config set connectivity interval_s=120 --dry-run
  rohi config set network ssid=office password=-
  echo 'hunter2hunter2' | rohi config set network password=-")]
    Set {
        /// The section.
        section: String,
        /// `FIELD=VALUE` pairs.
        #[arg(required = true, value_name = "FIELD=VALUE")]
        assignments: Vec<String>,
        /// Validate on the device without storing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Unset optional fields of a section.
    ///
    /// Unset is not the same as an empty string or zero: the field has no
    /// value afterwards.
    #[command(after_help = "\
Examples:
  rohi config unset network password")]
    Unset {
        /// The section.
        section: String,
        /// The fields to unset.
        #[arg(required = true)]
        fields: Vec<String>,
        /// Validate on the device without storing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the stored section and restore its defaults.
    ///
    /// A confirmation prompt is shown unless --yes is passed.
    #[command(after_help = "\
Examples:
  rohi config reset connectivity
  rohi config reset connectivity --yes")]
    Reset {
        /// The section.
        section: String,
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
        /// Validate on the device without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove every stored section and restore all defaults.
    ///
    /// Resets every configuration section the device lists: it is a
    /// `config reset` of each section. Every reset is validated on the device
    /// before the first section is changed. Actions and runtime state, such as
    /// the device identity, are not configuration and are not affected.
    /// A confirmation prompt is shown unless --yes is passed.
    #[command(after_help = "\
Examples:
  rohi config factory-reset
  rohi config factory-reset --yes
  rohi config factory-reset --dry-run")]
    FactoryReset {
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
        /// Validate on the device without removing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Write the configuration of the device to a JSON file.
    ///
    /// The file maps section names to their fields. Secrets and read-only
    /// fields are left out, so applying the file never touches them.
    /// Without a file, or with `-`, the document goes to stdout.
    #[command(after_help = "\
Examples:
  rohi config export config.json
  rohi config export > config.json")]
    Export {
        /// Output file.
        file: Option<PathBuf>,
    },
    /// Show what applying a configuration file would change.
    ///
    /// Compares the file with the device and prints the differences; nothing
    /// is changed. Secrets cannot be compared and are not part of files.
    #[command(after_help = "\
Examples:
  rohi config diff config.json")]
    Diff {
        /// The JSON file; `-` reads stdin.
        file: PathBuf,
    },
    /// Apply a configuration file.
    ///
    /// A field that is missing in the file is left unchanged, a field set to
    /// `null` is unset, and a section set to `null` is reset. Every section is
    /// validated on the device before the first one is changed, and each
    /// section is applied atomically. Applying several sections is not atomic
    /// as a whole: if the device fails in between, the sections already
    /// applied are listed.
    #[command(after_help = "\
Examples:
  rohi config apply config.json --dry-run
  rohi config apply config.json
  rohi config apply config.json --yes")]
    Apply {
        /// The JSON file; `-` reads stdin.
        file: PathBuf,
        /// Validate on the device without storing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum ActionCommand {
    /// List the actions of all services.
    #[command(after_help = "\
Examples:
  rohi action list")]
    List,
    /// Describe the arguments and results of an action.
    #[command(after_help = "\
Examples:
  rohi action describe network scan")]
    Describe {
        /// The service.
        service: String,
        /// The action.
        action: String,
    },
    /// Run an action.
    ///
    /// Arguments are `NAME=VALUE` pairs and all are required. Use `NAME=-` to
    /// read a value from stdin, or to be asked for it on a terminal, so that
    /// it stays out of the shell history; actions with secret arguments accept
    /// nothing else. Bytes are given as hex. The results are printed as a
    /// table, one row per record.
    #[command(after_help = "\
Examples:
  rohi action run network scan
  rohi action run network reconnect
  rohi action run identity provision secret=-
  rohi action run identity erase
  rohi -o json action run network scan")]
    Run {
        /// The service.
        service: String,
        /// The action.
        action: String,
        /// `NAME=VALUE` pairs.
        #[arg(value_name = "NAME=VALUE")]
        args: Vec<String>,
    },
}
