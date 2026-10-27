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
  rohi status wifi
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
    /// The configuration is a set of sections of typed fields that the device
    /// describes; values are parsed and checked against that description.
    ///
    ///   rohi config                       list the sections
    ///   rohi config SECTION               show a section
    ///   rohi config SECTION FIELD=VALUE   change fields of a section
    ///   rohi config SECTION --help        describe the fields of a section
    ///
    /// What a section shows is its effective configuration: the defaults of
    /// the firmware with your overrides applied. `FIELD=VALUE` sets an
    /// override, `FIELD=null` removes it so that the field follows the
    /// default again. A field you never set keeps following the defaults of
    /// the firmware, also after an update. Changes are patches applied
    /// atomically per section: if anything is invalid, nothing is stored.
    /// After a change the section is read again and shown as the device
    /// reports it.
    ///
    /// A section can be inactive until it is set up: a section with the
    /// defaults only (nothing overridden) is not validated, but the result of
    /// every change is, so the fields that belong together are set in one
    /// command.
    ///
    /// Secret fields are never given on the command line: use `FIELD=-` to
    /// read the value from stdin, or to be asked for it on a terminal. They
    /// are write-only and are never shown or exported. Bytes are given as
    /// hex.
    ///
    /// SECTION must not be one of the names of the commands below.
    #[command(
        verbatim_doc_comment,
        args_conflicts_with_subcommands = true,
        disable_help_flag = true,
        disable_help_subcommand = true,
        after_help = "\
Examples:
  rohi config
  rohi config wifi
  rohi config wifi ssid=office password=-
  rohi config wifi scan_timeout=5000
  rohi config wifi scan_timeout=null
  rohi config wifi scan_timeout=5000 --dry-run
  rohi config wifi --help
  echo 'hunter2hunter2' | rohi config wifi password=-"
    )]
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
        /// The section to show or change.
        #[arg(value_name = "SECTION")]
        section: Option<String>,
        /// Fields to set: `FIELD=VALUE` sets an override, `FIELD=null`
        /// removes it.
        #[arg(value_name = "FIELD=VALUE")]
        assignments: Vec<String>,
        /// Validate on the device without storing anything.
        #[arg(long)]
        dry_run: bool,
        /// Print help; for a section, describe its fields.
        #[arg(short = 'h', long = "help")]
        help: bool,
    },
    /// Format the configuration storage of the device.
    ///
    /// Wipes the whole configuration partition of the device: every section,
    /// the device identity (the device becomes unprovisioned) and anything
    /// else stored there, including the section versions and the damaged
    /// storage of a corrupted device, which is repaired this way. The runtime
    /// returns to the defaults. A confirmation prompt is shown unless --yes is
    /// passed.
    ///
    /// To restore the defaults of a single section use `rohi config reset`.
    #[command(after_help = "\
Examples:
  rohi factory-reset
  rohi factory-reset --yes")]
    FactoryReset {
        /// Skip the confirmation prompt.
        #[arg(long, short)]
        yes: bool,
    },
    /// Reboot the device.
    ///
    /// The device answers first and reboots shortly after, so the connection
    /// is lost. Devices that cannot reboot report it as not supported.
    #[command(after_help = "\
Examples:
  rohi reboot")]
    Reboot,
    /// List and run the actions of the services.
    ///
    /// Actions are explicit operations a service offers, for example scanning
    /// for networks or reconnecting. The device describes the arguments and
    /// the results of every action.
    ///
    ///   rohi action                            list all actions
    ///   rohi action SERVICE                    list the actions of a service
    ///   rohi action SERVICE ACTION [ARG=VALUE] run an action
    ///   rohi action SERVICE ACTION --help      describe an action
    ///
    /// Arguments are `NAME=VALUE` pairs and all are required. Use `NAME=-` to
    /// read a value from stdin, or to be asked for it on a terminal, so that
    /// it stays out of the shell history; actions with secret arguments accept
    /// nothing else. Bytes are given as hex. The results are printed as a
    /// table, one row per record.
    #[command(
        verbatim_doc_comment,
        disable_help_flag = true,
        after_help = "\
Examples:
  rohi action
  rohi action wifi
  rohi action wifi scan
  rohi action wifi connect ssid=- password=-
  rohi action wifi reconnect
  rohi action wifi connect --help
  rohi action identity provision secret=-
  rohi -o json action wifi scan"
    )]
    Action {
        /// The service whose actions to list or run.
        #[arg(value_name = "SERVICE")]
        service: Option<String>,
        /// The action to run.
        #[arg(value_name = "ACTION")]
        action: Option<String>,
        /// `NAME=VALUE` pairs.
        #[arg(value_name = "ARG=VALUE")]
        args: Vec<String>,
        /// Print help; for an action, describe its arguments and results.
        #[arg(short = 'h', long = "help")]
        help: bool,
    },
}

/// The fixed commands of `rohi config`; any other name is a section.
#[derive(Subcommand)]
pub enum ConfigAction {
    /// Remove every override of a section: it follows the defaults again.
    ///
    /// The effective configuration of the section is the defaults of the
    /// firmware afterwards. To remove a single override use
    /// `rohi config SECTION FIELD=null`. A confirmation prompt is shown unless
    /// --yes is passed.
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
    /// Write your overrides to a JSON file.
    ///
    /// The file maps section names to the fields you set explicitly, not to
    /// the effective configuration: fields that follow the defaults are left
    /// out, so applying the file elsewhere or later does not pin the defaults
    /// of today. Sections without overrides, secrets and read-only fields are
    /// left out as well. Without a file, or with `-`, the document goes to
    /// stdout.
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
    /// Compares the file with your overrides on the device and prints the
    /// differences; nothing is changed. Secrets cannot be compared and are not
    /// part of files.
    #[command(after_help = "\
Examples:
  rohi config diff config.json")]
    Diff {
        /// The JSON file; `-` reads stdin.
        file: PathBuf,
    },
    /// Apply a configuration file of overrides.
    ///
    /// A field with a value sets an override, a field set to `null` removes
    /// it (the field follows the default again), a field that is missing in
    /// the file is left unchanged, and a section set to `null` is reset. Every
    /// section is validated on the device before the first one is changed,
    /// and each section is applied atomically. Applying several sections is
    /// not atomic as a whole: if the device fails in between, the sections
    /// already applied are listed.
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
