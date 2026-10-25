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
//! `rohi config` and `rohi connectivity`.

use super::args::{ConfigSection, ConnectivityAction, SetConnectivity};
use super::diagnostic::CliError;
use super::{Ctx, dto};
use crate::model::{Capability, ConnectivityUpdate};
use crate::{ClientTransport, Device, Error, format};

/// Shows the stored configuration, all of it or one section.
///
/// The JSON output always has the same shape; only the text is narrowed to the
/// requested section.
pub async fn show<T: ClientTransport>(
    device: &mut Device<T>,
    section: Option<ConfigSection>,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let capabilities = device.capabilities().await?;
    let connectivity = capabilities.contains(&Capability::Connectivity);
    if matches!(section, Some(ConfigSection::Connectivity)) && !connectivity {
        return Err(Error::Unsupported(Capability::Connectivity).into());
    }
    let config = device.config().await?;
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::Config::new(&config)),
        || match section {
            Some(ConfigSection::Wifi) => format::wifi_config(&config),
            Some(ConfigSection::Connectivity) => format::connectivity_config(&config),
            None if connectivity => format::config(&config),
            None => format::wifi_config(&config),
        },
    )
}

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: ConnectivityAction,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        ConnectivityAction::Set(set) => {
            device.update_connectivity(&update(set)).await?;
            ctx.io.emit(ctx.format, &dto::done(), || {
                "Connectivity configuration stored and applied. \
                 Follow `rohi config connectivity` to see it.\n"
                    .to_owned()
            })
        }
    }
}

fn update(set: SetConnectivity) -> ConnectivityUpdate {
    ConnectivityUpdate {
        enabled: match (set.enable, set.disable) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        },
        node_id: set.node_id,
        interval_s: set.interval,
        host: set.host,
        port: set.port,
    }
}
