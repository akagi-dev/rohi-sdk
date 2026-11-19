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
//! `rohi admin`: the administrator key of signed requests.

use std::path::Path;

use crate::model::{FieldValue, Value};
use crate::{AdminKey, ClientTransport, Device, write_admin_key};

use super::args::AdminAction;
use super::{CliError, Ctx};

/// The field id of the `public_key` argument of `control_auth.set_admin_key`.
const PUBLIC_KEY: u32 = 1;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `rohi admin keygen FILE`: needs no device.
pub fn keygen(file: &Path, ctx: &mut Ctx) -> Result<(), CliError> {
    let (key, seed) = AdminKey::generate()?;
    write_admin_key(file, &seed)?;
    ctx.io.result(&format!(
        "Administrator key written to {} (readable by you only).\n\
         Public key: 0x{}\n\
         Keep the file secret. Tell a device to trust it with `rohi --admin-key {} admin enroll`.\n",
        file.display(),
        hex(key.public_key().as_bytes()),
        file.display()
    ))
}

fn loaded(ctx: &Ctx) -> Result<&AdminKey, CliError> {
    ctx.admin_key.as_ref().ok_or_else(|| {
        CliError::Usage(
            "this needs the administrator key: pass --admin-key FILE (or set ROHI_ADMIN_KEY); \
             `rohi admin keygen FILE` creates one"
                .into(),
        )
    })
}

/// `rohi admin public-key`: needs no device.
pub fn public_key(ctx: &mut Ctx) -> Result<(), CliError> {
    let text = format!("0x{}\n", hex(loaded(ctx)?.public_key().as_bytes()));
    ctx.io.result(&text)
}

/// `rohi admin enroll` and `rohi admin clear`, run on the device.
pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: AdminAction,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        AdminAction::Keygen { .. } | AdminAction::PublicKey => Err(CliError::Usage(
            "this command does not talk to a device".into(),
        )),
        AdminAction::Enroll => {
            let public = loaded(ctx)?.public_key();
            let records = device
                .invoke(
                    "control_auth",
                    "set_admin_key",
                    &[FieldValue {
                        field_id: PUBLIC_KEY,
                        value: Value::Bytes(public.to_bytes().to_vec()),
                    }],
                )
                .await?;
            let fingerprint = records
                .iter()
                .flatten()
                .find_map(|field| match &field.value {
                    Value::Bytes(bytes) => Some(hex(bytes)),
                    _ => None,
                })
                .unwrap_or_default();
            ctx.io.result(&format!(
                "The device now trusts this administrator key (fingerprint {fingerprint}).\n"
            ))
        }
        AdminAction::Clear => {
            device
                .invoke("control_auth", "clear_admin_key", &[])
                .await?;
            ctx.io
                .result("The administrator key is removed from the device.\n")
        }
    }
}
