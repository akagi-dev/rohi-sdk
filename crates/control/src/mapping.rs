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
//! Conversions between the generic service model and the wire messages.
//! Conversions between the generic service model and the wire messages.

use heapless::{String, Vec};
use rohi_control_proto::control as pb;
use rohi_control_proto::limits;
use rohi_service::{
    ActionDescriptor, ConfigEntry, ConfigField, DeviceHealth, EntryState, EnumDescriptor,
    FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, Mutation, MutationOp, SectionInfo,
    ServiceHealth, ServiceState, ServiceStatusSummary, Unit, Value, ValueKind,
};
use zeroize::Zeroize;

// The service model and the wire bounds are two definitions of the same
// limits; a model larger than the wire would silently truncate.
const _: () = {
    assert!(rohi_service::MAX_VALUE_LEN == limits::MAX_VALUE_LEN);
    assert!(rohi_service::MAX_NAME_LEN == limits::MAX_NAME_LEN);
    assert!(rohi_service::MAX_FIELD_VALUE_LEN == limits::MAX_FIELD_VALUE_LEN);
    assert!(rohi_service::MAX_FIELDS <= limits::MAX_STATUS_FIELDS);
    assert!(rohi_service::MAX_FIELDS <= limits::MAX_SCHEMA_FIELDS);
    assert!(rohi_service::MAX_CONFIG_FIELDS <= limits::MAX_CONFIG_FIELDS);
    assert!(rohi_service::MAX_MUTATIONS <= limits::MAX_MUTATIONS);
    assert!(rohi_service::MAX_ISSUES <= limits::MAX_ISSUES);
    assert!(rohi_service::MAX_ISSUE_DESCRIPTORS <= limits::MAX_ISSUE_DESCRIPTORS);
    assert!(rohi_service::MAX_ENUMS <= limits::MAX_ENUMS);
    assert!(rohi_service::MAX_ENUM_VARIANTS <= limits::MAX_ENUM_VARIANTS);
    assert!(rohi_service::MAX_VARIANT_NAME_LEN <= limits::MAX_VARIANT_NAME_LEN);
    assert!(rohi_service::MAX_ACTION_ARGS <= limits::MAX_ACTION_ARGS);
    assert!(rohi_service::MAX_RECORDS <= limits::MAX_RECORDS);
    assert!(rohi_service::MAX_RECORD_FIELDS <= limits::MAX_RECORD_FIELDS);
    assert!(rohi_service::MAX_REGISTRY_ENTRIES <= limits::MAX_SERVICES);
    assert!(rohi_service::MAX_REGISTRY_ENTRIES <= limits::MAX_CONFIG_SECTIONS);
};

/// `text` cut to `N` bytes at a character boundary. Nothing is cut in practice:
/// the names of schemas are checked when their section or service is built and
/// values are bounded by the same limits as the wire.
pub(crate) fn text<const N: usize>(text: &str) -> String<N> {
    let mut end = text.len().min(N);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    String::try_from(&text[..end]).unwrap_or_default()
}

// `FieldValue`, `ConfigEntry` and `ConfigMutation` repeat the same value
// cases in their own oneof, so the conversions are generated for each.
macro_rules! value_codec {
    ($to:ident, $from:ident, $wipe:ident, $oneof:ty) => {
        #[allow(dead_code)]
        pub(crate) fn $to(value: &Value) -> $oneof {
            type V = $oneof;
            match value {
                Value::Bool(v) => V::BoolValue(*v),
                Value::I64(v) => V::I64Value(*v),
                Value::U64(v) => V::U64Value(*v),
                Value::F32(v) => V::F32Value(*v),
                Value::String(v) => V::StringValue(text(v)),
                Value::Bytes(v) => V::BytesValue(cut(v)),
                Value::Enum(v) => V::EnumValue(*v),
            }
        }

        /// `None` for a case that carries no value.
        #[allow(dead_code)]
        pub(crate) fn $from(value: &$oneof) -> Option<Value> {
            type V = $oneof;
            Some(match value {
                V::BoolValue(v) => Value::Bool(*v),
                V::I64Value(v) => Value::I64(*v),
                V::U64Value(v) => Value::U64(*v),
                V::F32Value(v) => Value::F32(*v),
                V::StringValue(v) => Value::String(String::try_from(v.as_str()).ok()?),
                V::BytesValue(v) => Value::Bytes(cut(v)),
                V::EnumValue(v) => Value::Enum(*v),
                #[allow(unreachable_patterns)]
                _ => return None,
            })
        }

        /// Overwrites the content of a received value that may have held a
        /// secret.
        #[allow(dead_code)]
        pub(crate) fn $wipe(value: &mut $oneof) {
            type V = $oneof;
            match value {
                V::StringValue(text) => {
                    // SAFETY: only zero bytes are written, which are valid UTF-8.
                    unsafe { text.as_mut_vec() }.as_mut_slice().zeroize();
                    text.clear();
                }
                V::BytesValue(bytes) => {
                    bytes.as_mut_slice().zeroize();
                    bytes.clear();
                }
                _ => {}
            }
        }
    };
}

/// `bytes` cut to the capacity of the wire field; `text` cuts strings the same way.
fn cut<const N: usize>(bytes: &[u8]) -> Vec<u8, N> {
    let mut out = Vec::new();
    let _ = out.extend_from_slice(&bytes[..bytes.len().min(N)]);
    out
}

value_codec!(
    field_case,
    from_field_case,
    wipe_field_case,
    pb::FieldValue_::Value
);
value_codec!(
    entry_case,
    from_entry_case,
    wipe_entry_case,
    pb::ConfigEntry_::State
);
value_codec!(
    mutation_case,
    from_mutation_case,
    wipe_mutation_case,
    pb::ConfigMutation_::Operation
);
value_codec!(
    arg_case,
    from_arg_case,
    wipe_arg_case,
    pb::ActionArg_::Value
);

pub(crate) fn field_value(id: u32, value: &Value) -> pb::FieldValue {
    pb::FieldValue {
        field_id: id,
        value: Some(field_case(value)),
    }
}

/// `None` for an argument without content, which is not a valid request.
pub(crate) fn from_arg(arg: &pb::ActionArg) -> Option<(u32, Value)> {
    Some((arg.field_id, from_arg_case(arg.value.as_ref()?)?))
}

pub(crate) fn wipe_arg(arg: &mut pb::ActionArg) {
    if let Some(case) = &mut arg.value {
        wipe_arg_case(case);
    }
}

fn kind(kind: ValueKind) -> pb::ValueKind {
    match kind {
        ValueKind::Bool => pb::ValueKind::Bool,
        ValueKind::I64 => pb::ValueKind::I64,
        ValueKind::U64 => pb::ValueKind::U64,
        ValueKind::F32 => pb::ValueKind::F32,
        ValueKind::String => pb::ValueKind::String,
        ValueKind::Bytes => pb::ValueKind::Bytes,
        ValueKind::Enum => pb::ValueKind::Enum,
    }
}

fn unit(unit: Unit) -> pb::Unit {
    match unit {
        Unit::None => pb::Unit::None,
        Unit::Ms => pb::Unit::Ms,
        Unit::S => pb::Unit::S,
        Unit::Hz => pb::Unit::Hz,
        Unit::Dbm => pb::Unit::Dbm,
        Unit::Bytes => pb::Unit::Bytes,
        Unit::Percent => pb::Unit::Percent,
        Unit::Degrees => pb::Unit::Degrees,
        Unit::Meters => pb::Unit::Meters,
        Unit::Celsius => pb::Unit::Celsius,
        Unit::Pascal => pb::Unit::Pascal,
        Unit::Ppm => pb::Unit::Ppm,
        Unit::Ppb => pb::Unit::Ppb,
        Unit::Decibels => pb::Unit::Decibels,
        Unit::MicrogramsPerCubicMeter => pb::Unit::MicrogramsPerCubicMeter,
        Unit::MicrosievertsPerHour => pb::Unit::MicrosievertsPerHour,
    }
}

pub(crate) fn field(field: &FieldDescriptor) -> pb::FieldDescriptor {
    pb::FieldDescriptor {
        id: field.id,
        name: text(field.name),
        kind: kind(field.kind),
        unit: unit(field.unit),
        enum_id: field.enum_id,
    }
}

/// The wire form of `fields`, at most `N` of them.
pub(crate) fn fields<const N: usize>(fields: &[FieldDescriptor]) -> Vec<pb::FieldDescriptor, N> {
    let mut out = Vec::new();
    for descriptor in fields {
        if out.push(field(descriptor)).is_err() {
            break;
        }
    }
    out
}

/// The wire form of `enums`, at most `N` of them.
pub(crate) fn enums<const N: usize>(enums: &[EnumDescriptor]) -> Vec<pb::EnumDescriptor, N> {
    let mut out = Vec::new();
    for descriptor in enums {
        let mut variants = Vec::new();
        for variant in descriptor.variants {
            let wire = pb::EnumVariant {
                value: variant.value,
                name: text(variant.name),
            };
            if variants.push(wire).is_err() {
                break;
            }
        }
        let wire = pb::EnumDescriptor {
            id: descriptor.id,
            variants,
        };
        if out.push(wire).is_err() {
            break;
        }
    }
    out
}

pub(crate) fn issue_descriptors<const N: usize>(
    issues: &[IssueDescriptor],
) -> Vec<pb::IssueDescriptor, N> {
    let mut out = Vec::new();
    for issue in issues {
        let wire = pb::IssueDescriptor {
            code: issue.code,
            name: text(issue.name),
        };
        if out.push(wire).is_err() {
            break;
        }
    }
    out
}

pub(crate) fn config_field(config: &ConfigField) -> pb::ConfigFieldDescriptor {
    let mut out = pb::ConfigFieldDescriptor {
        writable: config.writable,
        secret: config.secret,
        optional: config.optional,
        ..Default::default()
    };
    out.set_field(field(&config.field));
    out
}

pub(crate) fn section_info(info: &SectionInfo) -> pb::ConfigSectionInfo {
    pb::ConfigSectionInfo {
        id: info.id,
        name: text(info.name),
        version: info.version,
    }
}

pub(crate) fn config_entry(entry: &ConfigEntry) -> pb::ConfigEntry {
    use pb::ConfigEntry_::State;
    pb::ConfigEntry {
        field_id: entry.field_id,
        state: Some(match &entry.state {
            EntryState::Value(v) => entry_case(v),
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
            Operation::Clear(_) => MutationOp::Clear,
            Operation::None(_) => MutationOp::None,
            set => MutationOp::Set(from_mutation_case(set)?),
        },
    })
}

pub(crate) fn wipe_mutation(mutation: &mut pb::ConfigMutation) {
    if let Some(case) = &mut mutation.operation {
        wipe_mutation_case(case);
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

/// A duration in milliseconds that saturates instead of wrapping.
fn millis(ms: u64) -> u32 {
    u32::try_from(ms).unwrap_or(u32::MAX)
}

pub(crate) fn summary(summary: &ServiceStatusSummary) -> pb::ServiceStatusSummary {
    pb::ServiceStatusSummary {
        service_id: summary.id,
        name: text(summary.name),
        version: summary.version,
        state: state(summary.state),
        health: health(summary.health),
        ready: summary.ready,
        revision: summary.revision,
        state_age_ms: millis(summary.state_age_ms),
    }
}

pub(crate) fn issue(issue: &Issue) -> pb::ServiceIssue {
    pb::ServiceIssue {
        code: issue.code,
        severity: match issue.severity {
            IssueSeverity::Info => pb::IssueSeverity::Info,
            IssueSeverity::Warning => pb::IssueSeverity::Warning,
            IssueSeverity::Error => pb::IssueSeverity::Error,
            IssueSeverity::Critical => pb::IssueSeverity::Critical,
        },
        age_ms: millis(issue.age_ms.unwrap_or(0)),
    }
}

pub(crate) fn action_info(
    service_id: u32,
    service: &str,
    action: &ActionDescriptor,
) -> pb::ActionInfo {
    pb::ActionInfo {
        service_id,
        service: text(service),
        id: action.id,
        name: text(action.name),
        privileged: action.privileged,
        secret_args: action.secret_args,
    }
}
