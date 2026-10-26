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

use rohi_control_proto::control as pb;

use crate::model::*;

pub(crate) fn info(info: pb::GetInfoResponse) -> DeviceInfo {
    DeviceInfo {
        firmware: info.firmware.as_str().to_owned(),
        board: info.board.as_str().to_owned(),
    }
}

pub(crate) fn value(value: &pb::Value) -> Option<Value> {
    use pb::Value_::Value as V;
    Some(match value.value.as_ref()? {
        V::BoolValue(v) => Value::Bool(*v),
        V::I64Value(v) => Value::I64(*v),
        V::U64Value(v) => Value::U64(*v),
        V::F64Value(v) => Value::F64(*v),
        V::StringValue(v) => Value::String(v.as_str().to_owned()),
        V::BytesValue(v) => Value::Bytes(v.to_vec()),
        V::EnumValue(v) => Value::Enum(v.as_str().to_owned()),
    })
}

pub(crate) fn to_proto(value: &Value) -> Option<pb::Value> {
    use pb::Value_::Value as V;
    Some(pb::Value {
        value: Some(match value {
            Value::Bool(v) => V::BoolValue(*v),
            Value::I64(v) => V::I64Value(*v),
            Value::U64(v) => V::U64Value(*v),
            Value::F64(v) => V::F64Value(*v),
            Value::String(v) => V::StringValue(v.as_str().try_into().ok()?),
            Value::Bytes(v) => V::BytesValue(heapless::Vec::from_slice(v).ok()?),
            Value::Enum(v) => V::EnumValue(v.as_str().try_into().ok()?),
        }),
    })
}

pub(crate) fn field_value(field: &pb::FieldValue) -> Option<FieldValue> {
    Some(FieldValue {
        field_id: field.field_id,
        value: value(field.value()?)?,
    })
}

pub(crate) fn field(field: &pb::FieldDescriptor) -> FieldDescriptor {
    FieldDescriptor {
        id: field.id,
        name: field.name.as_str().to_owned(),
        kind: ValueKind::from_proto(field.kind),
        description: field.description.as_str().to_owned(),
        unit: field.unit.as_str().to_owned(),
        variants: field
            .variants
            .iter()
            .map(|variant| variant.as_str().to_owned())
            .collect(),
    }
}

pub(crate) fn section(info: &pb::ConfigSectionInfo) -> SectionInfo {
    SectionInfo {
        name: info.name.as_str().to_owned(),
        version: info.version,
        revision: info.revision,
    }
}

pub(crate) fn config_schema(schema: &pb::ConfigSectionSchema) -> Option<ConfigSchema> {
    Some(ConfigSchema {
        section: section(schema.section()?),
        fields: schema
            .fields
            .iter()
            .filter_map(|field| {
                Some(ConfigField {
                    field: self::field(field.field()?),
                    writable: field.writable,
                    secret: field.secret,
                    optional: field.optional,
                    min: field.min().copied(),
                    max: field.max().copied(),
                })
            })
            .collect(),
    })
}

pub(crate) fn config_values(response: &pb::GetConfigSectionResponse) -> ConfigValues {
    ConfigValues {
        section: SectionInfo {
            name: response.section.as_str().to_owned(),
            version: response.version,
            revision: response.revision,
        },
        configured: response.configured,
        entries: response
            .entries
            .iter()
            .filter_map(|entry| {
                let state = match entry.state.as_ref()? {
                    pb::ConfigEntry_::State::Value(v) => EntryState::Value(value(v)?),
                    pb::ConfigEntry_::State::Redacted(_) => EntryState::Redacted,
                };
                Some(ConfigEntry {
                    field_id: entry.field_id,
                    state,
                })
            })
            .collect(),
    }
}

pub(crate) fn mutation(mutation: &Mutation) -> Option<pb::ConfigMutation> {
    Some(pb::ConfigMutation {
        field_id: mutation.field_id,
        operation: Some(match &mutation.op {
            MutationOp::Set(value) => pb::ConfigMutation_::Operation::Set(to_proto(value)?),
            MutationOp::Unset => pb::ConfigMutation_::Operation::Unset(pb::Empty {}),
        }),
    })
}

pub(crate) fn summary(summary: &pb::ServiceStatusSummary) -> ServiceSummary {
    ServiceSummary {
        service: summary.service.as_str().to_owned(),
        state: ServiceState::from_proto(summary.state),
        health: ServiceHealth::from_proto(summary.health),
        ready: summary.ready,
        revision: summary.revision,
        state_age_ms: summary.state_age_ms,
        reason: summary.reason().map(|reason| reason.as_str().to_owned()),
    }
}

pub(crate) fn issue(issue: &pb::ServiceIssue) -> Issue {
    Issue {
        severity: IssueSeverity::from_proto(issue.severity),
        code: issue.code.as_str().to_owned(),
        message: issue.message.as_str().to_owned(),
        age_ms: issue.age_ms,
    }
}

pub(crate) fn action_info(info: &pb::ActionInfo) -> ActionInfo {
    ActionInfo {
        service: info.service.as_str().to_owned(),
        name: info.name.as_str().to_owned(),
        description: info.description.as_str().to_owned(),
        privileged: info.privileged,
        secret_args: info.secret_args,
    }
}
