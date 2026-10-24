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
use core::cell::{Cell, RefCell};

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_async::delay::DelayNs;
use rohi_connectivity_proto::core_v1::{Message, Message_};
use rohi_connectivity_proto::crypto_v1::SignedEnvelope;
use rohi_connectivity_proto::device_v1::UrbanSensor_::Sensor;
use rohi_connectivity_proto::sensor_v1::{BME280_, ICS43434_, SDS011_};
use rohi_connectivity_proto::{decode, limits};
use rohi_hal::runtime::Clock;
use rohi_hal::{
    Ags3871Update, Aqi, Bmx280Update, Celsius, Decibels, MeasurementState, MeasurementStore,
    MeasurementUpdate, MeasurementsSnapshot, MicrogramsPerCubicMeter, MonotonicTimestamp,
    NoiseUpdate, Pascal, Ppb, Ppm, PulseCount, RadSensUpdate, RadiationLevel, RelativeHumidity,
    Scd4xUpdate, Sds011Update, SensorEvent, SensorKind, SensorState, Zmod4510Update,
};
use rohi_identity::{IdentityError, MockSigner, Signature, Signer};

use crate::*;

mod golden;
mod service;

fn ts(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn update(update: MeasurementUpdate) -> SensorEvent {
    SensorEvent::Measurements(update)
}

fn bme280(at: u64) -> SensorEvent {
    update(MeasurementUpdate::Bmx280(Bmx280Update::bme280(
        Celsius(23.47),
        RelativeHumidity(53.42),
        Pascal(101_325.0),
        ts(at),
    )))
}

fn sds011(at: u64) -> SensorEvent {
    update(MeasurementUpdate::Sds011(Sds011Update {
        pm25: MicrogramsPerCubicMeter(12.34),
        pm10: MicrogramsPerCubicMeter(20.06),
        timestamp: ts(at),
    }))
}

fn microphone(at: u64) -> SensorEvent {
    update(MeasurementUpdate::Microphone(NoiseUpdate {
        average: Decibels(45.4),
        maximum: Decibels(71.6),
        timestamp: ts(at),
    }))
}

fn snapshot_of(events: &[SensorEvent], now: u64) -> MeasurementsSnapshot {
    let mut store = MeasurementStore::new();
    for event in events {
        store.apply(*event);
    }
    store.snapshot(ts(now))
}

fn message_of(bytes: &[u8]) -> Message {
    decode(bytes).unwrap()
}

fn sensors(message: &Message) -> std::vec::Vec<Sensor> {
    match message.payload.as_ref() {
        Some(Message_::Payload::Urban(urban)) => urban
            .public
            .iter()
            .map(|entry| entry.sensor.unwrap())
            .collect(),
        other => panic!("not an urban payload: {other:?}"),
    }
}

// --- Encoder ---------------------------------------------------------------

#[test]
fn valid_measurements_are_emitted_with_expected_fields() {
    let snapshot = snapshot_of(&[bme280(100), sds011(100), microphone(100)], 200);
    let encoded = UrbanEncoder::encode_snapshot(&snapshot, 7, 1_700_000_000_123).unwrap();
    let message = message_of(encoded.as_bytes());

    let meta = message.metadata().unwrap();
    assert_eq!(meta.node_id, 7);
    assert_eq!(meta.timestamp, 1_700_000_000_123);

    let sensors = sensors(&message);
    assert_eq!(sensors.len(), 7);
    let bme = |m: BME280_::Measurement| {
        Sensor::Bme280(rohi_connectivity_proto::sensor_v1::BME280 {
            measurement: Some(m),
        })
    };
    use rohi_connectivity_proto::sensor_v1::{
        Humidity, NoiseLevel, PM10, PM25, Pressure, Temperature,
    };
    assert_eq!(
        sensors[0],
        bme(BME280_::Measurement::Temperature(Temperature {
            centi_celsius: 2347
        }))
    );
    assert_eq!(
        sensors[1],
        bme(BME280_::Measurement::Humidity(Humidity {
            centi_percent: 5342
        }))
    );
    assert_eq!(
        sensors[2],
        bme(BME280_::Measurement::Pressure(Pressure {
            deci_pascal: 1_013_250
        }))
    );
    let sds = |m| {
        Sensor::Sds011(rohi_connectivity_proto::sensor_v1::SDS011 {
            measurement: Some(m),
        })
    };
    assert_eq!(
        sensors[3],
        sds(SDS011_::Measurement::Pm25(PM25 { deci_ug_m3: 123 }))
    );
    assert_eq!(
        sensors[4],
        sds(SDS011_::Measurement::Pm10(PM10 { deci_ug_m3: 201 }))
    );
    let mic = |m| {
        Sensor::Ics43434(rohi_connectivity_proto::sensor_v1::ICS43434 {
            measurement: Some(m),
        })
    };
    assert_eq!(
        sensors[5],
        mic(ICS43434_::Measurement::NoiseAvg(NoiseLevel { db: 45 }))
    );
    assert_eq!(
        sensors[6],
        mic(ICS43434_::Measurement::NoiseMax(NoiseLevel { db: 72 }))
    );

    let stats = encoded.stats();
    assert_eq!(stats.emitted, 7);
    assert_eq!(stats.omitted_not_valid, 0);
    assert_eq!(stats.omitted_unsupported, 0);
}

#[test]
fn only_valid_state_is_emitted_even_when_a_value_is_cached() {
    for state in [
        SensorState::WarmingUp,
        SensorState::Error,
        SensorState::Initializing,
    ] {
        let snapshot = snapshot_of(
            &[
                bme280(100),
                SensorEvent::StateChanged {
                    sensor: SensorKind::Bmx280,
                    state,
                    timestamp: ts(150),
                },
            ],
            200,
        );
        let cell = snapshot.measurements.bmx280.unwrap().temperature;
        assert!(cell.value.is_some(), "the value is still cached");
        assert_ne!(cell.state, MeasurementState::Valid);
        assert_eq!(
            UrbanEncoder::encode_snapshot(&snapshot, 0, 1),
            Err(EncodeError::NothingToSend),
            "{state:?}"
        );
    }
}

#[test]
fn stale_measurements_are_omitted() {
    let snapshot = snapshot_of(&[bme280(0), sds011(1_000_000)], 1_000_100);
    assert_eq!(
        snapshot.measurements.bmx280.unwrap().temperature.state,
        MeasurementState::Stale
    );
    let encoded = UrbanEncoder::encode_snapshot(&snapshot, 0, 1).unwrap();
    let sensors = sensors(&message_of(encoded.as_bytes()));
    assert_eq!(sensors.len(), 2);
    assert!(sensors.iter().all(|s| matches!(s, Sensor::Sds011(_))));
    assert_eq!(encoded.stats().omitted_not_valid, 3);
}

#[test]
fn unavailable_and_missing_sensors_produce_nothing() {
    let snapshot = snapshot_of(&[], 100);
    assert_eq!(
        UrbanEncoder::encode_snapshot(&snapshot, 0, 1),
        Err(EncodeError::NothingToSend)
    );
}

#[test]
fn a_bmp280_is_not_relabeled_as_a_bme280() {
    let snapshot = snapshot_of(
        &[update(MeasurementUpdate::Bmx280(Bmx280Update::bmp280(
            Celsius(20.0),
            Pascal(100_000.0),
            ts(100),
        )))],
        110,
    );
    assert!(snapshot.measurements.bmx280.unwrap().temperature.is_valid());
    assert_eq!(
        UrbanEncoder::encode_snapshot(&snapshot, 0, 1),
        Err(EncodeError::NothingToSend)
    );
    let (_, stats) = UrbanEncoder::message(&snapshot_of(&[bme280(100)], 110), 0, 1).unwrap();
    assert_eq!(stats.omitted_unsupported, 0);
}

#[test]
fn sensors_without_a_schema_representation_are_omitted_and_counted() {
    let snapshot = snapshot_of(
        &[
            sds011(100),
            update(MeasurementUpdate::Scd4x(Scd4xUpdate {
                co2: Ppm(600.0),
                temperature: Celsius(21.0),
                humidity: RelativeHumidity(40.0),
                timestamp: ts(100),
            })),
            update(MeasurementUpdate::Zmod4510(Zmod4510Update {
                o3: Ppb(10.0),
                no2: Ppb(5.0),
                fast_aqi: Aqi(1),
                epa_aqi: Aqi(2),
                timestamp: ts(100),
            })),
            update(MeasurementUpdate::Ags3871(Ags3871Update {
                co: Ppm(1.0),
                timestamp: ts(100),
            })),
            update(MeasurementUpdate::RadSens(RadSensUpdate {
                radiation: RadiationLevel(0.1),
                pulses: PulseCount(3),
                timestamp: ts(100),
            })),
        ],
        110,
    );
    let encoded = UrbanEncoder::encode_snapshot(&snapshot, 0, 1).unwrap();
    let sensors = sensors(&message_of(encoded.as_bytes()));
    assert_eq!(sensors.len(), 2);
    assert!(sensors.iter().all(|s| matches!(s, Sensor::Sds011(_))));
    assert_eq!(encoded.stats().omitted_unsupported, 3 + 4 + 1 + 2);
}

#[test]
fn unrepresentable_values_are_omitted_not_clamped() {
    let snapshot = snapshot_of(
        &[update(MeasurementUpdate::Sds011(Sds011Update {
            pm25: MicrogramsPerCubicMeter(f32::NAN),
            pm10: MicrogramsPerCubicMeter(-1.0),
            timestamp: ts(100),
        }))],
        110,
    );
    assert_eq!(
        UrbanEncoder::encode_snapshot(&snapshot, 0, 1),
        Err(EncodeError::NothingToSend)
    );
    let snapshot = snapshot_of(
        &[
            sds011(100),
            update(MeasurementUpdate::Microphone(NoiseUpdate {
                average: Decibels(f32::INFINITY),
                maximum: Decibels(60.0),
                timestamp: ts(100),
            })),
        ],
        110,
    );
    let encoded = UrbanEncoder::encode_snapshot(&snapshot, 0, 1).unwrap();
    assert_eq!(encoded.stats().omitted_out_of_range, 1);
    assert_eq!(encoded.stats().emitted, 3);
}

#[test]
fn humidity_noise_is_clamped_to_the_schema_range() {
    let snapshot = snapshot_of(
        &[update(MeasurementUpdate::Bmx280(Bmx280Update::bme280(
            Celsius(-5.234),
            RelativeHumidity(100.4),
            Pascal(90_000.04),
            ts(100),
        )))],
        110,
    );
    let sensors = sensors(&message_of(
        UrbanEncoder::encode_snapshot(&snapshot, 0, 1)
            .unwrap()
            .as_bytes(),
    ));
    let Sensor::Bme280(t) = sensors[0] else {
        panic!()
    };
    assert!(matches!(
        t.measurement,
        Some(BME280_::Measurement::Temperature(t)) if t.centi_celsius == -523
    ));
    let Sensor::Bme280(h) = sensors[1] else {
        panic!()
    };
    assert!(matches!(
        h.measurement,
        Some(BME280_::Measurement::Humidity(h)) if h.centi_percent == 10_000
    ));
}

#[test]
fn the_largest_message_fits_its_bound() {
    use rohi_connectivity_proto::MessageEncode;
    assert!(Message::MAX_SIZE.unwrap() <= limits::MAX_MESSAGE_LEN);
    assert!(SignedEnvelope::MAX_SIZE.unwrap() <= limits::MAX_ENVELOPE_LEN);
}

// --- Envelope --------------------------------------------------------------

/// Nonce source counting up from 1: every nonce is different and known.
#[derive(Default)]
pub(crate) struct CountingNonce(pub u8);

impl NonceSource for CountingNonce {
    fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
        self.0 += 1;
        dst.fill(self.0);
        Ok(())
    }
}

struct NoEntropy;

impl NonceSource for NoEntropy {
    fn fill_nonce(&mut self, _: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
        Err(NonceError)
    }
}

fn frame(signer: &MockSigner, nonce: &mut CountingNonce, message: &[u8]) -> EnvelopeFrame {
    EnvelopeBuilder::new(signer, nonce).sign(message).unwrap()
}

#[test]
fn signature_covers_exactly_sensor_id_nonce_message() {
    let signer = MockSigner::default();
    let mut nonce = CountingNonce::default();
    let message = [0x0a, 0x02, 0x08, 0x01];
    let frame = frame(&signer, &mut nonce, &message);

    let envelope: SignedEnvelope = decode(frame.as_bytes()).unwrap();
    assert_eq!(
        envelope.sensor_id.as_slice(),
        signer.public_key().as_bytes()
    );
    assert_eq!(envelope.nonce.as_slice(), &[1u8; 16]);
    assert_eq!(envelope.message.as_slice(), &message);
    assert_eq!(envelope.signature.len(), 64);

    let mut preimage = std::vec::Vec::new();
    preimage.extend_from_slice(signer.public_key().as_bytes());
    preimage.extend_from_slice(&[1u8; 16]);
    preimage.extend_from_slice(&message);
    let signature = Signature::from_slice(&envelope.signature).unwrap();
    assert_eq!(signer.public_key().verify(&preimage, &signature), Ok(()));

    // Any change of the signed parts breaks the signature.
    for broken in [
        [&[0u8; 32][..], &[1u8; 16], &message].concat(),
        [signer.public_key().as_bytes(), &[2u8; 16][..], &message].concat(),
        [signer.public_key().as_bytes(), &[1u8; 16][..], &[0u8; 4]].concat(),
        [&[1u8; 16][..], &message].concat(),
    ] {
        assert!(signer.public_key().verify(&broken, &signature).is_err());
    }
}

#[test]
fn the_envelope_has_exactly_four_fields() {
    let signer = MockSigner::default();
    let frame = frame(&signer, &mut CountingNonce::default(), &[0x0a, 0x00]);
    let bytes = frame.as_bytes();
    // Walk the wire format: tags 1..=4, length-delimited, nothing else.
    let mut tags = std::vec::Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        tags.push(bytes[at] >> 3);
        assert_eq!(bytes[at] & 7, 2);
        at += 2 + bytes[at + 1] as usize;
    }
    assert_eq!(tags, [1, 2, 3, 4]);
    assert_eq!(at, bytes.len());
}

#[test]
fn every_frame_gets_a_new_nonce_and_signature() {
    let signer = MockSigner::default();
    let mut nonce = CountingNonce::default();
    let a = frame(&signer, &mut nonce, &[1, 2, 3]);
    let b = frame(&signer, &mut nonce, &[1, 2, 3]);
    assert_ne!(a.as_bytes(), b.as_bytes());
    let a: SignedEnvelope = decode(a.as_bytes()).unwrap();
    let b: SignedEnvelope = decode(b.as_bytes()).unwrap();
    assert_ne!(a.nonce, b.nonce);
    assert_ne!(a.signature, b.signature);
}

#[test]
fn building_fails_cleanly_without_identity_entropy_or_room() {
    let message = [1u8, 2, 3];
    let missing = MockSigner::failing(IdentityError::NotProvisioned);
    assert_eq!(
        EnvelopeBuilder::new(&missing, &mut CountingNonce::default())
            .sign::<{ limits::MAX_ENVELOPE_LEN }>(&message),
        Err(BuildError::NotProvisioned)
    );
    let signer = MockSigner::default();
    assert_eq!(
        EnvelopeBuilder::new(&signer, &mut NoEntropy)
            .sign::<{ limits::MAX_ENVELOPE_LEN }>(&message),
        Err(BuildError::Nonce)
    );
    assert_eq!(
        EnvelopeBuilder::new(&signer, &mut CountingNonce::default()).sign::<16>(&message),
        Err(BuildError::TooLarge)
    );
    assert_eq!(
        EnvelopeBuilder::new(&signer, &mut CountingNonce::default())
            .sign::<{ limits::MAX_ENVELOPE_LEN }>(&[0u8; limits::MAX_MESSAGE_LEN + 1]),
        Err(BuildError::TooLarge)
    );
}

#[test]
fn a_full_snapshot_fits_the_default_frame() {
    let snapshot = snapshot_of(&[bme280(100), sds011(100), microphone(100)], 200);
    let encoded = UrbanEncoder::encode_snapshot(&snapshot, u64::MAX, u64::MAX).unwrap();
    let signer = MockSigner::default();
    let frame = frame(&signer, &mut CountingNonce::default(), encoded.as_bytes());
    assert!(frame.len() <= limits::MAX_ENVELOPE_LEN);
}

// --- Outbox ------------------------------------------------------------------

#[test]
fn outbox_is_bounded_fifo_and_never_evicts() {
    let signer = MockSigner::default();
    let mut nonce = CountingNonce::default();
    let mut outbox = Outbox::<2>::new();
    let first = frame(&signer, &mut nonce, &[1]);
    let second = frame(&signer, &mut nonce, &[2]);
    let third = frame(&signer, &mut nonce, &[3]);
    outbox.push(first.clone()).unwrap();
    outbox.push(second.clone()).unwrap();
    assert!(outbox.is_full());
    assert_eq!(outbox.push(third), Err(OutboxFull));
    assert_eq!(outbox.len(), 2);
    assert_eq!(outbox.front(), Some(&first));
    assert_eq!(outbox.pop(), Some(first));
    assert_eq!(outbox.pop(), Some(second));
    assert!(outbox.is_empty());
}

// --- Transport classification ---------------------------------------------------

#[test]
fn http_statuses_are_classified_centrally() {
    use Disposition::*;
    assert_eq!(classify_status(202), Ok(TransportAck { status: 202 }));
    let disposition = |status| classify_status(status).unwrap_err().disposition();
    for status in [408, 429, 500, 502, 503, 504, 599] {
        assert_eq!(disposition(status), Retry, "{status}");
    }
    for status in [401, 403] {
        assert_eq!(disposition(status), Unauthorized, "{status}");
    }
    for status in [400, 404, 405, 413, 415, 422, 499] {
        assert_eq!(disposition(status), Permanent, "{status}");
    }
    // Not an acceptance, not a verdict on the frame either.
    for status in [100, 200, 204, 302, 307] {
        assert_eq!(disposition(status), Retry, "{status}");
    }
    for error in [
        TransportError::NetworkUnavailable,
        TransportError::Dns,
        TransportError::Connect,
        TransportError::Timeout,
        TransportError::Io,
        TransportError::BadResponse,
    ] {
        assert_eq!(error.disposition(), Retry);
    }
}

// --- Test doubles shared by the service and HTTP tests ------------------------------

pub(crate) struct NoDelay;

impl DelayNs for NoDelay {
    async fn delay_ns(&mut self, _ns: u32) {}
}

pub(crate) struct FakeClock(pub Cell<u64>);

impl Clock for &FakeClock {
    fn now(&self) -> MonotonicTimestamp {
        ts(self.0.get())
    }
}

#[allow(dead_code)]
pub(crate) fn run<F: core::future::Future>(f: F) -> F::Output {
    block_on(f)
}

#[allow(dead_code)]
type Shared = ConnectivityShared<NoopRawMutex>;

#[allow(dead_code)]
pub(crate) fn recorded<T: Clone>(cell: &RefCell<std::vec::Vec<T>>) -> std::vec::Vec<T> {
    cell.borrow().clone()
}

#[test]
fn sign_trait_is_object_compatible_with_the_mock() {
    let signer = MockSigner::default();
    assert_eq!(FrameSigner::sensor_id(&signer), Ok(signer.public_key()));
    assert_eq!(
        FrameSigner::sign_payload(&MockSigner::failing(IdentityError::NotProvisioned), b"x"),
        Err(SignError::NotProvisioned)
    );
    assert!(Signer::sign(&signer, b"x").is_ok());
}
