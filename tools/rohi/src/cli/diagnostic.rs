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
//! Errors of the command line and their presentation: what is printed on
//! stderr and the exit status.

use std::fmt;
use std::io;
use std::process::ExitCode;

use crate::model::ErrorCode;
use crate::{DiscoveredDevice, Error};

/// Exit status of an interrupted command (128 + SIGINT).
pub const EXIT_INTERRUPTED: u8 = 130;

/// Why a command failed. The typed library error stays intact in
/// [`CliError::Device`]; everything else is a failure of the command line
/// layer itself.
#[derive(Debug)]
pub enum CliError {
    /// The device, the link or the library failed.
    Device(Error),
    /// A command line value is not usable.
    Usage(String),
    /// The target needs a transport this version of `rohi` does not have.
    UnsupportedTransport(String),
    /// The `--device` selector matches no device that was found.
    NoMatch {
        /// What the user asked for.
        selector: String,
        /// What was found.
        found: Vec<DiscoveredDevice>,
    },
    /// More than one device fits and none can be chosen.
    Ambiguous {
        /// The selector, if one was given.
        selector: Option<String>,
        /// The candidates.
        devices: Vec<DiscoveredDevice>,
    },
    /// A destructive command needs confirmation and cannot ask.
    ConfirmationRequired,
    /// The user declined or abandoned a prompt.
    Cancelled,
    /// The user pressed Ctrl-C.
    Interrupted,
    /// A secret is needed and there is no way to ask for it.
    PasswordRequired {
        /// What the password is for.
        what: String,
        /// How to provide it.
        hint: String,
    },
    /// A configuration file cannot be used.
    File(String),
    /// Applying a configuration file stopped half way.
    PartialApply(Box<PartialApply>),
    /// Writing the result failed.
    Output(io::Error),
}

/// A configuration file that was applied only in part.
#[derive(Debug)]
pub struct PartialApply {
    /// Sections stored before the failure.
    pub applied: Vec<String>,
    /// The section that failed.
    pub failed: String,
    /// Sections that were not attempted.
    pub pending: Vec<String>,
    /// The failure.
    pub source: Error,
}

/// What is shown to the user.
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// One line, without the `error:` prefix.
    pub message: String,
    /// Lines shown below the message.
    pub details: Vec<String>,
    /// Suggested next steps.
    pub hints: Vec<String>,
}

impl Diagnostic {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: Vec::new(),
            hints: Vec::new(),
        }
    }

    pub(super) fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }

    pub(super) fn details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {}", self.message)?;
        if !self.details.is_empty() {
            write!(f, "\n\n{}", self.details.join("\n"))?;
        }
        if !self.hints.is_empty() {
            f.write_str("\n")?;
            for hint in &self.hints {
                write!(f, "\nhint: {hint}")?;
            }
        }
        Ok(())
    }
}

impl From<Error> for CliError {
    fn from(error: Error) -> Self {
        Self::Device(error)
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Output(error)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic().message)
    }
}

impl std::error::Error for CliError {}

fn device_list(devices: &[DiscoveredDevice]) -> Vec<String> {
    devices
        .iter()
        .enumerate()
        .map(|(index, device)| format!("  {}  {}", index + 1, describe(device)))
        .collect()
}

/// One line for a discovered device: name, short ID and signal.
pub fn describe(device: &DiscoveredDevice) -> String {
    let mut line = match &device.name {
        Some(name) => format!("{name}  {}", device.id),
        None => device.id.clone(),
    };
    if let Some(rssi) = device.rssi {
        line.push_str(&format!("  {rssi} dBm"));
    }
    line
}

impl CliError {
    /// Whether this is the reader of a pipeline hanging up, which is not an
    /// error worth a message.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Output(error) if error.kind() == io::ErrorKind::BrokenPipe)
    }

    /// Process exit status: 1 for failures, 130 when interrupted. A closed
    /// pipe is a normal way for a pipeline to end.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            _ if self.is_broken_pipe() => ExitCode::SUCCESS,
            Self::Interrupted => ExitCode::from(EXIT_INTERRUPTED),
            Self::Usage(_) => ExitCode::from(2),
            _ => ExitCode::FAILURE,
        }
    }

    /// The message, details and hints for the user.
    pub fn diagnostic(&self) -> Diagnostic {
        match self {
            Self::Device(error) => device_diagnostic(error),
            Self::Usage(message) => Diagnostic::new(message.clone()),
            Self::UnsupportedTransport(scheme) => {
                Diagnostic::new(format!("the {scheme} transport is not supported yet"))
                    .hint("use a Bluetooth LE device: --device ble://<NAME>")
            }
            Self::NoMatch { selector, found } => {
                let mut diagnostic =
                    Diagnostic::new(format!("no ROHI device matches `{selector}`"));
                if found.is_empty() {
                    diagnostic = diagnostic
                        .hint("make sure Bluetooth is enabled and the device is advertising");
                } else {
                    diagnostic = diagnostic.details(
                        std::iter::once("found instead:".to_owned())
                            .chain(device_list(found))
                            .collect(),
                    );
                }
                diagnostic.hint("run `rohi scan` to list available devices")
            }
            Self::Ambiguous { selector, devices } => {
                let message = match selector {
                    Some(selector) => format!("`{selector}` matches more than one ROHI device"),
                    None => "multiple ROHI devices found".to_owned(),
                };
                let mut hints = vec!["select one with --device <DEVICE>".to_owned()];
                hints.push("run `rohi scan` to list available devices".to_owned());
                Diagnostic {
                    message,
                    details: device_list(devices),
                    hints,
                }
            }
            Self::ConfirmationRequired => {
                Diagnostic::new("confirmation required in non-interactive mode")
                    .hint("pass --yes to confirm")
            }
            Self::Cancelled => Diagnostic::new("cancelled"),
            Self::Interrupted => Diagnostic::new("interrupted"),
            Self::PasswordRequired { what, hint } => {
                Diagnostic::new(format!("{what} is required")).hint(hint.clone())
            }
            Self::File(message) => Diagnostic::new(message.clone()),
            Self::PartialApply(partial) => {
                let PartialApply {
                    applied,
                    failed,
                    pending,
                    source,
                } = &**partial;
                let mut details = vec![format!("failed: {failed}")];
                let section_list = |title: &str, sections: &[String]| {
                    let mut lines = vec![format!("{title}:")];
                    if sections.is_empty() {
                        lines.push("  none".to_owned());
                    }
                    lines.extend(sections.iter().map(|section| format!("  {section}")));
                    lines
                };
                details.extend(section_list("applied", applied));
                details.extend(section_list("not applied", pending));
                let cause = device_diagnostic(source);
                let mut diagnostic = Diagnostic::new(format!(
                    "applying stopped after a failure: {}",
                    cause.message
                ))
                .details(details)
                .hint("applying is atomic per section only: the device keeps the sections listed as applied");
                diagnostic.hints.extend(cause.hints);
                diagnostic
            }
            Self::Output(error) => Diagnostic::new(format!("cannot write the result: {error}")),
        }
    }
}

fn device_diagnostic(error: &Error) -> Diagnostic {
    match error {
        Error::DeviceNotFound => Diagnostic::new("no ROHI device found")
            .hint("make sure Bluetooth is enabled and the device is advertising")
            .hint("try `rohi scan`"),
        Error::Transport(message) => Diagnostic::new(format!("Bluetooth: {message}"))
            .hint("make sure Bluetooth is enabled and the device is in range")
            .hint("a device accepts a single connection: close other apps connected to it"),
        Error::Timeout => Diagnostic::new("timed out waiting for the device")
            .hint("make sure the device is in range and no other client is connected to it")
            .hint("a slow device may need a longer --timeout <SECONDS>"),
        Error::Device(code) => {
            let diagnostic = Diagnostic::new(format!("the device refused the request: {code}"));
            match code {
                ErrorCode::Busy => diagnostic.hint("try again in a moment"),
                ErrorCode::Conflict => {
                    diagnostic.hint("the configuration changed meanwhile; read it again")
                }
                ErrorCode::PermissionDenied => {
                    diagnostic.hint("this change needs an encrypted or authenticated link")
                }
                _ => diagnostic,
            }
        }
        Error::Rejected(reason) => {
            let diagnostic = Diagnostic::new(format!("the device rejected the action: {reason}"));
            match reason.as_str() {
                "already_provisioned" => diagnostic
                    .hint("run `rohi action run identity erase` first to provision a new identity"),
                "not_configured" => {
                    diagnostic.hint("configure it first: `rohi config set <section> ...`")
                }
                _ => diagnostic,
            }
        }
        other => Diagnostic::new(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendering_has_the_message_details_and_hints() {
        let rendered = CliError::PartialApply(Box::new(PartialApply {
            applied: vec!["wifi".into()],
            failed: "connectivity".into(),
            pending: vec![],
            source: Error::Device(ErrorCode::InvalidArgument),
        }))
        .diagnostic()
        .to_string();
        assert!(
            rendered.starts_with("error: applying stopped after a failure"),
            "{rendered}"
        );
        assert!(rendered.contains("failed: connectivity"), "{rendered}");
        assert!(rendered.contains("applied:\n  wifi"), "{rendered}");
    }

    #[test]
    fn a_missing_device_comes_with_hints() {
        let rendered = CliError::from(Error::DeviceNotFound)
            .diagnostic()
            .to_string();
        assert_eq!(
            rendered,
            "error: no ROHI device found\n\n\
             hint: make sure Bluetooth is enabled and the device is advertising\n\
             hint: try `rohi scan`"
        );
    }

    #[test]
    fn exit_codes() {
        assert_eq!(CliError::Interrupted.exit_code(), ExitCode::from(130));
        assert_eq!(
            CliError::Usage(String::new()).exit_code(),
            ExitCode::from(2)
        );
        assert_eq!(CliError::Cancelled.exit_code(), ExitCode::FAILURE);
        let pipe = CliError::Output(io::ErrorKind::BrokenPipe.into());
        assert!(pipe.is_broken_pipe());
        assert_eq!(pipe.exit_code(), ExitCode::SUCCESS);
    }
}
