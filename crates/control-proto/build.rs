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

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use micropb_gen::{Config, Generator};

const PROTO: &str = "proto/rohi/control/v1/control.proto";

/// Compile-time bounds of the protocol. Written to `limits.rs` and applied to
/// the generated types, so the schema and the Rust constants cannot drift.
const LIMITS: &[(&str, usize, &str)] = &[
    (
        "MAX_REQUEST_LEN",
        768,
        "Maximum size of an encoded `Request`, in bytes.",
    ),
    (
        "MAX_RESPONSE_LEN",
        2048,
        "Maximum size of an encoded `Response`, in bytes.",
    ),
    (
        "MAX_TEXT_LEN",
        32,
        "Maximum length of a name or version string.",
    ),
    (
        "MAX_NAME_LEN",
        24,
        "Maximum length of a section, service, action, field or issue name, in bytes.",
    ),
    (
        "MAX_VALUE_LEN",
        64,
        "Maximum length of a string or bytes value of a request or config, in bytes.",
    ),
    (
        "MAX_FIELD_VALUE_LEN",
        32,
        "Maximum length of a string or bytes status value or action result, in bytes.",
    ),
    (
        "MAX_ENUMS",
        1,
        "Maximum number of enums in one schema response.",
    ),
    (
        "MAX_ENUM_VARIANTS",
        9,
        "Maximum number of variants of an enum.",
    ),
    (
        "MAX_VARIANT_NAME_LEN",
        16,
        "Maximum length of an enum variant name, in bytes.",
    ),
    (
        "MAX_SCHEMA_FIELDS",
        20,
        "Maximum number of field descriptors in one schema response.",
    ),
    (
        "MAX_STATUS_FIELDS",
        20,
        "Maximum number of status values in one status response.",
    ),
    (
        "MAX_CONFIG_FIELDS",
        16,
        "Maximum number of fields of a config section.",
    ),
    (
        "MAX_MUTATIONS",
        8,
        "Maximum number of mutations in one patch request.",
    ),
    (
        "MAX_CONFIG_SECTIONS",
        12,
        "Maximum number of config sections a device lists.",
    ),
    (
        "MAX_SERVICES",
        12,
        "Maximum number of services a device lists.",
    ),
    (
        "MAX_ISSUES",
        4,
        "Maximum number of active issues reported for one service.",
    ),
    (
        "MAX_ISSUE_DESCRIPTORS",
        12,
        "Maximum number of issues a service schema describes.",
    ),
    (
        "MAX_ACTIONS",
        12,
        "Maximum number of actions a device lists.",
    ),
    (
        "MAX_ACTION_ARGS",
        3,
        "Maximum number of arguments of an action.",
    ),
    (
        "MAX_RECORDS",
        6,
        "Maximum number of records an action returns.",
    ),
    (
        "MAX_RECORD_FIELDS",
        4,
        "Maximum number of fields of one action record.",
    ),
    (
        "MAX_REASON_LEN",
        32,
        "Maximum length of the reason of a rejected action, in bytes.",
    ),
];

/// Repeated fields: bounded by element count, not by byte length.
const REPEATED: &[&str] = &[
    ".rohi.control.v1.GetConfigSchemaResponse.fields",
    ".rohi.control.v1.GetConfigSchemaResponse.enums",
    ".rohi.control.v1.ListConfigSectionsResponse.sections",
    ".rohi.control.v1.GetConfigSectionResponse.effective",
    ".rohi.control.v1.GetConfigSectionResponse.overridden",
    ".rohi.control.v1.PatchConfigSectionRequest.mutations",
    ".rohi.control.v1.GetServicesStatusResponse.services",
    ".rohi.control.v1.GetServiceSchemaResponse.fields",
    ".rohi.control.v1.GetServiceSchemaResponse.enums",
    ".rohi.control.v1.GetServiceSchemaResponse.issues",
    ".rohi.control.v1.GetServiceStatusResponse.fields",
    ".rohi.control.v1.GetServiceStatusResponse.issues",
    ".rohi.control.v1.ListActionsResponse.actions",
    ".rohi.control.v1.GetActionSchemaResponse.args",
    ".rohi.control.v1.GetActionSchemaResponse.results",
    ".rohi.control.v1.GetActionSchemaResponse.enums",
    ".rohi.control.v1.InvokeActionRequest.args",
    ".rohi.control.v1.ActionRecord.fields",
    ".rohi.control.v1.InvokeActionResponse.records",
    ".rohi.control.v1.EnumDescriptor.variants",
];

fn limit(name: &str) -> usize {
    LIMITS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, value, _)| *value)
        .expect("unknown limit")
}

fn main() {
    println!("cargo:rerun-if-changed={PROTO}");
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    let mut limits = String::new();
    for (name, value, doc) in LIMITS {
        writeln!(limits, "/// {doc}\npub const {name}: usize = {value};").unwrap();
    }
    fs::write(out_dir.join("limits.rs"), limits).expect("write limits.rs");

    let mut generator = Generator::with_warning_callback(|_| {});
    generator.use_container_heapless();

    let bounded = [
        (".rohi.control.v1.GetInfoResponse.firmware", "MAX_TEXT_LEN"),
        (".rohi.control.v1.GetInfoResponse.board", "MAX_TEXT_LEN"),
        (".rohi.control.v1.Error.reason", "MAX_REASON_LEN"),
        // Values.
        (
            ".rohi.control.v1.FieldValue.string_value",
            "MAX_FIELD_VALUE_LEN",
        ),
        (
            ".rohi.control.v1.FieldValue.bytes_value",
            "MAX_FIELD_VALUE_LEN",
        ),
        (".rohi.control.v1.ActionArg.string_value", "MAX_VALUE_LEN"),
        (".rohi.control.v1.ActionArg.bytes_value", "MAX_VALUE_LEN"),
        (".rohi.control.v1.ConfigEntry.string_value", "MAX_VALUE_LEN"),
        (".rohi.control.v1.ConfigEntry.bytes_value", "MAX_VALUE_LEN"),
        (
            ".rohi.control.v1.ConfigMutation.string_value",
            "MAX_VALUE_LEN",
        ),
        (
            ".rohi.control.v1.ConfigMutation.bytes_value",
            "MAX_VALUE_LEN",
        ),
        // Descriptors.
        (".rohi.control.v1.FieldDescriptor.name", "MAX_NAME_LEN"),
        (".rohi.control.v1.EnumVariant.name", "MAX_VARIANT_NAME_LEN"),
        (
            ".rohi.control.v1.EnumDescriptor.variants",
            "MAX_ENUM_VARIANTS",
        ),
        (".rohi.control.v1.IssueDescriptor.name", "MAX_NAME_LEN"),
        // Config.
        (".rohi.control.v1.ConfigSectionInfo.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v1.ListConfigSectionsResponse.sections",
            "MAX_CONFIG_SECTIONS",
        ),
        (
            ".rohi.control.v1.GetConfigSchemaResponse.fields",
            "MAX_CONFIG_FIELDS",
        ),
        (
            ".rohi.control.v1.GetConfigSchemaResponse.enums",
            "MAX_ENUMS",
        ),
        (
            ".rohi.control.v1.GetConfigSectionResponse.effective",
            "MAX_CONFIG_FIELDS",
        ),
        (
            ".rohi.control.v1.GetConfigSectionResponse.overridden",
            "MAX_CONFIG_FIELDS",
        ),
        (
            ".rohi.control.v1.PatchConfigSectionRequest.mutations",
            "MAX_MUTATIONS",
        ),
        // Services.
        (".rohi.control.v1.ServiceStatusSummary.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v1.GetServicesStatusResponse.services",
            "MAX_SERVICES",
        ),
        (
            ".rohi.control.v1.GetServiceSchemaResponse.name",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetServiceSchemaResponse.fields",
            "MAX_SCHEMA_FIELDS",
        ),
        (
            ".rohi.control.v1.GetServiceSchemaResponse.enums",
            "MAX_ENUMS",
        ),
        (
            ".rohi.control.v1.GetServiceSchemaResponse.issues",
            "MAX_ISSUE_DESCRIPTORS",
        ),
        (
            ".rohi.control.v1.GetServiceStatusResponse.fields",
            "MAX_STATUS_FIELDS",
        ),
        (
            ".rohi.control.v1.GetServiceStatusResponse.issues",
            "MAX_ISSUES",
        ),
        // Actions.
        (".rohi.control.v1.ActionInfo.name", "MAX_NAME_LEN"),
        (".rohi.control.v1.ActionInfo.service", "MAX_NAME_LEN"),
        (
            ".rohi.control.v1.ListActionsResponse.actions",
            "MAX_ACTIONS",
        ),
        (
            ".rohi.control.v1.GetActionSchemaResponse.args",
            "MAX_ACTION_ARGS",
        ),
        (
            ".rohi.control.v1.GetActionSchemaResponse.results",
            "MAX_RECORD_FIELDS",
        ),
        (
            ".rohi.control.v1.GetActionSchemaResponse.enums",
            "MAX_ENUMS",
        ),
        (
            ".rohi.control.v1.InvokeActionRequest.args",
            "MAX_ACTION_ARGS",
        ),
        (".rohi.control.v1.ActionRecord.fields", "MAX_RECORD_FIELDS"),
        (
            ".rohi.control.v1.InvokeActionResponse.records",
            "MAX_RECORDS",
        ),
    ];
    for (path, name) in bounded {
        let size = limit(name) as u32;
        let config = if REPEATED.contains(&path) {
            Config::new().max_len(size)
        } else {
            Config::new().max_bytes(size)
        };
        generator.configure(path, config);
    }

    generator.configure(
        ".rohi.control.v1.ConfigMutation",
        Config::new().no_debug_impl(true),
    );
    generator.configure(
        ".rohi.control.v1.InvokeActionRequest",
        Config::new().no_debug_impl(true),
    );

    generator
        .compile_protos(&[PROTO], out_dir.join("control.rs"))
        .expect("compile control.proto");
}
