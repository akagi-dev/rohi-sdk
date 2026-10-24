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

use micropb::MessageEncode;

use crate::core_v1::{Message, Message_, Meta};
use crate::crypto_v1::SignedEnvelope;
use crate::device_v1::{Urban, UrbanSensor, UrbanSensor_};
use crate::sensor_v1::{BME280, BME280_, Temperature};
use crate::{DecodeError, EncodeError, UPSTREAM_REV, decode, encode, limits, signing_preimage};

#[test]
fn upstream_revision_is_pinned() {
    assert_eq!(UPSTREAM_REV, "33b4e7ec6eaa19a930b91bfb59bd3337c1ec88ac");
}

#[test]
fn worst_case_sizes_fit_the_limits() {
    assert!(Message::MAX_SIZE.unwrap() <= limits::MAX_MESSAGE_LEN);
    assert!(SignedEnvelope::MAX_SIZE.unwrap() <= limits::MAX_ENVELOPE_LEN);
}

fn message() -> Message {
    let mut message = Message::default();
    message.set_metadata(Meta {
        node_id: 7,
        timestamp: 1_700_000_000_123,
    });
    let mut urban = Urban::default();
    urban
        .public
        .push(UrbanSensor {
            sensor: Some(UrbanSensor_::Sensor::Bme280(BME280 {
                measurement: Some(BME280_::Measurement::Temperature(Temperature {
                    centi_celsius: 2347,
                })),
            })),
        })
        .unwrap();
    message.payload = Some(Message_::Payload::Urban(urban));
    message
}

#[test]
fn message_golden_bytes_pin_the_field_numbers() {
    let mut buf = [0u8; 64];
    let len = encode(&message(), &mut buf).unwrap();
    assert_eq!(
        &buf[..len],
        &[
            // metadata (1): node_id (1) = 7, timestamp (2) = 1_700_000_000_123
            0x0a, 0x09, 0x08, 0x07, 0x10, 0xfb, 0xd0, 0x95, 0xff, 0xbc, 0x31,
            // urban (2): public (1): bme280 (2): temperature (1): centi_celsius (1) = 2347
            0x12, 0x09, 0x0a, 0x07, 0x12, 0x05, 0x0a, 0x03, 0x08, 0xd6, 0x24,
        ]
    );
    let decoded: Message = decode(&buf[..len]).unwrap();
    assert_eq!(decoded.metadata().unwrap().timestamp, 1_700_000_000_123);
}

#[test]
fn envelope_golden_bytes_pin_the_field_numbers() {
    let mut envelope = SignedEnvelope::default();
    envelope.sensor_id.extend_from_slice(&[0xaa; 3]).unwrap();
    envelope.nonce.extend_from_slice(&[0xbb; 2]).unwrap();
    envelope.message.extend_from_slice(&[0xcc]).unwrap();
    envelope.signature.extend_from_slice(&[0xdd; 2]).unwrap();
    let mut buf = [0u8; 32];
    let len = encode(&envelope, &mut buf).unwrap();
    assert_eq!(
        &buf[..len],
        &[
            0x0a, 0x03, 0xaa, 0xaa, 0xaa, // sensor_id = 1
            0x12, 0x02, 0xbb, 0xbb, // nonce = 2
            0x1a, 0x01, 0xcc, // message = 3
            0x22, 0x02, 0xdd, 0xdd, // signature = 4
        ]
    );
    let decoded: SignedEnvelope = decode(&buf[..len]).unwrap();
    assert!(decoded == envelope);
}

#[test]
fn envelope_has_no_unknown_extra_fields_at_the_pinned_revision() {
    // Field 5 is not part of the pinned `SignedEnvelope`: it is skipped.
    let bytes = [0x0a, 0x01, 0x01, 0x28, 0x09];
    let decoded: SignedEnvelope = decode(&bytes).unwrap();
    assert_eq!(decoded.sensor_id.as_slice(), &[1]);
}

#[test]
fn private_sections_are_skipped_when_decoding() {
    // Urban.private (2) holding an `Encrypted`: not generated, ignored.
    let bytes = [0x12, 0x04, 0x12, 0x02, 0x08, 0x01];
    let decoded: Message = decode(&bytes).unwrap();
    assert!(decoded.payload.is_some());
}

#[test]
fn oversized_envelope_fields_are_rejected() {
    let mut bytes = [0u8; 40];
    bytes[0] = 0x0a;
    bytes[1] = 33;
    assert_eq!(decode::<SignedEnvelope>(&bytes), Err(DecodeError::Capacity));
}

#[test]
fn encoding_into_a_small_buffer_fails() {
    let mut buf = [0u8; 4];
    assert_eq!(
        encode(&message(), &mut buf),
        Err(EncodeError::BufferTooSmall)
    );
}

#[test]
fn preimage_is_sensor_id_nonce_message() {
    let mut out = [0u8; 8];
    let len = signing_preimage(&[1, 2], &[3], &[4, 5, 6], &mut out).unwrap();
    assert_eq!(&out[..len], &[1, 2, 3, 4, 5, 6]);
    assert_eq!(
        signing_preimage(&[1, 2], &[3], &[4, 5, 6], &mut [0u8; 5]),
        Err(EncodeError::BufferTooSmall)
    );
}
