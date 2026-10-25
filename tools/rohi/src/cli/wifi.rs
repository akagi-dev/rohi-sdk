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
//! `rohi wifi`.

use std::time::{Duration, Instant};

use super::args::{Configure, Connect, WifiAction};
use super::diagnostic::CliError;
use super::{Ctx, dto, status};
use crate::model::{PasswordChange, WifiState, WifiStatus, WifiUpdate};
use crate::{ClientTransport, Device, format};

const WIFI_PASSWORD_ENV: &str = "ROHI_WIFI_PASSWORD";

/// The password for `ssid` from the first source that has one: stdin when
/// asked for, the environment, the hidden prompt. An empty answer to the
/// prompt keeps the stored password.
pub async fn obtain_password(
    ctx: &mut Ctx,
    ssid: Option<&str>,
    from_stdin: bool,
    keep: bool,
) -> Result<PasswordChange, CliError> {
    let what = match ssid {
        Some(ssid) => format!("the Wi-Fi password for {ssid}"),
        None => "the Wi-Fi password".to_owned(),
    };
    if from_stdin {
        let password = ctx.io.secret_from_stdin(&what).await?;
        return Ok(PasswordChange::Set(password.as_bytes().to_vec()));
    }
    if let Some(password) = ctx.wifi_password.take() {
        return Ok(PasswordChange::Set(password.into_bytes()));
    }
    if !ctx.io.interactive() {
        return Err(CliError::PasswordRequired {
            what,
            hint: format!(
                "pass --open for a network without a password, set {WIFI_PASSWORD_ENV}, \
                 or pipe it in with --password-stdin"
            ),
        });
    }
    let hint = if keep {
        " (empty to keep the stored one)"
    } else {
        ""
    };
    let prompt = match ssid {
        Some(ssid) => format!("Password for {ssid}{hint}: "),
        None => format!("New password{hint}: "),
    };
    let password = ctx.io.hidden(&prompt).await?;
    if password.is_empty() && !keep {
        return Err(CliError::PasswordRequired {
            what,
            hint: "enter a password, or pass --open for a network without one".to_owned(),
        });
    }
    Ok(if password.is_empty() {
        PasswordChange::Keep
    } else {
        PasswordChange::Set(password.as_bytes().to_vec())
    })
}

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: WifiAction,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        WifiAction::Status => {
            let wifi = device.wifi_status().await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::Wifi::new(&wifi)),
                || status::wifi_text(&wifi),
            )?;
        }
        WifiAction::Scan => {
            let networks = device.wifi_scan().await?;
            ctx.io.emit(
                ctx.format,
                &dto::Versioned::new(dto::Networks::new(&networks)),
                || format::wifi_scan(&networks),
            )?;
        }
        WifiAction::Connect(connect) => self::connect(device, connect, ctx).await?,
        WifiAction::Configure(configure) => self::configure(device, configure, ctx).await?,
        WifiAction::Reconnect => {
            device.wifi_reconnect().await?;
            ctx.io
                .emit(ctx.format, &dto::done(), || "Reconnecting\n".to_owned())?;
        }
        WifiAction::Disconnect => {
            device.wifi_disconnect().await?;
            ctx.io
                .emit(ctx.format, &dto::done(), || "Disconnected\n".to_owned())?;
        }
        WifiAction::Forget { yes } => {
            ctx.io
                .confirm("Remove the stored Wi-Fi configuration?", yes)
                .await?;
            device.forget_wifi().await?;
            ctx.io.emit(ctx.format, &dto::done(), || {
                "Wi-Fi configuration removed\n".to_owned()
            })?;
        }
    }
    Ok(())
}

async fn configure<T: ClientTransport>(
    device: &mut Device<T>,
    configure: Configure,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let password = if configure.open {
        PasswordChange::Clear
    } else if let Some(password) = configure.password {
        PasswordChange::Set(password.into_bytes())
    } else {
        obtain_password(ctx, configure.ssid.as_deref(), false, true).await?
    };
    let update = WifiUpdate {
        ssid: configure.ssid.map(String::into_bytes),
        password,
    };
    device.update_wifi(&update).await?;
    ctx.io.emit(ctx.format, &dto::done(), || {
        "Wi-Fi configuration stored. Follow `rohi wifi status` to see the connection.\n".to_owned()
    })
}

/// Why the attempt is over without a connection, if it is.
fn failed(status: &WifiStatus) -> Option<String> {
    let reason = status.failure.map(|failure| failure.to_string());
    match (status.state, reason) {
        (WifiState::Error, Some(reason)) => Some(format!("Wi-Fi failed: {reason}")),
        (WifiState::Error, None) => Some("Wi-Fi failed".to_owned()),
        (WifiState::Disconnected, Some(reason)) => Some(format!("Wi-Fi failed: {reason}")),
        _ => None,
    }
}

async fn connect<T: ClientTransport>(
    device: &mut Device<T>,
    connect: Connect,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let password = if connect.open {
        PasswordChange::Clear
    } else {
        obtain_password(ctx, Some(&connect.ssid), connect.password_stdin, true).await?
    };
    let update = WifiUpdate {
        ssid: Some(connect.ssid.clone().into_bytes()),
        password,
    };
    device.update_wifi(&update).await?;

    if connect.no_wait {
        return ctx.io.emit(
            ctx.format,
            &dto::Versioned::new(dto::Connected {
                ok: true,
                stored: true,
                connected: false,
                wifi: None,
            }),
            || {
                "Wi-Fi configuration stored. Follow `rohi wifi status` to see the connection.\n"
                    .to_owned()
            },
        );
    }

    ctx.io.note(&format!("Connecting to {}...\n", connect.ssid));
    let deadline = Instant::now() + Duration::from_secs(connect.wait);
    let mut attempted = false;
    let mut polls = 0u32;
    let status = loop {
        let status = device.wifi_status().await?;
        polls += 1;
        let ours = status
            .connection
            .as_ref()
            .is_some_and(|c| c.ssid.to_text() == connect.ssid);
        if status.state == WifiState::Connected && ours {
            break status;
        }
        attempted |= matches!(status.state, WifiState::Connecting | WifiState::Scanning);
        // The first answers can still describe the previous attempt.
        if (attempted || polls > 2)
            && let Some(reason) = failed(&status)
        {
            return Err(CliError::WifiNotConnected(reason));
        }
        if Instant::now() >= deadline {
            return Err(CliError::WifiNotConnected(format!(
                "{} did not connect within {} s (state: {})",
                connect.ssid, connect.wait, status.state
            )));
        }
        tokio::time::sleep(ctx.poll_interval).await;
    };

    let address = status
        .connection
        .as_ref()
        .map(|c| c.ipv4.to_string())
        .unwrap_or_default();
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::Connected {
            ok: true,
            stored: true,
            connected: true,
            wifi: Some(dto::Wifi::new(&status)),
        }),
        || format!("Connected\nAddress: {address}\n"),
    )
}
