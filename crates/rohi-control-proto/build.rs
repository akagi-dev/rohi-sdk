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
        "Maximum length of a section, service or field name, in bytes.",
    ),
    (
        "MAX_VALUE_LEN",
        64,
        "Maximum length of a string or bytes `Value`, in bytes.",
    ),
    (
        "MAX_DESCRIPTION_LEN",
        40,
        "Maximum length of a field description, in bytes.",
    ),
    (
        "MAX_UNIT_LEN",
        8,
        "Maximum length of a field unit, in bytes.",
    ),
    (
        "MAX_VARIANTS",
        10,
        "Maximum number of variants of an enum field.",
    ),
    (
        "MAX_VARIANT_LEN",
        16,
        "Maximum length of an enum variant, in bytes.",
    ),
    (
        "MAX_SCHEMA_PAGE",
        4,
        "Maximum number of field descriptors in one schema response.",
    ),
    (
        "MAX_STATUS_PAGE",
        8,
        "Maximum number of status values in one status response.",
    ),
    (
        "MAX_CONFIG_FIELDS",
        10,
        "Maximum number of fields of a config section.",
    ),
    (
        "MAX_MUTATIONS",
        8,
        "Maximum number of mutations in one patch request.",
    ),
    (
        "MAX_CONFIG_SECTIONS",
        8,
        "Maximum number of config sections a device lists.",
    ),
    (
        "MAX_SERVICES",
        8,
        "Maximum number of services a device lists.",
    ),
    (
        "MAX_ISSUES",
        4,
        "Maximum number of active issues reported for one service.",
    ),
    (
        "MAX_ACTIONS_PAGE",
        8,
        "Maximum number of actions in one list response.",
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
        "MAX_CODE_LEN",
        32,
        "Maximum length of an issue code or status reason, in bytes.",
    ),
    (
        "MAX_ISSUE_MESSAGE_LEN",
        48,
        "Maximum length of an issue message, in bytes.",
    ),
];

/// Repeated fields are bounded in elements; strings and bytes in bytes.
const REPEATED: &[&str] = &[
    ".rohi.control.v1.ConfigSectionSchema.fields",
    ".rohi.control.v1.ListConfigSectionsResponse.sections",
    ".rohi.control.v1.GetConfigSectionResponse.entries",
    ".rohi.control.v1.PatchConfigSectionRequest.mutations",
    ".rohi.control.v1.ListServicesResponse.services",
    ".rohi.control.v1.GetServicesStatusResponse.services",
    ".rohi.control.v1.GetServiceSchemaResponse.fields",
    ".rohi.control.v1.GetServiceStatusResponse.fields",
    ".rohi.control.v1.GetServiceStatusResponse.issues",
    ".rohi.control.v1.ListActionsResponse.actions",
    ".rohi.control.v1.GetActionSchemaResponse.args",
    ".rohi.control.v1.GetActionSchemaResponse.results",
    ".rohi.control.v1.InvokeActionRequest.args",
    ".rohi.control.v1.ActionRecord.fields",
    ".rohi.control.v1.InvokeActionResponse.records",
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

    // The Debug removal below is intentional; do not turn it into build warnings.
    let mut generator = Generator::with_warning_callback(|_| {});
    generator.use_container_heapless();

    let bounded = [
        (".rohi.control.v1.GetInfoResponse.firmware", "MAX_TEXT_LEN"),
        (".rohi.control.v1.GetInfoResponse.board", "MAX_TEXT_LEN"),
        (".rohi.control.v1.Value.string_value", "MAX_VALUE_LEN"),
        (".rohi.control.v1.Value.bytes_value", "MAX_VALUE_LEN"),
        (".rohi.control.v1.Value.enum_value", "MAX_VARIANT_LEN"),
        (".rohi.control.v1.FieldDescriptor.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v1.FieldDescriptor.description",
            "MAX_DESCRIPTION_LEN",
        ),
        (".rohi.control.v1.FieldDescriptor.unit", "MAX_UNIT_LEN"),
        (".rohi.control.v1.ConfigSectionInfo.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v1.ConfigSectionSchema.fields",
            "MAX_SCHEMA_PAGE",
        ),
        (
            ".rohi.control.v1.ListConfigSectionsResponse.sections",
            "MAX_CONFIG_SECTIONS",
        ),
        (
            ".rohi.control.v1.GetConfigSchemaRequest.section",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetConfigSectionRequest.section",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetConfigSectionResponse.section",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetConfigSectionResponse.entries",
            "MAX_CONFIG_FIELDS",
        ),
        (
            ".rohi.control.v1.PatchConfigSectionRequest.section",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.PatchConfigSectionRequest.mutations",
            "MAX_MUTATIONS",
        ),
        (
            ".rohi.control.v1.ResetConfigSectionRequest.section",
            "MAX_NAME_LEN",
        ),
        (".rohi.control.v1.Error.reason", "MAX_CODE_LEN"),
        (".rohi.control.v1.ActionInfo.service", "MAX_NAME_LEN"),
        (".rohi.control.v1.ActionInfo.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v1.ActionInfo.description",
            "MAX_DESCRIPTION_LEN",
        ),
        (
            ".rohi.control.v1.ListActionsResponse.actions",
            "MAX_ACTIONS_PAGE",
        ),
        (
            ".rohi.control.v1.GetActionSchemaRequest.service",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetActionSchemaRequest.action",
            "MAX_NAME_LEN",
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
            ".rohi.control.v1.InvokeActionRequest.service",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.InvokeActionRequest.action",
            "MAX_NAME_LEN",
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
        (".rohi.control.v1.ServiceInfo.name", "MAX_NAME_LEN"),
        (
            ".rohi.control.v1.ServiceStatusSummary.service",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.ServiceStatusSummary.reason",
            "MAX_CODE_LEN",
        ),
        (".rohi.control.v1.ServiceIssue.code", "MAX_CODE_LEN"),
        (
            ".rohi.control.v1.ServiceIssue.message",
            "MAX_ISSUE_MESSAGE_LEN",
        ),
        (
            ".rohi.control.v1.ListServicesResponse.services",
            "MAX_SERVICES",
        ),
        (
            ".rohi.control.v1.GetServicesStatusResponse.services",
            "MAX_SERVICES",
        ),
        (
            ".rohi.control.v1.GetServiceSchemaRequest.service",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetServiceSchemaResponse.service",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetServiceSchemaResponse.fields",
            "MAX_SCHEMA_PAGE",
        ),
        (
            ".rohi.control.v1.GetServiceStatusRequest.service",
            "MAX_NAME_LEN",
        ),
        (
            ".rohi.control.v1.GetServiceStatusResponse.fields",
            "MAX_STATUS_PAGE",
        ),
        (
            ".rohi.control.v1.GetServiceStatusResponse.issues",
            "MAX_ISSUES",
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

    // A repeated string is bounded in elements and in the bytes of each one.
    generator.configure(
        ".rohi.control.v1.FieldDescriptor.variants",
        Config::new()
            .max_len(limit("MAX_VARIANTS") as u32)
            .max_bytes(limit("MAX_VARIANT_LEN") as u32),
    );

    // A mutation can carry a secret (a Wi-Fi password): no derived `Debug`,
    // see `lib.rs`.
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
