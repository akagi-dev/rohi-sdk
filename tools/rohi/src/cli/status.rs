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
//! `rohi status`: the runtime state of the device and its services.

use std::time::Duration;

use super::diagnostic::CliError;
use super::{Ctx, dto};
use crate::{ClientTransport, Device, format};

/// The device summary, or one service in detail.
pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    service: Option<String>,
    watch: bool,
    interval: u64,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let mut previous: Option<String> = None;
    let mut printed = 0usize;
    loop {
        let (text, json) = match &service {
            None => {
                let info = device.info().await?;
                let status = device.services_status().await?;
                (
                    format::services_status(&info, &status),
                    dto::services_status(&info, &status),
                )
            }
            Some(service) => {
                let schema = device.service_schema(service).await?;
                let status = device.service_status(service).await?;
                (
                    format::service_status(&status, &schema),
                    dto::service_status(&status, &schema),
                )
            }
        };
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
