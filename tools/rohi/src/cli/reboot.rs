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
//! `rohi reboot`: asks the device to reboot.

use super::diagnostic::CliError;
use super::{Ctx, dto};
use crate::{ClientTransport, Device};

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    device.reboot().await?;
    emit(ctx)
}

fn emit(ctx: &mut Ctx) -> Result<(), CliError> {
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(serde_json::json!({ "rebooting": true })),
        || "The device is rebooting\n".to_owned(),
    )
}
