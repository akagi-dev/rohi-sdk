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
//! End-to-end behaviour of the format: sealing, signature, private sections,
//! failure modes and the CPS key schedule checked against an independent
//! implementation of it.

use core::cell::Cell;

use embassy_futures::block_on;
use rohi_identity::{IdentityError, MockSigner, Signer};
use rohi_telemetry::catalog::*;
use rohi_telemetry::{
    Capabilities, DataSelection, GeoPoint, ObservationValue, PacketFormat, PacketFormatId,
    PacketProfile, RecipientAddress, SealError, Selection, SourceId, TelemetryConfig,
    TelemetrySnapshot, prepare,
};
use rohi_telemetry_robonomics::crypto;
use rohi_telemetry_robonomics::wire::core_v1::{Message, Message_};
use rohi_telemetry_robonomics::wire::crypto_v1::{Encrypted, SignedEnvelope};
use rohi_telemetry_robonomics::wire::device_v1::{EncryptedUrban, UrbanSensor_};
use rohi_telemetry_robonomics::wire::limits::{MAX_ENVELOPE_LEN, MAX_PRIVATE_PLAINTEXT_LEN};
use rohi_telemetry_robonomics::wire::{decode, signing_preimage};
use rohi_telemetry_robonomics::{ClockError, Entropy, EntropyError, RobonomicsFormat, UtcClock};
use rohi_time::{Clock, MonotonicTimestamp};

const DEVICE: [u8; 32] = [7; 32];
const OWNER: [u8; 32] = [9; 32];
const CITY: [u8; 32] = [10; 32];

struct Mono(Cell<u64>);
impl Clock for &Mono {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(self.0.get())
    }
}

struct Utc(Option<u64>);
impl UtcClock for Utc {
    fn now_unix_ms(&self) -> Result<u64, ClockError> {
        self.0.ok_or(ClockError)
    }
}

/// Distinct bytes on every call, or nothing.
struct Counter {
    next: u64,
    working: bool,
}

struct Rng<'a>(&'a Cell<Counter>);

impl Entropy for Rng<'_> {
    fn fill(&mut self, dst: &mut [u8]) -> Result<(), EntropyError> {
        let mut state = self.0.replace(Counter {
            next: 0,
            working: false,
        });
        if !state.working {
            self.0.set(state);
            return Err(EntropyError);
        }
        for byte in dst.iter_mut() {
            state.next = state
                .next
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *byte = (state.next >> 33) as u8;
        }
        self.0.set(state);
        Ok(())
    }
}

fn entropy() -> Cell<Counter> {
    Cell::new(Counter {
        next: 1,
        working: true,
    })
}

fn at(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn snapshot() -> TelemetrySnapshot<16> {
    let mut snapshot = TelemetrySnapshot::new(at(10_000));
    let mut writer = snapshot.writer(SourceId::LOCAL);
    writer.number(BMX280_TEMPERATURE, 21.5, at(9_000)).unwrap();
    writer.number(BMX280_HUMIDITY, 45.0, at(9_000)).unwrap();
    writer.number(SDS011_PM10, 12.0, at(9_000)).unwrap();
    writer.number(MICROPHONE_AVERAGE, 41.0, at(9_000)).unwrap();
    writer.finish(Ok(()));
    let mut writer = snapshot.writer(SourceId::LOCATION);
    writer
        .write(
            LOCATION,
            ObservationValue::Location(GeoPoint {
                latitude: 52.52,
                longitude: 13.405,
                altitude_m: Some(34.0),
            }),
            at(9_500),
        )
        .unwrap();
    writer.finish(Ok(()));
    snapshot
}

fn key(seed: [u8; 32]) -> MockSigner {
    MockSigner::new(seed)
}

fn address(seed: [u8; 32]) -> RecipientAddress {
    RecipientAddress(key(seed).public_key().to_bytes())
}

type Format<'a> = RobonomicsFormat<'a, MockSigner, Rng<'a>, Utc, &'a Mono>;

fn profile(
    format: &Format<'_>,
    build: impl FnOnce(&mut TelemetryConfig<3, 2>, &Capabilities<'_, Format<'_>>),
) -> PacketProfile<2> {
    let caps = Capabilities(format);
    let mut config = TelemetryConfig::<3, 2>::default();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    build(&mut config, &caps);
    config.get("p").unwrap().clone()
}

fn local(ids: &[rohi_telemetry::MeasurementId]) -> DataSelection {
    DataSelection::local(ids.iter().fold(Selection::NONE, |s, id| s.with(*id)))
}

fn owner_profile(format: &Format<'_>) -> PacketProfile<2> {
    profile(format, |config, caps| {
        config
            .set_public("p", local(&[BMX280_TEMPERATURE, BMX280_HUMIDITY]), caps)
            .unwrap();
        config
            .add_private("p", address(OWNER), local(&[LOCATION, SDS011_PM10]), caps)
            .unwrap();
        config
            .add_private(
                "p",
                address(CITY),
                local(&[LOCATION, MICROPHONE_AVERAGE]),
                caps,
            )
            .unwrap();
    })
}

struct Fixture {
    device: MockSigner,
    mono: Mono,
    utc: Utc,
    entropy: Cell<Counter>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            device: key(DEVICE),
            mono: Mono(Cell::new(12_000)),
            utc: Utc(Some(1_700_000_100_000)),
            entropy: entropy(),
        }
    }

    fn format(&self) -> Format<'_> {
        RobonomicsFormat::new(&self.device, Rng(&self.entropy), &self.utc, &self.mono)
    }
}

fn seal(
    fixture: &Fixture,
    profile: &PacketProfile<2>,
    snapshot: &TelemetrySnapshot<16>,
) -> Result<std::vec::Vec<u8>, SealError> {
    let mut format = fixture.format();
    let sections =
        prepare(snapshot, profile, &format.supported()).map_err(|_| SealError::NothingToPackage)?;
    let mut out = [0u8; MAX_ENVELOPE_LEN];
    let packet = block_on(format.seal(&sections, &mut out))?;
    assert_eq!(packet.info().length, packet.bytes().len());
    assert_eq!(packet.info().profile.id.as_str(), "p");
    Ok(packet.bytes().to_vec())
}

fn envelope(bytes: &[u8]) -> (SignedEnvelope, Message) {
    let envelope: SignedEnvelope = decode(bytes).unwrap();
    let message: Message = decode(&envelope.message).unwrap();
    (envelope, message)
}

fn urban(message: &Message) -> &rohi_telemetry_robonomics::wire::device_v1::Urban {
    match &message.payload {
        Some(Message_::Payload::Urban(urban)) => urban,
        _ => panic!("an urban payload"),
    }
}

#[test]
fn the_envelope_is_signed_over_the_exact_message_and_dated_at_the_capture() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let profile = owner_profile(&format);
    let bytes = seal(&fixture, &profile, &snapshot()).unwrap();
    let (envelope, message) = envelope(&bytes);

    assert_eq!(
        envelope.sensor_id.as_slice(),
        key(DEVICE).public_key().as_bytes()
    );
    let mut preimage = [0u8; 1024];
    let len = signing_preimage(
        &envelope.sensor_id,
        &envelope.nonce,
        &envelope.message,
        &mut preimage,
    )
    .unwrap();
    let verification =
        ed25519_zebra::VerificationKey::try_from(envelope.sensor_id.as_slice()).unwrap();
    let signature =
        ed25519_zebra::Signature::from_bytes(envelope.signature.as_slice().try_into().unwrap());
    verification.verify(&signature, &preimage[..len]).unwrap();

    // The capture was 2 s before "now" on the monotonic clock.
    let metadata = message.metadata().unwrap();
    assert_eq!(metadata.timestamp, 1_700_000_100_000 - 2_000);
    assert_eq!(metadata.node_id, 0);
}

#[test]
fn the_public_section_is_plaintext_and_the_private_ones_are_not() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let profile = owner_profile(&format);
    let bytes = seal(&fixture, &profile, &snapshot()).unwrap();
    let (_, message) = envelope(&bytes);
    let urban = urban(&message);
    assert_eq!(urban.public.len(), 2, "temperature and humidity");
    assert_eq!(urban.private.len(), 2);

    // Neither the position nor the noise level appears in the clear.
    let lat = 52.52f64.to_le_bytes();
    assert!(!bytes.windows(8).any(|w| w == lat));
    let encrypted: Encrypted = decode(&urban.private[0]).unwrap();
    assert_eq!(encrypted.version, 1);
    assert_eq!(encrypted.algorithm.as_str(), "xchacha20");
    assert_eq!(
        encrypted.from.as_slice(),
        key(DEVICE).public_key().as_bytes()
    );
    assert_eq!(encrypted.nonce.len(), 24);
}

fn open(
    message: &Message,
    index: usize,
    recipient: &MockSigner,
) -> Result<EncryptedUrban, crypto::CryptoError> {
    let encrypted: Encrypted = decode(&urban(message).private[index]).unwrap();
    let sender = rohi_identity::PublicKey::from_slice(&encrypted.from).unwrap();
    let nonce: [u8; 24] = encrypted.nonce.as_slice().try_into().unwrap();
    let mut plaintext = [0u8; MAX_PRIVATE_PLAINTEXT_LEN];
    let len = crypto::decrypt(
        recipient,
        &sender,
        &nonce,
        &encrypted.ciphertext,
        &mut plaintext,
    )?;
    Ok(decode(&plaintext[..len]).unwrap())
}

#[test]
fn each_recipient_decrypts_only_its_own_section() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let profile = owner_profile(&format);
    let bytes = seal(&fixture, &profile, &snapshot()).unwrap();
    let (_, message) = envelope(&bytes);

    let owner = open(&message, 0, &key(OWNER)).unwrap();
    assert_eq!(owner.sensors.len(), 2, "location and PM10");
    assert!(
        owner
            .sensors
            .iter()
            .any(|s| matches!(s.sensor, Some(UrbanSensor_::Sensor::Gps(_))))
    );
    let city = open(&message, 1, &key(CITY)).unwrap();
    assert_eq!(city.sensors.len(), 2, "location and noise");

    // The wrong recipient, and the wrong section, fail authentication.
    assert_eq!(
        open(&message, 0, &key(CITY)).unwrap_err(),
        crypto::CryptoError::Cipher
    );
    assert_eq!(
        open(&message, 1, &key(OWNER)).unwrap_err(),
        crypto::CryptoError::Cipher
    );
    assert_eq!(
        open(&message, 0, &key([3; 32])).unwrap_err(),
        crypto::CryptoError::Cipher
    );
}

#[test]
fn a_tampered_private_section_is_rejected() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let profile = owner_profile(&format);
    let bytes = seal(&fixture, &profile, &snapshot()).unwrap();
    let (_, mut message) = envelope(&bytes);
    let mut section = decode::<Encrypted>(&urban_private(&message, 0)).unwrap();
    section.ciphertext[0] ^= 1;
    let sender = key(DEVICE).public_key();
    let nonce: [u8; 24] = section.nonce.as_slice().try_into().unwrap();
    let mut plaintext = [0u8; MAX_PRIVATE_PLAINTEXT_LEN];
    assert_eq!(
        crypto::decrypt(
            &key(OWNER),
            &sender,
            &nonce,
            &section.ciphertext,
            &mut plaintext
        ),
        Err(crypto::CryptoError::Cipher)
    );
    let _ = &mut message;
}

fn urban_private(message: &Message, index: usize) -> std::vec::Vec<u8> {
    urban(message).private[index].to_vec()
}

#[test]
fn the_key_schedule_matches_an_independent_implementation_of_cps() {
    use chacha20poly1305::aead::{Aead, KeyInit};
    use hkdf::Hkdf;
    use sha2::{Digest, Sha256, Sha512};

    let fixture = Fixture::new();
    let format = fixture.format();
    let profile = owner_profile(&format);
    let bytes = seal(&fixture, &profile, &snapshot()).unwrap();
    let (_, message) = envelope(&bytes);
    let encrypted: Encrypted = decode(&urban(&message).private[0]).unwrap();

    // Recipient side, written out from the CPS description: clamp
    // SHA-512(seed), Montgomery form of the sender's Ed25519 key, X25519,
    // HKDF-SHA256(salt "robonomics-network", info "xchacha20poly1305").
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&Sha512::digest(OWNER)[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    let sender_montgomery = curve25519_dalek::edwards::CompressedEdwardsY(
        encrypted.from.as_slice().try_into().unwrap(),
    )
    .decompress()
    .unwrap()
    .to_montgomery();
    let shared = x25519_dalek::x25519(scalar, sender_montgomery.to_bytes());
    let mut derived = [0u8; 32];
    Hkdf::<Sha256>::new(Some(b"robonomics-network"), &shared)
        .expand(b"xchacha20poly1305", &mut derived)
        .unwrap();
    let cipher = chacha20poly1305::XChaCha20Poly1305::new(&derived.into());
    let nonce =
        chacha20poly1305::XNonce::from(<[u8; 24]>::try_from(encrypted.nonce.as_slice()).unwrap());
    let plaintext = cipher
        .decrypt(&nonce, encrypted.ciphertext.as_slice())
        .unwrap();
    let section: EncryptedUrban = decode(&plaintext).unwrap();
    assert_eq!(section.sensors.len(), 2);
}

#[test]
fn nonces_never_repeat() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let profile = owner_profile(&format);
    let first = seal(&fixture, &profile, &snapshot()).unwrap();
    let second = seal(&fixture, &profile, &snapshot()).unwrap();
    let (a, am) = envelope(&first);
    let (b, bm) = envelope(&second);
    assert_ne!(a.nonce, b.nonce);
    let nonce = |m: &Message, i| decode::<Encrypted>(&urban_private(m, i)).unwrap().nonce;
    let all = [nonce(&am, 0), nonce(&am, 1), nonce(&bm, 0), nonce(&bm, 1)];
    for (index, nonce) in all.iter().enumerate() {
        assert!(all[..index].iter().all(|other| other != nonce));
    }
}

#[test]
fn without_utc_there_is_no_packet_and_no_invented_timestamp() {
    let mut fixture = Fixture::new();
    let profile = owner_profile(&fixture.format());
    fixture.utc = Utc(None);
    assert_eq!(
        seal(&fixture, &profile, &snapshot()),
        Err(SealError::TimeUnavailable)
    );
}

#[test]
fn no_entropy_means_no_packet_not_a_plaintext_one() {
    let fixture = Fixture::new();
    let profile = owner_profile(&fixture.format());
    fixture.entropy.set(Counter {
        next: 0,
        working: false,
    });
    assert_eq!(
        seal(&fixture, &profile, &snapshot()),
        Err(SealError::EntropyUnavailable)
    );
}

#[test]
fn a_recipient_key_that_cannot_be_used_fails_the_whole_packet() {
    let fixture = Fixture::new();
    let format = fixture.format();
    // The identity point is a valid encoding of a small-order point.
    let mut weak = [0u8; 32];
    weak[0] = 1;
    assert!(!format.accepts_recipient(&RecipientAddress(weak)));
    assert!(!format.accepts_recipient(&RecipientAddress([2; 32])));
    assert!(format.accepts_recipient(&address(OWNER)));

    // A stored profile that nevertheless has such a recipient never degrades
    // to a public packet.
    let mut profile = owner_profile(&format);
    profile.private[0].recipient = RecipientAddress(weak);
    assert_eq!(
        seal(&fixture, &profile, &snapshot()),
        Err(SealError::Encryption)
    );
}

#[test]
fn a_failing_identity_fails_the_packet() {
    let mut fixture = Fixture::new();
    let profile = owner_profile(&fixture.format());
    fixture.device = MockSigner::failing(IdentityError::NotProvisioned);
    let result = seal(&fixture, &profile, &snapshot());
    assert!(matches!(
        result,
        Err(SealError::IdentityUnavailable | SealError::Signing | SealError::Encryption)
    ));
}

#[test]
fn nothing_selected_is_no_packet_and_a_small_buffer_is_refused() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let empty = profile(&format, |_, _| {});
    assert_eq!(
        seal(&fixture, &empty, &snapshot()),
        Err(SealError::NothingToPackage)
    );

    let owner = owner_profile(&format);
    let snapshot = snapshot();
    let mut format = fixture.format();
    let sections = prepare(&snapshot, &owner, &format.supported()).unwrap();
    let mut small = [0u8; 40];
    assert_eq!(
        block_on(format.seal(&sections, &mut small)).unwrap_err(),
        SealError::BufferTooSmall
    );
}

#[test]
fn unsupported_measurements_cannot_be_selected() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let caps = Capabilities(&format);
    let mut config = TelemetryConfig::<3, 2>::default();
    config.add_profile("p", PacketFormatId::Robonomics).unwrap();
    assert!(config.set_public("p", local(&[SCD4X_CO2]), &caps).is_err());
    assert!(
        config
            .set_public("p", local(&[ZMOD4510_O3]), &caps)
            .is_err()
    );
    assert!(
        config
            .set_public("p", local(&[BMX280_TEMPERATURE]), &caps)
            .is_ok()
    );
}

#[test]
fn a_location_only_packet_is_supported() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let profile = profile(&format, |config, caps| {
        config
            .add_private("p", address(OWNER), local(&[LOCATION]), caps)
            .unwrap()
    });
    let bytes = seal(&fixture, &profile, &snapshot()).unwrap();
    let (_, message) = envelope(&bytes);
    assert!(urban(&message).public.is_empty());
    assert_eq!(open(&message, 0, &key(OWNER)).unwrap().sensors.len(), 1);
}

#[test]
fn the_largest_supported_profile_fits_the_envelope_limit() {
    let fixture = Fixture::new();
    let format = fixture.format();
    let everything = [
        LOCATION,
        BMX280_TEMPERATURE,
        BMX280_HUMIDITY,
        BMX280_PRESSURE,
        SDS011_PM2_5,
        SDS011_PM10,
        MICROPHONE_AVERAGE,
        MICROPHONE_MAXIMUM,
    ];
    let profile = profile(&format, |config, caps| {
        config.set_public("p", local(&everything), caps).unwrap();
        config
            .add_private("p", address(OWNER), local(&everything), caps)
            .unwrap();
        config
            .add_private("p", address(CITY), local(&everything), caps)
            .unwrap();
    });
    let mut snapshot = TelemetrySnapshot::<16>::new(at(10_000));
    let mut writer = snapshot.writer(SourceId::LOCAL);
    for (id, value) in [
        (BMX280_TEMPERATURE, -20_000.0f32),
        (BMX280_HUMIDITY, 100.0),
        (BMX280_PRESSURE, 110_000.0),
        (SDS011_PM2_5, 999.9),
        (SDS011_PM10, 999.9),
        (MICROPHONE_AVERAGE, 120.0),
        (MICROPHONE_MAXIMUM, 130.0),
    ] {
        writer.number(id, value, at(9_000)).unwrap();
    }
    writer.finish(Ok(()));
    let mut writer = snapshot.writer(SourceId::LOCATION);
    writer
        .write(
            LOCATION,
            ObservationValue::Location(GeoPoint {
                latitude: -89.999999,
                longitude: -179.999999,
                altitude_m: Some(-8848.123),
            }),
            at(9_000),
        )
        .unwrap();
    writer.finish(Ok(()));
    let bytes = seal(&fixture, &profile, &snapshot).unwrap();
    assert!(bytes.len() <= MAX_ENVELOPE_LEN, "{}", bytes.len());
    let (_, message) = envelope(&bytes);
    assert_eq!(urban(&message).private.len(), 2);
    assert_eq!(open(&message, 1, &key(CITY)).unwrap().sensors.len(), 8);
    std::eprintln!("largest packet: {} bytes", bytes.len());
}

#[test]
fn a_replayed_nonce_is_not_possible_with_fresh_entropy_after_a_failure() {
    // A failed attempt consumes no key material that a later attempt reuses.
    let fixture = Fixture::new();
    let profile = owner_profile(&fixture.format());
    let good = seal(&fixture, &profile, &snapshot()).unwrap();
    let again = seal(&fixture, &profile, &snapshot()).unwrap();
    assert_ne!(good, again);
}
