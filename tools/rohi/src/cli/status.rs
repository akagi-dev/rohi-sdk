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
//! `rohi status`: one snapshot of the device assembled from the typed API.

use core::fmt::Write;

use super::diagnostic::CliError;
use super::dto;
use super::{Ctx, identity};
use crate::model::*;
use crate::{ClientTransport, Device, Error};

/// What the device told, section by section.
pub struct Snapshot {
    pub uri: Option<String>,
    pub info: DeviceInfo,
    pub capabilities: Vec<Capability>,
    pub wifi: Option<WifiStatus>,
    pub identity: Option<IdentityInfo>,
}

/// An optional section: a device that does not offer it, or answers that it
/// cannot, simply has no such section. Link and protocol failures still fail
/// the whole command.
fn optional<T>(result: Result<T, Error>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Error::Unsupported(_) | Error::Device(ErrorCode::NotSupported)) => Ok(None),
        Err(error) => Err(error),
    }
}

pub async fn collect<T: ClientTransport>(
    device: &mut Device<T>,
    uri: Option<String>,
) -> Result<Snapshot, Error> {
    let info = device.info().await?;
    let capabilities = device.capabilities().await?;
    let wifi = if capabilities.contains(&Capability::WifiStatus) {
        optional(device.wifi_status().await)?
    } else {
        None
    };
    let identity = if capabilities.contains(&Capability::Identity) {
        optional(device.identity().await)?
    } else {
        None
    };
    Ok(Snapshot {
        uri,
        info,
        capabilities,
        wifi,
        identity,
    })
}

fn section(out: &mut String, title: &str, lines: &[(&str, String)]) {
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(out, "{title}");
    for (name, value) in lines {
        let _ = writeln!(out, "  {name:<12}{value}");
    }
}

/// Lines of a Wi-Fi status for the human output.
pub fn wifi_lines(status: &WifiStatus) -> Vec<(&'static str, String)> {
    let mut lines = vec![("State", status.state.to_string())];
    if let Some(failure) = status.failure {
        lines.push(("Failure", failure.to_string()));
    }
    if let Some(connection) = &status.connection {
        lines.push(("Network", connection.ssid.to_text()));
        lines.push(("Signal", format!("{} dBm", connection.rssi_dbm)));
        lines.push(("Address", connection.ipv4.to_string()));
        if let Some(gateway) = connection.gateway {
            lines.push(("Gateway", gateway.to_string()));
        }
        if let Some(dns) = connection.dns {
            lines.push(("DNS", dns.to_string()));
        }
    }
    lines
}

pub fn human(snapshot: &Snapshot) -> String {
    let mut out = String::new();
    section(
        &mut out,
        "Device",
        &[
            ("Firmware", snapshot.info.firmware.clone()),
            ("Board", snapshot.info.board.clone()),
            ("URI", snapshot.uri.clone().unwrap_or("NONE".into())),
        ],
    );
    if let Some(wifi) = &snapshot.wifi {
        section(&mut out, "Wi-Fi", &wifi_lines(wifi));
    }
    if let Some(identity) = &snapshot.identity {
        let lines = identity::lines(identity);
        section(&mut out, "Identity", &lines);
    }
    out
}

pub fn json(snapshot: &Snapshot) -> dto::Versioned<dto::Status> {
    dto::Versioned::new(dto::Status {
        connection: snapshot.uri.clone().map(|uri| dto::Connection { uri }),
        device: dto::Device {
            firmware: snapshot.info.firmware.clone(),
            board: snapshot.info.board.clone(),
        },
        capabilities: snapshot
            .capabilities
            .iter()
            .map(dto::capability_id)
            .collect(),
        wifi: snapshot.wifi.as_ref().map(dto::Wifi::new),
        identity: snapshot.identity.as_ref().map(dto::Identity::new),
    })
}

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let snapshot = collect(device, ctx.uri.clone()).await?;
    ctx.io
        .emit(ctx.format, &json(&snapshot), || human(&snapshot))
}

/// The `Wi-Fi` section alone, for `rohi wifi status`.
pub fn wifi_text(status: &WifiStatus) -> String {
    let mut out = String::new();
    section(&mut out, "Wi-Fi", &wifi_lines(status));
    out
}
