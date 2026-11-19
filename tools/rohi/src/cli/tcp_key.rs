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

use crate::model::Value;
use crate::{ClientTransport, Device, write_tcp_key};

use super::args::TcpKeyAction;
use super::{CliError, Ctx};

/// The field ids of the results of `control_tcp.reveal_psk`.
const PSK: u32 = 1;
const FINGERPRINT: u32 = 2;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `rohi tcp-key reveal FILE`, run on the device: the device generates a new key, which is saved
/// to `FILE` and never printed.
pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    action: TcpKeyAction,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let TcpKeyAction::Reveal { file } = action;
    // The device replaces its key on every call: refuse before the call, not after it.
    if file.exists() {
        return Err(CliError::Usage(format!(
            "{} already exists; remove it or choose another file (the device replaces its key on every call)",
            file.display()
        )));
    }
    let records = device.invoke("control_tcp", "reveal_psk", &[]).await?;
    let field = |id: u32| {
        records
            .iter()
            .flatten()
            .find(|field| field.field_id == id)
            .and_then(|field| match &field.value {
                Value::Bytes(bytes) => Some(bytes.clone()),
                _ => None,
            })
    };
    let key: [u8; 32] = field(PSK)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| CliError::Usage("the device returned no 32-byte key".into()))?;
    write_tcp_key(&file, &key)?;
    let fingerprint = field(FINGERPRINT)
        .map(|bytes| hex(&bytes))
        .unwrap_or_default();
    ctx.io.result(&format!(
        "The device generated a new key (fingerprint {fingerprint}) and saved it to {} (readable by you only).\n\
         Connected TCP clients of the old key are disconnected. Connect with `--psk-file {}`.\n",
        file.display(),
        file.display()
    ))
}
