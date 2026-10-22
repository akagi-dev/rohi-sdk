use heapless::Vec;
use micropb::MessageEncode;

use crate::control::*;
use crate::{DecodeError, EncodeError, PROTOCOL_VERSION, decode, encode, limits};

fn bytes<const N: usize>(data: &[u8]) -> Vec<u8, N> {
    Vec::from_slice(data).unwrap()
}

#[test]
fn worst_case_sizes_fit_the_limits() {
    assert!(Request::MAX_SIZE.unwrap() <= limits::MAX_REQUEST_LEN);
    assert!(Response::MAX_SIZE.unwrap() <= limits::MAX_RESPONSE_LEN);
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
fn oversized_ssid_is_rejected_with_capacity() {
    let mut update = WifiConfigUpdate::default();
    update.set_ssid(bytes(&[b'a'; limits::MAX_SSID_LEN]));
    let mut buf = [0u8; 64];
    let len = encode(&update, &mut buf).unwrap();
    assert!(decode::<WifiConfigUpdate>(&buf[..len]).is_ok());

    // 33 bytes: field 1, length 33.
    let mut wire = [b'a'; 35];
    wire[0] = 0x0a;
    wire[1] = 33;
    assert_eq!(
        decode::<WifiConfigUpdate>(&wire).err(),
        Some(DecodeError::Capacity)
    );
}

#[test]
fn password_update_round_trips_and_stays_out_of_debug() {
    let mut update = WifiConfigUpdate::default();
    update.set_ssid(bytes(b"office"));
    update.password_update = Some(WifiConfigUpdate_::PasswordUpdate::Password(bytes(
        b"hunter2hunter2",
    )));
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        request: Some(Request_::Request::UpdateConfig(UpdateConfigRequest {
            wifi_update: Some(UpdateConfigRequest_::WifiUpdate::UpdateWifi(update.clone())),
        })),
    };
    let mut buf = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(&request, &mut buf).unwrap();
    assert!(decode::<Request>(&buf[..len]).unwrap() == request);

    let debug = std::format!("{update:?}");
    assert!(!debug.contains("hunter2"), "{debug}");
    assert!(debug.contains("<set>"), "{debug}");
}

#[test]
fn clear_password_is_distinct_from_omitted_password() {
    let clear = WifiConfigUpdate {
        password_update: Some(WifiConfigUpdate_::PasswordUpdate::ClearPassword(Empty {})),
        ..Default::default()
    };
    let mut buf = [0u8; 16];
    let len = encode(&clear, &mut buf).unwrap();
    let decoded: WifiConfigUpdate = decode(&buf[..len]).unwrap();
    assert!(matches!(
        decoded.password_update,
        Some(WifiConfigUpdate_::PasswordUpdate::ClearPassword(_))
    ));

    let omitted: WifiConfigUpdate = decode(&[]).unwrap();
    assert!(omitted.password_update.is_none());
    assert!(omitted.ssid().is_none());
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

#[test]
fn sensor_with_absent_value_has_no_value_field() {
    let measurement = Measurement {
        r#type: MeasurementType::Co2,
        state: MeasurementState::WarmingUp,
        ..Default::default()
    };
    let mut buf = [0u8; 32];
    let len = encode(&measurement, &mut buf).unwrap();
    let decoded: Measurement = decode(&buf[..len]).unwrap();
    assert_eq!(decoded.value(), None);
    assert_eq!(decoded.age_ms(), None);
    assert_eq!(decoded.state, MeasurementState::WarmingUp);
}
