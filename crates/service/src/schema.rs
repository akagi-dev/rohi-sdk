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
//! Compile-time validity of schemas.
//!
//! Requests address sections, services, actions and fields by number and
//! clients resolve names from the schemas, so a schema that repeats an id or a
//! name, or whose names do not fit the wire, would silently route requests to
//! the wrong place or truncate what clients see. Every table is checked in a
//! `const` block when its section or service is constructed: a violation is a
//! build error, never a surprise on the device.

use crate::action::{ActionDescriptor, MAX_ACTION_ARGS, MAX_RECORD_FIELDS};
use crate::field::{ConfigField, EnumDescriptor, FieldDescriptor};
use crate::status::{FIRST_SERVICE_ISSUE, ISSUE_STARTUP_FAILED, IssueDescriptor};
use crate::{
    MAX_ENUM_VARIANTS, MAX_ENUMS, MAX_ISSUE_DESCRIPTORS, MAX_NAME_LEN, MAX_VARIANT_NAME_LEN,
};
use rohi_config::ValueKind;

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// A name that is not empty and fits `max` bytes, so it crosses the wire
/// unchanged.
pub(crate) const fn name_fits(name: &str, max: usize) -> bool {
    !name.is_empty() && name.len() <= max
}

/// A section, service or action name.
pub(crate) const fn name_ok(name: &str) -> bool {
    name_fits(name, MAX_NAME_LEN)
}

const fn field_ok(field: &FieldDescriptor, enums: &[EnumDescriptor]) -> bool {
    if !name_ok(field.name) {
        return false;
    }
    if !matches!(field.kind, ValueKind::Enum) {
        return field.enum_id == 0;
    }
    let mut index = 0;
    while index < enums.len() {
        if enums[index].id == field.enum_id {
            return true;
        }
        index += 1;
    }
    false
}

const fn distinct(a: &FieldDescriptor, b: &FieldDescriptor) -> bool {
    a.id != b.id && !same(a.name, b.name)
}

/// Whether the fields are valid: names fit, ids and names are unique, every
/// enum field refers to an enum of `enums` and no other field does.
pub(crate) const fn fields_valid(fields: &[FieldDescriptor], enums: &[EnumDescriptor]) -> bool {
    let mut index = 0;
    while index < fields.len() {
        if !field_ok(&fields[index], enums) {
            return false;
        }
        let mut other = 0;
        while other < index {
            if !distinct(&fields[index], &fields[other]) {
                return false;
            }
            other += 1;
        }
        index += 1;
    }
    true
}

/// [`fields_valid`] for the fields of a config section.
pub(crate) const fn config_fields_valid(fields: &[ConfigField], enums: &[EnumDescriptor]) -> bool {
    let mut index = 0;
    while index < fields.len() {
        if !field_ok(&fields[index].field, enums) {
            return false;
        }
        let mut other = 0;
        while other < index {
            if !distinct(&fields[index].field, &fields[other].field) {
                return false;
            }
            other += 1;
        }
        index += 1;
    }
    true
}

/// Whether the writable schema fields and the overridable fields (`FIELD_IDS`
/// of the generated overrides) agree: a writable field needs an override to be
/// persisted, and an override without a schema field could never be changed or
/// reported. Read-only fields (computed values) have no override.
pub(crate) const fn field_ids_match(fields: &[ConfigField], ids: &[u32]) -> bool {
    let mut index = 0;
    while index < fields.len() {
        if fields[index].writable && !contains(ids, fields[index].field.id) {
            return false;
        }
        index += 1;
    }
    let mut index = 0;
    while index < ids.len() {
        let mut found = false;
        let mut other = 0;
        while other < fields.len() {
            if fields[other].field.id == ids[index] {
                found = true;
            }
            other += 1;
        }
        if !found {
            return false;
        }
        index += 1;
    }
    true
}

const fn contains(ids: &[u32], id: u32) -> bool {
    let mut index = 0;
    while index < ids.len() {
        if ids[index] == id {
            return true;
        }
        index += 1;
    }
    false
}

/// Whether the enums fit the limits of the control protocol and are unique:
/// ids are non-zero and unique, variant values and names are unique within
/// their enum and names fit the wire.
pub(crate) const fn enums_valid(enums: &[EnumDescriptor]) -> bool {
    if enums.len() > MAX_ENUMS {
        return false;
    }
    let mut index = 0;
    while index < enums.len() {
        let descriptor = &enums[index];
        if descriptor.id == 0 || descriptor.variants.len() > MAX_ENUM_VARIANTS {
            return false;
        }
        let mut other = 0;
        while other < index {
            if enums[other].id == descriptor.id {
                return false;
            }
            other += 1;
        }
        let variants = descriptor.variants;
        let mut variant = 0;
        while variant < variants.len() {
            if !name_fits(variants[variant].name, MAX_VARIANT_NAME_LEN) {
                return false;
            }
            let mut before = 0;
            while before < variant {
                if variants[before].value == variants[variant].value
                    || same(variants[before].name, variants[variant].name)
                {
                    return false;
                }
                before += 1;
            }
            variant += 1;
        }
        index += 1;
    }
    true
}

/// Whether the issue descriptors are valid: codes are in the range of the
/// service (or the SDK startup code) and unique, names fit and are unique.
pub(crate) const fn issues_valid(issues: &[IssueDescriptor]) -> bool {
    if issues.len() > MAX_ISSUE_DESCRIPTORS {
        return false;
    }
    let mut index = 0;
    while index < issues.len() {
        let issue = &issues[index];
        if !(issue.code >= FIRST_SERVICE_ISSUE || issue.code == ISSUE_STARTUP_FAILED)
            || !name_ok(issue.name)
        {
            return false;
        }
        let mut other = 0;
        while other < index {
            if issues[other].code == issue.code || same(issues[other].name, issue.name) {
                return false;
            }
            other += 1;
        }
        index += 1;
    }
    true
}

/// Whether the actions are valid: names fit, ids and names are unique, the
/// arguments and results fit the protocol and are valid fields of the enums of
/// their action.
pub(crate) const fn actions_valid(actions: &[ActionDescriptor]) -> bool {
    let mut index = 0;
    while index < actions.len() {
        let action = &actions[index];
        if !name_ok(action.name)
            || action.args.len() > MAX_ACTION_ARGS
            || action.results.len() > MAX_RECORD_FIELDS
            || !enums_valid(action.enums)
            || !fields_valid(action.args, action.enums)
            || !fields_valid(action.results, action.enums)
        {
            return false;
        }
        let mut other = 0;
        while other < index {
            if actions[other].id == action.id || same(actions[other].name, action.name) {
                return false;
            }
            other += 1;
        }
        index += 1;
    }
    true
}
