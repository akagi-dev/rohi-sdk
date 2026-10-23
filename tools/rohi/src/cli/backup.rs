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
//! `rohi backup`: export the stored configuration to a versioned file and
//! restore it, onto the same or another compatible device.
//!
//! The file format belongs to the command line, not to the library model: it
//! has its own `schema_version`, and distinguishes a value that is present,
//! one that is deliberately absent and a secret the device never reveals.
//! Passwords, identity keys and runtime state are never part of it.
//!
//! The control protocol has no multi-section update, so a restore is a
//! sequence of requests. Everything that can be checked is checked before the
//! first of them, and a failure in the middle reports exactly which sections
//! were stored.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use super::args::Import;
use super::diagnostic::{CliError, Diagnostic, PartialRestore};
use super::wifi::obtain_password;
use super::{Ctx, dto};
use crate::model::{Capability, Config, PasswordChange, WifiUpdate};
use crate::{ClientTransport, Device};

/// The only schema version this `rohi` reads and writes.
pub const SCHEMA_VERSION: u64 = 1;

/// Longest SSID the device accepts.
const MAX_SSID: usize = 32;

/// Whether a network has a password. The password itself is never saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Credential {
    /// A password is set; it cannot be read from the device and must be
    /// supplied when restoring.
    #[serde(rename = "required")]
    Required,
    /// The network is open.
    #[serde(rename = "none")]
    NotNeeded,
}

/// The Wi-Fi network of the device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiBackup {
    pub ssid: String,
    pub credential: Credential,
}

/// Configuration sections. A section that is missing was not part of the
/// backup; `null` says the device had nothing configured there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sections {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub wifi: Option<Option<WifiBackup>>,
    /// Sections this version of `rohi` does not know.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Where the backup came from, for diagnostics only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub firmware: String,
    pub board: String,
}

/// A configuration backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    pub schema_version: u64,
    pub source: Source,
    pub config: Sections,
}

/// Why a backup file cannot be used.
#[derive(Debug)]
pub enum BackupError {
    Read { path: PathBuf, error: io::Error },
    Write { path: PathBuf, error: io::Error },
    Malformed(String),
    Schema(u64),
    Invalid(String),
    UnknownSections(Vec<String>),
}

impl BackupError {
    pub fn diagnostic(&self) -> Diagnostic {
        match self {
            Self::Read { path, error } => {
                Diagnostic::new(format!("cannot read {}: {error}", path.display()))
            }
            Self::Write { path, error } => {
                Diagnostic::new(format!("cannot write {}: {error}", path.display()))
            }
            Self::Malformed(reason) => {
                Diagnostic::new(format!("the file is not a valid backup: {reason}"))
                    .hint("no changes were made")
            }
            Self::Schema(version) => Diagnostic::new(format!(
                "backup schema version {version} is not supported by this version of rohi"
            ))
            .hint(format!(
                "this version supports backup schema version {SCHEMA_VERSION}"
            )),
            Self::Invalid(reason) => Diagnostic::new(format!("the backup is not valid: {reason}"))
                .hint("no changes were made"),
            Self::UnknownSections(sections) => {
                Diagnostic::new("the backup has sections this version of rohi cannot restore")
                    .details(
                        std::iter::once("unsupported:".to_owned())
                            .chain(sections.iter().map(|section| format!("  {section}")))
                            .collect(),
                    )
                    .hint("no changes were made")
            }
        }
    }
}

impl Backup {
    /// Reads and validates a backup. The schema version is checked first, so
    /// a file from a newer `rohi` is reported as such rather than as garbage.
    pub fn parse(text: &str) -> Result<Self, BackupError> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| BackupError::Malformed(error.to_string()))?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| BackupError::Malformed("`schema_version` is missing".into()))?;
        if version != SCHEMA_VERSION {
            return Err(BackupError::Schema(version));
        }
        let backup: Self = serde_json::from_value(value)
            .map_err(|error| BackupError::Malformed(error.to_string()))?;
        backup.validate()?;
        Ok(backup)
    }

    fn validate(&self) -> Result<(), BackupError> {
        if !self.config.unknown.is_empty() {
            return Err(BackupError::UnknownSections(
                self.config.unknown.keys().cloned().collect(),
            ));
        }
        if let Some(Some(wifi)) = &self.config.wifi {
            if wifi.ssid.is_empty() {
                return Err(BackupError::Invalid("the Wi-Fi SSID is empty".into()));
            }
            if wifi.ssid.len() > MAX_SSID {
                return Err(BackupError::Invalid(format!(
                    "the Wi-Fi SSID is longer than {MAX_SSID} bytes"
                )));
            }
        }
        Ok(())
    }

    /// Reads the backup in `path`.
    pub fn load(path: &Path) -> Result<Self, BackupError> {
        let text = std::fs::read_to_string(path).map_err(|error| BackupError::Read {
            path: path.to_owned(),
            error,
        })?;
        Self::parse(&text)
    }

    /// The backup as pretty-printed JSON with a final newline.
    pub fn to_json(&self) -> Result<String, CliError> {
        let mut json = serde_json::to_string_pretty(self)
            .map_err(|error| CliError::Output(io::Error::other(error)))?;
        json.push('\n');
        Ok(json)
    }

    /// The backup of `config`, read from a device that reported `source`.
    pub fn of(source: Source, config: &Config) -> Self {
        let wifi = config.wifi.as_ref().map(|wifi| WifiBackup {
            ssid: wifi.ssid.to_text(),
            credential: if wifi.password_configured {
                Credential::Required
            } else {
                Credential::NotNeeded
            },
        });
        Self {
            schema_version: SCHEMA_VERSION,
            source,
            config: Sections {
                wifi: Some(wifi),
                unknown: BTreeMap::new(),
            },
        }
    }
}

/// What to do about a password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Password {
    /// It has to be supplied.
    Supply,
    /// The device has the right one already.
    Keep,
    /// The network is open: remove the stored one.
    Remove,
}

/// One section to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Store this network.
    SetWifi {
        from: Option<String>,
        ssid: String,
        password: Password,
    },
    /// Remove the stored network.
    ForgetWifi { from: String },
}

impl Change {
    /// Name of the section.
    pub fn section(&self) -> &'static str {
        match self {
            Self::SetWifi { .. } | Self::ForgetWifi { .. } => "wifi",
        }
    }

    /// Lines for the human report.
    pub fn lines(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::SetWifi {
                from,
                ssid,
                password,
            } => {
                let mut lines = Vec::new();
                if from.as_deref() != Some(ssid) {
                    lines.push((
                        "Wi-Fi SSID",
                        format!("{} → {ssid}", from.as_deref().unwrap_or("<unset>")),
                    ));
                }
                match password {
                    Password::Supply => lines.push(("Wi-Fi password", "required".to_owned())),
                    Password::Remove => lines.push(("Wi-Fi password", "removed".to_owned())),
                    Password::Keep => {}
                }
                lines
            }
            Self::ForgetWifi { from } => vec![("Wi-Fi SSID", format!("{from} → <unset>"))],
        }
    }
}

/// What restoring `backup` onto a device with `current` would change.
pub fn plan(backup: &Backup, current: &Config) -> Vec<Change> {
    let mut changes = Vec::new();
    let stored = current.wifi.as_ref();
    match &backup.config.wifi {
        None => {}
        Some(None) => {
            if let Some(stored) = stored {
                changes.push(Change::ForgetWifi {
                    from: stored.ssid.to_text(),
                });
            }
        }
        Some(Some(wifi)) => {
            let same = stored.filter(|stored| stored.ssid.to_text() == wifi.ssid);
            let has_password = same.is_some_and(|stored| stored.password_configured);
            let password = match wifi.credential {
                Credential::Required if has_password => Password::Keep,
                Credential::Required => Password::Supply,
                Credential::NotNeeded if has_password => Password::Remove,
                Credential::NotNeeded => Password::Keep,
            };
            if same.is_none() || password != Password::Keep {
                changes.push(Change::SetWifi {
                    from: stored.map(|stored| stored.ssid.to_text()),
                    ssid: wifi.ssid.clone(),
                    password,
                });
            }
        }
    }
    changes
}

fn report(changes: &[Change]) -> String {
    let mut out = String::new();
    for change in changes {
        for (name, value) in change.lines() {
            out.push_str(&format!("  {name:<18}{value}\n"));
        }
    }
    out
}

pub async fn export<T: ClientTransport>(
    device: &mut Device<T>,
    file: Option<PathBuf>,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let info = device.info().await?;
    let config = device.config().await?;
    let backup = Backup::of(
        Source {
            firmware: info.firmware,
            board: info.board,
        },
        &config,
    );
    let json = backup.to_json()?;
    match file {
        None => ctx.io.result(&json),
        Some(path) => {
            std::fs::write(&path, &json).map_err(|error| BackupError::Write {
                path: path.clone(),
                error,
            })?;
            let shown = path.display().to_string();
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::Written {
                    ok: true,
                    file: shown.clone(),
                }),
                || format!("Backup written to {shown}\n"),
            )
        }
    }
}

#[derive(Serialize)]
struct ChangeDto {
    section: &'static str,
    ssid_from: Option<String>,
    ssid_to: Option<String>,
    password: &'static str,
}

impl ChangeDto {
    fn new(change: &Change) -> Self {
        match change {
            Change::SetWifi {
                from,
                ssid,
                password,
            } => Self {
                section: change.section(),
                ssid_from: from.clone(),
                ssid_to: Some(ssid.clone()),
                password: match password {
                    Password::Supply => "required",
                    Password::Keep => "unchanged",
                    Password::Remove => "removed",
                },
            },
            Change::ForgetWifi { from } => Self {
                section: change.section(),
                ssid_from: Some(from.clone()),
                ssid_to: None,
                password: "removed",
            },
        }
    }
}

#[derive(Serialize)]
struct Plan {
    ok: bool,
    dry_run: bool,
    changes: Vec<ChangeDto>,
}

#[derive(Serialize)]
struct Restored {
    ok: bool,
    applied: Vec<&'static str>,
}

pub async fn import<T: ClientTransport>(
    device: &mut Device<T>,
    args: Import,
    backup: Backup,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    // Everything that can be checked, before the first request that changes
    // the device.
    let capabilities = device.capabilities().await?;
    if backup.config.wifi.is_some() && !capabilities.contains(&Capability::Config) {
        return Err(CliError::Incompatible(vec!["wifi"]));
    }
    let info = device.info().await?;
    let current = device.config().await?;
    let changes = plan(&backup, &current);

    let mut header = format!(
        "Backup:\n  Source firmware   {}\n  Source board      {}\n  Target firmware   {}\n  Target board      {}\n",
        backup.source.firmware, backup.source.board, info.firmware, info.board
    );
    header.push('\n');

    if changes.is_empty() {
        return ctx.io.emit(
            ctx.format,
            &dto::Versioned::new(Plan {
                ok: true,
                dry_run: args.dry_run,
                changes: Vec::new(),
            }),
            || "The device already matches the backup. No changes were made.\n".to_owned(),
        );
    }

    if args.dry_run {
        let plan = Plan {
            ok: true,
            dry_run: true,
            changes: changes.iter().map(ChangeDto::new).collect(),
        };
        return ctx.io.emit(ctx.format, &dto::Versioned::new(plan), || {
            format!(
                "{header}Would apply:\n{}\nNo changes were made.\n",
                report(&changes)
            )
        });
    }

    ctx.io
        .note(&format!("{header}Changes:\n{}\n", report(&changes)));

    let mut passwords = Vec::with_capacity(changes.len());
    for change in &changes {
        passwords.push(match change {
            Change::SetWifi {
                ssid,
                password: Password::Supply,
                ..
            } => obtain_password(ctx, Some(ssid), args.wifi_password_stdin, false).await?,
            Change::SetWifi {
                password: Password::Remove,
                ..
            } => PasswordChange::Clear,
            _ => PasswordChange::Keep,
        });
    }

    ctx.io
        .confirm("Apply this configuration?", args.yes)
        .await?;

    let mut applied: Vec<&'static str> = Vec::new();
    for (index, (change, password)) in changes.iter().zip(passwords).enumerate() {
        let result = match change {
            Change::SetWifi { ssid, .. } => {
                device
                    .update_wifi(&WifiUpdate {
                        ssid: Some(ssid.clone().into_bytes()),
                        password,
                    })
                    .await
            }
            Change::ForgetWifi { .. } => device.forget_wifi().await,
        };
        match result {
            Ok(()) => applied.push(change.section()),
            Err(error) if applied.is_empty() => return Err(error.into()),
            Err(error) => {
                return Err(CliError::PartialRestore(Box::new(PartialRestore {
                    applied,
                    failed: change.section(),
                    pending: changes[index + 1..].iter().map(Change::section).collect(),
                    source: error,
                })));
            }
        }
    }

    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(Restored {
            ok: true,
            applied: applied.clone(),
        }),
        || {
            "Configuration restored.\n\
                 The device stores the Wi-Fi configuration; follow it with `rohi wifi status`.\n"
                .to_owned()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ssid, WifiConfig};

    fn stored(ssid: &str, password: bool) -> Config {
        Config {
            wifi: Some(WifiConfig {
                ssid: Ssid(ssid.as_bytes().to_vec()),
                password_configured: password,
            }),
        }
    }

    fn backup(wifi: Option<Option<WifiBackup>>) -> Backup {
        Backup {
            schema_version: SCHEMA_VERSION,
            source: Source {
                firmware: "fw".into(),
                board: "board".into(),
            },
            config: Sections {
                wifi,
                unknown: BTreeMap::new(),
            },
        }
    }

    fn office(credential: Credential) -> Option<Option<WifiBackup>> {
        Some(Some(WifiBackup {
            ssid: "office".into(),
            credential,
        }))
    }

    #[test]
    fn the_backup_says_a_password_is_required_without_containing_one() {
        let json = Backup::of(
            Source {
                firmware: "altruist-urban-0.4.0".into(),
                board: "urban-esp32c3".into(),
            },
            &stored("office", true),
        )
        .to_json()
        .unwrap();
        assert_eq!(
            json,
            r#"{
  "schema_version": 1,
  "source": {
    "firmware": "altruist-urban-0.4.0",
    "board": "urban-esp32c3"
  },
  "config": {
    "wifi": {
      "ssid": "office",
      "credential": "required"
    }
  }
}
"#
        );
        assert!(!json.contains("password"));
    }

    #[test]
    fn an_open_network_and_no_network_are_different_things() {
        let open = Backup::of(backup(None).source, &stored("cafe", false));
        assert!(open.to_json().unwrap().contains(r#""credential": "none""#));

        let nothing = Backup::of(backup(None).source, &Config { wifi: None });
        let json = nothing.to_json().unwrap();
        assert!(json.contains(r#""wifi": null"#), "{json}");
        let again = Backup::parse(&json).unwrap();
        assert_eq!(again.config.wifi, Some(None));
    }

    #[test]
    fn backups_round_trip() {
        for wifi in [
            None,
            Some(None),
            office(Credential::Required),
            office(Credential::NotNeeded),
        ] {
            let original = backup(wifi);
            let parsed = Backup::parse(&original.to_json().unwrap()).unwrap();
            assert_eq!(parsed, original);
        }
    }

    #[test]
    fn other_schema_versions_are_rejected_before_anything_else() {
        let error = Backup::parse(r#"{"schema_version": 3, "whatever": [1, 2]}"#).unwrap_err();
        assert!(matches!(error, BackupError::Schema(3)));
        let rendered = error.diagnostic().to_string();
        assert!(
            rendered.contains("backup schema version 3 is not supported by this version of rohi")
        );
        assert!(rendered.contains("hint: this version supports backup schema version 1"));
    }

    #[test]
    fn malformed_backups_are_rejected() {
        for text in [
            "",
            "not json",
            "[]",
            r#"{"source": {}, "config": {}}"#,
            r#"{"schema_version": "1", "source": {"firmware": "a", "board": "b"}, "config": {}}"#,
            r#"{"schema_version": 1, "source": {"firmware": "a"}, "config": {}}"#,
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"wifi": {"ssid": "x"}}}"#,
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"wifi": {"ssid": "x", "credential": "maybe"}}}"#,
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {"wifi": {"ssid": "x", "credential": "none", "password": "hunter2"}}}"#,
            r#"{"schema_version": 1, "source": {"firmware": "a", "board": "b"}, "config": {}, "extra": 1}"#,
        ] {
            assert!(
                matches!(Backup::parse(text), Err(BackupError::Malformed(_))),
                "{text:?}"
            );
        }
    }

    #[test]
    fn invalid_values_and_unknown_sections_are_rejected() {
        let wrap = |config: &str| {
            format!(
                r#"{{"schema_version": 1, "source": {{"firmware": "a", "board": "b"}}, "config": {config}}}"#
            )
        };
        assert!(matches!(
            Backup::parse(&wrap(r#"{"wifi": {"ssid": "", "credential": "none"}}"#)),
            Err(BackupError::Invalid(_))
        ));
        let long = "x".repeat(33);
        assert!(matches!(
            Backup::parse(&wrap(&format!(
                r#"{{"wifi": {{"ssid": "{long}", "credential": "none"}}}}"#
            ))),
            Err(BackupError::Invalid(_))
        ));
        let error = Backup::parse(&wrap(r#"{"mqtt": {}, "location": {}}"#)).unwrap_err();
        let BackupError::UnknownSections(sections) = &error else {
            panic!("{error:?}");
        };
        assert_eq!(sections, &["location", "mqtt"]);
    }

    #[test]
    fn the_plan_is_the_difference_to_the_device() {
        // Nothing in the backup: nothing to do.
        assert!(plan(&backup(None), &stored("office", true)).is_empty());

        // Same network, password present on both: nothing to do.
        assert!(
            plan(
                &backup(office(Credential::Required)),
                &stored("office", true)
            )
            .is_empty()
        );
        assert!(
            plan(
                &backup(office(Credential::NotNeeded)),
                &stored("office", false)
            )
            .is_empty()
        );

        // A new network needs its password.
        assert_eq!(
            plan(
                &backup(office(Credential::Required)),
                &Config { wifi: None }
            ),
            [Change::SetWifi {
                from: None,
                ssid: "office".into(),
                password: Password::Supply
            }]
        );
        assert_eq!(
            plan(
                &backup(office(Credential::Required)),
                &stored("guests", true)
            ),
            [Change::SetWifi {
                from: Some("guests".into()),
                ssid: "office".into(),
                password: Password::Supply
            }]
        );
        // The same network without a password yet.
        assert_eq!(
            plan(
                &backup(office(Credential::Required)),
                &stored("office", false)
            ),
            [Change::SetWifi {
                from: Some("office".into()),
                ssid: "office".into(),
                password: Password::Supply
            }]
        );
        // The backup says open, the device has a password.
        assert_eq!(
            plan(
                &backup(office(Credential::NotNeeded)),
                &stored("office", true)
            ),
            [Change::SetWifi {
                from: Some("office".into()),
                ssid: "office".into(),
                password: Password::Remove
            }]
        );
        // An explicit "nothing configured".
        assert_eq!(
            plan(&backup(Some(None)), &stored("office", true)),
            [Change::ForgetWifi {
                from: "office".into()
            }]
        );
        assert!(plan(&backup(Some(None)), &Config { wifi: None }).is_empty());
    }
}
