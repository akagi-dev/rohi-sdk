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
//! `rohi status`: the runtime state of the device and its services.

use std::time::Duration;

use super::diagnostic::CliError;
use super::{Ctx, dto};
use crate::model::{ServicesStatus, Value};
use crate::{ClientTransport, Device, PublicKey, format};

/// The device summary, or one service in detail.
pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    service: Option<String>,
    watch: bool,
    interval: u64,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    // On a terminal a human watch redraws one screen in place, so uptime and
    // ages stay current; otherwise it appends a document per change.
    let in_place = watch && ctx.format == super::OutputFormat::Human && ctx.io.screen();
    let mut previous: Option<String> = None;
    let mut printed = 0usize;
    loop {
        let (text, json) = match &service {
            None => {
                let info = device.info().await?;
                let status = device.services_status().await?;
                let sensor_id = sensor_id(device, &status).await?;
                (
                    format::services_status(ctx.paint, &info, &status, sensor_id.as_deref()),
                    dto::services_status(&info, &status, sensor_id.as_deref()),
                )
            }
            Some(service) => {
                let schema = device.service_schema(service).await?;
                let status = device.service_status(service).await?;
                (
                    format::service_status(ctx.paint, &status, &schema),
                    dto::service_status(&status, &schema),
                )
            }
        };
        // A screen that does not fit the terminal is printed like a plain watch.
        if in_place && ctx.io.redraw(&text)? {
            printed += 1;
            if ctx.watch_limit.is_some_and(|limit| printed >= limit) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
            continue;
        }
        // A watch prints again only when what is shown changed. Uptime and
        // ages move all the time, so they are not part of the comparison of
        // the summary: its revisions say whether anything observable changed.
        let key = match (&service, watch) {
            (None, true) => revisions(&json),
            (Some(_), true) if ctx.format == super::OutputFormat::Json => json.to_string(),
            _ => text.clone(),
        };
        if previous.as_deref() != Some(key.as_str()) {
            if printed > 0 && ctx.format == super::OutputFormat::Human {
                ctx.io.result("\n")?;
            }
            ctx.io
                .emit(ctx.format, &dto::Versioned::new(json), || text)?;
            previous = Some(key);
            printed += 1;
        }
        if !watch || ctx.watch_limit.is_some_and(|limit| printed >= limit) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

/// What a summary watch compares: the device health and the revision of every
/// service.
fn revisions(json: &serde_json::Value) -> String {
    let mut key = json["health"].to_string();
    for service in json["services"].as_array().into_iter().flatten() {
        key.push_str(&format!("|{}:{}", service["service"], service["revision"]));
    }
    key
}

/// The SS58 address of the device identity; `None` when the device has no
/// identity service or no identity is provisioned.
async fn sensor_id<T: ClientTransport>(
    device: &mut Device<T>,
    status: &ServicesStatus,
) -> Result<Option<String>, CliError> {
    const SERVICE: &str = "identity";
    const FIELD: &str = "public_key";
    let Some(summary) = status
        .services
        .iter()
        .find(|service| service.service == SERVICE)
    else {
        return Ok(None);
    };
    // Only a provisioned identity is ready: without one there is no key to ask for.
    if !summary.ready {
        return Ok(None);
    }
    let schema = device.service_schema(SERVICE).await?;
    let Some(field) = schema.fields.iter().find(|field| field.name == FIELD) else {
        return Ok(None);
    };
    let identity = device.service_status_after(SERVICE, summary).await?;
    let key = identity
        .fields
        .iter()
        .find(|entry| entry.field_id == field.id)
        .and_then(|entry| match &entry.value {
            Value::Bytes(bytes) => PublicKey::from_slice(bytes),
            _ => None,
        });
    Ok(key.map(|key| crate::ss58(&key)))
}
