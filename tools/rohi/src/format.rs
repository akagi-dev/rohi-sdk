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
//! Human readable rendering of the model.
//!
//! Everything is driven by the schemas the device sends: nothing here knows
//! a section, a service or an action by name.

use core::fmt::Write;
use std::collections::HashMap;

use crate::model::*;

/// Terminal styling of the human output. [`Paint::plain`] adds nothing, so the
/// text stays clean for pipes and files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Paint {
    enabled: bool,
}

#[derive(Clone, Copy)]
enum Tone {
    Good,
    Warn,
    Bad,
    Muted,
    Strong,
}

impl Paint {
    /// No styling.
    pub const fn plain() -> Self {
        Self { enabled: false }
    }

    /// ANSI colors and weights.
    pub const fn ansi() -> Self {
        Self { enabled: true }
    }

    fn tone(self, tone: Tone, text: &str) -> String {
        if !self.enabled || text.is_empty() {
            return text.to_owned();
        }
        let code = match tone {
            Tone::Good => "32",
            Tone::Warn => "33",
            Tone::Bad => "31",
            Tone::Muted => "2",
            Tone::Strong => "1",
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }

    pub(crate) fn strong(self, text: &str) -> String {
        self.tone(Tone::Strong, text)
    }

    fn muted(self, text: &str) -> String {
        self.tone(Tone::Muted, text)
    }

    /// A state, health, readiness or severity word, colored by how well it
    /// is going; other text stays as it is.
    fn status(self, text: &str) -> String {
        let tone = match text.to_lowercase().as_str() {
            "ok" | "running" | "yes" => Tone::Good,
            "degraded" | "starting" | "stopping" | "warning" => Tone::Warn,
            "error" | "failed" | "critical" => Tone::Bad,
            "unknown" | "-" | "disabled" | "stopped" | "no" | "info" => Tone::Muted,
            _ => return text.to_owned(),
        };
        self.tone(tone, text)
    }
}

/// A duration as `2h 14m`, `34s` or `120ms`.
pub fn duration(ms: u64) -> String {
    let seconds = ms / 1_000;
    match seconds {
        0 => format!("{ms}ms"),
        1..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m {}s", seconds / 60, seconds % 60),
        3_600..=86_399 => format!("{}h {}m", seconds / 3_600, seconds % 3_600 / 60),
        _ => format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3_600),
    }
}

/// A value with the unit of its field.
pub fn value(value: &Value, field: Option<&FieldDescriptor>) -> String {
    let unit = field.map(|field| field.unit.as_str()).unwrap_or_default();
    let text = match value {
        Value::F64(number) => format!("{number:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned(),
        other => other.to_string(),
    };
    if unit.is_empty() {
        text
    } else {
        format!("{text} {unit}")
    }
}

/// Rows of cells, columns padded to the widest cell and two spaces apart.
/// A table with a header row; the cells of the `status` columns are colored by
/// their meaning.
fn table(paint: Paint, headers: &[&str], rows: &[Vec<String>], status: &[usize]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|header| header.len()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let mut line = |cells: &mut dyn Iterator<Item = &str>, header: bool| {
        let mut text = String::new();
        for (index, cell) in cells.enumerate() {
            text.push_str(&if header {
                paint.strong(cell)
            } else if status.contains(&index) {
                paint.status(cell)
            } else {
                cell.to_owned()
            });
            let padding = widths[index] - cell.chars().count() + 2;
            text.push_str(&" ".repeat(padding));
        }
        out.push_str(text.trim_end());
        out.push('\n');
    };
    line(&mut headers.iter().copied(), true);
    for row in rows {
        line(&mut row.iter().map(String::as_str), false);
    }
    out
}

/// `name  value` lines with the names padded.
fn pairs(paint: Paint, rows: &[(String, String)]) -> String {
    let width = rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = String::new();
    for (name, value) in rows {
        let _ = writeln!(out, "{}{value}", paint.strong(&format!("{name:<width$}")));
    }
    out
}

/// Devices found by a scan.
pub fn devices(paint: Paint, devices: &[crate::DiscoveredDevice]) -> String {
    if devices.is_empty() {
        return "No devices found\n".to_owned();
    }
    let mut out = String::new();
    for device in devices {
        let _ = writeln!(
            out,
            "{}  {}{}",
            paint.strong(device.name.as_deref().unwrap_or("(no name)")),
            device.id,
            device
                .rssi
                .map(|rssi| paint.muted(&format!("  {rssi} dBm")))
                .unwrap_or_default()
        );
    }
    out
}

fn health(health: ServiceHealth) -> String {
    match health {
        ServiceHealth::Unknown => "-".to_owned(),
        other => other.to_string(),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn since(summary: &ServiceSummary) -> String {
    match summary.state {
        ServiceState::Disabled => "-".to_owned(),
        _ => duration(summary.state_age_ms),
    }
}

/// The device-wide summary of `rohi status`.
///
/// `sensor_id` is the SS58 address of the device identity, shown when known.
pub fn services_status(
    paint: Paint,
    info: &DeviceInfo,
    status: &ServicesStatus,
    sensor_id: Option<&str>,
) -> String {
    let mut rows = vec![];
    if let Some(sensor_id) = sensor_id {
        rows.push(("SensorId:".to_owned(), sensor_id.to_owned()));
    }
    rows.push((
        "Device:".to_owned(),
        format!("{} ({})", info.firmware, info.board),
    ));
    rows.push(("Uptime:".to_owned(), duration(status.uptime_ms)));
    rows.push((
        "Health:".to_owned(),
        paint.status(&status.health.to_string()),
    ));
    let mut out = pairs(paint, &rows);
    out.push('\n');
    let rows: Vec<Vec<String>> = status
        .services
        .iter()
        .map(|service| {
            vec![
                service.service.clone(),
                service.state.to_string(),
                health(service.health),
                yes_no(service.ready).to_owned(),
                since(service),
            ]
        })
        .collect();
    out.push_str(&table(
        paint,
        &["SERVICE", "STATE", "HEALTH", "READY", "SINCE"],
        &rows,
        &[1, 2, 3],
    ));
    out
}

fn field_name(fields: &[FieldDescriptor], id: u32) -> Option<&FieldDescriptor> {
    fields.iter().find(|field| field.id == id)
}

/// The detailed status of one service.
pub fn service_status(paint: Paint, status: &ServiceStatus, schema: &ServiceSchema) -> String {
    let summary = &status.summary;
    let mut out = pairs(
        paint,
        &[
            ("Service:".to_owned(), summary.service.clone()),
            (
                "State:".to_owned(),
                paint.status(&summary.state.to_string()),
            ),
            (
                "Health:".to_owned(),
                paint.status(&summary.health.to_string()),
            ),
            ("Ready:".to_owned(), paint.status(yes_no(summary.ready))),
            ("Since:".to_owned(), since(summary)),
        ],
    );
    if !status.fields.is_empty() {
        out.push('\n');
        let rows: Vec<(String, String)> = status
            .fields
            .iter()
            .map(|entry| {
                let field = field_name(&schema.fields, entry.field_id);
                (
                    field
                        .map(|field| field.name.clone())
                        .unwrap_or_else(|| format!("#{}", entry.field_id)),
                    value(&entry.value, field),
                )
            })
            .collect();
        out.push_str(&pairs(paint, &rows));
    }
    if !status.issues.is_empty() {
        out.push_str(&format!("\n{}\n", paint.strong("Issues:")));
        for issue in &status.issues {
            let severity = issue.severity.as_str();
            let _ = writeln!(
                out,
                "  {}{} {}  ({})",
                paint.status(&severity.to_uppercase()),
                " ".repeat(8usize.saturating_sub(severity.chars().count())),
                issue.code,
                duration(issue.age_ms)
            );
            let _ = writeln!(out, "           {}", issue.message);
        }
    }
    out
}

/// The config sections of the device.
pub fn config_sections(paint: Paint, sections: &[SectionInfo]) -> String {
    if sections.is_empty() {
        return "The device has no configuration sections\n".to_owned();
    }
    let rows: Vec<Vec<String>> = sections
        .iter()
        .map(|section| vec![section.name.clone(), section.version.to_string()])
        .collect();
    table(paint, &["SECTION", "VERSION"], &rows, &[])
}

fn constraints(field: &ConfigField) -> String {
    let mut parts = Vec::new();
    match (field.min, field.max) {
        (Some(min), Some(max)) => parts.push(format!("{min}..{max}")),
        (Some(min), None) => parts.push(format!(">= {min}")),
        (None, Some(max)) => parts.push(format!("<= {max}")),
        (None, None) => {}
    }
    if !field.field.variants.is_empty() {
        parts.push(field.field.variants.join("|"));
    }
    let mut text = parts.join(" ");
    if !field.field.unit.is_empty() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&field.field.unit);
    }
    text
}

fn access(field: &ConfigField) -> &'static str {
    match (field.writable, field.secret) {
        (_, true) => "write-only",
        (true, false) => "read-write",
        (false, false) => "read-only",
    }
}

/// The fields of a config section.
pub fn config_schema(paint: Paint, schema: &ConfigSchema) -> String {
    let rows: Vec<Vec<String>> = schema
        .fields
        .iter()
        .map(|field| {
            let mut kind = field.field.kind.to_string();
            if field.optional {
                kind.push('?');
            }
            vec![
                field.field.name.clone(),
                kind,
                access(field).to_owned(),
                constraints(field),
                field.field.description.clone(),
            ]
        })
        .collect();
    table(
        paint,
        &["FIELD", "TYPE", "ACCESS", "CONSTRAINTS", "DESCRIPTION"],
        &rows,
        &[],
    )
}

/// The values of a config section; secrets are `<configured>`.
pub fn config_values(
    paint: Paint,
    values: &ConfigValues,
    schema: &ConfigSchema,
    only: Option<&str>,
) -> String {
    let mut rows = Vec::new();
    for field in &schema.fields {
        if only.is_some_and(|name| name != field.field.name) {
            continue;
        }
        let entry = values
            .entries
            .iter()
            .find(|entry| entry.field_id == field.field.id);
        let text = match entry.map(|entry| &entry.state) {
            Some(EntryState::Value(v)) => value(v, Some(&field.field)),
            Some(EntryState::Redacted) => paint.muted("<configured>"),
            None => paint.muted("-"),
        };
        rows.push((field.field.name.clone(), text));
    }
    let mut out = String::new();
    if !values.configured && only.is_none() {
        out.push_str("Not configured\n\n");
    }
    out.push_str(&pairs(paint, &rows));
    out
}

/// The actions of the device.
pub fn actions(paint: Paint, actions: &[ActionInfo]) -> String {
    if actions.is_empty() {
        return "The device offers no actions\n".to_owned();
    }
    let rows: Vec<Vec<String>> = actions
        .iter()
        .map(|action| {
            vec![
                action.service.clone(),
                action.name.clone(),
                if action.privileged { "write" } else { "read" }.to_owned(),
                action.description.clone(),
            ]
        })
        .collect();
    table(
        paint,
        &["SERVICE", "ACTION", "ACCESS", "DESCRIPTION"],
        &rows,
        &[],
    )
}

fn field_table(paint: Paint, fields: &[FieldDescriptor]) -> String {
    let rows: Vec<Vec<String>> = fields
        .iter()
        .map(|field| {
            vec![
                field.name.clone(),
                field.kind.to_string(),
                if field.variants.is_empty() {
                    field.unit.clone()
                } else {
                    field.variants.join("|")
                },
                field.description.clone(),
            ]
        })
        .collect();
    table(
        paint,
        &["FIELD", "TYPE", "UNIT/VALUES", "DESCRIPTION"],
        &rows,
        &[],
    )
}

/// The arguments and results of an action.
pub fn action_schema(paint: Paint, schema: &ActionSchema) -> String {
    let action = &schema.action;
    let mut out = pairs(
        paint,
        &[
            (
                "Action:".to_owned(),
                format!("{} {}", action.service, action.name),
            ),
            (
                "Access:".to_owned(),
                if action.privileged { "write" } else { "read" }.to_owned(),
            ),
            ("About:".to_owned(), action.description.clone()),
        ],
    );
    if !schema.args.is_empty() {
        let heading = if action.secret_args {
            "Arguments (all required, secret: give them as NAME=-):"
        } else {
            "Arguments (all required):"
        };
        out.push_str(&format!("\n{}\n", paint.strong(heading)));
        out.push_str(&field_table(paint, &schema.args));
    }
    if !schema.results.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            paint.strong("Results (one record each):")
        ));
        out.push_str(&field_table(paint, &schema.results));
    }
    out
}

/// The records an action returned, one table row each.
pub fn records(paint: Paint, records: &[Record], schema: &ActionSchema) -> String {
    if schema.results.is_empty() {
        return "Done\n".to_owned();
    }
    if records.is_empty() {
        return "No results\n".to_owned();
    }
    let by_id: HashMap<u32, &FieldDescriptor> = schema
        .results
        .iter()
        .map(|field| (field.id, field))
        .collect();
    let headers: Vec<String> = schema
        .results
        .iter()
        .map(|field| field.name.to_uppercase())
        .collect();
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|record| {
            schema
                .results
                .iter()
                .map(|field| {
                    record
                        .iter()
                        .find(|entry| entry.field_id == field.id)
                        .map(|entry| value(&entry.value, by_id.get(&field.id).copied()))
                        .unwrap_or_else(|| "-".to_owned())
                })
                .collect()
        })
        .collect();
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    table(paint, &headers, &rows, &[])
}

/// Text lines of a diff, see `rohi config diff`.
pub fn diff_line(paint: Paint, marker: char, name: &str, text: &str) -> String {
    let tone = match marker {
        '+' => Tone::Good,
        '-' => Tone::Bad,
        _ => Tone::Warn,
    };
    format!(
        "  {} {name:<16}{text}\n",
        paint.tone(tone, &marker.to_string())
    )
}
