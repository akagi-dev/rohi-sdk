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
//! Conversions between the generic service model and the wire messages.

use heapless::{String, Vec};
use rohi_control_proto::control as pb;
use rohi_service::{
    ActionDescriptor, ConfigEntry, ConfigField, DeviceHealth, EntryState, FieldDescriptor, Issue,
    IssueSeverity, Mutation, MutationOp, SectionInfo, ServiceHealth, ServiceInfo, ServiceState,
    ServiceStatusSummary, Value, ValueKind,
};

/// `text` cut to `N` bytes at a character boundary. Names are meant to fit;
/// longer presentation text is shortened rather than failing a whole response.
pub(crate) fn text<const N: usize>(text: &str) -> String<N> {
    let mut end = text.len().min(N);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    String::try_from(&text[..end]).unwrap_or_default()
}

pub(crate) fn value(value: &Value) -> pb::Value {
    use pb::Value_::Value as V;
    pb::Value {
        value: Some(match value {
            Value::Bool(v) => V::BoolValue(*v),
            Value::I64(v) => V::I64Value(*v),
            Value::U64(v) => V::U64Value(*v),
            Value::F64(v) => V::F64Value(*v),
            Value::String(v) => V::StringValue(text(v)),
            Value::Bytes(v) => V::BytesValue(v.clone()),
            Value::Enum(v) => V::EnumValue(text(v)),
        }),
    }
}

/// `None` for a value without content, which is not a valid request.
pub(crate) fn from_value(value: &pb::Value) -> Option<Value> {
    use pb::Value_::Value as V;
    Some(match value.value.as_ref()? {
        V::BoolValue(v) => Value::Bool(*v),
        V::I64Value(v) => Value::I64(*v),
        V::U64Value(v) => Value::U64(*v),
        V::F64Value(v) => Value::F64(*v),
        V::StringValue(v) => Value::String(String::try_from(v.as_str()).ok()?),
        V::BytesValue(v) => Value::Bytes(v.clone()),
        V::EnumValue(v) => Value::Enum(String::try_from(v.as_str()).ok()?),
    })
}

/// Overwrites the content of a received value that may have held a secret.
pub(crate) fn wipe(value: &mut pb::Value) {
    use pb::Value_::Value as V;
    match &mut value.value {
        Some(V::StringValue(text)) => {
            let len = text.len();
            text.clear();
            for _ in 0..len {
                let _ = text.push('\0');
            }
            text.clear();
        }
        Some(V::BytesValue(bytes)) => {
            bytes.iter_mut().for_each(|byte| *byte = 0);
            bytes.clear();
        }
        _ => {}
    }
}

pub(crate) fn field_value(id: u32, value: &Value) -> pb::FieldValue {
    let mut out = pb::FieldValue {
        field_id: id,
        ..Default::default()
    };
    out.set_value(self::value(value));
    out
}

fn kind(kind: ValueKind) -> pb::ValueKind {
    match kind {
        ValueKind::Bool => pb::ValueKind::Bool,
        ValueKind::I64 => pb::ValueKind::I64,
        ValueKind::U64 => pb::ValueKind::U64,
        ValueKind::F64 => pb::ValueKind::F64,
        ValueKind::String => pb::ValueKind::String,
        ValueKind::Bytes => pb::ValueKind::Bytes,
        ValueKind::Enum => pb::ValueKind::Enum,
    }
}

pub(crate) fn field(field: &FieldDescriptor) -> pb::FieldDescriptor {
    let mut variants = Vec::new();
    for variant in field.variants {
        if variants.push(text(variant)).is_err() {
            break;
        }
    }
    pb::FieldDescriptor {
        id: field.id,
        name: text(field.name),
        kind: kind(field.kind),
        description: text(field.description),
        unit: text(field.unit),
        variants,
    }
}

pub(crate) fn config_field(config: &ConfigField) -> pb::ConfigField {
    let mut out = pb::ConfigField {
        writable: config.writable,
        secret: config.secret,
        optional: config.optional,
        ..Default::default()
    };
    out.set_field(field(&config.field));
    if let Some(min) = config.min {
        out.set_min(min);
    }
    if let Some(max) = config.max {
        out.set_max(max);
    }
    out
}

pub(crate) fn section_info(info: &SectionInfo) -> pb::ConfigSectionInfo {
    pb::ConfigSectionInfo {
        name: text(info.name),
        version: info.version,
        revision: info.revision,
    }
}

pub(crate) fn config_entry(entry: &ConfigEntry) -> pb::ConfigEntry {
    use pb::ConfigEntry_::State;
    pb::ConfigEntry {
        field_id: entry.field_id,
        state: Some(match &entry.state {
            EntryState::Value(v) => State::Value(value(v)),
            EntryState::Redacted => State::Redacted(pb::Empty {}),
        }),
    }
}

/// `None` for a mutation without an operation or without value content.
pub(crate) fn mutation(mutation: &pb::ConfigMutation) -> Option<Mutation> {
    use pb::ConfigMutation_::Operation;
    Some(Mutation {
        field_id: mutation.field_id,
        op: match mutation.operation.as_ref()? {
            Operation::Set(v) => MutationOp::Set(from_value(v)?),
            Operation::Unset(_) => MutationOp::Unset,
        },
    })
}

pub(crate) fn service_info(info: &ServiceInfo) -> pb::ServiceInfo {
    pb::ServiceInfo {
        name: text(info.name),
        version: info.version,
    }
}

fn state(state: ServiceState) -> pb::ServiceState {
    match state {
        ServiceState::Disabled => pb::ServiceState::Disabled,
        ServiceState::Stopped => pb::ServiceState::Stopped,
        ServiceState::Starting => pb::ServiceState::Starting,
        ServiceState::Running => pb::ServiceState::Running,
        ServiceState::Stopping => pb::ServiceState::Stopping,
        ServiceState::Failed => pb::ServiceState::Failed,
    }
}

fn health(health: ServiceHealth) -> pb::ServiceHealth {
    match health {
        ServiceHealth::Unknown => pb::ServiceHealth::Unknown,
        ServiceHealth::Ok => pb::ServiceHealth::Ok,
        ServiceHealth::Degraded => pb::ServiceHealth::Degraded,
        ServiceHealth::Error => pb::ServiceHealth::Error,
    }
}

pub(crate) fn device_health(health: DeviceHealth) -> pb::DeviceHealth {
    match health {
        DeviceHealth::Ok => pb::DeviceHealth::Ok,
        DeviceHealth::Degraded => pb::DeviceHealth::Degraded,
        DeviceHealth::Error => pb::DeviceHealth::Error,
    }
}

pub(crate) fn summary(summary: &ServiceStatusSummary) -> pb::ServiceStatusSummary {
    let mut out = pb::ServiceStatusSummary {
        service: text(summary.name),
        state: state(summary.state),
        health: health(summary.health),
        ready: summary.ready,
        revision: summary.revision,
        state_age_ms: summary.state_age_ms,
        ..Default::default()
    };
    if let Some(reason) = summary.reason {
        out.set_reason(text(reason));
    }
    out
}

pub(crate) fn issue(issue: &Issue) -> pb::ServiceIssue {
    pb::ServiceIssue {
        severity: match issue.severity {
            IssueSeverity::Info => pb::IssueSeverity::Info,
            IssueSeverity::Warning => pb::IssueSeverity::Warning,
            IssueSeverity::Error => pb::IssueSeverity::Error,
            IssueSeverity::Critical => pb::IssueSeverity::Critical,
        },
        code: text(issue.code),
        message: text(issue.message),
        age_ms: issue.age_ms.unwrap_or(0),
    }
}

pub(crate) fn action_info(service: &str, action: &ActionDescriptor) -> pb::ActionInfo {
    pb::ActionInfo {
        service: text(service),
        name: text(action.name),
        description: text(action.description),
        privileged: action.privileged,
    }
}
