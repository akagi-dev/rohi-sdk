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
//! `rohi tcp-key`: the pre-shared key of the TCP control endpoint.

use std::path::Path;

use crate::model::{FieldValue, Value};
use crate::{ClientTransport, Device, TcpKey, write_tcp_key};

use super::args::TcpKeyAction;
use super::{CliError, Ctx};

/// The field id of the `psk` argument of `control_psk.set_tcp_psk`.
const PSK: u32 = 1;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `rohi tcp-key generate FILE`: needs no device.
pub fn generate(file: &Path, ctx: &mut Ctx) -> Result<(), CliError> {
    let key = TcpKey::generate()?;
    write_tcp_key(file, key.as_bytes())?;
    ctx.io.result(&format!(
        "Key written to {} (readable by you only).\n\
         Keep the file secret. Give it to a device over a protected link with \
         `rohi --psk-file {} tcp-key set`, then connect with `--psk-file {}`.\n",
        file.display(),
        file.display(),
        file.display()
    ))
}

/// `rohi tcp-key set` and `rohi tcp-key clear`, run on the device.
pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: TcpKeyAction,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    match action {
        TcpKeyAction::Generate { .. } => Err(CliError::Usage(
            "this command does not talk to a device".into(),
        )),
        TcpKeyAction::Set { new_key } => {
            let new = new_key.as_deref().map(TcpKey::load).transpose()?;
            let Some(key) = new.as_ref().or(ctx.tcp_key.as_ref()) else {
                return Err(CliError::Usage(
                    "this needs the key: pass --psk-file FILE (or set ROHI_PSK_FILE), or --new-key FILE; \
                     `rohi tcp-key generate FILE` creates one"
                        .into(),
                ));
            };
            let records = device
                .invoke(
                    "control_psk",
                    "set_tcp_psk",
                    &[FieldValue {
                        field_id: PSK,
                        value: Value::Bytes(key.as_bytes().to_vec()),
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
                "The device now authenticates TCP clients with this key (fingerprint {fingerprint}).\n\
                 Connected TCP clients are disconnected.\n"
            ))
        }
        TcpKeyAction::Clear => {
            device.invoke("control_psk", "clear_tcp_psk", &[]).await?;
            ctx.io
                .result("The TCP key is removed from the device; a restricted TCP endpoint serves nobody.\n")
        }
    }
}
