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

use micropb::MessageEncode;

use crate::wire::core_v1::{Message, Message_, Meta};
use crate::wire::crypto_v1::SignedEnvelope;
use crate::wire::device_v1::{Urban, UrbanSensor, UrbanSensor_};
use crate::wire::sensor_v1::{BME280, BME280_, Temperature};
use crate::wire::{DecodeError, EncodeError, decode, encode, limits, signing_preimage};

fn varint(value: usize) -> usize {
    match value {
        0..=0x7f => 1,
        0x80..=0x3fff => 2,
        _ => 3,
    }
}

#[test]
fn the_limits_are_the_worst_case_of_the_compiled_capabilities() {
    use crate::wire::crypto_v1::Encrypted;
    use crate::wire::sensor_v1::{BME280, GPS, ICS43434, SDS011};

    // The measurements the encoder supports are the sensors of a section.
    assert_eq!(
        crate::encoder::supported().bits().count(),
        limits::MAX_SENSORS_PER_MESSAGE
    );
    // One entry of a list of `UrbanSensor`: the entry's tag and length, the tag and length of
    // the oneof, and the sensor message at its largest.
    let entry = |sensor: usize| 2 + 2 + sensor;
    let sensors = entry(GPS::MAX_SIZE.unwrap())
        + 3 * entry(BME280::MAX_SIZE.unwrap())
        + 2 * entry(SDS011::MAX_SIZE.unwrap())
        + 2 * entry(ICS43434::MAX_SIZE.unwrap());
    assert_eq!(sensors, limits::MAX_PRIVATE_PLAINTEXT_LEN);
    assert_eq!(limits::MAX_PRIVATE_SECTIONS, 1);

    // `crypto.v1.Encrypted` as `format.rs` writes it: version 1, "xchacha20", the sender key, the
    // 24-byte nonce and the ciphertext with its tag.
    let ciphertext = limits::MAX_PRIVATE_PLAINTEXT_LEN + 16;
    let encrypted = 2
        + (2 + 9)
        + (2 + 32)
        + (2 + limits::NONCE_XCHACHA_LEN)
        + (1 + varint(ciphertext) + ciphertext);
    assert_eq!(encrypted, limits::MAX_ENCRYPTED_LEN);
    assert!(Encrypted::MAX_SIZE.unwrap() >= encrypted);

    let urban = sensors + limits::MAX_PRIVATE_SECTIONS * (1 + varint(encrypted) + encrypted);
    let message = (2 + Meta::MAX_SIZE.unwrap()) + (1 + varint(urban) + urban);
    assert_eq!(message, limits::MAX_MESSAGE_LEN);
    let envelope = (2 + limits::ED25519_KEY_LEN)
        + (2 + limits::NONCE_LEN)
        + (1 + varint(message) + message)
        + (2 + limits::ED25519_SIGNATURE_LEN);
    assert_eq!(envelope, limits::MAX_ENVELOPE_LEN);

    // The schema alone allows more (a list of eight GPS entries); the compiled capabilities do not.
    assert!(Message::MAX_SIZE.unwrap() >= limits::MAX_MESSAGE_LEN);
}

#[test]
fn the_largest_message_of_the_compiled_capabilities_is_exactly_the_limit() {
    use crate::wire::device_v1::UrbanSensor_::Sensor;
    use crate::wire::sensor_v1::{
        GPS, Humidity, ICS43434, ICS43434_, NoiseLevel, PM10, PM25, Pressure, SDS011, SDS011_,
    };

    let sensor = |sensor| UrbanSensor {
        sensor: Some(sensor),
    };
    let bme = |measurement| {
        sensor(Sensor::Bme280(BME280 {
            measurement: Some(measurement),
        }))
    };
    let mut urban = Urban::default();
    for entry in [
        sensor(Sensor::Gps(GPS {
            lat: -179.999_999_999_9,
            lon: -179.999_999_999_9,
            height_m: -8_848.123_456_789,
        })),
        bme(BME280_::Measurement::Temperature(Temperature {
            centi_celsius: i32::MIN,
        })),
        bme(BME280_::Measurement::Humidity(Humidity {
            centi_percent: u32::MAX,
        })),
        bme(BME280_::Measurement::Pressure(Pressure {
            deci_pascal: u32::MAX,
        })),
        sensor(Sensor::Sds011(SDS011 {
            measurement: Some(SDS011_::Measurement::Pm25(PM25 {
                deci_ug_m3: u32::MAX,
            })),
        })),
        sensor(Sensor::Sds011(SDS011 {
            measurement: Some(SDS011_::Measurement::Pm10(PM10 {
                deci_ug_m3: u32::MAX,
            })),
        })),
        sensor(Sensor::Ics43434(ICS43434 {
            measurement: Some(ICS43434_::Measurement::NoiseAvg(NoiseLevel {
                db: u32::MAX,
            })),
        })),
        sensor(Sensor::Ics43434(ICS43434 {
            measurement: Some(ICS43434_::Measurement::NoiseMax(NoiseLevel {
                db: u32::MAX,
            })),
        })),
    ] {
        urban.public.push(entry).unwrap();
    }
    let section = crate::wire::device_v1::EncryptedUrban {
        sensors: urban.public.clone(),
    };
    assert_eq!(section.compute_size(), limits::MAX_PRIVATE_PLAINTEXT_LEN);

    let mut private = heapless::Vec::<u8, { limits::MAX_ENCRYPTED_LEN }>::new();
    private.resize_default(limits::MAX_ENCRYPTED_LEN).unwrap();
    urban.private.push(private).unwrap();
    let mut message = Message::default();
    message.set_metadata(Meta {
        node_id: u64::MAX,
        timestamp: u64::MAX,
    });
    message.payload = Some(Message_::Payload::Urban(urban));
    assert_eq!(message.compute_size(), limits::MAX_MESSAGE_LEN);
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
fn private_sections_decode_as_a_bounded_list() {
    // Urban.private (2) holding an `Encrypted`: decodes into the bounded list.
    let bytes = [0x12, 0x04, 0x12, 0x02, 0x08, 0x01];
    let decoded: Message = decode(&bytes).unwrap();
    assert!(decoded.payload.is_some());
    let Some(crate::wire::core_v1::Message_::Payload::Urban(urban)) = decoded.payload else {
        panic!("an urban payload")
    };
    assert_eq!(urban.private.len(), 1);
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
