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
//! `rohi time`.

use super::diagnostic::CliError;
use super::{Ctx, dto, status};
use crate::model::TimeStatus;
use crate::{ClientTransport, Device};

/// Lines of the time status for the human output. The wall-clock time is
/// shown only when the device has one.
pub fn lines(status: &TimeStatus) -> Vec<(&'static str, String)> {
    let mut lines = vec![("State", status.state.to_string())];
    if let Some(millis) = status.unix_time_ms {
        lines.push((
            "Unix time",
            format!("{}.{:03}", millis / 1_000, millis % 1_000),
        ));
    }
    if let Some(source) = status.source {
        lines.push(("Source", source.to_string()));
    }
    if let Some(age) = status.sync_age_ms {
        lines.push(("Last sync", format!("{} s ago", age / 1_000)));
    }
    if let Some(uncertainty) = status.uncertainty_ms {
        lines.push(("Accuracy", format!("+-{uncertainty} ms")));
    }
    if status.consecutive_failures > 0 {
        let failures = match status.last_failure {
            Some(failure) => format!("{} (last: {failure})", status.consecutive_failures),
            None => status.consecutive_failures.to_string(),
        };
        lines.push(("Failures", failures));
    }
    lines
}

pub async fn run<T: ClientTransport>(
    device: &mut Device<T>,
    ctx: &mut Ctx,
) -> Result<(), CliError> {
    let time = device.time_status().await?;
    ctx.io.emit(
        ctx.format,
        &dto::Versioned::new(dto::Time::new(&time)),
        || status::section_text("Time", &lines(&time)),
    )
}
