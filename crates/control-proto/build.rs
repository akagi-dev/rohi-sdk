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

use micropb_gen::config::IntSize;
use micropb_gen::{Config, Generator};

const PROTO: &str = "proto/rohi/control/v2/control.proto";

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
        "MAX_BUILD_LEN",
        48,
        "Maximum length of the firmware build identifier, in bytes.",
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
        64,
        "Maximum length of a string or bytes status value or action result, in bytes.",
    ),
    (
        "MAX_ENUMS",
        3,
        "Maximum number of enums in one config schema response.",
    ),
    (
        "MAX_SERVICE_ENUMS",
        1,
        "Maximum number of enums in one service or action schema response.",
    ),
    (
        "MAX_SELECTIONS",
        2,
        "Maximum number of bitset selections in one action schema response.",
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
        16,
        "Maximum number of config sections a device lists.",
    ),
    (
        "MAX_SERVICES",
        16,
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
        "MAX_STREAM_CHUNK",
        512,
        "Maximum number of bytes one `ReadStreamResponse` carries.",
    ),
    (
        "MAX_PAGE_TOKEN_LEN",
        6,
        "Maximum length of the opaque page token of a `Records` result, in bytes.",
    ),
    (
        "MAX_STREAM_FILES",
        24,
        "Maximum number of files of one stream.",
    ),
    (
        "MAX_FEED_CHUNK",
        512,
        "Maximum number of bytes of data one `FeedPage` carries.",
    ),
    (
        "MAX_REASON_LEN",
        32,
        "Maximum length of the reason of a rejected action, in bytes.",
    ),
];

/// Repeated fields: bounded by element count, not by byte length.
const REPEATED: &[&str] = &[
    ".rohi.control.v2.GetConfigSchemaResponse.fields",
    ".rohi.control.v2.GetConfigSchemaResponse.enums",
    ".rohi.control.v2.ListConfigSectionsResponse.sections",
    ".rohi.control.v2.GetConfigSectionResponse.effective",
    ".rohi.control.v2.GetConfigSectionResponse.overridden",
    ".rohi.control.v2.PatchConfigSectionRequest.mutations",
    ".rohi.control.v2.GetServicesStatusResponse.services",
    ".rohi.control.v2.GetServiceSchemaResponse.fields",
    ".rohi.control.v2.GetServiceSchemaResponse.enums",
    ".rohi.control.v2.GetServiceSchemaResponse.issues",
    ".rohi.control.v2.GetServiceStatusResponse.fields",
    ".rohi.control.v2.GetServiceStatusResponse.issues",
    ".rohi.control.v2.ListActionsResponse.actions",
    ".rohi.control.v2.GetActionSchemaResponse.args",
    ".rohi.control.v2.GetActionSchemaResponse.results",
    ".rohi.control.v2.GetActionSchemaResponse.enums",
    ".rohi.control.v2.GetActionSchemaResponse.selections",
    ".rohi.control.v2.InvokeActionRequest.args",
    ".rohi.control.v2.ActionRecord.fields",
    ".rohi.control.v2.InlineResult.fields",
    ".rohi.control.v2.RecordsPage.records",
    ".rohi.control.v2.EnumDescriptor.variants",
    ".rohi.control.v2.StreamInfo.files",
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
        (".rohi.control.v2.GetInfoResponse.firmware", "MAX_TEXT_LEN"),
        (".rohi.control.v2.GetInfoResponse.board", "MAX_TEXT_LEN"),
        (".rohi.control.v2.GetInfoResponse.build", "MAX_BUILD_LEN"),
        (".rohi.control.v2.Error.reason", "MAX_REASON_LEN"),
        // Values.
        (
            ".rohi.control.v2.FieldValue.string_value",
            "MAX_FIELD_VALUE_LEN",
        ),
        (
            ".rohi.control.v2.FieldValue.bytes_value",
            "MAX_FIELD_VALUE_LEN",
        ),
        (".rohi.control.v2.ActionArg.string_value", "MAX_VALUE_LEN"),
        (".rohi.control.v2.ActionArg.bytes_value", "MAX_VALUE_LEN"),
        (".rohi.control.v2.ConfigEntry.string_value", "MAX_VALUE_LEN"),
        (".rohi.control.v2.ConfigEntry.bytes_value", "MAX_VALUE_LEN"),
        (
            ".rohi.control.v2.ConfigMutation.string_value",
            "MAX_VALUE_LEN",
        ),
        (
            ".rohi.control.v2.ConfigMutation.bytes_value",
            "MAX_VALUE_LEN",
        ),
        // Descriptors.
        (".rohi.control.v2.FieldDescriptor.name", "MAX_NAME_LEN"),
        (".rohi.control.v2.EnumVariant.name", "MAX_VARIANT_NAME_LEN"),
        (
            ".rohi.control.v2.EnumDescriptor.variants",
            "MAX_ENUM_VARIANTS",
        ),
        (".rohi.control.v2.IssueDescriptor.name", "MAX_NAME_LEN"),
        // Config.
        (".rohi.control.v2.ConfigSectionInfo.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v2.ListConfigSectionsResponse.sections",
            "MAX_CONFIG_SECTIONS",
        ),
        (
            ".rohi.control.v2.GetConfigSchemaResponse.fields",
            "MAX_CONFIG_FIELDS",
        ),
        (
            ".rohi.control.v2.GetConfigSchemaResponse.enums",
            "MAX_ENUMS",
        ),
        (
            ".rohi.control.v2.GetConfigSectionResponse.effective",
            "MAX_CONFIG_FIELDS",
        ),
        (
            ".rohi.control.v2.GetConfigSectionResponse.overridden",
            "MAX_CONFIG_FIELDS",
        ),
        (
            ".rohi.control.v2.PatchConfigSectionRequest.mutations",
            "MAX_MUTATIONS",
        ),
        // Services.
        (".rohi.control.v2.ServiceStatusSummary.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v2.GetServicesStatusResponse.services",
            "MAX_SERVICES",
        ),
        (
            ".rohi.control.v2.GetServiceSchemaResponse.name",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v2.GetServiceSchemaResponse.fields",
            "MAX_SCHEMA_FIELDS",
        ),
        (
            ".rohi.control.v2.GetServiceSchemaResponse.enums",
            "MAX_SERVICE_ENUMS",
        ),
        (
            ".rohi.control.v2.GetServiceSchemaResponse.issues",
            "MAX_ISSUE_DESCRIPTORS",
        ),
        (
            ".rohi.control.v2.GetServiceStatusResponse.fields",
            "MAX_STATUS_FIELDS",
        ),
        (
            ".rohi.control.v2.GetServiceStatusResponse.issues",
            "MAX_ISSUES",
        ),
        // Actions.
        (".rohi.control.v2.ActionInfo.name", "MAX_NAME_LEN"),
        (".rohi.control.v2.ActionInfo.service", "MAX_NAME_LEN"),
        (
            ".rohi.control.v2.ListActionsResponse.actions",
            "MAX_ACTIONS",
        ),
        (
            ".rohi.control.v2.GetActionSchemaResponse.args",
            "MAX_ACTION_ARGS",
        ),
        (
            ".rohi.control.v2.GetActionSchemaResponse.results",
            "MAX_RECORD_FIELDS",
        ),
        (
            ".rohi.control.v2.GetActionSchemaResponse.enums",
            "MAX_SERVICE_ENUMS",
        ),
        (
            ".rohi.control.v2.GetActionSchemaResponse.selections",
            "MAX_SELECTIONS",
        ),
        (
            ".rohi.control.v2.InvokeActionRequest.args",
            "MAX_ACTION_ARGS",
        ),
        (".rohi.control.v2.ActionRecord.fields", "MAX_RECORD_FIELDS"),
        (".rohi.control.v2.InlineResult.fields", "MAX_RECORD_FIELDS"),
        (".rohi.control.v2.RecordsPage.records", "MAX_RECORDS"),
        (
            ".rohi.control.v2.RecordsPage.page_token",
            "MAX_PAGE_TOKEN_LEN",
        ),
        (
            ".rohi.control.v2.ReadResultPageRequest.page_token",
            "MAX_PAGE_TOKEN_LEN",
        ),
        (
            ".rohi.control.v2.CloseResultPageRequest.page_token",
            "MAX_PAGE_TOKEN_LEN",
        ),
        // Streams.
        (".rohi.control.v2.StreamFile.name", "MAX_NAME_LEN"),
        (".rohi.control.v2.StreamInfo.files", "MAX_STREAM_FILES"),
        (
            ".rohi.control.v2.ReadStreamResponse.data",
            "MAX_STREAM_CHUNK",
        ),
        // Feeds.
        (".rohi.control.v2.FeedPage.data", "MAX_FEED_CHUNK"),
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

    // Table ids are small: at most MAX_SELECTIONS.
    generator.configure(
        ".rohi.control.v2.SelectionDescriptor.id",
        Config::new().int_size(IntSize::S8),
    );

    generator.configure(
        ".rohi.control.v2.ConfigMutation",
        Config::new().no_debug_impl(true),
    );
    generator.configure(
        ".rohi.control.v2.InvokeActionRequest",
        Config::new().no_debug_impl(true),
    );

    generator
        .compile_protos(&[PROTO], out_dir.join("control.rs"))
        .expect("compile control.proto");
}
