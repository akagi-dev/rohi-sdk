use heapless::Vec;
use micropb::MessageEncode;

use crate::control::*;
use crate::{DecodeError, EncodeError, PROTOCOL_VERSION, decode, encode, limits};

fn bytes<const N: usize>(data: &[u8]) -> Vec<u8, N> {
    Vec::from_slice(data).unwrap()
}

#[test]
fn the_request_fits_the_limit_in_the_worst_case() {
    assert!(Request::MAX_SIZE.unwrap() <= limits::MAX_REQUEST_LEN);
}

#[test]
fn protocol_version_is_one() {
    assert_eq!(PROTOCOL_VERSION, 1);
}

#[test]
fn get_info_request_golden_bytes() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 7,
        request: Some(Request_::Request::GetInfo(GetInfoRequest::default())),
    };
    let mut buf = [0u8; 16];
    let len = encode(&request, &mut buf).unwrap();
    // field 1 varint 1, field 2 varint 7, field 10 empty message.
    assert_eq!(&buf[..len], &[0x08, 0x01, 0x10, 0x07, 0x52, 0x00]);

    let decoded: Request = decode(&buf[..len]).unwrap();
    assert!(decoded == request);
}

#[test]
fn error_response_golden_bytes() {
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: 300,
        response: Some(Response_::Response::Error(Error {
            code: ErrorCode::NotSupported,
            ..Default::default()
        })),
    };
    let mut buf = [0u8; 16];
    let len = encode(&response, &mut buf).unwrap();
    // field 9 (error) holds field 1 = 3.
    assert_eq!(
        &buf[..len],
        &[0x08, 0x01, 0x10, 0xac, 0x02, 0x4a, 0x02, 0x08, 0x03]
    );
    assert_eq!(decode::<Response>(&buf[..len]).unwrap(), response);
}

#[test]
fn unknown_fields_are_skipped() {
    // protocol_version = 1, request_id = 5, unknown field 99 (varint), get_info.
    let wire = [0x08, 0x01, 0x10, 0x05, 0x98, 0x06, 0x2a, 0x52, 0x00];
    let request: Request = decode(&wire).unwrap();
    assert_eq!(request.request_id, 5);
    assert!(matches!(
        request.request,
        Some(Request_::Request::GetInfo(_))
    ));
}

#[test]
fn unknown_request_decodes_without_a_variant() {
    // A request type from a future protocol revision: field 99 message.
    let wire = [0x08, 0x01, 0x10, 0x09, 0x9a, 0x06, 0x00];
    let request: Request = decode(&wire).unwrap();
    assert_eq!(request.request_id, 9);
    assert!(request.request.is_none());
}

#[test]
fn unknown_enum_values_are_preserved() {
    let wire = [0x08, 0x01, 0x10, 0x01, 0x4a, 0x02, 0x08, 0x7f];
    let response: Response = decode(&wire).unwrap();
    let Some(Response_::Response::Error(error)) = response.response else {
        panic!("not an error");
    };
    assert_eq!(error.code, ErrorCode(0x7f));
}

#[test]
fn truncated_message_is_malformed() {
    assert_eq!(
        decode::<Request>(&[0x08, 0x01, 0x10]).err(),
        Some(DecodeError::Malformed)
    );
    assert_eq!(
        decode::<Request>(&[0x52, 0x05, 0x00]).err(),
        Some(DecodeError::Malformed)
    );
}

#[test]
fn encoding_into_a_short_buffer_fails() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 7,
        request: Some(Request_::Request::GetInfo(GetInfoRequest::default())),
    };
    let mut buf = [0u8; 5];
    assert_eq!(encode(&request, &mut buf), Err(EncodeError::BufferTooSmall));
}

fn text<const N: usize>(value: &str) -> heapless::String<N> {
    value.try_into().unwrap()
}

fn string_field(id: u32, value: &str) -> FieldValue {
    FieldValue {
        field_id: id,
        value: Some(FieldValue_::Value::StringValue(text(value))),
    }
}

fn number_field(id: u32, value: FieldValue_::Value) -> FieldValue {
    FieldValue {
        field_id: id,
        value: Some(value),
    }
}

#[test]
fn patch_request_round_trips_and_keeps_values_out_of_debug() {
    let mut mutations = Vec::new();
    mutations
        .push(ConfigMutation {
            field_id: 2,
            operation: Some(ConfigMutation_::Operation::StringValue(text(
                "hunter2hunter2",
            ))),
        })
        .ok()
        .unwrap();
    mutations
        .push(ConfigMutation {
            field_id: 3,
            operation: Some(ConfigMutation_::Operation::Clear(Empty {})),
        })
        .ok()
        .unwrap();
    let mut patch = PatchConfigSectionRequest {
        section_id: 1,
        mutations,
        dry_run: true,
        ..Default::default()
    };
    patch.set_expected_revision(4);
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        request: Some(Request_::Request::PatchConfigSection(patch)),
    };
    let mut buf = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(&request, &mut buf).unwrap();
    assert!(decode::<Request>(&buf[..len]).unwrap() == request);

    let Some(Request_::Request::PatchConfigSection(patch)) = &request.request else {
        unreachable!()
    };
    let debug = std::format!("{:?}", patch.mutations[0]);
    assert!(!debug.contains("hunter2"), "{debug}");
    assert!(debug.contains("redacted"), "{debug}");
}

#[test]
fn clear_is_distinct_from_an_empty_value() {
    let clear = ConfigMutation {
        field_id: 1,
        operation: Some(ConfigMutation_::Operation::Clear(Empty {})),
    };
    let empty = ConfigMutation {
        field_id: 1,
        operation: Some(ConfigMutation_::Operation::StringValue(text(""))),
    };
    let mut a = [0u8; 16];
    let mut b = [0u8; 16];
    let la = encode(&clear, &mut a).unwrap();
    let lb = encode(&empty, &mut b).unwrap();
    assert_ne!(&a[..la], &b[..lb]);
    assert!(matches!(
        decode::<ConfigMutation>(&a[..la]).unwrap().operation,
        Some(ConfigMutation_::Operation::Clear(_))
    ));
}

#[test]
fn oversized_value_is_rejected_with_capacity() {
    // string_value (field 6) of 65 bytes in a request, of 33 in a response.
    let mut wire = [b'a'; 67];
    wire[0] = 0x32;
    wire[1] = 65;
    assert_eq!(
        decode::<ActionArg>(&wire).err(),
        Some(DecodeError::Capacity)
    );
    wire[1] = 64;
    assert!(decode::<ActionArg>(&wire[..66]).is_ok());
    wire[1] = 33;
    assert_eq!(
        decode::<FieldValue>(&wire[..35]).err(),
        Some(DecodeError::Capacity)
    );
    wire[1] = 32;
    assert!(decode::<FieldValue>(&wire[..34]).is_ok());
}

#[test]
fn floats_are_single_precision_on_the_wire() {
    let field = number_field(1, FieldValue_::Value::F32Value(1.5));
    let mut buf = [0u8; 16];
    let len = encode(&field, &mut buf).unwrap();
    // field 1 = 1, field 5 (fixed32) = 1.5f32.
    assert_eq!(&buf[..len], &[0x08, 0x01, 0x2d, 0x00, 0x00, 0xc0, 0x3f]);
}

fn full_field(id: u32) -> FieldDescriptor {
    FieldDescriptor {
        id,
        name: text(&"n".repeat(limits::MAX_NAME_LEN)),
        kind: ValueKind::Enum,
        unit: Unit::MicrosievertsPerHour,
        enum_id: u32::MAX,
    }
}

fn full_enum() -> EnumDescriptor {
    let mut variants = Vec::new();
    for value in 0..limits::MAX_ENUM_VARIANTS {
        variants
            .push(EnumVariant {
                value: u32::MAX - value as u32,
                name: text(&"v".repeat(limits::MAX_VARIANT_NAME_LEN)),
            })
            .ok()
            .unwrap();
    }
    EnumDescriptor {
        id: u32::MAX,
        variants,
    }
}

fn full_enums<const N: usize>() -> Vec<EnumDescriptor, N> {
    let mut enums = Vec::new();
    for _ in 0..N {
        enums.push(full_enum()).ok().unwrap();
    }
    enums
}

fn round_trip(response: Response) -> usize {
    let mut buf = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&response, &mut buf).unwrap();
    assert!(decode::<Response>(&buf[..len]).unwrap() == response);
    len
}

fn response(response: Response_::Response) -> Response {
    Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: u32::MAX,
        response: Some(response),
    }
}

#[test]
fn full_config_schema_fits_the_response_limit() {
    let mut fields = Vec::new();
    for id in 0..limits::MAX_CONFIG_FIELDS {
        let mut field = ConfigFieldDescriptor {
            writable: true,
            secret: true,
            optional: true,
            ..Default::default()
        };
        field.set_field(full_field(u32::MAX - id as u32));
        fields.push(field).ok().unwrap();
    }
    let mut schema = GetConfigSchemaResponse {
        fields,
        enums: full_enums(),
        ..Default::default()
    };
    schema.set_section(ConfigSectionInfo {
        id: u64::MAX,
        name: text(&"s".repeat(limits::MAX_NAME_LEN)),
        version: u32::MAX,
        revision: u32::MAX,
    });
    round_trip(response(Response_::Response::GetConfigSchema(schema)));
}

#[test]
fn full_service_schema_fits_the_response_limit() {
    let mut fields = Vec::new();
    for id in 0..limits::MAX_SCHEMA_FIELDS {
        fields.push(full_field(u32::MAX - id as u32)).ok().unwrap();
    }
    let mut issues = Vec::new();
    for code in 0..limits::MAX_ISSUE_DESCRIPTORS {
        issues
            .push(IssueDescriptor {
                code: u32::MAX - code as u32,
                name: text(&"i".repeat(limits::MAX_NAME_LEN)),
            })
            .ok()
            .unwrap();
    }
    round_trip(response(Response_::Response::GetServiceSchema(
        GetServiceSchemaResponse {
            service_id: u32::MAX,
            name: text(&"s".repeat(limits::MAX_NAME_LEN)),
            version: u32::MAX,
            fields,
            enums: full_enums(),
            issues,
        },
    )));
}

#[test]
fn full_service_status_fits_the_response_limit() {
    let mut fields = Vec::new();
    for id in 0..limits::MAX_STATUS_FIELDS {
        fields
            .push(string_field(
                u32::MAX - id as u32,
                &"x".repeat(limits::MAX_FIELD_VALUE_LEN),
            ))
            .ok()
            .unwrap();
    }
    let mut issues = Vec::new();
    for code in 0..limits::MAX_ISSUES {
        issues
            .push(ServiceIssue {
                code: u32::MAX - code as u32,
                severity: IssueSeverity::Critical,
                age_ms: u32::MAX,
            })
            .ok()
            .unwrap();
    }
    round_trip(response(Response_::Response::GetServiceStatus(
        GetServiceStatusResponse {
            service_id: u32::MAX,
            revision: u32::MAX,
            fields,
            issues,
        },
    )));
}

#[test]
fn services_summary_with_every_service_fits_the_response_limit() {
    let mut services = Vec::new();
    for _ in 0..limits::MAX_SERVICES {
        services
            .push(ServiceStatusSummary {
                service_id: u32::MAX,
                name: text(&"s".repeat(limits::MAX_NAME_LEN)),
                version: u32::MAX,
                state: ServiceState::Failed,
                health: ServiceHealth::Error,
                ready: true,
                revision: u32::MAX,
                state_age_ms: u32::MAX,
            })
            .ok()
            .unwrap();
    }
    round_trip(response(Response_::Response::GetServicesStatus(
        GetServicesStatusResponse {
            uptime_ms: u64::MAX,
            health: DeviceHealth::Error,
            services,
        },
    )));
}

#[test]
fn every_action_listed_fits_the_response_limit() {
    let mut actions = Vec::new();
    for _ in 0..limits::MAX_ACTIONS {
        actions
            .push(ActionInfo {
                service_id: u32::MAX,
                id: u32::MAX,
                name: text(&"a".repeat(limits::MAX_NAME_LEN)),
                privileged: true,
                secret_args: true,
            })
            .ok()
            .unwrap();
    }
    round_trip(response(Response_::Response::ListActions(
        ListActionsResponse { actions },
    )));
}

#[test]
fn a_full_action_schema_fits_the_response_limit() {
    let mut args = Vec::new();
    for id in 0..limits::MAX_ACTION_ARGS {
        args.push(full_field(id as u32)).ok().unwrap();
    }
    let mut results = Vec::new();
    for id in 0..limits::MAX_RECORD_FIELDS {
        results.push(full_field(id as u32)).ok().unwrap();
    }
    let mut schema = GetActionSchemaResponse {
        args,
        results,
        enums: full_enums(),
        ..Default::default()
    };
    schema.set_action(ActionInfo {
        service_id: u32::MAX,
        id: u32::MAX,
        name: text(&"a".repeat(limits::MAX_NAME_LEN)),
        privileged: true,
        secret_args: true,
    });
    round_trip(response(Response_::Response::GetActionSchema(schema)));
}

#[test]
fn list_config_sections_request_golden_bytes() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 7,
        request: Some(Request_::Request::ListConfigSections(
            ListConfigSectionsRequest::default(),
        )),
    };
    let mut buf = [0u8; 16];
    let len = encode(&request, &mut buf).unwrap();
    // field 11, length-delimited and empty.
    assert_eq!(&buf[..len], &[0x08, 0x01, 0x10, 0x07, 0x5a, 0x00]);
}

#[test]
fn invoke_action_round_trips_and_keeps_arguments_out_of_debug() {
    let mut args = Vec::new();
    args.push(ActionArg {
        field_id: 1,
        value: Some(ActionArg_::Value::BytesValue(bytes(&[0x5a; 32]))),
    })
    .ok()
    .unwrap();
    let invoke = InvokeActionRequest {
        service_id: 3,
        action_id: 1,
        args,
    };
    let debug = std::format!("{invoke:?}");
    assert!(!debug.contains("90") && !debug.contains("5a"), "{debug}");
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        request: Some(Request_::Request::InvokeAction(invoke)),
    };
    let mut buf = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(&request, &mut buf).unwrap();
    assert!(decode::<Request>(&buf[..len]).unwrap() == request);
}

#[test]
fn a_full_action_result_fits_the_response_limit_with_typical_values() {
    // Scan results: SSID (32 bytes), rssi, channel, security.
    let mut records = Vec::new();
    for _ in 0..limits::MAX_RECORDS {
        let mut fields = Vec::new();
        for field in [
            string_field(1, &"s".repeat(32)),
            number_field(2, FieldValue_::Value::I64Value(-60)),
            number_field(3, FieldValue_::Value::U64Value(11)),
            number_field(4, FieldValue_::Value::EnumValue(3)),
        ] {
            fields.push(field).ok().unwrap();
        }
        records.push(ActionRecord { fields }).ok().unwrap();
    }
    let len = round_trip(response(Response_::Response::InvokeAction(
        InvokeActionResponse { records },
    )));
    assert!(len < limits::MAX_RESPONSE_LEN / 2, "{len}");
}

#[test]
fn a_rejection_carries_a_reason() {
    let mut error = Error {
        code: ErrorCode::Rejected,
        ..Default::default()
    };
    error.set_reason(text("already_provisioned"));
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        response: Some(Response_::Response::Error(error)),
    };
    let mut buf = [0u8; 64];
    let len = encode(&response, &mut buf).unwrap();
    assert!(decode::<Response>(&buf[..len]).unwrap() == response);
}

// Memory budgets of the generated types. The control task builds and moves
// these on the stack of a microcontroller, so growth must fail here and not
// as a stack overflow on the device. Raising a budget is a deliberate
// decision: measure the control task again and update `resource-budget.md`.
// Measured: 736, 1824, 48, 56, 536, 1192, 1504 (config read) and 728 (config
// patch) bytes, with a few percent of
// headroom. Before the compact layout: 760, 2616, 352, 88, 792 and 1272 (the
// last one for a page of eight values).
const REQUEST_BUDGET: usize = 768;
const RESPONSE_BUDGET: usize = 1900;
const FIELD_DESCRIPTOR_BUDGET: usize = 56;
const FIELD_VALUE_BUDGET: usize = 64;
const SERVICES_STATUS_BUDGET: usize = 576;
const SERVICE_STATUS_BUDGET: usize = 1280;
const CONFIG_SECTION_BUDGET: usize = 1536;
const PATCH_BUDGET: usize = 768;

#[test]
fn generated_types_stay_within_their_memory_budgets() {
    use core::mem::size_of;
    for (name, size, budget) in [
        ("Request", size_of::<Request>(), REQUEST_BUDGET),
        ("Response", size_of::<Response>(), RESPONSE_BUDGET),
        (
            "FieldDescriptor",
            size_of::<FieldDescriptor>(),
            FIELD_DESCRIPTOR_BUDGET,
        ),
        ("FieldValue", size_of::<FieldValue>(), FIELD_VALUE_BUDGET),
        (
            "GetServicesStatusResponse",
            size_of::<GetServicesStatusResponse>(),
            SERVICES_STATUS_BUDGET,
        ),
        (
            "GetServiceStatusResponse",
            size_of::<GetServiceStatusResponse>(),
            SERVICE_STATUS_BUDGET,
        ),
        (
            "GetConfigSectionResponse",
            size_of::<GetConfigSectionResponse>(),
            CONFIG_SECTION_BUDGET,
        ),
        (
            "PatchConfigSectionRequest",
            size_of::<PatchConfigSectionRequest>(),
            PATCH_BUDGET,
        ),
    ] {
        std::println!("size_of::<{name}>() = {size}");
        assert!(size <= budget, "{name} is {size} bytes, budget {budget}");
    }
}
