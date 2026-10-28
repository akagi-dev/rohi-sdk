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
//! Protobuf <-> model conversion.
//! Protobuf <-> model conversion.
//!
//! The wire carries enum values and issues as numbers. The schema of the
//! device names them; the conversions here translate with it, so the model
//! and the command line only ever see names.

use rohi_control_proto::control as pb;

use crate::model::*;

pub(crate) fn info(info: pb::GetInfoResponse) -> DeviceInfo {
    DeviceInfo {
        firmware: info.firmware.as_str().to_owned(),
        board: info.board.as_str().to_owned(),
    }
}

/// The field of `schema` with the given id.
fn find(schema: &[FieldDescriptor], id: u32) -> Option<&FieldDescriptor> {
    schema.iter().find(|field| field.id == id)
}

/// The name of the variant `value` of `field`; numbers the schema does not
/// list are kept readable instead of dropped.
fn variant_name(field: Option<&FieldDescriptor>, value: u32) -> String {
    field
        .and_then(|field| {
            let index = field.variant_values.iter().position(|v| *v == value)?;
            field.variants.get(index).cloned()
        })
        .unwrap_or_else(|| format!("#{value}"))
}

/// The number of the variant `name` of `field`.
fn variant_value(field: Option<&FieldDescriptor>, name: &str) -> Option<u32> {
    let field = field?;
    let index = field.variants.iter().position(|variant| variant == name)?;
    field.variant_values.get(index).copied()
}

// `FieldValue`, `ConfigEntry` and `ConfigMutation` repeat the value cases in
// their own oneof.
macro_rules! value_codec {
    ($from:ident, $to:ident, $oneof:ty) => {
        /// `None` for a case that carries no value.
        #[allow(dead_code)]
        fn $from(value: &$oneof, field: Option<&FieldDescriptor>) -> Option<Value> {
            type V = $oneof;
            Some(match value {
                V::BoolValue(v) => Value::Bool(*v),
                V::I64Value(v) => Value::I64(*v),
                V::U64Value(v) => Value::U64(*v),
                V::F32Value(v) => Value::F32(*v),
                V::StringValue(v) => Value::String(v.as_str().to_owned()),
                V::BytesValue(v) => Value::Bytes(v.to_vec()),
                V::EnumValue(v) => Value::Enum(variant_name(field, *v)),
                #[allow(unreachable_patterns)]
                _ => return None,
            })
        }

        /// `None` for a value that does not fit the protocol or names a
        /// variant the field does not have.
        #[allow(dead_code)]
        fn $to(value: &Value, field: Option<&FieldDescriptor>) -> Option<$oneof> {
            type V = $oneof;
            Some(match value {
                Value::Bool(v) => V::BoolValue(*v),
                Value::I64(v) => V::I64Value(*v),
                Value::U64(v) => V::U64Value(*v),
                Value::F32(v) => V::F32Value(*v),
                Value::String(v) => V::StringValue(v.as_str().try_into().ok()?),
                Value::Bytes(v) => V::BytesValue(heapless::Vec::from_slice(v).ok()?),
                Value::Enum(v) => V::EnumValue(variant_value(field, v)?),
            })
        }
    };
}

value_codec!(from_field_case, to_field_case, pb::FieldValue_::Value);
value_codec!(from_entry_case, to_entry_case, pb::ConfigEntry_::State);
value_codec!(
    from_mutation_case,
    to_mutation_case,
    pb::ConfigMutation_::Operation
);
value_codec!(from_arg_case, to_arg_case, pb::ActionArg_::Value);

pub(crate) fn field_value(
    field: &pb::FieldValue,
    schema: &[FieldDescriptor],
) -> Option<FieldValue> {
    Some(FieldValue {
        field_id: field.field_id,
        value: from_field_case(field.value.as_ref()?, find(schema, field.field_id))?,
    })
}

pub(crate) fn to_action_arg(arg: &FieldValue, schema: &[FieldDescriptor]) -> Option<pb::ActionArg> {
    Some(pb::ActionArg {
        field_id: arg.field_id,
        value: Some(to_arg_case(&arg.value, find(schema, arg.field_id))?),
    })
}

fn unit_symbol(unit: pb::Unit) -> &'static str {
    match unit {
        pb::Unit::Ms => "ms",
        pb::Unit::S => "s",
        pb::Unit::Hz => "Hz",
        pb::Unit::Dbm => "dBm",
        pb::Unit::Bytes => "B",
        pb::Unit::Percent => "%",
        pb::Unit::Degrees => "deg",
        pb::Unit::Meters => "m",
        pb::Unit::Celsius => "C",
        pb::Unit::Pascal => "Pa",
        pb::Unit::Ppm => "ppm",
        pb::Unit::Ppb => "ppb",
        pb::Unit::Decibels => "dB",
        pb::Unit::MicrogramsPerCubicMeter => "ug/m3",
        pb::Unit::MicrosievertsPerHour => "uSv/h",
        _ => "",
    }
}

/// A field with the variants of its enum taken from `enums`.
pub(crate) fn field(field: &pb::FieldDescriptor, enums: &[pb::EnumDescriptor]) -> FieldDescriptor {
    let table = enums.iter().find(|table| table.id == field.enum_id);
    FieldDescriptor {
        id: field.id,
        name: field.name.as_str().to_owned(),
        kind: ValueKind::from_proto(field.kind),
        unit: unit_symbol(field.unit).to_owned(),
        variants: table
            .map(|table| {
                table
                    .variants
                    .iter()
                    .map(|variant| variant.name.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default(),
        variant_values: table
            .map(|table| table.variants.iter().map(|variant| variant.value).collect())
            .unwrap_or_default(),
    }
}

pub(crate) fn section(info: &pb::ConfigSectionInfo) -> SectionInfo {
    SectionInfo {
        id: info.id,
        name: info.name.as_str().to_owned(),
        version: info.version,
        revision: info.revision,
    }
}

pub(crate) fn config_schema(response: &pb::GetConfigSchemaResponse) -> Option<ConfigSchema> {
    Some(ConfigSchema {
        section: section(response.section()?),
        fields: response
            .fields
            .iter()
            .filter_map(|field| {
                Some(ConfigField {
                    field: self::field(field.field()?, &response.enums),
                    writable: field.writable,
                    secret: field.secret,
                    optional: field.optional,
                })
            })
            .collect(),
    })
}

/// The content of a section; `schema` names its enum values.
pub(crate) fn config_values(
    response: &pb::GetConfigSectionResponse,
    schema: &ConfigSchema,
) -> ConfigValues {
    let fields: Vec<&FieldDescriptor> = schema.fields.iter().map(|field| &field.field).collect();
    let find = |id| fields.iter().copied().find(|field| field.id == id);
    ConfigValues {
        section: SectionInfo {
            id: response.section_id,
            name: schema.section.name.clone(),
            version: response.version,
            revision: response.revision,
        },
        overridden: response.overridden.iter().copied().collect(),
        entries: response
            .effective
            .iter()
            .filter_map(|entry| {
                let state = match entry.state.as_ref()? {
                    pb::ConfigEntry_::State::Redacted(_) => EntryState::Redacted,
                    value => EntryState::Value(from_entry_case(value, find(entry.field_id))?),
                };
                Some(ConfigEntry {
                    field_id: entry.field_id,
                    state,
                })
            })
            .collect(),
    }
}

pub(crate) fn mutation(mutation: &Mutation, schema: &ConfigSchema) -> Option<pb::ConfigMutation> {
    let field = schema
        .fields
        .iter()
        .map(|field| &field.field)
        .find(|field| field.id == mutation.field_id);
    Some(pb::ConfigMutation {
        field_id: mutation.field_id,
        operation: Some(match &mutation.op {
            MutationOp::Set(value) => to_mutation_case(value, field)?,
            MutationOp::None => pb::ConfigMutation_::Operation::None(pb::Empty {}),
            MutationOp::Clear => pb::ConfigMutation_::Operation::Clear(pb::Empty {}),
        }),
    })
}

pub(crate) fn summary(summary: &pb::ServiceStatusSummary) -> ServiceSummary {
    ServiceSummary {
        id: summary.service_id,
        service: summary.name.as_str().to_owned(),
        version: summary.version,
        state: ServiceState::from_proto(summary.state),
        health: ServiceHealth::from_proto(summary.health),
        ready: summary.ready,
        revision: summary.revision,
        state_age_ms: u64::from(summary.state_age_ms),
    }
}

/// Codes below 256 belong to the SDK and are not listed by service schemas.
fn builtin_issue(code: u32) -> Option<&'static str> {
    match code {
        1 => Some("startup_failed"),
        _ => None,
    }
}

pub(crate) fn issue(issue: &pb::ServiceIssue, names: &[IssueInfo]) -> Issue {
    let name = names
        .iter()
        .find(|info| info.code == issue.code)
        .map(|info| info.name.clone())
        .or_else(|| builtin_issue(issue.code).map(str::to_owned))
        .unwrap_or_else(|| format!("code_{}", issue.code));
    Issue {
        severity: IssueSeverity::from_proto(issue.severity),
        id: issue.code,
        code: name,
        age_ms: u64::from(issue.age_ms),
    }
}

/// `service` names the owner of the action.
pub(crate) fn action_info(info: &pb::ActionInfo, service: &str) -> ActionInfo {
    ActionInfo {
        service_id: info.service_id,
        service: service.to_owned(),
        id: info.id,
        name: info.name.as_str().to_owned(),
        privileged: info.privileged,
        secret_args: info.secret_args,
    }
}
