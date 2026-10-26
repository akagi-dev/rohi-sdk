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

fn string_value(value: &str) -> Value {
    Value {
        value: Some(Value_::Value::StringValue(text(value))),
    }
}

#[test]
fn patch_request_round_trips_and_keeps_values_out_of_debug() {
    let mut mutations = Vec::new();
    mutations
        .push(ConfigMutation {
            field_id: 2,
            operation: Some(ConfigMutation_::Operation::Set(string_value(
                "hunter2hunter2",
            ))),
        })
        .ok()
        .unwrap();
    mutations
        .push(ConfigMutation {
            field_id: 3,
            operation: Some(ConfigMutation_::Operation::Unset(Empty {})),
        })
        .ok()
        .unwrap();
    let mut patch = PatchConfigSectionRequest {
        section: text("wifi"),
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
fn unset_is_distinct_from_an_empty_value() {
    let unset = ConfigMutation {
        field_id: 1,
        operation: Some(ConfigMutation_::Operation::Unset(Empty {})),
    };
    let empty = ConfigMutation {
        field_id: 1,
        operation: Some(ConfigMutation_::Operation::Set(string_value(""))),
    };
    let mut a = [0u8; 16];
    let mut b = [0u8; 16];
    let la = encode(&unset, &mut a).unwrap();
    let lb = encode(&empty, &mut b).unwrap();
    assert_ne!(&a[..la], &b[..lb]);
    assert!(matches!(
        decode::<ConfigMutation>(&a[..la]).unwrap().operation,
        Some(ConfigMutation_::Operation::Unset(_))
    ));
}

#[test]
fn oversized_value_is_rejected_with_capacity() {
    // string_value (field 5) of 65 bytes.
    let mut wire = [b'a'; 67];
    wire[0] = 0x2a;
    wire[1] = 65;
    assert_eq!(decode::<Value>(&wire).err(), Some(DecodeError::Capacity));
    wire[1] = 64;
    assert!(decode::<Value>(&wire[..66]).is_ok());
}

fn full_field() -> FieldDescriptor {
    full_field_with(2)
}

fn full_field_with(variant_count: usize) -> FieldDescriptor {
    let mut variants = Vec::new();
    for _ in 0..variant_count {
        variants
            .push(text(&"v".repeat(limits::MAX_VARIANT_LEN)))
            .unwrap();
    }
    FieldDescriptor {
        id: u32::MAX,
        name: text(&"n".repeat(limits::MAX_NAME_LEN)),
        kind: ValueKind::Enum,
        description: text(&"d".repeat(limits::MAX_DESCRIPTION_LEN)),
        unit: text(&"u".repeat(limits::MAX_UNIT_LEN)),
        variants,
    }
}

#[test]
fn full_config_schema_fits_the_response_limit() {
    let mut fields = Vec::new();
    for _ in 0..limits::MAX_SCHEMA_PAGE {
        let mut field = ConfigField {
            writable: true,
            secret: true,
            optional: true,
            ..Default::default()
        };
        field.set_field(full_field());
        field.set_min(f64::MIN);
        field.set_max(f64::MAX);
        fields.push(field).ok().unwrap();
    }
    let mut schema = ConfigSectionSchema {
        fields,
        ..Default::default()
    };
    schema.set_section(ConfigSectionInfo {
        name: text(&"s".repeat(limits::MAX_NAME_LEN)),
        version: u32::MAX,
        revision: u32::MAX,
    });
    let mut schema_response = GetConfigSchemaResponse::default();
    schema_response.set_schema(schema);
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: u32::MAX,
        response: Some(Response_::Response::GetConfigSchema(schema_response)),
    };
    let mut buf = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&response, &mut buf).unwrap();
    assert!(decode::<Response>(&buf[..len]).unwrap() == response);
}

#[test]
fn full_service_status_fits_the_response_limit() {
    let mut fields = Vec::new();
    for id in 0..limits::MAX_STATUS_PAGE {
        let mut field = FieldValue {
            field_id: u32::MAX - id as u32,
            ..Default::default()
        };
        field.set_value(string_value(&"x".repeat(limits::MAX_VALUE_LEN)));
        fields.push(field).ok().unwrap();
    }
    let mut issues = Vec::new();
    for _ in 0..limits::MAX_ISSUES {
        issues
            .push(ServiceIssue {
                severity: IssueSeverity::Critical,
                code: text(&"c".repeat(limits::MAX_CODE_LEN)),
                message: text(&"m".repeat(limits::MAX_ISSUE_MESSAGE_LEN)),
                age_ms: u64::MAX,
            })
            .unwrap();
    }
    let mut summary = ServiceStatusSummary {
        service: text(&"s".repeat(limits::MAX_NAME_LEN)),
        state: ServiceState::Running,
        health: ServiceHealth::Degraded,
        ready: true,
        revision: u32::MAX,
        state_age_ms: u64::MAX,
        ..Default::default()
    };
    summary.set_reason(text(&"r".repeat(limits::MAX_CODE_LEN)));
    let mut status = GetServiceStatusResponse {
        fields,
        issues,
        ..Default::default()
    };
    status.set_summary(summary);
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: u32::MAX,
        response: Some(Response_::Response::GetServiceStatus(status)),
    };
    let mut buf = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&response, &mut buf).unwrap();
    assert!(decode::<Response>(&buf[..len]).unwrap() == response);
}

#[test]
fn services_summary_with_every_service_fits_the_response_limit() {
    let mut services = Vec::new();
    for _ in 0..limits::MAX_SERVICES {
        let mut summary = ServiceStatusSummary {
            service: text(&"s".repeat(limits::MAX_NAME_LEN)),
            state_age_ms: u64::MAX,
            revision: u32::MAX,
            ..Default::default()
        };
        summary.set_reason(text(&"r".repeat(limits::MAX_CODE_LEN)));
        services.push(summary).unwrap();
    }
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: u32::MAX,
        response: Some(Response_::Response::GetServicesStatus(
            GetServicesStatusResponse {
                uptime_ms: u64::MAX,
                health: DeviceHealth::Error,
                services,
            },
        )),
    };
    let mut buf = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&response, &mut buf).unwrap();
    assert!(decode::<Response>(&buf[..len]).unwrap() == response);
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
    args.push({
        let mut arg = FieldValue {
            field_id: 1,
            ..Default::default()
        };
        arg.set_value(Value {
            value: Some(Value_::Value::BytesValue(bytes(&[0x5a; 32]))),
        });
        arg
    })
    .ok()
    .unwrap();
    let invoke = InvokeActionRequest {
        service: text("identity"),
        action: text("provision"),
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
    // Eight scan results: SSID (32 bytes), rssi, channel, security.
    let mut records = Vec::new();
    for _ in 0..limits::MAX_RECORDS {
        let mut fields = Vec::new();
        for (id, value) in [
            (1, string_value(&"s".repeat(32))),
            (
                2,
                Value {
                    value: Some(Value_::Value::I64Value(-60)),
                },
            ),
            (
                3,
                Value {
                    value: Some(Value_::Value::U64Value(11)),
                },
            ),
            (
                4,
                Value {
                    value: Some(Value_::Value::EnumValue(text("wpa2"))),
                },
            ),
        ] {
            let mut field = FieldValue {
                field_id: id,
                ..Default::default()
            };
            field.set_value(value);
            fields.push(field).ok().unwrap();
        }
        records.push(ActionRecord { fields }).ok().unwrap();
    }
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: u32::MAX,
        response: Some(Response_::Response::InvokeAction(InvokeActionResponse {
            records,
        })),
    };
    let mut buf = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&response, &mut buf).unwrap();
    assert!(len < limits::MAX_RESPONSE_LEN / 2, "{len}");
    assert!(decode::<Response>(&buf[..len]).unwrap() == response);
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

#[test]
fn a_field_with_every_variant_fits() {
    let field = full_field_with(limits::MAX_VARIANTS);
    let mut buf = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&field, &mut buf).unwrap();
    assert_eq!(decode::<FieldDescriptor>(&buf[..len]).unwrap(), field);
}

#[test]
fn messages_stay_small_in_memory() {
    // A response is built, moved and encoded on the stack of the control task
    // of a microcontroller: every page, list and record bound is chosen so
    // that the largest variant stays within this budget.
    const BUDGET: usize = 2700;
    let response = core::mem::size_of::<Response>();
    let request = core::mem::size_of::<Request>();
    assert!(response <= BUDGET, "Response is {response} bytes");
    assert!(request <= BUDGET, "Request is {request} bytes");
}
