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
//! The JSON written by `-o json`. These documents are owned by the command
//! line and versioned with `schema_version`; they are deliberately not the
//! model types of the library, which may change independently.
//!
//! Values are keyed by field name. Secrets are never present: a set secret
//! is `{"secret": true}`.

use serde::Serialize;
use serde_json::{Map, Value as Json, json};

use crate::DiscoveredDevice;
use crate::model::*;

/// Version of every JSON document of the command line.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
pub struct Versioned<T> {
    schema_version: u32,
    #[serde(flatten)]
    body: T,
}

impl<T> Versioned<T> {
    pub fn new(body: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            body,
        }
    }
}

#[derive(Serialize)]
pub struct ScanDevice {
    pub id: String,
    pub name: Option<String>,
    pub rssi_dbm: Option<i16>,
    pub uri: String,
}

#[derive(Serialize)]
pub struct Scan {
    pub devices: Vec<ScanDevice>,
}

/// The `ble://` URI of a discovered device: the advertised name, or the ID.
pub fn ble_uri(device: &DiscoveredDevice) -> String {
    format!("ble://{}", device.name.as_deref().unwrap_or(&device.id))
}

impl Scan {
    pub fn new(devices: &[DiscoveredDevice]) -> Self {
        Self {
            devices: devices
                .iter()
                .map(|device| ScanDevice {
                    id: device.id.clone(),
                    name: device.name.clone(),
                    rssi_dbm: device.rssi,
                    uri: ble_uri(device),
                })
                .collect(),
        }
    }
}

fn json<T: Serialize>(value: &T) -> Json {
    serde_json::to_value(value).unwrap_or(Json::Null)
}

fn field_name(fields: &[FieldDescriptor], id: u32) -> String {
    fields
        .iter()
        .find(|field| field.id == id)
        .map(|field| field.name.clone())
        .unwrap_or_else(|| format!("#{id}"))
}

/// Field values keyed by field name.
pub fn values(fields: &[FieldValue], schema: &[FieldDescriptor]) -> Json {
    let mut map = Map::new();
    for field in fields {
        map.insert(field_name(schema, field.field_id), json(&field.value));
    }
    Json::Object(map)
}

fn units(schema: &[FieldDescriptor]) -> Json {
    let mut map = Map::new();
    for field in schema.iter().filter(|field| !field.unit.is_empty()) {
        map.insert(field.name.clone(), json(&field.unit));
    }
    Json::Object(map)
}

pub fn summary(summary: &ServiceSummary) -> Json {
    json(summary)
}

pub fn services_status(
    info: &DeviceInfo,
    status: &ServicesStatus,
    sensor_id: Option<&str>,
) -> Json {
    json!({
        "device": info,
        "sensor_id": sensor_id,
        "uptime_ms": status.uptime_ms,
        "health": status.health,
        "services": status.services,
    })
}

pub fn service_status(status: &ServiceStatus, schema: &ServiceSchema) -> Json {
    json!({
        "service": summary(&status.summary),
        "values": values(&status.fields, &schema.fields),
        "units": units(&schema.fields),
        "issues": status.issues,
    })
}

pub fn sections(sections: &[SectionInfo]) -> Json {
    json!({ "sections": sections })
}

pub fn config_schema(schema: &ConfigSchema) -> Json {
    json!({ "section": schema.section, "fields": schema.fields })
}

/// The effective values of a section, keyed by field name: a field without a
/// value is `null`, a set secret is `{"secret": true}`. `overridden` names
/// the fields the user set explicitly.
pub fn config_values(values: &ConfigValues, schema: &ConfigSchema) -> Json {
    let mut map = Map::new();
    let mut overridden = Vec::new();
    for field in &schema.fields {
        let entry = values
            .entries
            .iter()
            .find(|entry| entry.field_id == field.field.id);
        let shown = match entry.map(|entry| &entry.state) {
            Some(EntryState::Value(value)) => json(value),
            Some(EntryState::Redacted) => json!({ "secret": true }),
            None => Json::Null,
        };
        map.insert(field.field.name.clone(), shown);
        if values.is_overridden(field.field.id) {
            overridden.push(field.field.name.clone());
        }
    }
    json!({
        "section": values.section,
        "values": Json::Object(map),
        "overridden": overridden,
    })
}

pub fn applied(section: &str, applied: &Applied, dry_run: bool) -> Json {
    json!({
        "section": section,
        "revision": applied.revision,
        "restart_required": applied.restart_required,
        "dry_run": dry_run,
    })
}

pub fn actions(actions: &[ActionInfo]) -> Json {
    json!({ "actions": actions })
}

pub fn action_schema(schema: &ActionSchema) -> Json {
    json!({ "action": schema.action, "args": schema.args, "results": schema.results })
}

pub fn records(records: &[Record], schema: &ActionSchema) -> Json {
    let records: Vec<Json> = records
        .iter()
        .map(|record| values(record, &schema.results))
        .collect();
    json!({ "records": records, "units": units(&schema.results) })
}
