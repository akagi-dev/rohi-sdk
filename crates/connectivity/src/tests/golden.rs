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
//! Interoperability vectors.
//!
//! Generated with the reference implementation (`airalab/robins` at
//! `a6c1d9cb052093192fe886a4a2aa46821679afa1`, `edge` CLI) against the
//! protocol:
//!
//! ```text
//! SEED=0x4242424242424242424242424242424242424242424242424242424242424242
//! NONCE=0x01010101010101010101010101010101
//! edge message --board urban temp=23.47 humidity=53.42 pressure=101325 \
//!     pm25=12.3 pm10=20.1 noise_avg=45 noise_max=72 \
//!     --node-id 7 --timestamp 1700000000123 --output hex
//! edge envelope --sign $SEED --nonce $NONCE --verify --input hex --output hex <message>
//! ```
//!
//! The pressure is given in pascal: the CLI help says hPa, the field is
//! `Pressure.deci_pascal` (0.1 Pa) and the CLI encodes `101325` as `1013250`,
//! as the schema documents.
//!
//! Any accidental change of field numbering, value scaling, signing order or
//! envelope layout fails these tests.

use super::*;

const NODE_ID: u64 = 7;
const TIMESTAMP_MS: u64 = 1_700_000_000_123;

const PUBLIC_KEY: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12";
const NONCE: [u8; NONCE_LEN] = [0x01; NONCE_LEN];

/// `core.v1.Message`.
const MESSAGE: &str = "0a09080710fbd095ffbc31123d0a0712050a0308d6240a071205120308de290a0812061a040882ec3d0a061a040a02087b0a071a05120308c9010a0622041202082d0a0622040a020848";

/// Exact signing preimage: `sensor_id || nonce || message`.
const PREIMAGE: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12010101010101010101010101010101010a09080710fbd095ffbc31123d0a0712050a0308d6240a071205120308de290a0812061a040882ec3d0a061a040a02087b0a071a05120308c9010a0622041202082d0a0622040a020848";

/// Ed25519 signature over the preimage.
const SIGNATURE: &str = "b1c9f79931e0ae02cc9ea3f1c8ee2fe9f8f8b7fc047387e3136b3bd33b115de7c9fe4bffc20c55d1190a4bbc2d5b31e8b83e53b79c5ff521356e3a37148ce10a";

/// `crypto.v1.SignedEnvelope`.
const ENVELOPE: &str = "0a202152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db121210010101010101010101010101010101011a4a0a09080710fbd095ffbc31123d0a0712050a0308d6240a071205120308de290a0812061a040882ec3d0a061a040a02087b0a071a05120308c9010a0622041202082d0a0622040a0208482240b1c9f79931e0ae02cc9ea3f1c8ee2fe9f8f8b7fc047387e3136b3bd33b115de7c9fe4bffc20c55d1190a4bbc2d5b31e8b83e53b79c5ff521356e3a37148ce10a";

fn hex(text: &str) -> std::vec::Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// Altruist Urban values of the vector, as Rohi observations.
fn snapshot() -> MeasurementsSnapshot {
    snapshot_of(
        &[
            update(MeasurementUpdate::Bmx280(Bmx280Update::bme280(
                Celsius(23.47),
                RelativeHumidity(53.42),
                Pascal(101_325.0),
                ts(900),
            ))),
            update(MeasurementUpdate::Sds011(Sds011Update {
                pm25: MicrogramsPerCubicMeter(12.3),
                pm10: MicrogramsPerCubicMeter(20.1),
                timestamp: ts(900),
            })),
            update(MeasurementUpdate::Microphone(NoiseUpdate {
                average: Decibels(45.0),
                maximum: Decibels(72.0),
                timestamp: ts(900),
            })),
        ],
        1_000,
    )
}

/// Fixed nonce, for reproducible signatures.
struct FixedNonce;

impl NonceSource for FixedNonce {
    fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
        *dst = NONCE;
        Ok(())
    }
}

#[test]
fn the_vector_key_is_the_test_identity() {
    assert_eq!(
        MockSigner::default().public_key().as_bytes().as_slice(),
        hex(PUBLIC_KEY)
    );
}

#[test]
fn message_bytes_match_the_reference() {
    let encoded = UrbanEncoder::encode_snapshot(&snapshot(), NODE_ID, TIMESTAMP_MS).unwrap();
    assert_eq!(encoded.as_bytes(), hex(MESSAGE));
    assert_eq!(encoded.stats().emitted, 7);
}

#[test]
fn preimage_and_signature_match_the_reference() {
    let message = hex(MESSAGE);
    let mut preimage = [0u8; 256];
    let len = rohi_connectivity_proto::signing_preimage(
        &hex(PUBLIC_KEY),
        &NONCE,
        &message,
        &mut preimage,
    )
    .unwrap();
    assert_eq!(&preimage[..len], hex(PREIMAGE));

    let signature = MockSigner::default().sign(&preimage[..len]).unwrap();
    assert_eq!(signature.as_bytes().as_slice(), hex(SIGNATURE));
}

#[test]
fn envelope_bytes_match_the_reference() {
    let signer = MockSigner::default();
    let encoded = UrbanEncoder::encode_snapshot(&snapshot(), NODE_ID, TIMESTAMP_MS).unwrap();
    let frame: EnvelopeFrame = EnvelopeBuilder::new(&signer, &mut FixedNonce)
        .sign(encoded.as_bytes())
        .unwrap();
    assert_eq!(frame.as_bytes(), hex(ENVELOPE));
}

#[test]
fn the_reference_envelope_verifies_with_the_device_key() {
    let envelope: SignedEnvelope = decode(&hex(ENVELOPE)).unwrap();
    let key = rohi_identity::PublicKey::from_slice(&envelope.sensor_id).unwrap();
    let mut preimage = std::vec::Vec::new();
    preimage.extend_from_slice(&envelope.sensor_id);
    preimage.extend_from_slice(&envelope.nonce);
    preimage.extend_from_slice(&envelope.message);
    let signature = Signature::from_slice(&envelope.signature).unwrap();
    assert_eq!(key.verify(&preimage, &signature), Ok(()));
    let message = message_of(&envelope.message);
    assert_eq!(message.metadata().unwrap().timestamp, TIMESTAMP_MS);
}
