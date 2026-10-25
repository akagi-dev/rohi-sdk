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
            ..Default::default()
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

#[test]
fn provision_identity_request_golden_bytes() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 2,
        request: Some(Request_::Request::ProvisionIdentity(
            ProvisionIdentityRequest {
                ed25519_secret: bytes(&[0x5a; limits::ED25519_KEY_LEN]),
            },
        )),
    };
    let mut buf = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(&request, &mut buf).unwrap();
    // field 31 (provision_identity, tag 0xfa 0x01) holds field 1 = 32 bytes.
    let mut expected = std::vec![0x08, 0x01, 0x10, 0x02, 0xfa, 0x01, 0x22, 0x0a, 0x20];
    expected.extend_from_slice(&[0x5a; 32]);
    assert_eq!(&buf[..len], expected.as_slice());
    assert!(decode::<Request>(&buf[..len]).unwrap() == request);
}

#[test]
fn identity_secret_stays_out_of_debug() {
    let request = ProvisionIdentityRequest {
        ed25519_secret: bytes(&[0x5a; limits::ED25519_KEY_LEN]),
    };
    let debug = std::format!("{request:?}");
    assert!(!debug.contains("90"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
}

#[test]
fn oversized_identity_secret_is_rejected_with_capacity() {
    let mut wire = [0x5a; 35];
    wire[0] = 0x0a;
    wire[1] = 33;
    assert_eq!(
        decode::<ProvisionIdentityRequest>(&wire).err(),
        Some(DecodeError::Capacity)
    );
}

#[test]
fn identity_info_round_trips() {
    let mut info = IdentityInfo {
        state: IdentityState::Provisioned,
        ..Default::default()
    };
    info.set_public_key(bytes(&[0x01; limits::ED25519_KEY_LEN]));
    let mut out = GetIdentityResponse::default();
    out.set_identity(info);
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: 3,
        response: Some(Response_::Response::GetIdentity(out)),
    };
    let mut buf = [0u8; 64];
    let len = encode(&response, &mut buf).unwrap();
    assert_eq!(decode::<Response>(&buf[..len]).unwrap(), response);

    let absent = IdentityInfo {
        state: IdentityState::NotProvisioned,
        ..Default::default()
    };
    let len = encode(&absent, &mut buf).unwrap();
    assert_eq!(&buf[..len], &[0x08, 0x01]);
    assert_eq!(
        decode::<IdentityInfo>(&buf[..len]).unwrap().public_key(),
        None
    );
}

#[test]
fn get_time_status_request_golden_bytes() {
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 7,
        request: Some(Request_::Request::GetTimeStatus(
            GetTimeStatusRequest::default(),
        )),
    };
    let mut buf = [0u8; 16];
    let len = encode(&request, &mut buf).unwrap();
    // field 40 (time status), length-delimited and empty.
    assert_eq!(&buf[..len], &[0x08, 0x01, 0x10, 0x07, 0xc2, 0x02, 0x00]);
    assert!(decode::<Request>(&buf[..len]).unwrap() == request);
}

#[test]
fn full_time_status_response_fits_the_limit() {
    let mut time = GetTimeStatusResponse {
        state: TimeSyncState::Synchronized,
        consecutive_failures: u32::MAX,
        ..Default::default()
    };
    time.set_unix_time_ms(u64::MAX);
    time.set_source(TimeSource::Ntp);
    time.set_sync_age_ms(u64::MAX);
    time.set_uncertainty_ms(u32::MAX);
    time.set_last_failure(TimeSyncFailure::Source);
    let response = Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: u32::MAX,
        response: Some(Response_::Response::GetTimeStatus(time)),
    };
    let mut buf = [0u8; limits::MAX_RESPONSE_LEN];
    let len = encode(&response, &mut buf).unwrap();
    assert!(decode::<Response>(&buf[..len]).unwrap() == response);
}

#[test]
fn connectivity_update_round_trips_and_bounds_the_host() {
    let mut update = ConnectivityConfigUpdate::default();
    update.set_host(
        core::str::from_utf8(&[b'a'; limits::MAX_HOST_LEN])
            .unwrap()
            .try_into()
            .unwrap(),
    );
    update.set_port(8080);
    update.set_enabled(true);
    let request = Request {
        protocol_version: PROTOCOL_VERSION,
        request_id: 1,
        request: Some(Request_::Request::UpdateConfig(UpdateConfigRequest {
            wifi_update: Some(UpdateConfigRequest_::WifiUpdate::ClearWifi(Empty {})),
            connectivity_update: Some(
                UpdateConfigRequest_::ConnectivityUpdate::UpdateConnectivity(update),
            ),
        })),
    };
    let mut buf = [0u8; limits::MAX_REQUEST_LEN];
    let len = encode(&request, &mut buf).unwrap();
    assert!(decode::<Request>(&buf[..len]).unwrap() == request);

    // 65 bytes: field 4, length 65.
    let mut wire = [b'a'; 67];
    wire[0] = 0x22;
    wire[1] = 65;
    assert_eq!(
        decode::<ConnectivityConfigUpdate>(&wire).err(),
        Some(DecodeError::Capacity)
    );
}
