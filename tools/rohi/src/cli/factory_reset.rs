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
//! `rohi factory-reset`: formats the configuration storage of the device.

use super::diagnostic::CliError;
use super::{Ctx, dto};
use crate::model::ErrorCode;
use crate::{ClientTransport, Device, Error};

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    yes: bool,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    ctx.io
        .confirm(
            "The configuration storage of the device will be formatted:\n\n  \
             every section is removed, the device identity included,\n  \
             and the defaults are restored.\n\nContinue?",
            yes,
        )
        .await?;
    let restart_required = device.factory_reset().await?;
    // The device returned to its factory state; a reboot makes it start from
    // there. A device that cannot reboot is left running.
    let rebooting = match device.reboot().await {
        Ok(()) => true,
        Err(Error::Device(ErrorCode::NotSupported)) => false,
        Err(error) => return Err(error.into()),
    };
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(serde_json::json!({
            "restart_required": restart_required,
            "rebooting": rebooting,
        })),
        || {
            let mut text = "Factory reset: the configuration storage was formatted\n".to_owned();
            if rebooting {
                text.push_str("The device is rebooting\n");
            } else if restart_required {
                text.push_str("A restart is required for the changes to take effect\n");
            }
            text
        },
    )
}
